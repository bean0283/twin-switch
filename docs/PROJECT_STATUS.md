# TwinSwitch · 双栖 —— 项目进度盘点与续接基础

> ⚠️ **更名说明（v0.0.24，2026-10-06）**：本项目原名 **trae-switch-cn**。因为功能早已从
> 「Trae 账号切换器」长成「**Trae + WorkBuddy 双客户端管理台**」，v0.0.24 起正名为
> **TwinSwitch（双栖）**，并做了彻底的标识迁移（数据目录 `~/.twin-switch`、`localStorage` 键前缀、
> 环境变量、logo 与图标资产）。详情见 `docs/任务书.md` 的 **T23**。
>
> **下文凡出现 `trae-switch-cn` 之处，多为当时的实况记录，刻意保留不改**（例如 GitHub 仓库名
> 与发布产物名仍是旧的）；涉及**当前**路径与版本的行，已在行内标注或更新。

> 分析时间：2026-10-04（最近更新：2026-10-06 / **v0.2.0**）
> 分析依据：`D:\htw\签到\trae-session_这份是之前执行的记录…_6abf7aed_1.md`（28161 行 / 1.46 MB / 73 轮交互）
> 核验方式：除通读记录外，另对 **git 历史、工作区、版本一致性、Rust 编译、GitHub Release** 做了实际核查，下文凡标 ✅ 者为本次实检结论，非照抄记录。
> 当前进度以 **第五节「本轮新闭环」** 与 `docs/任务书.md` 的任务识别表为准。

---

## 一、项目是什么（一句话）

**TwinSwitch · 双栖**（原名 trae-switch-cn）是一个 Windows 桌面管理台（Tauri 2 + React + Rust），把 **Trae（国内版）** 与 **WorkBuddy** 两个 AI 客户端的账号、会话与本机数据放在同一处打理：

1. **多账号切换** —— 一键冷切换登录账号，失败自动回滚（Trae / WorkBuddy 双侧）；
2. **会话记录保留与互通** —— 解密 Trae 本地加密会话库，浏览 / 导入到其他账号 / 导出 / 彻底删除；并与 WorkBuddy 的明文会话**双向迁移**；
3. **本机数据清理** —— 扫描两个客户端的缓存与残留，移入回收站或彻底删除；
4. **记忆交接** —— 切换前把当前进度写成换账号后仍可读的交接记忆。

背景：Trae 多账号只能反复退出重登，且会话记录随账号切换而不可见。本项目沿袭 `workbuddy-switch`（CodeBuddy 版）的思路起步，为 Trae 国内版补齐该能力，随后扩展为双客户端管理台，并把「两端会话互迁 + 本机清理」纳入同一套界面。

| 项 | 值 |
| --- | --- |
| 产物名 | **TwinSwitch**（v0.0.24 起；Rust 包名仍为 `wb-switch-rust` / `wb-switch-core`） |
| 仓库 | https://github.com/bean0283/twin-switch（**主项目** ✅ 公开，单一根提交 `4f7e00b`「首次提交」；`v0.1.0` / `v0.2.0` 两个 tag 均指向它）· 旧仓库 https://github.com/bean0283/trae-switch-cn（**保持原样不动**，停在 v0.0.3；用户明确决定**不归档**，仍可提交/开 issue） |
| 技术栈 | Tauri 2（Rust）+ React 19 + TypeScript + Vite + Tailwind/Radix |
| 核心逻辑位置 | `crates/wb-switch-core/src/modules/`（31 个模块，约 13k 行，纯 Rust、不依赖 Tauri） |
| 前端 | `src/main.tsx`（**启动编排**：先 `primeOverview()` 读磁盘缓存 → 挂载 → 后台 `refreshOverview()`）· `src/pages/HomePage.tsx`（侧栏「首页」·本机概览仪表盘，数据走 `src/lib/overview-store.ts` 的进程内单例）、`src/pages/TraeSwitchPage.tsx` 与 `src/pages/TraeRecordsPage.tsx`（侧栏「Trae」区的账号管理与会话记录）、`src/pages/WorkbuddyImportPage.tsx` 与 `src/pages/WorkbuddyExportPage.tsx`（侧栏「数据迁移」区的双向会话迁移）、`src/pages/WorkbuddyCleanupPage.tsx` 与 `src/pages/TraeCleanupPage.tsx`（侧栏「本机维护」区的残留清理：WorkBuddy 侧 / Trae 侧）、`src/pages/WorkbuddySwitchPage.tsx` 与 `src/pages/WorkbuddyRecordsPage.tsx`（侧栏「WorkBuddy」区的账号管理与会话记录 / 复制 / 关联，账号卡内嵌积分块）。两个账号页共用 `src/components/credits-ui.tsx` 与 `src/components/account-card.tsx` 的展示层 |
| 派生缓存 | `~/.twin-switch/cache/`（**可整目录删**）：`overview.json`（总览 + 可回收空间）、`credits.json` / `trae-credits.json`（积分结果）、清理扫描的两份槽位 |
| 任务台账 | `docs/任务书.md`（后续 AI 先读它，再读本文件） |
| 当前版本 | **0.2.0** ✅（四处文件 + 一处兜底串：`package.json` / `src-tauri/Cargo.toml` / `src-tauri/tauri.conf.json` / `crates/wb-switch-core/Cargo.toml` / `src/components/update-entry.tsx`） |
| 自动更新 | ✅ v0.2.0 起对接 GitHub Releases（`tauri-plugin-updater` + `latest.json`）。实现见 `crates/wb-switch-core/src/modules/update.rs`（版本比较/端点/缓存）、`src-tauri/src/update_service.rs`（状态机）、`src/components/update-entry.tsx`（侧栏入口+对话框）；发版一键脚本 `npm run release` → `scripts/release.mjs` |

---

## 二、执行进度盘点：73 轮实际干了什么

记录里的 73 轮可归为 **三个阶段**。

### 阶段一：基础项目单目标化改造（记录第 1 ~ 3067 行，Milestone M1–M6）

延续的是一个**更早期**的 `workbuddy-switch` 项目（CodeBuddy 多账号切换，国际版），任务是把它以「国内版单客户端」形态重做。

| 里程碑 | 内容 | 状态 |
| --- | --- | --- |
| M1 / M2 / M3 | 单目标化（仅保留 WorkBuddy 客户端）、删除 codex/codeg 残留、测试重写 | ✅ 完成 |
| M4 | 悬浮窗裁剪（`integration_folder.rs` 写入中断） | ⚠️ 中断未收尾 |
| M5 / M6 | 残留清理（删除 npm 发布链路、`codebuddy-cli-helper`、update 脚本；`rate_limit_hook` 收敛；README 重写为国内版） | ✅ 完成 |

本阶段收尾于一个用户报障：**「安装后双击运行没有反应」**——排查结论为单实例锁把已运行实例唤醒致前台、应用本身渲染正常，端到端验证通过。

> ⚠️ **重要风险**：此阶段的工程目录 `D:\htw\签到\workbuddy-switch-cn` **当前已不存在**（本次实检确认 `D:\htw\签到` 下只剩 `trae-switch-cn`）。而 `trae-switch-cn` 的 git 历史只有 9 个提交（v0.0.1 首发起），**阶段一的成果并不在这份 git 历史里**。这部分连续性只能靠会话记录文本维系，建议后续明确该目录的去向。

### 阶段二：移植 Trae 能力（记录第 3068 行起，本项目的主体）

用户原始指令是「在 workbuddy-switch-cn 里添加 trae 记录导出器和账号切换功能」，随迭代逐步演化为独立项目。

关键参考源（路线）：
- `trae-session-export-main`（Python 参考实现）→ 提供 Km 编解码、SQLCipher 4 页面级解密、内存提钥、MD 导出
- `D:\htw\签到\trae-switch`（Node.js 参考实现）→ 提供冷切换编排、载体备份/还原、守护回滚

本阶段完成的能力矩阵：

| 能力 | 实现模块 | 说明 |
| --- | --- | --- |
| 客户端发现 | `trae_discover` / `trae_carriers` | 识别 Trae CN / TRAE SOLO CN 等，定位 `storage.json`、`database.db` |
| 内存提密钥 | `trae_memory_scan` / `scan_key` | 从客户端进程内存提取并 HMAC 验证 32 字节密钥 |
| 会话解密 | `trae_decrypt` | SQLCipher 4：AES-256-CBC + HMAC-SHA512，reserve=80，page 4096 |
| 冷切换 | `trae_switch` | 杀进程 → 还原目标载体 → 拉起 → 轮询 uid 校验，失败自动回滚 |
| 账号档案 | `trae_vault` / `trae_oauth` | 网页凭证登录（OAuth 回环）、备份/导入/重命名/删除 |
| 会话导入 | `trae_import` | 跨账号复制会话并改写归属（新 session_id + project 指向目标 uid） |
| 会话导出 | `trae_export` | 单会话 / 批量 zip，输出 Markdown |
| 会话删除 | `trae_delete` | 整库备份 → 删行 → 加密回写 → 原子替换 → 移入回收站 |
| 记忆交接 | `trae_handoff` | 写 `<项目>/TRAE_交接记忆.md`、`.trae/rules/`、记忆库 `topics.md`、工具目录归档 |
| 账号资料 | `trae_profile` / `trae_remote` | 拉取真实昵称、积分余额（登录时取一次 + 手动刷新） |
| 兼容合成 | `trae_synth` | 与 WorkBuddy 合成模块对接（衔接阶段一成果） |

### 阶段三：v0.0.1 → v0.0.3 迭代修障与发布（记录尾部）

用户以「发现新 bug → 继续 → 打包 → 发布」的高频节奏推进，共 9 个提交：

| 提交 | 修的内容 |
| --- | --- |
| `7cfceee` | v0.0.1 首发 |
| `6f68897` / `09874b1` | 文档：MIT 许可、开发背景、教程配图 |
| `74c8e6f` | 导入目标账号按客户端过滤、补齐关联表（导入后可继续对话）、账号名自动获取 → **v0.0.2** |
| `a5ed391` | 接入账号资料接口（真实昵称/积分 + 手动刷新按钮）、修复导入目标账号误判 → v0.0.3 |
| `938b8e8` | 刷新自动重解密同步 Trae 增删、导入候选账号名与首页一致、删除后自动重启客户端、侧栏版本号、关闭隐藏到托盘 |
| `5491207` | 导出路径提示与「打开」按钮、`reveal_path`、批量导出 UTF-8 截断 panic |
| `d3dc211` | 进程名含空格（`TRAE SOLO CN.exe`）时 `taskkill`/`tasklist` 加引号 |
| `22d87c1` | 改用 `taskkill /PID` 杀进程，修复切换后客户端秒退 exit 0 ← **HEAD** |

---

## 三、当前状态核验（2026-10-04 实检，非照抄记录）

| 核验项 | 结果 |
| --- | --- |
| git 分支 / HEAD | `main` @ `22d87c1` |
| 工作区 | 干净（`git status` 无输出）✅ |
| 版本一致性 | `0.0.3` × 3 处一致 ✅ |
| Rust 编译 | `cargo check` **17.75s 通过、零错误** ✅ |
| Release v0.0.3 | 已发布、非 draft、非 prerelease ✅ |
| 发布资产 | `trae-switch-cn_0.0.3_x64-setup.exe`（4.64 MB）+ `_en-US.msi`（6.55 MB），共 **11 次下载** ✅ |
| 仓库可见性 | 已改公开 ✅ |

**结论：主线上代码与发布均已收口，v0.0.3 是一个完整可交付的状态，不是半途中断的状态。**

---

## 四、关键技术机制（务必传承，别重新踩坑）

这几条是本项目最贵的经验，代码注释里也只有部分写了。

### 4.1 解密链路
`Trae 进程内存扫描提钥` → `SQLCipher 4 页面级解密` → 明文 SQLite → rusqlite 读取。
密钥参数：32 字节密钥 / 16 字节盐 / 16 字节 IV / 64 字节 HMAC-SHA512 / reserve=80 / page_size=4096。

### 4.2 内存扫描的窗口语义（踩过最久的坑）
- **错的做法**：只按「完整 hex 段」判断（64 整段、96 段前64+后32、>96 段前64+末32），奇数段直接跳过。
- **错的原因**：Trae 内存中的真实密钥常出现在 65~95 长的 hex 段里，或被更长 hex 段包裹，整段判断会整段漏掉。参考 Python 实现用 `[0-9a-fA-F]{64}` 做**非重叠 64 窗口扫描**才能命中。
- **最终方案**：**每 1 字节推进的全滑动窗口**，从根本上消除漏检。
- 附带坑：`candidates` 计数器曾写成 `const 0` 从不累加（纯 UI 问题）；扫描器必须**限定所选客户端的进程**，否则会出现「拿了 SOLO CN 进程的内存去验证 Trae CN 的盐 → 必然 0 候选」。

### 4.3 为什么不用 rusqlite 直连加密库
本工程 rusqlite **未编译 SQLCipher 特性**，`PRAGMA key` 根本不生效，打开加密库必报 `file is not a database`——这正是「反复重新解密也没用」的根因。
**统一方案**：页面级解密 → 改数据 → 加密回写 → 备份 + 原子替换。导入与删除严格对称复用这套链路。

### 4.4 删除时必须合并 WAL
Trae 常被强制结束（`/F`），WAL 里可能残留未检查点但已提交的帧。直接替换明文副本会**丢掉其他会话的最新数据**。
做法：按 SQLite 官方源码核实的帧校验规则（帧校验覆盖帧头前 8 字节 + 整页数据），把 WAL 已提交帧解密后合并进明文副本。

### 4.5 冷切换为什么必须冷
Trae 运行中会把内存登录态回写 `storage.json` / leveldb，不关进程直接改文件会立刻被覆盖，表现为「切了但没生效」。
守护判定：拉起后轮询 `storage.json`，进程没起来 / 凭据缺失 / 落到别的 uid → 回滚重拉；`live uid === 目标 uid` → 成功。

### 4.6 进程名含空格
`TRAE SOLO CN.exe` 直接 `taskkill /IM` 不引号会被拆成三个参数。**现解**：优先 `taskkill /PID`，退路才用带引号的 `/IM`。

### 4.7 构建 feature
`custom-protocol` 必须默认开启，否则 `cargo build --release` 产出的 exe 会指向 `devUrl http://localhost:1420`，Vite 没运行时窗口报「localhost 拒绝连接」。`tauri dev` 需 `--no-default-features`。

### 4.8 相对 Trae 账号切换的账号库显示
默认显示 **Trae 真实用户名**（`trae_profile` 拉取），不是 storage.json 里的名义名；导入候选账号名与首页保持一致。

---

## 五、遗留与未决问题（续接时的第一优先级）

> **本轮（v0.0.10 / v0.0.11 / v0.0.12）新闭环**：
> ① 账号显示名（三来源合并，`workbuddy_accounts.rs`）；
> ② 同标题副本判最新（`workbuddy_source::decide_group`）；
> ③ 本机残留扫描与受控清除（`workbuddy_cleanup.rs`）；
> ④ **修复「WorkBuddy 的详细过程导入 Trae 后显示异常」**——双重编码 + `plan_item` 字段缺失，
> 详见 `docs/任务书.md` 的 **T5**；
> ⑤ **修复「工具卡片卡在进行中、详细过程为空」**——`result.status` 透传了 WorkBuddy 的
> `completed`（不在 Trae 的词表里 → 被当成 `running`），且工具输出从未写进 `result.data`；
> 详见 `docs/任务书.md` 的 **T6**；
> ⑥ 新增只读探针 `examples/wb_live.rs`（读「快照 + WAL」的**客户端真实所见**）；
> ⑦ 补齐 **71 个单元测试 + 1 个真机自检用例** → 下文 P2-6「没有任何测试」的说法**已不适用**。

> **本轮（v0.0.13）新闭环**：
> ⑧ **修复「导入后仍有记录与 WorkBuddy 不一致」**——根因是 WorkBuddy 把**客户端自己生成的注入内容
> 也写成 `role=user`**（共 4 类：`<conversation_history_summary>` / `<cb_summary>` /
> `Please continue with the conversation…` / `<task-notification…>`），导入侧一律当用户提问 →
> Trae 里凭空多出 **19 条用户消息** 与 **10 个空白助手气泡**。修复后源侧 **36 回合 → 16 回合**，
> 全部为真实提问；详见 `docs/任务书.md` 的 **T7** 与 `docs/WORKBUDDY_IMPORT_DESIGN.md` 第十六节；
> ⑨ **新增「Trae 清理」功能**（`trae_cleanup.rs` + `TraeCleanupPage.tsx`）——针对本机会话 / 记录 /
> 残留文件清理：会话（含空会话推荐）、工具残留（解密快照 / 导入中间产物 / 整库备份 / 日志）、
> 客户端残留（可再生的 `Cache` / `GPU Cache` / `Crashpad` 等 17 项 + 分区缓存）、回收站；
> 默认**移入回收站**（可勾选彻底删除），本机实测可回收 **7 339.6 MB**；详见 `docs/任务书.md` 的 **T8**；
> ⑩ 测试补齐至 **80 个单元测试**；新增只读探针 `examples/wb_diff.rs`（源侧解析 vs Trae 实际落库比对）
> 与 `examples/clean_scan.rs`（可清理项与体积清单）。
>
> **本轮（v0.0.14）新闭环**：
> ⑪ **新增「WorkBuddy 账号管理」**（侧栏新增「WorkBuddy」区）——完整对齐 Trae 侧：列出本机账号 +
> 一键切换（含备份 / 回滚）+ OAuth 扫码登录 + 导入本机登录态 + 重命名 / 删除 / 账号包导入导出。
> 提取自 `workbuddy-switch-main.zip`（全功能版 14 个模块），**只取国内版账号这一块**，
> 积分 / Token / 签到 / CLI / IDE / 插件 / 轮换 / 更新 / 悬浮窗全部跳过。
> 两个照搬会出事的坑：token 可能是 5.6 起的**加密信封**必须原样读写；`logged-out` **退出标记**
> 必须清理否则凭据齐全也当未登录。详见 `docs/任务书.md` 的 **T9**；
> ⑫ **新增「WorkBuddy 会话记录 + 复制 + 跨账号关联」**（`workbuddy_sessions.rs` +
> `WorkbuddyRecordsPage.tsx`）——按 uid 列出（26 行 / 3 个 uid）→ 详情 → 导出 MD → 删除；
> 复制 = 新 UUID + INSERT 新行 + 正文 `cid→new_cid`，**源账号一行不改**；
> 复制后自动登记关联组。**`edge-sync-mapping*.db` 坚决不写**（写入会让 edge-sync 判定「已迁移」
> 跳过上传，云端永久缺会话）。真机写路径已实测并清理回基线。详见 `docs/任务书.md` 的 **T10**；
> ⑬ 测试补齐至 **114 个单元测试**；新增探针 `examples/wb_accounts_probe.rs` 与
> `examples/wb_sessions_probe.rs`（均只读）。
>
> **本轮（v0.0.15）新闭环**（用户一次给 7 条编号需求）：
> ⑭ **修复「WorkBuddy 会话页账号名显示成 uid 片段」**——根因是该页自己拼 `uid.slice(0,8)`，
> 完全没用已有的 `workbuddy_accounts::label_for()`。本轮把 `label` 从后端 `list_for_account()` /
> `list_by_account()` 一路带到前端下拉；同时把**本工具账号库** `~/.trae-switch-cn/workbuddy-accounts.json`
> 补进解析链第 3 来源（`own_vault_entries()` 直读 `config::store_dir()`，避免与 `workbuddy_vault` 循环依赖）；
> 详见 `docs/任务书.md` 的 **T11**；
> ⑮ **新增「首页·本机概览」**（`app_overview.rs` + `HomePage.tsx`）——仪表盘 4 张统计卡
> （Trae 账号 / WorkBuddy 账号 / 会话总数 / 可回收空间）+ 双栏状态卡 + 提示与快捷入口。
> **秒开原则**：后端只做同步统计（Trae 会话数取快照 `chat_session` 行数，**不复制 312 MB 快照**），
> 几 GB 的清理扫描由前端并行另拉；详见 `docs/任务书.md` 的 **T11**；
> ⑯ **新增「WorkBuddy 积分与积分包」**（`workbuddy_credits.rs` + `workbuddy-credits.tsx`，约 1000 行 / 14 条单测）
> ——本工具账号库 + **只读借用**参考工具库（`~/.wb-switch/accounts.json`，**一个字节都不写**）合并去重
> （明文凭据优先 → 同档位本工具库优先）；三路并行取数（summary / paid-packages / free-packages）；
> 卡片统计**有效积分包**（`remaining > 0` 按 `expireAt` 升序）+ 全部积分包可调尺寸弹窗；详见 `docs/任务书.md` 的 **T12**；
> ⑰ **侧栏信息架构调整**（`App.tsx`）——新增顶层「首页」，并把 Trae 相关页收进独立的「**Trae**」分组标签，
> 与 WorkBuddy / 数据迁移 / 本机维护 四区并列；详见 `docs/任务书.md` 的 **T13**；
> ⑱ **迁移弹窗放大 + 可拖拽调节**（新增 `src/components/ui/resizable-dialog-content.tsx`）——
> 默认 `90vw × 86vh`、右下角手柄拖拽、**双击手柄复原**、`localStorage` 记忆尺寸（键前缀 `trae-switch-cn:dialog-size:`）。
> 踩坑：居中锚定会让手柄以**两倍速度**漂移，必须用「顶部固定锚点」；详见 `docs/任务书.md` 的 **T13**；
> ⑲ **Trae 解密状态收纳**——`TraeRecordsPage.tsx` 的「解密库表清单」改为 `Collapsible` 折叠，
> 收起时只显示「N 张表 · M 行」，减少页面占用；详见 `docs/任务书.md` 的 **T13**；
> ⑳ 新增探针 `examples/wb_credits_probe.rs` / `examples/wb_overview_probe.rs`（均只读）；
> `cargo test -p wb-switch-core` → **131 passed / 0 failed / 1 ignored**。
>
> **本轮（v0.0.16）新闭环**：
> ㉑ **账号管理与积分包融合成一张卡**——原 `/workbuddy-switch` 上段账号卡、下段积分网格，
> 同一账号出现两次、信息割裂。现改为「一个账号 = 一张卡」：左侧账号身份 + 积分块，
> **右侧竖排「切换 / 改名 / 删除」控件**（窄屏落到下方横排）。
> `workbuddy-credits.tsx` 由「自给自足的面板」重构为**四件套**——`useWorkbuddyCredits()` 状态 hook +
> `CreditsToolbar`（含「刷新积分」）+ `CreditBlock`（嵌进账号卡）+ `CreditsDialog`；
> 原 `WorkbuddyCreditsPanel` 导出删除。账号按 **uid 小写**匹配，失败原因走 `creditErrorByUid`；
> **只在参考工具库里、本工具账号库没有的账号**单独一段只读展示（不给切换按钮，避免点了没反应）。
> 详见 `docs/任务书.md` 的 **T14**。
>
> **本轮（v0.0.17）新闭环**：
> ㉒ **对照参考项目重做账号卡**——抽参考实现 `account-card.tsx` 的五个做法：卡片网格、哈希色调头像、
> 右上角 `size-8` 小图标操作栏、积分区的固定视觉节奏、进度条按状态分色。
> 账号卡改为 `<header>`（头像 + 名称 + 身份 + 状态 Chip + 操作栏）/ `<section>`（积分块）两段，
> 网格 `sm:grid-cols-2 xl:grid-cols-3`；操作栏三个图标按钮（切换 / 改名 / 删除），
> 当前账号的「切换」渲染成**带对勾角标的非按钮态**。邮箱改脱敏展示。
> **关键发现**：国内版积分的 `PackageName` 是**运营原文**（本机 30 多个包全叫
> 「CodeBuddy个人版国内运营裂变包」），官方客户端按**商品码**在前端映射 → 新增
> `src/lib/credit-package-names.ts`（照抄官方 `package-name-resolver.ts`，`TCACA_code_007_…`
> → 「平台奖励积分」，与参考项目效果图一致）。详见 `docs/任务书.md` 的 **T15**。
> ㉓ **启动缓存：先读缓存 → 后台刷新 → 写回缓存**——首页原在组件挂载时 `await` 总览，
> 要枚举进程（WMI）+ 统计 SQLite，期间整页骨架屏**点不动**。现新增 `~/.trae-switch-cn/cache/`
> （与存用户资产的 `store_dir()` 分开）：`overview.json` 存总览 + 可回收空间，`credits.json` 存积分。
> 后端三个入口 `cached()`（只读文件不重算）/ `snapshot()`（算完即写回）/ `save_reclaim()`；
> 前端新增 `src/lib/overview-store.ts`（`useSyncExternalStore` 单例），在 **App 根组件**挂载时
> `warmOverview()` 预热 —— 用户切到首页时数据早就热好，**首页从此没有「首次加载」状态**。
> 真机实测：落盘后回读 `ageMs=29`；重建进程读 `ageMs=20915`（20 秒前的数据），**跨进程生效**。
> 详见 `docs/任务书.md` 的 **T16**。

> **本轮（v0.0.18）新闭环**（用户反馈「界面卡了 10s 才能操作，没有解决缓存读取问题」）：
> ㉔ **重活不许压在主线程上**——真根因不是缓存，而是 **Tauri 的同步命令跑在主线程上**
> （Windows 上同时是 WebView2 的消息循环）。`trae_cleanup_scan` / `trae_wb_cleanup_scan`
> 都是 `pub fn` 同步命令，首页一挂载就并行触发，在主线程上递归统计几 GB 目录。
> **实测合计 11134 ms**（5455 + 5679），与用户说的「10 秒」几乎完全对上；而且主线程被占着时
> **已经回来的 IPC 回包也派发不出去**，所以缓存的数字读到了也画不出来。
> 修法：`commands.rs` 新增 `off_main()`（`spawn_blocking` 薄包装），**32 条碰磁盘/DB/进程的命令**
> 改成 `async` + `off_main`，只剩 12 条微秒级命令保持同步；启动编排上移到 `main.tsx`，
> **先读缓存 → 挂载 → 后台重算**（`PRIME_TIMEOUT_MS = 800` 兜底），首屏第一帧即真实数据；
> 可回收空间扫描延后 600 ms 错开开局 I/O。
> 新增**回归护栏** `mod main_thread_guard`：静态扫源码，同步命令必须在 `SYNC_ALLOWLIST` 白名单里，
> 且白名单不许留已失效条目（`wb-switch-rust` 2 passed）。
> 详见 `docs/任务书.md` 的 **T17**。

> **本轮（v0.0.19）新闭环**（用户三条需求：①按推荐项做性能优化 ②Trae 账号页对齐 WorkBuddy 并显示积分包
> ③后续不再自动备份）：
> ㉕ **进程枚举从 3.5 s 降到毫秒级**——`trae_switch` / `workbuddy_switch` / `workbuddy_export`
> 三处原本都按映像名**逐个**起 `tasklist /FI "IMAGENAME eq X.exe"`（一次冷启动 300–500 ms，
> Trae 两个客户端 + WorkBuddy 一串名字 ⇒ 3 s+）。新增 `modules/process_list.rs`：
> Windows 用 `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` **一次**拿全量 `(映像名, pid)`，
> 调用方 `find_by_names()` 内存过滤（口径与旧实现一致：**精确**匹配整名，`.exe` 可省）。
> 顺带消掉了「首页概览重算 3.51 s」这个遗留项。
> ㉖ **两条清理扫描加磁盘缓存 + 10 分钟 TTL**——`config::{write_cache_slot, read_cache_slot,
> clear_cache_json}`（文档形状 `{version, scannedAt, payload}`，`version` 一改即当无缓存）。
> `trae_cleanup::cached(force)` / `workbuddy_cleanup::cached(force)`：默认吃缓存（毫秒级返回，
> 带 `cached/scannedAt/ageMs`），`force = true` 才重扫（前端「重新扫描」）；
> **purge / empty_trash 之后自动失效缓存**。界面标注「缓存于 N 分钟前」。
> ㉗ **Trae 账号页对齐 WorkBuddy（含积分与积分包）**——Trae 原本是「当前登录态卡 + 一行一账号的
> 列表 + 一行小字积分」。现在同样是**卡片网格**：`AccountCard`（共用卡壳）+
> `TraeCreditBlock`（积分块）。数据链路新增：
> `trae_profile::parse_entitlement`（解析 `user_current_entitlement_list`：
> `usage_summary` 汇总 + `user_entitlement_pack_list` 逐个包的额度 / 已用 / 到期，
> `credits_limit` 缺失 = 不限量包，token 过期时 `code: 1001` → 「凭据已失效」）→
> 落进账号自己的 `profile.json`（`credit_packs` 等）→ 新模块 `trae_credits::{cached, query}`
> + 两条新命令 `trae_credits_cached` / `trae_credits_query`（后者 5 分钟缓存，`force` 才联网）。
> 只有 oauth（网页登录）账号有 token 能查；切换载体来的账号走「不适用」的中性说明。
> ㉘ **积分展示层抽成共用组件**——`src/components/credits-ui.tsx` 定义统一模型
> `CreditView` / `CreditPack` + `CreditBlock` / `CreditsDialog` / `CreditsSummaryBar`；
> WorkBuddy 侧 `wbItemToView()`、Trae 侧 `traeEntryToView()` 各自适配。
> 账号卡外壳抽到 `src/components/account-card.tsx`。**两侧 UI 一致由结构保证，不再靠对照抄样式**。
> ㉙ **不再自动备份**（用户明确要求）：源码备份只在用户点名时执行，详见工作区 `MEMORY.md`。
> 详见 `docs/任务书.md` 的 **T18**。

> **本轮（v0.0.20）新闭环**（用户一条反馈：「trea 的显示混乱，你都没有自己检查」+ 截图）：
> ㉚ **Trae 账号卡「姓名消失 + 徽标一字一行」的排版错乱已修**——根因是 `account-card.tsx`
> 的卡头是**不带 `flex-wrap`** 的 flex：头像 + 身份列（`flex-1 min-w-0`）+ 操作栏。
> WorkBuddy 侧 3 个图标没事，**Trae 侧 6 个图标（≈222 px）**在 300–430 px 宽的卡片里把固定项吃满，
> 而 `flex-1`（`flex:1 1 0%`）+ `min-w-0` 的身份列**基准与下限都是 0** ⇒ 被压到 0 宽：
> 姓名与 uid 被 `truncate` 裁没、中文徽标一字一行溢出、手机号整串横着盖住按钮。
> 修法：卡头 `flex-wrap` + 身份列 `grow basis-28`（7 rem 下限）+ 操作栏
> `ml-auto min-w-0 flex-wrap justify-end` ⇒ 按钮放不下就整体换行，身份列拿满整行。
> 顺带把汇总文案改成「2/3 个账号可查（1 个失败）」。
> **方法性交付**：立了「改完 UI 必须自己渲染核对」的流程（临时 `preview/` 塞真实缓存数据 +
> 真组件 → `vite` → 无头 Edge 出图 + `--dump-dom` 量宽），改前 `col=101`、改后 `col=335`；
> WorkBuddy 侧数字与改前逐项一致（无回归）。详见 `docs/任务书.md` 的 **T19**。

> **本轮（v0.0.21）新闭环**（用户一条反馈：「已经有总的积分刷新按钮，每个账号单独的积分刷新可以取消；
> 回滚按钮也单独提取出来，和 workbuddy 的界面一样」）：
> ㉛ **卡上按钮收敛 + 回滚独立成卡 + 两页容器宽度对齐**：
> ① 删掉每张卡上的「刷新该账号资料」图标 —— 顶部 `CreditsSummaryBar` 的「刷新积分」本来就是逐个账号
> 调 `trae_credits::query(force=true)` → `trae_profile::refresh_profile`（昵称 / 头像 / 手机号 / 积分包
> 一起刷），单卡那个是重复入口（后端命令 `trae_refresh_profile` 保留，只是不再挂按钮）；
> ② 回滚从卡上移出，在账号区之后做成独立的「回滚」`<Card>`，与 WorkBuddy 账号页同一位置同一外观：
> 账号下拉 + 「回滚到该账号」（`trae_switch::rollback_to ≡ switch_to`，即切回该账号；Trae 侧没有
> 「上一次切换前」的持久快照，所以不是 WorkBuddy 那种无参一键回滚）+ 「导出该账号备份」；
> 卡上最终只剩与 WorkBuddy 完全相同的三件套（切换 / 改名 / 删除）。
> ③ **隐藏推手是容器宽度**：Trae 页原为 `max-w-5xl`(1024)、WorkBuddy 页 `max-w-6xl`(1152)，
> 同样 3 列网格下每张卡内容宽差 48 px（282.7 vs 330.7），T19 只加了 `flex-wrap` ⇒ 按钮确实换行了，
> 但**头像被挤到按钮那一行**、看着还是乱。现在 Trae 页容器改成与 WorkBuddy 逐字符一致
> （`max-w-6xl … px-4 py-6`），卡头 `col=153 / actions=108` 同行且留 42 px 余量。
> 另给 `CreditsSummaryBar` 加了 `refreshHint`（刷新按钮 `title`：说明顺带更新昵称 / 头像）。
> 详见 `docs/任务书.md` 的 **T20**。

> **本轮（v0.0.22）新闭环**（用户一条反馈：「workbuddy 清理失败」，报「未能于 20 秒内退出」）：
> ㉜ **「退出客户端」从「杀一次 + 死等 20 秒」改成「重试到真的没了 + 区分被重启」**：
> ① 现场：`~/.trae-switch-cn/` 里**从未出现过** `workbuddy_cleanup_backup/` ⇒ 死在退出这一步，
> 备份/删库/回收站都没开始。② 取证：非破坏性地只申请 `PROCESS_TERMINATE`（不终止）测 9 个
> `WorkBuddy.exe` 全部成功、本进程未提权 ⇒ **权限不是原因**；诱饵进程端到端复现
> `taskkill /PID x /T /F`（连 PATH 都换成应用那种 MSYS 风格）⇒ **也不是 taskkill 找不到**。
> ③ 真根因：`AppStartup.log` 记着 13:30:33 那次是 **`startup_type=upgrade`（5.6.2 → 5.7.6）**，
> `daemon.log` 从 `05:26:14Z` 断档到 `05:30:45Z` —— 客户端当时**正在自我升级，旧进程被杀掉后
> 更新器立刻把新版拉起来**，进程表永远非空 ⇒ 干等 20 秒报「未能退出」。**「杀不掉」是误判。**
> ④ 修法：`process_list` 新增 `kill_tree_and_wait`（每轮重新枚举 + 重试）、`kill_now`、
> `tree_roots`（Electron 只对树根发一次 `taskkill /T /F`）；超时后按「残留 PID 与首轮是否重叠」
> 判定 `restarted`，把文案一分为二（「被更新器拉起来了，等它稳定」/「真有 N 个进程没退出 + 命令返回」）；
> `run_cmd` 改为返回 `CmdResult{code, stdout, stderr}`（**旧实现把 stderr 丢进 `null`，taskkill 的
> 失败原因全在那里**），清理页失败时同时落到页面级 Alert。Trae 侧同构收口（`trae_switch::quit_client`）。
> ⑤ 真机验证：新示例 `cargo run -p wb-switch-core --example kill_tree_probe` 用**会自己复活的
> 诱饵进程**实测 `restarted=true`（6 轮）/ 正常路径 `attempts=1`。`cargo test --workspace` 152+2 全绿。
> 详见 `docs/任务书.md` 的 **T21**。

> **本轮（v0.0.23）新闭环**（用户一条反馈：「每次点击 workbuddy 清理和 trae 清理，都要自动刷新一次，
> 导致反应慢，之前扫过的记录可以用，发生更新可以手动扫描，不用每次自动扫，启动的时候后台已有自动扫描」）：
> ㉝ **清理扫描磁盘缓存从「10 分钟就失效」改成「有缓存就用」+ 卡头改网格（修一处排版回归）**：
> ① 现象实测：清理页调用时本来就是 `force=false`，但后端把缓存当成 **10 分钟 TTL 的闸门**，
> 超时就回落到实时扫描 —— Trae 一份 **29.9 分钟前**的缓存 ⇒ 进页面 **2852 ms**（实测）。
> ② 语义修正：新增 `config::read_cache_slot_stale_ok`（**版本对、结构没坏就交出 payload**，
> 另返回 `stale = age > ttl`），与老的 `read_cache_slot`（过期即 `None`）**并列存在** ——
> 「总览/积分必须新鲜」与「清理盘面可以旧但必须快」两种诉求无法共用一个签名。
> ③ 重扫收口成三条路径：**启动时首页后台扫**（`loadReclaim` 改 `force=true`，外层已有 30 分钟闸门）
> + 页面「重新扫描」按钮 + 清理完成后自动扫；只有「缓存根本不存在」才在进页面时扫。
> 界面在 `stale` 时用琥珀色写明「数据为 N 小时前，点『重新扫描』更新」，**只提示不自动扫**。
> ④ **顺带抓出一处排版回归**：文案变长后 WorkBuddy 卡头的「重新扫描」按钮掉到了第二行（1600 px
> 宽明明有余量）。根因：卡头是 `flex flex-wrap`，而**折行段落参与折行判定用的是自己的
> max-content 宽**而非可用宽度 ⇒ 文案一长就顶走邻居。**`flex-wrap` 只适合定宽小块并排，
> 不适合「文字段落 + 按钮」**。两个清理页卡头（共 4 处）改成
> `grid grid-cols-[minmax(0,1fr)_auto] items-center gap-2`（左列加 `min-w-0`），
> 按钮永远在右上角且与文案长短无关。
> ⑤ 验证：`cargo run -p wb-switch-core --example cleanup_cache_probe` ⇒ `cached(false)`
> **2852 ms → 6 ms**（`cached=true stale=true`）、`cached(true)` 仍真扫 2.5–2.9 s；
> 临时 `preview/` + 无头 Edge **1600/1360/730** 三档出图量宽：改前 `wb-stale sameLine=no`，
> 改后五行卡头全部 `sameLine=yes` / `btnRight=-25` / `overflowX=no`。
> `cargo test --workspace` **153 + 2 全绿**。详见 `docs/任务书.md` 的 **T22**。

> **本轮（v0.0.24）新闭环**（用户三条需求：「①本项目已经从之前 trae 账号切换功能升级到了
> workbuddy 和 trae 的管理工具，功能丰富，因此软件名称需要修改，你来帮我取名，由我确定
> ②软件 ui 和 logo 说明等要同步更新，logo 你重新设计 ③其他优化空间你来帮我确定，并安排执行计划」；
> 用户决策：**名称 = TwinSwitch · 双栖**、**改名深度 = 连数据目录一起改**、**logo 风格 = 极简线条 / 单色几何**）：
> ㉞ **正名 TwinSwitch · 双栖 + 全套标识迁移（八类落点）**：
> ① **为什么要改**：「trae-switch-cn」只描述第一个功能（Trae 账号切换），而工具早长出
> WorkBuddy 账号管理 / 会话互通 / 双向迁移 / 本机清理，名字与能力脱节。
> ② **改名一共八类落点**（漏一类就是「半边旧名」）：版本号五处 + **数据目录** + `localStorage` 键
> + 环境变量 + 图标资产 + 界面文案 + 文档 + **安装标识 `identifier`**（`com.traeswitch.cn` →
> `com.twinswitch.cn`；⚠️ 第八类是**用户点出来才补的**，它只在 `tauri.conf.json` 里孤零零一行，
> 却决定 WebView2 的 user data 目录与安装器的应用身份）。
> ②' **仓库另起**：主项目迁到 **`bean0283/twin-switch`**（公开，孤儿根 `500573e`，233 文件 / 14.44 MB），
> 与旧仓库历史彻底分离；旧仓库保持原样不动（用户决定不归档）。旧 9 次提交（`22d87c1`，v0.0.3）
> 留在本地分支 **`legacy-history`** 与旧仓库里。
> ③ **最险的是数据目录**：`~/.trae-switch-cn` → `~/.twin-switch`，里面压着几个 GB 真数据
> （626 MB 解密库 / 940 MB 回收站 / 318 MB 导入备份）。用**同盘 `rename`** —— 元数据操作，
> 与体积无关。`config::resolve_store_dir(home)` 三分支：新目录已在 → 用它（旧目录还在则**提示**）；
> 新旧都在 → 建新目录；只有旧目录 → 改名；**改名失败则本次运行退回旧目录并记 `MIGRATION_NOTE`**
> （不复制、不删除，下次启动自动重试）。重试四轮退避 `[120,240,480,960] ms`（Windows 句柄占用）。
> ④ **调用点必须在最前**：`lib.rs::run()` 开头、`Builder::default()` **之前** —— 因为
> `Builder::build()` 里单实例插件命中已有实例会直接 `process::exit(0)`，那时什么活都不该干。
> ⑤ **偏好类数据带迁移读**：新增 `src/lib/storage-keys.ts`，`readStored()` **新键优先 → 旧键兜底
> → 一次性写回新键**，`writeStored()` 只写新键，**旧键不删**（可回退旧版本）。覆盖主题与弹窗尺寸。
> ⑥ **环境变量新旧双认**：`TWIN_SWITCH_HOME` 优先，老的 `TRAE_SWITCH_HOME` 仍接受；抽成纯函数
> `home_override()` 才测得动（**不在测试里 `set_var`**，进程全局会和并行用例互踩）；用 `var_os`
> 而非 `var`，否则非 UTF-8 家目录会被丢掉。
> ⑦ **logo**：极简单色**双环相交**。应用内是**内联 SVG + `stroke="currentColor"`**（随主题自动
> 变色、任意尺寸锐利）；打包图标为石墨圆角方块 + 米白双环（`@resvg/resvg-js` 栅格化 → `npx tauri icon`）。
> 托盘走 `app.default_window_icon()`，**改图标即自动生效**，无需改代码。
> ⑧ **真机验证**：`cargo run -p wb-switch-core --example store_dir_migration_probe` ⇒ 造 **300 MB**
> 假数据走生产路径，**1.94 ms** 搬完，内容逐字节一致、旧目录消失、二次调用幂等。
> ⑨ **渲染核对**（按 T19 规矩）：临时 `preview/` 复刻侧栏（真组件 + 浅/深两套 220 px）⇒
> logo 实渲 **36×36**、品牌两行无截断、**9 个导航项全部单行**（最紧的「WorkBuddy 账号管理」
> 需 132 / 可用 145）。**顺带踩到两个度量坑**：`scrollWidth` 在内容**不溢出**时直接返回
> `clientWidth`，「所需宽」会退化成恒等于「可用宽」⇒ 必须用 `Range.getBoundingClientRect()`；
> 导航标签**没有 `whitespace-nowrap`**，放不下是**折行**不是横向溢出 ⇒ 要配**高度**维度
> （单行 40 px，超 44 即折行）。
> ⑩ 验证：`cargo test --workspace` **157 + 2 全绿**、`npx tsc --noEmit` 零错误。
> 护栏测试 `main_thread_guard::heavy_commands_must_not_be_sync` 因新增同步命令 `store_dir_info`
> **如实失败**，登记进 `SYNC_ALLOWLIST` 后通过。按用户点名做了源码备份
> （`backup/trae-switch-cn_v0.0.23_src_20261006-1433.zip`），**仍未发布 GitHub**。
> 详见 `docs/任务书.md` 的 **T23**。
>
> ⚠️ **升级操作前提**：目录搬迁发生在**启动瞬间**，请**先关掉所有旧实例**再启动 v0.0.24；
> 否则会走到「新目录已就绪、旧目录仍在」的分支（旧目录**不会自动删**，会提示手动处理）。

### P0 —— 记录里有结论但未闭环的待确认
1. **切换账号要不要重写登录态文件？** 用户明确提问：切换后 `C:\Users\11970\AppData\Roaming\TRAE SOLO CN\User\globalStorage\storage.json` 实测**没有发生变化**，问是否影响安全性、以及可否在切换时重写。记录中**未看到对用户的正式答复**。
2. **重复会话合并未实现**。用户问过「导入后会话列表出现 2 份，是否可以合并」。记录显示当时把合并任务**撤销**了，改为先修「导入的会话无法看详情/下载 MD」与「导入候选里目标账号还是本账号」两个真实缺陷。
   - 现状：两条重复会话分别归属源账号与导入目标账号，属于跨账号导入的正常副产物。
   - 需要决策：合并策略（保留哪一份——记录里已抛给用户但未定）。

### P1 —— 代码里已写明、但未验证的边界
3. **跨账号导入的云端同步行为未定义**。`trae_import.rs` 注释明写：写进目标库的会话是「本地新增」行，若目标账号的云端同步会清理本地孤儿行，**导入的会话可能被覆盖**。用户报过「导入后切换账号进去仅能查看、无法发送消息」→ 已在 `74c8e6f` 用「补齐关联表」修过一次，但云端双向同步仍未经真实账号长周期验证。
4. **密钥过期与重扫**。`trae_delete` 已实现「存盘密钥 HMAC 校验，过期自动重扫」，但 `trae_import` 的前置约束里仍写明「无存盘密钥时需该账号进程运行中做内存扫描」——两条链路的密钥获取策略**不完全一致**，值得统一。
5. **Phase A 成果去向不明**（见第二节风险提示）——`workbuddy-switch-cn` 目录已消失，且不在本仓库历史内。若阶段一的单目标化成果仍需保留，需要找回该目录或确认废弃。

### P2 —— 工程卫生
6. ~~仓库只有 9 个提交，**没有任何测试**~~ → **已修复**：`crates/wb-switch-core/src/modules/` 各模块内置单元测试，
   当前 **133 passed / 0 failed / 1 ignored**（`cargo test -p wb-switch-core`），另有 1 个真机自检用例
   （`real_plan_item_shape_matches_native`，含注入判别护栏）；`src-tauri/src/commands.rs` 的
   `mod main_thread_guard` 另有 **2 passed** —— 静态扫源码护航「同步命令不许干重活」（见 T17）。
7. `scan_key.rs` 里残留绝对路径 `D:\htw\签到\workbuddy-switch-cn\scan_key_decrypted.db`（示例程序，影响小但会误导）。
8. `src/lib/types.ts` 仍有 `workbuddy` 命名残留（类型标签），属改名遗留。

---

## 六、后续行动建议（按优先级）

### 先把 P0 的「答案」补齐 —— 这两个是用户问过、需要回话的
- **A. 登录态文件安全性答复**：给出「切换是否重写 `storage.json` / 当前不重写是否有风险」的明确结论与建议开关位（建议做成可配置项，默认沿用现状）。
- **B. 重复会话合并策略**：定「保留源 / 保留目标 / 保留更近更新」中的一种，实现去重（软删 `deleted_at` 列已存在，可复用）。

### 然后按这个顺序推进
1. **补测试**（解 P2-6）：至少给 `trae_decrypt` / `trae_import` / `trae_delete` 的页面级加解密补单元用例。这套「解密→改→加密回写→原子替换」是全项目最核心、也最没有自动化保护的资产，且**动了会直接破坏用户本地真实账号库**。
2. **统一密钥获取策略**（解 P1-5）：让 `trae_import` 与 `trae_delete` 走同一条「存盘 → 校验 → 过期重扫」链路。
3. **跨账号导入长周期验证**（解 P1-3）：找一个真实账号验证导入后能否发消息、云端是否会回滚本地孤儿行。
4. **版本号与发版流程固化**（见第七节），每次修完按最新版本号（当前 `0.0.18`）走一遍。
5. 顺手清理 P2-7 / P2-8 的命名与路径残留。

---

## 七、环境与操作备忘（续接时直接照抄）

### 目录
| 路径 | 用途 |
| --- | --- |
| `D:\htw\签到\trae-switch-cn` | 本项目（git 根） |
| `D:\htw\签到\trae-switch` | Node.js 参考：账号切换（**功能已复刻完毕**） |
| `C:\Users\11970\Downloads\Compressed\trae-session-export-main\...` | Python 参考：记录导出/解密（**已复刻完毕**） |
| `%LOCALAPPDATA%\Programs\MinGit\cmd\git.exe` | 本机 git 在此（便携版 MinGit），`PATH` 里**没有** `git.exe`，脚本里要用全路径 |
| 参考 Node 运行时 | `C:\Users\11970\AppData\Local\Programs\Python\...` 之外，Trae 自带 node 在 `%APPDATA%\TRAE SOLO CN\ModularData\ai-agent\vm\tools\node\node.exe` |

### 构建 / 打包
```bash
cd trae-switch-cn
npm run build              # tsc + vite（前端）
cd src-tauri && cargo check          # Rust 自检（本次 17.75s 通过）
npm run tauri build        # 产出 target/release/bundle/{nsis,msi}
```
- 打包首次会拉 **WiX**（NSIS/MSI 依赖），带宽不稳时会超时卡住 → 缓存目录必须放对位置，否则反复重下。
- 目标：**NSIS 版（推荐）+ MSI 版**。

### 发版（v0.0.7 走这套）
1. **五处版本号同步改**（曾经漏掉后两处，导致侧栏显示的版本和安装包对不上）：
   `package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`、
   `crates/wb-switch-core/Cargo.toml`、`src/components/update-entry.tsx` 的 `version || "x.y.z"` 兜底串
   （⚠️ v0.2.0 起兜底串已从 `src/App.tsx` 搬到 `src/components/update-entry.tsx`）
2. 顺手更新 `README.md` 的下载指引（README 里目前写死 0.0.3）
3. `git add -A && git commit -m "...；v0.0.7" && git push origin main`
4. 建 tag `v0.0.7` + Release，上传两个安装包
5. GitHub 工具链：`gh` CLI **本机未安装**；现用 `git credential fill` 取 GCM 的 OAuth token 再调 GitHub API。上传大文件**用 Node 的 fetch 比 PowerShell 的 HttpClient 稳**（记录里 PowerShell 版本连续失败、换 Node 才成功）。

### 分支与仓库
- 仅 `main` 一个分支。remote `origin` = `https://github.com/bean0283/twin-switch.git`（**主项目**）；
  remote `legacy` = `https://github.com/bean0283/trae-switch-cn.git`（旧仓库，只读参考，**不要再 push**）
- 本地另有 `legacy-history` 分支，指向改名前的最后一次提交（`22d87c1`，v0.0.3）——旧历史靠它兜底
- 两个仓库都是 **public**
- v0.1.0：仓库内容**重做为单一根提交 `8d0799d`（提交信息只有「首次提交」四个字，无父提交）**，与旧仓库历史彻底分离
- 已发布 **Release `v0.1.0`**（当时的 tag）：`TwinSwitch_0.1.0_x64-setup.exe`（NSIS，5.15 MB）+ `TwinSwitch_0.1.0_x64_en-US.msi`（7.20 MB）
- **v0.2.0（当前）**：加入**自动更新**（见 `docs/任务书.md` **T24**）。
  - 根提交 amend 后为 **`4f7e00b`**（239 文件；提交信息仍只有「首次提交」四字、无 body）
  - **`v0.1.0` 与 `v0.2.0` 两个 tag 均已同步指向 `4f7e00b`**（`git tag -f` + `push -f origin refs/tags/...`）
  - `Release v0.2.0` 资产三份：`latest.json` · `TwinSwitch_0.2.0_x64-setup.exe`（5 738 300 B）· 同名 `.exe.sig`（420 B）
  - 自动更新清单固定读 `https://github.com/bean0283/twin-switch/releases/latest/download/latest.json`
  - ⚠️ **签名私钥在仓库外 `~/.twin-switch-keys/twin-switch-updater.key`**，与 `tauri.conf.json` 的
    `plugins.updater.pubkey` 是一对；**丢失后老客户端再也验不过新包**，必须备份
  - ⚠️ **v0.1.0 的安装包不含更新能力**，用户须先手动装一次 v0.2.0，之后才会收到自动更新
- ⚠️ **版本号递增规则（用户 2026-10-06 明确要求）**：每次只进一位补丁号
  （`0.1.0 → 0.1.1 → 0.1.2`），**只有用户明确要求才跳次版本号**（如 `0.2.0`）
- ⚠️ **删除仓库需要 PAT 具备 `delete_repo` scope**：本机 GCM 里的令牌只有 `gist, repo, workflow`，
  `DELETE /repos/{owner}/{repo}` 返回 **403 "Must have admin rights to Repository."**。
  要真正删掉仓库，得先给令牌补 `delete_repo` scope，或在网页端手动删

### 发布核验命令（可复用）
```bash
curl -s https://api.github.com/repos/bean0283/twin-switch | python -m json.tool
```

---

## 八、给下一次的续接提示

1. **不要重做已完成的部分。** 主线已收口在 v0.0.3 + `22d87c1`，从「六、后续行动建议」第一条开始即可。
2. **v0.0.3 的四个 commit（`938b8e8` / `5491207` / `d3dc211` / `22d87c1`）对应的是用户最后一批报障**，改代码前先看这四条 commit，避免重复排查。
3. **最高频的返工根因就三条**：进程名含空格、rusqlite 无 SQLCipher 特性（必须页面级加解密）、内存扫描窗口语义（必须全滑动窗口）。动这三块前先读第四节。
4. **涉及导入 / 删除的操作会直接改写用户本地真实账号库**，任何改动都要保留「备份 → 原子替换 → 失败回滚」，并先跑单元测试。
5. 用户节奏偏好：**一次给一串编号问题（1、2、3…）→ 要求执行前先询问确认 → 修完要求提版本号 + 同步 GitHub**。续接时遇到「执行前发我发起询问」类指令，务必先出方案再动手。
