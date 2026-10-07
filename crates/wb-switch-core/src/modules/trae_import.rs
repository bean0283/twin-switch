//! 跨账号会话导入：把「本账号解密出的会话记录」写入另一个账号的本地加密库。
//!
//! 目标按**账号**（client@account）而不是客户端划分：同一客户端（如 TRAE SOLO CN）
//! 下的多个账号共享同一个本地库，跨账号导入 = 在明文副本里**复制会话并改写归属**
//! （新 session_id / message_id + project 指向目标 uid），源账号记录保留。
//!
//! 原理（与 `trae_decrypt` 严格对称的页面级实现，零新增依赖）：
//!   1. 目标库先整体解密为明文副本（复用 `decrypt_database`，按页分块多线程）；
//!   2. 在明文副本上插入源会话数据（rusqlite 动态列复制，列交集保真）；
//!   3. 把明文副本加密回写为 SQLCipher 库；
//!   4. 校验 + 原子替换（先备份原库与 wal/shm，失败回滚）。
//!
//! 第 3 步用 **`write_db_incremental`**（v0.0.6 起）：整库密文照抄一份，逐页把明文与
//! 原库比对，**只对变动的页**重新加密回写，其余页直接沿用原密文。依据是 SQLCipher
//! 逐页独立加密（每页自带随机 IV + HMAC、页号参与 HMAC、页长恒 4096），
//! 因此**首页 salt 必须沿用原值**——mac key 由 `key + salt` 派生，换 salt 会让所有
//! 未变动页的 HMAC 失效。实测 279 MB / 71477 页的库只改 3 行时，重写量 5 页（0.02 MB）。
//! `encrypt_db_file` / `encrypt_db_file_in_place`（整库重写）保留给 CLI 与回归测试使用。
//!
//! 前置约束：
//!   · 目标账号必须在本机登录过（有 database.db，且密钥可得：存盘密钥优先，
//!     无存盘密钥时需该账号进程运行中做内存扫描）；
//!   · 导入时目标客户端必须退出（避免文件占用与 WAL 不一致），会自动杀进程。
//!
//! 已知边界：写进目标库的会话是「本地新增」行，与云端任务列表的双向同步行为
//! 未定义——若该账号云端同步会清理本地孤儿行，导入的会话可能被覆盖。

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::modules::config::store_dir;
use crate::modules::trae_decrypt::{
    decrypt_database, decrypt_page, derive_mac_key, hex_to_bytes, verify_page1_hmac,
};
use crate::modules::trae_discover::{database_path, get_client, list_installed_clients, storage_json_path};
use crate::modules::trae_export::{decrypted_db_path, open_decrypted};
use crate::modules::trae_km::aes_cbc_encrypt;
use crate::modules::trae_memory_scan::{load_saved_key, save_key, scan_for_key};
use crate::modules::trae_switch::{is_running, kill_all, launch, wait_until_stopped};
use crate::modules::trae_vault::{list_vault_accounts, read_meta, uid_of_account, uid_from_storage_text};
use aes::Aes256;

const PAGE_SZ: usize = 4096;
const KEY_SZ: usize = 32;
const SALT_SZ: usize = 16;
const IV_SZ: usize = 16;
const HMAC_SZ: usize = 64;
const RESERVE_SZ: usize = 80;
/// 页 1 加密载荷起始（跳过 salt）。
const AUTH_DATA_START: usize = SALT_SZ;
/// 加密载荷 / 可用数据长度（4096 - 80）。
const USABLE_SZ: usize = PAGE_SZ - RESERVE_SZ;
/// HMAC 起始偏移（IV 之后）。
const HMAC_START: usize = PAGE_SZ - HMAC_SZ;

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut out = [0u8; N];
    getrandom::getrandom(&mut out).map_err(|e| format!("随机数生成失败: {e}"))?;
    Ok(out)
}

/// 生成与客户端原生一致的 24 位十六进制 id（12 随机字节）。
/// 客户端原生 session_id / message_id / project_id 均为 24 位 hex；
/// 32 位 UUID 会触发工具内 `is_session_id`（20~24 位）校验失败，导致详情/MD/删除不可用。
fn native_hex_id() -> String {
    let b = random_bytes::<12>().unwrap_or([0u8; 12]);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 把 `chat_session.context`（JSON 字符串）里的 `last_real_project_id` 换成 `new_pid`。
///
/// 为什么必须改：同库复制时副本照抄源行的 `context`，其中 `last_real_project_id` 仍指向
/// **源账号的旧项目**。Trae 用它定位会话的「真实工程」，指向不存在的项目会触发客户端
/// 自身的修复/重建逻辑，配合 `session_project` 归属不一致，就出现「在客户端里删不掉、
/// 重启后记录复活」。
///
/// 返回 `None` 表示「无需改写」（不是 JSON 对象、本来就没有该键、或值已等于 `new_pid`），
/// 调用方保持原值。解析失败同样返回 `None` —— 宁可不动也不能把客户的 context 写坏。
fn rewrite_last_real_project_id(context: &str, new_pid: &str) -> Option<String> {
    let mut v: Value = serde_json::from_str(context).ok()?;
    let obj = v.as_object_mut()?;
    let cur = obj.get("last_real_project_id")?.as_str()?;
    if cur == new_pid {
        return None;
    }
    obj.insert(
        "last_real_project_id".into(),
        Value::String(new_pid.to_string()),
    );
    serde_json::to_string(&v).ok()
}

/// 把一页明文（标准 SQLite 页，usable=4016）加密成 SQLCipher 页。
fn encrypt_sqlcipher_page(
    enc_key: &[u8; KEY_SZ],
    mac_key: &[u8; KEY_SZ],
    salt: [u8; SALT_SZ],
    pgno: u64,
    plain: &[u8; PAGE_SZ],
) -> Result<[u8; PAGE_SZ], String> {
    let mut out = [0u8; PAGE_SZ];
    let iv = random_bytes::<IV_SZ>()?;
    let (cipher_start, content) = if pgno == 1 {
        out[..SALT_SZ].copy_from_slice(&salt);
        (SALT_SZ, &plain[SALT_SZ..USABLE_SZ])
    } else {
        (0usize, &plain[..USABLE_SZ])
    };
    let cipher = aes_cbc_encrypt::<Aes256>(enc_key, &iv, content);
    if cipher.len() != USABLE_SZ - cipher_start {
        return Err(format!("页面加密长度不符：{}-{}", cipher.len(), cipher_start));
    }
    out[cipher_start..USABLE_SZ].copy_from_slice(&cipher);
    out[USABLE_SZ..USABLE_SZ + IV_SZ].copy_from_slice(&iv);

    // HMAC 覆盖密文+IV：页 1 从 salt 之后起，其余页从页首起；页号按 SQLCipher 约定取 4 字节
    use hmac::{Hmac, Mac};
    let hmac_start = if pgno == 1 { AUTH_DATA_START } else { 0 };
    let mut mac = Hmac::<sha2::Sha512>::new_from_slice(mac_key).map_err(|e| e.to_string())?;
    mac.update(&out[hmac_start..HMAC_START]);
    mac.update(&(pgno as u32).to_le_bytes());
    let digest = mac.finalize().into_bytes();
    out[HMAC_START..].copy_from_slice(&digest[..HMAC_SZ]);
    Ok(out)
}

/// 加密整库：明文副本 → SQLCipher 文件。返回写入页数。
/// 明文副本的页 1 头 reserved 字段必须已是 80（见 `prepare_plain_copy`），
/// 保证 SQLite 写入时不会越过 [..4016] 区域，页尾 80 字节保持清零。
pub fn encrypt_db_file(
    enc_key_hex: &str,
    plain_path: &Path,
    out_path: &Path,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<u64, String> {
    let enc_key_vec = hex_to_bytes(enc_key_hex.trim());
    if enc_key_vec.len() != KEY_SZ {
        return Err("密钥必须是 64 位十六进制字符串".into());
    }
    let mut enc_key = [0u8; KEY_SZ];
    enc_key.copy_from_slice(&enc_key_vec);
    let mut fin = std::fs::File::open(plain_path).map_err(|e| format!("打开明文副本失败: {e}"))?;
    use std::io::{Read, Seek, Write};
    let size = fin
        .seek(std::io::SeekFrom::End(0))
        .map_err(|e| format!("读取明文副本大小失败: {e}"))?;
    fin.seek(std::io::SeekFrom::Start(0))
        .map_err(|e| format!("定位失败: {e}"))?;
    if size % PAGE_SZ as u64 != 0 {
        return Err(format!("明文副本大小 {size} 不是页大小整数倍，结构异常"));
    }
    let pages = size / PAGE_SZ as u64;
    if pages == 0 {
        return Err("明文副本为空".into());
    }
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败: {e}"))?;
    }
    let mut fout = std::fs::File::create(out_path).map_err(|e| format!("创建加密库失败: {e}"))?;

    let salt = random_bytes::<SALT_SZ>()?;
    let mac_key = derive_mac_key(&enc_key, &salt);
    let mut buf = [0u8; PAGE_SZ];
    let t0 = std::time::Instant::now();
    for pgno in 1..=pages {
        fin.read_exact(&mut buf).map_err(|e| format!("读取明文页 {pgno} 失败: {e}"))?;
        // 明文库 reserve 区（[4016..4096]）必须全零，否则加密会丢字节
        if buf[USABLE_SZ..].iter().any(|b| *b != 0) {
            return Err(format!(
                "明文页 {pgno} 的尾部保留区非零（客户端版本不兼容？），中止回写以防数据损坏"
            ));
        }
        let enc = encrypt_sqlcipher_page(&enc_key, &mac_key, salt, pgno, &buf)?;
        fout.write_all(&enc).map_err(|e| format!("写加密页 {pgno} 失败: {e}"))?;
        if let Some(cb) = on_progress {
            if pgno % 256 == 0 || pgno == pages {
                cb(&format!(
                    "加密回写 {pgno}/{pages} 页（{:.0}%）",
                    pgno as f64 * 100.0 / pages as f64
                ));
            }
        }
    }
    let _ = t0;

    // 自检：首页 HMAC 必须通过，否则拒绝替换
    let mut page1 = [0u8; PAGE_SZ];
    {
        let mut chk = std::fs::File::open(out_path).map_err(|e| e.to_string())?;
        chk.read_exact(&mut page1).map_err(|e| format!("自检读首页失败: {e}"))?;
    }
    if !verify_page1_hmac(&enc_key, &page1) {
        return Err("加密回写自检失败：首页 HMAC 校验不通过，已中止替换".into());
    }
    Ok(pages)
}

/// 加密整库：明文副本 **原地** 改写为 SQLCipher 文件。返回页数。
///
/// 与 `encrypt_db_file` 的唯一区别是输出覆盖输入本身，省掉一份与明文等大的新库文件。
/// Trae 的目标库实测 279 MB（71477 页），一次导入的**峰值占用因此少 279 MB**。
///
/// 为什么原地安全：SQLCipher 的页加密是**逐页独立**的——每页自带随机 IV 与 HMAC，页号参与
/// HMAC，页与页之间没有依赖；加解密前后页大小恒为 4096。所以「读第 N 页 → 立即写回第 N 页
/// 的同一偏移」不会破坏尚未处理的其他页。
pub fn encrypt_db_file_in_place(
    enc_key_hex: &str,
    path: &Path,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<u64, String> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let enc_key_vec = hex_to_bytes(enc_key_hex.trim());
    if enc_key_vec.len() != KEY_SZ {
        return Err("密钥必须是 64 位十六进制字符串".into());
    }
    let mut enc_key = [0u8; KEY_SZ];
    enc_key.copy_from_slice(&enc_key_vec);

    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("打开明文副本失败: {e}"))?;
    let size = f.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if size % PAGE_SZ as u64 != 0 {
        return Err(format!("明文副本大小 {size} 不是页大小整数倍，结构异常"));
    }
    let pages = size / PAGE_SZ as u64;
    if pages == 0 {
        return Err("明文副本为空".into());
    }

    let salt = random_bytes::<SALT_SZ>()?;
    let mac_key = derive_mac_key(&enc_key, &salt);
    let mut buf = [0u8; PAGE_SZ];
    for pgno in 1..=pages {
        let off = (pgno - 1) * PAGE_SZ as u64;
        f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        f.read_exact(&mut buf)
            .map_err(|e| format!("读取明文页 {pgno} 失败: {e}"))?;
        // 明文库 reserve 区（[4016..4096]）必须全零，否则加密会丢字节
        if buf[USABLE_SZ..].iter().any(|b| *b != 0) {
            return Err(format!(
                "明文页 {pgno} 的尾部保留区非零（客户端版本不兼容？），中止回写以防数据损坏"
            ));
        }
        let enc = encrypt_sqlcipher_page(&enc_key, &mac_key, salt, pgno, &buf)?;
        f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        f.write_all(&enc)
            .map_err(|e| format!("写加密页 {pgno} 失败: {e}"))?;
        if let Some(cb) = on_progress {
            if pgno % 256 == 0 || pgno == pages {
                cb(&format!(
                    "加密回写 {pgno}/{pages} 页（{:.0}%）",
                    pgno as f64 * 100.0 / pages as f64
                ));
            }
        }
    }
    f.flush().map_err(|e| format!("刷新加密库失败: {e}"))?;
    drop(f);

    // 自检：首页 HMAC 必须通过，否则拒绝替换
    let mut page1 = [0u8; PAGE_SZ];
    {
        let mut chk = std::fs::File::open(path).map_err(|e| e.to_string())?;
        chk.read_exact(&mut page1)
            .map_err(|e| format!("自检读首页失败: {e}"))?;
    }
    if !verify_page1_hmac(&enc_key, &page1) {
        return Err("加密回写自检失败：首页 HMAC 校验不通过，已中止替换".into());
    }
    Ok(pages)
}

/// 增量加密回写的统计。
#[derive(Debug, Clone, serde::Serialize)]
pub struct IncrementalWriteStats {
    /// 新库页数。
    pub pages: u64,
    /// 原库页数。
    pub src_pages: u64,
    /// 真正被重新加密的页数（其余页直接沿用原密文）。
    pub changed_pages: u64,
    /// 因库增长而新增的页数（含在 changed_pages 内）。
    pub appended_pages: u64,
    /// 重写字节数（changed_pages × 4096）。
    pub changed_bytes: u64,
    pub elapsed_ms: u64,
}

/// **只重写变动页**的加密回写：把 `src_enc` 的整库密文复制到 `out_path`，逐页把
/// `new_plain` 与原库明文比对，仅对真正变动的页做页加密并覆盖回写。
///
/// 为什么可行：SQLCipher 逐页独立加密（每页自带随机 IV + HMAC、页号参与 HMAC、
/// 页长恒 4096），所以未变动的页沿用原密文天然合法。**首页的 salt 必须沿用原值**——
/// mac key 由 `key + salt` 派生，换 salt 会让所有未变动页的 HMAC 失效。
///
/// 效果：一次导入只加密几百页（几百 KB ~ 几 MB），而不是整库 71477 页（279 MB）。
/// 每页写完立即回读解密比对，做到「写多少验多少」。
pub fn write_db_incremental(
    enc_key_hex: &str,
    src_enc: &Path,
    new_plain: &Path,
    out_path: &Path,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<IncrementalWriteStats, String> {
    write_db_incremental_with(enc_key_hex, src_enc, None, new_plain, out_path, on_progress)
}

/// 同 `write_db_incremental`，但允许传入**原库的明文副本**（`old_plain`）作为比对基准。
///
/// 为什么值得多这个参数：不传时函数必须逐页 `decrypt_page` 才能知道「这一页有没有变」，
/// 那是又一次整库解密。调用方（`import_sessions`）为了改数据本来就已经解出过一份原库明文，
/// 把它留下来直接做字节比对，就能省掉这一整轮解密（8 万页量级）。
/// `old_plain` 必须与 `src_enc` 逐页一一对应，否则比对结果无效。
pub fn write_db_incremental_with(
    enc_key_hex: &str,
    src_enc: &Path,
    old_plain: Option<&Path>,
    new_plain: &Path,
    out_path: &Path,
    on_progress: Option<&dyn Fn(&str)>,
) -> Result<IncrementalWriteStats, String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    let t0 = std::time::Instant::now();
    let key_vec = hex_to_bytes(enc_key_hex.trim());
    if key_vec.len() != KEY_SZ {
        return Err("密钥必须是 64 位十六进制字符串".into());
    }
    let mut enc_key = [0u8; KEY_SZ];
    enc_key.copy_from_slice(&key_vec);

    // 1) 读原密文首页：校验密钥 + 取 salt（沿用，不换）
    let mut src = std::fs::File::open(src_enc).map_err(|e| format!("打开原加密库失败: {e}"))?;
    let mut page1 = [0u8; PAGE_SZ];
    src.read_exact(&mut page1)
        .map_err(|e| format!("读取原库首页失败: {e}"))?;
    if !verify_page1_hmac(&enc_key, &page1) {
        return Err("原库首页 HMAC 校验失败（密钥不匹配），已中止".into());
    }
    let salt = {
        let mut s = [0u8; SALT_SZ];
        s.copy_from_slice(&page1[..SALT_SZ]);
        s
    };
    let mac_key = derive_mac_key(&enc_key, &salt);
    let src_size = src
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("读取原库大小失败: {e}"))?;
    if src_size % PAGE_SZ as u64 != 0 {
        return Err(format!("原加密库大小 {src_size} 不是页大小整数倍，结构异常"));
    }
    let src_pages = src_size / PAGE_SZ as u64;

    let new_size = std::fs::metadata(new_plain)
        .map_err(|e| format!("读取明文副本大小失败: {e}"))?
        .len();
    if new_size % PAGE_SZ as u64 != 0 {
        return Err(format!("明文副本大小 {new_size} 不是页大小整数倍，结构异常"));
    }
    let new_pages = new_size / PAGE_SZ as u64;
    if new_pages == 0 {
        return Err("明文副本为空".into());
    }
    drop(src);

    // 2) 整库密文照抄一份（纯字节复制，零加解密），再按新页数截断/扩展
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建输出目录失败: {e}"))?;
    }
    std::fs::copy(src_enc, out_path).map_err(|e| format!("复制原加密库失败: {e}"))?;
    {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(out_path)
            .map_err(|e| format!("打开输出库失败: {e}"))?;
        f.set_len(new_pages * PAGE_SZ as u64)
            .map_err(|e| format!("调整输出库大小失败: {e}"))?;
    }

    // 3) 分块并行：比对 + 仅对变动页加密回写 + 逐页回读校验
    let threads = crate::modules::trae_decrypt::crypto_threads()
        .min(new_pages.max(1) as usize)
        .max(1);
    let chunk = new_pages.div_ceil(threads as u64).max(1);
    let done = AtomicU64::new(0);
    let changed: Mutex<Vec<u64>> = Mutex::new(Vec::new());
    let appended = AtomicU64::new(0);
    let fail: Mutex<Option<String>> = Mutex::new(None);

    std::thread::scope(|scope| {
        for t in 0..threads as u64 {
            let start = t * chunk + 1;
            let end = ((t + 1) * chunk).min(new_pages);
            if start > end {
                continue;
            }
            let done = &done;
            let changed = &changed;
            let appended = &appended;
            let fail = &fail;
            scope.spawn(move || {
                let run = || -> Result<(), String> {
                    let mut fnew = std::fs::File::open(new_plain).map_err(|e| e.to_string())?;
                    // 有 old_plain 就只读它（纯字节比对，零解密）；否则退回读原密文并逐页解密。
                    let mut fold = Some(match old_plain {
                        Some(p) => std::fs::File::open(p).map_err(|e| e.to_string())?,
                        None => std::fs::File::open(src_enc).map_err(|e| e.to_string())?,
                    });
                    let mut fout_w = std::fs::OpenOptions::new()
                        .write(true)
                        .open(out_path)
                        .map_err(|e| e.to_string())?;
                    let mut fout_r = std::fs::File::open(out_path).map_err(|e| e.to_string())?;
                    let mut nb = [0u8; PAGE_SZ];
                    let mut sb = [0u8; PAGE_SZ];
                    let mut rb = [0u8; PAGE_SZ];
                    for pgno in start..=end {
                        let off = (pgno - 1) * PAGE_SZ as u64;
                        fnew.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
                        fnew.read_exact(&mut nb).map_err(|e| e.to_string())?;
                        if pgno <= src_pages {
                            let f = fold.as_mut().expect("fold 已在上面初始化");
                            f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
                            f.read_exact(&mut sb).map_err(|e| e.to_string())?;
                            // 比对基准必须是原库的**明文**。有 old_plain 时直接比对
                            // （省掉整库解密）；没有才在这一页上真解一次（只读，不写字节）。
                            let same = if old_plain.is_some() {
                                sb == nb
                            } else {
                                decrypt_page(&enc_key, &sb, pgno).as_slice() == &nb[..]
                            };
                            if same {
                                done.fetch_add(1, Ordering::Relaxed);
                                continue; // 未变动：原密文直接沿用
                            }
                        } else {
                            appended.fetch_add(1, Ordering::Relaxed);
                        }
                        // 变动页：保留区必须全零，否则加密会丢字节
                        if nb[USABLE_SZ..].iter().any(|b| *b != 0) {
                            return Err(format!(
                                "明文页 {pgno} 的尾部保留区非零（客户端版本不兼容？），中止回写以防数据损坏"
                            ));
                        }
                        let enc = encrypt_sqlcipher_page(&enc_key, &mac_key, salt, pgno, &nb)?;
                        fout_w.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
                        fout_w.write_all(&enc).map_err(|e| e.to_string())?;
                        // 写完立刻回读解密比对（只验变动页，代价极小）
                        fout_r.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
                        fout_r.read_exact(&mut rb).map_err(|e| e.to_string())?;
                        if decrypt_page(&enc_key, &rb, pgno) != nb {
                            return Err(format!("页 {pgno} 回读校验不一致，已中止替换"));
                        }
                        changed.lock().unwrap_or_else(|p| p.into_inner()).push(pgno);
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(())
                };
                if let Err(e) = run() {
                    let mut g = fail.lock().unwrap_or_else(|p| p.into_inner());
                    if g.is_none() {
                        *g = Some(e);
                    }
                }
            });
        }
        let mut last = 0u64;
        loop {
            if fail.lock().map(|g| g.is_some()).unwrap_or(true) {
                break;
            }
            let d = done.load(Ordering::Relaxed);
            if d >= new_pages {
                break;
            }
            if d / 2000 != last / 2000 {
                last = d;
                if let Some(cb) = on_progress {
                    cb(&format!(
                        "比对并回写 {d}/{new_pages} 页（变动 {} 页）",
                        changed.lock().map(|c| c.len()).unwrap_or(0)
                    ));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    });

    if let Some(e) = fail.into_inner().unwrap_or_else(|p| p.into_inner()) {
        let _ = std::fs::remove_file(out_path);
        return Err(e);
    }

    // 4) 收尾自检：首页 HMAC 必须通过（首页几乎必然变动，但无论是否变动都成立）
    let mut chk1 = [0u8; PAGE_SZ];
    {
        let mut chk = std::fs::File::open(out_path).map_err(|e| e.to_string())?;
        chk.read_exact(&mut chk1)
            .map_err(|e| format!("自检读首页失败: {e}"))?;
    }
    if !verify_page1_hmac(&enc_key, &chk1) {
        let _ = std::fs::remove_file(out_path);
        return Err("增量回写自检失败：首页 HMAC 校验不通过，已中止替换".into());
    }
    let out_size = std::fs::metadata(out_path).map_err(|e| e.to_string())?.len();
    if out_size != new_pages * PAGE_SZ as u64 {
        let _ = std::fs::remove_file(out_path);
        return Err(format!("增量回写结果大小异常（{out_size} ≠ {}），已中止替换", new_pages * PAGE_SZ as u64));
    }
    let changed_pages = changed.into_inner().unwrap_or_else(|p| p.into_inner()).len() as u64;
    Ok(IncrementalWriteStats {
        pages: new_pages,
        src_pages,
        changed_pages,
        appended_pages: appended.load(Ordering::Relaxed),
        changed_bytes: changed_pages * PAGE_SZ as u64,
        elapsed_ms: t0.elapsed().as_millis() as u64,
    })
}

/// 把明文副本页 1 头部的 reserved-per-page 字段（offset 20）改为 80，
/// 使 SQLite 打开副本时认为 usable=4016，写入不会越过页尾保留区。
pub fn patch_reserved_field(path: &Path) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("打开明文副本补丁失败: {e}"))?;
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut head = [0u8; 100];
    f.read_exact(&mut head).map_err(|e| format!("读明文副本页头失败: {e}"))?;
    if &head[..16] != b"SQLite format 3\x00" {
        return Err("明文副本不是有效 SQLite 文件".into());
    }
    f.seek(SeekFrom::Start(20)).map_err(|e| e.to_string())?;
    f.write_all(&[RESERVE_SZ as u8])
        .map_err(|e| format!("写 reserved 字段失败: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 会话复制（动态列交集，按行整表保真）
// ---------------------------------------------------------------------------

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| format!("查询表结构 {table} 失败: {e}"))?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| format!("读取表结构 {table} 失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("解析表结构 {table} 失败: {e}"))?;
    if cols.is_empty() {
        return Err(format!("目标库没有表 {table}（客户端版本差异？）"));
    }
    Ok(cols)
}

/// 公共列交集，排除自增主键 `id`：源行的整型主键会与目标库已有行冲突，
/// 复制时跳过该列，由 SQLite 自动分配新值。
fn common_cols(src_cols: &[String], dst_cols: &[String]) -> Vec<String> {
    src_cols
        .iter()
        .filter(|c| dst_cols.contains(c) && c.as_str() != "id")
        .cloned()
        .collect()
}

/// 复制一张表中满足 `where_sql` 的行（源列 ∩ 目标列），返回复制行数。
fn copy_rows(
    src: &Connection,
    dst: &Connection,
    table: &str,
    where_sql: &str,
    param: &str,
) -> Result<usize, String> {
    let cols = common_cols(&table_columns(src, table)?, &table_columns(dst, table)?);
    if cols.is_empty() {
        return Err(format!("表 {table} 无公共列，无法复制"));
    }
    let col_list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let select_sql = format!("SELECT {col_list} FROM {table} WHERE {where_sql}");
    let mut stmt = src
        .prepare(&select_sql)
        .map_err(|e| format!("源表 {table} 查询失败: {e}"))?;
    let rows = stmt
        .query_map(params![param], |row| {
            let mut vals = Vec::new();
            for i in 0..cols.len() {
                vals.push(row.get::<_, rusqlite::types::Value>(i)?);
            }
            Ok(vals)
        })
        .map_err(|e| format!("源表 {table} 读取失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("源表 {table} 行读取失败: {e}"))?;
    if rows.is_empty() {
        return Ok(0);
    }
    let placeholders = (0..cols.len())
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(", ");
    let insert_sql = format!("INSERT INTO {table} ({col_list}) VALUES ({placeholders})");
    let mut ins = dst
        .prepare(&insert_sql)
        .map_err(|e| format!("目标表 {table} 插入准备失败: {e}"))?;
    for vals in &rows {
        let args: Vec<&dyn rusqlite::types::ToSql> = vals
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        ins.execute(rusqlite::params_from_iter(args))
            .map_err(|e| format!("目标表 {table} 插入失败: {e}"))?;
    }
    Ok(rows.len())
}

/// 会话复制参数：
/// - `dst_uid`：目标账号 uid（project 归属必须指向它）；
/// - `same_db`：源目标是否同一客户端库。同库复制时**每个会话必须各自生成全新 id**
///   （见 `new_sid` 的说明），跨客户端复制保留原 session_id（不同库唯一约束不冲突）。
#[derive(Default)]
struct CopyOpts {
    dst_uid: Option<String>,
    same_db: bool,
    /// 已确定的新 session_id。仅用于**单个**会话的复制路径（测试 / 单条调用）。
    /// 批量复制**不要**用它——那会让整批共用一个 id，见 [`CopyOpts::same_db`]。
    new_sid: Option<String>,
}

impl CopyOpts {
    /// 同库复制：每个会话独立生成新 id。
    fn same_db_for(dst_uid: Option<&str>) -> Self {
        Self {
            dst_uid: dst_uid.map(String::from),
            same_db: true,
            new_sid: None,
        }
    }

    /// 跨客户端复制：保留原 id。
    fn cross_db_for(dst_uid: Option<&str>) -> Self {
        Self {
            dst_uid: dst_uid.map(String::from),
            same_db: false,
            new_sid: None,
        }
    }
}

/// 复制单个会话（chat_session 一行 + project 归属 + 全部关联表行）。
/// 返回 (插入行数, 跳过原因)。目标库已有该 session_id 且非同库复制时返回跳过。
fn copy_session(
    src: &Connection,
    dst: &Connection,
    sid: &str,
    opts: &CopyOpts,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Option<(usize, String)>, String> {
    // ⚠️ 新 id 必须在这里按会话生成，**不能**由调用方在循环外传一个共用值进来。
    // 早先的写法是 `new_sid: Some(native_hex_id())` 在循环外调一次、整批共用，
    // 于是第二个会话插入时撞上第一个刚写进去的同一个 id，冲突检查判定「已存在」，
    // 返回 Ok(None) 被上层当成「跳过」——**同库多选复制只有第一个会成功，其余静默丢失**。
    let out_sid: String = match &opts.new_sid {
        Some(fixed) => fixed.clone(),
        None if opts.same_db => native_hex_id(),
        None => sid.to_string(),
    };
    let same_db = opts.same_db;
    let exists: Option<i64> = dst
        .query_row(
            "SELECT 1 FROM chat_session WHERE session_id=? LIMIT 1",
            params![out_sid],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| format!("目标库冲突检查失败: {e}"))?;
    if exists.is_some() {
        return Ok(None);
    }

    // project 归属：为目标 uid 查找/新建 project，新会话指向它
    let dst_uid = opts.dst_uid.as_deref();
    let dst_project_id = if let Some(uid) = dst_uid {
        Some(ensure_project_for(src, dst, sid, uid)?)
    } else {
        None
    };

    let mut n = 0usize;
    // 主表：chat_session 行（列交集复制，改写 project_id）
    {
        let cols = common_cols(&table_columns(src, "chat_session")?, &table_columns(dst, "chat_session")?);
        if cols.is_empty() {
            return Err("chat_session 无公共列，无法复制".into());
        }
        let col_list = cols
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = src
            .prepare(&format!("SELECT {col_list} FROM chat_session WHERE session_id=?"))
            .map_err(|e| format!("源 chat_session 查询失败: {e}"))?;
        let mut row = stmt
            .query_map(params![sid], |r| {
                let mut vals = Vec::new();
                for i in 0..cols.len() {
                    vals.push(r.get::<_, rusqlite::types::Value>(i)?);
                }
                Ok(vals)
            })
            .map_err(|e| format!("源 chat_session 读取失败: {e}"))?
            .next()
            .ok_or_else(|| format!("源会话 {sid} 不存在"))?
            .map_err(|e| format!("源 chat_session 行解析失败: {e}"))?;
        if same_db {
            row[cols
                .iter()
                .position(|c| *c == "session_id")
                .ok_or("chat_session 缺 session_id 列")?] = rusqlite::types::Value::Text(out_sid.clone());
        }
        if let (Some(pid), Some(idx)) = (
            dst_project_id.as_deref(),
            cols.iter().position(|c| *c == "project_id"),
        ) {
            row[idx] = rusqlite::types::Value::Text(pid.to_string());
            // 同库复制时，`context` 里的 `last_real_project_id` 也必须跟着换。
            // 它原本指向源账号的旧项目（副本照抄源行 ⇒ 指向旧项目，甚至指向一条
            // 已被删除的项目记录）。Trae 用它定位/重建会话的「真实工程」，指向不存在
            // 的项目时会触发客户端自身的修复逻辑 —— 与 `session_project` 不一致叠加，
            // 就表现为「客户端里删不掉、重启后记录复活」。
            if same_db {
                if let Some(ci) = cols.iter().position(|c| *c == "context") {
                    if let rusqlite::types::Value::Text(ctx) = &row[ci] {
                        if let Some(fixed) = rewrite_last_real_project_id(ctx, pid) {
                            row[ci] = rusqlite::types::Value::Text(fixed);
                        }
                    }
                }
            }
        }
        let placeholders = (0..cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
        let mut ins = dst
            .prepare(&format!("INSERT INTO chat_session ({col_list}) VALUES ({placeholders})"))
            .map_err(|e| format!("目标 chat_session 插入准备失败: {e}"))?;
        let args: Vec<&dyn rusqlite::types::ToSql> = row
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        ins.execute(rusqlite::params_from_iter(args))
            .map_err(|e| format!("目标 chat_session 插入失败: {e}"))?;
        n += 1;
    }

    // 消息表：chat_message（id 映射）+ general/task 关联
    let mids: Vec<String> = {
        let mut stmt = src
            .prepare("SELECT message_id FROM chat_message WHERE session_id=?")
            .map_err(|e| format!("源消息 id 查询失败: {e}"))?;
        let rows = stmt
            .query_map(params![sid], |r| r.get::<_, String>(0))
            .map_err(|e| format!("源消息 id 读取失败: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("源消息 id 解析失败: {e}"))?;
        rows
    };
    let mut mid_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    if !mids.is_empty() {
        let cols = common_cols(&table_columns(src, "chat_message")?, &table_columns(dst, "chat_message")?);
        let col_list = cols
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let in_sql = format!(
            "message_id IN ({})",
            (0..mids.len()).map(|_| "?").collect::<Vec<_>>().join(",")
        );
        let mut stmt = src
            .prepare(&format!("SELECT {col_list} FROM chat_message WHERE {in_sql}"))
            .map_err(|e| format!("源 chat_message 查询失败: {e}"))?;
        let rows: Vec<Vec<rusqlite::types::Value>> = {
            let mapped = stmt
                .query_map(rusqlite::params_from_iter(mids.iter().map(|s| s.as_str())), |r| {
                    let mut vals = Vec::new();
                    for i in 0..cols.len() {
                        vals.push(r.get::<_, rusqlite::types::Value>(i)?);
                    }
                    Ok(vals)
                })
                .map_err(|e| format!("源 chat_message 读取失败: {e}"))?;
            mapped
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("源 chat_message 行读取失败: {e}"))?
        };
        let mid_idx = cols
            .iter()
            .position(|c| *c == "message_id")
            .ok_or("chat_message 缺 message_id 列")?;
        let sid_idx = cols
            .iter()
            .position(|c| *c == "session_id")
            .ok_or("chat_message 缺 session_id 列")?;
        let reply_idx = cols.iter().position(|c| *c == "reply_to_message_id");
        // ⚠️⚠️ **不要把 `mids` 与 `rows` 按位置 zip**（曾经的重大缺陷，用户实测
        // 「同步到另一账号后客户端只剩一句」）：
        // `SELECT … WHERE message_id IN (…)` 由 SQLite 查询计划决定顺序 —— 这里走
        // `message_id` 索引，**按字母序**返回；而 `mids` 是 `session_id` 索引的 rowid 序。
        // 两者一对不上，每条消息被写上的新 id 就属于**另一条**源消息，`chat_turn` 的
        // reply/response（经 `mid_map` 改写）随之全部指向错人。
        //
        // 正解：新 id **只由「行自带的 `message_id`」推导**（行怎么排都不影响映射），
        // 且**先建齐全量映射、再改写会话内引用** —— 消息可能引用后面的消息，边遍历
        // 边查 `mid_map` 会漏掉那时还没生成的目标 id。
        let mut out: Vec<Vec<rusqlite::types::Value>> = Vec::with_capacity(rows.len());
        for mut vals in rows {
            let old = match &vals[mid_idx] {
                rusqlite::types::Value::Text(t) => t.clone(),
                _ => return Err("chat_message 行的 message_id 不是文本".into()),
            };
            let new_mid = if same_db {
                let m = native_hex_id();
                mid_map.insert(old, m.clone());
                m
            } else {
                old
            };
            vals[mid_idx] = rusqlite::types::Value::Text(new_mid);
            vals[sid_idx] = rusqlite::types::Value::Text(out_sid.clone());
            out.push(vals);
        }
        if same_db {
            if let Some(ri) = reply_idx {
                for vals in out.iter_mut() {
                    let key = match &vals[ri] {
                        rusqlite::types::Value::Text(t) => t.clone(),
                        _ => continue,
                    };
                    if let Some(nm) = mid_map.get(&key) {
                        vals[ri] = rusqlite::types::Value::Text(nm.clone());
                    }
                }
            }
        }
        if !out.is_empty() {
            let placeholders = (0..cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
            let mut ins = dst
                .prepare(&format!("INSERT INTO chat_message ({col_list}) VALUES ({placeholders})"))
                .map_err(|e| format!("目标 chat_message 插入准备失败: {e}"))?;
            for vals in &out {
                let args: Vec<&dyn rusqlite::types::ToSql> = vals
                    .iter()
                    .map(|v| v as &dyn rusqlite::types::ToSql)
                    .collect();
                ins.execute(rusqlite::params_from_iter(args))
                    .map_err(|e| format!("目标 chat_message 插入失败: {e}"))?;
            }
            n += out.len();
        }

        // chat_message_general / chat_message_task：message_id 映射后复制
        for table in ["chat_message_general", "chat_message_task"] {
            let t_cols = common_cols(&table_columns(src, table)?, &table_columns(dst, table)?);
            if t_cols.is_empty() {
                continue;
            }
            let t_col_list = t_cols
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = src
                .prepare(&format!(
                    "SELECT {t_col_list} FROM {table} WHERE {in_sql}"
                ))
                .map_err(|e| format!("源表 {table} 查询失败: {e}"))?;
            let rows: Vec<Vec<rusqlite::types::Value>> = {
                let mapped = stmt
                    .query_map(rusqlite::params_from_iter(mids.iter().map(|s| s.as_str())), |row| {
                        let mut vals = Vec::new();
                        for i in 0..t_cols.len() {
                            vals.push(row.get::<_, rusqlite::types::Value>(i)?);
                        }
                        Ok(vals)
                    })
                    .map_err(|e| format!("源表 {table} 读取失败: {e}"))?;
                mapped
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("源表 {table} 行读取失败: {e}"))?
            };
            if rows.is_empty() {
                continue;
            }
            let t_mid_idx = t_cols
                .iter()
                .position(|c| *c == "message_id")
                .ok_or(format!("{table} 缺 message_id 列"))?;
            let mut out: Vec<Vec<rusqlite::types::Value>> = Vec::new();
            for mut vals in rows {
                if let rusqlite::types::Value::Text(old) = &vals[t_mid_idx] {
                    if let Some(nm) = mid_map.get(old) {
                        vals[t_mid_idx] = rusqlite::types::Value::Text(nm.clone());
                    }
                }
                out.push(vals);
            }
            let placeholders = (0..t_cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
            let mut ins = dst
                .prepare(&format!("INSERT INTO {table} ({t_col_list}) VALUES ({placeholders})"))
                .map_err(|e| format!("目标表 {table} 插入准备失败: {e}"))?;
            for vals in &out {
                let args: Vec<&dyn rusqlite::types::ToSql> = vals
                    .iter()
                    .map(|v| v as &dyn rusqlite::types::ToSql)
                    .collect();
                ins.execute(rusqlite::params_from_iter(args))
                    .map_err(|e| format!("目标表 {table} 插入失败: {e}"))?;
            }
            n += out.len();
        }
        // 检索镜像（`fts_message_content` / `fts_session_title`）：**按源行整份重建**。
        //
        // ⚠️ 为什么不能指望客户端触发器：`trg_message_content_ai` 只在插入
        //    `chat_message_general` 时生成词条，且只认 `$.type == "text"` 的片段 ——
        //    源库里 `[{"text_content":…}]` 这种**没有 `type`** 的消息它就取不到文本，
        //    写出一条空词条；再加上它按 `chat_message.message_id` 反查会话，
        //    只要消息 id 配对稍有偏差就会把词条挂到别的会话名下。
        //    源库里这两张表是客户端自己写好的、内容完整 ⇒ 照抄才能保证
        //    「同步过来了也搜得到」。缺了它的症状很隐蔽：会话看着正常，**搜不到**。
        //
        // ⚠️ 顺序：先按目标会话清一遍（触发器在我们插 general 行时刚生成的半成品
        //    + 上一轮残留），再整份写入，否则会重复。
        if same_db {
            for table in ["fts_message_content", "fts_session_title"] {
                let (Ok(src_cols), Ok(dst_cols)) =
                    (table_columns(src, table), table_columns(dst, table))
                else {
                    continue; // 客户端版本差异，没有这张表就跳过
                };
                let cols = common_cols(&src_cols, &dst_cols);
                if cols.is_empty() {
                    continue;
                }
                dst.execute(
                    &format!("DELETE FROM \"{table}\" WHERE session_id=?"),
                    params![out_sid],
                )
                .map_err(|e| format!("清除旧词条 {table} 失败: {e}"))?;
                let col_list = cols
                    .iter()
                    .map(|c| format!("\"{c}\""))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut stmt = src
                    .prepare(&format!(
                        "SELECT {col_list} FROM \"{table}\" WHERE session_id=?"
                    ))
                    .map_err(|e| format!("源表 {table} 查询失败: {e}"))?;
                let rows: Vec<Vec<rusqlite::types::Value>> = stmt
                    .query_map(params![sid], |row| {
                        let mut vals = Vec::new();
                        for i in 0..cols.len() {
                            vals.push(row.get::<_, rusqlite::types::Value>(i)?);
                        }
                        Ok(vals)
                    })
                    .map_err(|e| format!("源表 {table} 读取失败: {e}"))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("源表 {table} 行读取失败: {e}"))?;
                if rows.is_empty() {
                    continue;
                }
                let sid_i = cols.iter().position(|c| c == "session_id");
                let mid_i = cols.iter().position(|c| c == "message_id");
                let mut out: Vec<Vec<rusqlite::types::Value>> = Vec::with_capacity(rows.len());
                for mut vals in rows {
                    if let Some(i) = sid_i {
                        vals[i] = rusqlite::types::Value::Text(out_sid.clone());
                    }
                    if let Some(i) = mid_i {
                        if let rusqlite::types::Value::Text(old) = &vals[i] {
                            if let Some(nm) = mid_map.get(old) {
                                vals[i] = rusqlite::types::Value::Text(nm.clone());
                            }
                        }
                    }
                    out.push(vals);
                }
                let placeholders = (0..cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
                let mut ins = dst
                    .prepare(&format!(
                        "INSERT INTO \"{table}\" ({col_list}) VALUES ({placeholders})"
                    ))
                    .map_err(|e| format!("目标表 {table} 插入准备失败: {e}"))?;
                for vals in &out {
                    let args: Vec<&dyn rusqlite::types::ToSql> = vals
                        .iter()
                        .map(|v| v as &dyn rusqlite::types::ToSql)
                        .collect();
                    ins.execute(rusqlite::params_from_iter(args))
                        .map_err(|e| format!("目标表 {table} 插入失败: {e}"))?;
                }
                n += out.len();
            }
        }

        // server_history_info / history_v2：历史/用量镜像行（同库复制时改写唯一键）
        for (table, col) in [("server_history_info", "conversation_id"), ("history_v2", "session_id")] {
            if same_db {
                if let Ok(v) = copy_history_aux(src, dst, table, col, sid, &out_sid, Some(&mid_map)) {
                    n += v;
                }
            } else if let Ok(added) = copy_rows(src, dst, table, &format!("{col} = ?"), sid) {
                n += added;
            }
        }
    }

    // session_project：会话-工程关联，客户端据此恢复会话的工程上下文（缺行会导致
    // 打开会话后无法继续对话）。同库复制时 session_id 改写为新会话。
    //
    // ⚠️ **同库复制时 project_id 必须一起改写**（踩过的坑）：`chat_session.project_id`
    // 上面已被改成 `dst_project_id`（目标账号的项目），而本表原本照抄源行 ⇒ 副本的
    // `session_project.project_id` 仍指向**源账号的旧项目**，两表打架。
    // 后果不是「显示不对」这么轻：Trae 客户端按 `session_project` 组织项目下的会话，
    // 于是这条副本挂在**旧项目**下；用户在客户端里删它时按当前项目归属校验，删除不生效，
    // **重启 Trae 后记录原地复活**（用户实测反馈）。所以这里必须与 `chat_session` 对齐。
    {
        let table = "session_project";
        let cols = common_cols(&table_columns(src, table)?, &table_columns(dst, table)?);
        if !cols.is_empty() {
            let col_list = cols
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = src
                .prepare(&format!("SELECT {col_list} FROM {table} WHERE session_id=?"))
                .map_err(|e| format!("源表 {table} 查询失败: {e}"))?;
            let rows: Vec<Vec<rusqlite::types::Value>> = stmt
                .query_map(params![sid], |row| {
                    let mut vals = Vec::new();
                    for i in 0..cols.len() {
                        vals.push(row.get::<_, rusqlite::types::Value>(i)?);
                    }
                    Ok(vals)
                })
                .map_err(|e| format!("源表 {table} 读取失败: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("源表 {table} 行读取失败: {e}"))?;
            if !rows.is_empty() {
                let sid_idx = cols
                    .iter()
                    .position(|c| *c == "session_id")
                    .ok_or_else(|| format!("{table} 缺 session_id 列"))?;
                let pid_idx = cols.iter().position(|c| *c == "project_id");
                // 仅在「同库复制且拿到了目标项目 id」时改写 project_id；跨库复制
                // （同 client 不同路径）时 project_id 沿用源行，与 chat_session 的行为一致。
                let new_pid = if same_db {
                    dst_project_id.clone()
                } else {
                    None
                };
                let mut out: Vec<Vec<rusqlite::types::Value>> = Vec::new();
                for mut vals in rows {
                    if same_db {
                        vals[sid_idx] = rusqlite::types::Value::Text(out_sid.clone());
                    }
                    if let (Some(pid), Some(pi)) = (new_pid.as_deref(), pid_idx) {
                        vals[pi] = rusqlite::types::Value::Text(pid.to_string());
                    }
                    out.push(vals);
                }
                let placeholders = (0..cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
                let mut ins = dst
                    .prepare(&format!("INSERT INTO {table} ({col_list}) VALUES ({placeholders})"))
                    .map_err(|e| format!("目标表 {table} 插入准备失败: {e}"))?;
                for vals in &out {
                    let args: Vec<&dyn rusqlite::types::ToSql> = vals
                        .iter()
                        .map(|v| v as &dyn rusqlite::types::ToSql)
                        .collect();
                    ins.execute(rusqlite::params_from_iter(args))
                        .map_err(|e| format!("目标表 {table} 插入失败: {e}"))?;
                }
                n += out.len();
            }
        }
    }

    // chat_turn：对话轮次（继续对话的语义结构）。同库复制时重生成 turn_id，
    // 消息引用（reply/response）经 mid_map 改写，并回写 chat_session.last_unread_turn_id。
    {
        let table = "chat_turn";
        let cols = common_cols(&table_columns(src, table)?, &table_columns(dst, table)?);
        if !cols.is_empty() {
            let col_list = cols
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = src
                .prepare(&format!("SELECT {col_list} FROM {table} WHERE session_id=?"))
                .map_err(|e| format!("源表 {table} 查询失败: {e}"))?;
            let rows: Vec<Vec<rusqlite::types::Value>> = stmt
                .query_map(params![sid], |row| {
                    let mut vals = Vec::new();
                    for i in 0..cols.len() {
                        vals.push(row.get::<_, rusqlite::types::Value>(i)?);
                    }
                    Ok(vals)
                })
                .map_err(|e| format!("源表 {table} 读取失败: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("源表 {table} 行读取失败: {e}"))?;
            if !rows.is_empty() {
                let sid_idx = cols
                    .iter()
                    .position(|c| *c == "session_id")
                    .ok_or_else(|| format!("{table} 缺 session_id 列"))?;
                let tid_idx = cols
                    .iter()
                    .position(|c| *c == "turn_id")
                    .ok_or_else(|| format!("{table} 缺 turn_id 列"))?;
                let mut turn_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
                let mut out: Vec<Vec<rusqlite::types::Value>> = Vec::new();
                for mut vals in rows {
                    if same_db {
                        let old_tid = match &vals[tid_idx] {
                            rusqlite::types::Value::Text(t) => t.clone(),
                            _ => String::new(),
                        };
                        let new_tid = native_hex_id();
                        turn_map.insert(old_tid, new_tid.clone());
                        vals[tid_idx] = rusqlite::types::Value::Text(new_tid);
                        vals[sid_idx] = rusqlite::types::Value::Text(out_sid.clone());
                        for c in ["reply_to_message_id", "response_message_id"] {
                            if let Some(ri) = cols.iter().position(|x| *x == c) {
                                if let rusqlite::types::Value::Text(rep) = &vals[ri] {
                                    if let Some(nm) = mid_map.get(rep) {
                                        vals[ri] = rusqlite::types::Value::Text(nm.clone());
                                    }
                                }
                            }
                        }
                    }
                    out.push(vals);
                }
                let placeholders = (0..cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
                let mut ins = dst
                    .prepare(&format!("INSERT INTO {table} ({col_list}) VALUES ({placeholders})"))
                    .map_err(|e| format!("目标表 {table} 插入准备失败: {e}"))?;
                for vals in &out {
                    let args: Vec<&dyn rusqlite::types::ToSql> = vals
                        .iter()
                        .map(|v| v as &dyn rusqlite::types::ToSql)
                        .collect();
                    ins.execute(rusqlite::params_from_iter(args))
                        .map_err(|e| format!("目标表 {table} 插入失败: {e}"))?;
                }
                n += out.len();
                if !turn_map.is_empty() {
                    let mut up = dst
                        .prepare("UPDATE chat_session SET last_unread_turn_id=? WHERE session_id=? AND last_unread_turn_id=?")
                        .map_err(|e| format!("回写 last_unread_turn_id 准备失败: {e}"))?;
                    for (old_tid, new_tid) in &turn_map {
                        up.execute(params![new_tid, out_sid, old_tid])
                            .map_err(|e| format!("回写 last_unread_turn_id 失败: {e}"))?;
                    }
                }
            }
        }
    }

    // agent_run：agent 执行记录。同库复制时重生成唯一键 agent_run_id，
    // 会话内 parent_run_id 同步映射，否则父子关系指向源会话的运行。
    {
        let table = "agent_run";
        let cols = common_cols(&table_columns(src, table)?, &table_columns(dst, table)?);
        if !cols.is_empty() {
            let col_list = cols
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = src
                .prepare(&format!("SELECT {col_list} FROM {table} WHERE session_id=?"))
                .map_err(|e| format!("源表 {table} 查询失败: {e}"))?;
            let rows: Vec<Vec<rusqlite::types::Value>> = stmt
                .query_map(params![sid], |row| {
                    let mut vals = Vec::new();
                    for i in 0..cols.len() {
                        vals.push(row.get::<_, rusqlite::types::Value>(i)?);
                    }
                    Ok(vals)
                })
                .map_err(|e| format!("源表 {table} 读取失败: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("源表 {table} 行读取失败: {e}"))?;
            if !rows.is_empty() {
                let sid_idx = cols
                    .iter()
                    .position(|c| *c == "session_id")
                    .ok_or_else(|| format!("{table} 缺 session_id 列"))?;
                let rid_idx = cols
                    .iter()
                    .position(|c| *c == "agent_run_id")
                    .ok_or_else(|| format!("{table} 缺 agent_run_id 列"))?;
                let parent_idx = cols.iter().position(|c| *c == "parent_run_id");
                let mut run_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
                let mut out: Vec<Vec<rusqlite::types::Value>> = Vec::new();
                for mut vals in rows {
                    if same_db {
                        let old_rid = match &vals[rid_idx] {
                            rusqlite::types::Value::Text(t) => t.clone(),
                            _ => String::new(),
                        };
                        let new_rid = native_hex_id();
                        run_map.insert(old_rid, new_rid.clone());
                        vals[rid_idx] = rusqlite::types::Value::Text(new_rid);
                        vals[sid_idx] = rusqlite::types::Value::Text(out_sid.clone());
                        if let Some(pi) = parent_idx {
                            if let rusqlite::types::Value::Text(p) = &vals[pi] {
                                if let Some(np) = run_map.get(p) {
                                    vals[pi] = rusqlite::types::Value::Text(np.clone());
                                }
                            }
                        }
                    }
                    out.push(vals);
                }
                let placeholders = (0..cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
                let mut ins = dst
                    .prepare(&format!("INSERT INTO {table} ({col_list}) VALUES ({placeholders})"))
                    .map_err(|e| format!("目标表 {table} 插入准备失败: {e}"))?;
                for vals in &out {
                    let args: Vec<&dyn rusqlite::types::ToSql> = vals
                        .iter()
                        .map(|v| v as &dyn rusqlite::types::ToSql)
                        .collect();
                    ins.execute(rusqlite::params_from_iter(args))
                        .map_err(|e| format!("目标表 {table} 插入失败: {e}"))?;
                }
                n += out.len();
            }
        }
    }
    if let Some(cb) = on_log {
        cb(&format!("会话 {sid}：已复制 {n} 行{}", if same_db { "（新 id）" } else { "" }));
    }
    // 回传实际写入的目标 session_id：同库复制时它是个新值，调用方（登记关联）
    // 必须用这个值，而不是源 id。
    Ok(Some((n, out_sid)))
}

/// 复制历史/用量镜像表（server_history_info / history_v2）供同库复制使用：
/// 改写会话关联列并重新生成唯一键（history_id / client_history_id / history_v2_id），
/// 避免与库内已有行冲突；整型自增 `id` 跳过，由 SQLite 分配。
/// `mid_map`（旧 message_id → 新 message_id）用于改写 history_v2.message_id，
/// 否则历史行会指向源会话的消息（数据串库）。
fn copy_history_aux(
    src: &Connection,
    dst: &Connection,
    table: &str,
    link_col: &str,
    old_sid: &str,
    new_sid: &str,
    mid_map: Option<&std::collections::HashMap<String, String>>,
) -> Result<usize, String> {
    let cols = common_cols(&table_columns(src, table)?, &table_columns(dst, table)?);
    if cols.is_empty() {
        return Ok(0);
    }
    let col_list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let select_sql = format!("SELECT {col_list} FROM {table} WHERE {link_col} = ?");
    let mut stmt = src
        .prepare(&select_sql)
        .map_err(|e| format!("源表 {table} 查询失败: {e}"))?;
    let rows = stmt
        .query_map(params![old_sid], |row| {
            let mut vals = Vec::new();
            for i in 0..cols.len() {
                vals.push(row.get::<_, rusqlite::types::Value>(i)?);
            }
            Ok(vals)
        })
        .map_err(|e| format!("源表 {table} 读取失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("源表 {table} 行读取失败: {e}"))?;
    if rows.is_empty() {
        return Ok(0);
    }
    let link_idx = cols
        .iter()
        .position(|c| *c == link_col)
        .ok_or_else(|| format!("{table} 缺 {link_col} 列"))?;
    let mut out = rows;
    for v in out.iter_mut() {
        v[link_idx] = rusqlite::types::Value::Text(new_sid.to_string());
        if let Some(idx) = cols.iter().position(|c| c.as_str() == "history_id") {
            v[idx] = rusqlite::types::Value::Text(Uuid::new_v4().simple().to_string());
        }
        if let Some(idx) = cols.iter().position(|c| c.as_str() == "client_history_id") {
            v[idx] = rusqlite::types::Value::Text(Uuid::new_v4().simple().to_string());
        }
        if let Some(idx) = cols.iter().position(|c| c.as_str() == "history_v2_id") {
            v[idx] = rusqlite::types::Value::Text(Uuid::new_v4().simple().to_string());
        }
        // history_v2.message_id：随消息复制改写为新 message_id（避免指向源会话消息）
        if let Some(map) = mid_map {
            if let Some(idx) = cols.iter().position(|c| c.as_str() == "message_id") {
                if let rusqlite::types::Value::Text(old) = &v[idx] {
                    if let Some(nm) = map.get(old) {
                        v[idx] = rusqlite::types::Value::Text(nm.clone());
                    }
                }
            }
        }
    }
    let placeholders = (0..cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
    let insert_sql = format!("INSERT INTO {table} ({col_list}) VALUES ({placeholders})");
    let mut ins = dst
        .prepare(&insert_sql)
        .map_err(|e| format!("目标表 {table} 插入准备失败: {e}"))?;
    for vals in &out {
        let args: Vec<&dyn rusqlite::types::ToSql> = vals
            .iter()
            .map(|v| v as &dyn rusqlite::types::ToSql)
            .collect();
        ins.execute(rusqlite::params_from_iter(args))
            .map_err(|e| format!("目标表 {table} 插入失败: {e}"))?;
    }
    Ok(out.len())
}

/// 为目标 uid 在目标库查找或新建 project（会话归属改写）。
/// 取一行里某列的值（列名不在 cols 中返回 None）。
fn col_value<'a>(
    row: &'a [rusqlite::types::Value],
    cols: &[&String],
    name: &str,
) -> Option<&'a rusqlite::types::Value> {
    let idx = cols.iter().position(|c| *c == name)?;
    row.get(idx)
}

/// 按主键取一行（列交集），None 表示行不存在。
fn read_row(
    conn: &Connection,
    table: &str,
    cols: &[&String],
    pk_col: &str,
    pk: &str,
) -> Result<Option<Vec<rusqlite::types::Value>>, String> {
    let col_list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn
        .prepare(&format!("SELECT {col_list} FROM {table} WHERE {pk_col}=?"))
        .map_err(|e| format!("读取 {table} 失败: {e}"))?;
    let mut rows = stmt
        .query_map(params![pk], |r| {
            let mut vals = Vec::new();
            for i in 0..cols.len() {
                vals.push(r.get::<_, rusqlite::types::Value>(i)?);
            }
            Ok(vals)
        })
        .map_err(|e| format!("读取 {table} 失败: {e}"))?;
    match rows.next() {
        Some(Ok(v)) => Ok(Some(v)),
        Some(Err(e)) => Err(format!("读取 {table} 行失败: {e}")),
        None => Ok(None),
    }
}

/// 源 project 行缺失时的兜底行（最小列集 + 默认值）。
fn minimal_project_row(cols: &[&String], pid: &str) -> Vec<rusqlite::types::Value> {
    cols.iter()
        .map(|c| match c.as_str() {
            "project_id" => rusqlite::types::Value::Text(pid.to_string()),
            "source" => rusqlite::types::Value::Text("native-ide".into()),
            "workspace_status" => rusqlite::types::Value::Text("single_root".into()),
            "created_at" | "updated_at" => rusqlite::types::Value::Integer(chrono::Utc::now().timestamp()),
            _ => rusqlite::types::Value::Null,
        })
        .collect()
}

/// 为目标 uid 在目标库查找或新建 project（会话归属改写）。
/// 查找规则：同 absolute_path 且未删除的 project 优先；找不到则复制源 project 行并换新 id。
fn ensure_project_for(
    src: &Connection,
    dst: &Connection,
    sid: &str,
    dst_uid: &str,
) -> Result<String, String> {
    let src_pid: String = src
        .query_row("SELECT project_id FROM chat_session WHERE session_id=?", params![sid], |r| r.get(0))
        .map_err(|e| format!("读取源会话 project 失败: {e}"))?;
    // 源 project 行（列交集保真；缺行时用最小兜底集）
    let src_cols = table_columns(src, "project")?;
    let dst_cols = table_columns(dst, "project")?;
    let cols: Vec<&String> = src_cols
        .iter()
        .filter(|c| dst_cols.contains(c) && c.as_str() != "id")
        .collect();
    if cols.is_empty() {
        return Err("源/目标库 project 表无公共列，无法建立归属".into());
    }
    let mut row = read_row(src, "project", &cols, "project_id", &src_pid)?
        .unwrap_or_else(|| minimal_project_row(&cols, &src_pid));
    // 查找目标库中归属 dst_uid 的同路径 project
    if let Some(rusqlite::types::Value::Text(path)) = col_value(&row, &cols, "absolute_path") {
        if !path.is_empty() {
            if let Ok(existing) = dst.query_row(
                "SELECT project_id FROM project WHERE user_id=? AND absolute_path=? AND (deleted_at IS NULL OR deleted_at=0) LIMIT 1",
                params![dst_uid, path],
                |r| r.get::<_, String>(0),
            ) {
                return Ok(existing);
            }
        }
    }
    // 新建：复制源 project 行，project_id / biz_project_id 换新，user_id = dst_uid
    let new_pid = native_hex_id();
    let new_biz = native_hex_id();
    for (name, v) in [
        ("project_id", rusqlite::types::Value::Text(new_pid.clone())),
        ("user_id", rusqlite::types::Value::Text(dst_uid.to_string())),
    ] {
        if let Some(idx) = cols.iter().position(|c| *c == name) {
            row[idx] = v;
        }
    }
    if let Some(idx) = cols.iter().position(|c| *c == "biz_project_id") {
        row[idx] = rusqlite::types::Value::Text(new_biz.clone());
    }
    let col_list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let placeholders = (0..cols.len()).map(|_| "?").collect::<Vec<_>>().join(", ");
    let inserted = dst.execute(
        &format!("INSERT INTO project ({col_list}) VALUES ({placeholders})"),
        rusqlite::params_from_iter(row.iter().map(|v| v as &dyn rusqlite::types::ToSql)),
    );
    match inserted {
        Ok(_) => Ok(new_pid),
        Err(e) => {
            // 并发/重复：退回按 (user_id, absolute_path) 查找一次
            if let Some(rusqlite::types::Value::Text(path)) = col_value(&row, &cols, "absolute_path") {
                if !path.is_empty() {
                    if let Ok(existing) = dst.query_row(
                        "SELECT project_id FROM project WHERE user_id=? AND absolute_path=? LIMIT 1",
                        params![dst_uid, path],
                        |r| r.get::<_, String>(0),
                    ) {
                        return Ok(existing);
                    }
                }
            }
            Err(format!("为目标账号创建 project 失败: {e}"))
        }
    }
}

// ---------------------------------------------------------------------------
// 对外命令
// ---------------------------------------------------------------------------

/// 从 storage 文本解析 uid 的便捷入口。
fn storage_uid_of(client_key: &str) -> Option<String> {
    let path = storage_json_path(get_client(client_key)?);
    uid_from_storage_text(&std::fs::read_to_string(path).ok()?)
}

fn uid_suffix(uid: &str) -> String {
    uid[uid.len().saturating_sub(6)..].to_string()
}

/// 账号标识 → uid：`live-<uid>` / `local-<uid>` 直取；否则查 vault 账号档案。
fn account_uid(client_key: &str, account_id: &str) -> Result<String, String> {
    for prefix in ["live-", "local-"] {
        if let Some(rest) = account_id.strip_prefix(prefix) {
            if !rest.is_empty() {
                return Ok(rest.to_string());
            }
        }
    }
    uid_of_account(client_key, account_id)
        .ok_or_else(|| format!("未知账号：{account_id}（该账号未建档或无法解析 uid）"))
}

/// 目标账号导入就绪状态（不写库）。
pub fn inspect_account(client_key: &str, account_id: &str) -> Result<Value, String> {
    let Some(client) = get_client(client_key) else {
        return Err(format!("未知客户端: {client_key}"));
    };
    let uid = account_uid(client_key, account_id)?;
    let db = database_path(client);
    let exists = db.is_file();
    let running = is_running(client);
    let key_ready = load_saved_key(client_key)
        .map(|k| {
            let mut page1 = [0u8; PAGE_SZ];
            std::fs::File::open(&db)
                .and_then(|mut f| {
                    use std::io::Read;
                    f.read_exact(&mut page1)
                })
                .map(|_| {
                    let key = crate::modules::trae_decrypt::hex_to_bytes(&k);
                    verify_page1_hmac(&key, &page1)
                })
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let mut sessions_now = 0i64;
    if exists {
        if let Ok(conn) = Connection::open(&db) {
            if let Ok(v) = conn.query_row("SELECT count(*) FROM chat_session", [], |r| r.get(0)) {
                sessions_now = v;
            }
        }
    }
    Ok(json!({
        "client_key": client_key,
        "client_label": client.label,
        "account_id": account_id,
        "uid": uid,
        "db_exists": exists,
        "running": running,
        "key_ready": key_ready,
        "key_source": if key_ready { "saved" } else if running { "scan_available" } else { "missing" },
        "sessions_now": sessions_now,
    }))
}

/// oauth.json 里的账号显示名（displayName / userName），无则 None。
fn oauth_display(client_key: &str, id: &str) -> Option<String> {
    let p = crate::modules::trae_vault::account_dir(client_key, id).join("oauth.json");
    let text = std::fs::read_to_string(p).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    for k in ["displayName", "userName"] {
        if let Some(s) = v.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
            return Some(s.to_string());
        }
    }
    None
}

/// 账号维度候选：**全部本机账号**——vault 账号 + 解密库出现的账号（local）+
/// 当前登录态，按 (客户端, uid) 去重，可自由切换目标账号。
/// 不排除所选会话的归属账号，而是用 `is_source` 标记它（前端禁用同客户端同归属的候选）。
fn push_candidate(
    out: &mut Vec<Value>,
    client_label: &str,
    ck: &str,
    account_id: &str,
    label: &str,
    display: Option<String>,
    uid: &str,
    kind: &str,
    db_exists: bool,
    is_current: bool,
    is_source: bool,
) {
    out.push(json!({
        "client_key": ck,
        "client_label": client_label,
        "account_id": account_id,
        "label": label,
        "display": display,
        "uid": uid,
        "kind": kind,
        "db_exists": db_exists,
        "is_current": is_current,
        "is_source": is_source,
    }));
}

/// `src_client_key` 为源（浏览会话的）客户端；`owner_uid` 为所选会话归属账号 uid。
pub fn list_account_candidates(src_client_key: &str, owner_uid: Option<&str>) -> Vec<Value> {
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut out = Vec::new();
    for c in list_installed_clients() {
        if !c.installed {
            continue;
        }
        let ck = c.key.to_string();
        let db_exists = database_path(get_client(&ck).expect("客户端 key")).is_file();
        let live_uid = storage_uid_of(&ck);
        // 1) vault 账号（carrier / oauth，信息最全）
        for id in list_vault_accounts(&ck) {
            let Some(uid) = uid_of_account(&ck, &id) else {
                continue;
            };
            if !seen.insert((ck.clone(), uid.clone())) {
                continue;
            }
            let kind = read_meta(&ck, &id).map(|m| m.kind).unwrap_or_else(|| "carrier".into());
            // 与账号库首页一致：优先 profile 昵称（GetUserInfo）→ 载体 storage.json 用户名 → oauth displayName
            let display = crate::modules::trae_vault::display_name(&ck, &id).or_else(|| oauth_display(&ck, &id));
            let label = match &display {
                Some(d) => format!("{d}（uid …{}）", uid_suffix(&uid)),
                None => format!("{}（uid …{}）", id, uid_suffix(&uid)),
            };
            push_candidate(
                &mut out,
                &c.label,
                &ck,
                &id,
                &label,
                display,
                &uid,
                &kind,
                db_exists,
                live_uid.as_deref() == Some(uid.as_str()),
                ck == src_client_key && owner_uid == Some(uid.as_str()),
            );
        }
        // 2) 解密库账号（local：库内 chat_session 归属的全部 user_id，
        //    覆盖「登录过但没备份凭证」的账号）
        let dec = decrypted_db_path(&ck);
        if dec.is_file() {
            if let Ok(conn) = Connection::open(&dec) {
                if let Ok(mut stmt) = conn.prepare(
                    "SELECT DISTINCT p.user_id FROM chat_session s \
                     JOIN project p ON s.project_id = p.project_id \
                     WHERE p.user_id IS NOT NULL AND p.user_id != ''",
                ) {
                    if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                        for uid in rows.flatten() {
                            if !seen.insert((ck.clone(), uid.clone())) {
                                continue;
                            }
                            let label = crate::modules::trae_vault::account_label(&ck, &uid);
                            push_candidate(
                                &mut out,
                                &c.label,
                                &ck,
                                &format!("local-{uid}"),
                                &label,
                                None,
                                &uid,
                                "local",
                                true,
                                live_uid.as_deref() == Some(uid.as_str()),
                                ck == src_client_key && owner_uid == Some(uid.as_str()),
                            );
                        }
                    }
                }
            }
        }
        // 3) 当前 live 登录态（与上面去重）
        if let Some(uid) = storage_uid_of(&ck) {
            if seen.insert((ck.clone(), uid.clone())) {
                push_candidate(
                    &mut out,
                    &c.label,
                    &ck,
                    &format!("live-{uid}"),
                    &format!("当前登录（uid …{}）", uid_suffix(&uid)),
                    None,
                    &uid,
                    "live",
                    db_exists,
                    true,
                    ck == src_client_key && owner_uid == Some(uid.as_str()),
                );
            }
        }
    }
    out
}

/// 有会话库但未列入候选的客户端提示（无解密副本且拿不到密钥时给出引导）。
pub fn client_hints() -> Vec<String> {
    let mut out = Vec::new();
    for c in list_installed_clients() {
        if !c.installed {
            continue;
        }
        let ck = c.key.to_string();
        if decrypted_db_path(&ck).is_file() {
            continue;
        }
        let Some(client) = get_client(&ck) else { continue };
        if !database_path(client).is_file() {
            continue;
        }
        if load_saved_key(&ck).is_some() || is_running(client) {
            continue;
        }
        out.push(format!(
            "客户端「{}」有本地会话库但未解密（无已保存密钥且未运行），其账号暂未列入候选；请先启动该客户端并对其执行『解密会话库』后再试。",
            c.label
        ));
    }
    out
}

/// 所选会话的归属账号 uid（解密库 chat_session → project.user_id）。
pub fn session_owner_uid(client_key: &str, sid: &str) -> Option<String> {
    let conn = open_decrypted(client_key).ok()?;
    let v: rusqlite::types::Value = conn
        .query_row(
            "SELECT p.user_id FROM chat_session s JOIN project p ON s.project_id = p.project_id \
             WHERE s.session_id = ?",
            params![sid],
            |r| r.get(0),
        )
        .ok()?;
    match v {
        rusqlite::types::Value::Text(s) => Some(s),
        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
        _ => None,
    }
}

/// 扫描目标库里「`session_project.project_id` 与 `chat_session.project_id` 不一致」的会话，
/// 返回 `(session_id, 当前关联的 project_id, 会话自身的 project_id)`。
///
/// 这是**诊断 + 自愈**的共用查询：诊断用它列清单，自愈用它算待修行数。
/// 只读，不改任何数据。
pub fn find_misaligned_session_projects(client_key: &str) -> Result<Vec<(String, String, String)>, String> {
    let conn = open_decrypted(client_key)?;
    let mut stmt = conn
        .prepare(
            "SELECT sp.session_id, ifnull(sp.project_id,''), ifnull(s.project_id,'') \
             FROM session_project sp JOIN chat_session s ON s.session_id = sp.session_id \
             WHERE ifnull(s.project_id,'') <> ifnull(sp.project_id,'')",
        )
        .map_err(|e| format!("查询工程归属不一致失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("查询工程归属不一致失败: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取工程归属不一致失败: {e}"))?;
    Ok(rows)}

/// **自愈存量脏数据**：把目标库里所有 `session_project.project_id` 对齐到
/// `chat_session.project_id`，并同步改写这些会话 `context` 里的 `last_real_project_id`。
///
/// 为什么需要它：早前版本的同库复制**只改 `session_id`、照抄源行的 `project_id`**，
/// 于是副本的 `session_project` 仍指向**源账号的旧项目**。用户在 Trae 客户端里点删除
/// **删不掉**（客户端按项目归属校验），**重启 Trae 后记录复活**（用户实测反馈）。
/// 复制逻辑已修，但**历史副本的脏数据还在库里** —— 本命令负责一次性清干净。
///
/// 走与删除/导入**完全一致**的安全链路：结束客户端 → 解密（含合并 WAL）→ 改明文 →
/// 增量加密回写 → 备份 + 原子替换 → 重启客户端。整库只重写变动的那几页。
///
/// 把实时库 WAL 里**已提交但未 checkpoint** 的帧合并进「工作副本」`plain`。
///
/// ⚠️ 任何「解密主库 → 改 → 回写 → 删 WAL」的路径**都必须先做这一步**，否则一定丢数据：
/// Trae 客户端是 WAL 模式，刚写下的内容很可能**还只在 `database.db-wal` 里**，
/// 只解密主库等于拿到一份陈旧内容，而回写完成后紧接着就会删掉 WAL ⇒ 这些改动被**永久抹掉**。
/// （删除路径 `delete_sessions_core`、同步路径 `sync_groups_inner` 都有这一步；
/// 复制 `import_sessions` 与自愈 `heal_session_projects` 曾经漏掉。）
///
/// ⚠️ 基准副本 `orig_plain`（增量回写的逐页比对基准）**必须保持「纯解密」、绝不合并** ——
/// 它要与加密库的真实内容一一对应；只有 `plain` 带上 WAL 的新内容，
/// 那些页才会被判为「已变动」而被重写进新库。
fn merge_live_wal_into(plain: &Path, live: &Path, enc_key_hex: &str, log: &dyn Fn(&str)) -> Result<usize, String> {
    let wal = PathBuf::from(format!("{}-wal", live.to_string_lossy()));
    let merged = crate::modules::trae_delete::merge_wal_into_plain(plain, &wal, enc_key_hex)
        .map_err(|e| format!("合并 WAL 失败: {e}"))?;
    if merged > 0 {
        log(&format!(
            "已合并 WAL 中 {merged} 个已提交页面帧（防止丢失未落盘的最新改动）"
        ));
        // 合并可能覆盖首页，reserved 字段要重新补一次。
        patch_reserved_field(plain)?;
    }
    Ok(merged)
}

/// ⚠️ 只对**同一客户端**做：跨客户端（Trae ↔ WorkBuddy）的 project 语义不同，
/// 不该拿这套规则去改对方的数据。
pub fn heal_session_projects(client_key: &str, on_log: Option<&dyn Fn(&str)>) -> Result<Value, String> {
    let log = |m: &str| {
        if let Some(f) = on_log {
            f(m);
        }
    };
    let Some(client) = get_client(client_key) else {
        return Err(format!("未知客户端: {client_key}"));
    };
    let live = database_path(client);

    // 1) 密钥 + 结束客户端（文件替换前提）
    let enc_key_hex = resolve_live_key_for_heal(client_key, &live)?;
    log("结束客户端进程（文件替换前提）…");
    let was_running = is_running(client);
    if was_running {
        let killed = kill_all(client);
        if !killed.is_empty() {
            log(&format!("已结束 {} 个进程", killed.len()));
        }
        if !wait_until_stopped(client, 20_000) {
            return Err("客户端未能在 20 秒内退出，已放弃（未改动任何文件）".into());
        }
    }

    // 2) 实时库 → 明文副本（合并 WAL，保证看到客户端刚写入的状态）
    let work = store_dir()
        .join("trae")
        .join("heal_tmp")
        .join(format!("{client_key}-{}", chrono::Local::now().timestamp()));
    std::fs::create_dir_all(&work).map_err(|e| format!("创建工作目录失败: {e}"))?;
    let orig_plain = work.join("live-orig.db");
    log("解密实时库为明文副本…");
    let rep = decrypt_database(&live, &enc_key_hex, &orig_plain, Some(&|m| log(&format!("   {m}"))))?;
    log(&format!("实时库 {} 页", rep.pages));
    patch_reserved_field(&orig_plain)?;
    let plain = work.join("live-plain.db");
    std::fs::copy(&orig_plain, &plain).map_err(|e| format!("复制明文副本失败: {e}"))?;
    patch_reserved_field(&plain)?;

    // 2b) 合并 WAL 中已提交但未 checkpoint 的帧。
    // ⚠️ 不做这一步，第 4 步回写后会删掉 WAL ⇒ 客户端刚写下、还没落盘的内容被永久抹掉。
    merge_live_wal_into(&plain, &live, &enc_key_hex, &|m| log(m))?;

    // 3) 改明文：对齐 session_project.project_id + 修 context.last_real_project_id
    let (fixed_sp, fixed_ctx, targets) = {
        let conn = Connection::open(&plain).map_err(|e| format!("打开明文副本失败: {e}"))?;
        let pairs: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare(
                    "SELECT sp.session_id, s.project_id \
                     FROM session_project sp JOIN chat_session s ON s.session_id = sp.session_id \
                     WHERE ifnull(s.project_id,'') <> ifnull(sp.project_id,'')",
                )
                .map_err(|e| format!("查询不一致失败: {e}"))?;
            let mapped = stmt
                .query_map([], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .map_err(|e| format!("查询不一致失败: {e}"))?;
            mapped
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("读取不一致失败: {e}"))?
        };
        let n_sp = conn
            .execute(
                "UPDATE session_project SET project_id = (\
                     SELECT s.project_id FROM chat_session s \
                     WHERE s.session_id = session_project.session_id\
                 ) \
                 WHERE EXISTS (\
                     SELECT 1 FROM chat_session s \
                     WHERE s.session_id = session_project.session_id \
                       AND ifnull(s.project_id, '') <> ifnull(session_project.project_id, '')\
                 )",
                [],
            )
            .map_err(|e| format!("对齐 session_project 失败: {e}"))?;

        // context.last_real_project_id 也一并修正（只碰有该列的库）
        let has_ctx = conn
            .prepare("SELECT context FROM chat_session LIMIT 1")
            .is_ok();
        let mut n_ctx = 0usize;
        if has_ctx {
            // 先收集需要改的行（值可能与新项目不同），再逐行改写 —— 逐行做是为了能用
            // 纯函数 `rewrite_last_real_project_id`，避免在 SQL 里拼 JSON。
            let items: Vec<(String, String, String)> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT s.session_id, s.project_id, ifnull(s.context,'') \
                         FROM chat_session s \
                         WHERE s.session_id IN (SELECT session_id FROM session_project) \
                           AND ifnull(s.context,'') <> ''",
                    )
                    .map_err(|e| format!("查询 context 失败: {e}"))?;
                let mapped = stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })
                    .map_err(|e| format!("查询 context 失败: {e}"))?;
                mapped
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| format!("读取 context 失败: {e}"))?
            };
            for (sid, pid, ctx) in items {
                if let Some(next) = rewrite_last_real_project_id(&ctx, &pid) {
                    let n = conn
                        .execute(
                            "UPDATE chat_session SET context = ?1 WHERE session_id = ?2",
                            params![next, sid],
                        )
                        .map_err(|e| format!("改写 context 失败: {e}"))?;
                    n_ctx += n;
                }
            }
        }
        let targets: Vec<String> = pairs.into_iter().map(|(sid, _)| sid).collect();
        conn.close().map_err(|(_, e)| format!("关闭明文副本失败: {e}"))?;
        (n_sp, n_ctx, targets)
    };

    if fixed_sp == 0 && fixed_ctx == 0 {
        let _ = std::fs::remove_dir_all(&work);
        log("没有需要修正的工程归属，库未改动");
        if was_running {
            let _ = launch(client_key, None);
        }
        return Ok(json!({
            "checked": true,
            "fixed_sessions": 0,
            "fixed_contexts": 0,
            "sessions": [],
            "relaunched": was_running,
            "note": "所有会话的工程归属都一致，无需修正",
        }));
    }

    // 4) 备份原库 + 增量加密回写 + 原子替换
    let backup = crate::modules::trae_export::import_backup_dir()
        .join(format!("heal-{}", chrono::Local::now().timestamp()));
    std::fs::create_dir_all(&backup).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let backup_db = backup.join("database.db");
    std::fs::copy(&live, &backup_db).map_err(|e| format!("备份原库失败: {e}"))?;
    // WAL 也一并备份：客户端强杀后的最新状态都在里面
    let wal = PathBuf::from(format!("{}-wal", live.to_string_lossy()));
    if wal.exists() {
        let _ = std::fs::copy(&wal, backup.join("database.db-wal"));
    }

    let new_db = work.join("live-new.db");
    log("增量加密回写（只重写变动页）…");
    let stats = write_db_incremental_with(
        &enc_key_hex,
        &live,
        Some(&orig_plain),
        &plain,
        &new_db,
        Some(&|m| log(&format!("   {m}"))),
    )?;
    log(&format!(
        "回写完成：全库 {} 页，仅重写 {} 页（{:.2} MB）",
        stats.pages,
        stats.changed_pages,
        stats.changed_bytes as f64 / 1_048_576.0
    ));

    // 原子替换（同目录 rename）
    let bak_live = work.join("live-old.db");
    std::fs::rename(&live, &bak_live).map_err(|e| format!("移开原库失败: {e}"))?;
    if let Err(e) = std::fs::rename(&new_db, &live) {
        let _ = std::fs::rename(&bak_live, &live);
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("替换实时库失败（已回滚）：{e}"));
    }
    // WAL 是旧页的残留，替换主库后必须清掉，否则会把旧内容又合并回来
    let _ = std::fs::remove_file(&wal);
    let _ = std::fs::remove_file(format!("{}-shm", live.to_string_lossy()));
    let _ = std::fs::remove_dir_all(&work);
    drop(bak_live);

    // 5) 同步刷新解密快照（否则列表还显示旧数据）
    let _ = std::fs::remove_file(decrypted_db_path(client_key));
    let _ = std::fs::remove_file(crate::modules::trae_export::snapshot_meta_path(client_key));

    // 6) 重启客户端
    let mut relaunched = false;
    if was_running {
        match launch(client_key, None) {
            Ok(_) => relaunched = true,
            Err(e) => log(&format!("重启客户端失败（请手动启动）：{e}")),
        }
    }

    log(&format!(
        "已修正 {fixed_sp} 条工程归属、{fixed_ctx} 条会话上下文（备份在 {}）",
        backup.display()
    ));
    Ok(json!({
        "checked": true,
        "fixed_sessions": fixed_sp,
        "fixed_contexts": fixed_ctx,
        "sessions": targets,
        "backup": backup.to_string_lossy(),
        "relaunched": relaunched,
        "note": "已把副本的工程归属对齐到其真实账号的项目；客户端里现在应当能正常删除了",
    }))
}

/// 自愈链路用的密钥解析：存盘密钥优先（首页 HMAC 校验），过期则从进程内存重扫。
fn resolve_live_key_for_heal(client_key: &str, live: &Path) -> Result<String, String> {
    if let Some(k) = load_saved_key(client_key) {
        if k.trim().len() == 64 {
            if let Ok(page1) = read_first_page(live) {
                if verify_page1_hmac(&hex_to_bytes(&k), &page1) {
                    return Ok(k);
                }
            }
        }
    }
    let Some(client) = get_client(client_key) else {
        return Err("未知客户端".into());
    };
    if is_running(client) {
        let scan = scan_for_key(client_key, live, None).map_err(|e| format!("重新扫描密钥失败：{e}"))?;
        if let Some(k) = scan.key {
            let _ = save_key(client_key, &k);
            return Ok(k);
        }
    }
    Err("实时库密钥不匹配且无法自动恢复：请先启动该客户端并登录对应账号后重试。".into())
}

/// 读文件首页（4096 字节）。
fn read_first_page(path: &Path) -> Result<[u8; PAGE_SZ], String> {
    use std::io::Read;
    let mut buf = [0u8; PAGE_SZ];
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// 执行导入：源账号（已解密）→ 目标账号本地库。
/// `dst_uid` 为目标账号 uid（归属改写依据）；源目标同一客户端时为同库复制（新 id）。
pub fn import_sessions(
    src_client_key: &str,
    dst_client_key: &str,
    dst_uid: Option<&str>,
    session_ids: &[String],
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let same_db = src_client_key == dst_client_key;
    let Some(dst_client) = get_client(dst_client_key) else {
        return Err(format!("未知目标客户端: {dst_client_key}"));
    };
    let log = |m: &str| {
        if let Some(cb) = on_log {
            cb(m);
        }
    };

    // 0) 源库必须是已解密状态
    let src_db = decrypted_db_path(src_client_key);
    if !src_db.is_file() {
        return Err(format!(
            "未找到源账号解密库：{}\n请先在「Trae 记录」页对源账号运行「扫描密钥并解密」。",
            src_db.display()
        ));
    }
    let src_conn = open_decrypted(src_client_key)?;

    // 同客户端跨账号：必须给出目标 uid。源账号按「所选会话的归属账号」判定
    // （而非解密库中出现最多的账号），已属于目标账号的会话直接跳过。
    let session_ids: Vec<String> = if same_db {
        let uid = dst_uid.ok_or("同客户端导入必须指定目标账号 uid")?;
        let mut kept: Vec<String> = Vec::new();
        for sid in session_ids {
            match session_owner_uid(src_client_key, sid) {
                Some(o) if o == uid => {
                    log(&format!("会话 {sid} 已属于目标账号（uid …{}），跳过（无需复制）", uid_suffix(&uid)));
                }
                _ => kept.push(sid.clone()),
            }
        }
        if kept.is_empty() {
            return Err("目标账号就是源账号（所选会话均已属于该账号），无需导入".into());
        }
        log(&format!("同客户端（{}）跨账号复制：源记录保留，会话归属改写为目标账号", dst_client.label));
        kept
    } else {
        session_ids.to_vec()
    };

    // 源账号 uid：登记关联时要用它作为「源」一侧。取所选会话的归属账号——
    // 同库跨账号场景下它们必然同属一个账号（上面已把属于目标账号的剔掉）。
    let src_owner_uid: Option<String> = if same_db {
        session_ids
            .iter()
            .find_map(|sid| session_owner_uid(src_client_key, sid))
    } else {
        None
    };

    // 1) 目标库与密钥
    let dst_db = database_path(&dst_client);
    if !dst_db.is_file() {
        return Err(format!("目标账号 {} 在本机没有会话库，请先用它登录一次 Trae", dst_client.label));
    }
    if is_running(&dst_client) {
        log(&format!("目标客户端 {} 正在运行，先将其退出…", dst_client.label));
        let killed = kill_all(&dst_client);
        log(&format!("已结束 {} 个进程，等待退出…", killed.len()));
        if !wait_until_stopped(&dst_client, 10_000) {
            return Err("目标客户端未能退出，无法写入数据库".into());
        }
    }
    let enc_key_hex = match load_saved_key(dst_client_key) {
        Some(k) => k,
        None => {
            log("目标账号没有存盘密钥，需要从进程内存扫描（请确保该账号客户端已启动并登录）…");
            let scan = scan_for_key(dst_client_key, &dst_db, None)
                .map_err(|e| format!("扫描目标密钥失败：{e}"))?;
            let k = scan.key.ok_or("未在目标客户端进程中找到有效密钥")?;
            save_key(dst_client_key, &k)?;
            log("已扫描到目标密钥并保存");
            k
        }
    };
    // 用目标库自己的盐验证密钥
    {
        let mut page1 = [0u8; PAGE_SZ];
        use std::io::Read;
        std::fs::File::open(&dst_db)
            .map_err(|e| format!("打开目标库失败: {e}"))?
            .read_exact(&mut page1)
            .map_err(|e| format!("读目标库首页失败: {e}"))?;
        let key = crate::modules::trae_decrypt::hex_to_bytes(&enc_key_hex);
        if !verify_page1_hmac(&key, &page1) {
            return Err("目标库密钥不匹配（密钥过期或目标账号已重新登录），请重新扫描密钥".into());
        }
    }

    // 2) 目标库 → 明文副本（tmp），补 reserved 字段
    //
    // ⚠️ 这里解出的是**原库明文**（orig），要原样保留到第 4 步：增量回写靠它做字节比对，
    // 才能免掉「再解密一遍原密文」这一整轮开销。真正用来改数据的是它的**副本** plain。
    let work = store_dir()
        .join("trae")
        .join("import_tmp")
        .join(format!("{dst_client_key}-{}.db", chrono::Local::now().timestamp()));
    std::fs::create_dir_all(work.parent().unwrap_or(Path::new(".")))
        .map_err(|e| format!("创建工作目录失败: {e}"))?;
    let orig_plain = work.join("target-orig.db");
    let plain = work.join("target-plain.db");
    log("解密目标库为明文副本…");
    let rep = decrypt_database(&dst_db, &enc_key_hex, &orig_plain, Some(&|m| log(&format!("   {m}"))))?;
    log(&format!("目标库 {} 页 / {} 表", rep.pages, rep.tables.len()));
    patch_reserved_field(&orig_plain)?;
    // 复制一份给 SQLite 改（原明文留作比对基准）。纯字节复制，比重新解密便宜一个数量级。
    std::fs::copy(&orig_plain, &plain).map_err(|e| format!("复制明文副本失败: {e}"))?;
    patch_reserved_field(&plain)?;

    // 2b) 合并 WAL 中已提交但未 checkpoint 的帧。
    //
    // ⚠️ 不做这一步，第 4 步回写后会删掉 WAL/SHM ⇒ **客户端刚写下、还没落盘的消息被永久抹掉**。
    //    这正是「复制/导入之后客户端里刚聊的内容凭空消失」的成因：客户端是 WAL 模式，
    //    写入几乎全在 `database.db-wal` 里，主库文件字节往往纹丝不动。
    //    `orig_plain` 保持纯解密（增量回写的逐页比对基准），只有 `plain` 带上 WAL 的新内容。
    merge_live_wal_into(&plain, &dst_db, &enc_key_hex, &|m| log(m))?;

    // 3) 复制会话（先复制无冲突的）
    let dst_conn = Connection::open(&plain).map_err(|e| format!("打开目标明文副本失败: {e}"))?;
    let opts = if same_db {
        CopyOpts::same_db_for(dst_uid)
    } else {
        CopyOpts::cross_db_for(dst_uid)
    };
    let mut copied = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    // (源会话 id, 目标会话 id)：同库复制会生成全新 id，必须由 copy_session 回传，
    // 否则登记关联时会把「源 id」误当成「目标 id」，关联表直接指错。
    let mut sid_pairs: Vec<(String, String)> = Vec::new();
    for sid in &session_ids {
        match copy_session(&src_conn, &dst_conn, sid.as_str(), &opts, Some(&log)) {
            Ok(Some((n, out_sid))) => {
                copied += n;
                sid_pairs.push((sid.clone(), out_sid));
            }
            Ok(None) => skipped.push(sid.clone()),
            Err(e) => return Err(format!("复制会话 {sid} 失败：{e}")),
        }
    }
    // 会话总数在 close 之前取好，供第 6 步轻量自检使用（那时明文副本已关闭）。
    let plain_sessions: i64 = dst_conn
        .query_row("SELECT count(*) FROM chat_session", [], |r| r.get(0))
        .unwrap_or(0);

    // 5b) 目标库自愈：把 `session_project.project_id` 对齐到 `chat_session.project_id`。
    //
    // 为什么要有这一步：早前版本的同库复制**只改 `session_id`、照抄源行的 `project_id`**，
    // 于是历史副本的 `session_project` 仍指向源账号的旧项目。用户在 Trae 客户端里点删除
    // **删不掉**（客户端按项目归属校验），**重启 Trae 后记录复活**（用户实测反馈）。
    // 现版本已修掉复制逻辑，但**已经产生的脏数据还在库里** —— 这里顺手全量对齐一次：
    // 只扫 `session_project`（几十行量级），不是 8 万页，零成本。
    //
    // ⚠️ 只在**同库**导入时做：跨客户端（Trae → WorkBuddy）的 project 语义不同，
    // 不该拿这套规则去改对方的数据。
    if same_db {
        let fixed = dst_conn
            .execute(
                "UPDATE session_project SET project_id = (\
                     SELECT s.project_id FROM chat_session s \
                     WHERE s.session_id = session_project.session_id\
                 ) \
                 WHERE EXISTS (\
                     SELECT 1 FROM chat_session s \
                     WHERE s.session_id = session_project.session_id \
                       AND ifnull(s.project_id, '') <> ifnull(session_project.project_id, '')\
                 )",
                [],
            )
            .unwrap_or(0);
        if fixed > 0 {
            log(&format!(
                "已修正 {fixed} 条工程归属不一致的会话关联（历史副本遗留，修掉后客户端才能正常删除）"
            ));
        }
    }

    dst_conn.close().map_err(|(_, e)| format!("关闭明文副本失败: {e}"))?;
    if copied == 0 && skipped.is_empty() {
        let _ = std::fs::remove_dir_all(&work);
        return Err("没有会话被复制（请检查会话 ID）".into());
    }

    // 4) 增量加密回写 + 自检
    //
    // ⚠️ 性能关键：这里**必须**走增量路径。SQLCipher 逐页独立加密（每页自带 IV + HMAC、
    // 页号参与 HMAC），未变动的页沿用原密文天然合法，所以只重写真正变动的页即可。
    // 早前误用 `encrypt_db_file`（整库重写）⇒ 一次复制要重新加密并写盘 8 万页 / 300+ MB，
    // 用户体感「慢得离谱」（进度条里那串 `加密回写 N/80041 页` 就是它）。
    // 现在改成 `write_db_incremental`：整库密文照抄，只有几百页被重写。
    let new_db = work.join("target-new.db");
    log("增量加密回写（只重写变动页，其余沿用原密文）…");
    let stats = write_db_incremental_with(
        &enc_key_hex,
        &dst_db,
        Some(&orig_plain),
        &plain,
        &new_db,
        Some(&|m| log(&format!("   {m}"))),
    )?;
    log(&format!(
        "增量回写完成：全库 {} 页，仅重写 {} 页（{:.2} MB，追加 {} 页），耗时 {:.2} s",
        stats.pages,
        stats.changed_pages,
        stats.changed_bytes as f64 / 1_048_576.0,
        stats.appended_pages,
        stats.elapsed_ms as f64 / 1000.0
    ));
    let pages = stats.pages;

    // 5) 原子替换（备份原库 + wal/shm → 替换 → 失败回滚）
    let backup_dir = store_dir().join("trae").join("import_backup").join(format!(
        "{dst_client_key}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    std::fs::create_dir_all(&backup_dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let wal = dst_db.with_extension("db-wal");
    let shm = dst_db.with_extension("db-shm");
    for (label, p) in [("主库", &dst_db), ("WAL", &wal), ("SHM", &shm)] {
        if p.exists() {
            let dst = backup_dir.join(p.file_name().unwrap_or_default());
            std::fs::copy(p, &dst).map_err(|e| format!("备份{label}失败: {e}"))?;
        }
    }
    let swap = || -> Result<(), String> {
        if wal.exists() {
            std::fs::remove_file(&wal).map_err(|e| format!("清理 WAL 失败: {e}"))?;
        }
        if shm.exists() {
            std::fs::remove_file(&shm).map_err(|e| format!("清理 SHM 失败: {e}"))?;
        }
        if dst_db.exists() {
            std::fs::remove_file(&dst_db).map_err(|e| format!("移除旧库失败: {e}"))?;
        }
        std::fs::rename(&new_db, &dst_db).map_err(|e| format!("替换数据库失败: {e}"))?;
        Ok(())
    };
    if let Err(e) = swap() {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("替换失败（已保留备份）：{e}"));
    }

    // 6) 轻量自检
    //
    // ⚠️ 早前这里是「把替换后的新库**整库再解密一遍**，只为查一个 count(*)」——
    // 又是 8 万页 / 300+ MB 的白工。而正确性其实已经被三层覆盖：
    //   ① `write_db_incremental` 内**逐页回读解密比对**（写多少验多少）；
    //   ② 其收尾校验首页 HMAC + 输出大小 = 页数 × 4096；
    //   ③ 加密前我们会用明文副本（就是回写的源头）核对会话数。
    // 所以这里只做「新库首页 HMAC + 文件大小」两项廉价核对即可。
    let new_size = std::fs::metadata(&dst_db).map_err(|e| format!("读新库大小失败: {e}"))?.len();
    if new_size != pages * PAGE_SZ as u64 {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!(
            "自检失败：新库大小 {new_size} ≠ {pages} 页 × {PAGE_SZ}（已保留备份）"
        ));
    }
    {
        use std::io::Read;
        let mut h = [0u8; PAGE_SZ];
        let mut f = std::fs::File::open(&dst_db).map_err(|e| format!("打开新库失败: {e}"))?;
        f.read_exact(&mut h).map_err(|e| format!("读新库首页失败: {e}"))?;
        let key = crate::modules::trae_decrypt::hex_to_bytes(&enc_key_hex);
        if !verify_page1_hmac(&key, &h) {
            let _ = std::fs::remove_dir_all(&work);
            return Err("自检失败：新库首页 HMAC 校验不通过（已保留备份）".into());
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    log(&format!(
        "自检通过：新库 {pages} 页 / {:.1} MB，会话 {plain_sessions} 个",
        new_size as f64 / 1_048_576.0
    ));

    // 6b) 只保留最新 2 批导入备份：每批 ≈ 一个整库大小，留着会越攒越多。
    let (pruned, pruned_bytes) =
        crate::modules::trae_export::prune_backups(&crate::modules::trae_export::import_backup_dir(), 2);
    if pruned > 0 {
        log(&format!(
            "已清理 {pruned} 个旧备份，回收 {:.1} MB（仅保留最新 2 批）",
            pruned_bytes as f64 / 1_048_576.0
        ));
    }

    // 6c) 登记跨账号关联：把「源会话 → 目标副本」记进关联表，供「关联」页签展示。
    //
    // 只有**同库跨账号**才有意义：跨客户端（Trae ↔ WorkBuddy）的两条记录分属不同产品线，
    // 会话 id 语义都不同，谈不上「同一段对话的两个副本」。
    // 登记失败**不影响导入结果**（库已经替换成功），只记一条日志——
    // 反过来把已经成功的导入判成失败，会让用户以为白干了。
    let mut linked = 0usize;
    if same_db {
        if let (Some(src_uid), Some(tgt_uid)) = (src_owner_uid, dst_uid) {
            match crate::modules::trae_session_links::register_batch(
                dst_client_key,
                src_uid.as_str(),
                tgt_uid,
                &sid_pairs,
            ) {
                Ok(n) => {
                    linked = n;
                    log(&format!("已登记 {n} 组跨账号关联"));
                }
                Err(e) => log(&format!("导入成功，但登记关联失败：{e}")),
            }
        } else {
            log("导入成功，但源会话归属账号未知，未登记关联");
        }
    }

    let in_list = session_ids
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let src_total: i64 = src_conn
        .query_row(
            &format!("SELECT count(*) FROM chat_session WHERE session_id IN ({in_list})"),
            [],
            |r| r.get(0),
        )
        .map_err(|e| format!("源会话统计失败: {e}"))?;

    // 「已校验会话数」：真正落进目标库的会话数 = 成功返回了目标 id 的会话数。
    // （早前是整库重解密后数一遍，代价是 8 万页白工；现在直接取复制阶段的结果，等价且免费。）
    let verified_sessions = sid_pairs.len();

    // 7) 导入期间结束过目标客户端进程，成功后自动重启，保证导入的会话立即可用
    let relaunched = match launch(dst_client_key, None) {
        Ok(_) => {
            log("导入成功，已自动重启目标客户端");
            true
        }
        Err(e) => {
            log(&format!("导入成功，但自动重启目标客户端失败：{e}"));
            false
        }
    };

    Ok(json!({
        "copied_rows": copied,
        "sessions_requested": session_ids.len(),
        "sessions_src": src_total,
        "skipped": skipped,
        "target_client": dst_client_key,
        "target_label": dst_client.label,
        "target_uid": dst_uid,
        "same_db": same_db,
        "linked": linked,
        "pages": pages,
        // 轻量自检后不再整库重解密，这里的「已校验会话数」= 目标库中该批源会话
        // 对应的副本在明文副本里确实存在的个数（即真正落库成功的数量）。
        "verified_sessions": verified_sessions,
        "relaunched": relaunched,
        "backup_dir": backup_dir.to_string_lossy(),
        "pruned_backups": pruned,
    }))
}

/// 「同步差异」：把关联组里较新一端的会话内容，覆盖到较旧一端。
///
/// ## 为什么不是「再复制一次」
///
/// 普通复制（[`import_sessions`]）永远**新建一个会话 id**，于是每次同步都会多出一条记录，
/// 关联组里堆出一串副本。而「同步差异」的语义是**就地更新**：把陈旧那份的正文换成新的，
/// **会话 id 保持不变**，于是它在 Trae 客户端里的位置、归属、关联登记全都不用动。
///
/// 实现 = **删掉旧的那条的全部行 + 以旧 id 重新写入新的内容**。两步放在**同一次**
/// 解密 / 回写里完成，所以不会出现「删掉了但没写上」的中间态。
///
/// ## 批量入口
///
/// [`sync_group`] 是单组便捷封装；[`sync_groups`] 一次收多个组。批量版共享**同一个**
/// 写库周期（一次退客户端 / 一次备份 / 一次重启），「切号后把这几组差异一起同步」
/// 不至于把客户端重启 N 次。
///
/// ## 安全约束（与删除 / 导入严格一致）
/// 结束客户端 → 整份备份 → 写 → 校验 → 重启。任一步失败都中止，**不会留下半成品**。
/// 多组时**先把所有组校验完**再动写，任何一组不合规则整体放弃 ——
/// 绝不做「前两组成功、第三组失败」的局部产物。
///
/// `group_id`：关联组 id；`direction`：`"sourceToTarget"` 或 `"targetToSource"`，
/// 表示**把哪一端当作最新**去覆盖另一端。
pub fn sync_group(
    client_key: &str,
    group_id: &str,
    direction: &str,
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let r = sync_groups_inner(
        client_key,
        &[(group_id.to_string(), direction.to_string())],
        on_log,
    )?;
    let g = &r.outcomes[0];
    Ok(json!({
        "ok": true,
        "groupId": g.group_id,
        "direction": g.direction,
        "keptSid": g.kept_sid,
        "fromSid": g.from_sid,
        "removedRows": g.removed_rows,
        "writtenRows": g.written_rows,
        "pages": r.pages,
        "relaunched": r.relaunched,
        "backupDir": r.backup_dir.to_string_lossy(),
        "note": "已在原会话 id 上就地更新，客户端里的位置与归属不变",
    }))
}

/// **批量**同步：`requests` 每项为 `(group_id, direction)`，`direction` 同 [`sync_group`]。
///
/// 所有组合用一个写库周期（一次退客户端 / 一次备份 / 一次重启），返回 `groups[]` 逐组结果。
pub fn sync_groups(
    client_key: &str,
    requests: &[(String, String)],
    on_log: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let r = sync_groups_inner(client_key, requests, on_log)?;
    Ok(json!({
        "ok": true,
        "count": r.outcomes.len(),
        "groups": r.outcomes.iter().map(|g| json!({
            "groupId": g.group_id,
            "direction": g.direction,
            "keptSid": g.kept_sid,
            "fromSid": g.from_sid,
            "removedRows": g.removed_rows,
            "writtenRows": g.written_rows,
        })).collect::<Vec<Value>>(),
        "pages": r.pages,
        "relaunched": r.relaunched,
        "backupDir": r.backup_dir.to_string_lossy(),
    }))
}

/// 单组的同步计划：把 `from_sid` 的内容，写进保留 `kept_sid`（id 与归属都不动）。
#[derive(Debug, Clone)]
struct SyncPlan {
    group_id: String,
    direction: String,
    from_sid: String,
    kept_uid: String,
    kept_sid: String,
}

#[derive(Debug, Clone)]
struct SyncOutcome {
    group_id: String,
    direction: String,
    kept_sid: String,
    from_sid: String,
    removed_rows: usize,
    written_rows: usize,
}

struct BatchSyncResult {
    outcomes: Vec<SyncOutcome>,
    /// 库页数（`write_db_incremental_with` 统计口径）。
    pages: u64,
    relaunched: bool,
    backup_dir: PathBuf,
}

/// 按同步方向，把关联组的两端映射成「内容来源（from）/ 被覆盖（kept）」两方。
///
/// 返回 `(from_uid, from_sid, kept_uid, kept_sid)`：
/// `sourceToTarget` = 源的内容写进目标；`targetToSource` = 反之。
///
/// ⚠️ **保持单一出口**：归属校验（`kept` 端按 `kept_uid` 比对）与写库
/// （`CopyOpts.dst_uid` 取 `kept_uid`）**都必须**从这里取值 ——
/// 两边各写一套就会把 uid 配错，而这正是跨账号同步曾经 100% 失败的原因。
fn plan_roles(
    src_uid: String,
    src_sid: String,
    tgt_uid: String,
    tgt_sid: String,
    direction: &str,
) -> Result<(String, String, String, String), String> {
    match direction {
        "sourceToTarget" => Ok((src_uid, src_sid, tgt_uid, tgt_sid)),
        "targetToSource" => Ok((tgt_uid, tgt_sid, src_uid, src_sid)),
        other => Err(format!("未知同步方向: {other}")),
    }
}

fn sync_groups_inner(
    client_key: &str,
    requests: &[(String, String)],
    on_log: Option<&dyn Fn(&str)>,
) -> Result<BatchSyncResult, String> {
    let log = |m: &str| {
        if let Some(cb) = on_log {
            cb(m);
        }
    };
    if requests.is_empty() {
        return Err("没有指定要同步的关联组".into());
    }
    let Some(client) = get_client(client_key) else {
        return Err(format!("未知客户端: {client_key}"));
    };

    // 归属校验只读一次会话库 —— 别每组调两次 `session_owner_uid`，
    // 那会按组数重复整库解密（N 组 ⇒ 2N 次全量解密）。
    let probe = open_decrypted(client_key).map_err(|e| format!("打开会话库失败: {e}"))?;
    let owner_of = |sid: &str| -> Option<String> {
        let v: rusqlite::types::Value = probe
            .query_row(
                "SELECT p.user_id FROM chat_session s JOIN project p ON s.project_id = p.project_id \
                 WHERE s.session_id = ?",
                params![sid],
                |r| r.get(0),
            )
            .ok()?;
        match v {
            rusqlite::types::Value::Text(s) => Some(s),
            rusqlite::types::Value::Integer(i) => Some(i.to_string()),
            _ => None,
        }
    };

    let mut plans: Vec<SyncPlan> = Vec::with_capacity(requests.len());
    for (group_id, direction) in requests {
        if plans.iter().any(|p| p.group_id == *group_id) {
            return Err(format!("关联组 {group_id} 重复出现在本次同步里"));
        }
        // 1) 定位关联组的两端成员
        let links = crate::modules::trae_session_links::group_members(client_key, group_id)?
            .ok_or_else(|| format!("没有找到关联组 {group_id}"))?;
        let (src_uid, src_sid) = links
            .iter()
            .find(|m| m.role == "source")
            .map(|m| (m.uid.clone(), m.session_id.clone()))
            .ok_or("关联组缺少源成员")?;
        let (tgt_uid, tgt_sid) = links
            .iter()
            .find(|m| m.role == "target")
            .map(|m| (m.uid.clone(), m.session_id.clone()))
            .ok_or("关联组缺少目标成员")?;

        // 「新的一端」= 保留它的内容；「旧的一端」= 被覆盖，但**占据会话 id 与账号归属**。
        let (from_uid, from_sid, kept_uid, kept_sid) =
            plan_roles(src_uid, src_sid, tgt_uid, tgt_sid, direction)?;
        if from_sid == kept_sid {
            return Err(format!("关联组 {group_id}：两端是同一条会话，无需同步"));
        }
        //
        // ⚠️ 归属校验：**from 端按 from_uid 校验，kept 端按 kept_uid 校验**。
        //    这两个 uid 随 `direction` 互换 —— 一旦按 role 写死成「校验 src_uid」，
        //    跨账号（两端 uid 必然不同）的**每一次**同步都会被这一句判死。
        //    历史 bug：正是这个分支写反了，导致同步 100% 报「会话已不在预期账号下」。
        if owner_of(&from_sid).as_deref() != Some(from_uid.as_str()) {
            return Err(format!(
                "关联组 {group_id}：较新一端的会话已不在预期账号下，请先刷新关联状态"
            ));
        }
        if owner_of(&kept_sid).as_deref() != Some(kept_uid.as_str()) {
            return Err(format!(
                "关联组 {group_id}：被覆盖的会话已不在预期账号下，请先刷新关联状态"
            ));
        }
        log(&format!(
            "同步方向：{}（较新）→ {}（覆盖）",
            uid_suffix(&from_uid),
            uid_suffix(&kept_uid)
        ));
        plans.push(SyncPlan {
            group_id: group_id.clone(),
            direction: direction.clone(),
            from_sid,
            kept_uid,
            kept_sid,
        });
    }
    // 同一批里两组若「互为来源 / 目标」，串行执行会让后一组读到前一组的写入结果。
    for p in &plans {
        if plans.iter().any(|q| q.group_id != p.group_id && q.from_sid == p.kept_sid) {
            return Err(format!(
                "关联组 {} 的目标会话同时是另一个组的拷贝源，无法在同一次同步里处理",
                p.group_id
            ));
        }
    }
    drop(probe);

    let dst_db = database_path(&client);
    if !dst_db.is_file() {
        return Err(format!("{} 在本机没有会话库", client.label));
    }

    // 2) 结束客户端（写库前提）
    if is_running(&client) {
        log(&format!("{} 正在运行，先将其退出…", client.label));
        let killed = kill_all(&client);
        log(&format!("已结束 {} 个进程，等待退出…", killed.len()));
        if !wait_until_stopped(&client, 20_000) {
            return Err("客户端未能退出，无法写入数据库".into());
        }
    }

    // 3) 密钥
    let enc_key_hex = match load_saved_key(client_key) {
        Some(k) => k,
        None => {
            log("没有存盘密钥，从进程内存扫描…");
            let scan = scan_for_key(client_key, &dst_db, None)
                .map_err(|e| format!("扫描密钥失败：{e}"))?;
            let k = scan.key.ok_or("未在客户端进程中找到有效密钥")?;
            save_key(client_key, &k)?;
            k
        }
    };
    {
        use std::io::Read;
        let mut page1 = [0u8; PAGE_SZ];
        std::fs::File::open(&dst_db)
            .map_err(|e| format!("打开库失败: {e}"))?
            .read_exact(&mut page1)
            .map_err(|e| format!("读首页失败: {e}"))?;
        let key = crate::modules::trae_decrypt::hex_to_bytes(&enc_key_hex);
        if !verify_page1_hmac(&key, &page1) {
            return Err("密钥不匹配（可能已重新登录），请重新扫描密钥".into());
        }
    }

    // 4) 解密目标库 → 明文副本（orig 留作增量比对基准）
    let work = store_dir()
        .join("trae")
        .join("import_tmp")
        .join(format!("{client_key}-sync-{}.db", chrono::Local::now().timestamp()));
    std::fs::create_dir_all(work.parent().unwrap_or(Path::new(".")))
        .map_err(|e| format!("创建工作目录失败: {e}"))?;
    let orig_plain = work.join("target-orig.db");
    let plain = work.join("target-plain.db");
    log("解密为明文副本…");
    let rep = decrypt_database(&dst_db, &enc_key_hex, &orig_plain, Some(&|m| log(&format!("   {m}"))))?;
    log(&format!("{} 页 / {} 表", rep.pages, rep.tables.len()));
    patch_reserved_field(&orig_plain)?;
    std::fs::copy(&orig_plain, &plain).map_err(|e| format!("复制明文副本失败: {e}"))?;
    patch_reserved_field(&plain)?;

    // 4b) 合并 WAL 中**已提交但未 checkpoint** 的帧。
    //
    // ⚠️ 不做这一步就一定会丢数据：同步随后会删掉 WAL/SHM，而客户端是 WAL 模式，
    //    刚写下的最新消息很可能还只在 WAL 里 ⇒ 只解密主库等于拿到一份陈旧内容，
    //    再删 WAL 就把它们永久抹掉。删除路径（`delete_sessions_core` 第 3 步）早就有
    //    这一步，同步路径此前漏了。
    // ⚠️ `orig_plain` **保持纯解密、不合并** —— 它是增量回写的逐页比对基准，必须与
    //    加密库的真实内容一一对应；只有工作副本 `plain` 带上 WAL 的新内容，
    //    这些页才会被判为「已变动」而重写进新库。
    {
        let wal = dst_db.with_extension("db-wal");
        let merged = crate::modules::trae_delete::merge_wal_into_plain(&plain, &wal, &enc_key_hex)
            .map_err(|e| format!("合并 WAL 失败: {e}"))?;
        if merged > 0 {
            log(&format!("已合并 WAL 中 {merged} 个已提交页面帧（防止丢失未落盘的最新改动）"));
            // 合并可能覆盖首页，reserved 字段要重新补一次。
            patch_reserved_field(&plain)?;
        }
    }

    // 5) 就地更新：每组删掉陈旧那条的全部行，再以**同一个 id** 写入新内容。
    //
    // ⚠️ 两步必须在同一个连接 / 同一次回写里完成，否则中途失败会留下
    // 「旧的已删、新的没写」的空洞会话。
    // ⚠️ `dst_uid` 取 kept 一侧（已在计划里随 direction 算好）—— 覆盖后的会话归属
    // **跟随被覆盖的那条**，同步只换内容，不该改变会话在账号间的归属。
    let conn = Connection::open(&plain).map_err(|e| format!("打开明文副本失败: {e}"))?;
    let mut outcomes: Vec<SyncOutcome> = Vec::with_capacity(plans.len());
    for plan in &plans {
        let removed = crate::modules::trae_delete::delete_rows_in_conn(&conn, &plan.kept_sid)?;
        log(&format!("已清除陈旧副本 {} 的 {removed} 行", plan.kept_sid));
        let opts = CopyOpts {
            dst_uid: Some(plan.kept_uid.clone()),
            same_db: true,
            // 固定 id ⇒ 写回来的还是原来那条，客户端里的位置不变。
            new_sid: Some(plan.kept_sid.clone()),
        };
        let written = match copy_session(&conn, &conn, &plan.from_sid, &opts, Some(&log)) {
            Ok(Some((n, out_sid))) => {
                debug_assert_eq!(out_sid, plan.kept_sid);
                n
            }
            Ok(None) => {
                return Err(format!("关联组 {}：写入失败：目标 id 仍被占用（清除不彻底）", plan.group_id))
            }
            Err(e) => return Err(format!("关联组 {}：写入新内容失败：{e}", plan.group_id)),
        };
        log(&format!("已写入 {written} 行（会话 id 保持 {}）", plan.kept_sid));
        outcomes.push(SyncOutcome {
            group_id: plan.group_id.clone(),
            direction: plan.direction.clone(),
            kept_sid: plan.kept_sid.clone(),
            from_sid: plan.from_sid.clone(),
            removed_rows: removed,
            written_rows: written,
        });
    }

    // 顺手对齐工程归属（T29 的历史脏数据规则，同样适用于就地重建的这条）。
    let _ = conn.execute(
        "UPDATE session_project SET project_id = (\
             SELECT s.project_id FROM chat_session s \
             WHERE s.session_id = session_project.session_id\
         ) \
         WHERE EXISTS (\
             SELECT 1 FROM chat_session s \
             WHERE s.session_id = session_project.session_id \
               AND ifnull(s.project_id, '') <> ifnull(session_project.project_id, '')\
         )",
        [],
    );
    let plain_sessions: i64 = conn
        .query_row("SELECT count(*) FROM chat_session", [], |r| r.get(0))
        .unwrap_or(0);
    conn.close().map_err(|(_, e)| format!("关闭明文副本失败: {e}"))?;

    // 6) 增量加密回写
    let new_db = work.join("target-new.db");
    log("增量加密回写（只重写变动页）…");
    let stats = write_db_incremental_with(
        &enc_key_hex,
        &dst_db,
        Some(&orig_plain),
        &plain,
        &new_db,
        Some(&|m| log(&format!("   {m}"))),
    )?;
    log(&format!(
        "回写完成：{} 页，重写 {} 页，耗时 {:.2} s",
        stats.pages,
        stats.changed_pages,
        stats.elapsed_ms as f64 / 1000.0
    ));
    let pages = stats.pages;

    // 7) 备份 + 原子替换（失败回滚）
    let backup_dir = store_dir()
        .join("trae")
        .join("import_backup")
        .join(format!("{client_key}-sync-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
    std::fs::create_dir_all(&backup_dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    let wal = dst_db.with_extension("db-wal");
    let shm = dst_db.with_extension("db-shm");
    for (label, p) in [("主库", &dst_db), ("WAL", &wal), ("SHM", &shm)] {
        if p.exists() {
            let dst = backup_dir.join(p.file_name().unwrap_or_default());
            std::fs::copy(p, &dst).map_err(|e| format!("备份{label}失败: {e}"))?;
        }
    }
    let swap = || -> Result<(), String> {
        if wal.exists() {
            std::fs::remove_file(&wal).map_err(|e| format!("清理 WAL 失败: {e}"))?;
        }
        if shm.exists() {
            std::fs::remove_file(&shm).map_err(|e| format!("清理 SHM 失败: {e}"))?;
        }
        if dst_db.exists() {
            std::fs::remove_file(&dst_db).map_err(|e| format!("移除旧库失败: {e}"))?;
        }
        std::fs::rename(&new_db, &dst_db).map_err(|e| format!("替换数据库失败: {e}"))?;
        Ok(())
    };
    if let Err(e) = swap() {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("替换失败（已保留备份）：{e}"));
    }

    // 8) 轻量自检
    let new_size = std::fs::metadata(&dst_db).map_err(|e| format!("读新库大小失败: {e}"))?.len();
    if new_size != pages * PAGE_SZ as u64 {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!("自检失败：新库大小 {new_size} ≠ {pages} 页 × {PAGE_SZ}（已保留备份）"));
    }
    {
        use std::io::Read;
        let mut h = [0u8; PAGE_SZ];
        std::fs::File::open(&dst_db)
            .map_err(|e| format!("打开新库失败: {e}"))?
            .read_exact(&mut h)
            .map_err(|e| format!("读新库首页失败: {e}"))?;
        let key = crate::modules::trae_decrypt::hex_to_bytes(&enc_key_hex);
        if !verify_page1_hmac(&key, &h) {
            let _ = std::fs::remove_dir_all(&work);
            return Err("自检失败：新库首页 HMAC 不通过（已保留备份）".into());
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    log(&format!("自检通过：新库 {pages} 页，会话 {plain_sessions} 个"));

    let (pruned, pruned_bytes) =
        crate::modules::trae_export::prune_backups(&crate::modules::trae_export::import_backup_dir(), 2);
    if pruned > 0 {
        log(&format!("已清理 {pruned} 个旧备份，回收 {:.1} MB", pruned_bytes as f64 / 1_048_576.0));
    }

    // 9) 重启客户端
    let relaunched = match launch(client_key, None) {
        Ok(_) => {
            log(&format!(
                "同步完成（{} 个关联组），已自动重启客户端",
                outcomes.len()
            ));
            true
        }
        Err(e) => {
            log(&format!(
                "同步完成（{} 个关联组），但自动重启客户端失败：{e}",
                outcomes.len()
            ));
            false
        }
    };

    Ok(BatchSyncResult {
        outcomes,
        pages,
        relaunched,
        backup_dir,
    })
}

/// 备份目录清理（导入前旧临时目录）。
pub fn clean_stale_import_dirs() -> Result<(), String> {
    let dir = store_dir().join("trae").join("import_tmp");
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("清理导入临时目录失败: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把源码按「顶层函数」切段（用于静态护栏扫描）。
    ///
    /// ⚠️ **必须剔掉注释行**：否则把调用注释掉就能骗过护栏
    /// （`// merge_live_wal_into(...)` 仍含子串）—— 这一点已被负向测试坐实过。
    fn split_fn_bodies(src: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut cur: Option<String> = None;
        for line in src.lines() {
            let starts_fn = ["pub fn ", "fn ", "pub async fn ", "async fn "]
                .iter()
                .any(|p| line.starts_with(p));
            if starts_fn {
                if let Some(b) = cur.take() {
                    out.push(b);
                }
                cur = Some(String::new());
            }
            let trimmed = line.trim_start();
            let is_comment = trimmed.starts_with("//");
            if let Some(b) = cur.as_mut() {
                if !is_comment {
                    b.push_str(line);
                    b.push('\n');
                }
            }
        }
        if let Some(b) = cur {
            out.push(b);
        }
        out
    }

    /// ⚠️ **结构护栏：任何会删掉实时库 WAL 的写路径，必须先合并 WAL。**
    ///
    /// 客户端是 WAL 模式，写入几乎全在 `database.db-wal` 里（主库文件字节往往纹丝不动）。
    /// 一条写路径若「只解密主库 → 改明文 → 回写 → 删 WAL」，就会把客户端**刚写下、还没落盘**
    /// 的内容永久抹掉 —— 表现是「复制/同步之后，客户端里刚聊的那几轮凭空消失」。
    ///
    /// 删除路径（`delete_sessions_core`）与同步路径（`sync_groups_inner`）一直有这一步；
    /// **复制（`import_sessions`）与自愈（`heal_session_projects`）曾经漏掉**（T32 补）。
    /// 本用例按函数体扫描，新增写路径若忘了合并会直接失败。
    #[test]
    fn every_wal_removal_site_merges_wal_first() {
        let files: [(&str, &str); 3] = [
            ("trae_import.rs", include_str!("trae_import.rs")),
            ("trae_delete.rs", include_str!("trae_delete.rs")),
            ("workbuddy_import.rs", include_str!("workbuddy_import.rs")),
        ];
        let merge_calls = ["merge_wal_into_plain(", "merge_live_wal_into("];
        let mut checked = 0usize;
        for (name, src) in files {
            for body in split_fn_bodies(src) {
                if !body.contains("remove_file(&wal)") {
                    continue;
                }
                checked += 1;
                let head: Vec<&str> = body.lines().take(1).collect();
                assert!(
                    merge_calls.iter().any(|c| body.contains(c)),
                    "⚠️ {name} 的 `{}` 删除了实时库 WAL，却没有先合并 WAL \
                     ⇒ 会永久丢掉客户端未 checkpoint 的写入。\
                     请在解密明文后、改动前调用 `merge_wal_into_plain` / `merge_live_wal_into`。",
                    head.first().unwrap_or(&"<未知函数>")
                );
            }
        }
        assert!(
            checked >= 5,
            "护栏失效：只扫到 {checked} 处删 WAL 的写路径，预期至少 5 处（删除/自愈/复制/同步/WorkBuddy 导入）"
        );
    }

    /// 页级加密 ↔ 解密 round-trip：
    /// 合成 SQLCipher 明文页（尾部保留区清零）→ 加密 → 首页 HMAC 自检通过
    /// （页号按 4 字节参与 HMAC，与真实 SQLCipher 一致）→ 解密回字节级一致。
    #[test]
    fn roundtrip_sqlcipher_page() {
        let enc_key = {
            let mut r = [0u8; KEY_SZ];
            getrandom::getrandom(&mut r).unwrap();
            r
        };
        let salt = {
            let mut r = [0u8; SALT_SZ];
            getrandom::getrandom(&mut r).unwrap();
            r
        };
        let mac_key = derive_mac_key(&enc_key, &salt);
        for pgno in [1u64, 2, 5, 100] {
            // 合成明文页：伪随机内容，尾部保留区清零（SQLCipher 明文页特征）
            let mut plain = [0u8; PAGE_SZ];
            for (i, b) in plain.iter_mut().enumerate() {
                *b = ((i as u64) % 251) as u8;
            }
            plain[USABLE_SZ..].fill(0);
            let enc = encrypt_sqlcipher_page(&enc_key, &mac_key, salt, pgno, &plain).unwrap();
            // 首页 HMAC 必须通过（u32 页号约定）
            if pgno == 1 {
                assert!(verify_page1_hmac(&enc_key, &enc), "页 1 HMAC 自检失败");
            }
            let dec = crate::modules::trae_decrypt::decrypt_page(&enc_key, &enc, pgno);
            if pgno == 1 {
                // 页 1 前 16 字节以标准 SQLite 头重建，其余应逐字节还原
                assert_eq!(&dec[..16], b"SQLite format 3\x00");
                assert_eq!(&dec[16..], &plain[16..], "页 {pgno} round-trip 内容不一致");
            } else {
                assert_eq!(&dec[..USABLE_SZ], &plain[..USABLE_SZ], "页 {pgno} round-trip 内容不一致");
            }
        }
    }

    // -----------------------------------------------------------------------
    // 并行解密 / 增量回写
    // -----------------------------------------------------------------------

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("wb_{tag}_{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn random_key_hex() -> String {
        let mut r = [0u8; KEY_SZ];
        getrandom::getrandom(&mut r).unwrap();
        r.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 合成一个「像 SQLCipher 明文页」的页：内容与页号相关，尾部保留区清零。
    fn synth_page(pgno: u64, seed: u64) -> [u8; PAGE_SZ] {
        let mut p = [0u8; PAGE_SZ];
        for (i, b) in p.iter_mut().enumerate() {
            *b = (pgno
                .wrapping_mul(1_000_003)
                .wrapping_add(i as u64)
                .wrapping_mul(2_654_435_761)
                .wrapping_add(seed)) as u8;
        }
        p[USABLE_SZ..].fill(0);
        p
    }

    /// 合成多页明文库（页 1 带标准 SQLite 头）。
    fn synth_plain(pages: u64, seed: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(pages as usize * PAGE_SZ);
        for pgno in 1..=pages {
            out.extend_from_slice(&synth_page(pgno, seed));
        }
        out[..16].copy_from_slice(b"SQLite format 3\x00");
        out
    }

    #[test]
    fn decrypt_database_parallel_restores_every_page() {
        // 24 页 > 并行度上限（8），分块边界与块内连续偏移都被覆盖
        let dir = tmp_dir("dec_mt");
        let plain = synth_plain(24, 7);
        let p_plain = dir.join("plain.db");
        let p_enc = dir.join("enc.db");
        let p_out = dir.join("out.db");
        std::fs::write(&p_plain, &plain).unwrap();

        let key = random_key_hex();
        encrypt_db_file(&key, &p_plain, &p_enc, None).unwrap();
        let rep =
            crate::modules::trae_decrypt::decrypt_database(&p_enc, &key, &p_out, None).unwrap();
        assert_eq!(rep.pages, 24);
        assert_eq!(
            std::fs::read(&p_out).unwrap(),
            plain,
            "并行解密必须与原明文逐字节一致（分块不能错位）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -----------------------------------------------------------------------
    // 同库多会话复制：每个会话必须各自生成新 id
    // -----------------------------------------------------------------------

    /// 建一个最小的真实 Trae 结构库（chat_session + project + chat_message）。
    ///
    /// `copy_session` 会按「源/目标列交集」逐表复制，交集为空就跳过，
    /// 所以只有它**无条件查询**的表必须建出来：`chat_session`、`chat_message`。
    /// 其余（chat_turn / agent_run / history_v2 / …）不建也不会报错。
    fn make_trae_db(path: &std::path::Path, sessions: &[(&str, &str, &str)]) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE project (
                 project_id TEXT PRIMARY KEY,
                 user_id TEXT,
                 absolute_path TEXT
             );
             CREATE TABLE chat_session (
                 session_id TEXT PRIMARY KEY,
                 session_title TEXT,
                 project_id TEXT,
                 created_at TEXT,
                 updated_at TEXT,
                 last_unread_turn_id TEXT
             );
             CREATE TABLE chat_message (
                 message_id TEXT PRIMARY KEY,
                 session_id TEXT,
                 turn_id TEXT,
                 role TEXT,
                 content TEXT,
                 reply_to_message_id TEXT,
                 response_message_id TEXT
             );
             CREATE TABLE chat_message_general (
                 message_id TEXT PRIMARY KEY,
                 session_id TEXT,
                 turn_id TEXT,
                 role TEXT,
                 content TEXT
             );
             CREATE TABLE chat_message_task (
                 message_id TEXT PRIMARY KEY,
                 session_id TEXT,
                 turn_id TEXT,
                 role TEXT,
                 content TEXT
             );
             CREATE TABLE chat_turn (
                 turn_id TEXT PRIMARY KEY,
                 session_id TEXT,
                 conversation_id TEXT,
                 created_at TEXT
             );
             CREATE TABLE session_project (
                 session_id TEXT,
                 project_id TEXT
             );
             CREATE TABLE agent_run (
                 agent_run_id TEXT PRIMARY KEY,
                 session_id TEXT,
                 parent_run_id TEXT,
                 turn_id TEXT,
                 status TEXT
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO project (project_id, user_id, absolute_path) VALUES ('p1','u1','/x')",
            [],
        )
        .unwrap();
        for (sid, title, updated) in sessions {
            conn.execute(
                "INSERT INTO chat_session (session_id, session_title, project_id, created_at, updated_at)
                 VALUES (?1, ?2, 'p1', ?3, ?3)",
                rusqlite::params![sid, title, updated],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chat_message (message_id, session_id, turn_id, role, content)
                 VALUES (?1, ?2, 't1', 'user', 'hello')",
                rusqlite::params![format!("{sid}-m1"), sid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chat_message_general (message_id, session_id, turn_id, role, content)
                 VALUES (?1, ?2, 't1', 'user', 'hello')",
                rusqlite::params![format!("{sid}-g1"), sid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chat_message_task (message_id, session_id, turn_id, role, content)
                 VALUES (?1, ?2, 't1', 'user', 'hello')",
                rusqlite::params![format!("{sid}-k1"), sid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chat_turn (turn_id, session_id, conversation_id, created_at)
                 VALUES (?1, ?2, 'c1', ?3)",
                rusqlite::params![format!("{sid}-t1"), sid, updated],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_project (session_id, project_id) VALUES (?1, 'p1')",
                rusqlite::params![sid],
            )
            .unwrap();
        }
        conn.close().unwrap();
    }

    /// ⚠️ 回归护栏：同库复制**多个**会话时，每个会话都必须拿到各自的新 id。
    ///
    /// 曾经的缺陷：`new_sid` 在循环外 `native_hex_id()` 只生成一次、整批共用，
    /// 于是第二个会话插入时撞上第一个刚写入的同一个 id，冲突检查判定「已存在」，
    /// 返回 `Ok(None)` 被上层当成「跳过」→ **多选复制只有第一个成功，其余静默丢失**。
    #[test]
    fn same_db_multi_copy_generates_distinct_ids_per_session() {
        let dir = tmp_dir("same_db_multi");
        let src_db = dir.join("src.db");
        let dst_db = dir.join("dst.db");
        make_trae_db(
            &src_db,
            &[
                ("s1", "first", "2026-10-01 10:00:00"),
                ("s2", "second", "2026-10-02 10:00:00"),
                ("s3", "third", "2026-10-03 10:00:00"),
            ],
        );
        // 目标库先只有 project，没有会话
        make_trae_db(&dst_db, &[]);

        let src = Connection::open(&src_db).unwrap();
        let dst = Connection::open(&dst_db).unwrap();
        let opts = CopyOpts::same_db_for(Some("u2"));

        let mut out_ids: Vec<String> = Vec::new();
        for sid in ["s1", "s2", "s3"] {
            match copy_session(&src, &dst, sid, &opts, None) {
                Ok(Some((_n, out_sid))) => out_ids.push(out_sid),
                Ok(None) => panic!("会话 {sid} 被当成「已存在」跳过了——这正是那个 id 共用的 bug"),
                Err(e) => panic!("复制会话 {sid} 失败：{e}"),
            }
        }

        assert_eq!(out_ids.len(), 3, "三个会话都应当被复制");
        let uniq: std::collections::HashSet<&String> = out_ids.iter().collect();
        assert_eq!(uniq.len(), 3, "三个会话的目标 id 必须两两不同，实际为 {out_ids:?}");
        for id in &out_ids {
            assert_ne!(id.as_str(), "s1");
            assert_ne!(id.as_str(), "s2");
            assert_ne!(id.as_str(), "s3");
            assert_eq!(id.len(), 24, "新 id 应是 24 位 hex（native_hex_id 的格式）");
        }

        // 目标库里必须真的有 3 条会话
        let n: i64 = dst
            .query_row("SELECT count(*) FROM chat_session", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3, "目标库中应新增 3 条会话");

        drop(src);
        drop(dst);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 跨库复制必须**保留**原 id（不同库之间唯一约束不冲突，且关联表要能对上）。
    #[test]
    fn cross_db_copy_keeps_original_ids() {
        let dir = tmp_dir("cross_db");
        let src_db = dir.join("src.db");
        let dst_db = dir.join("dst.db");
        make_trae_db(&src_db, &[("s1", "one", "2026-10-01 10:00:00")]);
        make_trae_db(&dst_db, &[]);

        let src = Connection::open(&src_db).unwrap();
        let dst = Connection::open(&dst_db).unwrap();
        let opts = CopyOpts::cross_db_for(None);
        match copy_session(&src, &dst, "s1", &opts, None) {
            Ok(Some((_n, out_sid))) => assert_eq!(out_sid, "s1", "跨库复制应保留原 id"),
            other => panic!("跨库复制应成功，实际 {:?}", other.map(|o| o.map(|(n, s)| (n, s)))),
        }
        drop(src);
        drop(dst);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同库复制时，重复复制同一个会话应被判定为「已存在」并跳过（不是静默覆盖）。
    /// 注意：这里要模拟「目标库里已经有源会话 s1 本身」的情况。
    #[test]
    fn same_db_copy_of_present_id_is_skipped() {
        let dir = tmp_dir("same_db_skip");
        let src_db = dir.join("src.db");
        let dst_db = dir.join("dst.db");
        make_trae_db(&src_db, &[("s1", "one", "2026-10-01 10:00:00")]);
        // 目标库已经有 s1（模拟源目标本来同库）
        make_trae_db(&dst_db, &[("s1", "one", "2026-10-01 10:00:00")]);

        let src = Connection::open(&src_db).unwrap();
        let dst = Connection::open(&dst_db).unwrap();
        let opts = CopyOpts::same_db_for(Some("u2"));
        // 同库复制会生成新 id，所以即便 s1 已存在也不应撞车 → 应当成功
        let r = copy_session(&src, &dst, "s1", &opts, None);
        assert!(
            matches!(r, Ok(Some(_))),
            "同库复制生成新 id，不该命中已存在的 s1"
        );
        drop(src);
        drop(dst);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⚠️ 回归护栏：同库复制后，`session_project.project_id` 必须与
    /// `chat_session.project_id` **一致**（都指向目标账号的项目）。
    ///
    /// 曾经的缺陷：`session_project` 复制时**只改 `session_id`、照抄源行的 `project_id`**，
    /// 而 `chat_session.project_id` 已改成目标项目 ⇒ 副本挂在**源账号的旧项目**下。
    /// 后果是用户在 Trae 客户端里点删除**删不掉**（按当前项目归属校验），
    /// **重启 Trae 后记录原地复活**（用户实测反馈）。
    ///
    /// 同时锁定 `context.last_real_project_id` 也要跟着改写。
    #[test]
    fn same_db_copy_remaps_session_project_and_context_project_id() {
        let dir = tmp_dir("copy_proj_remap");
        let src_db = dir.join("src.db");
        let dst_db = dir.join("dst.db");
        make_trae_db(&src_db, &[("s1", "wenku", "2026-10-05 15:20:43")]);
        make_trae_db(&dst_db, &[]);

        // 源会话带上 context（含 last_real_project_id），并给两库补 context 列
        for db in [&src_db, &dst_db] {
            let c = Connection::open(db).unwrap();
            if c.prepare("SELECT context FROM chat_session").is_err() {
                c.execute("ALTER TABLE chat_session ADD COLUMN context TEXT", [])
                    .unwrap();
            }
        }
        {
            let c = Connection::open(&src_db).unwrap();
            c.execute(
                "UPDATE chat_session SET context=?1 WHERE session_id='s1'",
                rusqlite::params![r#"{"has_remote_counterpart":false,"last_real_project_id":"p1","vm_mode":"aha_vm"}"#],
            )
            .unwrap();
        }

        let src = Connection::open(&src_db).unwrap();
        let dst = Connection::open(&dst_db).unwrap();
        let opts = CopyOpts::same_db_for(Some("u2"));
        let (_n, out_sid) =
            copy_session(&src, &dst, "s1", &opts, None).unwrap().expect("应当复制成功");

        // 目标会话的 project_id
        let db_pid: String = dst
            .query_row(
                "SELECT project_id FROM chat_session WHERE session_id=?1",
                rusqlite::params![out_sid],
                |r| r.get(0),
            )
            .unwrap();
        assert_ne!(db_pid, "p1", "副本必须挂到目标账号的新项目，而不是源项目 p1");

        // 关联表里的 project_id 必须与 chat_session 一致（本次修复的核心）
        let sp_pid: String = dst
            .query_row(
                "SELECT project_id FROM session_project WHERE session_id=?1",
                rusqlite::params![out_sid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            sp_pid, db_pid,
            "session_project.project_id 必须等于 chat_session.project_id（否则 Trae 里删不掉、重启复活）"
        );

        // context.last_real_project_id 也要改成新项目
        let ctx: String = dst
            .query_row(
                "SELECT context FROM chat_session WHERE session_id=?1",
                rusqlite::params![out_sid],
                |r| r.get(0),
            )
            .unwrap();
        let v: Value = serde_json::from_str(&ctx).unwrap();
        assert_eq!(
            v.get("last_real_project_id").and_then(Value::as_str),
            Some(db_pid.as_str()),
            "context.last_real_project_id 必须指向新项目"
        );

        drop(src);
        drop(dst);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `rewrite_last_real_project_id` 的纯函数行为：只认 JSON 对象里的该键；
    /// 非 JSON / 无该键 / 值已相同 一律返回 `None`（调用方保持原值，不写坏数据）。
    #[test]
    fn rewrite_last_real_project_id_only_touches_matching_json() {
        let ctx = r#"{"a":1,"last_real_project_id":"old","b":"x"}"#;
        let out = rewrite_last_real_project_id(ctx, "new").expect("应当改写");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v.get("last_real_project_id").and_then(Value::as_str), Some("new"));
        assert_eq!(v.get("a").and_then(Value::as_i64), Some(1), "其余键必须原样保留");
        assert_eq!(v.get("b").and_then(Value::as_str), Some("x"));

        // 值已相同 ⇒ 不动
        assert!(rewrite_last_real_project_id(ctx, "old").is_none());
        // 不是 JSON 对象
        assert!(rewrite_last_real_project_id("not-json", "new").is_none());
        assert!(rewrite_last_real_project_id("[1,2]", "new").is_none());
        // 没有该键
        assert!(rewrite_last_real_project_id(r#"{"a":1}"#, "new").is_none());
        // 空串
        assert!(rewrite_last_real_project_id("", "new").is_none());
    }

    /// 目标库自愈 SQL 的语义：把 `session_project.project_id` 对齐到 `chat_session.project_id`，
    /// 且**只动不一致的那行**、不动其他行、没有对应会话的行也不动。
    ///
    /// 用真 SQLite 跑（不 mock），因为这条 SQL 本身就是本次修复的核心。
    #[test]
    fn session_project_self_heal_aligns_project_id() {
        let dir = tmp_dir("self_heal");
        let db = dir.join("t.db");
        let c = Connection::open(&db).unwrap();
        c.execute_batch(
            "CREATE TABLE chat_session (session_id TEXT PRIMARY KEY, project_id TEXT);
             CREATE TABLE session_project (id INTEGER PRIMARY KEY, session_id TEXT, project_id TEXT);",
        )
        .unwrap();
        c.execute(
            "INSERT INTO chat_session (session_id, project_id) VALUES ('a','pA'),('b','pB'),('c','pC')",
            [],
        )
        .unwrap();
        // a 一致；b 不一致（要修成 pB）；c 没有关联行
        c.execute(
            "INSERT INTO session_project (id, session_id, project_id) VALUES
             (1,'a','pA'), (2,'b','OLD'), (3,'orphan','pX')",
            [],
        )
        .unwrap();

        let fixed = c
            .execute(
                "UPDATE session_project SET project_id = (\
                     SELECT s.project_id FROM chat_session s \
                     WHERE s.session_id = session_project.session_id\
                 ) \
                 WHERE EXISTS (\
                     SELECT 1 FROM chat_session s \
                     WHERE s.session_id = session_project.session_id \
                       AND ifnull(s.project_id, '') <> ifnull(session_project.project_id, '')\
                 )",
                [],
            )
            .unwrap();
        assert_eq!(fixed, 1, "只应修正 b 这一行");

        let get = |sid: &str| -> String {
            c.query_row(
                "SELECT project_id FROM session_project WHERE session_id=?1",
                rusqlite::params![sid],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(get("a"), "pA", "本来就是一致的不能被改");
        assert_eq!(get("b"), "pB", "b 必须被对齐成 chat_session 里的 pB");
        assert_eq!(get("orphan"), "pX", "没有对应会话的孤儿行不能被动");

        // 幂等：再跑一次不该再改任何行
        let again = c
            .execute(
                "UPDATE session_project SET project_id = (\
                     SELECT s.project_id FROM chat_session s \
                     WHERE s.session_id = session_project.session_id\
                 ) \
                 WHERE EXISTS (\
                     SELECT 1 FROM chat_session s \
                     WHERE s.session_id = session_project.session_id \
                       AND ifnull(s.project_id, '') <> ifnull(session_project.project_id, '')\
                 )",
                [],
            )
            .unwrap();
        assert_eq!(again, 0, "自愈必须是幂等的");

        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn incremental_write_reuses_unchanged_ciphertext() {
        let dir = tmp_dir("incr_reuse");
        let key = random_key_hex();
        let base = synth_plain(20, 11);
        let p_base = dir.join("base-plain.db");
        let p_src = dir.join("src-enc.db");
        let p_new = dir.join("new-plain.db");
        let p_out = dir.join("out-enc.db");
        std::fs::write(&p_base, &base).unwrap();
        // src_enc 的 salt 由 encrypt_db_file 随机生成，增量回写必须沿用原库首页的 salt
        encrypt_db_file(&key, &p_base, &p_src, None).unwrap();

        // 新明文 = 原明文 + 改第 3 页 + 追加第 21、22 页
        let mut newp = base.clone();
        let off3 = 2 * PAGE_SZ;
        let mut p3 = [0u8; PAGE_SZ];
        p3.copy_from_slice(&newp[off3..off3 + PAGE_SZ]);
        for b in p3[..USABLE_SZ].iter_mut() {
            *b ^= 0x5a;
        }
        newp[off3..off3 + PAGE_SZ].copy_from_slice(&p3);
        newp.extend_from_slice(&synth_page(21, 99));
        newp.extend_from_slice(&synth_page(22, 99));
        std::fs::write(&p_new, &newp).unwrap();

        let stats = write_db_incremental(&key, &p_src, &p_new, &p_out, None).unwrap();
        assert_eq!(stats.src_pages, 20);
        assert_eq!(stats.pages, 22);
        assert_eq!(stats.appended_pages, 2);
        assert_eq!(
            stats.changed_pages, 3,
            "只有被改的第 3 页与新增的 2 页需要重新加密，其余 19 页沿用原密文"
        );

        let src = std::fs::read(&p_src).unwrap();
        let out = std::fs::read(&p_out).unwrap();
        assert_eq!(out.len(), 22 * PAGE_SZ);
        for pgno in 1..=20u64 {
            if pgno == 3 {
                continue;
            }
            let o = ((pgno - 1) * PAGE_SZ as u64) as usize;
            assert_eq!(
                &out[o..o + PAGE_SZ],
                &src[o..o + PAGE_SZ],
                "页 {pgno} 未变动，应逐字节复用原密文"
            );
        }
        // 整个新库解密后必须与目标明文逐字节一致
        let key_bytes = hex_to_bytes(&key);
        for pgno in 1..=22u64 {
            let o = ((pgno - 1) * PAGE_SZ as u64) as usize;
            let mut page = [0u8; PAGE_SZ];
            page.copy_from_slice(&out[o..o + PAGE_SZ]);
            assert_eq!(
                crate::modules::trae_decrypt::decrypt_page(&key_bytes, &page, pgno),
                newp[o..o + PAGE_SZ].to_vec(),
                "页 {pgno} 解密结果与目标明文不一致"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn incremental_write_truncates_when_plaintext_shrinks() {
        let dir = tmp_dir("incr_shrink");
        let key = random_key_hex();
        let base = synth_plain(20, 3);
        let p_base = dir.join("base-plain.db");
        let p_src = dir.join("src-enc.db");
        let p_new = dir.join("new-plain.db");
        let p_out = dir.join("out-enc.db");
        std::fs::write(&p_base, &base).unwrap();
        encrypt_db_file(&key, &p_base, &p_src, None).unwrap();

        // 明文缩到 12 页（合成页里页 1 不记录页数，所以前 12 页内容未变）
        let newp = base[..12 * PAGE_SZ].to_vec();
        std::fs::write(&p_new, &newp).unwrap();
        let stats = write_db_incremental(&key, &p_src, &p_new, &p_out, None).unwrap();
        assert_eq!(stats.pages, 12);
        assert_eq!(stats.changed_pages, 0);
        assert_eq!(
            std::fs::metadata(&p_out).unwrap().len(),
            12 * PAGE_SZ as u64,
            "输出库必须按新明文页数截断"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 传入 `old_plain`（原库明文）时，**解密后的明文**必须与「不传、逐页解密比对」等价。
    ///
    /// ⚠️ 不能断言密文逐字节相同：SQLCipher 每页加密用**随机 IV**，同一明文页两次加密
    /// 得到不同密文（都合法）。所以判据是「解密回明文一致」+「变动页数一致」。
    /// 唯一能逐字节比的是**未变动页**——它们该直接沿用原密文。
    #[test]
    fn incremental_write_with_old_plain_matches_decrypt_path() {
        let dir = tmp_dir("incr_oldplain");
        let key = random_key_hex();
        let base = synth_plain(20, 11);
        let p_base = dir.join("base-plain.db");
        let p_src = dir.join("src-enc.db");
        let p_new = dir.join("new-plain.db");
        let p_slow = dir.join("slow-enc.db");
        let p_fast = dir.join("fast-enc.db");
        std::fs::write(&p_base, &base).unwrap();
        encrypt_db_file(&key, &p_base, &p_src, None).unwrap();

        // 新明文：改第 5 页 + 追加第 21 页（共 21 页）
        let mut newp = base.clone();
        let off5 = 4 * PAGE_SZ;
        let mut p5 = [0u8; PAGE_SZ];
        p5.copy_from_slice(&newp[off5..off5 + PAGE_SZ]);
        for b in p5[..USABLE_SZ].iter_mut() {
            *b ^= 0x3c;
        }
        newp[off5..off5 + PAGE_SZ].copy_from_slice(&p5);
        newp.extend_from_slice(&synth_page(21, 7));
        std::fs::write(&p_new, &newp).unwrap();
        let total_pages = 21u64;

        let slow = write_db_incremental(&key, &p_src, &p_new, &p_slow, None).unwrap();
        let fast =
            write_db_incremental_with(&key, &p_src, Some(&p_base), &p_new, &p_fast, None).unwrap();

        assert_eq!(slow.changed_pages, fast.changed_pages, "变动页数必须一致");
        assert_eq!(slow.appended_pages, fast.appended_pages);
        assert_eq!(slow.pages, fast.pages);
        assert_eq!(fast.changed_pages, 2, "只有改动的第 5 页与新增的第 21 页");

        // ① 两条路径产出的库解密后必须逐页等于目标明文
        let key_bytes = hex_to_bytes(&key);
        let (out_slow, out_fast) = (std::fs::read(&p_slow).unwrap(), std::fs::read(&p_fast).unwrap());
        assert_eq!(out_slow.len(), out_fast.len());
        assert_eq!(out_fast.len(), total_pages as usize * PAGE_SZ, "输出应为 21 页");
        for pgno in 1..=total_pages {
            let o = ((pgno - 1) * PAGE_SZ as u64) as usize;
            let mut a = [0u8; PAGE_SZ];
            let mut b = [0u8; PAGE_SZ];
            a.copy_from_slice(&out_slow[o..o + PAGE_SZ]);
            b.copy_from_slice(&out_fast[o..o + PAGE_SZ]);
            let want = &newp[o..o + PAGE_SZ];
            assert_eq!(
                crate::modules::trae_decrypt::decrypt_page(&key_bytes, &a, pgno),
                want.to_vec(),
                "慢路径页 {pgno} 解密结果与目标明文不一致"
            );
            assert_eq!(
                crate::modules::trae_decrypt::decrypt_page(&key_bytes, &b, pgno),
                want.to_vec(),
                "快路径页 {pgno} 解密结果与目标明文不一致"
            );
        }

        // ② 未变动页（除 5、21 外）两条路径都应**逐字节沿用原密文**
        let src = std::fs::read(&p_src).unwrap();
        for pgno in 1..=20u64 {
            if pgno == 5 {
                continue;
            }
            let o = ((pgno - 1) * PAGE_SZ as u64) as usize;
            assert_eq!(&out_fast[o..o + PAGE_SZ], &src[o..o + PAGE_SZ], "快路径页 {pgno} 应复用原密文");
            assert_eq!(&out_slow[o..o + PAGE_SZ], &src[o..o + PAGE_SZ], "慢路径页 {pgno} 应复用原密文");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 判定为「变动」的页若其**尾部保留区非零**，必须中止回写并报错、且不留下半成品文件。
    /// （保留区非零意味着加密会丢字节 ⇒ 宁可失败也不能写出坏库。）
    #[test]
    fn incremental_write_aborts_when_changed_page_reserved_area_dirty() {
        let dir = tmp_dir("incr_dirty_reserved");
        let key = random_key_hex();
        let base = synth_plain(8, 5);
        let p_base = dir.join("base-plain.db");
        let p_src = dir.join("src-enc.db");
        let p_dirty = dir.join("dirty-plain.db");
        let p_out = dir.join("out-enc.db");
        std::fs::write(&p_base, &base).unwrap();
        encrypt_db_file(&key, &p_base, &p_src, None).unwrap();

        // 待回写的明文里，第 3 页保留区塞非零；old_plain 用干净的原库明文 ⇒ 该页被判为变动。
        let mut dirty = base.clone();
        let o3 = 2 * PAGE_SZ;
        for b in dirty[o3 + USABLE_SZ..o3 + PAGE_SZ].iter_mut() {
            *b = 0x7f;
        }
        std::fs::write(&p_dirty, &dirty).unwrap();

        let r = write_db_incremental_with(&key, &p_src, Some(&p_base), &p_dirty, &p_out, None);
        assert!(r.is_err(), "保留区非零时应中止回写，而不是产出可能损坏的库");
        assert!(!p_out.exists(), "中止后不得留下半成品输出文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- 同步方向 → 两端角色（plan_roles）------------------------------------

    /// 回归：跨账号同步曾经 100% 报「会话已不在预期账号下」。
    /// 根因是**归属校验与写库各写了一套 uid**：校验按 role 写死取 src_uid，
    /// 而跨账号时两端 uid 必然不同 ⇒ 每次都判失败。
    /// 现在两者都走 `plan_roles` 这一个出口，这里把映射关系锁住。
    #[test]
    fn plan_roles_maps_from_and_kept_by_direction() {
        let src = ("u-src".to_string(), "s-src".to_string());
        let tgt = ("u-tgt".to_string(), "s-tgt".to_string());

        let got = plan_roles(
            src.0.clone(),
            src.1.clone(),
            tgt.0.clone(),
            tgt.1.clone(),
            "sourceToTarget",
        )
        .unwrap();
        // 内容来自源，**占据 id 与归属的是目标端**。
        assert_eq!(
            got,
            (
                "u-src".to_string(),
                "s-src".to_string(),
                "u-tgt".to_string(),
                "s-tgt".to_string()
            )
        );

        let got = plan_roles(src.0, src.1, tgt.0, tgt.1, "targetToSource").unwrap();
        assert_eq!(
            got,
            (
                "u-tgt".to_string(),
                "s-tgt".to_string(),
                "u-src".to_string(),
                "s-src".to_string()
            )
        );
    }

    #[test]
    fn plan_roles_rejects_unknown_direction() {
        let r = plan_roles(
            "u1".to_string(),
            "s1".to_string(),
            "u2".to_string(),
            "s2".to_string(),
            "sideways",
        );
        assert!(r.is_err(), "未知方向必须被拒，不能悄悄当成某一边");
    }

    // --- 同库复制：消息 id ↔ 内容/引用的对齐（zip 错位的回归护栏）----------------

    /// ⚠️ 回归护栏：同库复制时，「源 `message_id` → 新 id」的映射必须**按行自带的
    /// `message_id`** 建立，绝不能拿「源 id 列表」与「`IN` 查询返回的行」按位置硬配。
    ///
    /// 曾经的缺陷（用户实测「同步到另一账号后客户端只剩一句」）：
    /// `mids.iter().zip(rows)`。SQLite 的 `WHERE message_id IN (…)` 走 `message_id`
    /// 索引、**按字母序**返回，而 `mids` 是 `session_id` 索引的 rowid 序 —— 两者一错位，
    /// 每条消息被写上的新 id 就属于**另一条**源消息；`chat_turn` 的 reply/response
    /// （经 `mid_map` 改写）随之全部指向错人，客户端按轮次渲染就整段塌掉。
    ///
    /// 本用例刻意让 id 的**字母序与插入序完全相反**，一旦退回「按位置硬配」必然失败。
    #[test]
    fn same_db_copy_aligns_message_ids_with_their_own_rows() {
        let dir = tmp_dir("mid_align");
        let db = dir.join("t.db");
        make_trae_db(&db, &[("s1", "wenku", "2026-10-06 22:00:00")]);
        {
            let c = Connection::open(&db).unwrap();
            c.execute("ALTER TABLE chat_turn ADD COLUMN reply_to_message_id TEXT", [])
                .unwrap();
            c.execute("ALTER TABLE chat_turn ADD COLUMN response_message_id TEXT", [])
                .unwrap();
            for t in [
                "chat_message",
                "chat_message_general",
                "chat_message_task",
                "chat_turn",
            ] {
                c.execute(&format!("DELETE FROM {t}"), []).unwrap();
            }
            // 字母序 www < xxx < yyy < zzz，与插入序 zzz,yyy,xxx,www **完全相反**
            for (mid, role, content) in [
                ("zzz-m1", "user", "c1"),
                ("yyy-m2", "assistant", "c2"),
                ("xxx-m3", "user", "c3"),
                ("www-m4", "assistant", "c4"),
            ] {
                c.execute(
                    "INSERT INTO chat_message (message_id, session_id, turn_id, role, content) \
                     VALUES (?1,'s1','t1',?2,?3)",
                    rusqlite::params![mid, role, content],
                )
                .unwrap();
            }
            c.execute(
                "UPDATE chat_message SET reply_to_message_id='zzz-m1' WHERE message_id='yyy-m2'",
                [],
            )
            .unwrap();
            c.execute(
                "UPDATE chat_message SET reply_to_message_id='xxx-m3' WHERE message_id='www-m4'",
                [],
            )
            .unwrap();
            for (tid, reply, resp) in [("T1", "zzz-m1", "yyy-m2"), ("T2", "xxx-m3", "www-m4")] {
                c.execute(
                    "INSERT INTO chat_turn (turn_id, session_id, reply_to_message_id, response_message_id) \
                     VALUES (?1,'s1',?2,?3)",
                    rusqlite::params![tid, reply, resp],
                )
                .unwrap();
            }
        }

        let conn = Connection::open(&db).unwrap();
        let opts = CopyOpts::same_db_for(Some("u2"));
        let (_n, out) = copy_session(&conn, &conn, "s1", &opts, None)
            .unwrap()
            .expect("同库复制应当成功");

        // 副本里：新 message_id → 它承载的内容
        let mut content_of: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        {
            let mut st = conn
                .prepare("SELECT message_id, content FROM chat_message WHERE session_id=?1")
                .unwrap();
            let rows = st
                .query_map([&out], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap();
            for r in rows.flatten() {
                content_of.insert(r.0, r.1);
            }
        }
        assert_eq!(content_of.len(), 4, "源会话 4 条消息都要复制过来");

        // ① 轮次引用：reply→c1 的响应必须是 c2；错位时会变成 (c4,c3) 之类
        let mut pairs: Vec<(String, String)> = Vec::new();
        {
            let mut st = conn
                .prepare(
                    "SELECT reply_to_message_id, response_message_id FROM chat_turn WHERE session_id=?1",
                )
                .unwrap();
            let rows = st
                .query_map([&out], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap();
            for r in rows.flatten() {
                let a = content_of.get(&r.0).cloned().unwrap_or_else(|| "<缺>".into());
                let b = content_of.get(&r.1).cloned().unwrap_or_else(|| "<缺>".into());
                pairs.push((a, b));
            }
        }
        pairs.sort();
        assert_eq!(
            pairs,
            vec![
                ("c1".to_string(), "c2".to_string()),
                ("c3".to_string(), "c4".to_string()),
            ],
            "chat_turn 的 reply/response 必须按 id 映射落到对应消息上（zip 错位会打乱配对）"
        );

        // ② 消息自身引用：副本里的父子关系必须落在**副本自己的**消息上
        let mut child_pairs: Vec<(String, String)> = Vec::new();
        {
            let mut st = conn
                .prepare(
                    "SELECT message_id, reply_to_message_id FROM chat_message WHERE session_id=?1",
                )
                .unwrap();
            let rows = st
                .query_map([&out], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap();
            for r in rows.flatten() {
                if r.1.is_empty() {
                    continue;
                }
                let me = content_of.get(&r.0).cloned().unwrap_or_else(|| "<缺>".into());
                let parent = content_of
                    .get(&r.1)
                    .cloned()
                    .unwrap_or_else(|| "<缺>".into());
                child_pairs.push((me, parent));
            }
        }
        child_pairs.sort();
        assert_eq!(
            child_pairs,
            vec![
                ("c2".to_string(), "c1".to_string()),
                ("c4".to_string(), "c3".to_string()),
            ],
            "chat_message.reply_to_message_id 也必须按 id 映射（不能指向源会话的消息）"
        );

        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- 真实数据只读演练（默认 #[ignore]，需显式跑）-----------------------------

    /// 把**当前实时库**解密到临时副本，在副本上重放 `sync_groups` 的核心
    /// （`delete_rows_in_conn` + `copy_session`），检查复制后 `chat_turn` 的
    /// 引用是否仍与消息类型对齐。
    ///
    /// ⚠️ 全程只读实时库：不替换、不写回、不重启客户端。
    ///
    ///   cargo test -p wb-switch-core real_data_sync_rehearsal -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_data_sync_rehearsal() {
        use crate::modules::{trae_decrypt, trae_delete, trae_discover};
        let client_key = std::env::var("TW_CLIENT_KEY").unwrap_or_else(|_| "solo-cn".into());
        let Some(client) = get_client(&client_key) else {
            println!("未安装客户端 {client_key}");
            return;
        };
        let Some(key) = crate::modules::trae_memory_scan::load_saved_key(&client_key) else {
            println!("没有存盘密钥，先跑一次「扫描密钥并解密」");
            return;
        };
        // 关联组的第一组；角色（source / target）由 `group_members` 推导。
        let store = store_dir().join("trae-session-links.json");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&store).expect("读关联表"))
            .expect("解析关联表");
        let Some(gid) = v["groups"][0]["id"].as_str().map(str::to_string) else {
            println!("关联表里还没有任何关联组");
            return;
        };
        let links = crate::modules::trae_session_links::group_members(&client_key, &gid)
            .ok()
            .flatten()
            .unwrap_or_default();
        let pick = |role: &str| {
            links
                .iter()
                .find(|m| m.role == role)
                .map(|m| (m.uid.clone(), m.session_id.clone()))
        };
        let (Some((s_uid, s_sid)), Some((t_uid, t_sid))) = (pick("source"), pick("target")) else {
            println!("关联组缺少 source/target 成员");
            return;
        };
        println!(
            "关联组 {gid}：source {} / target {}",
            &s_sid[..8.min(s_sid.len())],
            &t_sid[..8.min(t_sid.len())]
        );

        let live = trae_discover::database_path(&client);
        let wal = PathBuf::from(format!("{}-wal", live.to_string_lossy()));
        let dir = tmp_dir("real_rehearsal");
        let orig = dir.join("orig.db");
        let work = dir.join("work.db");
        trae_decrypt::decrypt_database(&live, &key, &orig, None).expect("解密实时库");
        std::fs::copy(&orig, &work).unwrap();
        let merged = trae_delete::merge_wal_into_plain(&work, &wal, &key).unwrap_or(0);
        println!("已合并 WAL {merged} 帧（只读副本）");

        let conn = Connection::open(&work).unwrap();

        // `(轮次引用错位数, 缺内容行的消息数, 词条镜像行数)`
        let probe = |tag: &str, sid: &str| -> (usize, usize, i64) {
            let mut bad = 0usize;
            let mut total = 0usize;
            let mut st = conn
                .prepare(
                    "SELECT t.reply_to_message_id, t.response_message_id, \
                            (SELECT message_type FROM chat_message WHERE message_id=t.reply_to_message_id), \
                            (SELECT message_type FROM chat_message WHERE message_id=t.response_message_id) \
                     FROM chat_turn t WHERE t.session_id=?1",
                )
                .unwrap();
            let rows = st
                .query_map([sid], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })
                .unwrap();
            for r in rows.flatten() {
                total += 1;
                let ok = r.2.as_deref() == Some("general") && r.3.as_deref() == Some("task");
                if !ok {
                    bad += 1;
                    println!(
                        "   ✗ turn reply_type={:?} resp_type={:?}（应 general → task）",
                        r.2, r.3
                    );
                }
            }
            // ⚠️ 「只剩一句」的直接判据：**按类型**找内容行 —— `general` 消息要有
            // `chat_message_general` 行，`task` 消息要有 `chat_message_task` 行。
            // 不能图省事写成「两张表任一张有行就算有内容」：错位时 user 消息的 mid
            // 上恰好挂着一行 task 内容，那样数出来是 0，**假通过**。
            let missing: i64 = conn
                .query_row(
                    "SELECT count(*) FROM chat_message m WHERE m.session_id=?1 AND ( \
                       (m.message_type='general' AND NOT EXISTS \
                          (SELECT 1 FROM chat_message_general g WHERE g.message_id=m.message_id)) \
                       OR (m.message_type='task' AND NOT EXISTS \
                          (SELECT 1 FROM chat_message_task t WHERE t.message_id=m.message_id)) \
                     )",
                    [sid],
                    |r| r.get(0),
                )
                .unwrap_or(-1);
            let fts: i64 = conn
                .query_row(
                    "SELECT count(*) FROM fts_message_content WHERE session_id=?1",
                    [sid],
                    |r| r.get(0),
                )
                .unwrap_or(-1);
            println!(
                "[{tag}] sid {} → {total} 个 turn／{bad} 引用错位；{missing} 条消息缺内容；词条镜像 {fts} 行",
                &sid[..8.min(sid.len())]
            );
            (bad, missing as usize, fts)
        };

        // 按 `message_index` 取「客户端会渲染出来的内容」逐条快照，用于**逐条比对**。
        let contents = |sid: &str| -> Vec<(i64, String, String)> {
            let mut st = conn
                .prepare(
                    "SELECT m.message_index, m.message_role, \
                        ifnull((SELECT substr(content,1,80) FROM chat_message_general g WHERE g.message_id=m.message_id), \
                        ifnull((SELECT substr(content,1,80) FROM chat_message_task t WHERE t.message_id=m.message_id), '<无内容>')) \
                     FROM chat_message m WHERE m.session_id=?1 ORDER BY m.message_index",
                )
                .unwrap();
            st.query_map([sid], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
        };

        println!("--- 重放前 ---");
        let (bad_s, miss_s, fts_s) = probe("before source", &s_sid);
        let (bad_t, miss_t, fts_t) = probe("before target", &t_sid);
        println!(
            "内容对照：source {:?} / target {:?}",
            contents(&s_sid).len(),
            contents(&t_sid).len()
        );

        // 修复目标：把**当前引用错位**的那一侧当作 kept（保留它的会话 id 与归属），
        // 从**干净**的一侧把内容整份复制过来 —— 这就是「同步」本身要做的事。
        let (keep_sid, keep_uid, keep_before, from_sid) = if bad_s > 0 || miss_s > 0 {
            (s_sid.clone(), s_uid.clone(), bad_s.max(miss_s), t_sid.clone())
        } else {
            (t_sid.clone(), t_uid.clone(), bad_t.max(miss_t), s_sid.clone())
        };
        let _ = (fts_s, fts_t);
        println!(
            "演练方向：from {}（干净）→ kept {}（修它的引用）",
            &from_sid[..8.min(from_sid.len())],
            &keep_sid[..8.min(keep_sid.len())]
        );
        if keep_before == 0 {
            println!("两侧本来都没有引用错位，无需修复（跳过复制）");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let from_contents = contents(&from_sid);
        let _ = trae_delete::delete_rows_in_conn(&conn, &keep_sid);
        let opts = CopyOpts {
            dst_uid: Some(keep_uid),
            same_db: true,
            new_sid: Some(keep_sid.clone()),
        };
        let copied = copy_session(&conn, &conn, &from_sid, &opts, None);
        println!("复制结果：{copied:?}");
        println!("--- 重放后（修复后应当 0 个错位、0 条缺内容、内容逐条一致）---");
        let (after, missing_after, fts_after) = probe("after kept", &keep_sid);
        let kept_contents = contents(&keep_sid);

        // ⚠️⚠️ 这一条才是用户报的「同步过去还是旧记录」的直接判据：
        //     修复后 kept 端**逐条内容**必须与源端完全一致（条数、序号、role、正文）。
        assert_eq!(
            kept_contents.len(),
            from_contents.len(),
            "同步后消息条数必须与源端一致"
        );
        for (i, (k, f)) in kept_contents.iter().zip(from_contents.iter()).enumerate() {
            assert_eq!(k.0, f.0, "第 {i} 条 message_index 不一致");
            assert_eq!(k.1, f.1, "第 {i} 条 message_role 不一致");
            assert_eq!(
                k.2, f.2,
                "第 {i} 条内容不一致（同步后显示的必须是源端内容）：kept={:?} from={:?}",
                k.2, f.2
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(after, 0, "修复后不允许再出现 turn 引用错位");
        assert_eq!(missing_after, 0, "修复后不允许再有「消息在、内容行不在」");
        assert_eq!(
            fts_after, fts_t.max(fts_s),
            "词条镜像行数应与源端一致（否则客户端搜不到同步过来的内容）"
        );
        println!(
            "\n重放前错位 {keep_before} 个 / 缺内容 {miss_s} 条 → 重放后错位 {after} 个 / 缺内容 {missing_after} 条；\
             词条镜像 {fts_after} 行（实时库未被改动）"
        );
    }
}
