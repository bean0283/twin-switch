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
  /** 已并入哪个客户端（key）；null = 仍是独立客户端。 */
  merged_into?: string | null;
  /** 本机能不能真把它跑起来（数据目录在 **且** 找得到主程序）。 */
  runnable?: boolean;
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
  /**
   * 这个客户端能不能当导入**目标**（本机有它自己的主程序）。
   *
   * ⚠️ 与 `db_exists` 无关：2026-10 官方把 TRAE SOLO 并进 Trae CN 之后，SOLO 的库与
   * 登录态都还在（`db_exists` / `is_current` 都成立），但它已经没有自己的程序了 ——
   * 写进去**不报错**，只是客户端永远不读，用户看到的是「导进去但找不到」。
   */
  as_target?: boolean;
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

/**
 * `trae_vault_adopt_merged` 里一条「已搬入」的账号。
 *
 * 背景：2026-10 官方把 TRAE SOLO 并进 Trae CN 时只搬了程序与会话，
 * **本工具自己的账号库**（`<工具目录>/trae/vault/`）官方不会碰 ⇒ SOLO 下备份过的账号
 * 在 Trae CN 下看不见、切不过去。这条记录描述搬进来的是哪一个。
 */
export interface TraeAdoptMergedItem {
  /** 源客户端 key（`solo-cn`）。 */
  from: string;
  /** 源客户端显示名。 */
  from_label: string;
  /** 源账号库里的账号 id。 */
  source_id: string;
  /** 落进目标账号库后的账号 id（预演时等于 `source_id`）。 */
  id: string;
  /** 归属 uid。 */
  uid: string | null;
  /** 实际写入的文件数（预演时无此字段）。 */
  files?: number;
}

/** 一条「没搬」的记录及原因（不要静默丢弃，界面要如实说明）。 */
export interface TraeAdoptMergedSkip {
  from: string;
  from_label: string;
  source_id: string;
  uid: string | null;
  reason: string;
}

export interface TraeAdoptMergedResult {
  target: string;
  target_label: string;
  /** true = 只预演，一个字节都没写。 */
  dry_run: boolean;
  adopted: TraeAdoptMergedItem[];
  skipped: TraeAdoptMergedSkip[];
  /**
   * 顺带**补全**的残档：老版本「一并带入」只搬了载体文件，`oauth.json` / `profile.json`
   * 没跟过来 ⇒ 账号显示成 `oauth_xxxxxx` / `0 B` / 查不了积分。这里列出补回了哪些文件。
   */
  repaired: TraeAdoptMergedRepair[];
}

/** 一条「补全残档」记录（只补目标缺的文件，绝不覆盖）。 */
export interface TraeAdoptMergedRepair {
  from: string;
  from_label: string;
  source_id: string;
  /** 目标库里那份残档的账号 id。 */
  id: string;
  uid: string | null;
  /** 补回来的文件名（`oauth.json` / `profile.json`）。 */
  files: string[];
}

// ---------------------------------------------------------------------------
// 账号 token（头像弹窗：看 / 复制 / 续期）
// ---------------------------------------------------------------------------

/** `trae_account_token`：账号的 jwt 本体与有效期。**只读、不联网。** */
export interface TraeAccountToken {
  ok: boolean;
  clientKey: string;
  id: string;
  uid: string | null;
  /** JWT 本体（`xxx.yyy.zzz`）。 */
  token: string;
  /** 到期时间（毫秒）；null = 解不出 exp。 */
  tokenExp: number | null;
  /** 这份 token 从哪儿读出来的：网页凭证 / 载体登录态。 */
  source: "oauth" | "carrier";
  /** 续期三件套（refreshToken + 设备私钥 + deviceId）齐了才为 true。 */
  canRefresh: boolean;
  /** 续期门槛（天）：剩余**大于**它就默认不刷新。 */
  thresholdDays: number;
  /** 剩余天数（负数 = 已过期）。 */
  remainingDays: number;
}

/** `trae_account_token_refresh` 的结果。 */
export interface TraeAccountTokenRefresh {
  ok: boolean;
  /** **false = 本次没刷**（token 还新鲜），不是失败 —— 界面据此弹「强制刷新」确认窗。 */
  refreshed: boolean;
  reason?: "fresh";
  force?: boolean;
  source?: "oauth" | "carrier";
  token: string;
  tokenExp: number | null;
  tokenExpBefore?: number | null;
  remainingDays: number;
  refreshExpiredAt?: number | null;
  /** 实际写回的文件（`oauth.json` / `User/globalStorage/storage.json`）。 */
  updatedFiles?: string[];
  thresholdDays?: number;
  message: string;
}

/**
 * `workbuddy_account_token` 的结果。
 *
 * 与 Trae 侧同一个形状，只有 `source` 的含义不同：WorkBuddy 的凭据就在本工具账号库里，
 * 所以这里是**凭据形态**（明文 / 加密信封 / 缺失），不是「从哪个文件读出来的」。
 */
export interface WbAccountToken {
  ok: boolean;
  id: string;
  uid: string;
  name: string;
  /** JWT 本体（`xxx.yyy.zzz`）。 */
  token: string;
  /** 到期时间（毫秒）；null = 解不出 exp。 */
  tokenExp: number | null;
  /** 刷新额度到期（毫秒）；null = 没有这个字段。 */
  refreshExpiredAt: number | null;
  /** `plain` = 明文可用；`envelope` = 加密信封解不开；`missing` = 没采到凭据。 */
  source: "plain" | "envelope" | "missing";
  /** 不能用的可读原因；null = 正常。 */
  blockedReason: string | null;
  canRefresh: boolean;
  /** 续期门槛（天）：剩余**大于**它就默认不刷新。 */
  thresholdDays: number;
  /** 剩余天数（负数 = 已过期）。 */
  remainingDays: number;
}

/** `workbuddy_account_token_refresh` 的结果（与 Trae 侧同语义）。 */
export interface WbAccountTokenRefresh {
  ok: boolean;
  refreshed: boolean;
  reason?: "fresh";
  force?: boolean;
  token: string;
  tokenExp: number | null;
  tokenExpBefore?: number | null;
  remainingDays: number;
  refreshExpiredAt?: number | null;
  updatedFiles?: string[];
  thresholdDays?: number;
  message: string;
}

/**
 * token 弹窗内部用的**归一化**形状：两个产品的差异（来源字段怎么讲、能不能刷）在
 * 各自产品页里烘成一句话，弹窗只认这一份 —— 避免弹窗里出现 `if (provider === ...)`
 * 散落在每个展示点上。
 */
export interface AccountTokenInfo {
  token: string;
  tokenExp: number | null;
  refreshExpiredAt: number | null;
  /** 来源 / 凭据形态的一句话说明。 */
  sourceLabel: string;
  /** 不可用 / 不可刷新的可读原因；null = 正常。 */
  blockedReason: string | null;
  canRefresh: boolean;
  thresholdDays: number;
  remainingDays: number;
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
  /**
   * 备份批次专用：这一批**是否超出了设置里的保留策略**。
   * ⚠️ 由后端 `settings::excess_with_reasons` 统一判定 ⇒ 与真正删文件的判据同源。
   */
  over_policy?: boolean;
  /** 备份批次专用：超出策略的原因（可能是多条，用「；」连接），例如「超过 30 天；超过保留份数（每客户端 3 份）」。 */
  policy_reason?: string;
  /**
   * 备份批次专用：是不是该客户端**最旧**的一批（最新那批不算）。
   * 清理顺序是「先从最老的清」，所以界面上给它一个「最旧」标记，让规则看得见。
   */
  oldest?: boolean;
  /**
   * 「已并入、本机已没有它自己的程序」的客户端留下的那**一条**汇总项（T61）。
   *
   * ⚠️ 为什么要有这个字段：合并后那两个 SOLO 只有历史数据，但它们的缓存本来是**逐目录**
   * 列的，真机会刷出 17 行 `TRAE SOLO CN · Cache / Code Cache / …` —— 看着像这个客户端还装着。
   * 后端把它们折成一条并打上这个标记，界面加「已并入」徽章，说明里写清「只剩历史数据、
   * 删除不会结束任何进程」。**这也是自查探针断言「影子没被拆回十几行」的钩子。**
   */
  merged_shadow?: boolean;
  /** 折成一条时吃掉的路径条数（仅 `merged_shadow` 项有）。 */
  merged_paths?: number;
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
  /**
   * 「工具残留」这一类才有：当前生效的整库备份保留策略。
   * 界面直接显示它的 `summary`（后端原文），**不要自己拼** —— 否则就成了同一屏两个说法。
   */
  backup_policy?: CleanupBackupPolicy;
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
// T46 · S4 WorkBuddy 会话编辑器
//
// 数据模型与 Trae **完全不同**：元数据是 `workbuddy.db` 的 `sessions` 一张表，
// 正文是 `projects/<工作区>/<cid>.jsonl`（**一行一个事件**）。
// 所以读分两步（回合索引 / 按行区间取完整行），写走「操作数组」（只回传改动过的行）。
// ---------------------------------------------------------------------------

/** `sessions` 表的一列（`PRAGMA table_info`）。 */
export interface WbEditColumn {
  name: string;
  type: string;
  notNull: boolean;
  pk: boolean;
  dflt: string | null;
  /** 库里是 TEXT、存的是 JSON。sessions 表目前恒为 false。 */
  json: boolean;
  /** 主键（`id`）：不可改，也不参与写回。 */
  readOnly: boolean;
}

/**
 * 正文一行派生出的种类。**由后端判定**（`workbuddy_session_edit::classify_line`），
 * 前端只按它决定画成什么卡片 —— 分类规则只有一处实现。
 */
export type WbLineKind =
  | "user" // 真实用户提问（正文包在 <user_query> 里）
  | "auto" // 客户端自动续写 / 后台任务通知（role=user 但不是用户敲的）
  | "summary" // 纯内部上下文摘要（界面上看不见）
  | "assistant" // 助手回答
  | "reasoning" // 思考
  | "tool_call" // 工具调用
  | "tool_result" // 工具结果
  | "meta" // ai-title / session-meta / file-history-snapshot
  | "message" // 其它 role 的 message
  | "unknown"
  | "bad";

/** 轻量行描述（`edit_doc` 的回合索引里用；`text` 可能被截断）。 */
export interface WbLineSummary {
  index: number;
  type: string;
  kind: WbLineKind;
  label: string;
  name: string;
  status: string;
  timestamp: number;
  bytes: number;
  textBytes: number;
  text: string;
  textTruncated: boolean;
  /** `text` 在该行 JSON 里的路径；null = 不能就地改（只能改原始 JSON）。 */
  textPath: (string | number)[] | null;
  /** 用户提问是否包在 `<user_query>…</user_query>` 里。 */
  wrapped: boolean;
  parseError: string | null;
}

/** 完整行（`edit_lines` 返回）：带解析后的 `json` 与原始文本 `raw`。 */
export interface WbEditLine extends WbLineSummary {
  raw: string;
  json: Record<string, unknown> | null;
}

/** 一个回合：从一条真实用户提问到下一条之前。 */
export interface WbEditTurn {
  index: number;
  start: number;
  /** 结束行号（不含）。 */
  end: number;
  lines: number;
  userText: string;
  replyText: string;
  tools: string[];
  time: number;
  status: string;
}

export interface WbEditBody {
  path: string;
  /** 相对 `projects/` 的工作区目录名。 */
  workspace: string;
  exists: boolean;
  bytes: number;
  lineCount: number;
}

/** 会话编辑器读模型。 */
export interface WbEditDoc {
  clientKey: string;
  sessionId: string;
  ownerUid: string;
  ownerLabel: string;
  title: string;
  /** `sessions` 行全字段（BLOB 用 `{"$blob": …}` 标记，可逆往返）。 */
  row: Record<string, unknown>;
  columns: WbEditColumn[];
  body: WbEditBody;
  turns: WbEditTurn[];
  /** 第一条用户提问之前的行号（`session-meta` / `ai-title` 常落在这里）。 */
  head: number[];
  /** 种类 → 行数（键：user / auto / summary / assistant / reasoning / toolCall / toolResult / meta / bad / message / unknown）。 */
  counts: Record<string, number>;
  /**
   * **行号 → 种类**（长度 = `body.lineCount`）。
   *
   * 前端把它回传给校验命令，后端才能在不读整份正文的前提下算出
   * 「删掉的行里有几条用户提问」（空会话硬拦）—— 判据只有后端一份。
   */
  lineKinds: WbLineKind[];
  warnings: string[];
}

/** 一次行取回的返回。 */
export interface WbEditLinesResult {
  ok: boolean;
  sessionId: string;
  from: number;
  to: number;
  lines: WbEditLine[];
}

/** 分级校验结果（后端 `validate_submit`）。 */
export interface WbEditReport {
  errors: string[];
  warnings: string[];
  errorCount: number;
  warnCount: number;
  stats: Record<string, unknown>;
}

/** 写入目标账号。 */
export interface WbEditTargetAccount {
  uid: string;
  label: string;
  sessions: number;
}

export interface WbEditTargets {
  ok: boolean;
  error?: string;
  accounts: WbEditTargetAccount[];
}

/** 写入结果。校验没过时 `ok=false` + `errors`，**不是 Err**。 */
export interface WbEditSaveResult {
  ok: boolean;
  mode?: "overwrite" | "insert";
  /** 源会话 id。 */
  sourceId?: string;
  /** 最终写入的会话 id（`insert` 模式下是新 UUID）。 */
  sessionId?: string;
  ownerUid?: string;
  ownerLabel?: string;
  lines?: number;
  bodyPath?: string;
  bodyBackup?: string;
  dbBackup?: string;
  warnings?: string[];
  /** `ok=false` 时：校验阶段说明。 */
  stage?: string;
  errors?: string[];
}

/** 正文改动操作（只回传**改动过**的那几行）。 */
export type WbEditOp =
  | { op: "replace"; index: number; json: unknown }
  | { op: "delete"; indexes: number[] }
  | { op: "insert"; at: number; json: unknown }
  | { op: "append"; json: unknown };

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
  /** 已并入哪个客户端（key）；null = 仍是独立客户端。 */
  mergedInto?: string | null;
  /** 本机能不能真把它跑起来（数据目录在 **且** 找得到主程序）。 */
  runnable?: boolean;
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


// ---------------------------------------------------------------------------
// T46 · 会话编辑器
// ---------------------------------------------------------------------------

/** 会话编辑器：一列的元数据（来自 `PRAGMA table_info` + JSON 文本列判定）。 */
export interface TraeEditColumn {
  name: string;
  /** 声明类型（TEXT / INTEGER / …；可能为空）。 */
  type: string;
  notNull: boolean;
  pk: boolean;
  /**
   * 该列带 UNIQUE 索引 —— 也就是这张表的**身份列**。
   *
   * 光看 `pk` 不够：真机上 `chat_message` 的主键是自增 `id`，真正唯一的是
   * `message_id`（TEXT + UNIQUE），漏掉它就撞 `UNIQUE constraint failed`。
   * 「新增 / 复制一行」时这些列必须换成新值，映射漏一列就会被后端校验当场拦下。
   */
  unique: boolean;
  dflt: string | null;
  /**
   * 该列在库里是 TEXT，存的却是一段 JSON —— 读取时已解成真正的树，
   * 写入时再序列化回文本。前端因此可以按结构化表单渲染 `content` / `context`。
   */
  json: boolean;
  /** 自增主键（`id`）：不显示为可改，也不参与写回（交给 SQLite 分配）。 */
  readOnly: boolean;
}

/** 会话编辑器：一张表在该会话里的全部行。 */
export interface TraeEditTable {
  name: string;
  /** `session` = 用 session_id 取行；`message` = 用 message_id 间接取行。 */
  scope: "session" | "message";
  editable: boolean;
  /** `fts_*` 检索镜像：改动只影响搜索命中，不影响会话内容。 */
  derived: boolean;
  /**
   * 本表有多少个单元格被折叠成了「大字段引用」（`$jsonRef`）。
   * 真机实测：`chat_message_task.content` 单行可达 9.8 MB，一条会话合计 74 MB，
   * 全量塞给前端会把渲染冻死 —— 超过 32 KB 的单元格只回引用，点「展开」才拉内容。
   */
  refCount: number;
  columns: TraeEditColumn[];
  rows: Record<string, unknown>[];
}

/**
 * 大字段引用：超过阈值的 JSON 单元格不回内容，只回这个对象。
 *
 * ⚠️ 不展开就**原样保留**（写回时后端按 `pk` 回查原文），展开后才能改。
 * 所以前端可以放心地把它当「值」原样传来传去。
 */
export interface TraeJsonRef {
  $jsonRef: {
    /** 原行主键（SQLite INTEGER 主键，回查用）。 */
    pk: number | string;
    /** 原文字节数，用于提示体积、判断「没动过」。 */
    bytes: number;
    /** 开头一小段，让用户看得出这是什么。 */
    preview: string;
  };
}

/** 按需展开一个大字段的返回。 */
export interface TraeEditCellResult {
  ok: boolean;
  bytes: number;
  /** 解析好的 JSON 树（不是 JSON 文本时原样返回字符串）。 */
  value: unknown;
}

/** 会话编辑器：一条会话的完整可编辑文档。 */
export interface TraeEditDoc {
  clientKey: string;
  clientLabel: string;
  sessionId: string;
  title: string;
  /** 归属账号 uid（无归属为 null）。 */
  ownerUid: string | null;
  tables: TraeEditTable[];
  /** 表名 → 行数。 */
  counts: Record<string, number>;
  /** 全会话折叠成引用的单元格总数。 */
  refCount: number;
  /** 读取阶段的告警（例如库里缺 chat_message 表导致某些表被跳过）。 */
  warnings: string[];
}

/** 分级校验结果：`errors` 硬拦（不修好不让写），`warnings` 只提醒。 */
export interface TraeEditReport {
  ok: boolean;
  errors: string[];
  warnings: string[];
}

/** 跨账号写入时的一个候选目标账号。 */
export interface TraeEditTargetAccount {
  uid: string;
  /** 显示名（后端统一走 `trae_vault::account_label`，前端别自己截 uid）。 */
  label: string;
  /** 该账号在库里的工程数。 */
  projects: number;
  /** 该账号在库里的会话数。 */
  sessions: number;
}

/** 「跨账号写入」可选的目标账号清单。 */
export interface TraeEditTargets {
  clientKey: string;
  accounts: TraeEditTargetAccount[];
}

/** 写入结果。 */
export interface TraeEditSaveResult {
  ok: boolean;
  mode: "overwrite" | "insert";
  /** 源会话 id。 */
  sourceSessionId: string;
  /** 最终写入的会话 id（`insert` 模式下是新生成的）。 */
  sessionId: string;
  /** 是否做了 id 重映射（`insert` 模式为 true）。 */
  idRemapped: boolean;
  /** 跨账号写入的目标账号 uid（没跨账号时为 null）。 */
  targetUid: string | null;
  /** 表名 → 写入行数。 */
  tables: Record<string, number>;
  rows: number;
  /** 覆写模式下清掉的旧行数。 */
  removedRows: number;
  /**
   * 覆写模式下**原样保留**（用户没改）的表名。
   * 只改标题时这里会是除 `chat_session` 外的全部表 —— 也就是说那 74 MB 一行都没动。
   */
  skippedTables: string[];
  walMerged: number;
  pages: number;
  changedPages: number;
  /** 写入前的整库备份文件。 */
  backup: string[];
  relaunched: boolean;
  warnings: string[];
}

/**
 * 跨**客户端**写入的一个候选目标账号（T46 · S5，只给两个「数据迁移」页用）。
 *
 * ⚠️ 与 `TraeEditTargetAccount` / `WbEditTargetAccount` 的区别：那两个是**同客户端**的
 * 另一个账号；这个是**对面客户端**的账号。
 */
export interface CrossWriteAccount {
  uid: string;
  /** 目标 Trae 客户端的 key；WorkBuddy 侧是空串。 */
  clientKey: string;
  /**
   * `clientKey` 对应的**客户端显示名**（`Trae CN（国内版）` / `Trae（国际版）` / `WorkBuddy`）。
   *
   * ⚠️ 必须与账号名分开给：**Trae CN 与 TRAE SOLO CN 是两个客户端**，本机 3 个 uid 在两个
   * 客户端下各有一份（实测），混在一个账号下拉里既分不清、也选不对（见 [`crossKeyOf`]）。
   */
  clientLabel: string;
  /** 账号显示名，形如 `张三（uid …97eac1）`；**不含客户端名**（客户端由 `clientLabel` 单独给）。 */
  label: string;
  /** 该账号在本机没有会话库 ⇒ 写进去必然失败，界面上标注出来。 */
  noDb?: boolean;
}

/**
 * 跨端写入目标的**唯一标识**：`${clientKey}::${uid}`。
 *
 * ⚠️⚠️ 绝不能只用 `uid`：同一个 uid 会同时出现在 Trae CN 与 TRAE SOLO CN 两个客户端下
 * （实测本机 `…223728` / `…717036`… 都是两套）。老实现按 `uid` 反查账号，
 * `find()` 命中的是列表里第一个同名账号 ⇒ **选了 SOLO 却写到 IDE 库**。
 */
export function crossKeyOf(a: { clientKey: string; uid: string }): string {
  return `${a.clientKey}::${a.uid}`;
}

/**
 * 编辑器页脚的「跨客户端写入」能力（T46 · S5）。
 *
 * 数据迁移页传了它之后，页脚的同客户端「写入方式 / 写入到账号」会被替换成
 * 「写入到 <`toLabel`> 账号」—— 因为那两个页面要的就是**把这条会话搬到对面**。
 *
 * 落地方式遵循设计文档 §6 的约定「跨端写入复用现有导入链路」：
 * ① 有改动先把改动写回**源**会话（导入链路读的是源库里的内容，绕不过去）；
 * ② 再调用目标方向的既有迁移命令把它搬过去。**源会话不会被删。**
 */
export interface CrossWriteTarget {
  /** 目标客户端显示名："Trae" / "WorkBuddy"。 */
  toLabel: string;
  /**
   * 候选目标账号（平铺）。
   *
   * ⚠️ 账号清单里可能有**多个客户端**（Trae 侧就有国内版与国际版）——编辑器会按
   * `clientKey` 自动分组，先让用户选客户端、再列该客户端下的账号，不会混在一个下拉里。
   */
  accounts: CrossWriteAccount[];
  /** 目标账号清单读取失败的原因（非空只提示）。 */
  accountsError?: string | null;
  /** 目标侧当前不可写的原因（本机没有该客户端 / 没有数据），非空即禁用。 */
  blocked?: string | null;
  /** 默认选中的目标账号（[`crossKeyOf`] 形态）。 */
  defaultKey?: string | null;
  /**
   * 把这条会话搬到目标账号。返回一行结果说明（写进编辑器的写入日志）。
   * ⚠️ 调用**之前**编辑器已经把改动写回源会话了。
   */
  push: (target: CrossWriteAccount) => Promise<string>;
}

/**
 * 整库备份保留策略（T62）。
 *
 * ⚠️ 键名 **camelCase**，与后端 `settings::BackupRetention` 的 serde 命名、以及磁盘上
 * `~/.twin-switch/settings.json` 里的键**完全一致**（三处同一个形状，不会出现
 * 「界面存了 A、后端读的是 B」）。
 *
 * 它管的是「删会话 / 编辑回写 / 导入 / 导出」这四条链路留下的整库备份（每批 ≈ 300 MB）。
 */
export interface BackupRetention {
  /** 总开关。关掉 ⇒ 完全不自动清理（备份只增不减，到清理页逐批手动删）。 */
  enabled: boolean;
  /** **时间**：早于这个天数的批次清掉。`0` = 不按时间判。 */
  keepDays: number;
  /** **份数**：每个客户端各自保留多少批，超出的旧批清掉。`0` = 不按份数判。 */
  keepCount: number;
  /** **容量**：每个备份目录最多留多少 MB，从新到旧累加超出后更旧的清掉。`0` = 不按容量判。 */
  maxTotalMb: number;
}

/** 设置快照（`settings_get` / `settings_set` 的返回）。 */
export interface SettingsSnapshot {
  ok: boolean;
  backupRetention: BackupRetention;
  /**
   * 后端生成的策略原文 —— **与真正删文件的判据同源**，界面直接显示它，
   * 别自己再拼一句（那就是「同一屏两个说法」）。
   */
  summary: string;
  /** `settings.json` 的绝对路径（读坏时用户能自己去看）。 */
  path: string;
}

/** 「工具残留」这一类头上的保留策略（与 `BackupRetention` 同源，多一句摘要）。 */
export interface CleanupBackupPolicy extends BackupRetention {
  summary: string;
}
