import { invoke } from "@tauri-apps/api/core";
import type { ErrorLogKind, UpdateCheckResult, UpdateSnapshot } from "./types";
import type {
  BackupRetention,
  CheckinResult,
  SettingsSnapshot,
  TraeAccountOverview,
  TraeAccountToken,
  TraeAccountTokenRefresh,
  TraeBackupInfo,
  WbAccountToken,
  WbAccountTokenRefresh,
  TraeAdoptMergedResult,
  TraeBrowserInfo,
  TraeCleanupReport,
  TraeCleanupScan,
  TraeCreditsResult,
  TraeDecryptReport,
  TraeDecryptedStatus,
  TraeEnsureDecrypted,
  TraeDeleteInfo,
  TraeExportedFile,
  TraeExportAllReport,
  TraeHandoffResult,
  TraeImportCandidate,
  TraeImportInspect,
  TraeImportReport,
  TraeImportResult,
  TraeBatchDeleteReport,
  TraeClientUsageSnapshot,
  TraeInstalledClient,
  TraeOAuthPending,
  TraeOAuthSessionStatus,
  TraeOAuthStartResult,
  TraeProfileRefreshResult,
  TraeScanResult,
  TraeSessionDetail,
  TraeSessionInfo,
  TraeEditCellResult,
  TraeEditDoc,
  TraeEditReport,
  TraeEditSaveResult,
  TraeEditTargets,
  TraeSwitchResult,
  TraeLinksPreview,
  TraeDivergedLinks,
  TraeVaultMeta,
  TraeWbCleanupReport,
  TraeWbCleanupScan,
  TraeWbExportReport,
  TraeWbPreview,
  TraeWbSourceList,
  TraeWbTarget,
  TraeWorkbuddyList, TraeWorkbuddyPreviewItem, TraeWorkbuddyReport,
  WorkbuddyAccountList,
  WorkbuddySwitchPrecheck,
  WorkbuddySwitchResult,
  WorkbuddyOAuthStart,
  WorkbuddyOAuthPoll,
  WorkbuddyImportPreview,
  WbSessionList,
  WbSessionListByAccount,
  WbSessionDetail,
  WbSessionCopyPreview,
  WbGroupCandidatesReport,
  WbGroupUnifyReport,
  WbSessionCopyReport,
  WbSessionGroup,
  WbSessionGroupsReport,
  WbSessionLinkAllReport,
  WbLinksPreview,
  WbSessionDeleteReport,
  WbEditColumn,
  WbEditDoc,
  WbEditLinesResult,
  WbEditOp,
  WbEditReport,
  WbEditSaveResult,
  WbEditTargets,
  Overview,
  OverviewCache,
  OverviewReclaimCache,
  WbCreditItem,
  WbCreditsResult,
  WbCreditsAccounts,
} from "./trae-types";

/** 是否为提供桌面专属能力的 Tauri 宿主（本应用仅桌面分发）。 */
export function isWebui(): boolean {
  return typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);
}

/** Tauri mobile 也注入内部 API；用现有平台 UA 约定把桌面宿主与移动宿主区分开。 */
function isMobilePlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  const ua = navigator.userAgent;
  return (
    /Android|iPhone|iPad|iPod/i.test(ua) ||
    (ua.includes("Macintosh") && navigator.maxTouchPoints > 1)
  );
}

export function isDesktop(): boolean {
  return !isWebui() && !isMobilePlatform();
}

function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(cmd, args);
}

// ---------------------------------------------------------------------------
// Trae 模块：账号切换 / 记录解密导出 / 彻底删除 / 交接记忆
// ---------------------------------------------------------------------------

/**
 * 列出已安装的 Trae 客户端（含登录态与安装路径）。
 *
 * **顺序已由后端排好**：使用记忆（切换次数 ×3 + 打开页面次数）**降序**，
 * 但排序键**第一段是「本机能不能把它跑起来」**—— 跑不起来的排在能跑的后面
 * （2026-10 官方把 SOLO 并进 Trae CN 后，`solo-cn` 的使用分仍最高却没有自己的程序）。
 * 前端不要再自己排序，否则换个页面顺序就变了。`usage.topPick` 供「常用」徽标使用。
 *
 * ⚠️ 界面**不显示**「已并入且本机跑不起来」的客户端 —— 那层过滤在
 * `@/lib/trae-client-list`，别在这里或组件里各写一份。
 */
export function traeListClients(refresh = false): Promise<{
  clients: TraeInstalledClient[];
  usage?: TraeClientUsageSnapshot;
}> {
  // `refresh = true` 时后端会丢掉「注册表探测」的缓存重算一次 ——
  // 用户装完客户端点「重新检测」必须真的重新看一遍系统。
  return call("trae_list_clients", { refresh });
}

/** 清空「客户端使用记忆」（排序回到内置偏好；界面上仍看不到已被摘掉的客户端）。 */
export function traeClientUsageReset(): Promise<{ ok: boolean }> {
  return call("trae_client_usage_reset");
}

/** Trae 账号总览：当前登录态 + 账号库已建档列表。 */
export function traeAccountOverview(clientKey: string): Promise<TraeAccountOverview> {
  return call("trae_account_overview", { clientKey });
}

/** 手动刷新单个账号资料（GetUserInfo 真实昵称 + 积分余额），仅在按钮点击时调用。 */
export function traeRefreshProfile(
  clientKey: string,
  accountId: string,
): Promise<TraeProfileRefreshResult> {
  return call("trae_refresh_profile", { clientKey, accountId });
}

/** 识别当前登录账号（写入账号库前调用，拿 uid 做归属）。 */
export function traeIdentifyLive(clientKey: string): Promise<Record<string, unknown>> {
  return call("trae_identify_live", { clientKey });
}

/** 把当前登录态备份进账号库（写入前自动识别 uid）。 */
export function traeBackupAccount(
  clientKey: string,
  accountId: string,
): Promise<{ ok: boolean; accountId: string; verifiedUid?: string | null; meta?: TraeVaultMeta }> {
  return call("trae_backup_account", { clientKey, accountId });
}

/** 切换到账号库中的某账号（冷切换：终止进程 → 还原载体 → 重启 → daemon 判定）。 */
export function traeSwitchTo(clientKey: string, accountId: string): Promise<TraeSwitchResult> {
  return call("trae_switch_to", { clientKey, accountId });
}

/** 回滚到某账号（本质是切回它，用于切换异常后的恢复）。 */
export function traeRollbackTo(clientKey: string, accountId: string): Promise<TraeSwitchResult> {
  return call("trae_rollback_to", { clientKey, accountId });
}

/** 从账号库删除某个账号的备份（只删本地档案，不影响客户端登录态）。 */
export function traeRemoveAccount(clientKey: string, accountId: string): Promise<{ ok: boolean }> {
  return call("trae_remove_account", { clientKey, accountId });
}

/** 重命名账号库条目（按账号名管理）。 */
export function traeRenameAccount(
  clientKey: string,
  fromId: string,
  toId: string,
): Promise<{ ok: boolean; id: string }> {
  return call("trae_rename_account", { clientKey, fromId, toId });
}

/** 导出账号备份为自包含 JSON（文件内容 base64 内联）。 */
export function traeExportAccount(
  clientKey: string,
  accountId: string,
): Promise<Record<string, unknown>> {
  // ⚠️ 后端 `trae_vault::export_account` 返回的**就是整份载荷本身**
  //    （顶层 `format` / `version` / `entries` / `files`），没有 `{ok, payload}` 这层信封。
  //    以前这里声明成 `{ok,id,payload}`，前端读 `r.payload` 永远是 `undefined`
  //    ⇒ 「导出账号包」把每个账号都当成失败（2026-10-24 用户报的 oauth_* 全军覆没）。
  return call("trae_export_account", { clientKey, accountId });
}

/**
 * 导出**账号包**（写盘版）：后端把所有账号打进一个 JSON 存到磁盘，返回路径。
 *
 * 比前端拼 Blob 下载好在两点：文件有确定的落点（`~/.twin-switch/trae/exports/`），
 * 而且拿到路径就能 `traeRevealPath` 在资源管理器里定位它。
 */
export function traeExportPack(clientKey: string): Promise<{
  count: number;
  failed: { id: string; error: string }[];
  filename: string;
  path: string;
}> {
  return call("trae_export_pack", { clientKey });
}

/** 导入账号备份（自包含 JSON，preferName 可选覆盖账号名）。 */
export function traeImportAccount(
  clientKey: string,
  payload: Record<string, unknown>,
  preferName?: string,
): Promise<{ ok: boolean; id: string; files: number }> {
  return call("trae_import_account", {
    clientKey,
    payload,
    ...(preferName ? { preferName } : {}),
  });
}

/** 已存盘的 SQLCipher 密钥（供状态展示）。 */
export function traeSavedKey(clientKey: string): Promise<{ clientKey: string; key: string | null }> {
  return call("trae_saved_key", { clientKey });
}

/** 一键：扫描进程内存提密钥 → 校验 HMAC → 解密整库到明文 SQLite。 */
export function traeScanAndDecrypt(
  clientKey: string,
): Promise<{
  scan: TraeScanResult;
  report: TraeDecryptReport;
  decryptedDb: string;
  progress: string[];
}> {
  return call("trae_scan_and_decrypt", { clientKey });
}

/** 用已存密钥直接解密（跳过内存扫描，密钥过期会报 HMAC 失败）。 */
export function traeDecryptWithSavedKey(
  clientKey: string,
): Promise<{ report: TraeDecryptReport; decryptedDb: string }> {
  return call("trae_decrypt_with_saved_key", { clientKey });
}

/** 解密库状态（是否已生成 + 表行数概览）。 */
export function traeDecryptedStatus(clientKey: string): Promise<TraeDecryptedStatus> {
  return call("trae_decrypted_status", { clientKey });
}

/** 确保解密快照可用且与实时库一致（一致则零解密直接复用）。 */
export function traeEnsureDecrypted(clientKey: string): Promise<TraeEnsureDecrypted> {
  return call("trae_ensure_decrypted", { clientKey });
}

/** 一键回收工作文件占用：清理三处旧备份（各留最新 1 批）+ 删除可再生的解密快照。 */
export function traeCleanupWorkingFiles(): Promise<{
  freed_bytes: number;
  freed_mb: number;
  removed_entries: number;
  details: { name: string; removed: number; beforeMb?: number; freedMb: number }[];
}> {
  return call("trae_cleanup_working_files", {});
}

/** Trae 会话列表（解密库，按最后活动倒序）。 */
export function traeListSessions(
  clientKey: string,
): Promise<{ sessions: TraeSessionInfo[] }> {
  return call("trae_list_sessions", { clientKey });
}

/**
 * 单会话详情（标题 / 轮数 / 完整对话）。
 *
 * ⚠️ 记录页那个**只读的「会话详情」弹窗已撤掉**（「查看详情」按钮改成了「编辑会话」），
 * 所以这个包装函数**当前没有调用方**。保留它、而不是连后端命令一起删，是因为它是
 * 唯一一个「不进编辑态、不碰库」就能整条读会话内容的入口，排障时用得上。
 * 若哪天确认不需要，删这里的同时记得删 `src-tauri` 的 `trae_session_detail` 注册。
 */
export function traeSessionDetail(
  clientKey: string,
  sessionId: string,
): Promise<TraeSessionDetail> {
  return call("trae_session_detail", { clientKey, sessionId });
}

/** 导出单个会话为 MD 文件（导出目录内自动去重命名）。 */
export function traeExportSession(
  clientKey: string,
  sessionId: string,
): Promise<TraeExportedFile> {
  return call("trae_export_session", { clientKey, sessionId });
}

// ---------------------------------------------------------------------------
// T46 · 会话编辑器
// ---------------------------------------------------------------------------

/** 会话编辑器：读出该会话在全部相关表里的行（**只读**，不碰任何文件）。 */
export function traeSessionEditDoc(
  clientKey: string,
  sessionId: string,
): Promise<TraeEditDoc> {
  return call("trae_session_edit_doc", { clientKey, sessionId });
}

/**
 * 会话编辑器：分级校验（纯计算，不碰库）。
 *
 * ⚠️ 规则只有后端这一份实现 —— 前端只负责显示 `errors` / `warnings`，
 * **绝不在前端重写一遍**（同一套规则两处独立推导，必然跑偏一次）。
 */
export function traeSessionEditValidate(doc: unknown): Promise<TraeEditReport> {
  return call("trae_session_edit_validate", { doc });
}

/**
 * 会话编辑器：写回。
 *
 * ⚠️ 写库操作（结束客户端 → 备份 → 解密 → 合并 WAL → 改 → 增量回写 → 校验 → 重启），
 * 全程几十秒，界面要给足进度反馈。
 *
 * `mode`：`overwrite` 就地覆写（保持 session_id）；`insert` 同库新增（自动重映射全部 id）。
 *
 * `dirtyTables`：只有这些表会被重写，其余原样保留。**不传 = 全部重写**（旧行为，最保险）。
 * 这只是性能提示，标错不会写坏数据：少标 = 少写一张表（库里保持原样）。
 *
 * `targetUid`（S2 跨账号）：把**新增**的会话写进另一个账号。只对 `mode = "insert"` 生效，
 * 且必须是 `traeSessionEditTargets()` 列出的 uid。后端会一并改写
 * `chat_session.project_id` / `session_project.project_id` / `context.last_real_project_id`。
 */
export function traeSessionEditSave(
  doc: unknown,
  mode: "overwrite" | "insert",
  dirtyTables?: string[],
  targetUid?: string,
): Promise<TraeEditSaveResult> {
  return call("trae_session_edit_save", { doc, mode, dirtyTables, targetUid });
}

/**
 * 会话编辑器：可写入的目标账号清单（**只读**）。
 *
 * 数据源是库里的 `project.user_id`（= Trae 判定会话归属的那一列），显示名由后端
 * 统一走 `trae_vault::account_label` —— 前端**别自己截 uid 尾号**。
 */
export function traeSessionEditTargets(clientKey: string): Promise<TraeEditTargets> {
  return call("trae_session_edit_targets", { clientKey });
}

/**
 * 会话编辑器：生成一批新的 24 位 hex id（「新增回合 / 复制消息」用）。
 *
 * ⚠️ 别在前端自己造 id —— 生成规则只有一处实现。形态一旦跑偏，症状是
 * 「写入成功但客户端打不开这条会话」，属于最难查的一类问题。
 */
export function traeSessionEditNewIds(count: number): Promise<string[]> {
  return call("trae_session_edit_new_ids", { count });
}

/**
 * 会话编辑器：按需拉取一个**大字段**的完整内容（点「展开」时调）。
 *
 * 超过 32 KB 的 JSON 单元格在 `edit_doc` 里只回了 `$jsonRef` 引用
 * —— 一条会话实测 90 MB，全量塞给前端会把渲染冻死。
 */
export function traeSessionEditCell(
  clientKey: string,
  sessionId: string,
  table: string,
  column: string,
  pk: number | string,
): Promise<TraeEditCellResult> {
  return call("trae_session_edit_cell", { clientKey, sessionId, table, column, pk });
}

/** 一键导出全部会话为 zip（可跨数据源）。 */
export function traeExportAll(sources: string[]): Promise<TraeExportAllReport> {
  return call("trae_export_all", { sources });
}

/** 导出目录绝对路径（提示 / 打开用）。 */
export function traeExportDir(): Promise<string> {
  return call("trae_export_dir", {});
}

/** 在文件管理器中定位文件/目录。 */
export function traeRevealPath(path: string): Promise<void> {
  return call("reveal_path", { path });
}

/** 删除预览：标题 / 各表行数 / 磁盘文件（只读，不删任何东西）。 */
export function traeDeleteInfo(clientKey: string, sessionId: string): Promise<TraeDeleteInfo> {
  return call("trae_delete_info", { clientKey, sessionId });
}

/** 彻底删除会话：整库备份 → 写实时加密库删行 → 同步删解密库 → 文件移入回收站。 */
export function traeDeleteSession(
  clientKey: string,
  sessionId: string,
): Promise<Record<string, unknown> & { progress?: string[] }> {
  return call("trae_delete_session", { clientKey, sessionId });
}

/**
 * **批量**彻底删除会话。
 *
 * 与单条删除是同一条链路，但整批只走一趟：一次解密、一次增量回写、**一份**整库备份。
 * 逐条调用会让备份目录按整库大小线性膨胀（删 20 条 = 20 份全库备份）。
 */
export function traeDeleteSessions(
  clientKey: string,
  sessionIds: string[],
): Promise<TraeBatchDeleteReport> {
  return call("trae_delete_sessions", { clientKey, sessionIds });
}

/** 账号维度导入候选：全部本机账号（vault + 解密库 local + 当前登录态），可自由切换目标。 */
export function traeImportCandidates(
  exclude: string,
  sessionId?: string,
): Promise<{ candidates: TraeImportCandidate[]; hints: string[] }> {
  return call("trae_import_candidates", { exclude, sessionId });
}

/** 目标账号导入就绪状态探测（不写库）。 */
export function traeImportInspect(
  clientKey: string,
  accountId: string,
): Promise<TraeImportInspect> {
  return call("trae_import_inspect", { clientKey, accountId });
}

/** 跨账号导入会话：源账号（已解密）→ 目标账号本地库（进度走 trae-import-progress 事件）。 */
export function traeImportRun(
  src: string,
  dst: string,
  uid: string | null,
  sessions: string[],
): Promise<TraeImportReport> {
  return call("trae_import_run", { src, dst, uid, sessions });
}

/**
 * 把「已并入该客户端」的客户端账号库里的账号一并带入。
 *
 * 2026-10 官方把 TRAE SOLO 并进 Trae 时（国内版 → `trae-cn`，国际版 → `trae-intl`）
 * 只搬了程序与会话，**本工具自己的账号库官方不会碰** ⇒ SOLO 下备份过的账号在承载方
 * 下看不见、切不过去。
 * `dryRun` 只预演（列出会搬哪些），一个字节都不写。
 */
export function traeVaultAdoptMerged(
  clientKey: string,
  dryRun?: boolean,
): Promise<TraeAdoptMergedResult> {
  return call("trae_vault_adopt_merged", { clientKey, dryRun });
}

/**
 * 账号的 jwt 本体与有效期（点账号头像弹窗用）。**只读、不联网。**
 *
 * 凭证取自该账号自己的档案：优先 `oauth.json`（网页登录凭证），其次它的载体
 * `storage.json`（切换来的账号）。两者都没有 ⇒ 报错，界面照实说明。
 */
export function traeAccountToken(
  clientKey: string,
  accountId: string,
): Promise<TraeAccountToken> {
  return call("trae_account_token", { clientKey, accountId });
}

/**
 * 刷新账号 token（设备签名续期 ExchangeToken）。
 *
 * ⚠️ `force = false` 时，token 剩余 > 7 天**不是失败**：返回
 * `{ ok: true, refreshed: false, reason: "fresh" }`，界面据此弹窗让用户决定要不要强制刷新。
 * 真失败（网络 / 服务端拒绝 / 写回自检不过）才抛错。
 */
export function traeAccountTokenRefresh(
  clientKey: string,
  accountId: string,
  force?: boolean,
): Promise<TraeAccountTokenRefresh> {
  return call("trae_account_token_refresh", { clientKey, accountId, force });
}

/**
 * WorkBuddy 账号 token（jwt 本体 + 有效期 + 能不能续期）。**只读、不联网。**
 *
 * 凭据取自本工具账号库（`~/.twin-switch/workbuddy-accounts.json`）。
 */
export function workbuddyAccountToken(accountId: string): Promise<WbAccountToken> {
  return call("workbuddy_account_token", { accountId });
}

/**
 * 刷新 WorkBuddy 账号 token（`POST {base}/v2/billing/meter/auth/token/refresh`）。
 *
 * 语义与 Trae 侧一致：`force = false` 且剩余 > 7 天 ⇒ 返回
 * `{ refreshed: false, reason: "fresh" }`（**不是失败**），界面据此弹「强制刷新」确认窗。
 */
export function workbuddyAccountTokenRefresh(
  accountId: string,
  force?: boolean,
): Promise<WbAccountTokenRefresh> {
  return call("workbuddy_account_token_refresh", { accountId, force });
}

/** 诊断：列出「会话的工程归属与关联表不一致」的会话（只读）。 */
export function traeFindMisalignedProjects(clientKey: string): Promise<{
  count: number;
  sessions: {
    session_id: string;
    session_project_id: string;
    chat_session_project_id: string;
  }[];
}> {
  return call("trae_find_misaligned_projects", { clientKey });
}

/** 自愈：把历史副本的工程归属对齐到其真实账号的项目（一次修干净）。 */
export function traeHealSessionProjects(clientKey: string): Promise<{
  checked: boolean;
  fixed_sessions: number;
  fixed_contexts: number;
  sessions: string[];
  /**
   * 备份目录。⚠️ **`null` = 未备份**（「设置 → 写库前整库备份」关着，默认如此），
   * 与「备份失败」（会抛错）是两回事。
   */
  backup: string | null;
  /** `"delta"` / `"full"` / `"skipped"`。 */
  backup_mode: string;
  relaunched: boolean;
  note: string;
}> {
  return call("trae_heal_session_projects", { clientKey });
}

/**
 * 同客户端下两个账号之间的会话副本关联（只读）。
 *
 * 会读一次已解密库判定副本存活；`client_key` 是 Trae 客户端键（如 `trae-cn`）。
 */
export function traeSessionLinksPreview(
  clientKey: string,
  sourceUid: string,
  targetUid: string,
): Promise<TraeLinksPreview> {
  return call("trae_session_links_preview", { clientKey, sourceUid, targetUid });
}

/** 解除一个关联组（只删本工具的关联记录，不动任何会话数据）。 */
export function traeSessionUnlink(
  groupId: string,
): Promise<{ ok: boolean; remaining: number }> {
  return call("trae_session_unlink", { groupId });
}

/**
 * 「同步差异」：把关联组里较新一端的内容就地覆盖到较旧一端。
 *
 * ⚠️ 这是**写库**操作（会结束客户端 → 备份 → 增量回写 → 校验 → 重启），
 * 耗时几十秒。进度走 `trae-import-progress` 事件。
 *
 * `direction` 指「以哪一端为准」：`sourceToTarget` / `targetToSource`。
 */
export function traeSessionSyncGroup(
  clientKey: string,
  groupId: string,
  direction: "sourceToTarget" | "targetToSource",
): Promise<{
  ok: boolean;
  groupId: string;
  direction: string;
  keptSid: string;
  fromSid: string;
  removedRows: number;
  writtenRows: number;
  pages: number;
  relaunched: boolean;
  /** ⚠️ `null` = 未备份（开关关着），与「备份失败」不是一回事。 */
  backupDir: string | null;
  backupMode: string;
  note: string;
}> {
  return call("trae_session_sync_group", { clientKey, groupId, direction });
}

/**
 * **批量**「同步差异」：一次写库周期完成多个关联组。
 *
 * 逐组调 `traeSessionSyncGroup` 会把客户端重启 N 次、备份 N 份；批量版只重启一次。
 * 同样是写库操作（会结束客户端 → 备份 → 增量回写 → 校验 → 重启），耗时几十秒。
 */
export function traeSessionSyncGroups(
  clientKey: string,
  items: { groupId: string; direction: "sourceToTarget" | "targetToSource" }[],
): Promise<{
  ok: boolean;
  count: number;
  groups: {
    groupId: string;
    direction: string;
    keptSid: string;
    fromSid: string;
    removedRows: number;
    writtenRows: number;
  }[];
  pages: number;
  relaunched: boolean;
  /** ⚠️ `null` = 未备份（开关关着），与「备份失败」不是一回事。 */
  backupDir: string | null;
  backupMode: string;
}> {
  return call("trae_session_sync_groups", { clientKey, items });
}

/**
 * 切号后的**分叉探测**（只读）：某 uid 参与的关联组里，哪些两端已经不一致。
 *
 * 每一项都带 `partnerUid` / `selfRole` / `suggestedDirection`，供前端弹「要不要同步」。
 * `count` 为 0 表示没有需要提醒的内容。
 */
export function traeSessionLinksDiverged(
  clientKey: string,
  uid: string,
): Promise<TraeDivergedLinks> {
  return call("trae_session_links_diverged", { clientKey, uid });
}

/** 交接记忆预览：解密库自动生成条目 → 组装文档，报告落点，不落盘。 */
export function traeHandoffPreview(args: {
  clientKey: string;
  projectPath?: string;
  sessionIds?: string[];
  nextSteps?: string[];
  keyFiles?: string[];
  note?: string;
}): Promise<TraeHandoffResult> {
  return call("trae_handoff_preview", args as unknown as Record<string, unknown>);
}

/** 写入交接记忆：项目工作目录 + 项目规则 + Trae 记忆库 topics 追加 + 工具目录归档。 */
export function traeHandoffWrite(args: {
  clientKey: string;
  projectPath?: string;
  sessionIds?: string[];
  nextSteps?: string[];
  keyFiles?: string[];
  note?: string;
}): Promise<TraeHandoffResult> {
  return call("trae_handoff_write", args as unknown as Record<string, unknown>);
}

// ---------------------------------------------------------------------------
// Trae 网页（OAuth）登录（trae_oauth_* 命令）
// ---------------------------------------------------------------------------

/** 发起 Trae 网页登录：起回环监听，返回授权页 URL（不自动打开浏览器）。 */
export function traeOauthStart(clientKey: string, name?: string): Promise<TraeOAuthStartResult> {
  return call("trae_oauth_start", {
    clientKey,
    ...(name ? { name } : {}),
  });
}

/**
 * 网页登录状态轮询（约 1.5s 一次）。
 * 收到回调时后端在本调用内驱动完成 token 交换并落库，返回终态。
 */
export function traeOauthStatus(): Promise<TraeOAuthSessionStatus> {
  return call("trae_oauth_status");
}

/** 停止网页登录监听（未启动时幂等）。 */
export function traeOauthStop(): Promise<{ ok: boolean }> {
  return call("trae_oauth_stop");
}

/** 查询是否有「已落盘但本进程没在监听」的登录会话（服务重启 / 弹层关闭后）。 */
export function traeOauthPending(): Promise<{ pending: TraeOAuthPending | null }> {
  return call("trae_oauth_pending");
}

/** 手动粘贴授权页回调 URL 完成登录（不依赖回环监听）。 */
export function traeOauthManual(
  clientKey: string,
  callbackUrl: string,
  name?: string,
): Promise<TraeOAuthSessionStatus> {
  return call("trae_oauth_manual", {
    clientKey,
    callbackUrl,
    ...(name ? { name } : {}),
  });
}

/** 打开授权页：默认系统浏览器，或指定浏览器的私密窗口。 */
export function traeOauthOpenUrl(
  url: string,
  privateMode?: boolean,
  browser?: string,
): Promise<{ ok: boolean; private: boolean; browser?: string; browserKey?: string }> {
  return call("trae_oauth_open_url", {
    url,
    ...(privateMode ? { private: true } : {}),
    ...(browser ? { browser } : {}),
  });
}

/** 本机可用浏览器列表（私密窗口打开用）。 */
export function traeOauthBrowsers(): Promise<{ browsers: TraeBrowserInfo[] }> {
  return call("trae_oauth_browsers");
}

// ---------------------------------------------------------------------------
// 本地登录态导入（trae_import_* 命令）
// ---------------------------------------------------------------------------

/** 导入本地登录态：解密指定客户端 storage.json 的授权条目，落库为凭证账号。 */
export function traeImportLocalLogin(clientKey: string): Promise<TraeImportResult> {
  return call("trae_import_local_login", { clientKey });
}

/** 一键导入全部已安装客户端的本地登录态。 */
export function traeImportAllLocalLogins(): Promise<{ results: TraeImportResult[] }> {
  return call("trae_import_all_local_logins");
}

// ---------------------------------------------------------------------------
// 错误日志（前端崩溃 / 未捕获错误落盘，见 lib/error-report.ts）
// ---------------------------------------------------------------------------

/** 把 Tauri command 抛出的错误统一为 Error。 */
export function asError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e ?? "未知错误");
}

/** 上报一条错误到本地错误日志（落盘 `~/.twin-switch/error.log`）。 */
export function logError(kind: ErrorLogKind, message: string, detail?: string): Promise<void> {
  return call<unknown>("log_error", { kind, message, detail: detail ?? null }).then(
    () => undefined,
  );
}

/** 错误日志文件路径。 */
export function getErrorLogPath(): Promise<string> {
  return call<string>("get_error_log_path");
}

/** 在文件管理器中定位错误日志（日志尚未生成时由后端打开所在目录）。 */
export function revealErrorLog(): Promise<void> {
  return call<unknown>("reveal_error_log").then(() => undefined);
}

// ---------------------------------------------------------------------------
// WorkBuddy → Trae 会话移植
// ---------------------------------------------------------------------------

/** 列出本机 WorkBuddy 会话（只读，供勾选导入）。 */
export function traeWorkbuddyList(): Promise<TraeWorkbuddyList> {
  return call("trae_workbuddy_list");
}

/** 转换预览：报告每个会话将转换出多少回合与工具步骤（只读，不写库）。 */
export function traeWorkbuddyPreview(sessions: string[]): Promise<{
  preview: TraeWorkbuddyPreviewItem[];
}> {
  return call("trae_workbuddy_preview", { sessions });
}

/**
 * 把选中的 WorkBuddy 会话移植进目标 Trae 账号本地库。
 * 会先退出目标客户端，完成后自动重启；进度走 `trae-workbuddy-progress` 事件。
 */
export function traeWorkbuddyImport(
  dst: string,
  uid: string,
  sessions: string[],
): Promise<TraeWorkbuddyReport> {
  return call("trae_workbuddy_import", { dst, uid, sessions });
}

// ---------------------------------------------------------------------------
// Trae → WorkBuddy 会话导出（反向）
// ---------------------------------------------------------------------------

/** WorkBuddy 侧现状：数据根、客户端是否在运行、可选目标账号（只读）。 */
export function traeWbExportTarget(): Promise<TraeWbTarget> {
  return call("trae_wb_export_target");
}

/** 某个 Trae 客户端里可导出的会话列表（读解密快照）。 */
export function traeWbExportSessions(clientKey: string): Promise<TraeWbSourceList> {
  return call("trae_wb_export_sessions", { clientKey });
}

/** 导出预览：报告回合数 / 工具步骤 / 将落到哪个 WorkBuddy 工作区（只读）。 */
export function traeWbExportPreview(
  clientKey: string,
  sessions: string[],
): Promise<TraeWbPreview> {
  return call("trae_wb_export_preview", { clientKey, sessions });
}

/**
 * 把选中的 Trae 会话导出成 WorkBuddy 的明文会话。
 * 会先退出 WorkBuddy 桌面版，写完再自动拉起；进度走 `trae-wb-export-progress` 事件。
 */
export function traeWbExportRun(
  clientKey: string,
  uid: string,
  sessions: string[],
): Promise<TraeWbExportReport> {
  return call("trae_wb_export_run", { clientKey, uid, sessions });
}

// ---------------------------------------------------------------------------
// WorkBuddy 本机垃圾清理（扫描 + 受控清除）
// ---------------------------------------------------------------------------

/**
 * 扫描本机 WorkBuddy 的可清理项（只读）。
 * 默认**有缓存就用缓存**（毫秒级，不按时间过期，超 10 分钟只在结果里标 `stale`）；
 * `force = true` 强制重扫 —— 只有「重新扫描」按钮和清空回收站这类动作才该传。
 */
export function traeWbCleanupScan(force = false): Promise<TraeWbCleanupScan> {
  return call("trae_wb_cleanup_scan", { force });
}

/**
 * 清理选中的项。
 * `hard = false` 移入工具回收站（可搬回）；`hard = true` 彻底删除。
 * 两种情况都会先退出 WorkBuddy 并整份备份数据库；进度走 `trae-wb-cleanup-progress`。
 */
export function traeWbCleanupPurge(
  ids: string[],
  hard: boolean,
): Promise<TraeWbCleanupReport> {
  return call("trae_wb_cleanup_purge", { ids, hard });
}

/**
 * 清空工具回收站，彻底释放被清理文件占用的空间。
 *
 * ⚠️ 后端是**逐项删 + 短退避重试 + 先清只读**的，所以这里可能拿到**部分成功**：
 * `failed_count > 0` 表示还有条目删不掉（多半是被某个进程长期占用），
 * `failed` 里是「条目名（失败原因）」。**别把这种情况当整体失败** —— 能释放的已经释放了。
 */
export function traeWbCleanupEmptyTrash(): Promise<{
  removed: number;
  bytes: number;
  dir: string;
  /** 删不掉的条目：「名称（原因）」。 */
  failed: string[];
  failed_count: number;
}> {
  return call("trae_wb_cleanup_empty_trash");
}

// ---------------------------------------------------------------------------
// 设置（`~/.twin-switch/settings.json`）
// ---------------------------------------------------------------------------

/**
 * 读设置（整库备份保留策略）。
 *
 * ⚠️ **会如实报错**：「文件不存在」= 全新安装 = 默认值；「文件在但坏了」= 抛错。
 *    调用方不能把失败当成「用了默认值」——那会让用户以为自己的设置被重置了。
 */
/** 取「正在运行的 Qoder」主程序路径（设置页「用运行中的」，按区域分开）。 */
export function qoderExeFromRunning(region: "cn" | "global"): Promise<{
  ok: boolean;
  region: string;
  path: string;
}> {
  return call("qoder_exe_from_running", { region });
}

export function settingsGet(): Promise<SettingsSnapshot> {
  return call("settings_get", {});
}

/**
 * 写设置（**增量补丁**：只写你给了的字段，没给的一律保持原样）。
 *
 * ⚠️ 别传「整个 SettingSnapshot」—— 那会把另一项用前端自己以为的值覆盖掉
 * （两个界面同时开着就会互相踩）。
 *
 * 返回**实际落盘之后**的快照（后端回读一次），调用方直接用返回值刷界面 ——
 * 这样不会出现「写成功了但界面还显示旧值」的两处不同源。
 */
export function settingsSet(patch: {
  backupRetention?: BackupRetention;
  backupBeforeWrite?: boolean;
  /**
   * 手动指定的客户端主程序路径（`clientKey → 绝对路径`）。
   *
   * ⚠️ **整表覆盖**：传「改完之后的全量映射」，某个键不存在 = 那个客户端恢复自动探测。
   * 后端会先校验「是不是一个真实存在的 .exe」，不通过就整体报错、一个都不写。
   */
  traeExe?: Record<string, string>;
  /** 手动指定的 WorkBuddy 主程序路径（空串 = 恢复自动探测）。 */
  workbuddyExe?: string;
  /** 手动指定的 Qoder 主程序路径（`cn | global → 绝对路径`，整表覆盖）。 */
  qoderExe?: Record<string, string>;
  /** 切号时是否同步 WorkBuddy 的定时任务。 */
  syncWorkbuddyAutomations?: boolean;
  /** 切号时是否把 WorkBuddy 会话自动同步给所有账号（写会话库）。 */
  autoLinkWorkbuddySessions?: boolean;
}): Promise<SettingsSnapshot> {
  return call("settings_set", { patch });
}

/**
 * 取「正在运行的客户端」的主程序路径（设置页的「用运行中的客户端」）。
 *
 * 为什么需要：Trae 允许装到任意目录，自动探测再全也可能漏；**它正在跑**本身就是
 * 最可靠的线索。没在跑时后端**如实报错**（不会回一个猜的路径）——
 * 填错路径的后果是「切换时启动了别的东西」，比「没找到」难查得多。
 */
export function traeExeFromRunning(clientKey: string): Promise<{
  ok: boolean;
  clientKey: string;
  label: string;
  path: string;
}> {
  return call("trae_exe_from_running", { clientKey });
}

/**
 * 取「正在运行的 WorkBuddy」的主程序路径（设置页 WorkBuddy 那一行的按钮）。
 *
 * 没在跑时后端**如实报错**，不会回一个猜的路径。
 */
export function workbuddyExeFromRunning(): Promise<{ ok: boolean; path: string }> {
  return call("workbuddy_exe_from_running");
}

/**
 * 一批备份的概览（只读）：增量还是整份、省了多少、**能不能还原**。
 *
 * ⚠️ 「可以还原」只听后端的 `canRestore` —— 前端别自己看目录里有没有文件。
 */
export function traeBackupInspect(backupDir: string): Promise<TraeBackupInfo> {
  return call("trae_backup_inspect", { backupDir });
}

/**
 * 把一批备份还原到实时库。
 *
 * ⚠️ 后端会：① 只认本工具自己的备份根目录；② **先停客户端**、还原完自动重启；
 * ③ 还原前先把当前库**另存一份**（还原错了还能退回）。三者都在后端兜住。
 */
export function traeBackupRestore(
  clientKey: string,
  backupDir: string,
): Promise<Record<string, unknown>> {
  return call("trae_backup_restore", { clientKey, backupDir });
}

// ---------------------------------------------------------------------------
// Trae 本机清理（会话 / 工具残留 / 客户端残留 / 回收站）
// ---------------------------------------------------------------------------

/**
 * 扫描本机 Trae 相关的可清理项（只读）。
 * 默认**有缓存就用缓存**（毫秒级，不按时间过期，超 10 分钟只在结果里标 `stale`）；
 * `force = true` 强制重扫 —— 只有「重新扫描」按钮和清空回收站这类动作才该传。
 */
export function traeCleanupScan(force = false): Promise<TraeCleanupScan> {
  return call("trae_cleanup_scan", { force });
}

/**
 * 账号库积分的**离线快照**：只读各账号的 profile.json，不联网、毫秒级。
 * 首屏先拿它渲染。
 */
export function traeCreditsCached(clientKey?: string | null): Promise<TraeCreditsResult> {
  return call("trae_credits_cached", { clientKey: clientKey ?? null });
}

/**
 * 查询账号库积分（额度 + 逐个积分包）。
 * `force = false` 命中 5 分钟缓存直接返回；`force = true` 逐个账号真打接口。
 */
export function traeCreditsQuery(
  clientKey?: string | null,
  force = false,
): Promise<TraeCreditsResult> {
  return call("trae_credits_query", { clientKey: clientKey ?? null, force });
}

/**
 * 一键签到（Trae / WorkBuddy 的账号）。
 *
 * `scope = "all"`（默认）签两边；`"trae"` / `"workbuddy"` 只签一边。
 * `clientKey` **只在 Trae 侧生效**，限定本客户端下的账号（账号管理页按当前选中的客户端签）。
 * `clientKey: null` = Trae 的全部客户端。
 *
 * 每个账号一条结果，`status` 六档（见 `CheckinStatus`）互不合并 ——
 * 单账号失败不会影响其它账号，也不会让整个调用抛错。
 */
export function checkinRun(
  scope: "all" | "trae" | "workbuddy" | "qoder" = "all",
  clientKey?: string | null,
): Promise<CheckinResult> {
  return call("checkin_run", { scope, clientKey: clientKey ?? null });
}

/**
 * 执行清理。
 * `hard = false`（默认）把文件/目录移入工具回收站（可搬回）；`hard = true` 彻底删除。
 * 选中会话时走批量删除链路（一次解密、一次增量回写、一份整库备份），本地删完再尽力同步
 * 云端记录。进度走 `trae-cleanup-progress`。
 */
export function traeCleanupPurge(
  ids: string[],
  hard: boolean,
): Promise<TraeCleanupReport> {
  return call("trae_cleanup_purge", { ids, hard });
}

/** 清空本页回收站，彻底释放磁盘。 */
export function traeCleanupEmptyTrash(): Promise<{
  ok: boolean;
  removed: number;
  before_mb: number;
  freed_bytes: number;
  freed_mb: number;
  /** 删不掉的条目：「名称（原因）」。⚠️ 原来失败是静默的，用户看到「已清空」却仍有残留。 */
  failed: string[];
  failed_count: number;
}> {
  return call("trae_cleanup_empty_trash");
}

// ---------------------------------------------------------------------------
// WorkBuddy 账号管理（国内版）
// ---------------------------------------------------------------------------

export function workbuddyAccountList(): Promise<WorkbuddyAccountList> {
  return call("workbuddy_account_list");
}

export function workbuddySwitchPrecheck(accountId: string): Promise<WorkbuddySwitchPrecheck> {
  return call("workbuddy_switch_precheck", { accountId });
}

export function workbuddySwitchTo(accountId: string, relaunch: boolean): Promise<WorkbuddySwitchResult> {
  return call("workbuddy_switch_to", { accountId, relaunch });
}

export function workbuddyRollback(): Promise<{ ok: boolean; uid: string | null; restoredFrom: string }> {
  return call("workbuddy_rollback");
}

export function workbuddyImportLocal(): Promise<Record<string, unknown>> {
  return call("workbuddy_import_local");
}

/**
 * 搬家：把参考工具账号库里**有明文凭据**的账号并进本工具账号库。
 *
 * 为什么需要它：本工具账号库里的凭据是 WorkBuddy 加密信封，本地解不出明文，查不了积分；
 * 搬一次明文进来之后就与外部账号库再无关系（积分链路已完全不读它）。
 *
 * ⚠️ **由 `main.tsx` 的启动编排自动调用**，界面上没有按钮（2026-10-09 起）。
 * 所以这个函数**失败不该弹错**：对方没装那个工具、文件损坏都是常态。
 * 重复调用是幂等的（本地已是明文就落入 `kept`，不重写）。
 */
export function workbuddyImportReferenceAccounts(): Promise<{
  ok: boolean;
  /** 参考工具账号库读到了**且**能解析。false = 对方没装/没登录过/文件坏了，**不是错误**。 */
  available: boolean;
  created: number;
  updated: number;
  /** 来源记录本身不可用（加密信封 / 空 token / 无 uid）。 */
  skipped: number;
  /** 本地已经是可用的明文凭据，**刻意保留**（本地那份通常更新），不是失败。 */
  kept: number;
  /** 用户**亲手删过**的身份，刻意不搬回来（护栏生效，不是失败）。 */
  blocked: number;
  /** `created + updated`：前端只需要这一个数就能决定要不要提示、要不要刷新。 */
  imported: number;
  total: number;
  /** `available === false` 时给出的原因。 */
  note: string | null;
}> {
  return call("workbuddy_import_reference_accounts");
}

export function workbuddyRemoveAccount(accountId: string): Promise<{ ok: boolean }> {
  return call("workbuddy_remove_account", { accountId });
}

export function workbuddyRenameAccount(accountId: string, name: string): Promise<{ ok: boolean }> {
  return call("workbuddy_rename_account", { accountId, name });
}

export function workbuddyExportAccounts(): Promise<Record<string, unknown>> {
  return call("workbuddy_export_accounts");
}


/** 导出账号包（写盘版）：后端存到 `~/.twin-switch/workbuddy/exports/` 并返回路径。 */
export function workbuddyExportPack(): Promise<{
  count: number;
  filename: string;
  path: string;
}> {
  return call("workbuddy_export_pack", {});
}
export function workbuddyPreviewImport(payload: unknown): Promise<WorkbuddyImportPreview> {
  return call("workbuddy_preview_import", { payload });
}

export function workbuddyImportAccounts(payload: unknown, mode?: string): Promise<Record<string, unknown>> {
  return call("workbuddy_import_accounts", { payload, mode });
}

export function workbuddyOauthStart(): Promise<WorkbuddyOAuthStart> {
  return call("workbuddy_oauth_start");
}

export function workbuddyOauthPoll(loginId: string): Promise<WorkbuddyOAuthPoll> {
  return call("workbuddy_oauth_poll", { loginId });
}

export function workbuddyOauthStop(loginId: string): Promise<void> {
  return call("workbuddy_oauth_stop", { loginId });
}

export function workbuddyOpenUrl(url: string): Promise<{ ok: boolean }> {
  return call("workbuddy_open_url", { url });
}

// ---------------------------------------------------------------------------
// WorkBuddy 会话记录 / 复制 / 关联（国内版）
// ---------------------------------------------------------------------------

export function workbuddySessionListByAccount(): Promise<WbSessionListByAccount> {
  return call("workbuddy_session_list_by_account");
}

export function workbuddySessionList(uid: string): Promise<WbSessionList> {
  return call("workbuddy_session_list", { uid });
}

export function workbuddySessionDetail(uid: string, cid: string): Promise<WbSessionDetail> {
  return call("workbuddy_session_detail", { uid, cid });
}

export function workbuddySessionExportMd(
  uid: string,
  cid: string,
): Promise<{ ok: boolean; path: string; bytes: number; turns: number; title: string }> {
  return call("workbuddy_session_export_md", { uid, cid });
}

export function workbuddySessionDelete(uid: string, ids: string[]): Promise<WbSessionDeleteReport> {
  return call("workbuddy_session_delete", { uid, ids });
}

export function workbuddySessionCopyPreview(
  sourceUid: string,
  targetUid: string,
  ids: string[],
): Promise<WbSessionCopyPreview> {
  return call("workbuddy_session_copy_preview", { sourceUid, targetUid, ids });
}

export function workbuddySessionCopy(
  sourceUid: string,
  targetUid: string,
  ids: string[],
): Promise<WbSessionCopyReport> {
  return call("workbuddy_session_copy", { sourceUid, targetUid, ids });
}

export function workbuddySessionLinksPreview(
  sourceUid: string,
  targetUid: string,
): Promise<WbLinksPreview> {
  return call("workbuddy_session_links_preview", { sourceUid, targetUid });
}

/**
 * 一键互通：把本机所有会话补齐到所有 WorkBuddy 账号，并登记成关联组。
 *
 * 幂等：已同步过的账号不会被重复复制；副本被删掉的也不会被「复活」。
 * 客户端正在运行时后端**直接拒绝**（要写会话库），返回的错误文案可直接展示。
 */
export function workbuddySessionLinkAll(): Promise<WbSessionLinkAllReport> {
  return call("workbuddy_session_link_all", {});
}

/**
 * 只登记「同一段对话已有副本」的关联，**不复制任何文件**（0 字节）。
 *
 * 客户端在跑也能用（不碰会话库）：适合「只要关联关系、不想让磁盘翻倍」。
 */
export function workbuddySessionLinkExisting(): Promise<WbSessionLinkAllReport> {
  return call("workbuddy_session_link_existing", {});
}

/**
 * 一键互通的只读预览：与真跑**同一份计划函数**，所以两个数字必然一致。
 *
 * 只读，不写库；客户端是否在运行也一并带回来（那是唯一会被后端拒掉的情况）。
 */
/** 关联会话：按「同一段对话」聚合各账号副本（只读）。 */
export function workbuddySessionGroups(): Promise<WbSessionGroupsReport> {
  return call("workbuddy_session_groups");
}

/** 关联会话：单个组的成员明细（与列表同源）。 */
export function workbuddySessionGroupDetail(groupKey: string): Promise<WbSessionGroupsReport & WbSessionGroup> {
  return call("workbuddy_session_group_detail", { groupKey });
}

/** 关联会话：还没纳入关联组的会话（供「新增关联会话」选择）。 */
export function workbuddySessionGroupCandidates(): Promise<WbGroupCandidatesReport> {
  return call("workbuddy_session_group_candidates");
}

/** 关联会话：以某个副本为准，覆盖组内其它账号的副本（要求客户端已关闭）。 */
export function workbuddySessionGroupUnify(
  groupKey: string,
  sourceSessionId: string,
  /** 客户端在跑时由后端主动退出它再写（用户在确认框里点过才传 true）。 */
  closeClient = false,
): Promise<WbGroupUnifyReport> {
  return call("workbuddy_session_group_unify", {
    groupKey,
    sourceSessionId,
    closeClient,
  });
}

/** 关联会话：给组里缺的账号补一份副本。 */
export function workbuddySessionGroupLinkAccount(
  groupKey: string,
  targetUid: string,
  closeClient = false,
): Promise<WbSessionLinkAllReport> {
  return call("workbuddy_session_group_link_account", {
    groupKey,
    targetUid,
    closeClient,
  });
}

/** 关联会话：把一个未关联的会话复制给目标账号（复制后自动进同一个组）。 */
export function workbuddySessionGroupLinkSession(
  sessionId: string,
  targetUid: string,
  closeClient = false,
): Promise<WbSessionLinkAllReport> {
  return call("workbuddy_session_group_link_session", {
    sessionId,
    targetUid,
    closeClient,
  });
}

export function workbuddySessionLinkAllPreview(): Promise<WbSessionLinkAllReport> {
  return call("workbuddy_session_link_all_preview", {});
}

export function workbuddySessionUnlink(groupId: string): Promise<{ ok: boolean; remaining: number }> {
  return call("workbuddy_session_unlink", { groupId });
}

export function workbuddySessionEdgeSyncDbs(): Promise<{
  items: Array<{ name: string; path: string; size: number }>;
  note: string;
}> {
  return call("workbuddy_session_edge_sync_dbs");
}

// ---------------------------------------------------------------------------
// T46 · S4 WorkBuddy 会话编辑器
// ---------------------------------------------------------------------------

/**
 * 会话编辑器（WorkBuddy）：读出元数据 + **回合索引**（只读、轻量）。
 *
 * ⚠️ 正文**不在这里**回：真机单个正文 19 MB、最大的 136 MB，整份过 IPC 会把界面冻死。
 * 每个回合的完整行走 `workbuddySessionEditLines`，只取当前看的那一个回合。
 */
export function workbuddySessionEditDoc(uid: string, cid: string): Promise<WbEditDoc> {
  return call("workbuddy_session_edit_doc", { uid, cid });
}

/** 会话编辑器（WorkBuddy）：按行区间 `[from, to)` 取完整行（后端上限一次 400 行）。 */
export function workbuddySessionEditLines(
  uid: string,
  cid: string,
  from: number,
  to: number,
): Promise<WbEditLinesResult> {
  return call("workbuddy_session_edit_lines", { uid, cid, from, to });
}

/**
 * 会话编辑器（WorkBuddy）：可写入的目标账号清单（只读）。
 *
 * 数据源与「WorkBuddy 会话记录」页一致（`sessions.user_id` 去重），显示名统一走
 * `workbuddy_accounts::label_for` —— 前端**别自己截 uid 尾号**。
 */
export function workbuddySessionEditTargets(): Promise<WbEditTargets> {
  return call("workbuddy_session_edit_targets");
}

/**
 * 会话编辑器（WorkBuddy）：分级校验（**不读文件、不开库**，毫秒级）。
 *
 * ⚠️ 规则只有后端这一份实现；前端只显示 `errors` / `warnings`。
 * `summary` 传 `edit_doc` 带回来的 `{lineCount, counts, lineKinds}`，后端靠它算
 * 「改完之后还剩几条用户提问」（空会话硬拦）。`lineKinds` 是**行号 → 种类**的数组，
 * 用于判断被删掉的行里有没有用户提问。
 *
 * 这只是快速反馈：写入前 `workbuddySessionEditSave` 还会跑一次全量校验。
 */
export function workbuddySessionEditValidate(
  ops: WbEditOp[],
  rowPatch: Record<string, unknown>,
  columns: WbEditColumn[],
  summary: Record<string, unknown>,
  sessionId: string,
): Promise<WbEditReport> {
  return call("workbuddy_session_edit_validate", {
    ops,
    rowPatch,
    columns,
    summary,
    sessionId,
  });
}

/**
 * 会话编辑器（WorkBuddy）：写回。
 *
 * ⚠️ 写盘 + 开库 + 可能结束/重启客户端，几十秒 ⇒ 界面要给足进度反馈。
 *
 * `mode`：`overwrite` 就地改这条会话；`insert` 新 UUID 造一条新会话（源会话一行不动）。
 * `targetUid`：只在 `insert` 时可用（同 Trae 侧的决定：覆写顺带换账号＝把会话悄悄搬走）。
 * `ops`：**只含改动过的那几行**，未改动的行逐字节原样保留。
 *
 * ⚠️ 后端把「写入前的全量校验」错误以 `ok=false` + `errors` **正常返回**（不是抛错），
 * 所以这里能拿到明细并展示。
 */
export function workbuddySessionEditSave(
  uid: string,
  cid: string,
  mode: "overwrite" | "insert",
  ops: WbEditOp[],
  rowPatch: Record<string, unknown>,
  targetUid?: string,
): Promise<WbEditSaveResult> {
  return call("workbuddy_session_edit_save", { uid, cid, mode, ops, rowPatch, targetUid });
}

// ---------------------------------------------------------------------------
// 首页总览 + WorkBuddy 积分（国内版）
// ---------------------------------------------------------------------------

/** 本机总览快照（只读、不联网、不触发解密）；算完会落盘到 `~/.twin-switch/cache/overview.json`。 */
export function appOverviewSnapshot(): Promise<Overview> {
  return call("app_overview_snapshot");
}

/** 只读上次落盘的总览缓存（毫秒级、不重算）；没有缓存时 `empty=true`。 */
export function appOverviewCached(): Promise<OverviewCache> {
  return call("app_overview_cached");
}

/** 把前端并行扫出的可回收空间写回总览缓存，下次首屏连这块也是热的。 */
export function appOverviewSaveReclaim(
  traeBytes: number,
  wbBytes: number,
): Promise<OverviewReclaimCache> {
  return call("app_overview_save_reclaim", { traeBytes, wbBytes });
}

/** 可查询积分的账号清单（只读）。 */
export function workbuddyCreditsAccounts(): Promise<WbCreditsAccounts> {
  return call("workbuddy_credits_accounts");
}

/** 只读积分缓存（不发请求）。 */
export function workbuddyCreditsCached(): Promise<WbCreditsResult> {
  return call("workbuddy_credits_cached");
}

/** 查询全部账号积分；force=false 时命中 5 分钟缓存。 */
export function workbuddyCreditsQuery(force?: boolean): Promise<WbCreditsResult> {
  return call("workbuddy_credits_query", { force });
}

/** 查询单个账号积分（不走缓存）。 */
export function workbuddyCreditsOne(accountId: string): Promise<WbCreditItem> {
  return call("workbuddy_credits_one", { accountId });
}

// ---------------------------------------------------------------------------
// 自动更新
// ---------------------------------------------------------------------------

/**
 * 当前更新状态快照。
 *
 * 与 `update-state` 事件配对使用：挂载时先拉一次（事件只在订阅**之后**发生的事
 * 才会到达，检查可能在挂载前就完成了），之后一律靠事件推送。
 */
export function updateState(): Promise<UpdateSnapshot> {
  return call("update_state");
}

/** 检查更新；`force=true`（默认）绕过 Rust 侧 6 小时缓存。 */
export function updateCheck(force = true): Promise<UpdateCheckResult> {
  return call("update_check", { force });
}

/** 开始下载更新包：立即返回，进度走 `update-state` 事件。 */
export function updateDownload(): Promise<void> {
  return call("update_download");
}

/** 安装已下载的更新包并重启应用（无包时会返回可读错误）。 */
export function updateRestart(): Promise<void> {
  return call("update_restart");
}


// ---------------------------------------------------------------------------
// 积分共享（本地额度网关 127.0.0.1:57892）
// ---------------------------------------------------------------------------

export async function gatewayStatus(): Promise<unknown> {
  return invoke("gateway_status");
}

export async function gatewayPool(): Promise<unknown> {
  return invoke("gateway_pool");
}

export async function gatewayPreview(model: string): Promise<unknown> {
  return invoke("gateway_preview", { model });
}

export async function gatewayKeysList(): Promise<unknown> {
  return invoke("gateway_keys_list");
}

/// 新建 Key：**明文只返回这一次**。
export async function gatewayKeyCreate(
  name: string,
  scopes: string[] = [],
): Promise<{ plaintext: string; key: unknown }> {
  return invoke("gateway_key_create", { name, scopes });
}

export async function gatewayKeyRevoke(id: string): Promise<unknown> {
  return invoke("gateway_key_revoke", { id });
}

export async function gatewayKeyRestore(id: string): Promise<unknown> {
  return invoke("gateway_key_restore", { id });
}

export async function gatewayKeyDelete(id: string): Promise<void> {
  return invoke("gateway_key_delete", { id });
}

export async function gatewayModelsGet(): Promise<unknown> {
  return invoke("gateway_models_get");
}

export async function gatewayModelsSet(value: unknown): Promise<void> {
  return invoke("gateway_models_set", { value });
}

export async function gatewayUpstreamsGet(): Promise<unknown> {
  return invoke("gateway_upstreams_get");
}

export async function gatewayUpstreamsSet(value: unknown): Promise<void> {
  return invoke("gateway_upstreams_set", { value });
}


/// 自动识别本机客户端里的模型（附带每家来源说明）。
export async function gatewayModelsDiscover(): Promise<{
  models: { scope: string; product: string; model: string; display: string; source: string }[];
  reports: { product: string; scope: string; found: number; note: string }[];
}> {
  return invoke("gateway_models_discover");
}

/// 已勾选的模型。
export async function gatewayModelsSelected(): Promise<Record<string, string[]>> {
  return invoke("gateway_models_selected");
}

/// 保存勾选结果。
export async function gatewayModelsSave(map: Record<string, string[]>): Promise<void> {
  return invoke("gateway_models_save", { map });
}


// ---------------------------------------------------------------------------
// Qoder 账号（国内 / 国际两套账号库）
// ---------------------------------------------------------------------------

export async function qoderRegions(): Promise<unknown> {
  return invoke("qoder_regions");
}

export async function qoderAccountsList(region?: string): Promise<unknown> {
  return invoke("qoder_accounts_list", { region: region ?? null });
}

export async function qoderAccountAdd(
  region: string,
  uid: string,
  name: string,
  token: string,
  email?: string | null,
): Promise<unknown> {
  return invoke("qoder_account_add", { region, uid, name, token, email: email ?? null });
}

export async function qoderAccountRemove(region: string, id: string): Promise<unknown> {
  return invoke("qoder_account_remove", { region, id });
}

export async function qoderAccountRestore(region: string, id: string): Promise<unknown> {
  return invoke("qoder_account_restore", { region, id });
}

export async function qoderAccountSetToken(
  region: string,
  id: string,
  token: string,
): Promise<unknown> {
  return invoke("qoder_account_set_token", { region, id, token });
}

export async function qoderAccountRefresh(region: string, id: string): Promise<unknown> {
  return invoke("qoder_account_refresh", { region, id });
}

/// 批量刷新一个区域的额度（串行查，单个失败不影响其它账号）。
export async function qoderAccountsRefreshAll(region: string): Promise<unknown> {
  return invoke("qoder_accounts_refresh_all", { region });
}

/** 近 N 天积分消耗（本机快照算出来的，不是官方用量）。 */
export function qoderCreditHistory(days = 7): Promise<{
  ok: boolean;
  windowDays: number;
  days: { day: string; used: number }[];
  accounts: { uid: string; name: string; region: string; used: number }[];
  totalUsed: number;
  resetEvents: number;
  points: number;
  enoughData: boolean;
  file: string;
}> {
  return invoke("qoder_credit_history", { days });
}


// Qoder 浏览器登录（设备授权 + PKCE）
export async function qoderLoginStart(region: string): Promise<{
  loginId: string;
  verificationUri: string;
  expiresInSecs: number;
}> {
  return invoke("qoder_login_start", { region });
}

export async function qoderLoginPoll(
  region: string,
  loginId: string,
): Promise<{ done: boolean; result?: unknown; error?: string }> {
  return invoke("qoder_login_poll", { region, loginId });
}

export async function qoderLoginCancel(loginId: string): Promise<boolean> {
  return invoke("qoder_login_cancel", { loginId });
}


// Qoder 账号包导出 / 导入 / 签到
export async function qoderAccountsExport(region: string): Promise<{
  count: number;
  filename: string;
  path: string;
  json: string;
}> {
  return invoke("qoder_accounts_export", { region });
}

export async function qoderImportPreview(
  region: string,
  jsonText: string,
): Promise<{
  region: string;
  total: number;
  items: { index: number; uid: string; name: string; hasToken: boolean }[];
  rejected: { index: number; uid: string; reason: string }[];
}> {
  return invoke("qoder_import_preview", { region, json: jsonText });
}

export async function qoderImportApply(
  region: string,
  jsonText: string,
  indexes?: number[] | null,
): Promise<{ imported: number; updated: number; skipped: number; rejected: unknown[] }> {
  return invoke("qoder_import_apply", { region, json: jsonText, indexes: indexes ?? null });
}

export async function qoderCheckinAll(region: string): Promise<{
  claimed: number;
  already: number;
  blocked: number;
  blockedReason?: string;
  failed: number;
  messages: string[];
  failures: string[];
}> {
  return invoke("qoder_checkin_all", { region });
}


// Qoder token 刷新 / 改名 / 设为默认
export async function qoderTokenRefresh(region: string, id: string): Promise<unknown> {
  return invoke("qoder_token_refresh", { region, id });
}

export async function qoderAccountRename(
  region: string,
  id: string,
  name: string,
): Promise<unknown> {
  return invoke("qoder_account_rename", { region, id, name });
}

export async function qoderAccountSwitch(
  region: string,
  id: string,
): Promise<{ ok: boolean; defaultUid?: string; note: string; launched?: string | null }> {
  return invoke("qoder_account_switch", { region, id });
}


/// 从本机 Qoder 客户端导入登录态 —— 自动识别国内版 / 国际版，各写各的库。
export async function qoderImportLocal(): Promise<
  Record<
    "cn" | "global",
    {
      imported: number;
      updated: number;
      accounts: { uid: string; name: string; source: string }[];
      failed: { uid: string; error: string }[];
      reason?: string;
    }
  >
> {
  return invoke("qoder_import_local");
}


// Qoder 会话记录（只读本地 SQLite）
export async function qoderSessionsList(
  region: string,
  includeArchived = false,
): Promise<
  {
    id: string;
    title: string;
    cwd: string;
    model: string;
    createdAt: number;
    updatedAt: number;
    archived: boolean;
    messageCount: number;
    bytes: number;
    source: string;
    importedFrom: string | null;
  }[]
> {
  return invoke("qoder_sessions_list", { region, includeArchived });
}

export async function qoderSessionDetail(
  region: string,
  sessionId: string,
): Promise<{
  id: string;
  region: string;
  meta: { title: string; cwd: string; model: string; createdAt: number; updatedAt: number } | null;
  messages: {
    messageId: string;
    role: string;
    text: string;
    thinking: string | null;
    toolCount: number;
    createdAt: number;
    status: string;
  }[];
}> {
  return invoke("qoder_session_detail", { region, sessionId });
}

export async function qoderSessionsCounts(): Promise<Record<"cn" | "global", number>> {
  return invoke("qoder_sessions_counts");
}


// Qoder 会话写操作（客户端在跑时会拒绝；写前自动备份库）
export async function qoderSessionRename(
  region: string,
  sessionId: string,
  title: string,
): Promise<{ ok: boolean; backup: string; title: string }> {
  return invoke("qoder_session_rename", { region, sessionId, title });
}

export async function qoderSessionDelete(
  region: string,
  sessionId: string,
): Promise<{ ok: boolean; backup: string; soft: boolean }> {
  return invoke("qoder_session_delete", { region, sessionId });
}

/**
 * 批量真删会话。**单条也走这里** —— 只有一条写入路径，行为与批量完全一致
 * （后端一次关客户端、一次备份、一个事务、一次重启）。
 */
export async function qoderSessionDeleteMany(
  region: string,
  sessionIds: string[],
): Promise<{
  ok: boolean;
  backup: string;
  deletedSessions: number;
  deletedMessages: number;
  /** 选中的会话里，库里已经不存在的（列表过期或被客户端删了）。 */
  missing?: string[];
  requested?: number;
  hard?: boolean;
  verified?: boolean;
  clientRelaunch?: {
    ok: boolean;
    action?: "launched" | "already-running" | "skipped";
    exe?: string;
    error?: string;
    wasRunning?: boolean;
  };
}> {
  return invoke("qoder_session_delete_many", { region, sessionIds });
}

export async function qoderSessionDuplicate(
  region: string,
  sessionId: string,
): Promise<{ ok: boolean; id: string }> {
  return invoke("qoder_session_duplicate", { region, sessionId });
}


/// 存该账号的 Cosy 设备身份（抓包里 cosy-machinetoken / cosy-machinetype）。
export async function qoderAccountSetCosy(
  region: string,
  id: string,
  machineToken: string,
  machineType: string,
): Promise<unknown> {
  return invoke("qoder_account_set_cosy", { region, id, machineToken, machineType });
}


/** 积分共享总开关（关闭会真的停掉网关）。 */
export async function gatewaySetEnabled(enabled: boolean): Promise<{ enabled: boolean }> {
  return invoke("gateway_set_enabled", { enabled });
}


/** 改 Qoder 会话里一条消息的正文。 */
export async function qoderSessionUpdateMessage(
  region: string,
  sessionId: string,
  messageId: string,
  text: string,
): Promise<unknown> {
  return invoke("qoder_session_update_message", { region, sessionId, messageId, text });
}

/** 删除 Qoder 会话里的一条消息。 */
export async function qoderSessionDeleteMessage(
  region: string,
  sessionId: string,
  messageId: string,
): Promise<unknown> {
  return invoke("qoder_session_delete_message", { region, sessionId, messageId });
}


/** 首页 Qoder 概况。 */
export async function qoderHomeSummary(): Promise<{
  accounts: number;
  credits: number | null;
  regions: { region: string; label: string; accounts: number; usable: number; credits: number | null; hasClient: boolean }[];
  sessions: Record<string, number>;
  running: boolean;
  processCount: number;
}> {
  return invoke("qoder_home_summary");
}


/** Qoder 本机数据：扫可清理项。 */
export async function qoderCleanupScan(): Promise<{
  sides: {
    region: string;
    label: string;
    dataRoot: string | null;
    bytes: number;
    items: {
      key: string;
      label: string;
      path: string;
      bytes: number;
      defaultOn: boolean;
      note: string;
      /** 「清空回收站」项：勾选后是硬删，不可恢复。 */
      isTrash?: boolean;
    }[];
  }[];
}> {
  return invoke("qoder_cleanup_scan");
}

/** Qoder 本机数据清理（只删勾选项）。 */
export async function qoderCleanupRun(region: string, keys: string[]): Promise<unknown> {
  return invoke("qoder_cleanup_run", { region, keys });
}


/** 清空 Qoder 清理页的回收站（硬删，不可恢复）。 */
export async function qoderCleanupEmptyTrash(): Promise<{ ok: boolean; freed: number }> {
  return invoke("qoder_cleanup_empty_trash");
}


/** 手动添加 WorkBuddy 账号：直接粘 access_token（可选 refresh_token），不走授权流程。 */
export async function workbuddyAccountAddToken(args: {
  accessToken: string;
  refreshToken?: string;
  domain?: string;
  name?: string;
}): Promise<{
  ok: boolean;
  accountId: string | null;
  uid: string | null;
  nickname: string | null;
  domain: string | null;
  expiresAt: number | null;
  refreshExpiresAt: number | null;
}> {
  return invoke("workbuddy_account_add_token", args);
}


/** 使用说明页的真机自检（每步都是现场真实结果）。 */
export async function gatewayGuideSelftest(args: {
  key?: string;
  model?: string;
}): Promise<{
  ok: boolean;
  error?: string;
  steps: { id: string; label: string; ok: boolean; detail: string; model?: string; sample?: string[] }[];
  models: { count: number; sample: string[] };
  chat: { ok: boolean; ms?: number; status?: number; content?: string; error?: string } | null;
  checkedAt: number;
}> {
  return invoke("gateway_guide_selftest", args);
}
