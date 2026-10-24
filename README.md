# TwinSwitch · 双栖

TwinSwitch 是一个 Windows 桌面管理台（Tauri 2 + React + Rust），把 **Trae（国内版 / 国际版）**、**WorkBuddy**、**Qoder（国内 / 国际）** 三个 AI 客户端的账号、会话、本机数据放在同一处打理，并把它们的**积分接成一个本地 OpenAI 兼容网关**——填一个地址、一个模型名，就能让 A 客户端的积分去跑 B 客户端的活。

> 「双栖」= 一个工具同时栖息在三家客户端上。

<p align="center">
  <img src="public/icon.png" alt="TwinSwitch 图标" width="128" />
</p>

## 功能

侧栏是**两级导航**：一级只有 5 项，账号 / 会话 / 迁移 / 清理按产品收进分组，避免入口拉成一长条。

| 模块 | 说明 |
| --- | --- |
| 首页 · 本机概览 | 三个客户端的安装数 / 运行数、会话数、本地占用与「可回收空间」建议；顶部有**一键签到**与积分合计 |
| Trae 账号 | 网页凭证登录（OAuth 回环）、一键切换（含守护回滚）、重命名 / 导出 / 导入 / 删除；可看积分与全部积分包明细 |
| Trae 会话 | 从客户端进程内存提取 SQLCipher 密钥并解密本地库 → 浏览、逐回合编辑、导出 MD / ZIP、导入到指定账号、彻底删除（删除前整库备份） |
| WorkBuddy 账号 | 账号卡片网格：切换 / 改名 / 删除 / 积分；支持**手动粘贴 Token 添加账号**（不走授权、不消耗 re 额度） |
| WorkBuddy 会话 | 浏览本机明文会话（`projects/<工作区>/*.jsonl`），逐回合编辑与删除 |
| Qoder 账号 | **国内 / 国际两个区域分家**：本机登录态导入、账号包导入导出、切换（写回 `auth.v1.dat`）、积分与额度、**每日签到 / 活动领奖**、Cosy 设备身份 |
| Qoder 会话 | 浏览 `main.sqlite` 里的会话，**逐条编辑正文 / 删除单条消息 / 删除整条会话**；删除会真的从库里删掉并自动重启 Qoder 客户端 |
| 签到 | 首页「一键签到」跑全部；各产品页也有各自的签到按钮（Trae / WorkBuddy / Qoder） |
| 积分共享（模型网关） | 本地 OpenAI 兼容网关（`127.0.0.1:57892`）：三家的积分合成一个模型池，客户端填一个地址就能用；带独立「使用说明」页 |
| WorkBuddy → Trae | 读取 WorkBuddy 明文会话（JSONL），转换成 Trae 的关系库结构后加密写入指定账号 |
| Trae → WorkBuddy | 反向导出：把 Trae 会话写成本机 WorkBuddy 的明文 JSONL 记录 |
| WorkBuddy / Trae / Qoder 清理 | 清缓存、无正文会话、孤儿正文、陈旧快照、备份批次；**先进回收站**，可逐项恢复或彻底删除 |
| 自动更新 | 启动后自动检查 GitHub Releases，应用内下载并重启安装；更新包带 minisign 签名校验 |

## 使用教程

### 1. 安装

从 [Releases](https://github.com/bean0283/twin-switch/releases) 下载 `_x64-setup.exe`（NSIS 版，推荐）或 MSI 版，双击安装。

> 关闭窗口不会退出，程序缩到右下角托盘；托盘菜单可「显示主窗口」或「退出」。

### 2. 账号管理

进入「账号及会话管理」→ 选具体产品。三个产品**同一个版式**：一个账号一张卡，卡上是切换 / 改名 / 删除，卡内是积分与签到。

- **网页登录 / 设备码登录**：走官方授权，浏览器或客户端完成确认后账号自动入库（这一条会消耗平台的 re 额度）；
- **手动添加 Token**（WorkBuddy）：直接把 `access_token` 粘进来，**不走授权、不消耗额度**；
  - `refresh_token` **强烈建议一起填**：只填 access 时，这枚 token 一旦在别处登录或被客户端刷新就会被服务端判失效，而本工具没有东西可以续期；
  - 留空时会去本机登录态里找**同账号、同登录会话**的 refresh 自动补；补不到就是一次性的；
  - 域名留空 = 按 token 自己的签发方自动判断（`codebuddy.cn` / `workbuddy.cn`）。
- **切换**：应用退出客户端 → 还原目标账号载体 → 重启客户端，期间显示实时进度；失败自动回滚到原账号并重新拉起；
- **Qoder 区域**：国内与国际是两套账号库、两套接口，页面顶部切换区域，互不串号。
- **签到**：Trae / WorkBuddy / Qoder（含国际版活动）都由同一套入口发起；「已签」与「未签」是两条不同的代码路径，界面照实显示，不会把失败画成 0。

> 注意：凭据若是 WorkBuddy 的**信封加密**形式（`{"$wbEncrypted":…}`），本地解不出明文，任何接口都只会 401，界面会直接说明而不是反复重试。

### 3. 会话管理

- **Trae 会话**：先「扫描密钥并解密」（扫描进程内存 / 本地存储，验证后解密到 `~/.twin-switch/trae/decrypted/`），再浏览 / 编辑 / 导出 / 导入 / 删除。Trae 重启后本地密钥可能变化，重新解密前请再扫一次。
- **WorkBuddy 会话**：明文 JSONL，直接读写；编辑按「回合索引 + 预览」取，只回传改动的行，未改动内容逐字节原样带回。
- **Qoder 会话**：读 `main.sqlite`（`chat_sessions` / `chat_session_messages`）。写库前会先关掉对应区域的客户端、备份到 `~/.twin-switch/qoder/backups/<区域>/`（每区留 5 份）、`wal_checkpoint(truncate)`，删完复查；**删完会自动把 Qoder 重新拉起来**。

### 4. 两个客户端之间迁移会话

- **WorkBuddy → Trae**：选会话 → 选目标账号 → 导入。流程为：退出目标客户端 → 解密目标库 → 合并 WAL 已提交帧 → 转换写入 → 加密回写 → 备份 + 原子替换 → 自检 → 自动重启客户端。
- **Trae → WorkBuddy**：把 Trae 会话写成本机 WorkBuddy 的明文 `projects/<工作区>/<会话>.jsonl`，并登记进 `workbuddy.db`。

转换会把每个回合映射到 Trae 的 `chat_message` / `chat_message_general` / `chat_message_task` / `history_v2`，并补齐 `session_project`、`chat_turn`、`agent_run` 三张关联表（缺行会导致会话打开后无法继续对话）。**源侧只读，不会被改动。**

### 5. 本机清理

进入「数据迁移及维护」→ 选 Trae / WorkBuddy / Qoder 清理：

1. 页面打开时**直接用上次扫描的结果**（毫秒级），数据过旧时页头会提示「N 小时前，点『重新扫描』更新」；
2. 勾选要清理的项 → 「清理到回收站」（默认，同盘 `rename` 近乎零成本）或显式勾「彻底删除」；
3. 回收站单独占一行并计入总可回收空间，**不会被重复算进每个分类**；「清空回收站」才是真删。

`main.sqlite`、`auth.v1.dat`、`Local State` 这类登录态与主库**永远不会出现在清理清单里**。

### 6. 积分共享（把一个地址当三种积分用）

进入「积分共享」页。这是本工具最不直观的一块，建议第一次先点右上角**「使用说明」**——那一页打开就会跑一次真机自检（网关是否在听、Key 是否可用、模型是否能列出），通过后给三步接入指引。

**它做了什么**：在本机起一个 OpenAI 兼容网关（默认 `127.0.0.1:57892`，只绑回环，不做登录），把登录在这个工具里的账号积分当作上游。客户端只要支持「自定义 OpenAI 兼容接口」，就能用你的 Trae / WorkBuddy / Qoder 积分。

**接入**

| 客户端类型 | 填什么 |
| --- | --- |
| WorkBuddy 这类「接口地址」要填完整端点 | `http://127.0.0.1:57892/v1/chat/completions` |
| 其他 OpenAI 兼容客户端（Trae 等）填 Base URL | `http://127.0.0.1:57892/v1` |

API Key 在页面里新建，形如 `sk-ts-…`（库里只存 sha256 与前后 4 位）。Key 可以限定**只能花哪几家的积分**（`trae` / `wb` / `qoder`，国际版为 `trae-g` / `wb-g` / `qoder-g`）。吊销不等于删除：吊销只是停用，记录还在。

**模型怎么选**

- 模型清单优先读本地的 `~/.twin-switch/models.json`（新电脑上没有就自动创建），空闲时后台刷新；识别不出时点「重新识别」；
- **通用模型** = 多家都提供的模型，单独一个方框。点模型名或右侧复制图标 = 复制**裸名**（如 `glm-5`），网关会在所有提供它的产品里**自动选号 —— 优先花最早到期的那份积分**；
- **专有模型** = 只有一家提供，各自一个方框，复制出来直接就是裸名；
- 想让某个通用模型**只花某一家的积分**：点行内的 `Trae` / `WB` / `Qoder` 徽章，复制出来是带前缀的形式（如 `wb:glm-5`），**前缀会锁死那一家**，不会再被「谁更早到期」带走。

**账号池**在页面下方：按积分到期紧迫度排序，已失效（401）的账号直接标成不可用并给出原因，不会让你切过去才发现。页面顶部有**总开关**，关掉会真的停掉网关。

### 7. 自动更新

侧栏左下角（版本号那一行）就是更新入口：

1. 启动 15 秒后后台查一次 GitHub Releases，之后每 30 分钟一次；查的是 release 资产里的 `latest.json`，**不消耗 GitHub API 配额**；
2. 发现新版 → 显示「发现 vX.Y.Z，点此升级」并弹通知；
3. 下载完成**不自动安装**，点「重启并安装」才替换文件并重启；
4. 点刷新图标走强制刷新，跳过 6 小时缓存。

> 安装前会校验 minisign 签名，**签名对不上的包会被直接拒绝**。
> 连不上 GitHub 时，可在 `~/.twin-switch/update.json` 里写 `{"proxy": "http://127.0.0.1:7890"}`。

## 数据目录

| 路径 | 内容 |
| --- | --- |
| `~/.twin-switch/workbuddy-accounts.json` | WorkBuddy 账号库（唯一写出口） |
| `~/.twin-switch/trae/decrypted/` | Trae 解密库快照（`*.db` + `.meta.json`） |
| `~/.twin-switch/backup/`、`~/.twin-switch/backups/` | 整库备份 / 备份批次 |
| `~/.twin-switch/deleted_sessions/` | 删除的会话归档 |
| `~/.twin-switch/trash/` | 清理时移入的回收站 |
| `~/.twin-switch/qoder/accounts.{cn,global}.json` | Qoder 两区域账号库 |
| `~/.twin-switch/qoder/backups/<区域>/` | Qoder 写库前备份（每区留 5 份） |
| `~/.twin-switch/qoder/trash/` | Qoder 清理回收站 |
| `~/.twin-switch/models.json` | 积分共享的模型清单（可删，会自动重建） |
| `~/.twin-switch/gateway-keys.json` | 网关 Key（只存哈希与前后 4 位） |
| `~/.twin-switch/gateway-http.log` | 网关请求日志 |
| `~/.twin-switch/cache/` | **纯派生缓存**（总览、积分），可整目录删，删掉只是变慢 |
| `~/.twin-switch/settings.json` | 设置（写库前备份、切号同步定时任务等） |

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

> ⚠️ 上面这条只在**配好签名密钥**后能跑通：`bundle.createUpdaterArtifacts` 已开启，没有密钥会在打包阶段直接失败。日常本地打包请用 `npm run build:signed`。

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

| 客户端 | 账号管理 | 账号切换 | 积分查看 | 签到 | 会话浏览 | 会话编辑 | 会话删除 | 本机清理 | 积分接入网关 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Trae（国内） | ✅ | ✅ | ✅ | ✅ | ✅（需解密） | ✅ | ✅ | ✅ | ✅ |
| Trae（国际） | ✅ | ✅ | ✅ | ✅ | ✅（需解密） | ✅ | ✅ | ✅ | ✅ |
| WorkBuddy | ✅ | ✅ | ✅ | ✅ | ✅（明文） | ✅ | ✅ | ✅ | ✅ |
| Qoder（国内） | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Qoder（国际） | ✅ | ✅ | ✅ | ✅ 活动制 | ✅ | ✅ | ✅ | ✅ | ✅ |

会话在 WorkBuddy 与 Trae 之间可以互迁；Qoder 的会话结构是第三套模型，只在本产品内编辑 / 删除。

## 致谢

本项目在实现过程中参考了以下开源项目：

- [changexbc/workbuddy-switch](https://github.com/changexbc/workbuddy-switch) —— 账号载体合成与切换流程
- [yiyiqd/trae-session-export](https://github.com/yiyiqd/trae-session-export) —— Trae 本地会话库解密方案
- [NextAgentX/qoder-trae-workbuddy-switch](https://github.com/NextAgentX/qoder-trae-workbuddy-switch) —— Qoder 区域划分、CoSy 签名与签到活动流程

## 许可协议

[MIT License](LICENSE)（Copyright © 2026 TwinSwitch）
