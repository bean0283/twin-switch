//! Trae 会话彻底删除（页面级加解密，零 SQLCipher 依赖）。
//!
//! 为什么不用 rusqlite 直连实时加密库：本工程的 rusqlite 未编译 SQLCipher 特性，
//! `PRAGMA key` 不生效，打开加密库必然报「file is not a database」——这正是
//! 「重新解密也没用」的根因。删除因此复用与导入（`trae_import`）严格对称的链路：
//!
//!   1. 解析实时库密钥：存盘密钥按首页 HMAC 校验，过期则从进程内存自动重扫
//!      （Trae 重启后密钥会变化，无需用户手动「扫描密钥并解密」）；
//!   2. 结束客户端进程（文件替换前提）；
//!   3. 解密实时库为明文副本，并把 WAL 中「最后一个已提交事务」的帧合并进去
//!      （避免强杀进程残留的 WAL 帧丢失其他会话的最新数据）；
//!   4. 明文副本删行 → 加密回写为 SQLCipher 库 → 备份原库 + 原子替换；
//!   5. 同步删解密库 → 会话文件移入回收站目录（可恢复）。

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::modules::config::store_dir;
use crate::modules::trae_decrypt::{decrypt_database, decrypt_page, hex_to_bytes, verify_page1_hmac};
use crate::modules::trae_discover::{database_path, extra_roots, get_client, snapshot_root};
use crate::modules::trae_export::{backup_dir, deleted_sessions_dir, is_session_id, label_of, open_decrypted};
use crate::modules::trae_import::{patch_reserved_field, session_owner_uid};
use crate::modules::trae_memory_scan::{load_saved_key, save_key, scan_for_key};
use crate::modules::trae_remote::has_cloud_credential;
use crate::modules::trae_switch::{is_running, kill_all, launch, wait_until_stopped};
use crate::modules::trae_vault::account_label;

const PAGE_SZ: usize = 4096;
const WAL_HDR_SZ: usize = 32;
const WAL_FRAME_HDR_SZ: usize = 24;

/// 实时库密钥解析：存盘密钥优先（按首页 HMAC 校验），过期时若客户端进程
/// 运行中则从内存自动重扫并保存（Trae 重启后密钥变化场景，免手动重新扫描）。
fn resolve_live_key(client_key: &str, live: &Path) -> Result<String, String> {
    if let Some(k) = load_saved_key(client_key) {
        if k.trim().len() == 64 {
            let page1 = read_page1(live)?;
            if verify_page1_hmac(&hex_to_bytes(&k), &page1) {
                return Ok(k);
            }
        }
    }
    let client = get_client(client_key).ok_or("未知客户端")?;
    if is_running(client) {
        let scan = scan_for_key(client_key, live, None)
            .map_err(|e| format!("重新扫描密钥失败：{e}"))?;
        if let Some(k) = scan.key {
            let _ = save_key(client_key, &k);
            return Ok(k);
        }
    }
    Err(
        "实时库密钥不匹配且无法自动恢复：Trae 重启后加密密钥会变化。\n\
         请先启动该客户端（并登录对应账号）后再重试删除，工具会自动从进程内存重新扫描密钥。"
            .into(),
    )
}

fn read_page1(path: &Path) -> Result<[u8; PAGE_SZ], String> {
    let mut page1 = [0u8; PAGE_SZ];
    let mut f = std::fs::File::open(path).map_err(|e| format!("打开实时库失败: {e}"))?;
    f.read_exact(&mut page1)
        .map_err(|e| format!("读取实时库首页失败: {e}"))?;
    Ok(page1)
}

fn wal_be_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// SQLite WAL 校验和（s0/s1 斐波那契加权，按 magic 决定 32 位字的字节序）。
fn wal_checksum_run(data: &[u8], magic: u32, mut s0: u32, mut s1: u32) -> (u32, u32) {
    let big = magic == 0x377f0683;
    let mut i = 0usize;
    while i + 4 <= data.len() {
        let w0 = if big {
            u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]])
        } else {
            u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]])
        };
        s0 = s0.wrapping_add(w0).wrapping_add(s1);
        i += 4;
        if i + 4 > data.len() {
            break;
        }
        let w1 = if big {
            u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]])
        } else {
            u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]])
        };
        s1 = s1.wrapping_add(w1).wrapping_add(s0);
        i += 4;
    }
    (s0, s1)
}

/// 解析 WAL，返回「截至最后一个已提交事务」的帧 `(pgno, commit_size, body_offset)`。
/// 非 WAL / 头部无效 / 页大小不符 / 无有效提交帧 → 返回空列表（等价于「无需合并」）。
fn wal_committed_frames(wal_data: &[u8]) -> Vec<(u32, u32, usize)> {
    if wal_data.len() < WAL_HDR_SZ {
        return Vec::new();
    }
    let magic = wal_be_u32(wal_data, 0);
    if magic != 0x377f0682 && magic != 0x377f0683 {
        return Vec::new(); // 非 WAL 文件 / 已重置
    }
    let page_size = wal_be_u32(wal_data, 8) as usize;
    if page_size != PAGE_SZ {
        return Vec::new(); // 页大小不匹配，跳过（不应发生）
    }
    let salt1 = wal_be_u32(wal_data, 16);
    let salt2 = wal_be_u32(wal_data, 20);
    let (h0, h1) = wal_checksum_run(&wal_data[..24], magic, 0, 0);
    if h0 != wal_be_u32(wal_data, 24) || h1 != wal_be_u32(wal_data, 28) {
        return Vec::new(); // 头部校验和不符：空/陈旧 WAL
    }
    let frame_sz = WAL_FRAME_HDR_SZ + PAGE_SZ;
    let n_frames = (wal_data.len() - WAL_HDR_SZ) / frame_sz;
    if n_frames == 0 {
        return Vec::new();
    }

    let mut s0 = h0;
    let mut s1 = h1;
    let mut frames: Vec<(u32, u32, usize)> = Vec::new(); // (pgno, commit_size, body_offset)
    let mut last_commit: Option<usize> = None;
    for idx in 0..n_frames {
        let off = WAL_HDR_SZ + idx * frame_sz;
        let hdr = &wal_data[off..off + WAL_FRAME_HDR_SZ];
        let pgno = wal_be_u32(hdr, 0);
        if pgno == 0 {
            break; // SQLite 视 pgno=0 帧为无效，扫描到此结束
        }
        let commit = wal_be_u32(hdr, 4);
        if wal_be_u32(hdr, 8) != salt1 || wal_be_u32(hdr, 12) != salt2 {
            break; // 盐不匹配：剩余帧属于其他检查点世代
        }
        // 帧校验和仅覆盖帧头前 8 字节（pgno+commit）与整页数据（与 SQLite 一致）
        let (c0, c1) = wal_checksum_run(&hdr[..8], magic, s0, s1);
        let body = &wal_data[off + WAL_FRAME_HDR_SZ..off + frame_sz];
        let (b0, b1) = wal_checksum_run(body, magic, c0, c1);
        if b0 != wal_be_u32(hdr, 16) || b1 != wal_be_u32(hdr, 20) {
            break; // 校验和不符：文件截断或损坏
        }
        s0 = b0;
        s1 = b1;
        frames.push((pgno, commit, off + WAL_FRAME_HDR_SZ));
        if commit > 0 {
            last_commit = Some(frames.len() - 1);
        }
    }
    match last_commit {
        Some(end) => {
            frames.truncate(end + 1); // 最后提交帧之后的帧属未提交事务，丢弃
            frames
        }
        None => Vec::new(), // 无已提交事务：整体回滚，主库文件即权威
    }
}

/// WAL 里「待合并的已提交帧」数量（0 = 没有需要合并的内容）。
/// 读取视图据此决定是否要复制快照（复制 279 MB 只为合并时会显得很亏，所以先探一下）。
pub fn wal_pending_frames(wal: &Path) -> usize {
    let Ok(data) = std::fs::read(wal) else {
        return 0;
    };
    wal_committed_frames(&data).len()
}

/// 把 WAL 中「最后一个已提交事务」的帧解密合并进明文副本（页面覆盖写）。
/// WAL 缺失 / 头部无效 / 页大小不符 / 无有效提交帧时静默跳过，返回 Ok(0)。
/// 帧有效性按 SQLite 规则判定：盐与头部一致 + 连续校验和一致；
/// 截断/损坏处即停止，最后提交帧之后的帧属未提交事务，一并忽略。
/// 供解密扫描 / 删除流程共用：Trae 运行中新增/删除的会话在 WAL 里，
/// 不合并则解密快照停留在上次 checkpoint，列表看不到最新变化。
pub fn merge_wal_into_plain(plain: &Path, wal: &Path, enc_key_hex: &str) -> Result<usize, String> {
    let wal_data = match std::fs::read(wal) {
        Ok(d) => d,
        Err(_) => return Ok(0),
    };
    let frames = wal_committed_frames(&wal_data);
    if frames.is_empty() {
        return Ok(0);
    }

    let enc_key = hex_to_bytes(enc_key_hex.trim());
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(plain)
        .map_err(|e| format!("打开明文副本合并 WAL 失败: {e}"))?;
    let mut file_pages = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? / PAGE_SZ as u64;
    let mut merged = 0usize;
    let mut commit_pages: u64 = 0;
    for (pgno, commit, body_off) in &frames {
        let body = &wal_data[*body_off..*body_off + PAGE_SZ];
        let dec = decrypt_page(&enc_key, body, *pgno as u64);
        while file_pages < *pgno as u64 {
            f.seek(SeekFrom::Start(file_pages * PAGE_SZ as u64))
                .map_err(|e| e.to_string())?;
            f.write_all(&[0u8; PAGE_SZ])
                .map_err(|e| format!("扩展明文副本失败: {e}"))?;
            file_pages += 1;
        }
        f.seek(SeekFrom::Start((*pgno as u64 - 1) * PAGE_SZ as u64))
            .map_err(|e| e.to_string())?;
        f.write_all(&dec).map_err(|e| format!("合并 WAL 帧页 {pgno} 失败: {e}"))?;
        merged += 1;
        if *commit > 0 {
            commit_pages = *commit as u64;
        }
    }
    // 提交帧声明的库大小大于当前明文副本时补齐（库增长场景）
    while commit_pages > file_pages {
        f.seek(SeekFrom::Start(file_pages * PAGE_SZ as u64))
            .map_err(|e| e.to_string())?;
        f.write_all(&[0u8; PAGE_SZ])
            .map_err(|e| format!("扩展明文副本失败: {e}"))?;
        file_pages += 1;
    }
    f.flush().map_err(|e| format!("刷新明文副本失败: {e}"))?;
    Ok(merged)
}

/// 把库里的表分成两组：(带 session_id 列的表, 仅带 message_id 列的表)。
fn classify_tables(conn: &Connection) -> Result<(Vec<String>, Vec<String>), String> {
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .map_err(|e| e.to_string())?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut with_sid = Vec::new();
    let mut mid_only = Vec::new();
    for name in names {
        if name.starts_with("sqlite_") {
            continue;
        }
        let cols: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{name}\")"))
            .map_err(|e| e.to_string())?
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        if cols.iter().any(|c| c == "session_id") {
            with_sid.push(name);
        } else if cols.iter().any(|c| c == "message_id") {
            mid_only.push(name);
        }
    }
    Ok((with_sid, mid_only))
}

/// 会话在实时库各表的行数 / 删除行数。顺序敏感：先删仅 message_id 的表。
/// 返回 (表名 → 行数或错误说明)。
fn rows_for(
    conn: &Connection,
    sid: &str,
    tables_with_sid: &[String],
    tables_mid_only: &[String],
    delete: bool,
) -> std::collections::HashMap<String, Value> {
    let mut out = std::collections::HashMap::new();
    let mut plan: Vec<(String, String)> = Vec::new(); // (表名, where 子句)
    for t in tables_mid_only {
        plan.push((
            t.clone(),
            format!(
                "\"{t}\" WHERE message_id IN \
                 (SELECT message_id FROM chat_message WHERE session_id=?)"
            ),
        ));
    }
    for t in tables_with_sid {
        plan.push((t.clone(), format!("\"{t}\" WHERE session_id=?")));
    }
    for (t, where_clause) in plan {
        let sql = if delete {
            format!("DELETE FROM {where_clause}")
        } else {
            format!("SELECT count(*) FROM {where_clause}")
        };
        let r: Result<Value, rusqlite::Error> = if delete {
            conn.execute(&sql, [sid]).map(|n| json!(n)).map_err(|e| e)
        } else {
            conn.query_row(&sql, [sid], |row| row.get::<_, i64>(0))
                .map(|n| json!(n))
                .map_err(|e| e)
        };
        match r {
            Ok(v) => out.insert(t, v),
            Err(e) => out.insert(t, json!(format!("跳过({e})"))),
        };
    }
    out
}

/// 该会话在磁盘上的文件/目录：snapshot git 目录 + 全局附加目录里的同名子目录。
fn collect_session_files(client_key: &str, sid: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let client = match get_client(client_key) {
        Some(c) => c,
        None => return out,
    };
    let snap = snapshot_root(client).join(sid);
    if snap.exists() {
        out.push(snap);
    }
    for root in extra_roots() {
        if !root.exists() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().contains(sid) {
                    out.push(e.path());
                }
            }
        }
    }
    out
}

fn dir_size_mb(p: &Path) -> f64 {
    let mut total: u64 = 0;
    if p.is_file() {
        total += p.metadata().map(|m| m.len()).unwrap_or(0);
    } else if let Ok(entries) = std::fs::read_dir(p) {
        for e in entries.flatten() {
            let q = e.path();
            if q.is_dir() {
                total += dir_size_mb(&q) as u64 * 1024 * 1024;
                let _ = total;
            } else {
                total += q.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total as f64 / 1024.0 / 1024.0
}

/// 会话标题（解密库内查询；不存在返回错误）。
fn check_session_exists(client_key: &str, sid: &str) -> Result<String, String> {
    let conn = open_decrypted(client_key)?;
    let title = conn
        .query_row(
            "SELECT session_title FROM chat_session WHERE session_id=?",
            [sid],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| "会话不在当前数据源中（可能已删除）".to_string())?;
    Ok(title.trim().to_string())
}

/// 删除预览：会话标题、各表行数（以解密库为准）、磁盘文件。只读，不删任何东西。
pub fn delete_info(client_key: &str, session_id: &str) -> Result<Value, String> {
    if !is_session_id(session_id) {
        return Err("Session ID 格式不正确（20~24 位十六进制）".into());
    }
    let title = check_session_exists(client_key, session_id)?;

    let tables: Vec<Value> = match open_decrypted(client_key) {
        Ok(conn) => {
            let (ws, mo) = classify_tables(&conn)?;
            let rows = rows_for(&conn, session_id, &ws, &mo, false);
            rows.into_iter()
                .map(|(name, v)| json!({ "name": name, "count": v.as_i64().unwrap_or(0) }))
                .collect()
        }
        Err(e) => return Err(e),
    };

    let files: Vec<Value> = collect_session_files(client_key, session_id)
        .into_iter()
        .map(|p| {
            json!({
                "path": p.to_string_lossy().into_owned(),
                "size_mb": (dir_size_mb(&p) * 10.0).round() / 10.0,
            })
        })
        .collect();

    let owner_uid = session_owner_uid(client_key, session_id).unwrap_or_default();
    Ok(json!({
        "ok": true,
        "session_id": session_id,
        "source": label_of(client_key),
        "title": title,
        "owner_uid": owner_uid,
        "owner_label": if owner_uid.is_empty() {
            "（无归属）".to_string()
        } else {
            account_label(client_key, &owner_uid)
        },
        "cloud_credential": !owner_uid.is_empty() && has_cloud_credential(client_key, &owner_uid),
        "tables": tables,
        "files": files,
        "live_ok": true,
        "note": "删除 = 整库备份 → 解密副本删行 → 加密回写替换实时库 → 同步删解密库 → 会话文件移入回收站目录（可恢复）",
    }))
}

/// 批量删除的执行结果（内部）。
struct BatchOutcome {
    /// `(session_id, 标题, 各表删除行数, 归档成功文件数)`
    deleted: Vec<(String, String, std::collections::HashMap<String, Value>, usize)>,
    moved_detail: Vec<String>,
    wal_merged: usize,
    relaunched: bool,
    backup: Vec<String>,
    trash_dirs: Vec<String>,
}

/// 批量删除的公共实现：**一趟**完成密钥解析 → 结束客户端 → 解密 + 合并 WAL →
/// 批量删行 → 增量回写 → 备份一次 + 原子替换 → 归档文件 → 重启。
///
/// 为什么要批量：单会话删除每次都要整库备份（每批 ≈ 一个整库大小）。逐条删除几十个
/// 空会话会累积出几十份备份（实测 `trae/backup` 曾堆到 2.2 GB）。批量走一趟，
/// 只做一次解密与一次增量回写，备份只留 1 份。
/// 在一个**已打开的明文副本连接**上删掉某个会话的全部行。
///
/// 与 [`delete_sessions_core`] 的区别：那个函数自己管「结束客户端 → 解密 → 改 → 加密回写 →
/// 替换」的全套流程；本函数**只做「删行」这一步**，供需要把「删旧 + 写新」放在**同一次**
/// 回写里完成的调用方复用（目前是 `trae_import::sync_group` 的差异同步）。
///
/// 复用 [`classify_tables`] 的「按列名找表」策略：只要表带 `session_id` 或
/// `message_id` 就会被清掉 —— Trae 各版本表结构不一，写死表名列表必然漏表。
///
/// 返回实际删除的行数合计。
pub fn delete_rows_in_conn(conn: &Connection, sid: &str) -> Result<usize, String> {
    let (tables_with_sid, tables_mid_only) = classify_tables(conn)?;
    let plan = rows_for(conn, sid, &tables_with_sid, &tables_mid_only, false);
    // 先删仅 message_id 的表（它引用 chat_message.message_id，必须在外键来源之前删）
    let mut total = 0usize;
    for t in &tables_mid_only {
        // 表不存在该会话时不报错，跳过即可（rows_for 已在 count 阶段容错）
        if plan.get(t).and_then(|v| v.as_i64()).unwrap_or(0) == 0 {
            continue;
        }
        let sql = format!(
            "DELETE FROM \"{t}\" WHERE message_id IN \
             (SELECT message_id FROM chat_message WHERE session_id=?)"
        );
        total += conn
            .execute(&sql, [sid])
            .map_err(|e| format!("清除 {t} 失败: {e}"))?;
    }
    for t in &tables_with_sid {
        if plan.get(t).and_then(|v| v.as_i64()).unwrap_or(0) == 0 {
            continue;
        }
        let sql = format!("DELETE FROM \"{t}\" WHERE session_id=?");
        total += conn
            .execute(&sql, [sid])
            .map_err(|e| format!("清除 {t} 失败: {e}"))?;
    }
    Ok(total)
}

fn delete_sessions_core(
    client_key: &str,
    session_ids: &[String],
    log: &dyn Fn(&str),
) -> Result<BatchOutcome, String> {
    // 去重并保持顺序：同一会话传两次不该删两遍
    let mut uniq: Vec<String> = Vec::new();
    {
        let mut seen = std::collections::BTreeSet::new();
        for id in session_ids {
            if !is_session_id(id) {
                return Err(format!("Session ID 格式不正确（20~24 位十六进制）：{id}"));
            }
            if seen.insert(id.clone()) {
                uniq.push(id.clone());
            }
        }
    }
    if uniq.is_empty() {
        return Err("没有选中任何会话".to_string());
    }

    // 标题先取：删除后库里的行就没了（也顺带校验会话确实存在）
    let mut titles: Vec<String> = Vec::with_capacity(uniq.len());
    for id in &uniq {
        titles.push(check_session_exists(client_key, id)?);
    }

    let client = get_client(client_key).ok_or("未知客户端")?;
    let live = database_path(client);
    if !live.exists() {
        return Err(format!("实时加密库不存在：{}", live.display()));
    }
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let work = store_dir()
        .join("trae")
        .join("delete_tmp")
        .join(format!("{client_key}-{stamp}"));
    let cleanup = |work: &Path| {
        let _ = std::fs::remove_dir_all(work);
    };
    let err = |e: String| -> Result<BatchOutcome, String> {
        cleanup(&work);
        Err(e)
    };

    // 0. 解析实时库密钥：存盘密钥按首页 HMAC 校验，过期则从进程内存自动重扫
    log("校验实时库密钥…");
    let enc_key_hex = match resolve_live_key(client_key, &live) {
        Ok(k) => k,
        Err(e) => return Err(e),
    };

    // 1. 结束客户端进程（文件替换前提；WAL 残留帧由第 3 步合并，不会丢数据）
    if is_running(client) {
        log("结束客户端进程…");
        let killed = kill_all(client);
        log(&format!("已结束 {} 个进程，等待退出…", killed.len()));
        if !wait_until_stopped(client, 10_000) {
            return Err("客户端未能退出，无法替换数据库".into());
        }
    }

    // 2. 解密实时库为明文副本
    log("解密实时库为明文副本（可能数百 MB，请稍候）…");
    let plain = work.join("target-plain.db");
    if let Err(e) = decrypt_database(&live, &enc_key_hex, &plain, Some(&|m| log(&format!("   {m}")))) {
        return err(e);
    }

    // 3. 合并 WAL 已提交帧（防丢其他会话最新数据）
    let wal = live.with_extension("db-wal");
    let wal_merged = match merge_wal_into_plain(&plain, &wal, &enc_key_hex) {
        Ok(n) => n,
        Err(e) => return err(e),
    };
    if wal_merged > 0 {
        log(&format!("已合并 WAL 中 {wal_merged} 个已提交页面帧"));
    }
    if let Err(e) = patch_reserved_field(&plain) {
        return err(e);
    }

    // 4. 明文副本上删行（**单个事务**包住全部会话：要么全删，要么一行不动）
    log(&format!("删除 {} 个会话的数据…", uniq.len()));
    let per_session_rows: Vec<std::collections::HashMap<String, Value>> = {
        let mut conn = match Connection::open(&plain) {
            Ok(c) => c,
            Err(e) => return err(format!("打开明文副本失败: {e}")),
        };
        let (ws, mo) = match classify_tables(&conn) {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(e) => return err(format!("开启事务失败: {e}")),
        };
        let mut counts = Vec::with_capacity(uniq.len());
        for id in &uniq {
            counts.push(rows_for(&tx, id, &ws, &mo, true));
        }
        if let Err(e) = tx.commit() {
            return err(format!("提交失败: {e}"));
        }
        counts
    };

    // 5. 删除期间**不动解密快照**：快照必须保持「实时库的纯解密结果」这一不变量，
    //    导入流程拿它当差异比对基准。等替换成功后再用新明文整体替换快照（见第 7 步）。

    // 6. 增量加密回写：只重写真正变动的页，其余页直接沿用原密文
    log("增量加密回写（只重写变动页）…");
    let new_db = work.join("target-new.db");
    let stats = match crate::modules::trae_import::write_db_incremental(
        &enc_key_hex,
        &live,
        &plain,
        &new_db,
        Some(&|m| log(&format!("   {m}"))),
    ) {
        Ok(s) => s,
        Err(e) => return err(e),
    };
    log(&format!(
        "增量回写完成：全库 {} 页，仅重写 {} 页（{:.1} MB），用时 {} ms",
        stats.pages,
        stats.changed_pages,
        stats.changed_bytes as f64 / 1_048_576.0,
        stats.elapsed_ms
    ));

    // 7. 备份原库（含 wal/shm）→ 原子替换
    log("备份原库并原子替换…");
    let bdir = backup_dir();
    if let Err(e) = std::fs::create_dir_all(&bdir) {
        return err(format!("创建备份目录失败: {e}"));
    }
    let mut backup_paths: Vec<String> = Vec::new();
    for (label, suffix) in [("主库", ""), ("WAL", "-wal"), ("SHM", "-shm")] {
        let f = PathBuf::from(format!("{}{}", live.to_string_lossy(), suffix));
        if f.exists() {
            let dst = bdir.join(format!("{client_key}_before_delete_{stamp}{suffix}.db.bak"));
            match std::fs::copy(&f, &dst) {
                Ok(_) => backup_paths.push(dst.to_string_lossy().into_owned()),
                Err(e) => return err(format!("备份{label}失败，已中止删除：{e}")),
            }
        }
    }
    // 清理目录里更旧的整库备份：每批 ≈ 一个整库大小，留着会越攒越多（实测曾累积到 2.2 GB）。
    // 刚写下的这份**就是**删除前的完整状态，保留它即可。
    {
        let (n, freed) = crate::modules::trae_export::prune_backups(&bdir, 1);
        if n > 0 {
            log(&format!(
                "已清理 {n} 个旧备份，回收 {:.1} MB（仅保留最新 1 批）",
                freed as f64 / 1_048_576.0
            ));
        }
    }
    let swap = || -> Result<(), String> {
        let shm = live.with_extension("db-shm");
        if wal.exists() {
            std::fs::remove_file(&wal).map_err(|e| format!("清理 WAL 失败: {e}"))?;
        }
        if shm.exists() {
            std::fs::remove_file(&shm).map_err(|e| format!("清理 SHM 失败: {e}"))?;
        }
        if live.exists() {
            std::fs::remove_file(&live).map_err(|e| format!("移除旧库失败: {e}"))?;
        }
        std::fs::rename(&new_db, &live).map_err(|e| format!("替换数据库失败: {e}"))?;
        Ok(())
    };
    if let Err(e) = swap() {
        return err(format!("替换失败（备份已保留，可手动恢复）：{e}"));
    }

    // 7.1 把删除后的明文提升为新快照：它就是新库的完整明文（增量回写逐页已校验），
    //     提升后列表 / 导出 / 下次导入都无需再整库解密。
    if let Err(e) = crate::modules::trae_export::promote_snapshot(client_key, &plain) {
        log(&format!("快照更新失败（不影响删除结果，下次读取会重新解密）：{e}"));
        crate::modules::trae_export::drop_snapshot_meta(client_key);
    }

    // 8. 会话文件移入回收站目录（可恢复）
    log("归档会话文件…");
    let mut moved_detail: Vec<String> = Vec::new();
    let mut trash_dirs: Vec<String> = Vec::new();
    let mut moved_counts: Vec<usize> = Vec::with_capacity(uniq.len());
    for id in &uniq {
        let paths = collect_session_files(client_key, id);
        if paths.is_empty() {
            moved_counts.push(0);
            continue;
        }
        let dest_root = deleted_sessions_dir().join(format!("{id}_{stamp}"));
        if let Err(e) = std::fs::create_dir_all(&dest_root) {
            return err(format!("创建回收站目录失败: {e}"));
        }
        trash_dirs.push(dest_root.to_string_lossy().into_owned());
        let mut ok_count = 0usize;
        for p in paths {
            let dest = dest_root.join(p.file_name().unwrap_or_default());
            match std::fs::rename(&p, &dest) {
                Ok(_) => {
                    ok_count += 1;
                    moved_detail.push(p.to_string_lossy().into_owned());
                }
                Err(e) => moved_detail.push(format!("{} (移动失败: {e})", p.display())),
            }
        }
        moved_counts.push(ok_count);
    }

    // 9. 删除期间结束过客户端进程，成功后自动重启，保证删除结果立即可见
    let relaunched = match launch(client_key, None) {
        Ok(_) => {
            log("删除成功，已自动重启客户端");
            true
        }
        Err(e) => {
            log(&format!("删除成功，但自动重启客户端失败：{e}"));
            false
        }
    };

    cleanup(&work);
    log("删除完成");
    Ok(BatchOutcome {
        deleted: uniq
            .into_iter()
            .zip(titles)
            .zip(per_session_rows)
            .zip(moved_counts)
            .map(|(((sid, title), rows), moved)| (sid, title, rows, moved))
            .collect(),
        moved_detail,
        wal_merged,
        relaunched,
        backup: backup_paths,
        trash_dirs,
    })
}

/// 彻底删除会话：密钥校验/刷新 → 结束客户端 → 解密副本+合并 WAL → 删行 →
/// 加密回写 → 备份 + 原子替换 → 同步删解密库 → 会话文件归档。
pub fn delete_session(
    client_key: &str,
    session_id: &str,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(cb) = on_progress {
            cb(m);
        }
    };
    let out = delete_sessions_core(client_key, &[session_id.to_string()], &log)?;
    let (_, title, deleted_rows, moved_n) = out.deleted.into_iter().next().unwrap_or_else(|| {
        (
            session_id.to_string(),
            String::new(),
            std::collections::HashMap::new(),
            0,
        )
    });
    Ok(json!({
        "ok": true,
        "session_id": session_id,
        "title": title,
        "source": label_of(client_key),
        "deleted_rows": deleted_rows,
        "wal_merged": out.wal_merged,
        "relaunched": out.relaunched,
        "moved_files": moved_n,
        "moved_detail": out.moved_detail,
        "backup": out.backup,
        "trash_dir": out.trash_dirs.into_iter().next().unwrap_or_default(),
        "hint": if out.relaunched { "已自动重启客户端，列表应为最新状态" } else { "客户端未自动重启，可手动启动" },
    }))
}

/// **批量**彻底删除多个会话：与 [`delete_session`] 同一条链路，但只做一趟 ——
/// 一次解密、一次增量回写、**一份**整库备份。
///
/// 用于「Trae 清理」页的多选删除（例如一次性清掉几十个空会话）。逐条调用
/// `delete_session` 会让备份目录按整库大小线性膨胀。
pub fn delete_sessions(
    client_key: &str,
    session_ids: &[String],
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(cb) = on_progress {
            cb(m);
        }
    };
    let out = delete_sessions_core(client_key, session_ids, &log)?;
    let total_rows: i64 = out
        .deleted
        .iter()
        .flat_map(|(_, _, rows, _)| rows.values())
        .filter_map(Value::as_i64)
        .sum();
    Ok(json!({
        "ok": true,
        "source": label_of(client_key),
        "count": out.deleted.len(),
        "sessions": out.deleted.iter().map(|(sid, title, rows, moved)| json!({
            "session_id": sid,
            "title": title,
            "deleted_rows": rows,
            "moved_files": moved,
        })).collect::<Vec<_>>(),
        "deleted_rows_total": total_rows,
        "wal_merged": out.wal_merged,
        "relaunched": out.relaunched,
        "moved_detail": out.moved_detail,
        "backup": out.backup,
        "trash_dirs": out.trash_dirs,
        "hint": if out.relaunched { "已自动重启客户端，列表应为最新状态" } else { "客户端未自动重启，可手动启动" },
    }))
}

/// 密钥是否可用（删除预检提示用）：存盘密钥存在且与实时库首页 HMAC 匹配。
pub fn has_key(client_key: &str) -> bool {
    let Some(k) = load_saved_key(client_key) else {
        return false;
    };
    if k.trim().len() != 64 {
        return false;
    }
    let Some(client) = get_client(client_key) else {
        return false;
    };
    let db = database_path(client);
    let Ok(mut f) = std::fs::File::open(&db) else {
        return false;
    };
    let mut page1 = [0u8; PAGE_SZ];
    if f.read_exact(&mut page1).is_err() {
        return false;
    }
    verify_page1_hmac(&hex_to_bytes(&k), &page1)
}
