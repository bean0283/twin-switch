//! 重置 Trae 客户端的**登录态**（issue #10：登录第 4 个账号后无法对话时的自救手段）。
//!
//! 为什么需要：`trae_switch` 切换账号是整目录替换 `storage.json` + 各 `Local Storage/leveldb`，
//! 但有两处会让「新号」与「旧态」混着用：
//!   ① 网页(OAuth)凭证账号的载体是**以当前登录态为骨架合成**的，骨架里带着上一台的机器指纹；
//!   ② `User/globalStorage` 被 `is_global_state_entry` 排除在替换之外，旧登录态可能留在别处。
//! 表现就是「能进界面、发消息被拒」。这时最有效的自救是：把客户端侧登录态清干净，重新登录一次。
//!
//! ⚠️ 边界（写死在代码里，别松）：
//!   - **只动客户端自己的登录载体**：`storage.json` 里 `iCubeAuthInfo*` 这几个键，以及（可选）
//!     各 `Local Storage/leveldb` 目录；
//!   - **不动本工具的账号档案**（`~/.twin-switch` 里的 vault），也不碰任何会话数据 ——
//!     清完照样能把原来的账号再切回来；
//!   - 动手前先整份备份到 `~/.twin-switch/trae/backups/login-reset-<时间戳>/`，可原样搬回。

use serde_json::{json, Value};
use std::path::Path;

use crate::modules::config::store_dir;
use crate::modules::trae_carriers::{expand_entries, overwrite_entry};
use crate::modules::trae_discover::{get_client, storage_json_path, user_data_dir, TraeClient};
use crate::modules::trae_switch::{effective_entries, is_running, launch, quit_client};

/// storage.json 里代表登录态的键前缀（`iCubeAuthInfo://icube.cloudide`、`://usertag` …）。
///
/// ⚠️ 只清这个前缀：`telemetry.machineId` / `telemetry.devDeviceId` 必须留着 ——
/// 清掉会让服务端把本机当成新设备，反而更容易触发风控。
const AUTH_KEY_PREFIX: &str = "iCubeAuthInfo";

fn sep(rel: &str) -> String {
    rel.replace('/', std::path::MAIN_SEPARATOR.to_string().as_str())
}

/// 某个载体条目的体量：`(kind, bytes, files)`。目录递归统计。
fn measure(p: &Path) -> (String, u64, usize) {
    let Ok(meta) = std::fs::metadata(p) else {
        return ("missing".into(), 0, 0);
    };
    if meta.is_file() {
        return ("file".into(), meta.len(), 1);
    }
    fn walk(dir: &Path, bytes: &mut u64, files: &mut usize, depth: usize) {
        if depth > 6 {
            return;
        }
        let Ok(children) = std::fs::read_dir(dir) else {
            return;
        };
        for child in children.flatten() {
            let Ok(m) = child.metadata() else { continue };
            if m.is_dir() {
                walk(&child.path(), bytes, files, depth + 1);
            } else {
                *bytes += m.len();
                *files += 1;
            }
        }
    }
    let (mut b, mut f) = (0u64, 0usize);
    walk(p, &mut b, &mut f, 0);
    ("dir".into(), b, f)
}

/// storage.json 里现存的登录态键（只读；预览与执行共用同一份判据）。
fn auth_keys_of(client: &TraeClient) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(storage_json_path(client)) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    v.as_object()
        .map(|o| {
            o.keys()
                .filter(|k| k.starts_with(AUTH_KEY_PREFIX))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn backup_root() -> std::path::PathBuf {
    store_dir().join("trae").join("backups")
}

/// 预览：会动哪些文件、多大、当前登录是谁、客户端是否在跑。**只读**。
pub fn preview(client_key: &str) -> Value {
    let Some(client) = get_client(client_key) else {
        return json!({ "ok": false, "error": "未知客户端" });
    };
    let root = user_data_dir(client);
    let entries = effective_entries(client_key, &root);
    if entries.is_empty() {
        return json!({
            "ok": false,
            "error": "没有探测到该客户端的登录载体文件（先在该客户端里登录一次再看）",
        });
    }
    let mut items = Vec::new();
    let mut total = 0u64;
    for rel in &entries {
        let p = root.join(sep(rel));
        let (kind, bytes, files) = measure(&p);
        total += bytes;
        let is_leveldb = rel.to_lowercase().ends_with("leveldb");
        items.push(json!({
            "rel": rel,
            "kind": kind,
            "bytes": bytes,
            "files": files,
            "leveldb": is_leveldb,
            // 默认只清 storage.json 的 auth 键；leveldb 要用户勾选才会整目录清掉。
            "action": if is_leveldb { "可选：整目录清除" } else { "删除登录键" },
        }));
    }
    let live = crate::modules::trae_switch::describe_account(client_key);
    json!({
        "ok": true,
        "clientKey": client.key,
        "clientLabel": client.label,
        "rootDir": root.to_string_lossy(),
        "running": is_running(client),
        "storagePath": storage_json_path(client).to_string_lossy(),
        "authKeys": auth_keys_of(client),
        "liveUid": live.get("uid").cloned().unwrap_or(Value::Null),
        "liveLabel": live.get("label").cloned().unwrap_or(Value::Null),
        "entries": items,
        "totalBytes": total,
        "backupRoot": backup_root().to_string_lossy(),
    })
}

/// 只删 `storage.json` 里的登录键；**保留**其它键（机器指纹、版本、偏好）。
///
/// 写临时文件再 rename：写一半掉电不会留下半个 JSON 把客户端搞坏。
fn strip_auth_keys(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 storage.json 失败：{e}"))?;
    let mut v: Value = serde_json::from_str(&text)
        .map_err(|e| format!("解析 storage.json 失败（未做任何改动）：{e}"))?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| "storage.json 不是 JSON 对象（未做任何改动）".to_string())?;
    let keys: Vec<String> = obj
        .keys()
        .filter(|k| k.starts_with(AUTH_KEY_PREFIX))
        .cloned()
        .collect();
    for k in &keys {
        obj.remove(k);
    }
    let body = serde_json::to_vec_pretty(&v).map_err(|e| format!("序列化 storage.json 失败：{e}"))?;
    let tmp = path.with_extension("json.reset-tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("写入临时文件失败：{e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("替换 storage.json 失败：{e}"))?;
    Ok(keys)
}

/// 执行重置。
///
/// `include_leveldb`：连各 `Local Storage/leveldb` 一起清（最彻底，但会连带清掉界面偏好）。
/// `relaunch`：清完是否把客户端拉起来（让用户直接手动登录）。
pub fn reset_login_state(
    client_key: &str,
    include_leveldb: bool,
    relaunch: bool,
    progress: Option<&dyn Fn(&str)>,
) -> Result<Value, String> {
    let say = |m: &str| {
        if let Some(f) = progress {
            f(m);
        }
    };
    let client = get_client(client_key).ok_or("未知客户端")?;
    let root = user_data_dir(client);
    let entries = effective_entries(client_key, &root);
    if entries.is_empty() {
        return Err("没有探测到登录载体文件，未做任何改动".into());
    }
    let storage = storage_json_path(client);
    if !storage.is_file() {
        return Err(format!(
            "找不到 storage.json（{}），未做任何改动",
            storage.display()
        ));
    }

    // 1) 客户端在跑就先退：否则它会把内存里的登录态再写回去，白清。
    let stopped = if is_running(client) {
        say("先退出客户端（它运行中会把登录态写回去）…");
        let out = quit_client(client, 20_000);
        if !out.ok {
            return Err(format!(
                "已尝试退出 {} 但没成功：{}",
                client.label,
                out.hint(&client.label)
            ));
        }
        true
    } else {
        false
    };

    // 2) 整份备份（storage.json + 所有 leveldb 目录，按 exp_all 展开成具体文件）
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let backup = backup_root().join(format!("login-reset-{stamp}"));
    std::fs::create_dir_all(&backup).map_err(|e| format!("创建备份目录失败：{e}"))?;
    for rel in &entries {
        overwrite_entry(&root, &backup, rel);
    }
    let backed = expand_entries(&root, &entries).len();
    say(&format!("已备份 {backed} 个文件到 {}", backup.display()));

    // 3) 清 storage.json 的登录键
    let removed_keys = strip_auth_keys(&storage)?;
    say(&format!("已删除登录键 {} 个", removed_keys.len()));

    // 4) 可选：清 leveldb（登录态缓存在这里面）
    let mut removed_dirs: Vec<String> = Vec::new();
    if include_leveldb {
        for rel in &entries {
            if !rel.to_lowercase().ends_with("leveldb") {
                continue;
            }
            let p = root.join(sep(rel));
            if p.is_dir() && std::fs::remove_dir_all(&p).is_ok() {
                removed_dirs.push(rel.clone());
            }
        }
        say(&format!("已清除 {} 个 leveldb 目录", removed_dirs.len()));
    }

    // 5) 拉起客户端（用户手动登录一次）
    let mut relaunched = false;
    let mut relaunch_error: Option<String> = None;
    if relaunch {
        match launch(client_key) {
            Ok(_) => relaunched = true,
            Err(e) => relaunch_error = Some(e),
        }
    }

    Ok(json!({
        "ok": true,
        "clientKey": client.key,
        "clientLabel": client.label,
        "removedKeys": removed_keys,
        "removedDirs": removed_dirs,
        "backupDir": backup.to_string_lossy(),
        "backedFiles": backed,
        "clientStopped": stopped,
        "clientRelaunched": relaunched,
        "relaunchError": relaunch_error,
        "note": "账号档案与会话数据都没有改动；客户端下次启动会要求重新登录。",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只删登录键，其它键（机器指纹 / 版本 / 偏好）一个都不许动。
    #[test]
    fn strip_auth_keys_keeps_machine_identity() {
        let dir = std::env::temp_dir().join(format!("wb-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("storage.json");
        std::fs::write(
            &p,
            r#"{"telemetry.machineId":"m-1","iCubeAuthInfo://icube.cloudide":"KM:x","iCubeAuthInfo://usertag":"u","iCubeLastVersion":"1.2"}"#,
        )
        .unwrap();
        let removed = strip_auth_keys(&p).unwrap();
        assert_eq!(removed.len(), 2, "{removed:?}");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["telemetry.machineId"], "m-1");
        assert_eq!(v["iCubeLastVersion"], "1.2");
        assert!(v.get("iCubeAuthInfo://icube.cloudide").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 没登录态时不能报错（幂等）：删 0 个键也算成功。
    #[test]
    fn strip_auth_keys_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("wb-reset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("storage.json");
        std::fs::write(&p, r#"{"a":1}"#).unwrap();
        assert!(strip_auth_keys(&p).unwrap().is_empty());
        assert!(strip_auth_keys(&p).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
