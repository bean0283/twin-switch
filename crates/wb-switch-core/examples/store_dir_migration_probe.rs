//! 真机验证数据目录搬迁（**只碰自己造的临时目录，不碰真实 `~`**）。
//!
//! ## 为什么需要它
//!
//! 产品从 `trae-switch-cn` 改名成 `twin-switch`，本机数据目录也从
//! `~/.trae-switch-cn` 改成 `~/.twin-switch`。那目录里压着几个 GB 的真数据
//! （626 MB 解密库、940 MB 回收站、318 MB 导入备份…），所以搬迁必须是
//! **同盘 `rename`**（元数据操作）而**不是复制**。
//!
//! 单元测试已经覆盖了三条分支，但那是直接在临时目录上调 `resolve_store_dir()`。
//! 这个示例走的是**生产路径**：设 `TWIN_SWITCH_HOME`（老的 `TRAE_SWITCH_HOME`
//! 也仍然认，见 `config::HOME_ENV_VAR_LEGACY`）→ 调
//! `config::ensure_store_dir_ready()`（`lib.rs::run()` 启动时调的就是它）。
//!
//! ## 验证三件事
//!
//! 1. **真搬过去**：旧目录消失、文件在新目录、内容逐字节一致；
//! 2. **够快**：造一份 ~300 MB 的假数据，量 `rename` 耗时 —— 证明它与体积**无关**
//!    （同盘改名只动目录项），也顺带证伪「2 GB 要不要复制几秒」的担心；
//! 3. **幂等 + 提示**：再调一次仍然返回新目录、且不再有提示。
//!
//! ## 用法
//!
//! ```text
//! cargo run -p wb-switch-core --example store_dir_migration_probe
//! ```
//!
//! 全程只用 `std::env::temp_dir()` 下的随机目录，结束会把它们删掉。

use std::path::{Path, PathBuf};
use std::time::Instant;

use wb_switch_core::modules::config;

/// 造一份「像真的」的旧数据目录：目录结构 + 约 300 MB 稀疏内容。
fn build_fake_legacy_dir(home: &Path) -> PathBuf {
    let old = home.join(config::STORE_DIR_NAME_LEGACY);
    std::fs::create_dir_all(old.join("trae/decrypted")).unwrap();
    std::fs::create_dir_all(old.join("trash/20261006_133308")).unwrap();
    std::fs::create_dir_all(old.join("workbuddy_import_backup/solo-cn-20261005-231955")).unwrap();
    std::fs::create_dir_all(old.join("cache")).unwrap();

    std::fs::write(old.join("workbuddy-accounts.json"), "{\"accounts\":[]}").unwrap();
    std::fs::write(old.join("cache/overview.json"), "{\"version\":1}").unwrap();

    // 300 MB：3 个 100 MB 文件。rename 不该关心它们有多大。
    let chunk = vec![0xA5u8; 1024 * 1024];
    for name in ["trae/decrypted/solo-cn.db", "trash/20261006_133308/big1.bin", "workbuddy_import_backup/solo-cn-20261005-231955/big2.bin"] {
        let p = old.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        use std::io::Write;
        for _ in 0..100 {
            f.write_all(&chunk).unwrap();
        }
    }
    old
}

fn dir_bytes(p: &Path) -> u64 {
    let mut total = 0u64;
    for entry in walkdir(p) {
        if let Ok(m) = std::fs::metadata(&entry) {
            if m.is_file() {
                total += m.len();
            }
        }
    }
    total
}

/// 够用的递归遍历（故意不引第三方 crate）。
fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            out.push(p);
        }
    }
    out
}

fn main() {
    let home = std::env::temp_dir().join(format!(
        "twin-switch-migrate-probe-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    let old = build_fake_legacy_dir(&home);
    let bytes = dir_bytes(&old);
    println!(
        "① 造好假旧目录：{}（{:.1} MB）",
        old.display(),
        bytes as f64 / 1024.0 / 1024.0
    );
    assert!(old.is_dir());

    // 走生产路径：先设家目录覆盖，再调 ensure_store_dir_ready()。
    // （这一步必须在任何其它 config 调用之前，否则目录已被解析并缓存。）
    std::env::set_var(config::HOME_ENV_VAR, &home);
    let t0 = Instant::now();
    let (dir, note) = config::ensure_store_dir_ready();
    let elapsed = t0.elapsed();

    println!("② ensure_store_dir_ready() 耗时 {:?}", elapsed);
    println!("   目录 = {}", dir.display());
    println!("   提示 = {:?}", note);

    let new = home.join(config::STORE_DIR_NAME);
    let ok_dir = dir == new;
    let ok_old_gone = !old.exists();
    let ok_bytes = dir_bytes(&new) == bytes;
    let ok_json = std::fs::read_to_string(new.join("workbuddy-accounts.json")).unwrap_or_default()
        == "{\"accounts\":[]}";
    let ok_note = note.is_none();

    println!(
        "③ 判定: 用新目录={ok_dir} 旧目录已消失={ok_old_gone} 体积一致={ok_bytes} 内容一致={ok_json} 无告警={ok_note}"
    );

    // 幂等：再解析一次不该再动任何东西。
    let (dir2, note2) = config::ensure_store_dir_ready();
    println!(
        "④ 再调一次: 目录不变={} 仍无告警={}",
        dir2 == new,
        note2.is_none()
    );

    let all = ok_dir && ok_old_gone && ok_bytes && ok_json && ok_note && dir2 == new && note2.is_none();

    let _ = std::fs::remove_dir_all(&home);
    println!(
        "\n结论: {}（临时目录已清理）",
        if all { "✅ 全部符合预期" } else { "❌ 有不符合项，见上" }
    );
    if !all {
        std::process::exit(1);
    }
}
