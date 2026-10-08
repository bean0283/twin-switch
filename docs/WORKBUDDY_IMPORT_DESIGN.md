# WorkBuddy 记录 → Trae 会话导入：可行性与设计方案

> 分析时间：2026-10-04
> 分析对象：`workbuddy-switch-main.zip`（WorkBuddy 多账号切换项目，含会话复制内核）
> 实测样本：本机 `~/.workbuddy/`（WorkBuddy 桌面版真实数据）+ `~/.twin-switch/trae/decrypted/solo-cn.db`（已解密的 Trae 真实库）
> **本文所有结构均为实测所得，非推断。**

---

## 一、WorkBuddy 的记录文件是如何识别的

WorkBuddy 桌面版的会话由 **「数据三件套」** 共同定义（缺一不可），这是识别一个会话的唯一依据：

| 部位 | 路径 | 作用 |
| --- | --- | --- |
| ① 正文 | `~/.workbuddy/projects/{工作区目录}/{会话id}.jsonl` | 全部对话事件流（JSONL，一行一个事件） |
| ② 元数据 | `~/.workbuddy/workbuddy.db` → `sessions` 表 | 标题 / 工作区 / 归属账号 / 时间 / 模型 / 删除标记 |
| ③ 云端映射 | `~/.workbuddy/edge-sync-mapping-v{N}.db` → `edge_sync_mapping` | `session_id` + `msg_channel=convmsg:{uid}` 决定云端归属 |

### 识别规则（实测确认）

- **会话 ID = 正文文件的文件名词干**，形如 `4a56be9e-dd0f-480d-9fab-132d0e429ab0`（**UUID 带连字符**），与 `sessions.id` 完全一致。
- **工作区目录不参与识别**：定位逻辑是遍历 `projects/` 下所有子目录找 `{会话id}.jsonl`，因此目录名规则（实测 `D:/htw/签到` → `d-htw-签到`）无需精确推导。
- **账号归属 = `sessions.user_id`**（本机两个账号：`63e05cca-…` 与 `6aa2c43b-…`）。
- `sessions.deleted_at` 非空即已删除（本机存在 `-1` 与毫秒时间戳两种写法）。

### 正文 JSONL 的事件类型（本机实测统计）

某真实会话 67 行的事件分布：

| `type` | 条数 | 含义 | 关键字段 |
| --- | --- | --- | --- |
| `session-meta` | 2 | 会话头 | `sessionId`、`timestamp`、`meta` |
| `message` | 2 | **对话正文** | `role`（user/assistant）、`content[]`（`{type:"input_text"\|"text", text}`） |
| `reasoning` | 17 | 模型思考 | `rawContent[].text`、`parentId` |
| `function_call` | 21 | 工具调用 | `name`、`providerData`（model/traceId/usage） |
| `function_call_result` | 21 | 工具结果 | `name`、`callId`、`status`、`output.text` |
| `file-history-snapshot` | 3 | 文件快照 | `snapshot`、`cwd` |
| `ai-title` | 1 | 自动标题 | `aiTitle` |

**一个「回合」的构成**：`message(role=user)` → `reasoning` / `function_call` / `function_call_result` 若干 → `message(role=assistant)`（最终回答）。

### 关键结论

> WorkBuddy 的记录是 **纯明文**（JSONL 文本 + 明文 SQLite），**不需要解密**。可以直接读取、直接改写。

---

## 二、与 Trae 会话记录是否格式类似

**结论：完全不类似。两者在存储形态、数据结构、加密方式三个维度上都不同。**

| 对比维度 | WorkBuddy | Trae |
| --- | --- | --- |
| 加密 | **无加密**，明文可读 | **SQLCipher 4 全库加密**（AES-256-CBC + HMAC-SHA512，reserve=80） |
| 存储形态 | 一个会话 = 一个 `.jsonl` 文本文件 | 一个会话 = 加密库中 **8 张表的多行记录** |
| 数据模型 | 扁平事件流（一行一个事件，`type` 区分） | 强关系型（主表 + 内容表 + 历史表 + 回合表） |
| ID 形态 | UUID 带连字符（36 字符） | 无连字符 hex（会话 20~24 位，消息 25 位） |
| 内容组织 | 每行自带 `content[]` 文本块 | **内容按角色拆表**：用户走 `chat_message_general`，助手走 `chat_message_task` + `history_v2` |
| 时间戳 | 毫秒整数 | **秒**整数 |
| 思考/工具 | 独立的 `reasoning` / `function_call` 事件行 | 塞进 `chat_message_task.content` 的 `plan_item` 结构 + `history_v2` 多行 |
| 删除语义 | `sessions.deleted_at` | `deleted_at` 列分散在 8 张表 |

### Trae 侧的完整表结构（实测）

一个 Trae 会话要完整落地，需要以下 8 张表协同：

| 表 | 作用 | 关键列 |
| --- | --- | --- |
| `project` | 会话归属的工作区 | `project_id`、`user_id`、`absolute_path`、`name` |
| `chat_session` | 会话主表 | `session_id`、`project_id`、`session_title`、`created_at`、`session_type`、`work_mode` |
| `chat_message` | 消息索引 | `session_id`、`message_id`、`message_type`、`message_role`、`message_index`、`reply_to_message_id` |
| `chat_message_general` | **用户**文本 | `message_id`、`content` = `[{"type":"text","text_content":"…"}]` |
| `chat_message_task` | **助手**过程 | `message_id`、`task_id`、`content` = `{"task_id":…,"messages":[{plan_item…}]}` |
| `chat_turn` | 回合关系 | `turn_id`、`reply_to_message_id`、`response_message_id`、`turn_status` |
| `history_v2` | 助手流式文本 | `history_v2_id`、`message_id`、`content_source`、`messages` = `{"raw_messages":[…]}` |
| `server_history_info` | 云端历史 | 本机样本中**为空**，可不写 |

---

## 三、能否「加密后写入 Trae」

**Q3 答案：不能直接写入** —— 格式不同，WorkBuddy 的记录没有 Trae 所需的表结构，无法加密后原样投递。

**Q4 答案：可以转换后再写入，且技术上完全可行。** 这正是本项目要走的路线：

```
WorkBuddy 明文 JSONL + 明文 sessions 表
        ↓  ① 读取
    归一化中间层（回合 = user 文本 + assistant 文本 + 过程事件）
        ↓  ② 转换（字段映射，见下表）
    Trae 8 张表的行（未加密的明文 SQLite 副本）
        ↓  ③ 加密回写（复用现有页面级加解密链路）
    Trae 加密库（SQLCipher）
```

第 ③ 步**无需新写代码**：`trae_import.rs` / `trae_delete.rs` 已经实现了「解密 → 改数据 → 逐页加密回写 → 备份 → 原子替换 → WAL 已提交帧合并」的完整链路，新功能只要把「行」交给它即可。

### 字段映射草案

| WorkBuddy | → | Trae | 处理 |
| --- | --- | --- | --- |
| `sessions.id`（UUID） | → | `chat_session.session_id` | **重新生成** Trae 风格短 hex（冲突检测） |
| `sessions.title` 或 `ai-title` | → | `chat_session.session_title` | 优先 `custom_title` → `title` |
| `sessions.created_at`（ms） | → | `chat_session.created_at`（s） | ÷1000 |
| `sessions.cwd` | → | `project.absolute_path` + `project.name` | 按目标 uid 查找/新建 project |
| `message(role=user).content[].text` | → | `chat_message_general.content` | 包一层 `[{"type":"text","text_content":…}]` |
| `message(role=assistant).content[].text` | → | `history_v2.messages`（`content_source="llm_default"`） | 包 `{"raw_messages":[{"role":"assistant","content":[{"type":"text","text":…}]}]}` |
| `reasoning.rawContent[].text` | → | `chat_message_task.content` 的 `plan_item.reasoning_content` | 归入所属回合 |
| `function_call` + `function_call_result` | → | `chat_message_task.content` 的 `plan_item` 序列 | 工具名 → `tool_call_info` |
| 一个 user+assistant 配对 | → | `chat_turn` 一行 | `turn_status="completed"` |
| 整个会话 | → | `chat_message` 逐条 | `message_index` 递增、`reply_to_message_id` 反向指用户消息 |
| — | → | `server_history_info` | **不写**（本机样本为空，非必需） |

---

## 四、风险与不确定点（实施前必须知道）

1. **云端清理孤儿行（最高风险）**。`trae_import.rs` 的注释已自陈：写进本地库的会话是「本地新增」行，若 Trae 的云端同步会清理本地孤儿行，导入的会话**可能被覆盖或清掉**。WorkBuddy 的云端归属（`edge_sync_mapping`）与 Trae 完全无关，这条风险比 Trae↔Trae 导入时更高。
2. **可读 ≠ 可续聊**。Trae 的续聊依赖 `chat_turn` + `history_v2` 的服务端会话上下文。本地合成的行能否让 Trae 接受后续追问，**需要真实测试**（历史上 Trae↔Trae 导入就踩过「只能看不能发」的坑）。
3. **助手内容的还原度**。Trae 的助手回答分散在 `chat_message_task`（过程/计划）和 `history_v2`（流式文本），且 `plan_item` 结构复杂（`agent_status`、`render_mode`、`tool_call_info` 等）。WorkBuddy 的 `reasoning` / `function_call` 与其**不是一一对应**，只能做「近似映射」，过程细节必然有损。
4. **时间戳与排序**。Trae 用秒、WorkBuddy 用毫秒，且 `message_index` 必须严格递增，跨设备时区可能引入乱序。
5. **`project` 归属**：Trae 按 `user_id` + `absolute_path` 组织工作区。WorkBuddy 的 `cwd` 要映射到 Trae 的目标账号，路径可能不存在于 Trae 侧。

---

## 五、待确认问题（决定实现方向）

见对话中的提问。核心分歧点：

1. **目标位置**：写进哪个 Trae 客户端 / 账号？
2. **导入范围**：导入哪些 WorkBuddy 会话？
3. **保真度**：只导入对话正文，还是连思考与工具调用一起还原？
4. **目标效果**：只要「能看」，还是要求「能接着聊」？

---

## 附：实测证据留档

- WorkBuddy 数据根：`C:\Users\11970\.workbuddy\`（`projects/` + `workbuddy.db` + `edge-sync-mapping-v4.db` 齐备 → 能力探测通过）
- 样本会话：`projects/d-htw-签到-trae-switch-cn/4a56be9e-dd0f-480d-9fab-132d0e429ab0.jsonl`（67 行 / 391 KB）
- WorkBuddy 账号：`63e05cca-cf7d-4dfa-af52-65168597eac1`（18 个会话）、`6aa2c43b-05f5-489a-840a-298f6cbe81ba`（3 个会话）
- Trae 解密库：`C:\Users\11970\.trae-switch-cn\trae\decrypted\solo-cn.db`（另有 `trae-cn.db`）
- Trae 样本会话：`6abe400b3e6fc329aa07a32b`（2 条消息 / 9 条 history_v2）

---

## 六、实现记录（v0.0.4，2026-10-04）

用户确认的方案：**目标沿用现有导入的账号选择**、**界面手动勾选来源**、**尽量完整还原过程**、
**必须能在 Trae 里接着聊**。据此落地如下。

### 新增模块

| 文件 | 职责 |
| --- | --- |
| `crates/wb-switch-core/src/modules/workbuddy_source.rs` | 发现并解析 WorkBuddy 会话：读 `workbuddy.db`（先复制 db+wal+shm 避免争锁）、定位 `projects/*/{id}.jsonl`、把事件流归一化为「回合 = 用户提问 + 助手最终回答 + 有序过程事件」 |
| `crates/wb-switch-core/src/modules/workbuddy_import.rs` | 转换 + 编排：把回合映射为 Trae 10 张表的行，复用页面级解密 / 加密回写 / 备份 / 原子替换链路写回目标账号 |
| `src/components/workbuddy-import-card.tsx` → **T39（v0.2.12）已并入 `src/pages/WorkbuddyImportPage.tsx`** | 前端：目标卡（客户端 / 账号）+ 来源勾选（筛选 / 全选 / 每组只留最新）+ 操作条 + 确认弹窗（`trae_workbuddy_preview` 统计与逐条明细）；版式与 `WorkbuddyExportPage` 逐块同构 |

新增 Tauri 命令：`trae_workbuddy_list`、`trae_workbuddy_preview`、`trae_workbuddy_import`
（进度事件 `trae-workbuddy-progress`）。

### 两个关键设计决定

1. **模板取自目标库自身**。`chat_session.context` 与 `chat_message.user_message_context` 的形状
   随客户端版本演进（`vm_mode`、`skill_list_revisions`、`model_info` 等），硬编码易与已安装版本错位。
   改为读目标库中最近一条真实行作模板，只替换 `last_real_project_id` / `query` / `parsed_query`。
2. **agent 身份从目标库探测**。`solo-cn` 用 `solo_work_lite` / SOLO MTC，`trae-cn` 用 `solo_agent` /
   SOLO Agent。实现取库内 `chat_turn` 出现最多的取值，无历史行时按客户端回退——避免写死。

### 事件到 plan_item 的映射

WorkBuddy 的节奏是「叙述 / 思考 → 工具调用 → 工具结果」循环，末尾剩一段只有叙述的内容即最终回答：

| WorkBuddy 事件 | Trae plan_item 字段 |
| --- | --- |
| `assistant_text`（中间叙述） | `thought` |
| `reasoning` | `reasoning_content` |
| `function_call` | `tool_call_info.name` / `params` / `meta.llm_toolcall_id` |
| `function_call_result` | `tool_call_info.result.status` |
| 末段 `assistant_text` | 不进过程流，作为该回合最终回答写入 `history_v2`（`llm_source=llm_default`） |

### 安全加固

- 写库前**合并 WAL 已提交帧**（`trae_delete::merge_wal_into_plain`）。`import_sessions` 原实现直接
  删 wal/shm，若 Trae 被强杀，WAL 里其他会话的最新数据会随替换丢失；新链路补上了这一步。
- WorkBuddy 侧**全程只读**：只读快照，绝不改动 `~/.workbuddy` 下任何文件。
- 原库备份到 `~/.twin-switch/workbuddy_import_backup/`，替换失败保留备份并报错。

### 验证结果

| 验证项 | 方法 | 结果 |
| --- | --- | --- |
| 会话发现与解析 | `examples/wb_scan.rs` 跑真实本机会话 | 18 个会话正确列出，回合切分正确 |
| 结构同构 | `examples/wb_verify.rs` 对比合成会话与真实会话的不变式 | 孤立消息 0 / 断链引用 0 / 缺历史 0 / 坏 JSON 0，全部合格 |
| **加密往返** | `examples/wb_roundtrip.rs`：写入 → 补 reserved → 加密 71477 页 → 重新解密 | 全部通过，会话与内容完整存活 |
| 单元测试 | `cargo test` | 22 项通过（新增 7 项覆盖属性列映射） |

### 未覆盖（需用户真机验证）

1. **实时加密库的原子替换与客户端重启**——离线自检走的是副本，真实路径需要用户实测。
2. **能否真的「接着聊」**——本地行结构已补齐 `session_project` / `chat_turn` / `agent_run`，
   但 Trae 服务端是否接受为可续聊会话，必须真机验证（历史坑：Trae↔Trae 导入曾出现「只能看不能发」）。
3. **云端同步是否会清理这些本地 orphan 行**。

---

## 七、首版真机测试失败与修复（2026-10-04）

### 现象

用户真机测试后反馈三条：**①导入提示成功但 Trae 里看不到会话；②Trae 会话加载异常变慢；
③开关工具自己的会话列表一直空白（解密慢）。**

### 排查方法

先定位真实库：`%APPDATA%\TRAE SOLO CN\ModularData\ai-agent\database.db`（279 MB）。
工具已把活库解密出快照 `~/.twin-switch/trae/decrypted/solo-cn.db`，直接用 sqlite 查询即可——
**这一步是整个排查的关键：不要猜结构，去真实库里把「健康行」和「我们写的行」逐列对比。**

对比后确认首版写入了 8 张表、会话确实在库里（`chat_session` 有 `nihao` / `向助手打招呼` 两行），
但违反四条**实测硬约束**：

| # | 缺陷 | 实测依据 | 后果 |
| --- | --- | --- | --- |
| 1 | **完全没写 `task` 表** | 正常会话 `task 行数 ≡ 回合数`（2/2、9/9、3/3、73/73）；我们 0 行 | 客户端「任务列表」按 `task` 渲染 → **列表里看不见** |
| 2 | id 用纯随机 | 真实 6/6 会话 `id[:8]` 换算**精确等于**其 `created_at`（`0x6abf7aed`=1790933741） | id 兼作排序键与时间范围查询 → 记录落到错误时间区间 |
| 3 | `history_v2_id` 写 32 位 | 真实是 **24 位** | 结构不符 |
| 4 | `project` 行非法 | `biz_project_id` 只写 6 位（真实是 `project_id` 数值减一的 24 位 hex）；`absolute_path` 写成 `D:/htw/test`（真实是 `d:\htw\test`），导致匹配不到已有项目、重复建行 | 项目关联异常 |

附带发现：`agent_run_id` 真实是 **UUID v5**（第三段首字符 `5`，我们用了 v4）；
`chat_turn.context` 真实是 24 KB 级的复杂 JSON（我们写了 `'{}'`）。

### 修复

1. **补写 `task` 表**（每回合一行，`message_id` = 该回合助手消息 id，`task_id` 同步进 `chat_message_task.content`）。
2. `hex_id_at(secs)`：**前 8 位 = unix 秒 hex，后 16 位随机**；所有 id（session/message/turn/task/history）统一走它。
3. `history_v2_id` 改 24 位；`agent_run_id` 改 `uuid5()`；`agent_run.created_at` 对齐到 turn+1s。
4. `project` 行：`normalize_abs_path()` 归一化路径（小写盘符 + 反斜杠）后再查/写；`biz_of()` 按减一规则生成 24 位；`workspace_status` 用 `virtual`。
5. `chat_turn.context` 与 session/message 一样**取目标库真实行做模板**。
6. **幂等**：`session_id` 由源会话 id 确定性派生（时间戳前缀 + 源 id 前 16 hex），重复导入先删旧行再写，不再堆重复会话。
7. **自愈清理**：导入前先跑 `purge_broken_sessions()`（删「有回合却无 task」的幽灵会话）与
   `purge_malformed_projects()`（删畸形的、已无人引用的 project 行），逐条打日志。备份仍在替换前一刻生成，可回滚。

### 验证（离线干跑，未触碰线上库）

新增 `examples/wb_dryrun.rs`：把解密快照复制一份，在副本上执行「清理 + 转换写入」，再逐条校验硬约束。

```
清理异常会话 13a30c1b97d3436eb3a0b5bf（1 个回合 / 0 条 task）「nihao」
清理异常会话 7d07ae1481a946aaa8ee35cc（2 个回合 / 0 条 task）「向助手打招呼」
清理畸形项目 98152de6f6b74b9db6d6206a（biz_project_id 非 24 位，已无会话引用）「test」D:/htw/test

会话总数 7，违反硬约束的 0 个 ✅ 全部符合
  6ac22cadf92b521cbac24f51 turns=11 task=11 id前缀✅ ✅ [分析签到项目执行进度]
```

新会话复用了既有项目 `6abf7a9a853237fb78521db7`（`d:\htw\签到`），不再新建重复项目；
`history_v2_id` 24 位、`agent_run_id` 为 v5、`chat_turn.context` 24 KB 模板——全部对齐真实行。

单元测试 22 → **28 项**（新增 6 项：id 时间戳前缀、biz 减一规则、路径归一化、session_id 确定性、
uuid5 版本位、幽灵会话清理），`cargo check --workspace --all-targets` 零错误零告警。

### 教训（写进约束，避免复发）

- **不要用「看起来合理」的随机值填充有语义的字段**。Trae 的 id 是带时间戳的有序键，随机 id 不是「等价实现」。
- **关系库的「表清单」不等于「必写清单」**。首版按 10 张表写，漏掉了 `task`，而它恰是列表渲染的入口。
- **判断依据只能是目标库自身的存量数据**：凡是「真实行恒满足」的不变式（`task ≡ turn`、`id[:8] == created_at`），
  都必须逐条比对，而不是靠 schema 推断。

---

## 八、第二轮真机测试失败与修复（v0.0.5，2026-10-04）

### 现象

首版修复后用户再测，反馈**三条**：

1. **「我就导入一个小会话，为什么需要这么大的空间？」**
2. **「导入的会话内容显示不一致」**——Trae 里点开导入的会话，看到的是**别的会话的提问**。
3. **「Trae 的会话加载进度异常慢，这个问题还是没有修复」**。

### 先把「用户此刻看到的到底是什么」查清楚

排查前必须先确认：用户测的是**哪一版**代码写进去的库。做法是把**活库**重新解密一份快照
（`examples/wb_probe.rs`，会一并合并 `-wal` 已提交帧），再逐列读。

结果：活库里那条导入会话（`6ac23c9a183522f035e04d36`「向助手打招呼」）**仍然是旧版写的畸形行**——
用户测的是修复前的构建，所以「还是没有修复」。

该行实测出的全部缺陷：

| 列 | 实测值 | 应该是什么 | 直接后果 |
| --- | --- | --- | --- |
| `chat_turn.context → persist_user_message_context.query` | `提交更新到github，包括Releases…`（**属于 73 回合的大会话 `6abf7aed…`**） | 本会话自己的提问 | **内容显示不一致**：会话里显示别的会话的问题 |
| `chat_turn.context → workspace_folders` | `["d://htw//签到"]`（别的会话的工作区） | `["d://htw//test"]` | 客户端**预热/挂载错误工程** → 加载慢 |
| `chat_turn.context → context_usage` | 整段别的会话的 `contexts` 数组 | 空 | 客户端重新水合大批上下文 → 加载慢 |
| `chat_turn.context → token_usage.prompt_tokens` | `127929`（别的会话的缓存 prompt） | 0 | 客户端以为本会话已有巨型上下文 → 加载慢 |
| `chat_session.context → skill_list_revisions` | 键名含 **`conversation_skill_list_revision_6abf7aed853237fb78521db8_…`** | `{}` | 客户端据此**去预热另一个会话的技能列表** → 「会话加载异常慢」的直接来源 |
| `chat_message.revertible` | `0` | `1` | 回合不可回退，渲染路径异常 |
| `agent_run` | 2 行（每回合一行） | 1 行（**会话级**） | 结构不符 |
| `chat_message_task.content` | `messages: []`（空数组） | 末项必须是 `finish` `plan_item` | **助手回答为空** |

> **关键结论**：三条抱怨其实同源——**「整段照抄他会话的 context」**。它同时造成内容串台（①）、
> 预热错误工程与技能列表（②）、以及旧版漏写 `finish` 项导致空回答（③）。

### 修复

**A. context 只借「字段形状」，值全部重建**（`workbuddy_import.rs`）

- 新增 `neutralize()` / `nullify()`，把模板 JSON 里所有**值**清空（只保留键结构）。
- `build_session_context(conn, pid)`：强制
  `activated_feature_flags: []`、`file_read_state_cache: null`、`cc_file_read_state_cache: null`、
  `has_remote_counterpart: false`、`is_worktree: false`、**`skill_list_revisions: {}`**、
  `last_real_project_id: pid`，保留 `server_history_cache_limit`、`vm_mode`。
- `build_turn_context(conn, workspace, user_text, t0, t1)`：借形状 + `model_info` + `version_code`/`locale`；
  **用本回合的提问重写** `persist_user_message_context.query`/`parsed_query`；`render_context` 用 `nullify`
  清空；重新生成 `trace_id`；`token_usage` 归零；`workspace_folders = [workspace]`；`context_usage` 置空；
  `chat_start/end_time` = 秒 × 1000。
- 工作区不存在时（`Path::is_dir()` 为假）**直接写 `[]` 并打日志**，绝不写一个不存在的目录——
  否则客户端会反复尝试挂载/预热该工程。

**B. 助手回答落在 `finish` 项里**（`build_task_content`）

真实库 **129/129** 条助手消息都至少有一个 `plan_item`，且末项是 `tool_call_info.name == "finish"`，
正文在 `params.summary`。首版写了 `messages: []`，所以**回答是空白的**。
现在**无论本回合是否拿到正文，都补一个末项 `finish`**（拿不到时 `summary` 留空），保持结构一致，
避免再出现空 `messages` 的畸形行。

**C. 会话级 / 消息级字段对齐**

- `chat_message.revertible`：`0 → 1`（用户与助手消息都是）。
- `agent_run`：由「每回合一行」改为**会话级一行**（`run_id = uuid5(session_id:start)`，在回合循环前插一次）。
- `chat_session.context.skill_list_revisions`：`{}`（不再泄漏别的会话 id）。

**D. 空间占用**

导入链路本来就不可避免要「解密 → 改 → 加密」整库（279 MB），但**不该留下第二份等大的库**，
更不该让备份无限累积。实测清点如下：

| 目录 | 实测占用 | 说明 |
| --- | --- | --- |
| `trae/backup` | **2.2 GB** | 删除会话前的整库备份，`*_before_delete_<时间戳>[-wal\|-shm].db.bak`，**从不清理** |
| `workbuddy_import_backup` | 843 MB | 3 批 × 281 MB |
| `trae/decrypted` | 689 MB | 两个客户端的解密快照（可再生） |
| `trae/import_backup` | 6 批 | 分批导入备份，**从不清理** |

改动：

1. **原地加密回写**：`encrypt_db_file_in_place()`（`trae_import.rs`）按页读 → 加密 → **回写同一偏移**，
   省掉一份与目标库等大的临时文件。SQLCipher 每页独立加密（随机 IV + 每页 HMAC、页号参与 HMAC、页长恒 4096），
   所以「读第 N 页就原地覆盖第 N 页」是安全的；写完再自校验首页 HMAC。
2. **统一备份清理** `trae_export::prune_backups(dir, keep)`：按**客户端**分组（分组键 = 客户端 + `YYYYMMDD_HHMMSS` 时间戳，
   字典序即时间序），每组只留最新 `keep` 批；同时支持**子目录**（导入备份）与**文件组**
   （删除前的整库备份 + `-wal`/`-shm` 兄弟文件整组删）。三处备份点全部接入（各留 2 批）。
3. **一键回收** `trae_export::cleanup_working_files()`：三处备份各留最新 1 批 + 删除可再生的解密快照，
   返回逐项回收明细；暴露为命令 `trae_cleanup_working_files`，在「Trae 会话记录」页加了「清理工作文件」按钮。
4. 导入过程中的明文工作副本（`workbuddy_import/<client>-<ts>/`，≈279 MB）在自检后即 `remove_dir_all`。

### 验证

- `cargo test -p wb-switch-core`：**35 项通过**（新增 4 项：三种备份命名解析、按客户端保留 + 兄弟文件整组删、
  子目录递归回收字节数、目录不存在时为空操作）。
- `cargo check --workspace --all-targets`：零错误。
- `tsc --noEmit`：零错误。
- 离线干跑（`examples/wb_dryrun.rs`，不触碰线上库）对两个会话自检全绿：

```
=== v0.0.5 不变量自检 ===
  本会话提问 16 条；其他会话提问 112 条（本会话 context 只允许出现自己的提问）
  context 提问文本不匹配 0 处 ✅
  助手消息 16 条：末项不是 finish 的 0 条 ✅；正文为空的 3 条
  revertible≠1 的消息 0 条 ✅；agent_run 1 条（真实为会话级，通常 1 条）
  首回合 workspace_folders = ["d://htw//签到"]
```

（「正文为空的 3 条」是源会话本身那些**只跑工具、没产出正文**的回合，不是转换缺陷；
按 `finish` 项必写原则，这些回合仍会渲染成结构完整的过程，只是回答区为空。）

### 教训

- **排查「用户还在报同一个问题」时，第一步是确认用户跑的是哪一版、库里到底是哪一版写的数据**——
  否则会把「新代码没生效」误判成「修复无效」。
- **`context` 这类「版本敏感的巨型 JSON」只能借字段形状、不能抄值**。它把会话的「问题文本 + 工作区 +
  文件索引结果 + 技能列表版本」都编码在内，抄一份等于把别的会话整个搬过来。
- **可见的回答不在 `history_v2` 里，而在 `chat_message_task.content` 末尾的 `finish` 项**——
  这条只有读真实行才能发现，schema 上看不出来。

---

## 九、第三轮真机测试：三条抱怨的根因与「不该做的工作量」（v0.0.6，2026-10-04）

### 现象

1. **「我就导一个会话，为什么要解密这么多数据」**——进度条上滚的是
   `加密回写 34304/71477 页 (48%)`。
2. **「显示有问题，超出了显示范围」**——导入弹窗比窗口还高，底部日志被裁掉。
3. **「Trae 的会话记录加载很慢，你已经忽略 3 次没有修复他了」**。

### 先说第 3 条：这次找到的真凶在**工具自己**身上，不在 Trae 身上

`src/pages/TraeRecordsPage.tsx` 的 `refresh()` 原来是这么写的：

```ts
let st = await api.traeDecryptedStatus(key);   // ← 这里会对 ~180 张表逐个 count(*)
if (st.exists) {
  const res = await api.traeDecryptWithSavedKey(key);   // ← 整库 279 MB 重新解密
  ...
}
```

也就是说：**每次打开「Trae 会话记录」页（以及每次切客户端、每次导入/删除/清理之后），
工具都会把 279 MB 的库重新解密一遍，再对全部表做一次行数统计。**
再加上当时解密是**单线程**跑 71477 页（AES-CBC + HMAC-SHA512），
一次刷新几秒钟是必然的。前两轮我一直盯着 Trae 客户端侧的数据结构，
没有回头量工具自己的刷新路径——这就是「忽略 3 次」的原因。

### 第 1 条：SQLCipher 逐页独立加密 ⇒ 本来就**不必**重写整库

SQLCipher 的每页自带随机 IV 与 HMAC、**页号参与 HMAC**、页长恒 4096，
页与页之间没有任何依赖。这意味着：

> 只要不改这一页，它的**原密文**就是新库里合法的一页，直接抄过去即可。

改动前的导入链路做了 **3 遍**整库运算：解密（71477 页）→ 加密（71477 页）→ 再解密自检（71477 页）。
改动后只剩 **1 遍只读扫描**（用来判断哪些页真的变了）+ 对变动页加密 + 对变动页回读校验。

### 修复

**A. 解密快照变成「带元信息的缓存」（`trae_export.rs`）**

- 新增 `SnapshotMeta`（`decrypted/<key>.db.meta.json`）：源库大小 / mtime / 首页 salt /
  首页 `change_counter`·`page_count`·`schema_cookie` / 页数 / **表行数**。
- `ensure_decrypted(client_key)`：签名一致 → **零解密**直接复用；不一致 → 重新解密并写元信息。
  解密前后各取一次签名，只有前后一致才落盘（防止「解密期间实时库被客户端改写」）。
- 关键前提：**Trae 的库跑在 WAL 模式，未 checkpoint 前实时库文件的字节不会变**，
  所以「大小 + mtime + 首页 salt + 首页字段」一致就等价于「快照仍是实时库的纯解密结果」。
- `trae_decrypted_status` 不再逐表 `count(*)`，行数直接读元信息缓存。
- 快照被彻底改造成**只读缓存**：读路径改用 `reader_plain_path()`——
  WAL 有待合并帧时复制一份快照、在**副本**上合并，绝不动快照本体
  （导入流程拿快照当差异比对基准）。副本按 `(快照大小/mtime, WAL 大小/mtime, 帧数)`
  做来源标记，没变就直接复用，不重复复制 279 MB。

**B. 增量加密回写 `write_db_incremental()`（`trae_import.rs`）**

```
out = 原库密文整份字节复制（零加解密）
for 每一页 N:
    解密(原库, N) 与 目标明文页 N 比对
    不同 → 用**原库首页的 salt** 重新加密该页 → 写到同一偏移 → 立刻回读解密比对
   （新增页：库增长出的页，全部需要加密）
```

- **salt 必须沿用原值**：mac key = PBKDF2(密钥, salt)，换 salt 会让所有未变动页的 HMAC 失效。
- 只校验变动页（未变动页是逐字节复制，天然有效），外加首页 HMAC 与文件大小自检。
- 导入流程随之改写：`ensure_decrypted` 拿快照 → 复制为工作明文 → 合并 WAL → 转换写入 →
  `write_db_incremental` → 备份 + 原子替换 → **把工作明文提升为新快照**（写入元信息）。
  去掉了原来那次「再解密整库做自检」。
- 删除会话（`trae_delete.rs`）同样改为增量回写，并在替换成功后提升快照；
  顺手去掉了「同步删解密库」那一步（它原本会就地改写快照，破坏上面的不变量）。

**C. 解密并行化（`trae_decrypt.rs`）**

`decrypt_database` 按页分块、多线程（≤8）并行，主线程只报进度
（避免把 `&dyn Fn` 塞进子线程而要求 `Sync`）。

**D. 弹窗超出显示范围（第 2 条）**

- `ui/dialog.tsx` 基础样式加 `max-h-[85vh] overflow-y-auto`（所有弹窗都不再超过窗口）。
- 导入弹窗改成「头/底固定 + 正文自身滚动」：
  `max-h-[86vh] grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden` + 正文 `min-h-0 overflow-y-auto`；
  会话列表 `max-h-72 → max-h-60`、日志区 `max-h-40 → max-h-36`。

### 真机实测（`examples/wb_incr_check.rs`，真实 279 MB / 71477 页库）

```
[1] 解密实时库 → ref.db                      71477 页 / 62 表 / 453 ms（并行）
[2] 复制为 new.db 并做一次真实改动（改 3 行会话标题）
[3] write_db_incremental(实时库 + new.db → out.db)
    全库 71477 页（原 71477 页）｜重写 5 页（新增 0 页）｜0.02 MB｜514 ms
[4] 解密 out.db 并与 new.db 逐字节比对        ✓ 全部 71477 页逐字节一致
[5] ensure_decrypted                         reused=true pages=71477  3 ms
```

| | 改动前 | 改动后 |
| --- | --- | --- |
| 记录页每次刷新 | 整库解密（单线程，数秒）+ ~180 张表 `count(*)` | 签名一致 → **0 次解密**（3 ms） |
| 导入一个会话的加密量 | 71477 页（279 MB） | **5 页**（0.02 MB）*实测小额改动；真实导入为变动页数 |
| 导入链路整库运算 | 解密 + 加密 + 解密自检（3 遍） | 1 遍只读扫描 + 变动页 |

### 教训

- **抱怨「慢」的时候，先量工具自己的路径**。前两轮我一直在查 Trae 客户端侧的 `context` /
  技能列表，而真正的每次刷新整库解密就在自家页面的 `refresh()` 里，一眼可查。
- **SQLCipher 的页独立性是一份可用的红利**：既然每页自带 IV/HMAC，就不存在「必须整体重写」的理由。
  唯一不能动的是**首页 salt**。
- **缓存要有明确的不变量，并且只给一个消费者用**。快照必须是「实时库的纯解密结果」，
  读路径要合并 WAL 就必须用副本，否则导入的差异比对基准会被悄悄改坏。

---

## 十、第四轮真机测试：导入直接失败 + 日志重复（v0.0.7，2026-10-04）

### 现象

1. **「新版本直接导入失败了」**——红色报错：
   `导入失败: 写入 agent_run 失败: UNIQUE constraint failed: agent_run.agent_run_id`
2. **「输出日志总是重复输出」**——日志面板里每一条都成对出现：
   ```
   已解析 1 个 WorkBuddy 会话
   已解析 1 个 WorkBuddy 会话
   目标客户端 TRAE SOLO CN 正在运行，先将其退出…
   目标客户端 TRAE SOLO CN 正在运行，先将其退出…
   ```

### 第 1 条：库里残留了「孤儿 agent_run 行」

先把**活库**重新解密（`examples/wb_probe.rs`）后统计 `agent_run`：

```
agent_run 共 25 行，其中 14 行是孤儿（session_id 在 chat_session 里查不到）
  0875053a-69a2-5afe-a020-77a07f0375aa   sess=6ac23c9a183522f035e04d36   孤儿
  b99b8fa6-a400-56dd-8826-172f5cd94dae   sess=6ac23c9a183522f035e04d36   孤儿
  …
```

`6ac23c9a…` 正是我们**上一轮导入过、后来被删掉**的会话。也就是说：**客户端（或用户在 Trae 里
手动删除）删会话时只清了 `chat_session`，把 `agent_run` 留下了。**

为什么这会致命？因为 `agent_run_id` 是全库**唯一**一个由会话 id「确定性」推导出来的 id：

```rust
let sid    = det_session_id(&src.id, start);          // hex(start) + wb_id 前 16 位
let run_id = uuid5(&format!("{sid}:{start}"));        // ← 每次导入都算出同一个值
```

而当时的清理逻辑是**有条件的**：

```rust
let already = SELECT count(*) FROM chat_session WHERE session_id = sid;
if already > 0 { delete_session_rows(...) }           // ← chat_session 查不到就完全不清
```

会话行已被客户端删掉 ⇒ `already == 0` ⇒ 不清 ⇒ 紧接着 `INSERT agent_run(run_id)` 撞 UNIQUE ⇒
**整次导入在第一个会话就炸掉**。逐条会话核算确认，全部 WorkBuddy 会话里只有
`183522f0…`「向助手打招呼」会命中（其余 23 条都算不出库里已存在的 run id），与用户截图完全一致。

**修复：无条件清，且再按 id 兜一遍**（`purge_session_rows`）

```rust
let sid = det_session_id(&src.id, start);
let run_id = uuid5(&format!("{sid}:{start}"));
let wiped = purge_session_rows(conn, &sid, &run_id)?;   // ← 不再看 chat_session 在不在
if wiped > 0 { on_log("   清理同源会话的旧记录 {wiped} 行（含客户端删除后残留的孤儿行）"); }
```

- 先按 `session_id` 清全部关联表（含两张 FTS 虚表）；
- 再 `DELETE FROM agent_run WHERE agent_run_id = ?` 兜一遍（孤儿行的 `session_id` 可能为空/被改写）；
- 清理天然幂等：没东西可清就是 0 行、无副作用。

顺带把「单条会话读不出来」从**整批失败**改成**跳过并记日志**（列表过期、正文文件被清掉、
或误选了「无正文」的会话时，不该让其它会话一起白跑），并在写入失败的报错里带上会话名。

### 第 2 条：`listen()` 是异步的，React StrictMode 下会残留一个监听器

```tsx
useEffect(() => {
  let un;
  void listen("trae-workbuddy-progress", cb).then((u) => { un = u; });
  return () => un?.();          // ← 首轮清理时 un 还是 undefined
}, []);
```

开发模式下 React 会「挂载 → 卸载 → 再挂载」跑两遍 effect。首轮清理执行时
`listen()` 的 Promise 还没 resolve，`un` 仍是 `undefined`，于是**第一个监听器永远注销不掉**；
第二次 effect 又注册一个 ⇒ 每条进度事件被投递两次 ⇒ 日志成对重复。实测的成对交错顺序
（1,1,2,2,3,3…）与「同一事件被两个监听器同时收下」完全吻合。

修复：用一个 `active` 标记兜住竞态——如果监听器拿到手时组件已经卸载，就**立刻注销**它。
`WorkbuddyImportPage.tsx`（T39 前叫 `workbuddy-import-card.tsx`）与 `TraeRecordsPage.tsx`（`trae-import-progress`）两处同模式都改了。

### 验证

新增 `examples/wb_write_probe.rs`：在**真实 279 MB 库的副本**上离线调 `write_session`，
既复现事故、也做回归（不碰线上库）。

```
源会话  : 「向助手打招呼」2 个回合
预期 run: 0875053a-69a2-5afe-a020-77a07f0375aa
写入前：同 id 的 agent_run 1 行 / chat_session 0 行
        ↑ 正是线上事故的形态：孤儿 run 行（客户端删会话留下的）
      清理同源会话的旧记录 36 行（含客户端删除后残留的孤儿行）
✅ 写入成功：2 回合 / 4 消息 / 4 history / 清理旧行 1
写入后：agent_run 1 行（必须=1）/ chat_turn 2 / task 2
==> 通过
```

同一工具再跑一条工具密集型会话（`f92b521c…` 23 回合 / 700 工具步骤）：写入成功，
`task` 行数 == `chat_turn` 行数不变量成立。

- `cargo test -p wb-switch-core`：**40 passed / 0 failed**（新增两项：孤儿 run 行清理后同 id 可重写、
  无残骸时为空操作。测试里先用同一 id INSERT 一次**断言确实会撞 UNIQUE**，再验证清理后能写入）。
- `cargo check --workspace --all-targets` 零错误；`tsc --noEmit` 零错误。
- 版本号 **0.0.6 → 0.0.7**（五处同步），方便确认跑的是新构建。

### 教训

- **「唯一索引 + 确定性 id」= 必须假设库里随时有同 id 的史前残留。**
  只要 id 是算出来的（而不是随机的），写之前就得无条件清一遍——
  「目标行不存在」不等于「没有东西会撞唯一约束」。这类残骸往往来自**别的程序删数据时没删全**，
  不能假定只有自己写过这张表。
- **随机 id 是天然的碰撞护城河。** `hex_id_at` 后 16 位用 UUID v4 随机填充，
  所以 message/turn/task 的 id 从不重复；唯独 `agent_run_id`（uuid5）是确定性的，
  事故也只发生在它身上——这不是巧合。
- **异步注册的事件监听，清理函数必须处理「还没注册完就卸载」的竞态**，
  否则 StrictMode（或任何快速重挂载）都会悄悄留下第二份监听器。

---

## 十一、入口重组：WorkBuddy 导入独立成页（v0.0.8，2026-10-04）

**问题（交互层）**：导入能力一直被塞在「Trae 会话记录」页最底部，和该页的主线
（客户端选择 → 解密状态 → 会话列表 → 详情/导出/删除）混在一起。用户需要一路滚到底才能找到它，
而且它本质上是**跨工具的迁移**（WorkBuddy → Trae），和「查看/管理 Trae 自己会话」不是一类事。

**改法**：

- 新增路由 `/workbuddy-import` 与页面 `src/pages/WorkbuddyImportPage.tsx`（只负责标题 + 承载卡片）。
- 侧栏拆成两个区域：Trae 自身的「Trae 账号管理 / Trae 会话记录」一组，
  下方用分隔线 + `数据迁移` 小标题另起一组，放「WorkBuddy 会话导入」。
- 侧栏导航项抽成 `SidebarLink` 组件（原先是两段重复的 `className` 计算函数），新增入口不再复制粘贴。
- `TraeRecordsPage` 移除卡片引用与 import，页面回归单一职责。
- 卡片内部逻辑零改动：独立页面下 `defaultClientKey` 传 `null`，
  卡片自身的 `prev ?? defaultClientKey ?? cand.candidates[0]?.client_key` 会回落到第一个候选客户端。

**版本号 0.0.7 → 0.0.8**（五处同步）。

**校验**：`tsc --noEmit` 零错误；`vite build` 通过（1920 modules）；`cargo check --workspace --all-targets` 通过。

---

## 十二、反向：Trae 会话导出到 WorkBuddy（v0.0.9，2026-10-04）

**需求**：此前只做了 WorkBuddy → Trae（`workbuddy_import`）。本轮补上反方向——
把 Trae 里的会话搬到本机 WorkBuddy，让迁移变成双向。

### 1. 目标格式：WorkBuddy 里「一个会话」= 两样东西

| 组成 | 位置 | 说明 |
| --- | --- | --- |
| 正文 | `~/.workbuddy/projects/{工作区key}/{会话id}.jsonl` | 一行一个事件 |
| 元数据 | `~/.workbuddy/workbuddy.db` → `sessions` 表一行 | `id / cwd / user_id / title / status / created_at / updated_at` 等 |

- **工作区 key**：cwd 小写后**删掉盘符冒号**，再把 `\` `/` 换成 `-`。
  实测三例：`D:/htw/test` → `d-htw-test`、`D:/htw/签到` → `d-htw-签到`、
  `D:/htw/签到/trae-switch-cn` → `d-htw-签到-trae-switch-cn`。
  > 首版写成「把 `:` `\` `/` 全部换成 `-`」，`D:/htw/test` 会得到 `d--htw-test`（多一个横线），
  > 被单测 `workspace_key_matches_real_dirs` 当场挡下。规则是「删冒号」而不是「冒号换横线」。
- **正文事件骨架**（逐字取自真实原生会话）：

  ```text
  session-meta ×2        （成对，宿主/进程元信息）
  message(user)          content=[{type:"input_text", text}]
  file-history-snapshot  （每条用户消息后各一条，trackedFileBackups 可为空对象）
  reasoning              rawContent=[{type:"reasoning_text", text}]
  message(assistant)     content=[{type:"output_text", text, providerData:{annotations:[]}}]
  function_call          arguments 是 JSON **字符串**；callId 与结果配对
  function_call_result   output={type:"text", text}
  ...
  ai-title               aiTitle，放文件末尾
  ```

- 事件 id 是 **UUID v7**（前缀 = 48 位毫秒时间戳），复刻用同一套构造。
- `sessions/*.json`（pid 心跳）与 `.file-rollback.ndjson` **不是**会话数据，无需生成。

### 2. Trae 侧字段来源（与 `workbuddy_import` 的写入位置一一对应）

| 内容 | Trae 位置 |
| --- | --- |
| 用户提问 | `chat_message_general.content` |
| 助手过程叙述 | `plan_item.thought` |
| 思考 | `plan_item.reasoning_content` |
| 工具调用 | `plan_item.tool_call_info.params` + `meta.llm_toolcall_id` |
| 工具结果 | `plan_item.tool_call_info.result`（结构化 `data`，按常见键取文本） |
| 回合最终回答 | 末项 `plan_item`（`tool_call_info.name == "finish"`）的 `params.summary` |

### 3. 三条设计约束

1. **会话 id 确定性派生**：`uuid5(trae-wb:{client_key}:{session_id})`，再把版本位改写成 4
   （外形与原生随机 UUID 一致）。于是同一 Trae 会话重复导出命中同一条，走
   `INSERT OR REPLACE` 覆盖，不会堆重复项。单测 `det_session_uuid_is_stable_and_v4_shaped` 守着。
2. **只借模板的静态字段**：以 `sessions` 最近一条真实行为模板，只取
   `model / mode / permission_mode / source_mode / use_sandbox_cli / context_window /
   thought_level / transport`；`addon_selection / buddy_binding_json / group_id / group_title /
   expert_* / session_settings / project_id` 一律留空。这些列描述的是**模板那条会话的上下文**
   （场景、工作区、专家、分组），照抄会让导入的会话冒充别人的上下文——反向导入正是栽在这里
   （见 `workbuddy_import` 第 5 条硬约束）。单测 `insert_sessions_writes_against_real_schema`
   用**真实 DDL** 建表，显式断言这些列必须为 `NULL`。
3. **写盘顺序：先正文、后数据库**。万一插 `sessions` 行失败，多出来的 JSONL 在客户端里
   不可见（列表读的是库表），属无害残骸；反过来则会留下一条打不开的幽灵会话。
   数据库写失败时回滚到备份，并清掉本轮新建的 JSONL。

### 4. 为什么导出前要退出 WorkBuddy

`workbuddy.db` 跑在 WAL 模式，且客户端会把会话列表缓存在内存里。导出前先结束 WorkBuddy
进程（写完再拉起）：
- 避免客户端退出时用旧快照覆盖刚写入的行；
- 保证重新打开后列表立刻就带上导入的会话；
- 客户端关闭后可以安全 `PRAGMA wal_checkpoint(TRUNCATE)`，退出时留下干净状态。

与「导入 Trae 前先退出 Trae 客户端」同构。主程序路径按 `%ProgramFiles%\WorkBuddy\WorkBuddy.exe`
→ `%LOCALAPPDATA%\Programs\WorkBuddy\WorkBuddy.exe` → 询问运行中进程 的顺序解析；
探测不到时只提示「手动启动」，不影响数据写入。

### 5. 校验（离线闭环）

新增 `examples/wb_export_probe.rs`：真实 Trae 库 → `convert` → 落 JSONL →
用 `workbuddy_source::parse_body`（**与 WorkBuddy 读取端同一套解析**）读回来做比对。

> 为此把 `workbuddy_source::load_body` 拆出 `pub fn parse_body(session_id, text)`，
> 让「写出去 → 读回来」共用一份解析，而不是各写一份可能同时出错的实现。

三个真实会话的实测结果（`cargo run -p wb-switch-core --example wb_export_probe`）：

| 会话 | 回合 | 工具步骤 | 事件 | 体积 | 结果 |
| --- | --- | --- | --- | --- | --- |
| 交叉口流量分析源码整合 | 10 | 1784 | 4258 | 8.2 MB | 全项 ✓ |
| 员工信息提醒程序开发 | 82 | 1579 | 4444 | 53.6 MB | 全项 ✓（11 个回合源会话本就无回答） |
| 开发中级经济师刷题程序 | 3 | 1189 | 3125 | 8.8 MB | 全项 ✓ |

校验项：逐行 JSON 合法、回合数一致、提问与最终回答非空、思考/工具调用/结果计数与解析回读一致、
每个 `function_call` 都有配对结果、提问文本往返零改写。

**两个被校验抓出来的真问题**（都已修）：
- `workspace_key` 规则写错（`d--htw-test`）——见上文第 1 节。
- 「有回答的回合数」口径写错：原先数 `finish` 项，但**一个 Trae 回合可能有多条 assistant
  消息、各自带一个 `finish`**（工具打断后续写时就是这样），而 WorkBuddy 只认「下一条用户
  消息之前的最后一条 assistant」。改为 `count_answered()` 按 WorkBuddy 的解析语义重算。
- 顺带补了 `finish.summary` 为空时的兜底：从 `history_v2` 取该助手消息的**最后一段非空文本**
  （取最后一段而非整段拼接，避免与已写出的 `thought` 重复）。

> 关于「源会话无回答的回合」：实测 82 回合里有 11 个在 Trae 侧就是空的
> （`chat_message_task.messages` 为空且 `history_v2` 无行，属被中断/重试的回合）。
> 这是源数据实情而非转换缺陷——工具自己的 MD 导出对这些回合同样只有「（无记录）」。
> 因此校验把它作为**对账项**（与转换期实际写出的回答数比对），而不是要求必须为 0。

### 6. 校验命令汇总

```bash
cargo test -p wb-switch-core                                  # 49 passed（新增 8 项）
cargo check --workspace --all-targets                         # 零错误
cargo run -p wb-switch-core --example wb_export_probe         # 真实库离线闭环
npx tsc --noEmit && npx vite build                            # 前端零错误
```

### 7. 待真机验证（未做，需用户在界面上跑一次）

离线校验覆盖了「转换 + 正文解析」，`insert_sessions` 用真实 DDL 覆盖了「插库」。
**未覆盖**的是：真实 `workbuddy.db` 上的写入、WorkBuddy 进程退出/拉起、以及导入的会话在
WorkBuddy 列表里的实际显示。首次真机运行时请重点确认：

1. 导出后 WorkBuddy 自动重启，列表里能看到新会话，标题与轮数正确；
2. 打开会话能正常渲染提问、回答、思考与工具卡片；
3. `sessions.cwd` 对应的工作区出现在侧栏，会话归在正确的工作区下。

**版本号 0.0.8 → 0.0.9**（五处同步）。按约定**未发布 GitHub**。

---

## 十三、账号显示名 / 同标题副本判新 / 本机清理（v0.0.10，2026-10-04）

本轮四件事：①分不清哪个 WorkBuddy 账号是哪个；②两个账号有同一段对话时看不出谁最新；
③导入列表里选不中的「无正文」会话与「已删除」会话怎么清干净；④写任务书。

### 1. 账号显示名（`workbuddy_accounts.rs`）

WorkBuddy **没有本地账号表** —— `workbuddy.db` 里只有 10 张表，没有任何 accounts 相关表，
会话只记 `sessions.user_id`。所以显示名只能从三处拼，按可信度由低到高依次合并：

| 优先级 | 来源 | 说明 |
| --- | --- | --- |
| 1 | `~/.workbuddy/logs/*.log` | 遥测行 `\"userId\":\"…\",\"username\":\"…\",\"userNickname\":\"…\"`（JSON 被嵌进日志后引号会变成 `\"` 甚至 `\\"`，解析要容忍） |
| 2 | `~/.wb-switch/accounts.json` | WorkBuddy 账号切换工具留下的账号库，含 `profile_raw.phoneNumber`（装了才有） |
| 3 | `storage/skeleton/account-snapshot.json` | WorkBuddy 自己写的，**只覆盖当前登录账号**，最权威 |

- 显示成 `昵称（uid …97eac1）`，次行给 `手机号 · 个人版 · 免费版`（与显示名重复的自动去掉）。
- 解析不出来一律回退 `uid …尾6`，**绝不编造**。
- **只读 uid / 昵称 / 手机号 / 邮箱 / 类型 / 版本，绝不读 token**；`nickname` 若是
  `{$wbEncrypted: …}` 加密信封对象则视为不可用（当字符串透传会让前端 React error #31 白屏）。
- 解析前先做 UUID 形状校验（36 字符、4 个连字符）：日志里还有
  `[AuthenticationManager] userId changed: <init> -> <empty>` 这种纯文本行，不校验就会误命中。

**本机实测**：`…97eac1` → `13780001455`（当前登录 / 免费版）·`…be81ba` → `弦ྂ思ྂ`（手机
18858464309）·`…5d5e70` → `19550125362`，来源都标注为「运行日志+账号库(±账号快照)」。

### 2. 同标题副本判新（`workbuddy_source.rs`）

参考 `workbuddy-switch-main` 的 `session_link.rs`：JSONL 逐行摘要 + 有序前缀判「谁包含谁」。
落地时做了三处适配：

1. **归一掉自己的会话 id**：同一段对话在两个账号下的副本 sessionId 不同，其余逐字相同 ——
   替换成固定标记后逐行摘要，两份就能拿到**同一个总摘要**，这是「内容完全相同」的判据。
2. **判定顺序：内容包含关系优先于时间**。某份是其它所有成员的有序前缀 ⇒ 它最新，即使
   `updated_at` 更旧（多账号/多设备下时钟不可靠）。没有包含关系（内容分叉）才退回时间戳。
3. **逐行完全相同 ⇒ 回落到时间戳**。全是「互相覆盖」的候选时，靠位置先后定输赢会把列表
   靠前的旧份选成最新 —— 单测 `identical_content_falls_back_to_timestamp` 守着这条。

**成本控制**：逐行摘要只对**同标题分组**内成员做，且分组 ≤ 8 个、单文件 ≤ 64 MB；
不在重复组里的会话 `body_lines` 保持 0（前端只显示 > 0 的），避免在大仓库上读上百个 10 MB 级正文。

**本机实测**（`wb_audit` 第 2 节）：标题「查看 cargo 测试后台任务结果」两份 ——
97eac1 是 3348 行 / 14.6 MB，be81ba 是 3052 行 / 13.0 MB，系统判出后者
「比最新副本少 296 条记录」；另有一对「向助手打招呼」内容分叉（10 条 vs 12 条），也如实标注。

### 3. 本机清理（`workbuddy_cleanup.rs`）

**调研**：本机 `workbuddy.db` 21 行 —— 15 行 `deleted_at` 非空、15 行无正文（含交集），
3 行是「有行无正文且未删除」的纯孤儿，正好就是导入列表里显示「无正文」的那三条。
另有 `edge_sync_mapping` 表 6 条按 `session_id` 建的**云端映射**（`msg_channel` 形如
`convmsg:<uid>`）—— 这三条孤儿全在里面。**只删 `sessions` 会留下孤儿映射，下次同步可能把会话拉回来。**

**六类垃圾**（都走「扫描 → 逐项勾选 → 确认」）：

| 分类 | 判定 | 处置 |
| --- | --- | --- |
| `deleted` | `deleted_at` 非空 | 删行 + 正文/附属文件/工作区移入回收站 |
| `orphan_meta` | 有行、无正文、未删除 | 删行（含 `session_usage`、`edge_sync_mapping`） |
| `orphan_body` | 有 JSONL、无行 | 文件移入回收站 |
| `stale_artifacts` | `changes-detail/<id>`、`file-history/<id>`、`file-tree-manifests/<id>.json` 指向已不存在的会话 | 移入回收站 |
| `stale_workspace` | `workspace/sessions/<id>` 指向已不存在的会话 | 移入回收站 |
| `logs_old` / `traces_old` | `logs/<日期>`、`logs/sandbox`、`traces/<编号>` | 移入回收站（早于保留窗口的标 `recommended`） |

**安全约束（逐条对应实现）**：`scan()` 只读 → 清除前先退出 WorkBuddy（否则改库会被客户端
内存快照覆盖）→ 整份备份 `workbuddy.db` 与 `edge-sync-mapping-v*.db`（含 WAL/SHM）→
删行（`sessions` + `session_usage` + 两类同步映射）→ 文件**移入回收站**保持相对路径、可搬回
（只有显式勾「彻底删除」才 unlink）→ 重新读取校验 → 重启客户端。确认弹窗逐条列出将被处理的
绝对路径，彻底删除前用加粗红字警告不可逆。

另附只读的 `large_holdings`，解释「为什么 `~/.workbuddy` 有 4 GB」并标明哪些**勿清**
（`workspace` 2.74 GB 是当前会话的改动备份，是回滚依据）。

**本机实测（只读扫描）**：39 项 / **542.4 MB**，推荐 24 项 / 185.9 MB。

### 4. 为什么清除没有代跑

清除会**先退出 WorkBuddy**，而当前这轮对话就跑在 WorkBuddy 里 —— 代跑会把会话本身掐断；
且它写的是用户真实数据，按约定由用户在自己选的时间点确认执行。

### 5. 校验

```bash
cargo test -p wb-switch-core                    # 69 passed / 0 failed（本轮新增 15 项）
cargo check --workspace --all-targets           # 零错误
npx tsc --noEmit && npx vite build              # 零错误
cargo run -p wb-switch-core --example wb_audit  # 真机只读核查（账号 / 判新 / 清理扫描）
```

**版本号 0.0.9 → 0.0.10**（五处同步）。按约定**未发布 GitHub**。

---

## 十四、修复：导入后 Trae 看不到工具过程（v0.0.11，2026-10-04）

### 1. 现象

用户反馈：导入到 Trae 的会话，WorkBuddy 里能看到的工具调用 / 思考过程，在 Trae 里「显示奇怪」。
实测截图与库数据共同指向：**Trae 会话里只剩每条助手的最终回答（finish 项），工具卡片全部不见**。

### 2. 定位过程（方法可复用）

1. **在同一个库里找对照组**：`solo-cn.db` 里既有导入会话（`6ac22cadf92b…`，本工具写的），
   也有原生会话（`6abf7aed8532…`，Trae 自己写的）。两者结构同源，可直接 diff。
2. **diff `chat_message_task.content`**（两边都是 `{"task_id":…,"messages":[plan_item…]}`），
   逐字段并集对比：
   - `tool_call_info.params`：原生 **100% 是对象**；导入 **95% 是字符串**。
   - `tool_call_info` 缺 `already_emitted_generating_event` / `already_emitted_run_event`
     （原生 2791/2791 项都有：前者默认 `false`、后者默认 `true`；导入只有末项 finish 有）。
   - `plan_item` 缺 `confirm_info` / `parent_agent_run_ids`（原生多数为 `null`）。
   - `result` 缺 `error_variant` / `is_async`（原生多数为 `null`）。
3. **去客户端核对结构体定义**：Trae 的 agent 逻辑是原生 Rust 模块，位于
   `%LOCALAPPDATA%\Programs\TRAE SOLO CN\resources\app\modules\ai-agent\{ai_agent,harness}.dll`。
   DLL 里能直接搜到字段名表（`id name params result meta already_emitted_generating_event
   already_emitted_run_event …`）——**说明客户端按固定字段反序列化，缺字段会整条解析失败**。

### 3. 根因

**① 双重编码（主因）** —— `workbuddy_source.rs` 解析 `function_call`：

```rust
// 旧写法：
text: v.get("arguments").map(|a| a.to_string()).unwrap_or_default(),
```

真实 jsonl 里 `arguments` **本身就是字符串**：

```json
{"type":"function_call","name":"Read","arguments":"{\"file_path\": \"D:/…\"}"}
```

`Value::to_string()` 把这个**字符串值**再序列化一遍 → `"{\"file_path\": \"D:/…\"}"`
（外层多一对引号 + 内部转义）。下游 `parse_params` 解析它得 `Value::String`，写进
`tool_call_info.params` 就是字符串 → Trae 按对象取字段取不到 → 卡片残缺/异常。

**② 字段缺失（并发因）** —— `build_task_content` 只给末项 finish 写了
`already_emitted_generating_event` / `already_emitted_run_event`，也没写
`confirm_info` / `parent_agent_run_ids` / `error_variant` / `is_async`。客户端 Rust 端
反序列化缺字段 → **整条 plan_item 被丢弃** → 工具卡片不渲染。

### 4. 修复

`crates/wb-switch-core/src/modules/workbuddy_source.rs`

- `function_call`：`arguments` 优先 `as_str()` 取内容，只有本身是对象/数组时才 `to_string()`。
- `function_call_result`：`output` 分四种形态 —— 字符串直接用 / `{type,text}` 取 text /
  `[{…,text}]` 拼各段 text / 其余序列化兜底。**绝不**对字符串直接 `to_string()`。

`crates/wb-switch-core/src/modules/workbuddy_import.rs`

- `parse_params`：结果是字符串且内容又是 JSON 对象/数组时**再拆一层**（兜底防双重编码）。
- `build_task_content`：补齐上述全部字段；`hide` 由 `false` 改为 `null`（对齐原生）。

### 5. 验证

- 单测断言：`params` 必须是对象、`tool_call_info` 必带两个 `already_emitted_*`、
  `plan_item` 必带 `confirm_info`/`parent_agent_run_ids`、`result` 必带 `error_variant`/`is_async`。
- **真机自检**（读本机真实数据、不写库、默认 ignored）：
  `cargo test -p wb-switch-core -- --ignored real_plan_item_shape --nocapture`
  → 实测 **22 回合 / 1142 个工具项，字段全部齐备**。
- **注意**：修复只对新导入生效；库里已有的坏数据需**重新导入一次**
  （导入会先 `delete_session_rows` 清旧行再写）。

**版本号 0.0.10 → 0.0.11**（五处同步）。按约定**未发布 GitHub**。

---

## 十五、修复：工具卡片「卡在进行中」+ 详细过程为空（v0.0.12，2026-10-04）

### 1. 反馈

用户重新导入后：「最后的导入状态不错，但之前异常的部分没有恢复」——
工具卡片**外壳**出现了（有「任务耗时」折叠条），但展开后**详细过程仍然不对**。

### 2. 定位路径（这次的关键是**分清数据层与渲染层**）

**第一步：先证明数据层没问题。**

新增只读探针 `crates/wb-switch-core/examples/wb_live.rs`：

```
cargo run -p wb-switch-core --example wb_live -- solo-cn 6ac22cadf92b521cbac24f51
```

它走 `trae_export::reader_plain_path()`，读的是**「解密快照 + 未 checkpoint 的 WAL」**。

> **为什么必须走这条**：Trae 的库跑在 WAL 模式，**客户端运行时的写入全在 WAL 里**，
> 主库文件 `database.db` 的 mtime 根本不变。只看 `decrypted/solo-cn.db`
> （纯解密快照）会读到「上次 checkpoint 时」的旧状态，**得不出客户端的真实所见**。
> 实测差异：快照 62 条消息 / 31 回合；实时视图 **64 条 / 32 回合**（用户在 Trae 里
> 又追问了一条「之前执行了什么任务」）。

结论：**31 个导入回合的字段全 OK，数据层无罪**。于是把矛头转向渲染层。

**第二步：读 Trae 的前端实现。**

`resources/app/node_modules/@byted-icube/ai-modules-chat/dist/index.mjs`
（14.6 MB，字段名未压缩、可直接搜）：

```js
transformToolCallStatus(e){
  switch(e){
    case "error": case "failed": return "failed";
    case "skipped": return "skipped";
    case "canceled": case "cancelled": return "canceled";
    case "running": case "pending": default: return "running";   // ← 兜底
    case "success": return "success";
  }
}
transformToolCallResult(e){
  return {status: this.transformToolCallStatus(e.status),
          errorMessage: e.error_message, data: e.data, render: e.render,
          isTruncated: e.is_truncated, interrupt: e.interrupt};
}
transformPlanItem(e,t,r){
  parentAgentRunIds: e.parent_agent_run_ids ?? [],
  thought: e.thought ?? "",
  toolCallInfo: {id, name, params: e.tool_call_info?.params ?? {}, result, meta},
  confirmInfo: e.confirm_info, hide: e.hide, ...
}
```

命令行卡片的判空逻辑读的是：

```js
a = i && t ? t.stderr || t.stdout : void 0;              // 输出内容
u = vZ(i,"display_command") || vJ(e) || vZ(i,"command"); // 命令行文本
```

### 3. 根因（两个）

**① 状态值不在 Trae 的词表里（主因）**

WorkBuddy 侧 `function_call_result.status` **恒为 `completed`**（实测 1259/1259），
而 Trae 前端只认 `success`/`failed`/`skipped`/`canceled`/`running`/`pending`。
导入**原样透传** `completed` → 落到 `default` → 被当成 **`running`（进行中）**
→ 卡片永远转圈，结果区不渲染。

实测对照：

| 字段 | 导入 | 原生 |
| --- | --- | --- |
| `result.status` | `completed` × **1188** | `success` × **2741** |
| `agent_status.status` | `completed` × 1220 | `running` × 2556 / `completed` × 55 |

**② 工具输出被整块丢弃（并发因）**

`build_task_content` 里 `result.data` 恒写成 `{}`，而 `Step.result`
（`workbuddy_source` 已解析好的工具输出）**完全没被使用**。
前端读 `data.stdout` / `data.display_command` → 取不到 → 卡片只剩空壳。

### 4. 修复

`crates/wb-switch-core/src/modules/workbuddy_import.rs`

- 新增 `map_tool_status(raw, ok)`：把状态归一到 Trae 的词表。
  `completed` / `success` / `partial_success` / `no_need_execute` / 空
  → `success`（无结果时 `failed`）；`error`/`failed` → `failed`；其余按词表归。
- 新增 `tool_result_data(tool, params, output)`：按工具分派写 `result.data`。
  - 命令行类（`Bash`/`Shell`/`Exec`/`PowerShell`/`RunCommand`）
    → 对齐 harness.dll 的 `RunCommandOutput`：
      `stdout` / `exit_code` / `command` / `display_command` / `cwd` / `stdout_complete`
  - 写改类（`Write`/`Edit`/`MultiEdit`/`DeleteFile`）
    → `can_show_diff` / `change_time` / `file_path` / `content`
  - 其余 → 通用 `content: [{type, text}]`（原生 MCP 类工具即此形态）
  - 输出为空仍写 `{}`，不硬塞空壳。

### 5. 验证

- 单测：`tool_status_speaks_trae_vocabulary`（穷举全部状态 × ok 组合）、
  `tool_result_data_keeps_output` → `cargo test -p wb-switch-core` **71 passed**。
- 真机自检扩展到断言 `result.status` 合法 + 有输出的工具 `result.data` 非空：
  → **24 回合 / 1309 个工具项**全过。
- `cargo check --workspace --all-targets` 通过；`npx tsc --noEmit` 零错误。
  （注：首次 `cargo check` 报 `os error 5` 写 codegen 资源被拒，**重跑即过** ——
  Windows 下 target 内文件被瞬时占用，非代码问题。）

### 6. 顺带弄清的一件事：为什么「同一个会话有两份」

`~/.workbuddy/projects/d-htw-签到/` 下同时存在：

| 文件 | 行数 | 归属 uid | 标题 | 最后写入 |
| --- | --- | --- | --- | --- |
| `8334395a-….jsonl` | 3052 | `6aa2c43b` | 查看 cargo 测试后台任务结果 | 22:03 |
| `f92b521c-….jsonl` | 4191 | `63e05cca`（现役） | 调整会话导入的标签栏排布 | 23:09 |

两者**起始时间戳完全相同**、**事件 id 逐条相同**（`01a1067e-…`、`01a1068d-…`…）
→ 是同一段对话在两个账号下的两份拷贝，后者多 1139 行。

这正是 T2「同标题副本判新」要处理的场景，也印证了用户「可能是历史记录没有清理」的直觉。

**版本号 0.0.11 → 0.0.12**（五处同步）。按约定**未发布 GitHub**。

---

## 十六、修复：把 WorkBuddy 的「系统注入」当成用户提问（v0.0.13）

### 1. 现象

用户反馈「之前还是有部分记录显示与 WorkBuddy 不一致」，附图中 Trae 里有一条**超大卡片**，
内容赫然是 `<conversation_history_summary> Summary: 1. **Primary Request and Intent:** …`。

### 2. 定位：新增只读比对探针 `examples/wb_diff.rs`

不看两边是很难定位的，所以先做了个「同屏对照」工具：左半边打印 **WorkBuddy 源 jsonl
解析出的回合**，右半边打印 **Trae 库（解密快照 + 合并 WAL）里实际落下的消息**，
逐条对齐后差异一目了然。

```bash
cargo run -p wb-switch-core --example wb_diff -- \
    solo-cn <trae会话id> <wb源会话id>
```

用它 + 一段直接读 jsonl 的统计脚本，得到结论：**源侧 36 个「回合」，只有 16 个是真实提问。**

### 3. 根因：`role=user` 里混着 4 类「客户端自己生成」的条目

WorkBuddy 把内部注入也写成 `role=user`。它们不是用户敲的字，界面也不会当提问展示。

| 类型 | 开头标记 | 有助手回答？ | 实测长度 |
| --- | --- | --- | --- |
| A 上下文压缩摘要 | `<conversation_history_summary>` | **否** | 6 447 / 10 249 / 11 143 / 18 015 |
| B cb 摘要 | `<cb_summary>` | **否** | 80 947 / 87 883 / 89 835 / 95 596 / **157 517** |
| C 自动续写指令 | `Please continue with the conversation based on the summarized context…` | 是 | 155 |
| D 后台任务通知 | `<task-notification>…` | 是 | 816～1 145 |

原实现 `extract_user_text` 的两处写法把问题放大：

1. **`<user_query>` 全文 `find`**：摘要正文里引用了 `<user_query>`（实测最多 **7 处**），
   于是从摘要里「捡」出一段毫不相干的文字——有时是源码片段，有时是上一轮的续写指令。
   这条解释了一个诡异现象：`<cb_summary>`（89 835 字符）最终变成了 155 字符的
   `Please continue with the conversation…`。
2. **剥离注入块也全文搜**：`<cb_summary>` 正文里成段引用 `<system-reminder>`，
   全文剥离把摘要**拦腰削断**——80 947 字符削成 8 614 字符的乱码。
3. **`<task-notification>` 块外还挂着指令尾巴**（`Use the TaskOutput tool with task_id="…"`、
   `IMPORTANT: Before responding, scroll back…`）。只剥标签会把这些文字留下当提问 → 6 条。

净效果：Trae 里多出 **19 条用户从没见过的用户消息**，并为其中 10 条无回答的注入
各补出一个**空白助手气泡**。

### 4. 修复

`workbuddy_source.rs`：

```rust
pub enum UserMsgKind { Real, SilentSummary, AutoTrigger }
pub fn classify_user_message(raw: &str) -> UserMsgKind   // 只看**行首**标记
fn strip_leading_blocks(text: &str) -> &str              // 只剥行首，遇非注入立即停手
fn extract_user_text(text: &str) -> String               // 先剥行首 → 再认 <user_query>
```

`parse_body` 的回合归属改为：

| 判定 | 处理 |
| --- | --- |
| `SilentSummary` | **整条丢弃**（无回答、界面上也没有） |
| `AutoTrigger` | 丢掉这层壳，**不闭合当前回合** —— 随后的助手回答并入上一个真实回合 |
| `Real` | 闭合上一回合、开新回合 |

这样既不会凭空多出用户消息，也不会丢掉助手的输出。
附带地把「注入与紧随其后的真实消息**时间戳倒挂**」（如摘要 ts 比它后面的提问大 14 秒）
一并消除，因为倒挂源头就是这些注入。

### 5. 效果与验证

- 该会话源侧 **36 回合 → 16 回合**，全部是真实提问；19 条注入消息 + 10 个空白气泡消失。
- 导出方向（`workbuddy_export`）共用同一套解析，一并修好。
- 新增单测 4 条（含两条回归护栏：`user_text_does_not_scan_body_for_tags`、
  `parse_body_drops_injections_and_keeps_replies`）。
- 真机自检新增断言「解析出的提问不得是注入、且不得为空」→ **31 回合 / 2 509 个工具项**全过；
  `cargo test -p wb-switch-core` → **80 passed**。

### 6. 需要知悉的代价

一次上下文压缩（摘要 + `Please continue`）或一次后台任务通知，会把其后的助手输出
**并进上一个真实回合**。这与 WorkBuddy 的语义一致——一个提问对应「到下次提问前的全部
助手输出」——代价是少数回合的工具项会变多（实测单回合最大 **234 项**）。
若日后觉得这样太长，可改为「为 `AutoTrigger` 单独建回合、用户消息留空」，
但那需要先确认 Trae 前端能渲染「没有用户消息的助手消息」，**目前没有验证过，故不采用**。

**版本号 0.0.12 → 0.0.13**（五处同步）。按约定**未发布 GitHub**。
