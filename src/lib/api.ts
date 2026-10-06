import { invoke } from "@tauri-apps/api/core";
import type { ErrorLogKind, UpdateCheckResult, UpdateSnapshot } from "./types";
import type {
  TraeAccountOverview,
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
  TraeInstalledClient,
  TraeOAuthPending,
  TraeOAuthSessionStatus,
  TraeOAuthStartResult,
  TraeProfileRefreshResult,
  TraeScanResult,
  TraeSessionDetail,
  TraeSessionInfo,
  TraeSwitchResult,
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
  WbSessionCopyReport,
  WbLinksPreview,
  WbSessionDeleteReport,
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

/** 列出已安装的 Trae 客户端（含登录态与安装路径）。 */
export function traeListClients(): Promise<{ clients: TraeInstalledClient[] }> {
  return call("trae_list_clients");
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
): Promise<{ ok: boolean; id: string; payload: Record<string, unknown> }> {
  return call("trae_export_account", { clientKey, accountId });
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

/** 单会话详情（标题 / 轮数 / 完整对话，供记录页预览）。 */
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

/** 清空工具回收站，彻底释放被清理文件占用的空间。 */
export function traeWbCleanupEmptyTrash(): Promise<{
  removed: number;
  bytes: number;
  dir: string;
}> {
  return call("trae_wb_cleanup_empty_trash");
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

export function workbuddyRemoveAccount(accountId: string): Promise<{ ok: boolean }> {
  return call("workbuddy_remove_account", { accountId });
}

export function workbuddyRenameAccount(accountId: string, name: string): Promise<{ ok: boolean }> {
  return call("workbuddy_rename_account", { accountId, name });
}

export function workbuddyExportAccounts(): Promise<Record<string, unknown>> {
  return call("workbuddy_export_accounts");
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
