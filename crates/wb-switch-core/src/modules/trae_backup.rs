//! Trae 会话库的**写前备份**：页级增量 + 原子替换 + 可校验还原。
//!
//! ## 为什么能只备份「受影响的部分」
//!
//! Trae 的 `database.db` 是**按页加密**的（4096 B 一页，逐页独立加解密），而所有写库路径
//! 都走 `trae_import::write_db_incremental*` —— 它先把原库密文**整份照抄**，再只把「变了的页」
//! 重新加密写回去。于是：
//!
//! ```text
//! 新库与原库的差异，恰好就是那几个变动页（其余页逐字节相同）
//! ```
//!
//! 所以「备份原库」根本不必复制 609 MB：**只存变动页的原密文**就够了，
//! 还原 = 把当前库的那几页换回原样。实测本机 609 MB 的库，删几个会话的备份从 609 MB
//! 降到几 MB 量级。这也是 WorkBuddy 那边的既有做法（它只备份**被改的那一个**会话正文文件），
//! 只是它的库本身只有 0.2 MB，所以看不出来。
//!
//! ## ⚠️ 增量备份的边界（必须如实告诉用户）
//!
//! 增量的基准是「**这次操作写出的那个新库**」。若之后 Trae 自己又改过库（它会），当前库
//! 就不再等于那个基准 ⇒ 还原会被**明确拒绝**，绝不会糊里糊涂拼出一个坏库。
//! `meta.json` 同时记了基准与结果的 sha256：**还原前验基准、还原后验结果**，两道都过才落盘。
//! 所以增量备份的语义是「**撤销那一次操作**」，不是「随时回到任意历史版本」。
//!
//! ## 开关：默认**不备份**
//!
//! 整库有 600 MB 量级，每写一次库就复制一份，实测能堆到几 GB。所以
//! [`snapshot_before_write`] 会先问 `settings::backup_before_write()`，**默认关闭**。
//! 关闭时返回 `mode = "skipped"`（`Ok`，不是错），界面与日志要把「未备份（设置已关闭）」
//! 和「备份失败」分得清清楚楚。
//!
//! ## 原子替换：让「不丢库」不再依赖备份
//!
//! 备份开关默认是关的，那「替换到一半失败」岂不是丢库？不会 —— [`swap_in_place`] 用
//! `rename(旧库 → 旧库.tsold)` + `rename(新库 → 库位)` 两步走，任何一步失败都会**自动回滚**。
//! 同目录改名是原子的，所以「防丢失」由替换本身保证，备份只负责「留历史版本」。

use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// 与 `trae_decrypt` / `trae_import` / `trae_delete` 同一个页大小。
///
/// ⚠️ Trae 的库是**定长页**结构（大小必须是它的整数倍），改动这里等于改库格式。
pub const PAGE_SZ: u64 = 4096;

/// 比对/复制的块大小（4 MB）：既不让 syscall 太多，也不吃内存。
const CHUNK: usize = 1024 * PAGE_SZ as usize;

/// 增量文件头长度：magic(8) + ver(4) + page_size(4) + orig_pages(8) + new_pages(8) + changed(8)。
const HEADER_LEN: u64 = 40;
const MAGIC: &[u8; 8] = b"TSWDLTA1";
const FORMAT_VERSION: u32 = 1;

/// 增量文件 / 整份文件 / 元信息在备份目录里的固定名字。
pub const DELTA_FILE: &str = "delta.bin";
pub const FULL_FILE: &str = "database.db";
pub const META_FILE: &str = "meta.json";

/// 备份条目名（**唯一出口**）。
///
/// ⚠️ 必须与 `trae_export::backup_owner_and_stamp` 认得的形状一致：
/// `<客户端>_before_<动作>_<YYYYMMDD_HHMMSS>` —— 保留策略靠它把同一客户端的批次归到一组。
/// 命名里换个词、少一段，就会让每份备份自成一个 owner ⇒ 一份都清不掉（T62 踩过）。
pub fn entry_name(client_key: &str, action: &str, stamp: &str) -> String {
    format!("{client_key}_before_{action}_{stamp}")
}

/// 当前时间戳（与既有备份命名同格式）。
pub fn new_stamp() -> String {
    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
}

/// 给路径加后缀（`a.db` + `-wal` → `a.db-wal`）。
fn suffixed(p: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", p.to_string_lossy(), suffix))
}

fn read_exact_at(f: &mut std::fs::File, off: u64, buf: &mut [u8]) -> Result<(), String> {
    f.seek(SeekFrom::Start(off)).map_err(|e| format!("定位读取失败: {e}"))?;
    f.read_exact(buf).map_err(|e| format!("读取失败: {e}"))
}

/// 文件 sha256（流式，1 MB 缓冲）。
pub fn sha256_file(p: &Path) -> Result<String, String> {
    let f = std::fs::File::open(p).map_err(|e| format!("打开 {} 失败: {e}", p.display()))?;
    let mut r = std::io::BufReader::with_capacity(1 << 20, f);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf).map_err(|e| format!("读取失败: {e}"))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// 取文件页数。**不是页整数倍就是结构异常**，直接报错而不是猜。
fn pages_of(p: &Path) -> Result<u64, String> {
    let len = std::fs::metadata(p).map_err(|e| format!("读取 {} 大小失败: {e}", p.display()))?.len();
    if len == 0 || len % PAGE_SZ != 0 {
        return Err(format!("{} 大小 {len} 不是 {PAGE_SZ} 的整数倍，结构异常", p.display()));
    }
    Ok(len / PAGE_SZ)
}

// ---------------------------------------------------------------------------
// 写备份
// ---------------------------------------------------------------------------

/// 写一份备份：能增量就增量，不行就整份复制。
///
/// `orig_db` 是**改动前**的实时库，`new_db` 是本次要写进去的新库（调用方随后会用
/// [`swap_in_place`] 把它换上去）。`dir` 是**这一份备份自己的目录**（调用方决定落在哪个
/// 备份根目录下、叫什么名字，见 [`entry_name`]）。
///
/// 返回的形状（给 Tauri 层与界面用）：
/// ```json
/// { "dir": "...", "mode": "delta"|"full", "bytes": 3145728,
///   "fullBytes": 639000000, "changedPages": 12, "totalPages": 156000,
///   "savedBytes": 635854288, "reason": "..." }
/// ```
pub fn write_snapshot(
    orig_db: &Path,
    new_db: &Path,
    dir: &Path,
    client_key: &str,
    action: &str,
    note: &str,
) -> Result<Value, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let orig_pages = pages_of(orig_db)?;
    let new_pages = pages_of(new_db)?;

    let orig_len = orig_pages * PAGE_SZ;
    let new_len = new_pages * PAGE_SZ;
    let common = orig_len.min(new_len);

    // 增量体先落到临时文件；头 40 字节占位，扫描结束后回填。
    let delta_path = dir.join(DELTA_FILE);
    let tmp = dir.join("delta.bin.tmp");
    let mut w = BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp).map_err(|e| format!("创建增量文件失败: {e}"))?);
    w.write_all(&[0u8; HEADER_LEN as usize]).map_err(|e| e.to_string())?;

    let mut fo = std::fs::File::open(orig_db).map_err(|e| format!("打开原库失败: {e}"))?;
    let mut fnw = std::fs::File::open(new_db).map_err(|e| format!("打开新库失败: {e}"))?;
    let mut ho = Sha256::new();
    let mut hn = Sha256::new();
    let mut ob = vec![0u8; CHUNK];
    let mut nb = vec![0u8; CHUNK];
    let ps = PAGE_SZ as usize;

    let mut changed: u64 = 0;
    let mut off: u64 = 0;
    while off < common {
        let n = ((common - off) as usize).min(CHUNK);
        read_exact_at(&mut fo, off, &mut ob[..n])?;
        read_exact_at(&mut fnw, off, &mut nb[..n])?;
        ho.update(&ob[..n]);
        hn.update(&nb[..n]);
        let mut k = 0usize;
        while k < n {
            if ob[k..k + ps] != nb[k..k + ps] {
                changed += 1;
                let pgno = (off + k as u64) / PAGE_SZ + 1;
                w.write_all(&pgno.to_le_bytes()).map_err(|e| e.to_string())?;
                w.write_all(&ob[k..k + ps]).map_err(|e| e.to_string())?;
            }
            k += ps;
        }
        off += n as u64;
    }

    // 原库比新库长的那截：那些页在新库里**不存在**（被截断），还原时必须原样补回来，
    // 所以它们一律算「变动页」。⚠️ 少写这几页 = 还原出来的库缺一截，还验不过 sha。
    if orig_len > common {
        let mut o = common;
        while o < orig_len {
            let n = ((orig_len - o) as usize).min(CHUNK);
            read_exact_at(&mut fo, o, &mut ob[..n])?;
            ho.update(&ob[..n]);
            let mut k = 0usize;
            while k < n {
                changed += 1;
                let pgno = (o + k as u64) / PAGE_SZ + 1;
                w.write_all(&pgno.to_le_bytes()).map_err(|e| e.to_string())?;
                w.write_all(&ob[k..k + ps]).map_err(|e| e.to_string())?;
                k += ps;
            }
            o += n as u64;
        }
    }
    // 新库比原库长的那截不用存（还原时按 `orig_pages` 截断即可），但哈希要算全。
    if new_len > common {
        let mut o = common;
        while o < new_len {
            let n = ((new_len - o) as usize).min(CHUNK);
            read_exact_at(&mut fnw, o, &mut nb[..n])?;
            hn.update(&nb[..n]);
            o += n as u64;
        }
    }
    w.flush().map_err(|e| e.to_string())?;
    drop(w);

    let orig_sha = hex(&ho.finalize());
    let new_sha = hex(&hn.finalize());
    let delta_bytes = HEADER_LEN + changed * (8 + PAGE_SZ);

    // 变动页超过一半 ⇒ 增量根本省不下空间，退回整份复制。
    // （整份复制不依赖基准，任何时候都能还原，语义更简单，没必要为了几 MB 多一层不确定性。）
    if changed * PAGE_SZ * 2 >= orig_len {
        let _ = std::fs::remove_file(&tmp);
        let mut out = copy_full(orig_db, dir, client_key, action, note)?;
        if let Some(o) = out.as_object_mut() {
            o.insert(
                "reason".into(),
                json!(format!("变动页占 {:.0}%，改用整份备份", changed as f64 * 100.0 / orig_pages as f64)),
            );
        }
        return Ok(out);
    }

    // 回填文件头，再原子改名成 delta.bin。
    {
        let mut f = std::fs::OpenOptions::new().write(true).open(&tmp).map_err(|e| e.to_string())?;
        let mut hdr: Vec<u8> = Vec::with_capacity(HEADER_LEN as usize);
        hdr.extend_from_slice(MAGIC);
        hdr.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        hdr.extend_from_slice(&(PAGE_SZ as u32).to_le_bytes());
        hdr.extend_from_slice(&orig_pages.to_le_bytes());
        hdr.extend_from_slice(&new_pages.to_le_bytes());
        hdr.extend_from_slice(&changed.to_le_bytes());
        f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        f.write_all(&hdr).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(&delta_path);
    std::fs::rename(&tmp, &delta_path).map_err(|e| format!("落盘增量失败: {e}"))?;
    // 整份模式下留下的 database.db 必须清掉，否则还原时不知道该信谁。
    let _ = std::fs::remove_file(dir.join(FULL_FILE));

    let meta = json!({
        "format": "twin-switch.trae-backup",
        "version": FORMAT_VERSION,
        "mode": "delta",
        "clientKey": client_key,
        "action": action,
        "note": note,
        "createdAt": crate::modules::config::now_ms(),
        "pageSize": PAGE_SZ,
        "original": { "bytes": orig_len, "pages": orig_pages, "sha256": orig_sha },
        "base": { "bytes": new_len, "pages": new_pages, "sha256": new_sha },
        "delta": { "changedPages": changed, "bytes": delta_bytes },
    });
    write_meta(dir, &meta)?;

    Ok(json!({
        "dir": dir.to_string_lossy(),
        "mode": "delta",
        "bytes": delta_bytes,
        "fullBytes": orig_len,
        "changedPages": changed,
        "totalPages": orig_pages,
        "savedBytes": orig_len.saturating_sub(delta_bytes),
        "note": note,
    }))
}

/// 整份复制备一份（含 `-wal`；`-shm` 是运行时缓存，不备）。
pub fn copy_full(
    db: &Path,
    dir: &Path,
    client_key: &str,
    action: &str,
    note: &str,
) -> Result<Value, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let dst = dir.join(FULL_FILE);
    std::fs::copy(db, &dst).map_err(|e| format!("备份原库失败: {e}"))?;
    let wal = suffixed(db, "-wal");
    if wal.is_file() {
        std::fs::copy(&wal, dir.join("database.db-wal")).map_err(|e| format!("备份 WAL 失败: {e}"))?;
    }
    let bytes = std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
    let sha = sha256_file(&dst)?;
    // 覆盖写过的目录里可能还留着旧的 delta.bin，不清掉就会「两份备份打架」。
    let _ = std::fs::remove_file(dir.join(DELTA_FILE));
    if let Ok(text) = std::fs::read_to_string(dir.join(META_FILE)) {
        if let Ok(old) = serde_json::from_str::<Value>(&text) {
            if old.get("mode").and_then(Value::as_str) == Some("delta") {
                let _ = std::fs::remove_file(dir.join(META_FILE));
            }
        }
    }
    let meta = json!({
        "format": "twin-switch.trae-backup",
        "version": FORMAT_VERSION,
        "mode": "full",
        "clientKey": client_key,
        "action": action,
        "note": note,
        "createdAt": crate::modules::config::now_ms(),
        "pageSize": PAGE_SZ,
        "original": { "bytes": bytes, "pages": bytes / PAGE_SZ, "sha256": sha },
    });
    write_meta(dir, &meta)?;
    Ok(json!({
        "dir": dir.to_string_lossy(),
        "mode": "full",
        "bytes": bytes,
        "fullBytes": bytes,
        "changedPages": Value::Null,
        "totalPages": bytes / PAGE_SZ,
        "savedBytes": 0,
        "note": note,
    }))
}

fn write_meta(dir: &Path, meta: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(META_FILE), text).map_err(|e| format!("写元信息失败: {e}"))
}

/// 读备份目录的元信息（老格式没有，返回 `None`）。
pub fn read_meta(dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join(META_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 给界面看的备份概览：这份能不能还原、是增量还是整份、省了多少。
pub fn inspect(dir: &Path) -> Value {
    let bytes = crate::modules::trae_export::path_size(dir);
    let meta = read_meta(dir);
    let has_delta = dir.join(DELTA_FILE).is_file();
    let has_full = dir.join(FULL_FILE).is_file();

    let (mode, note, created_at, original_bytes, changed_pages) = match &meta {
        Some(m) => (
            m.get("mode").and_then(Value::as_str).unwrap_or("?").to_string(),
            m.get("note").and_then(Value::as_str).unwrap_or("").to_string(),
            m.get("createdAt").and_then(Value::as_i64),
            m.pointer("/original/bytes").and_then(Value::as_i64),
            m.pointer("/delta/changedPages").and_then(Value::as_i64),
        ),
        None => (
            if has_full { "full".to_string() } else { "unknown".to_string() },
            // 老格式（T66 之前的整份备份）：没有元信息，只能如实标出来。
            "老格式备份（无校验信息）".to_string(),
            None,
            None,
            None,
        ),
    };

    // 能不能还原：得有「可用的数据文件」，而且不能两份打架。
    let usable = match mode.as_str() {
        "delta" => has_delta && !has_full,
        "full" => has_full,
        _ => false,
    };
    let why = if usable {
        String::new()
    } else if has_delta && has_full {
        "备份目录里同时存在增量与整份数据，来源不明，不敢还原".to_string()
    } else if mode == "delta" {
        "缺少增量文件 delta.bin".to_string()
    } else {
        "缺少 database.db".to_string()
    };

    json!({
        "dir": dir.to_string_lossy(),
        "mode": mode,
        "bytes": bytes,
        "originalBytes": original_bytes,
        "changedPages": changed_pages,
        "createdAt": created_at,
        "note": note,
        "canRestore": usable,
        "reason": why,
    })
}

// ---------------------------------------------------------------------------
// 统一出口：写库前要不要备、怎么备
// ---------------------------------------------------------------------------

/// **写库前的备份 —— 唯一出口**。所有写库链路（Trae 删除 / 编辑 / 导入修复 /
/// WorkBuddy 导入）都只调它，不许各自 `std::fs::copy` 整库。
///
/// 返回值的 `mode` 有三种，**必须能分辨**：
///
/// | `mode` | 含义 | 调用方该做什么 |
/// |---|---|---|
/// | `"skipped"` | 设置里关掉了「写库前整库备份」 | **照常往下写库**，日志照 [`describe`] 写「未备份（设置已关闭）」 |
/// | `"delta"` / `"full"` | 真写了备份 | 照常往下写库，把 `dir` / `bytes` 报给界面 |
///
/// 返回 `Err` 才是**真失败** —— 调用方保留原有语义（备份失败即中止本次写入）。
///
/// ⚠️⚠️ 「设置关了所以没备」与「备份失败了」**绝不许长得一样**：
/// 前者是 `Ok(mode=skipped)`，后者是 `Err`。任何把两者合成一个 `bool` / 一个空
/// `Option` 的写法都违反这条（T55 的教训）。
pub fn snapshot_before_write(
    orig_db: &Path,
    new_db: &Path,
    dir: &Path,
    client_key: &str,
    action: &str,
    note: &str,
) -> Result<Value, String> {
    if !crate::modules::settings::backup_before_write() {
        return Ok(skipped_outcome(dir, note));
    }
    write_snapshot(orig_db, new_db, dir, client_key, action, note)
}

/// 「**未备份**」的返回形状 —— 单独抽出来，好处有二：
///
/// 1. **可单测**：这个形状能不能与「备份失败」区分开，是本项目反复强调的硬规则
///    （「空」与「失败」绝不许长得一样）。抽出来就能直接断言，而不必去读真实设置文件
///    （测试里改 `TWIN_SWITCH_HOME` 是全局副作用，会串到并行跑的其它测试上）。
/// 2. **只有一份**：哪个调用方要「跳过时也返回同形」，就调它，别各拼一份 JSON。
pub fn skipped_outcome(dir: &Path, note: &str) -> Value {
    json!({
        "dir": dir.to_string_lossy(),
        "mode": "skipped",
        "bytes": 0,
        "fullBytes": 0,
        "changedPages": Value::Null,
        "totalPages": Value::Null,
        "savedBytes": 0,
        "note": note,
        "reason": "设置里已关闭「写库前整库备份」",
    })
}

/// 把 [`snapshot_before_write`] 的结果变成一行**给人看**的文案（日志 / 界面共用）。
///
/// ⚠️ 只处理 `Ok` 分支。失败（`Err`）由调用方自己拼「备份失败：…」，两者不许共用同一句。
pub fn describe(outcome: &Value) -> String {
    match outcome.get("mode").and_then(Value::as_str).unwrap_or("?") {
        "skipped" => format!(
            "未备份（{}）",
            outcome
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("设置里已关闭「写库前整库备份」")
        ),
        "delta" => format!(
            "已增量备份原库：共 {} 页、只有 {} 页与原库不同，占 {}（省下 {}）",
            outcome.get("totalPages").and_then(Value::as_u64).unwrap_or(0),
            outcome.get("changedPages").and_then(Value::as_u64).unwrap_or(0),
            human_bytes(outcome.get("bytes").and_then(Value::as_u64).unwrap_or(0)),
            human_bytes(outcome.get("savedBytes").and_then(Value::as_u64).unwrap_or(0)),
        ),
        "full" => format!(
            "已整份备份原库：{}",
            human_bytes(outcome.get("bytes").and_then(Value::as_u64).unwrap_or(0))
        ),
        other => format!("备份结果未知（mode={other}）"),
    }
}

fn human_bytes(b: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let f = b as f64;
    if f >= GB {
        format!("{:.2} GB", f / GB)
    } else if f >= MB {
        format!("{:.1} MB", f / MB)
    } else if f >= KB {
        format!("{:.1} KB", f / KB)
    } else {
        format!("{b} B")
    }
}

// ---------------------------------------------------------------------------
// 还原
// ---------------------------------------------------------------------------

/// 把一份备份还原到 `live` 位置。
///
/// ⚠️ 调用方**必须先把客户端停掉**（本函数不动进程）。
///
/// 步骤：验基准 → 拼出原库到 `<live>.tsrestore` → **复核 sha256** → 原子替换。
/// 任何一步不过都直接 `Err`，`live` 保持原样 —— 宁可还不了，也绝不落一个坏库上去。
///
/// ⚠️ 只还原**主库文件**。`-wal` / `-shm` 由 [`swap_in_place`] 连同旧库一起清掉：
/// 它们是「当前库」的补丁，套在还原出来的旧库上只会把内容搞混。
/// 想「还原前先留一手」，用 [`restore_with_park`]。
pub fn restore(dir: &Path, live: &Path) -> Result<Value, String> {
    restore_inner(dir, live, None)
}

/// 还原一份备份，**并先把「当前的库」另存一份**（这样「还原错了」还能再还原回去）。
///
/// 另存走的是同一套页级增量（基准 = 还原后的库、原库 = 还原前的库）⇒ **不会**多复制
/// 609 MB，实测几 MB 量级。落点目录名由 [`entry_name`] 生成（`<客户端>_before_restore_<stamp>`），
/// 保留策略照样认得出归属。
///
/// ⚠️ 另存失败 ⇒ **中止还原**。还原是「拿旧库盖掉现库」，没有后悔药就不该动手 ——
/// 宁可还不了，也不能让用户事后发现「退不回去了」。
pub fn restore_with_park(
    dir: &Path,
    live: &Path,
    park_root: &Path,
    client_key: &str,
) -> Result<Value, String> {
    let park = park_root.join(entry_name(client_key, "restore", &new_stamp()));
    restore_inner(dir, live, Some((&park, client_key)))
}

fn restore_inner(
    dir: &Path,
    live: &Path,
    park: Option<(&Path, &str)>,
) -> Result<Value, String> {
    let meta = read_meta(dir);
    let mode = meta
        .as_ref()
        .and_then(|m| m.get("mode").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| {
            if dir.join(FULL_FILE).is_file() { "full".to_string() } else { "unknown".to_string() }
        });

    let staged = suffixed(live, ".tsrestore");
    let _ = std::fs::remove_file(&staged);

    let orig_bytes;
    let base_bytes;
    let changed_pages;
    match mode.as_str() {
        "delta" => {
            let full = dir.join(DELTA_FILE);
            if !full.is_file() {
                return Err(format!("这份备份缺少增量文件：{}", full.display()));
            }
            let (hdr, body_off) = read_delta_header(&full)?;
            // ① 基准必须对上：增量只对「这个基准」成立，对不上就不可能还原对。
            let live_pages = pages_of(live)?;
            if live_pages != hdr.new_pages {
                return Err(format!(
                    "这份增量备份的基准是「写库那一刻的库」（{} 页），当前库已经是 {} 页 —— \
                     备份之后 Trae 又改动过库，用增量还原只会拼出错误的库，已中止。",
                    hdr.new_pages, live_pages
                ));
            }
            let cur_sha = sha256_file(live)?;
            let want_base = meta
                .as_ref()
                .and_then(|m| m.pointer("/base/sha256").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            if want_base.is_empty() || cur_sha != want_base {
                return Err(
                    "这份增量备份的基准与当前库不一致（备份之后 Trae 又改动过库），\
                     增量还原会拼出错误的库，已中止，原库未改动。"
                        .to_string(),
                );
            }
            orig_bytes = meta
                .as_ref()
                .and_then(|m| m.pointer("/original/bytes").and_then(Value::as_u64))
                .ok_or_else(|| "备份元信息缺少 original.bytes".to_string())?;
            base_bytes = hdr.new_bytes();
            changed_pages = hdr.changed;

            // ② 复制当前库，再按增量把变动页换回原样。
            std::fs::copy(live, &staged).map_err(|e| format!("准备还原文件失败: {e}"))?;
            apply_delta(&full, body_off, &hdr, &staged)?;
            // ③ 页数对齐到原库（新库多出来的尾巴要截掉）。
            let f = std::fs::OpenOptions::new().write(true).open(&staged).map_err(|e| e.to_string())?;
            f.set_len(orig_bytes).map_err(|e| format!("调整还原文件大小失败: {e}"))?;
            f.sync_all().map_err(|e| e.to_string())?;

            let want = meta
                .as_ref()
                .and_then(|m| m.pointer("/original/sha256").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            let got = sha256_file(&staged)?;
            if want.is_empty() || got != want {
                let _ = std::fs::remove_file(&staged);
                return Err("还原结果校验失败（备份文件可能已损坏），已中止，原库未改动".to_string());
            }
        }
        "full" => {
            let src = dir.join(FULL_FILE);
            if !src.is_file() {
                return Err(format!("这份备份缺少数据文件：{}", src.display()));
            }
            orig_bytes = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
            base_bytes = orig_bytes;
            changed_pages = 0;
            std::fs::copy(&src, &staged).map_err(|e| format!("准备还原文件失败: {e}"))?;
            let want = meta
                .as_ref()
                .and_then(|m| m.pointer("/original/sha256").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            if !want.is_empty() {
                let got = sha256_file(&staged)?;
                if got != want {
                    let _ = std::fs::remove_file(&staged);
                    return Err("备份文件校验失败（可能已损坏），已中止，原库未改动".to_string());
                }
            }
        }
        _ => {
            return Err(format!(
                "这个目录里找不到可还原的备份（既没有 {DELTA_FILE} 也没有 {FULL_FILE}）：{}",
                dir.display()
            ));
        }
    }

    // 还原前的「后悔药」：把当前库增量另存一份（基准 = 还原后的库、原库 = 当前的库）。
    // ⚠️ 必须在 `swap_in_place` **之前**做 —— 那一刻 `live` 还是原样、`staged` 已经验过，
    //    两个文件同时有效，diff 才有意义。
    let parked = match park {
        Some((park_dir, key)) => {
            match write_snapshot(
                live,
                &staged,
                park_dir,
                key,
                "restore",
                "还原前的整库备份（用于撤销本次还原）",
            ) {
                Ok(out) => Some(out),
                Err(e) => {
                    let _ = std::fs::remove_file(&staged);
                    return Err(format!("还原前的另存失败，已中止还原（原库未改动）：{e}"));
                }
            }
        }
        None => None,
    };

    let swap = swap_in_place(&staged, live)?;
    Ok(json!({
        "ok": true,
        "mode": mode,
        "restoredBytes": orig_bytes,
        "baseBytes": base_bytes,
        "changedPages": changed_pages,
        "verified": meta.is_some(),
        "live": live.to_string_lossy(),
        "backupDir": dir.to_string_lossy(),
        "swap": swap,
        // 还原前的另存（`None` = 调用方没要求留后悔药）
        "parked": parked,
    }))
}

struct DeltaHeader {
    page_size: u32,
    orig_pages: u64,
    new_pages: u64,
    changed: u64,
}

impl DeltaHeader {
    fn new_bytes(&self) -> u64 {
        self.new_pages * self.page_size as u64
    }
}

fn read_delta_header(p: &Path) -> Result<(DeltaHeader, u64), String> {
    let mut f = std::fs::File::open(p).map_err(|e| format!("打开增量文件失败: {e}"))?;
    let mut hdr = [0u8; HEADER_LEN as usize];
    f.read_exact(&mut hdr).map_err(|e| format!("增量文件头不完整: {e}"))?;
    if &hdr[..8] != MAGIC {
        return Err("增量文件标识不对（不是本工具写出的备份）".to_string());
    }
    let ver = u32::from_le_bytes(hdr[8..12].try_into().unwrap());
    if ver != FORMAT_VERSION {
        return Err(format!("增量文件版本 {ver} 不认识（当前支持 {FORMAT_VERSION}）"));
    }
    let page_size = u32::from_le_bytes(hdr[12..16].try_into().unwrap());
    if page_size != PAGE_SZ as u32 {
        return Err(format!("增量文件页大小 {page_size} 与当前实现 {PAGE_SZ} 不一致"));
    }
    let h = DeltaHeader {
        page_size,
        orig_pages: u64::from_le_bytes(hdr[16..24].try_into().unwrap()),
        new_pages: u64::from_le_bytes(hdr[24..32].try_into().unwrap()),
        changed: u64::from_le_bytes(hdr[32..40].try_into().unwrap()),
    };
    Ok((h, HEADER_LEN))
}

fn apply_delta(delta: &Path, body_off: u64, hdr: &DeltaHeader, out: &Path) -> Result<(), String> {
    let ps = hdr.page_size as usize;
    let mut r = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(delta).map_err(|e| e.to_string())?);
    r.seek(SeekFrom::Start(body_off)).map_err(|e| e.to_string())?;
    let mut w = std::fs::OpenOptions::new().write(true).open(out).map_err(|e| e.to_string())?;
    let mut rec = vec![0u8; 8 + ps];
    for i in 0..hdr.changed {
        r.read_exact(&mut rec).map_err(|e| format!("增量文件第 {i} 条记录不完整: {e}"))?;
        let pgno = u64::from_le_bytes(rec[..8].try_into().unwrap());
        // 页号越界 ⇒ 文件被改过，绝不能不查就写（会写坏库）。
        if pgno == 0 || pgno > hdr.orig_pages {
            return Err(format!("增量文件里出现了越界页号 {pgno}（原库只有 {} 页），已中止", hdr.orig_pages));
        }
        w.seek(SeekFrom::Start((pgno - 1) * hdr.page_size as u64)).map_err(|e| e.to_string())?;
        w.write_all(&rec[8..]).map_err(|e| e.to_string())?;
    }
    w.flush().map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 原子替换
// ---------------------------------------------------------------------------

/// 把 `new_db` 换到 `live` 的位置，**任何一步失败都自动回滚**。
///
/// 与旧写法（`remove(live)` → `rename(new)`）的区别：那两步之间一旦崩溃/断电就**丢库**，
/// 于是「写库前必须留备份」成了硬要求。改名是原子操作 —— 旧库先改名让位、新库再改名就位，
/// 中途出事就把旧库改回来。这样备份只负责「留历史版本」，不再承担「防丢失」。
///
/// 返回 `{ "replaced": true, "wal": true, "copied": false }`。
pub fn swap_in_place(new_db: &Path, live: &Path) -> Result<Value, String> {
    if !new_db.is_file() {
        return Err(format!("要替换进去的库不存在：{}", new_db.display()));
    }
    let old_db = suffixed(live, ".tsold");
    let live_wal = suffixed(live, "-wal");
    let live_shm = suffixed(live, "-shm");
    let old_wal = suffixed(&old_db, "-wal");
    let old_shm = suffixed(&old_db, "-shm");

    let _ = std::fs::remove_file(&old_db);
    let _ = std::fs::remove_file(&old_wal);
    let _ = std::fs::remove_file(&old_shm);

    if live.is_file() {
        std::fs::rename(live, &old_db).map_err(|e| format!("移开旧库失败（库未改动）：{e}"))?;
    }
    let mut moved_wal = false;
    if live_wal.is_file() {
        if let Err(e) = std::fs::rename(&live_wal, &old_wal) {
            let _ = std::fs::rename(&old_db, live);
            return Err(format!("移开旧 WAL 失败（已回滚，库未改动）：{e}"));
        }
        moved_wal = true;
    }
    let mut moved_shm = false;
    if live_shm.is_file() {
        if let Err(e) = std::fs::rename(&live_shm, &old_shm) {
            if moved_wal {
                let _ = std::fs::rename(&old_wal, &live_wal);
            }
            let _ = std::fs::rename(&old_db, live);
            return Err(format!("移开旧 SHM 失败（已回滚，库未改动）：{e}"));
        }
        moved_shm = true;
    }

    let rollback = |moved_wal: bool, moved_shm: bool| {
        if moved_shm {
            let _ = std::fs::rename(&old_shm, &live_shm);
        }
        if moved_wal {
            let _ = std::fs::rename(&old_wal, &live_wal);
        }
        let _ = std::fs::rename(&old_db, live);
    };

    // 优先改名（同卷、原子）。跨卷时 rename 会失败，退回「复制 + 校验」——
    // 此时旧库仍在 `.tsold` 上，所以复制到一半失败也不会丢数据。
    let mut copied = false;
    if let Err(rename_err) = std::fs::rename(new_db, live) {
        let size_new = std::fs::metadata(new_db).map(|m| m.len()).unwrap_or(0);
        if let Err(e) = std::fs::copy(new_db, live) {
            rollback(moved_wal, moved_shm);
            return Err(format!(
                "替换数据库失败（已回滚，原库完好）：改名不行（{rename_err}），复制也不行（{e}）"
            ));
        }
        let size_live = std::fs::metadata(live).map(|m| m.len()).unwrap_or(0);
        if size_live != size_new {
            let _ = std::fs::remove_file(live);
            rollback(moved_wal, moved_shm);
            return Err(format!(
                "替换数据库失败（已回滚，原库完好）：跨卷复制后大小不一致（{size_live} ≠ {size_new}）"
            ));
        }
        let _ = std::fs::remove_file(new_db);
        copied = true;
    }

    // 新库已就位，旧库三件套可以清了。
    let _ = std::fs::remove_file(&old_db);
    let _ = std::fs::remove_file(&old_wal);
    let _ = std::fs::remove_file(&old_shm);
    Ok(json!({ "replaced": true, "copied": copied, "wal": moved_wal, "shm": moved_shm }))
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("tsw-backup-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 造一个「像库一样」的文件：页数 × 4096，内容可复现。
    fn fake_db(path: &Path, pages: u64, seed: u8) {
        let mut v = Vec::with_capacity(pages as usize * PAGE_SZ as usize);
        for pg in 0..pages {
            for i in 0..PAGE_SZ as usize {
                // 每一页有辨识度，避免「整页相同」让变动页数结论失真
                v.push(seed.wrapping_mul(31).wrapping_add((pg as u8).wrapping_mul(7)).wrapping_add(i as u8));
            }
        }
        std::fs::write(path, &v).unwrap();
    }

    fn patch_page(path: &Path, pgno: u64, fill: u8) {
        let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.seek(SeekFrom::Start((pgno - 1) * PAGE_SZ)).unwrap();
        f.write_all(&vec![fill; PAGE_SZ as usize]).unwrap();
        f.flush().unwrap();
    }

    #[test]
    fn delta_backup_round_trips_to_the_original_bytes() {
        let root = tmp("roundtrip");
        let orig = root.join("database.db");
        let new = root.join("new.db");
        fake_db(&orig, 64, 3);
        // 新库 = 原库，改了 2 页、多了 2 页
        std::fs::copy(&orig, &new).unwrap();
        patch_page(&new, 5, 0xAA);
        patch_page(&new, 40, 0xBB);
        let mut f = std::fs::OpenOptions::new().append(true).open(&new).unwrap();
        f.write_all(&vec![0x11u8; (PAGE_SZ * 2) as usize]).unwrap();
        drop(f);

        let dir = root.join("bak");
        let r = write_snapshot(&orig, &new, &dir, "trae-cn", "delete", "测试").unwrap();
        assert_eq!(r["mode"], json!("delta"));
        // 只存了 2 页，不该接近整份大小
        assert_eq!(r["changedPages"], json!(2));
        assert!(r["bytes"].as_u64().unwrap() < PAGE_SZ * 4, "增量应当只有两页多");
        assert_eq!(r["totalPages"], json!(64));

        // 还原：把「新库」当当前库
        let live = root.join("live.db");
        std::fs::copy(&new, &live).unwrap();
        let out = restore(&dir, &live).unwrap();
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["changedPages"], json!(2));
        // 结果必须与备份时的原库逐字节一致
        assert_eq!(std::fs::read(&live).unwrap(), std::fs::read(&orig).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 库变短（原库比新库长）时，被截断的那几页也必须能补回来。
    #[test]
    fn delta_backup_restores_pages_that_the_new_db_dropped() {
        let root = tmp("shrink");
        let orig = root.join("database.db");
        let new = root.join("new.db");
        fake_db(&orig, 40, 9);
        // 新库删掉了最后 8 页
        let bytes = std::fs::read(&orig).unwrap();
        std::fs::write(&new, &bytes[..(PAGE_SZ * 32) as usize]).unwrap();

        let dir = root.join("bak");
        let r = write_snapshot(&orig, &new, &dir, "trae-cn", "delete", "变短").unwrap();
        assert_eq!(r["mode"], json!("delta"));
        assert_eq!(r["changedPages"], json!(8), "被截断的 8 页必须全部记进增量");

        let live = root.join("live.db");
        std::fs::copy(&new, &live).unwrap();
        restore(&dir, &live).unwrap();
        assert_eq!(std::fs::read(&live).unwrap(), bytes, "还原后应与原库完全一致");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_refuses_when_the_base_changed() {
        let root = tmp("stale");
        let orig = root.join("database.db");
        let new = root.join("new.db");
        fake_db(&orig, 32, 5);
        std::fs::copy(&orig, &new).unwrap();
        patch_page(&new, 3, 0x77);
        let dir = root.join("bak");
        write_snapshot(&orig, &new, &dir, "trae-cn", "edit", "x").unwrap();

        // Trae 之后又改过库 ⇒ 基准对不上
        let live = root.join("live.db");
        std::fs::copy(&new, &live).unwrap();
        patch_page(&live, 20, 0x01);
        let before = std::fs::read(&live).unwrap();
        let e = restore(&dir, &live).unwrap_err();
        assert!(e.contains("基准"), "必须说清是基准不匹配：{e}");
        assert_eq!(std::fs::read(&live).unwrap(), before, "拒绝时必须一字未改");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_refuses_a_corrupted_delta_instead_of_writing_a_broken_db() {
        let root = tmp("corrupt");
        let orig = root.join("database.db");
        let new = root.join("new.db");
        fake_db(&orig, 32, 6);
        std::fs::copy(&orig, &new).unwrap();
        patch_page(&new, 7, 0x5A);
        let dir = root.join("bak");
        write_snapshot(&orig, &new, &dir, "trae-cn", "edit", "x").unwrap();

        // 把增量体里那一页改坏
        let d = dir.join(DELTA_FILE);
        let mut f = std::fs::OpenOptions::new().write(true).open(&d).unwrap();
        f.seek(SeekFrom::Start(HEADER_LEN + 16)).unwrap();
        f.write_all(&[0x00; 32]).unwrap();
        drop(f);

        let live = root.join("live.db");
        std::fs::copy(&new, &live).unwrap();
        let before = std::fs::read(&live).unwrap();
        let e = restore(&dir, &live).unwrap_err();
        assert!(e.contains("校验失败"), "必须是校验失败而不是默默写上去：{e}");
        assert_eq!(std::fs::read(&live).unwrap(), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nearly_every_page_changed_falls_back_to_a_full_copy() {
        let root = tmp("full");
        let orig = root.join("database.db");
        let new = root.join("new.db");
        fake_db(&orig, 16, 1);
        fake_db(&new, 16, 200); // 页页不同
        let dir = root.join("bak");
        let r = write_snapshot(&orig, &new, &dir, "trae-cn", "delete", "x").unwrap();
        assert_eq!(r["mode"], json!("full"), "省不下空间就该整份");
        assert!(dir.join(FULL_FILE).is_file());
        assert!(!dir.join(DELTA_FILE).exists(), "整份模式下不能留下增量文件");

        let live = root.join("live.db");
        std::fs::copy(&new, &live).unwrap();
        restore(&dir, &live).unwrap();
        assert_eq!(std::fs::read(&live).unwrap(), std::fs::read(&orig).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_accepts_a_legacy_full_backup_without_meta() {
        let root = tmp("legacy");
        let orig = root.join("database.db");
        fake_db(&orig, 8, 4);
        let dir = root.join("bak");
        std::fs::create_dir_all(&dir).unwrap();
        // 老格式：只有 database.db，没有 meta.json
        std::fs::copy(&orig, dir.join(FULL_FILE)).unwrap();
        assert!(!dir.join(META_FILE).exists());

        let live = root.join("live.db");
        fake_db(&live, 8, 77);
        let out = restore(&dir, &live).unwrap();
        assert_eq!(out["mode"], json!("full"));
        assert_eq!(out["verified"], json!(false), "老格式没有校验信息，必须如实标出来");
        assert_eq!(std::fs::read(&live).unwrap(), std::fs::read(&orig).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn inspect_says_why_a_directory_cannot_be_restored() {
        let root = tmp("inspect");
        let dir = root.join("empty");
        std::fs::create_dir_all(&dir).unwrap();
        let v = inspect(&dir);
        assert_eq!(v["canRestore"], json!(false));
        assert!(!v["reason"].as_str().unwrap().is_empty(), "说不能还原就必须给理由");

        // 两份数据同时存在 ⇒ 来源不明，不许还原
        std::fs::write(dir.join(FULL_FILE), vec![0u8; 4096]).unwrap();
        std::fs::write(dir.join(DELTA_FILE), vec![0u8; 64]).unwrap();
        write_meta(&dir, &json!({ "mode": "delta", "original": { "bytes": 4096 } })).unwrap();
        let v = inspect(&dir);
        assert_eq!(v["canRestore"], json!(false));
        assert!(v["reason"].as_str().unwrap().contains("同时存在"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn swap_in_place_replaces_atomically_and_clears_the_old_copy() {
        let root = tmp("swap");
        let live = root.join("database.db");
        let new = root.join("new.db");
        fake_db(&live, 4, 1);
        fake_db(&new, 4, 2);
        let wal = suffixed(&live, "-wal");
        let shm = suffixed(&live, "-shm");
        std::fs::write(&wal, b"wal").unwrap();
        std::fs::write(&shm, b"shm").unwrap();

        let new_bytes = std::fs::read(&new).unwrap();
        let out = swap_in_place(&new, &live).unwrap();
        assert_eq!(out["replaced"], json!(true));
        assert_eq!(out["copied"], json!(false));
        assert_eq!(std::fs::read(&live).unwrap(), new_bytes, "库位上的内容必须等于新库");
        assert!(!suffixed(&live, ".tsold").exists(), "成功后就该把旧库清掉");
        assert!(!wal.exists() && !shm.exists(), "旧库的 WAL/SHM 必须一起清掉");
        assert!(!new.exists(), "新库应当被「改名」而不是复制");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// ⚠️⚠️ 这条是 T66 的核心契约：**「没备份」与「备份失败」绝不许长得一样**。
    #[test]
    fn a_skipped_backup_is_ok_not_an_error_and_says_so() {
        let root = tmp("skipped");
        let dir = root.join("trae-cn_before_delete_20261014_213000");
        let out = skipped_outcome(&dir, "删除会话前的整库备份");

        // ① 是 `Ok`（`snapshot_before_write` 的 skipped 分支就返回它），不是 `Err`；
        // ② 形状与真备份同构（调用方不必为「没备份」写第二套读取逻辑）；
        assert_eq!(out["mode"], json!("skipped"));
        assert_eq!(out["bytes"], json!(0));
        assert_eq!(out["savedBytes"], json!(0));
        // ③ 必须带得出「为什么没备」——否则界面只能显示一个空字符串，用户以为是失败。
        let reason = out["reason"].as_str().unwrap();
        assert!(reason.contains("关闭"), "理由要说清是「设置关了」：{reason}");

        // ④ 文案：说「未备份（…）」，**不能**出现「失败」二字。
        let text = describe(&out);
        assert!(text.starts_with("未备份"), "应说「未备份」，实际：{text}");
        assert!(!text.contains("失败"), "未备份不是失败，文案里不许出现「失败」：{text}");

        // ⑤ 目录**不该被建出来** —— 「关掉了不备份」就得连目录都不留。
        assert!(!dir.exists(), "关掉开关时不应留下任何备份目录");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 真备份的文案必须与「未备份」明显不同，且把「省了多少」说出来。
    #[test]
    fn a_real_backup_describes_what_it_saved() {
        let root = tmp("describe");
        let orig = root.join("database.db");
        let new = root.join("new.db");
        fake_db(&orig, 128, 3);
        std::fs::copy(&orig, &new).unwrap();
        patch_page(&new, 9, 0x5C);
        let dir = root.join("bak");
        let out = write_snapshot(&orig, &new, &dir, "trae-cn", "delete", "x").unwrap();

        let text = describe(&out);
        assert!(text.starts_with("已增量备份"), "实际：{text}");
        assert!(text.contains("128 页"), "要说清库有多大：{text}");
        assert!(!text.contains("未备份"), "真备份的文案不许与未备份混：{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn entry_name_stays_parseable_by_the_retention_policy() {
        // ⚠️ 名字一变，保留策略就认不出 owner ⇒ 备份永远清不掉（T62 的坑）。
        let n = entry_name("trae-cn", "delete", "20261014_213000");
        assert_eq!(n, "trae-cn_before_delete_20261014_213000");
        let (owner, stamp) = crate::modules::trae_export::backup_owner_and_stamp(&n);
        assert_eq!(owner, "trae-cn");
        assert_eq!(stamp, "20261014_213000");
    }
}
