//! 删除文件/目录的公共实现：**先清只读 + 短退避重试 + 逐项互不牵连**。
//!
//! 为什么单独拎出来：`workbuddy_cleanup` 与 `trae_cleanup` 都要「删一个条目」这件事，
//! 两边各写一份必然跑偏 —— 2026-10-09 的真事就是同一个应用里两个「清空回收站」行为
//! 不一样：Trae 侧逐项删（不报错），WorkBuddy 侧一句 `remove_dir_all` **整棵树一把梭**，
//! 任何一个条目失败就整体 `Err`，用户拿到「清空回收站失败: 拒绝访问。 (os error 5)」，
//! 而且**一个条目都没删掉**。判据：**同一件事的公共实现只该有一份。**
//!
//! 三条判据（都来自真事）：
//! 1. **逐项**：任何「清空」都指「能删的都删掉」，不是「一次全成、否则全不成」；
//! 2. **重试**：删不动的多数是**瞬时**占用（实时防护扫描刚移入的 GB 级文件、
//!    `purge` 收尾重启的客户端正在重新打开日志目录）；
//! 3. **先清只读**：Windows 上 `FILE_ATTRIBUTE_READONLY` 会让删除直接 access denied
//!    —— 极易被误判成「文件被占用」。

use std::path::Path;
use std::time::Duration;

/// 删一个文件/目录（**不跟随符号链接**）：先清只读，失败则短退避重试。
///
/// 4 次尝试、间隔 150/300/450 ms。真正长期占用的（用户自己开着的编辑器）会一路失败，
/// 由调用方把它记进 `failed` 上报，而不是让整次「清空」报销。
pub(crate) fn remove_tree_with_retry(p: &Path) -> Result<(), String> {
    const ATTEMPTS: u64 = 4;
    let mut last = String::new();
    for attempt in 0..ATTEMPTS {
        // 上一轮可能已经把内容删掉了（只差目录壳），这里以「真的不存在了」为成功。
        if std::fs::symlink_metadata(p).is_err() {
            return Ok(());
        }
        clear_readonly_recursive(p);
        let is_dir = std::fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false);
        let r = if is_dir {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        };
        match r {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                if attempt + 1 < ATTEMPTS {
                    std::thread::sleep(Duration::from_millis(150 * (attempt + 1)));
                }
            }
        }
    }
    Err(last)
}

/// 递归清掉只读属性（含子项，**不跟随符号链接**）。
///
/// Windows 上只读文件会让 `remove_dir_all` 直接返回 `拒绝访问。 (os error 5)` ——
/// 这正是 2026-10-09 那次报错最容易被误读成「文件被占用」的地方。
pub(crate) fn clear_readonly_recursive(p: &Path) {
    let Ok(md) = std::fs::symlink_metadata(p) else {
        return;
    };
    if md.permissions().readonly() {
        let mut perm = md.permissions();
        perm.set_readonly(false);
        let _ = std::fs::set_permissions(p, perm);
    }
    if md.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                clear_readonly_recursive(&e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("wbcl-fsrm-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn removes_readonly_file() {
        let base = tmp("ro");
        let f = base.join("ro.txt");
        std::fs::write(&f, "x").unwrap();
        let mut perm = std::fs::metadata(&f).unwrap().permissions();
        perm.set_readonly(true);
        std::fs::set_permissions(&f, perm).unwrap();
        // 只读文件的删除必须先清属性，否则 Windows 上直接 access denied
        remove_tree_with_retry(&f).unwrap();
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn removes_dir_tree_and_missing_path_is_ok() {
        let base = tmp("dir");
        let sub = base.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("c.txt"), "c").unwrap();
        remove_tree_with_retry(&base.join("a")).unwrap();
        assert!(!base.join("a").exists());
        // 已经不存在的路径 = 目标达成，不是错误（重试路径会反复走到这里）
        remove_tree_with_retry(&base.join("a")).unwrap();
        let _ = std::fs::remove_dir_all(&base);
    }
}
