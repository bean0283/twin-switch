//! Qoder 本机数据清理：**只扫缓存 / 日志 / 我们自己的备份**，绝不碰库与登录态。
//!
//! ## 流程（与 Trae / WorkBuddy 清理页**同一套**）
//!
//! 1. `scan()` 列出两侧的可清理项（含大小），只列**存在**的；
//! 2. 用户勾选后调 `clean()`；
//! 3. **默认移入回收站**（`~/.twin-switch/qoder/trash/<时间戳>/<序号>/…`），
//!    同盘 `rename` 几乎零成本；**只有勾「回收站」那一项才是硬删**（清空）；
//! 4. 界面显示释放了多少，用户随时可以再扫、再清。
//!
//! ## 硬边界（任何一项都不许删）
//!
//! - `main.sqlite` / `-wal` / `-shm`：会话库本体；
//! - `auth.v1.dat` / `Local State`：登录态与解密主密钥；
//! - `qoder-data.v1.json`：客户端配置。
//!
//! ⚠️ `main.sqlite.bak-*` 是**我们自己**在写库前做的副本（不是客户端数据），
//! 所以它可以清 —— 但默认**不勾**（删了就不能回滚到那次操作之前）。

use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

use crate::modules::qoder_realm::QoderRegion;

/// 一个可清理项（可能包含多个路径，例如「客户端目录里的历史备份」是多个文件）。
struct Entry {
    key: &'static str,
    label: &'static str,
    paths: Vec<PathBuf>,
    /// 默认是否勾选（缓存默认勾、备份与回收站默认不勾）。
    default_on: bool,
    /// 说明（界面上原样显示）。
    note: &'static str,
}

/// 区域的数据根目录（按存在性判断，**不跨区兜底**）。
pub fn data_root(region: QoderRegion) -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let name = match region {
        QoderRegion::Cn => "com.qodercn.app.stable",
        QoderRegion::Global => "com.qoder.app.stable",
    };
    let p = PathBuf::from(appdata).join(name);
    p.is_dir().then_some(p)
}

/// 我们自己的备份目录（写库前备份就放这）。
pub fn backup_root() -> PathBuf {
    crate::modules::config::store_dir()
        .join("qoder")
        .join("backups")
}

/// 本页回收站根目录。
pub fn trash_root() -> PathBuf {
    crate::modules::config::store_dir()
        .join("qoder")
        .join("trash")
}

/// 单个路径的体积（文件取长度，目录递归求和）。
fn path_size(path: &Path) -> u64 {
    let Ok(md) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if md.is_file() {
        return md.len();
    }
    let mut total = 0u64;
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in rd.flatten() {
        total += path_size(&entry.path());
    }
    total
}

fn entry_size(paths: &[PathBuf]) -> u64 {
    paths.iter().map(|p| path_size(p)).filter(|n| *n > 0).sum()
}

/// 词法规范化：消掉 `.` / `..`，**不碰文件系统**（不要求路径存在）。
///
/// ⚠️ `Path::starts_with` 是**逐组件**比较的，对 `a/trash/../qoder` 这种写法会误判
/// 「在某目录下」。凡是用路径判断「在不在某目录下」，两边都必须先过这里。
fn lexically_normalized(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 该路径是否必须**硬删**（不进回收站）。
///
/// ⚠️ 回收站自己（以及它里面的任何东西）只有一个合理动作：**清空**。
/// 「把回收站移进回收站」会导致无限自我嵌套，把盘写满（Trae 那边真机踩过 116GB）。
fn must_hard_remove(p: &Path) -> bool {
    let t = lexically_normalized(&trash_root());
    let q = lexically_normalized(p);
    q == t || q.starts_with(&t)
}

/// 硬删除（文件 / 目录 / 符号链接）。
fn hard_remove(p: &Path) -> Result<(), String> {
    let md = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
    if md.is_dir() {
        std::fs::remove_dir_all(p).map_err(|e| e.to_string())
    } else {
        std::fs::remove_file(p).map_err(|e| e.to_string())
    }
}

/// 递归复制。`stop_at` = 本次写出的目标根 —— 遇到它会成为其祖先的条目一律跳过，
/// 否则源里包含目标时会把新写出的内容再复制一遍，无限嵌套。
fn copy_recursive(src: &Path, dst: &Path, stop_at: &Path) -> std::io::Result<()> {
    let md = std::fs::symlink_metadata(src)?;
    if md.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)?.flatten() {
            let child = e.path();
            if stop_at.starts_with(&child) {
                continue;
            }
            copy_recursive(&child, &dst.join(e.file_name()), stop_at)?;
        }
        Ok(())
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// 移入回收站（同盘 `rename`，几乎零成本）；跨盘时退化为「复制 + 删除」。
fn move_to_trash(p: &Path, stamp: &str, seq: usize) -> Result<PathBuf, String> {
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "item".to_string());
    let dest_dir = trash_root().join(stamp).join(format!("{seq:03}"));
    let dest = dest_dir.join(&name);
    if lexically_normalized(&dest).starts_with(&lexically_normalized(p)) {
        return Err(format!(
            "拒绝执行：目标 {} 落在源目录自己里面（会造成无限自我复制），已跳过",
            dest.display()
        ));
    }
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("创建回收站目录失败: {e}"))?;
    if std::fs::rename(p, &dest).is_ok() {
        return Ok(dest);
    }
    copy_recursive(p, &dest, &dest).map_err(|e| format!("移入回收站失败: {e}"))?;
    hard_remove(p)?;
    Ok(dest)
}

/// 客户端目录里**我们自己**留下的历史备份（`main.sqlite.bak-*`，含 `-wal` / `-shm`）。
///
/// 早期版本把备份写在客户端数据目录里，一次一份整库（19MB×N），越积越多，
/// 而清理页原先只扫工具目录 ⇒ 这堆东西谁都看不见。这里把它列出来给用户清。
fn client_backups(region: QoderRegion) -> Vec<PathBuf> {
    let Some(root) = data_root(region) else {
        return Vec::new();
    };
    let Ok(rd) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("main.sqlite.bak-"))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

/// 扫描一侧的候选项（只把**存在**的列出来）。
fn entries_for(region: QoderRegion) -> Vec<Entry> {
    let mut out = Vec::new();
    if let Some(root) = data_root(region) {
        // Electron 的缓存目录都是「删了会自己重建」的。
        for (key, label) in [
            ("cache", "浏览器缓存（Cache）"),
            ("code_cache", "代码缓存（Code Cache）"),
            ("gpu_cache", "显卡缓存（GPUCache）"),
            ("dawn_cache", "Dawn 缓存（DawnWebGPUCache）"),
            ("cached_data", "缓存数据（CachedData）"),
            ("partitions", "分区缓存（Partitions）"),
            ("crashpad", "崩溃转储（Crashpad）"),
            ("logs", "日志（logs）"),
        ] {
            let (dir, note) = match key {
                "cache" => (root.join("Cache"), "删除后客户端首次启动会重建，不影响登录与会话"),
                "code_cache" => (root.join("Code Cache"), "同浏览器缓存，可安全删除"),
                "gpu_cache" => (root.join("GPUCache"), "同浏览器缓存，可安全删除"),
                "dawn_cache" => (root.join("DawnWebGPUCache"), "GPU 渲染缓存，可安全删除"),
                "cached_data" => (root.join("CachedData"), "同浏览器缓存，可安全删除"),
                "partitions" => (root.join("Partitions"), "各分区（多窗口）的缓存，可安全删除"),
                "crashpad" => (root.join("Crashpad"), "崩溃转储文件，删掉不影响使用"),
                _ => (root.join("logs"), "客户端运行日志，删掉不影响功能"),
            };
            if dir.exists() {
                out.push(Entry {
                    key,
                    label,
                    paths: vec![dir],
                    default_on: true,
                    note,
                });
            }
        }
    }

    // 我们自己写库前的备份（新位置）：默认在列但**默认不勾**。
    let backups = backup_root();
    if backups.is_dir() && path_size(&backups) > 0 {
        out.push(Entry {
            key: "backups",
            label: "写库前自动备份（main.sqlite 副本）",
            paths: vec![backups],
            default_on: false,
            note: "改标题 / 改消息 / 删消息 / 删会话前的自动备份（每区域保留最近 5 份）；删了就不能回滚到那些操作之前",
        });
    }

    // 旧版本写在客户端目录里的历史备份：这是**我们自己造成的垃圾**，默认勾选。
    let old = client_backups(region);
    if !old.is_empty() {
        out.push(Entry {
            key: "client_baks",
            label: "客户端目录里的历史备份（旧版本遗留）",
            paths: old,
            default_on: true,
            note: "早期版本把整库副本写在客户端数据目录里（每份约等于整个 main.sqlite）；不是客户端数据，删掉不影响会话，只是不能回滚到那些操作",
        });
    }

    out
}

/// 列出两侧的可清理项（含大小）。
///
/// ⚠️ 回收站是**全局一份**（不分区域），所以放在顶层 `trash` 里，
/// 绝不能让每个区域各列一次 —— 那会把同一份回收站算两遍，界面上「合计」直接翻倍
/// （用户实测截图里 193 MB 变成 386 MB 就是这么来的）。
pub fn scan() -> Value {
    let mut sides = Vec::new();
    let mut total_count = 0usize;
    let mut total_bytes = 0u64;
    let mut rec_count = 0usize;
    let mut rec_bytes = 0u64;

    for region in [QoderRegion::Cn, QoderRegion::Global] {
        let entries = entries_for(region);
        let items: Vec<Value> = entries
            .iter()
            .map(|e| {
                let bytes = entry_size(&e.paths);
                total_count += 1;
                total_bytes += bytes;
                if e.default_on {
                    rec_count += 1;
                    rec_bytes += bytes;
                }
                json!({
                    "key": e.key,
                    "label": e.label,
                    "path": if e.paths.len() == 1 {
                        e.paths[0].display().to_string()
                    } else {
                        format!(
                            "{} 个文件（{} 等）",
                            e.paths.len(),
                            e.paths.first().map(|p| p.display().to_string()).unwrap_or_default()
                        )
                    },
                    "bytes": bytes,
                    "defaultOn": e.default_on,
                    "note": e.note,
                })
            })
            .collect();
        sides.push(json!({
            "region": region.as_str(),
            "label": region.display_name(),
            "dataRoot": data_root(region).map(|p| p.display().to_string()),
            "items": items,
            "bytes": entries.iter().map(|e| entry_size(&e.paths)).sum::<u64>(),
        }));
    }

    let trash_bytes = path_size(&trash_root());
    json!({
        "sides": sides,
        "trash": {
            "path": trash_root().display().to_string(),
            "bytes": trash_bytes,
            "items": trash_item_count(),
        },
        "totals": {
            "count": total_count,
            "bytes": total_bytes,
            "recommendedCount": rec_count,
            "recommendedBytes": rec_bytes,
        },
        "scannedAt": chrono::Utc::now().timestamp_millis(),
    })
}

/// 回收站里的条目数（`<时间戳>/<序号>/…` 里的最内层条目）。
fn trash_item_count() -> usize {
    let root = trash_root();
    let Ok(stamps) = std::fs::read_dir(&root) else {
        return 0;
    };
    let mut n = 0usize;
    for stamp in stamps.flatten() {
        if let Ok(slots) = std::fs::read_dir(stamp.path()) {
            for slot in slots.flatten() {
                if let Ok(items) = std::fs::read_dir(slot.path()) {
                    n += items.count();
                }
            }
        }
    }
    n
}

/// 清空回收站（**硬删**，不可恢复）。
pub fn empty_trash() -> Result<Value, String> {
    let root = trash_root();
    if !root.exists() {
        return Ok(json!({ "ok": true, "freed": 0, "already": true }));
    }
    let freed = path_size(&root);
    hard_remove(&root)?;
    Ok(json!({ "ok": true, "freed": freed }))
}

/// 执行清理：只处理**勾选的 key**，且 key 必须在本轮扫描结果里（防任意路径删除）。
///
/// 默认**移入回收站**；只有 `trash` 项（以及回收站内部的路径）是硬删。
pub fn clean(region: QoderRegion, keys: &[String]) -> Result<Value, String> {
    if keys.is_empty() {
        return Err("没有选中任何清理项".into());
    }
    let entries = entries_for(region);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let mut removed = Vec::new();
    let mut freed_total: u64 = 0;
    let mut failed = Vec::new();
    let mut seq = 0usize;

    for key in keys {
        // 回收站是全局项：不管传哪个区域，都是清空同一份。
        if key == "trash" {
            match empty_trash() {
                Ok(v) => {
                    let freed = v.get("freed").and_then(Value::as_u64).unwrap_or(0);
                    freed_total += freed;
                    removed.push(json!({ "key": "trash", "label": "回收站", "bytes": freed, "purged": true }));
                }
                Err(e) => failed.push(format!("回收站：{e}")),
            }
            continue;
        }
        let Some(entry) = entries.iter().find(|e| e.key == key.as_str()) else {
            failed.push(format!("{key}：不在可清理清单里，已跳过"));
            continue;
        };
        let size = entry_size(&entry.paths);
        let mut moved_all = true;
        for p in &entry.paths {
            if !p.exists() {
                continue;
            }
            seq += 1;
            let result = if must_hard_remove(p) {
                hard_remove(p)
            } else {
                move_to_trash(p, &stamp, seq).map(|_| ())
            };
            if let Err(e) = result {
                moved_all = false;
                failed.push(format!("{}：{e}", entry.label));
            }
        }
        if moved_all {
            freed_total += size;
            removed.push(json!({
                "key": entry.key,
                "label": entry.label,
                "bytes": size,
                "purged": false,
            }));
        }
    }

    Ok(json!({
        "ok": failed.is_empty(),
        "region": region.as_str(),
        "removed": removed,
        "freed": freed_total,
        "failed": failed,
        "trashRoot": trash_root().display().to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_touches_database_or_auth_files() {
        // 关键文件**从不出现在**可清理清单里（防以后有人手滑加进去）。
        // 注：`main.sqlite.bak-*` 是我们自己的备份，允许出现，所以按**文件名精确**比对。
        for region in [QoderRegion::Cn, QoderRegion::Global] {
            for e in entries_for(region) {
                for p in &e.paths {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let forbidden = [
                        "main.sqlite",
                        "main.sqlite-wal",
                        "main.sqlite-shm",
                        "auth.v1.dat",
                        "local state",
                        "qoder-data.v1.json",
                    ];
                    assert!(
                        !forbidden.contains(&name.as_str()),
                        "可清理清单里出现了关键文件：{}",
                        p.display()
                    );
                }
            }
        }
    }

    /// 真机自检：打印两侧的扫描结果（只读，不动任何文件）。
    #[test]
    #[ignore]
    fn probe_scan() {
        let v = scan();
        eprintln!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    }

    #[test]
    fn trash_itself_is_always_hard_removed() {
        // 「把回收站移进回收站」是 Trae 那边爆过盘的触发器，这里必须钉死为硬删。
        assert!(must_hard_remove(&trash_root()));
        assert!(must_hard_remove(
            &trash_root().join("20261024-101010").join("001")
        ));
        assert!(!must_hard_remove(
            &crate::modules::config::store_dir().join("qoder").join("backups")
        ));
    }
}
