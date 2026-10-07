// ---------------------------------------------------------------------------
// Trae 模块：账号切换 / 记录解密导出 的类型定义（对应 src-tauri commands.rs 的 trae_* 命令）
// ---------------------------------------------------------------------------

export interface TraeInstalledClient {
  key: string;
  label: string;
  user_data_dir: string;
  installed: boolean;
  exe: string | null;
  has_login: boolean;
}

/** 单个客户端的「使用记忆」（对应 `client_usage::ClientUsage`）。 */
export interface TraeClientUsageEntry {
  key: string;
  /** 打开该客户端功能页（账号页 / 会话记录页）的次数。 */
  uses: number;
  /** 成功切换 / 回滚到该客户端账号的次数。 */
  switches: number;
  lastUsedAt: number;
  /** `switches * switchWeight + uses`，越大越常用。 */
  score: number;
}

/**
 * `trae_list_clients` 里的排序快照。
 *
 * `topPick` = 排在最前**且确有使用历史**的客户端 key；全是 0 分时是 `null`
 * —— 那种情况排第一的只是内置默认值，不该给它打「常用」徽标。
 */
export interface TraeClientUsageSnapshot {
  topPick: string | null;
  switchWeight: number;
  preference: string[];
  clients: TraeClientUsageEntry[];
}

export interface TraeCarrierFile {
  rel: string;
  len: number;
}

export interface TraeVaultMeta {
  id: string;
  client: string;
  /** 账号类型：carrier（登录态载体，可切换）/ oauth（网页凭证，可合成载体后切换）。 */
  kind?: "carrier" | "oauth";
  root_dir: string;
  entries: string[];
  files: TraeCarrierFile[];
  file_count: number;
  total_bytes: number;
  created_at: string;
  last_used_at?: string | null;
  verified_uid?: string | null;
}

/** 网页（OAuth）登录得到的凭证账号内容（对应后端 oauth.json）。 */
export interface TraeOAuthAccount {
  kind: "oauth";
  id: string;
  client: string;
  displayName: string;
  uid?: string | null;
  userName?: string | null;
  avatar?: string | null;
  tokenExp?: number;
  expiredAt?: string | null;
  refreshExpiredAt?: string | null;
  deviceId?: string;
  machineId?: string;
  appVersion?: string;
  deviceSource?: string;
  host?: string;
  userRegion?: string;
  loginSource?: string;
  createdAt?: string;
  updatedAt?: string;
}

/** 账号资料（GetUserInfo 昵称 / 手机号 + 积分包明细，缓存于 profile.json）。 */
export interface TraeAccountProfile {
  /** 真实昵称（GetUserInfo ScreenName，与 Trae 界面一致）。 */
  screen_name?: string | null;
  user_id?: string | null;
  /** 脱敏手机号。 */
  mobile?: string | null;
  avatar?: string | null;
  region?: string | null;
  /** 剩余积分（user_current_entitlement_list 计算）。 */
  credits?: number | null;
  credits_total?: number | null;
  credits_used?: number | null;
  /** 逐个积分包的明细。 */
  credit_packs?: TraeCreditPack[];
  /** 积分接口是否成功（token 过期时为 false，此时 `credit_error` 有原因）。 */
  credit_ok?: boolean;
  credit_error?: string | null;
  host?: string;
  fetched_at?: string;
}

/** 一个积分包（`user_current_entitlement_list` 的一项）。 */
export interface TraeCreditPack {
  /** 稳定 key（entitlement_id）。 */
  key: string;
  name: string;
  /** 分组名（「每日签到」等）。 */
  group: string;
  /** 总额度；`null` = 接口没给额度（不限量包，见 `unlimited`）。 */
  total: number | null;
  used: number;
  /** 剩余；`null` = 不限量。 */
  remaining: number | null;
  /** 额度缺失 = 这个包不限量（接口确实这么给，不是解析失败）。 */
  unlimited: boolean;
  /** 到期时间（毫秒）；`null` = 长期有效。 */
  expire_at: number | null;
  /** 1 = 有效。 */
  status: number;
}

/** 账号库里一个账号的积分状态（含无法查询的说明）。 */
export interface TraeCreditEntry {
  id: string;
  clientKey: string;
  uid: string;
  name: string;
  avatar: string | null;
  mobile: string | null;
  host: string | null;
  /** 有网页凭证（oauth.json）才谈得上联网查积分。 */
  queryable: boolean;
  ok: boolean;
  error: string | null;
  total: number | null;
  used: number | null;
  remaining: number | null;
  packCount: number;
  packs: TraeCreditPack[];
  /** 上次成功拉取资料的时间（毫秒）。 */
  updatedAt: number | null;
}

/** 账号库积分查询结果。 */
export interface TraeCreditsResult {
  ok: boolean;
  clientKey: string;
  accounts: TraeCreditEntry[];
  summary: { queried: number; succeeded: number; failed: number; totalRemaining: number };
  updatedAt: number | null;
  cached: boolean;
}

/** 手动刷新账号资料的结果。 */
export interface TraeProfileRefreshResult {
  id: string;
  profile: TraeAccountProfile;
  displayName?: string | null;
}

export interface TraeVaultEntry {
  id: string;
  meta: TraeVaultMeta | null;
  /** carrier（登录态载体）/ oauth（网页凭证，可合成载体后切换）。 */
  kind?: "carrier" | "oauth";
  /** kind=oauth 时的凭证详情。 */
  oauth?: TraeOAuthAccount | null;
  /** 自动获取的真实账号名（接口昵称 / oauth displayName / 载体 storage.json username）。 */
  displayName?: string | null;
  /** 接口拉取的账号资料（昵称 / 积分），网络失败时为空。 */
  profile?: TraeAccountProfile | null;
}

export interface TraeLiveAccount {
  clientKey: string;
  label: string;
  hasStorage: boolean;
  loggedIn: boolean;
  uid: string | null;
  username: string | null;
  avatarUrl: string | null;
  email: string | null;
  region: string | null;
  host: string;
  deviceId: string | null;
  machineId: string | null;
  devDeviceId: string | null;
  tokenExp: number;
  refreshExp: number;
  tokenExpText: string | null;
  refreshExpText: string | null;
  clientVersion: string | null;
  knownUids: string[];
}

export interface TraeAccountOverview {
  clientKey: string;
  loggedIn: boolean;
  running: boolean;
  live: TraeLiveAccount;
  vault: TraeVaultEntry[];
}

export type TraeSwitchOutcome = "active" | "rolled_back" | "needs_confirm";

export interface TraeSwitchResult {
  outcome: TraeSwitchOutcome;
  account_id: string;
  entries: number;
  uid: string | null;
  message: string;
  progress?: string[];
}

export interface TraeTableStat {
  name: string;
  count: number;
}

export interface TraeDecryptReport {
  out_path: string;
  pages: number;
  total_bytes?: number;
  elapsed_ms: number;
  hmac_ok: boolean;
  tables: TraeTableStat[];
}

export interface TraeScanResult {
  found: boolean;
  key: string | null;
  address: string | null;
  candidates: number;
  scanned_mb: number;
  elapsed_ms: number;
  pid: number | null;
  message: string;
}

export interface TraeDecryptedStatus {
  clientKey: string;
  exists: boolean;
  path: string;
  /** 解密快照页数（来自元信息缓存）。 */
  pages?: number;
  /** 快照是否仍等于「实时库的纯解密结果」；false 表示读取时需重新解密。 */
  current?: boolean;
  tables: TraeTableStat[];
}

/** 「确保解密快照可用」的结果：reused = 本次没有做整库解密。 */
export interface TraeEnsureDecrypted {
  reused: boolean;
  path: string;
  pages: number;
  elapsed_ms: number;
  tables: TraeTableStat[];
}

export interface TraeSessionInfo {
  id: string;
  title: string;
  created: string;
  updated: string;
  turns: number;
  /** 消息总条数（chat_message 全部未删行）。列表「正文」列用它当体量指标。 */
  messages: number;
  /** 归属账号 uid（project.user_id；无归属为空串）。 */
  owner_uid: string;
  /** 归属账号显示名（昵称或 uid 尾号）。 */
  owner_label: string;
}

/** Trae 详情里的一个「提问 + 回答」回合（形态对齐 WbSessionTurn，好让两端共用渲染）。 */
export interface TraeSessionRound {
  userText: string;
  assistantText: string;
  /** 该回合出现过的工具调用次数。 */
  toolCalls: number;
}

export interface TraeSessionDetail {
  session_id: string;
  title: string;
  source: string;
  turns: number;
  messages: number;
  created: string;
  updated: string;
  /** 归属账号（原列表列，现挪进详情）。 */
  owner_uid: string;
  owner_label: string;
  /** 逐回合正文。 */
  rounds: TraeSessionRound[];
}

export interface TraeExportMeta {
  title: string;
  turns: number;
  messages: number;
  empty_user: number;
  empty_assistant: number;
  chars: number;
}

export interface TraeExportedFile {
  session_id: string;
  filename: string;
  path: string;
  size_kb: number;
  stats: TraeExportMeta;
}

export interface TraeExportAllReport {
  path: string;
  filename: string;
  ok: number;
  failed: string[];
  total: number;
}

export interface TraeImportCandidate {
  client_key: string;
  client_label: string;
  account_id: string;
  label: string;
  display: string | null;
  uid: string;
  kind: "carrier" | "oauth" | "live" | "local";
  db_exists: boolean;
  /** 是否为该客户端当前登录账号。 */
  is_current?: boolean;
  /** 是否为本记录所属账号（同客户端同归属，导入无意义，前端禁用）。 */
  is_source?: boolean;
}

export interface TraeImportInspect {
  client_key: string;
  client_label: string;
  account_id: string;
  uid: string;
  db_exists: boolean;
  running: boolean;
  key_ready: boolean;
  key_source: "saved" | "scan_available" | "missing";
  sessions_now: number;
}

export interface TraeImportReport {
  copied_rows: number;
  sessions_requested: number;
  sessions_src: number;
  skipped: string[];
  target_client: string;
  target_label: string;
  pages: number;
  verified_sessions: number;
  /** 导入后是否已自动重启目标客户端。 */
  relaunched?: boolean;
  backup_dir: string;
  /** 源目标是否同一客户端库（同库跨账号复制）。 */
  same_db?: boolean;
  /** 本次成功登记的跨账号关联组数（仅同库复制时有值）。 */
  linked?: number;
}

// ---------------------------------------------------------------------------
// 跨账号关联（Trae）
// ---------------------------------------------------------------------------

/** 关联组里的一个副本：某账号上的某个会话。 */
export interface TraeLinkMemberInfo {
  sessionId: string;
  /** 副本是否仍然有效：会话存在、未软删、且归属账号仍是预期账号。 */
  alive: boolean;
  ownedBy?: string | null;
  title: string | null;
  /** Trae 的时间是字符串，原样透传（不做时区换算）。 */
  updated?: string | null;
  created?: string | null;
  /** 消息条数（分叉判定的主判据；不可读时为 null）。 */
  messages?: number | null;
  /**
   * 数据自检问题清单（**空 = 健康**）。判据见后端 `session_integrity`：
   * 消息缺内容行、轮次引用错位 —— 都是「客户端一定渲染不出来」的硬伤。
   * 与「分叉」正交：写坏的副本两端条数可能完全相等，光看分叉发现不了。
   */
  integrity?: string[];
}

/**
 * 两端副本的分叉状态（复制之后各自继续使用导致的差异）：
 * `none` 未分叉 · `sourceAhead` 源更靠前 · `targetAhead` 目标更靠前 ·
 * `unknown` 任一端读不到（不推断）。
 */
export type TraeLinkDivergence = "none" | "sourceAhead" | "targetAhead" | "unknown";

export interface TraeLinkGroup {
  groupId: string;
  title: unknown;
  source: TraeLinkMemberInfo;
  target: TraeLinkMemberInfo;
  verdict: "linked" | "targetMissing" | "sourceMissing" | "gone";
  /** 两端是否已分叉（见 `TraeLinkDivergence`）。 */
  divergence?: TraeLinkDivergence;
  /** 是否允许「同步差异」（两端都在且确实分叉）。 */
  canSync?: boolean;
  /** 是否允许「按对端重建」（两端都在即可，方向由用户点）。 */
  canRebuild?: boolean;
  /** 任一端自检有问题（副本数据写坏了，客户端里渲染不出来）。 */
  broken?: boolean;
  canCopy: boolean;
  defaultChecked: boolean;
  linkedAt: number;
}

export interface TraeLinksPreview {
  ok: boolean;
  storeStatus?: string;
  clientKey?: string;
  sourceUid: string;
  targetUid: string;
  count: number;
  groups: TraeLinkGroup[];
  storePath?: string;
  error?: string;
}

/** 关联组里「用哪一端覆盖哪一端」。相对**规范角色**说的，见后端 `group_members`。 */
export type TraeSyncDirection = "sourceToTarget" | "targetToSource";

/**
 * 切号后探测到的一项：**肯定已分叉**的关联组。
 *
 * 除常规组字段外，带一组「当前账号视角」的信息，前端据此写「谁比谁新」。
 * `selfRole` 决定当前账号对应 `source` 还是 `target` —— 同步方向 DO NOT guess。
 */
export interface TraeDivergedGroup extends TraeLinkGroup {
  partnerUid: string;
  partnerLabel: string;
  selfLabel: string;
  selfRole: "source" | "target";
  /**
   * 后端建议的方向（`sourceAhead` ⇒ `sourceToTarget`）；用户仍可改。
   *
   * ⚠️ **`null` = 推不出方向**（副本写坏了 ⇒ 两端条数可能相等，谁新无从判断）。
   * 此时界面不给「（推荐）」、不预置选择，用户不点就**不下发**这条同步。
   */
  suggestedDirection: TraeSyncDirection | null;
  /** 当前账号那份的自检问题（空 = 健康）。 */
  selfIssues?: string[];
  /** 对端那份的自检问题（空 = 健康）。 */
  partnerIssues?: string[];
  selfMessages: number | null;
  selfUpdated: string | null;
  partnerMessages: number | null;
  partnerUpdated: string | null;
}

/** `trae_session_links_diverged` 的返回：`count` 为 0 表示没有需要提醒的内容。 */
export interface TraeDivergedLinks {
  ok: boolean;
  storeStatus?: string;
  error?: string;
  clientKey: string;
  uid: string;
  count: number;
  groups: TraeDivergedGroup[];
}

export interface TraeDeleteFileInfo {
  path: string;
  size_mb: number;
}

export interface TraeDeleteInfo {
  ok: boolean;
  session_id: string;
  source: string;
  title: string;
  /** 归属账号 uid（无归属为空串）。 */
  owner_uid?: string;
  /** 归属账号显示名。 */
  owner_label?: string;
  /** 该归属账号是否有云端凭证（决定是否尝试同步删除任务列表）。 */
  cloud_credential?: boolean;
  tables: TraeTableStat[];
  files: TraeDeleteFileInfo[];
  live_ok: boolean;
  note: string;
}

/** 删除结果中的云端同步状态（trae_delete_session 返回的 cloud 字段）。 */
export interface TraeCloudDeleteInfo {
  attempted?: boolean;
  ok?: boolean;
  http?: unknown;
  reason?: "no_credential" | "no_owner" | string;
  error?: string;
}

/** 批量删除结果（trae_delete_sessions）。整批只做一趟解密 / 回写 / 备份。 */
export interface TraeBatchDeleteReport {
  ok: boolean;
  source: string;
  /** 实际删掉的会话数（去重后）。 */
  count: number;
  sessions: Array<{
    session_id: string;
    title: string;
    /** 各表删掉的行数（表名 → 行数）。 */
    deleted_rows: Record<string, number>;
    /** 移入回收站的文件数。 */
    moved_files: number;
  }>;
  deleted_rows_total: number;
  wal_merged: number;
  relaunched: boolean;
  moved_detail?: string[];
  backup: string[];
  trash_dirs: string[];
  hint: string;
  /** 云端任务列表的删除汇总：逐条尽力而为，失败不影响本地结果。 */
  cloud: {
    deleted: number;
    failed: Array<{ sessionId: string; uid: string; error: string }>;
    skipped: Array<{ sessionId: string; uid?: string; reason: string }>;
  };
  progress?: string[];
}

export interface TraeHandoffItem {
  sessionId?: string;
  title?: string;
  summary?: string;
  steps?: string[];
  tools?: string[];
  progress?: string;
  [key: string]: unknown;
}

export interface TraeHandoffResult {
  ok: boolean;
  clientKey: string;
  projectPath: string;
  projectName: string;
  itemCount: number;
  totalItems: number;
  markdown?: string;
  files: string[];
  skipped: string[];
  archiveDir: string;
}

// ---------------------------------------------------------------------------
// Trae 网页（OAuth）登录（对应 commands.rs 的 trae_oauth_* 命令）
// ---------------------------------------------------------------------------

export interface TraeOAuthStartResult {
  loginUrl: string;
  port: number;
  fellBack: boolean;
  deviceSource: string;
}

export interface TraeBrowserInfo {
  key: string;
  label: string;
}

export interface TraeOAuthPending {
  clientKey: string;
  name: string | null;
  loginUrl: string;
  startedAt: number;
}

/** trae_oauth_status / trae_oauth_manual 的返回（登录会话状态 + 终态结果）。 */
export interface TraeOAuthSessionStatus {
  state: string;
  message: string;
  ok?: boolean;
  account?: string | null;
  uid?: string | null;
  duplicate?: boolean;
  carrierTwin?: string | null;
  note?: string | null;
  id?: string;
  loginUrl?: string;
  port?: number;
  fellBack?: boolean;
  startedAt?: number;
  deviceSource?: string | null;
  restored?: boolean;
  pending?: TraeOAuthPending | null;
}

/** trae_import_local_login 的返回（本地登录态导入结果）。 */
export interface TraeImportResult {
  ok: boolean;
  client: string;
  id?: string;
  uid?: string | null;
  displayName?: string;
  duplicate?: boolean;
  carrierTwin?: string | null;
  source?: string;
  storageFile?: string;
  error?: string;
}

// ---------------------------------------------------------------------------
// WorkBuddy → Trae 会话移植
// ---------------------------------------------------------------------------

/** 本机 WorkBuddy（桌面版）的一个会话（元数据来自 workbuddy.db 的 sessions 表）。 */
export interface TraeWorkbuddySession {
  /** 会话 id（带连字符的 UUID，即正文 JSONL 的文件名词干）。 */
  id: string;
  title: string;
  /** 工作目录（原样）。 */
  cwd: string;
  /** 归属账号 uid。 */
  user_id: string;
  /** 创建 / 最后更新时间（毫秒）。 */
  created_at: number;
  updated_at: number;
  model: string;
  /** 是否已在 WorkBuddy 里被删除。 */
  deleted: boolean;
  /** 磁盘上是否存在正文 JSONL。 */
  has_body: boolean;
  /** 正文文件字节数。 */
  body_bytes: number;
  /** 正文行数（只在同标题重复组内计算，其余为 0）。 */
  body_lines: number;
  /** 正文内容摘要（前 16 位十六进制；无正文为空）。 */
  content_digest: string;
  /** 同标题分组键（唯一标题时为空串）。 */
  dup_group: string;
  /** 同组内是否为最新副本（无重复时恒为 true）。 */
  is_newest: boolean;
  /** 相对最新副本的差异说明（无重复时为空串）。 */
  dup_note: string;
}

/** WorkBuddy 账号的可展示信息（不含任何凭据；由本机多个来源合并而来）。 */
export interface TraeWbAccountInfo {
  uid: string;
  /** 主显示名：`昵称（uid …97eac1）`。 */
  label: string;
  /** 纯名字（昵称 → 手机号 → 邮箱）。 */
  name: string;
  /** 次要信息：手机号 · 账号类型 · 版本。 */
  meta: string;
  /** `personal` / `enterprise`。 */
  kind: string;
  /** `free` / `pro`。 */
  edition: string;
  /** 是否本机当前登录账号。 */
  is_primary: boolean;
  /** 名字来源（中文，便于判断可信度）。 */
  name_source: string;
}

/** WorkBuddy 来源列表。 */
export interface TraeWorkbuddyList {
  /** 本机是否具备 WorkBuddy 数据（projects/ + workbuddy.db）。 */
  available: boolean;
  data_root: string;
  /** 本机已知账号的显示名（按 user_id 查表）。 */
  accounts: TraeWbAccountInfo[];
  sessions: TraeWorkbuddySession[];
}

/** 单会话的转换预览（只读，不写库）。 */
export interface TraeWorkbuddyPreviewItem {
  session_id: string;
  ai_title: string | null;
  turns: number;
  tool_steps: number;
}

/** WorkBuddy → Trae 导入结果。 */
export interface TraeWorkbuddyReport {
  written_sessions: number;
  turns: number;
  tool_steps: number;
  pages: number;
  /** 真正被重新加密的页数（其余页沿用原密文）。 */
  changed_pages?: number;
  /** 因库增长而新增的页数。 */
  appended_pages?: number;
  /** 重写字节数（MB）。 */
  changed_mb?: number;
  /** 增量回写耗时（ms）。 */
  write_ms?: number;
  verified_sessions: number;
  target_client: string;
  target_label: string;
  target_uid: string;
  /** 导入后是否已自动重启目标客户端。 */
  relaunched?: boolean;
  backup_dir: string;
}

// ---------------------------------------------------------------------------
// Trae → WorkBuddy 会话导出（反向）
// ---------------------------------------------------------------------------

/** WorkBuddy 侧的一个可选目标账号。 */
export interface TraeWbAccount {
  uid: string;
  /** 显示名：`昵称（uid …97eac1）`；名字解析不出来时退化成 `uid …97eac1`。 */
  label: string;
  /** 纯名字（昵称 → 手机号 → 邮箱），用于列表标题。 */
  name: string;
  /** 次要信息：手机号 · 账号类型 · 版本。 */
  meta: string;
  kind: string;
  edition: string;
  /** 名字来源（中文）。 */
  name_source: string;
  /** 是否本机最近活动的账号（默认选中）。 */
  is_current: boolean;
  /** 该账号现有会话数。 */
  sessions: number;
  last_active_ms: number;
}

/** WorkBuddy 侧现状（数据根 / 客户端状态 / 可选账号）。 */
export interface TraeWbTarget {
  /** 本机是否具备 WorkBuddy 数据（projects/ + workbuddy.db）。 */
  available: boolean;
  data_root: string;
  db_path: string;
  projects_dir: string;
  /** WorkBuddy 桌面版当前是否在运行（导出前会自动退出，写完再拉起）。 */
  running: boolean;
  /** 探测到的主程序路径（探测不到为 null，此时需手动启动）。 */
  exe: string | null;
  accounts: TraeWbAccount[];
  default_uid: string;
}

/** 某个 Trae 客户端里可导出的会话。 */
export interface TraeWbSourceSession {
  id: string;
  title: string;
  /** Trae 会话的工作目录（会原样写进 WorkBuddy 的 sessions.cwd）。 */
  cwd: string;
  /** 正文将落到 `projects/{workspace_key}/`。 */
  workspace_key: string;
  created_ms: number;
  updated_ms: number;
  /** 用户提问数（= 回合数）。 */
  turns: number;
  owner_uid: string;
  owner_label: string;
}

/** Trae 侧可导出会话列表。 */
export interface TraeWbSourceList {
  client_key: string;
  client_label: string;
  data_root: string;
  count: number;
  sessions: TraeWbSourceSession[];
}

/** 单会话的导出预览（只读，不写任何文件）。 */
export interface TraeWbPreviewItem {
  trae_id: string;
  workbuddy_id: string;
  title: string;
  cwd: string;
  workspace_key: string;
  turns: number;
  /** 源会话里真正有回答的回合数（其余回合在 Trae 里本就没有回答）。 */
  answered: number;
  tool_steps: number;
  events: number;
  bytes: number;
}

export interface TraeWbPreview {
  preview: TraeWbPreviewItem[];
  skipped: { session_id: string; reason: string }[];
}

/** 单个会话的导出结果。 */
export interface TraeWbExportedSession {
  trae_id: string;
  workbuddy_id: string;
  title: string;
  cwd: string;
  workspace_key: string;
  turns: number;
  tool_steps: number;
}

/** Trae → WorkBuddy 导出结果。 */
export interface TraeWbExportReport {
  written_sessions: number;
  turns: number;
  tool_steps: number;
  target_uid: string;
  target_label: string;
  /** 本次落到的 WorkBuddy 工作区目录名。 */
  workspaces: string[];
  sessions: TraeWbExportedSession[];
  /** 写入后 WorkBuddy 库里的会话总数。 */
  verified_sessions: number;
  /** 导出前 WorkBuddy 是否在运行。 */
  was_running: boolean;
  /** 是否已自动重新拉起 WorkBuddy。 */
  relaunched: boolean;
  backup_dir: string;
  pruned_backups: number;
}

// ---------------------------------------------------------------------------
// WorkBuddy 本机垃圾清理（扫描 + 受控清除）
// ---------------------------------------------------------------------------

/** 一条可清理项。 */
export interface TraeWbCleanupItem {
  id: string;
  /** `session` 会删库行；`file` / `dir` 只搬文件。 */
  kind: "session" | "file" | "dir";
  category: string;
  title: string;
  uid: string;
  /** 归属账号显示名（会话类才有）。 */
  owner: string;
  updated_at: number;
  bytes: number;
  detail: string;
  /** 是否建议清理（前端默认勾选）。 */
  recommended: boolean;
  /** 将被搬走 / 删除的路径（确认弹窗逐条列出）。 */
  paths: string[];
}

/** 一类可清理项。 */
export interface TraeWbCleanupCategory {
  key: string;
  title: string;
  desc: string;
  count: number;
  bytes: number;
  items: TraeWbCleanupItem[];
}

/** `~/.workbuddy` 里的占用大头（只读说明，解释「为什么这么占地方」）。 */
export interface TraeWbCleanupHolding {
  name: string;
  path: string;
  bytes: number;
  /** 本工具是否提供了清理入口。 */
  cleanable: boolean;
  note: string;
}

/** 清理扫描结果（只读）。 */
export interface TraeWbCleanupScan {
  available: boolean;
  data_root: string;
  db_path: string;
  running: boolean;
  exe: string | null;
  generated_at: number;
  log_keep_days: number;
  categories: TraeWbCleanupCategory[];
  totals: {
    count: number;
    bytes: number;
    recommended_count: number;
    recommended_bytes: number;
    sessions: number;
  };
  large_holdings: TraeWbCleanupHolding[];
  trash: { dir: string; count: number; bytes: number };
  /** 本次结果是否直接来自磁盘缓存（有缓存就用缓存，不再按时间过期）。 */
  cached?: boolean;
  /** 这份数据的生成时间（毫秒）；实时扫描时等于 `generated_at`。 */
  scannedAt?: number;
  /** 距离生成过了多久（毫秒）；实时扫描为 0。 */
  ageMs?: number;
  /** 缓存已超过 10 分钟阈值 —— 界面提示「建议重新扫描」，但**不会**自动重扫。 */
  stale?: boolean;
}

/** 清理执行结果。 */
export interface TraeWbCleanupReport {
  requested: number;
  purged: number;
  sessions: number;
  rows_deleted: number;
  files_removed: number;
  planned_bytes: number;
  hard: boolean;
  /** 彻底删除时才真正释放的字节数。 */
  reclaimed_bytes: number;
  trash_dir: string;
  backup_dir: string;
  verified_sessions: number;
  was_running: boolean;
  relaunched: boolean;
  failed: string[];
  skipped: { id: string; reason: string }[];
}

// ---------------------------------------------------------------------------
// Trae 本机清理（会话 / 工具残留 / 客户端残留 / 回收站）
// ---------------------------------------------------------------------------

/** 一条可清理项。 */
export interface TraeCleanupItem {
  id: string;
  /** `session` 会删库行；`batch` 是一组同批备份文件；`file` / `dir` 只搬文件。 */
  kind: "session" | "batch" | "file" | "dir";
  category: string;
  title: string;
  detail: string;
  bytes: number;
  bytes_mb: number;
  /** 是否建议清理（前端默认勾选）。 */
  recommended: boolean;
  /** 该项需要先关闭 Trae 客户端才能清理（缓存被进程占用）。 */
  needs_client_stop: boolean;
  client_key: string;
  client_label: string;
  /** 会话类才有。 */
  session_id: string;
  turns: number;
  owner?: string;
  owner_uid?: string;
  updated: string;
  created?: string;
  /** 将被删除 / 搬走的路径（确认弹窗逐条列出）。 */
  paths: string[];
}

/** 一类可清理项。 */
export interface TraeCleanupCategory {
  id: string;
  title: string;
  desc: string;
  count: number;
  bytes: number;
  bytes_mb: number;
  recommended_count: number;
  items: TraeCleanupItem[];
}

/** 清理扫描结果（只读）。 */
export interface TraeCleanupScan {
  store_root: string;
  trash_root: string;
  total_bytes: number;
  total_mb: number;
  categories: TraeCleanupCategory[];
  /** 扫描时发现的注意事项（例如客户端正在运行）。 */
  notes: string[];
  /** 本次结果是否直接来自磁盘缓存（有缓存就用缓存，不再按时间过期）。 */
  cached?: boolean;
  /** 这份数据的生成时间（毫秒）。 */
  scannedAt?: number;
  /** 距离生成过了多久（毫秒）；实时扫描为 0。 */
  ageMs?: number;
  /** 缓存已超过 10 分钟阈值 —— 界面提示「建议重新扫描」，但**不会**自动重扫。 */
  stale?: boolean;
}

/** 清理执行结果。 */
export interface TraeCleanupReport {
  ok: boolean;
  /** 是否直接彻底删除（false = 移入回收站）。 */
  hard: boolean;
  removed: number;
  freed_bytes: number;
  freed_mb: number;
  session_count: number;
  details: Array<{
    kind: string;
    category?: string;
    path?: string;
    bytes?: number;
    client?: string;
    count?: number;
  }>;
  errors: string[];
  unknown_ids: string[];
  trash_root: string;
  /** 被删除会话的云端归属（本地删完后据此同步云端记录）。 */
  session_targets?: Array<{ client_key: string; session_id: string; owner_uid: string }>;
  /** 云端同步删除的逐条结果（缺失表示本次没有会话被删）。 */
  cloud?: Array<{
    session_id: string;
    attempted: boolean;
    ok?: boolean;
    reason?: string;
    error?: string;
  }>;
}

// ---------------------------------------------------------------------------
// WorkBuddy 账号管理（国内版）
// ---------------------------------------------------------------------------

export interface WorkbuddyAccount {
  id: string;
  uid: string;
  name: string;
  nickname: string | null;
  email: string | null;
  hasToken: boolean;
  expiresAt: number | null;
  lastUsedAt: number | null;
  isCurrent: boolean;
}

export interface WorkbuddyAccountList {
  accounts: WorkbuddyAccount[];
  currentUid: string | null;
  loggedIn: boolean;
  authFilePath: string;
}

export interface WorkbuddySwitchPrecheck {
  running: boolean;
  exe: string | null;
  loggedIn: boolean;
  currentUid: string | null;
  hasToken: boolean;
  alreadyCurrent: boolean;
  authFilePath: string;
}

export interface WorkbuddySwitchResult {
  ok: boolean;
  accountId: string;
  uid: string;
  backup: string | null;
  killed: string[];
  relaunched: boolean;
}

export interface WorkbuddyOAuthStart {
  loginId: string;
  verificationUri: string;
  expiresIn: number;
}

export interface WorkbuddyOAuthPoll {
  done: boolean;
  result?: { id: string | null; uid: string | null; name: string };
  error?: string | null;
}

export interface WorkbuddyImportPreview {
  items: Array<{
    name: string;
    uid: string | null;
    action: "create" | "update";
    hasToken: boolean;
  }>;
  count: number;
}

// ---------------------------------------------------------------------------
// WorkBuddy 会话记录 / 复制 / 关联（国内版）
// ---------------------------------------------------------------------------

export interface WbSessionItem {
  id: string;
  title: string;
  cwd: string;
  model: string;
  createdAt: number;
  updatedAt: number;
  hasBody: boolean;
  bodyBytes: number;
  dupGroup: string;
  isNewest: boolean;
  dupNote: string;
}

export interface WbSessionList {
  ok: boolean;
  uid: string;
  label?: string;
  count: number;
  sessions: WbSessionItem[];
  error?: string;
}

export interface WbAccountSessions {
  uid: string;
  /** 账号显示名（昵称 / 手机号 + uid 尾号）——界面一律用它，不要用 uid 前缀。 */
  label?: string;
  count: number;
  sessions: WbSessionItem[];
}

export interface WbSessionListByAccount {
  ok: boolean;
  accounts: WbAccountSessions[];
  error?: string;
}

export interface WbSessionTurn {
  userText: string;
  assistantText: string;
  createdAt: number;
  updatedAt: number;
  events: number;
  toolCalls: number;
}

export interface WbSessionDetail {
  ok: boolean;
  session?: WbSessionItem;
  bodyPath?: string | null;
  aiTitle?: string | null;
  turnCount?: number;
  turns?: WbSessionTurn[];
  bodyError?: string;
  error?: string;
}

export interface WbSessionCopyPreviewItem {
  id: string;
  title?: string;
  available: boolean;
  reason?: string;
  hasBody?: boolean;
  bodyBytes?: number;
  alreadyLinked?: boolean;
  targetSessionId?: string | null;
}

export interface WbSessionCopyPreview {
  ok: boolean;
  sourceUid: string;
  targetUid: string;
  appRunning: boolean;
  targetIsCurrent: boolean;
  blocked: boolean;
  reason?: string | null;
  items: WbSessionCopyPreviewItem[];
  error?: string;
}

export interface WbSessionCopyReport {
  ok: boolean;
  sourceUid: string;
  targetUid: string;
  copied: Array<{
    sourceId: string;
    targetId: string;
    title: string;
    bodyPath: string;
    groupId: string;
  }>;
  skipped: Array<{ id: string; title: string; reason: string; targetSessionId: string }>;
  count: number;
  backup: string;
  edgeSyncSkipped: Array<{ name: string; path: string; size: number }>;
  note: string;
}

export interface WbLinkMemberInfo {
  sessionId: string;
  alive: boolean;
  ownedBy?: string;
  title: string | null;
  updatedAt: number;
  hasBody: boolean;
}

export interface WbLinkGroup {
  groupId: string;
  title: unknown;
  source: WbLinkMemberInfo;
  target: WbLinkMemberInfo;
  verdict: "linked" | "targetMissing" | "sourceMissing" | "gone";
  canCopy: boolean;
  defaultChecked: boolean;
  linkedAt: number;
}

export interface WbLinksPreview {
  ok: boolean;
  storeStatus?: string;
  sourceUid: string;
  targetUid: string;
  count: number;
  groups: WbLinkGroup[];
  storePath?: string;
  error?: string;
}

export interface WbSessionDeleteReport {
  ok: boolean;
  uid: string;
  deleted: Array<{ id: string; title: string; bodyMoved: boolean }>;
  count: number;
  deletedAt: number;
  backup: string;
  killed: string[];
  relaunched: boolean;
  trashDir: string;
  moveErrors: string[];
}

// ---------------------------------------------------------------------------
// WorkBuddy 积分 / 积分包（国内版）
// ---------------------------------------------------------------------------

/** 一个积分包（归一化后的统一形态）。 */
export interface WbCreditResource {
  packageCode: string | null;
  packageName: string | null;
  total: number;
  remaining: number;
  used: number;
  status: number | null;
  /** 到期时间（毫秒）；null = 长期有效。 */
  expireAt: number | null;
  expired: boolean;
  /** 7 天内到期。 */
  expiringSoon: boolean;
}

/** 可查询积分的账号（不含任何凭据字段）。 */
export interface WbCreditAccountInfo {
  id: string;
  uid: string;
  name: string;
  /** own = 本工具账号库（可写）；ref = 参考工具账号库（只读借用）。 */
  origin: "own" | "ref";
  /** plain = 明文可查；envelope = 加密信封查不了；missing = 无凭据。 */
  tokenState: "plain" | "envelope" | "missing";
  queryable: boolean;
  blockedReason: string | null;
}

/** 单账号积分查询结果。 */
export interface WbCreditItem {
  ok: boolean;
  account: WbCreditAccountInfo;
  totalCapacity?: number;
  totalRemaining?: number;
  expiringSoonRemaining?: number;
  expiredRemaining?: number;
  soonestExpireAt?: number | null;
  expiringSoon?: WbCreditResource[];
  expired?: boolean;
  packageCount?: number;
  /** 还有剩余、按到期时间升序的积分包（卡片上的「N 个积分包」用它统计）。 */
  activePackageCount?: number;
  resources?: WbCreditResource[];
  activeResources?: WbCreditResource[];
  updatedAt?: number;
  /** summary = 三路新接口；legacy = 旧单接口回退。 */
  source?: "summary" | "legacy";
  refreshed?: boolean;
  error?: string;
}

export interface WbCreditsResult {
  ok: boolean;
  accounts: WbCreditItem[];
  errors: Array<{ id: string; uid: string; name: string; error: string }>;
  updatedAt: number | null;
  cached: boolean;
  stale?: boolean;
  empty?: boolean;
  summary: {
    queried: number;
    succeeded: number;
    failed: number;
    totalCapacity?: number;
    totalRemaining?: number;
  };
}

export interface WbCreditsAccounts {
  ok: boolean;
  count: number;
  queryable: number;
  accounts: WbCreditAccountInfo[];
  /** 只读借用的参考工具账号库路径。 */
  referenceStore: string;
}

// ---------------------------------------------------------------------------
// 首页总览
// ---------------------------------------------------------------------------

export interface OverviewTraeDecrypted {
  exists: boolean;
  current: boolean;
  path: string;
  pages: number;
  tableCount: number;
  createdMs: number;
  sessionCount: number | null;
  dbBytes: number;
}

export interface OverviewTraeCredits {
  /** 该客户端账号库里的账号数。 */
  accountCount: number;
  /** 其中有多少个能联网查积分（有网页凭证）。 */
  queryable: number;
  /** 其中有多少个已经拉到过积分数据。 */
  withData: number;
  /** 合计剩余积分（只算拉取成功的账号）。 */
  totalRemaining: number;
  /** 最近一次成功拉取的时间（毫秒）；从没拉过是 null。 */
  updatedAt: number | null;
}

export interface OverviewTraeClient {
  key: string;
  label: string;
  installed: boolean;
  exe: string | null;
  userDataDir: string;
  hasLogin: boolean;
  /** 当前登录账号显示名（真实昵称 + uid 尾号）；未登录或读不出是 null。 */
  loginLabel: string | null;
  running: boolean;
  processCount: number;
  accounts: number;
  /** 该客户端自己的积分摘要（离线读 profile.json，不联网）。 */
  credits: OverviewTraeCredits;
  decrypted: OverviewTraeDecrypted;
}

export interface OverviewWorkbuddyAccount {
  id: string;
  uid: string;
  name: string;
  tokenState: "plain" | "envelope" | "missing";
  queryable: boolean;
  isCurrent: boolean;
  lastUsedAt: number | null;
}

export interface Overview {
  ok: boolean;
  generatedAt: number;
  trae: {
    clients: OverviewTraeClient[];
    installedClients: number;
    runningClients: number;
    accountTotal: number;
    sessionTotal: number;
    anyDecrypted: boolean;
    /** 排在最前且确有使用历史的客户端 key（全 0 分时是 null）——用来打「常用」徽标。 */
    topPick: string | null;
  };
  workbuddy: {
    running: boolean;
    processCount: number;
    processes: Array<{ name: string; pid: number }>;
    exe: string | null;
    loggedIn: boolean;
    currentUid: string;
    currentLabel: string | null;
    authFilePath: string;
    authFileExists: boolean;
    authFileBytes: number;
    accounts: OverviewWorkbuddyAccount[];
    accountCount: number;
    queryableCount: number;
    sessionCount: number | null;
    deletedSessionCount: number | null;
    bodyFileCount: number;
    dbPath: string;
    dbBytes: number;
    dbExists: boolean;
    lastSwitch: Record<string, unknown> | null;
  };
  credits: WbCreditsResult;
  notes: string[];
}

/** 落盘的「可回收空间」结果（前端扫完后写回缓存，下次首屏直接可用）。 */
export interface OverviewReclaimCache {
  traeBytes: number;
  wbBytes: number;
  totalBytes: number;
  /** 扫描完成时间（ms）。 */
  at: number;
}

/**
 * 只读总览缓存（`app_overview_cached`）。
 *
 * 启动时先拿它渲染界面（毫秒级），再后台调 `app_overview_snapshot` 刷新。
 * `empty = true` 表示本机还没写过缓存（首次运行 / 缓存被删），此时只能走实时计算。
 */
export interface OverviewCache {
  ok: boolean;
  empty: boolean;
  /** 这份缓存是什么时候算出来的（ms）；`empty` 时为 null。 */
  generatedAt: number | null;
  /** 距今多久（ms）；`empty` 时为 null。 */
  ageMs: number | null;
  snapshot: Overview | null;
  reclaim: OverviewReclaimCache | null;
}

