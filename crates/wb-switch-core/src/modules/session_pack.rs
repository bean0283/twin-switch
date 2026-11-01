//! 会话包（`.zip`）的公共部分：manifest 结构 + 解包/校验。
//!
//! Trae 与 WorkBuddy 的会话存储形态不同（前者是 SQLite 多表，后者是「库里的行 + jsonl 正文」），
//! 所以各自的打包逻辑分开写；但**包的元信息与解包规则必须共用一份**，
//! 否则两边的 `formatVersion` 语义迟早跑偏，导入时认不出对方的包。
//!
//! ## 安全
//!
//! 解包时逐条过 [`zip::read::ZipFile::enclosed_name`]，**拒绝任何带 `..` 或绝对路径的条目**
//! （zip slip）。包里是可以来自别人机器的文件，不能想当然。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

/// 包格式标识。两个产品共用一个标识前缀，靠 `kind` 区分。
pub const PACK_FORMAT: &str = "twinswitch.session-pack";
/// 当前支持的包版本。导入时**只接受 ≤ 本版本**的包。
pub const PACK_VERSION: u32 = 1;

/// 包里的元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// 固定为 [`PACK_FORMAT`]。
    pub format: String,
    /// 包格式版本。高版本的包**拒绝导入**（老程序不认识新字段，硬导会写坏数据）。
    pub format_version: u32,
    /// 导出时间（RFC3339）。
    pub exported_at: String,
    /// 导出方程序版本（仅供排查，不参与判定）。
    #[serde(default)]
    pub app_version: String,
    /// 来源客户端 key（`trae-cn` / 多源时逗号分隔）。
    #[serde(default)]
    pub source_client: String,
    /// 来源客户端显示名（给人看）。
    #[serde(default)]
    pub source_label: String,
    /// 来源账号 uid（可能为空：多账号混装时取第一个）。
    #[serde(default)]
    pub source_uid: Option<String>,
    /// 会话清单。
    #[serde(default)]
    pub sessions: Vec<ManifestSession>,
    /// 涉及的工作空间路径（导入侧用来提示「哪些路径本机没有」）。
    #[serde(default)]
    pub workspaces: Vec<String>,
    /// 包里包含的表（仅 Trae 有意义）。
    #[serde(default)]
    pub tables: Vec<String>,
    /// 产品类型：`trae` / `workbuddy`。
    ///
    /// ⚠️ 必须显式区分：两种包的内部结构完全不同，
    /// 把 WB 的包喂给 Trae 导入会得到一句「包里缺少 pack.db」，
    /// 那种错误提示对用户毫无帮助。
    #[serde(default)]
    pub kind: String,
}

/// 清单里的单个会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSession {
    pub session_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub messages: i64,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub workspace_path: String,
    #[serde(default)]
    pub updated: String,
    /// 来源账号 uid（Trae 用；WorkBuddy 的会话本来就带 `user_id`）。
    #[serde(default)]
    pub owner_uid: String,
    /// 是否有正文。`false` 的会话导进去也是空壳，导入侧可以据此提示。
    #[serde(default)]
    pub has_body: bool,
}

/// 把 zip 解到临时目录，返回目录路径（调用方负责删）。
///
/// `tag` 只用于临时目录命名，便于排查「上次导入残留」。
pub fn unpack_to_temp(zip_path: &str, tag: &str) -> Result<PathBuf, String> {
    let src = Path::new(zip_path);
    if !src.is_file() {
        return Err(format!("找不到会话包：{zip_path}"));
    }
    let dir = std::env::temp_dir().join(format!(
        "twinswitch-{tag}-{}-{}",
        std::process::id(),
        chrono::Local::now().timestamp_millis()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;

    let file = std::fs::File::open(src).map_err(|e| format!("打开会话包失败: {e}"))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("不是有效的 zip：{e}"))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("读取 zip 条目失败: {e}"))?;
        // ⚠️ zip slip 防护：`enclosed_name` 会拒掉 `..`、绝对路径、盘符前缀。
        let Some(rel) = entry.enclosed_name() else {
            return Err(format!(
                "会话包里有一个非法路径（试图写到解压目录之外）：{}",
                entry.name()
            ));
        };
        if entry.is_dir() {
            std::fs::create_dir_all(dir.join(&rel)).map_err(|e| format!("创建目录失败: {e}"))?;
            continue;
        }
        let out = dir.join(&rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buf)
            .map_err(|e| format!("解压 {} 失败: {e}", entry.name()))?;
        std::fs::write(&out, &buf).map_err(|e| format!("写入 {} 失败: {e}", out.display()))?;
    }
    Ok(dir)
}

/// 读 manifest 并校验格式/版本/产品类型。
///
/// `expect_kind`：`trae` 或 `workbuddy`。包里没写 `kind` 时按旧包处理（放过），
/// 但写了且不匹配就明确拒绝 —— 比让用户看到「缺少 pack.db」强得多。
pub fn read_manifest(
    dir: &Path,
    expect_format: &str,
    max_version: u32,
) -> Result<Manifest, String> {
    let p = dir.join("manifest.json");
    if !p.is_file() {
        return Err("会话包里缺少 manifest.json（不是有效的会话包）".into());
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("读取 manifest 失败: {e}"))?;
    let m: Manifest = serde_json::from_str(&text).map_err(|e| format!("manifest 格式不对: {e}"))?;
    if m.format != expect_format {
        return Err(format!(
            "这不是 TwinSwitch 的会话包（format = {}）",
            if m.format.is_empty() { "空" } else { m.format.as_str() }
        ));
    }
    if m.format_version == 0 || m.format_version > max_version {
        return Err(format!(
            "会话包版本 {} 高于当前程序支持的 {}，请先升级 TwinSwitch",
            m.format_version, max_version
        ));
    }
    Ok(m)
}

/// 校验包属于指定产品（`trae` / `workbuddy`）。
pub fn ensure_kind(m: &Manifest, expect: &str) -> Result<(), String> {
    if m.kind.is_empty() {
        // 旧包没这个字段：放过（由后续「缺少 pack.db / 正文」来兜底）。
        return Ok(());
    }
    if m.kind != expect {
        let name = |k: &str| -> String {
            match k {
                "trae" => "Trae".to_string(),
                "workbuddy" => "WorkBuddy".to_string(),
                other => other.to_string(),
            }
        };
        return Err(format!(
            "这是{}的会话包，不能导入到{}。请到对应的页签操作。",
            name(&m.kind),
            name(expect)
        ));
    }
    Ok(())
}

/// manifest 转成前端好用的 JSON（字段名保持 camelCase）。
pub fn manifest_json(m: &Manifest) -> Value {
    serde_json::to_value(m).unwrap_or(Value::Null)
}
