# TwinSwitch · 双栖

TwinSwitch 是一个 Windows 桌面管理台（Tauri 2 + React + Rust），把 **Trae（国内版）** 与 **WorkBuddy** 两个 AI 客户端的账号、会话与本机数据放在同一处打理：多账号切换、会话记录解密 / 导入 / 导出 / 删除、两个客户端之间的会话互迁，以及本机缓存与残留的清理。

> 「双栖」= 一个工具同时栖息在两个客户端上。

<p align="center">
  <img src="public/icon.png" alt="TwinSwitch 图标" width="128" />
</p>

## 功能

| 模块 | 说明 |
| --- | --- |
| 首页 · 本机概览 | 同步统计两个客户端的安装数 / 运行数、会话数、本地占用，并给出「可回收空间」建议（缓存、备份批次、临时解密快照、空会话等）与快捷入口 |
| Trae 账号管理 | 网页凭证登录（OAuth 回环）、一键切换（含守护回滚）、重命名 / 导出 / 导入 / 删除；展开可看该账号的**积分**与全部积分包明细 |
| Trae 会话记录 | 从客户端进程内存提取 SQLCipher 密钥并解密本地库 → 查看、导出 MD / ZIP、导入到指定账号、彻底删除（删除前自动整库备份） |
| WorkBuddy 账号管理 | 账号卡片网格：切换 / 改名 / 删除 + 积分；账号来源为本机客户端登录态与历史遥测日志的解析结果 |
| WorkBuddy 会话记录 | 浏览本机 WorkBuddy 的明文会话记录（`projects/<工作区>/*.jsonl`） |
| WorkBuddy → Trae | 读取 WorkBuddy 明文会话（JSONL），转换为 Trae 的关系库结构后加密写入指定账号的本地库——包含提问、最终回答，以及思考与工具调用过程 |
| Trae → WorkBuddy | 反向导出：把 Trae 会话写成本机 WorkBuddy 的明文 JSONL 记录 |
| WorkBuddy 本机清理 | 清掉 WorkBuddy 数据目录里的残留：「无正文」会话、删不干净的已删会话、孤儿正文与陈旧快照、诊断日志。清理前先退出 WorkBuddy 并整份备份数据库 |
| Trae 本机清理 | 回收本机上跟 Trae 有关的空间：Trae 库里的会话、本工具的整库备份与解密快照等 |
| 自动更新 | 启动后自动检查 GitHub Releases，发现新版本可在应用内下载并重启安装；更新包带 minisign 签名校验，签名不符一律拒绝 |

## 使用教程

### 1. 安装

从 [Releases](https://github.com/bean0283/twin-switch/releases) 下载 `_x64-setup.exe`（NSIS 版，推荐）或 MSI 版，双击安装后从开始菜单 / 桌面快捷方式启动。

> 关闭窗口不会退出，程序缩到右下角托盘；托盘菜单可「显示主窗口」或「退出」。

### 2. 账号管理

进入「Trae 账号管理」或「WorkBuddy 账号管理」页。两个页面布局一致：**一个账号一张卡**，卡上有三个动作。

- **网页登录**：点击「登录 Trae 账号」，按提示发起网页登录，浏览器完成授权后账号自动入库；
- **切换**：应用会退出客户端 → 还原目标账号载体 → 重启客户端，期间显示实时进度；失败自动回滚到原账号并重新拉起；
- **改名 / 删除**：卡片右上角图标按钮；
- **积分**：卡片下半部分是积分块，顶部工具条可「刷新积分」或展开看该账号的全部积分包。

> 注意：凭据若是 WorkBuddy 的**信封加密**形式（`{"$wbEncrypted":…}`），本地解不出明文，任何接口都只会 401，界面会直接提示而不是反复重试。

### 3. 会话记录

进入「Trae 会话记录」页：

1. **扫描密钥并解密**：点击「扫描密钥并解密」，程序扫描 Trae 进程内存 / 本地存储，验证密钥后把加密会话库解密到 `~/.twin-switch/trae/decrypted/`；
2. **浏览会话**：按账号筛选会话列表，点击进入会话详情查看消息；
3. **导入**：把其它账号 / 客户端的已解密会话导入到目标账号（同库复制，云端归属目标账号）；
4. **删除**：勾选会话后彻底删除——先备份整库，再对实时加密库删行并加密回写。

> 提示：Trae 重启后本地密钥可能变化，重新解密前请再次「扫描密钥并解密」。

### 4. 两个客户端之间迁移会话

- **WorkBuddy → Trae**（`WorkBuddy → Trae` 页）：选择本机会话 → 选目标账号 → 导入。流程为：退出目标客户端 → 解密目标库 → 合并 WAL 已提交帧 → 转换并写入 → 加密回写 → 备份 + 原子替换 → 自检 → 自动重启客户端。
- **Trae → WorkBuddy**（`Trae → WorkBuddy` 页）：把 Trae 会话写成本机 WorkBuddy 的明文 `projects/<工作区>/<会话>.jsonl`，并登记进 `workbuddy.db`。

转换会把每个回合映射为 Trae 的 `chat_message` / `chat_message_general` / `chat_message_task` / `history_v2`，并补齐 `session_project`、`chat_turn`、`agent_run` 三张关联表（缺行会导致会话打开后无法继续对话）。**源侧只读，不会被改动。**

### 5. 本机清理

进入「WorkBuddy 本机清理」或「Trae 本机清理」页：

1. 页面打开时**直接用上次扫描的结果**（毫秒级），不会每次都重扫；数据过旧时页头会用琥珀色提示「数据为 N 小时前，点『重新扫描』更新」；
2. 想拿最新盘面就点「重新扫描」；启动应用时首页也会在后台顺手扫一次；
3. 勾选要清理的项 → 「清理到回收站」（默认，同盘 `rename` 近乎零成本）或显式勾「彻底删除」。

清理完成后会自动重扫一次，界面立刻反映新的盘面。

### 6. 自动更新

侧栏左下角（版本号那一行）就是更新入口：

1. **自动检查**：启动 15 秒后在后台查一次 GitHub Releases，之后每 30 分钟一次。检查走的是 release 资产里的 `latest.json`，**不消耗 GitHub API 配额**，也不会打扰正在干活的应用；
2. **发现有新版**：版本号下方会出现「发现 vX.Y.Z，点此升级」，同时弹一条通知。点开对话框即可「下载更新」；
3. **下载完成**：包先存在内存里**不自动安装** —— 点「重启并安装」才替换文件并重启（Windows 安装器要求应用先退出，自动安装等于强制重启）；
4. **想手动查**：点版本号那行右边的刷新图标，走强制刷新，跳过 6 小时缓存。

> 更新包来自本仓库的 Releases，安装前会校验 minisign 签名：**签名对不上的包会被直接拒绝**。
> 网络连不上 GitHub 时，可以在 `~/.twin-switch/update.json` 里写 `{"proxy": "http://127.0.0.1:7890"}` 让检查与下载都走代理。

## 数据目录

所有本工具自己的资产都在 `~/.twin-switch/`：

| 路径 | 内容 |
| --- | --- |
| `~/.twin-switch/` | 应用状态：账号库、已保存凭据、回收站、备份 |
| `~/.twin-switch/trae/decrypted/` | Trae 解密库快照（`*.db` + `.meta.json`） |
| `~/.twin-switch/backup/` | 整库备份 |
| `~/.twin-switch/deleted_sessions/` | 删除的会话归档 |
| `~/.twin-switch/trash/` | 清理时移入的回收站 |
| `~/.twin-switch/cache/` | **纯派生缓存**（总览 `overview.json`、积分），可整目录删除，删掉只是变慢 |
| `~/.twin-switch/update.json` | 可选的更新配置，目前只认 `proxy`（GitHub 直连不通时用） |

可用环境变量 `TWIN_SWITCH_HOME` 覆盖家目录（测试沙箱 / 自定义部署）。

## 开发

```bash
npm install        # 安装前端依赖
npm run tauri dev  # 本地开发（vite + cargo）
```

打包安装包（NSIS / MSI）：

```bash
npm run tauri build
```

> ⚠️ 上面这条只在**配好签名密钥**后能跑通：`bundle.createUpdaterArtifacts` 已开启，
> 没有密钥会在打包阶段直接失败。日常本地打包请用下面 `npm run build:signed`。

测试与静态检查：

```bash
cargo test --workspace
npx tsc --noEmit
```

> 注意：`cargo check` / `cargo test` / `vite build` **不要并行**——它们共享 `target/` 与 `dist/`，会互相锁或读到半截产物。

### 打包与发布

Tauri 的更新机制要求每个更新包都有 **minisign 签名**，客户端只认 `tauri.conf.json` 里那把公钥：

```bash
# 一次性：生成密钥对。私钥千万别进版本库（.gitignore 已挡）。
npx tauri signer generate -w ~/.twin-switch-keys/twin-switch-updater.key -p <口令>

# 只打包（本地验证）：带签名构建 + 生成 latest.json，不碰 GitHub
npm run build:signed

# 完整发布：构建 → 生成 latest.json → 发布 Release → 上传更新包 / .sig / latest.json
npm run release
```

私钥默认从 `~/.twin-switch-keys/` 读（可用 `TWIN_SWITCH_KEY_DIR` 指定别处）；
发布令牌优先读环境变量 `GITHUB_TOKEN`，没有就向系统凭据管理器要。

> ⚠️ **私钥丢了就再也签不出老客户端认得的包**，只能让用户手动重装。请把它纳入备份。
>
> 每次发版都必须把 `latest.json` 传成 **release 资产**：客户端查版本不看 GitHub API（有 60 次/小时/IP 限流），只认 `releases/latest/download/latest.json`。少传这一份，老版本就永远看不到新版。

## 支持范围

| 客户端 | 账号管理 | 账号切换 | 积分查看 | 会话浏览 | 会话解密 | 会话导入 | 会话删除 | 本机清理 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Trae（国内版） | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| WorkBuddy | ✅ | ✅ | ✅ | ✅（明文） | —（本身明文） | ✅ 互迁 | ✅ | ✅ |

## 致谢

本项目在实现过程中参考了以下开源项目：

- [changexbc/workbuddy-switch](https://github.com/changexbc/workbuddy-switch) —— 账号载体合成与切换流程
- [yiyiqd/trae-session-export](https://github.com/yiyiqd/trae-session-export) —— Trae 本地会话库解密方案

## 许可协议

[MIT License](LICENSE)（Copyright © 2026 TwinSwitch）
