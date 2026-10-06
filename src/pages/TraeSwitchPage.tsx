import { useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  ArchiveRestore,
  CheckCircle2,
  ExternalLink,
  FileDown,
  FileUp,
  Globe,
  Loader2,
  LogIn,
  PencilLine,
  Power,
  RotateCcw,
  Save,
  Trash2,
  UserPlus,
  X,
} from "lucide-react";

import { AccountCard, Chip, IconAction } from "@/components/account-card";
import {
  TraeCreditBlock,
  TraeCreditsDialog,
  TraeCreditsHeader,
  useTraeCredits,
} from "@/components/trae-credits";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import type {
  TraeAccountOverview,
  TraeBrowserInfo,
  TraeCreditEntry,
  TraeInstalledClient,
  TraeOAuthSessionStatus,
  TraeOAuthStartResult,
  TraeSwitchResult,
  TraeVaultEntry,
} from "@/lib/trae-types";

function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function outcomeBadge(outcome: TraeSwitchResult["outcome"]): { label: string; tone: string } {
  switch (outcome) {
    case "active":
      return { label: "切换成功", tone: "bg-emerald-500/15 text-emerald-600" };
    case "rolled_back":
      return { label: "已回滚", tone: "bg-amber-500/15 text-amber-600" };
    default:
      return { label: "需人工确认", tone: "bg-orange-500/15 text-orange-600" };
  }
}

function oauthStateLabel(state: string): string {
  switch (state) {
    case "waiting":
      return "等待浏览器完成授权…";
    case "callback_received":
      return "已收到授权回调，正在完成登录…";
    case "processing":
      return "正在完成登录…";
    case "done":
      return "登录完成";
    case "error":
      return "登录失败";
    case "stopped":
      return "已停止监听";
    default:
      return "等待浏览器完成授权…";
  }
}

/** uid 只显示首尾，中间省略（完整值挂在 title 上）。 */
function shortUid(uid: string | null | undefined): string {
  if (!uid) return "—";
  if (uid.length <= 18) return uid;
  return `${uid.slice(0, 8)}…${uid.slice(-6)}`;
}

/** 令牌到期的人话描述；只在 7 天内到期时提示，减少噪声。 */
function tokenExpiryLabel(ms: number | null | undefined): string | null {
  if (!ms) return null;
  const days = Math.floor((ms - Date.now()) / 86400_000);
  if (days < 0) return "令牌已过期";
  if (days <= 7) return `${days} 天后过期`;
  return null;
}

export default function TraeSwitchPage() {
  const [clients, setClients] = useState<TraeInstalledClient[]>([]);
  const [clientKey, setClientKey] = useState<string | null>(null);
  const [overview, setOverview] = useState<TraeAccountOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<string[]>([]);

  // 网页（OAuth）登录
  const [loginOpen, setLoginOpen] = useState(false);
  const [loginName, setLoginName] = useState("");
  const [oauthStart, setOauthStart] = useState<TraeOAuthStartResult | null>(null);
  const [oauthStatus, setOauthStatus] = useState<TraeOAuthSessionStatus | null>(null);
  const [oauthBrowsers, setOauthBrowsers] = useState<TraeBrowserInfo[]>([]);
  const [oauthBrowserKey, setOauthBrowserKey] = useState("");
  const [manualOpen, setManualOpen] = useState(false);
  const [manualUrl, setManualUrl] = useState("");

  // 备份 / 重命名 / 导入 / 交接记忆 对话框
  const [backupOpen, setBackupOpen] = useState(false);
  const [backupName, setBackupName] = useState("");
  const [renameTarget, setRenameTarget] = useState<TraeVaultEntry | null>(null);
  const [renameName, setRenameName] = useState("");
  const [importOpen, setImportOpen] = useState(false);
  const [importText, setImportText] = useState("");
  const [importName, setImportName] = useState("");
  const [handoffOpen, setHandoffOpen] = useState(false);

  /** 底部「回滚」卡里选中的账号 id（回滚 = 切回它）。 */
  const [rollbackTarget, setRollbackTarget] = useState<string | null>(null);

  // 交接记忆表单
  const [hoProject, setHoProject] = useState("");
  const [hoSessions, setHoSessions] = useState("");
  const [hoSteps, setHoSteps] = useState("");
  const [hoKeyFiles, setHoKeyFiles] = useState("");
  const [hoNote, setHoNote] = useState("");
  const [hoResult, setHoResult] = useState<{ files: string[]; skipped: string[] } | null>(null);

  const progressRef = useRef<HTMLPreElement>(null);
  useEffect(() => {
    if (progressRef.current) {
      progressRef.current.scrollTop = progressRef.current.scrollHeight;
    }
  }, [progress]);

  async function loadClients() {
    try {
      const { clients } = await api.traeListClients();
      setClients(clients);
      const installed = clients.filter((c) => c.installed);
      const preferred = installed.find((c) => c.key === "trae-cn") ?? installed[0];
      setClientKey((prev) => prev ?? preferred?.key ?? null);
    } catch (cause) {
      setError(api.asError(cause));
    }
  }

  useEffect(() => {
    void loadClients();
  }, []);

  async function loadOverview(key: string) {
    setLoading(true);
    setError(null);
    try {
      const data = await api.traeAccountOverview(key);
      setOverview(data);
    } catch (cause) {
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (clientKey) void loadOverview(clientKey);
  }, [clientKey]);

  // 网页登录：发起时拉取本机浏览器列表（私密窗口用）
  useEffect(() => {
    if (!loginOpen) return;
    api
      .traeOauthBrowsers()
      .then(({ browsers }) => {
        setOauthBrowsers(browsers);
        setOauthBrowserKey((prev) => prev || browsers[0]?.key || "");
      })
      .catch(() => setOauthBrowsers([]));
  }, [loginOpen]);

  // 网页登录：1.5s 轮询状态；收到回调时后端自动完成 token 交换并落库
  useEffect(() => {
    if (!loginOpen || !clientKey) return;
    let cancelled = false;
    const t = setInterval(async () => {
      try {
        const s = await api.traeOauthStatus();
        if (cancelled) return;
        setOauthStatus(s);
        if (s.state === "done" || s.state === "error" || s.state === "stopped") {
          clearInterval(t);
          if (s.state === "done") {
            toast.success(s.message);
            void loadOverview(clientKey);
          }
        }
      } catch {
        // 单次轮询失败忽略，下一轮继续
      }
    }, 1500);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  }, [loginOpen, clientKey]);

  const oauthLoginUrl = oauthStart?.loginUrl ?? oauthStatus?.loginUrl ?? "";

  async function onOauthStart() {
    if (!clientKey || busy) return;
    setBusy(true);
    try {
      const res = await api.traeOauthStart(clientKey, loginName.trim() || undefined);
      setOauthStart(res);
      setOauthStatus(null);
      setLoginOpen(true);
      await api.traeOauthOpenUrl(res.loginUrl);
    } catch (cause) {
      toast.error("发起登录失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  async function onImportLocal() {
    if (!clientKey || busy) return;
    setBusy(true);
    try {
      const r = await api.traeImportLocalLogin(clientKey);
      if (r.ok) {
        toast.success(
          `已导入本地登录态${r.duplicate ? "（账号已存在，已更新）" : ""}`,
          { description: `${r.displayName || r.uid || r.id} · uid ${r.uid ?? "-"}` },
        );
        void loadOverview(clientKey);
      } else {
        toast.error("导入本地登录态失败", { description: r.error });
      }
    } catch (cause) {
      toast.error("导入本地登录态失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  async function onImportAll() {
    if (!clientKey || busy) return;
    setBusy(true);
    try {
      const { results } = await api.traeImportAllLocalLogins();
      const okCount = results.filter((r) => r.ok).length;
      const failCount = results.length - okCount;
      const imported = results.filter((r) => r.ok);
      const msg = `已导入 ${okCount} 个客户端的本地登录态${failCount ? `，${failCount} 个未登录或失败` : ""}`;
      toast.success(msg, {
        description:
          imported.length > 0
            ? imported.map((r) => `${r.client}：${r.displayName || r.uid || r.id}`).join("；")
            : "未发现已登录的客户端登录态",
      });
      void loadOverview(clientKey);
    } catch (cause) {
      toast.error("一键导入失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  async function onOauthOpenUrl(privateMode: boolean) {
    if (!oauthLoginUrl) return;
    try {
      await api.traeOauthOpenUrl(oauthLoginUrl, privateMode, privateMode ? oauthBrowserKey || undefined : undefined);
    } catch (cause) {
      toast.error("打开浏览器失败", { description: api.asError(cause) });
    }
  }

  async function onOauthStop() {
    try {
      await api.traeOauthStop();
      setOauthStatus((prev) =>
        prev
          ? { ...prev, state: "stopped", message: "已停止监听" }
          : { state: "stopped", message: "已停止监听" },
      );
    } catch (cause) {
      toast.error("停止监听失败", { description: api.asError(cause) });
    }
  }

  async function onOauthManual() {
    if (!clientKey || !manualUrl.trim()) return;
    setBusy(true);
    try {
      const s = await api.traeOauthManual(clientKey, manualUrl.trim(), loginName.trim() || undefined);
      setOauthStatus(s);
      if (s.state === "done") {
        toast.success(s.message);
        void loadOverview(clientKey);
      } else if (s.state === "error") {
        toast.error(s.message);
      }
    } catch (cause) {
      toast.error("手动完成登录失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  function onOauthClose() {
    setLoginOpen(false);
    setOauthStart(null);
    setOauthStatus(null);
    setManualOpen(false);
    setManualUrl("");
    void api.traeOauthStop();
  }

  function pushProgress(lines: string[]) {
    setProgress((prev) => [...prev, ...lines]);
  }

  async function onSwitch(entry: TraeVaultEntry) {
    if (!clientKey || busy) return;
    setBusy(true);
    setProgress([]);
    try {
      const res = await api.traeSwitchTo(clientKey, entry.id);
      pushProgress(res.progress ?? []);
      const badge = outcomeBadge(res.outcome);
      toast.success(`${badge.label}：${entry.id}`, {
        description: res.message || undefined,
      });
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("切换失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  /**
   * 回滚到指定账号。后端语义 `trae_switch::rollback_to` ≡ `switch_to`，
   * 即「结束客户端 → 写回该账号的登录态 → 重启」，用于切换异常后恢复。
   * 入口在页面底部的独立「回滚」卡里（与 WorkBuddy 账号页同款位置与外观）。
   */
  async function onRollback(accountId: string) {
    if (!clientKey || busy) return;
    setBusy(true);
    setProgress([]);
    try {
      const res = await api.traeRollbackTo(clientKey, accountId);
      pushProgress(res.progress ?? []);
      const badge = outcomeBadge(res.outcome);
      toast.success(`${badge.label}：${accountId}`, {
        description: res.message || undefined,
      });
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("回滚失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  async function onBackup() {
    if (!clientKey || busy || !backupName.trim()) return;
    setBusy(true);
    try {
      const res = await api.traeBackupAccount(clientKey, backupName.trim());
      setBackupOpen(false);
      setBackupName("");
      toast.success("已备份当前登录态", {
        description: res.verifiedUid ? `识别到账号 uid：${res.verifiedUid}` : undefined,
      });
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("备份失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  async function onRename() {
    if (!clientKey || !renameTarget || !renameName.trim()) return;
    try {
      const res = await api.traeRenameAccount(clientKey, renameTarget.id, renameName.trim());
      setRenameTarget(null);
      toast.success("已重命名", { description: `新名称：${res.id}` });
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("重命名失败", { description: api.asError(cause) });
    }
  }

  async function onExportAccount(accountId: string) {
    try {
      const res = await api.traeExportAccount(clientKey!, accountId);
      const text = JSON.stringify(res.payload ?? {}, null, 2);
      await navigator.clipboard.writeText(text);
      toast.success("已导出账号备份到剪贴板", {
        description: `粘贴到文本文件保存即可（含 base64 载体，可再导入）`,
      });
    } catch (cause) {
      toast.error("导出失败", { description: api.asError(cause) });
    }
  }

  async function onRemove(entry: TraeVaultEntry) {
    if (!clientKey) return;
    if (!window.confirm(`确定从账号库移除「${entry.id}」？\n只删本地档案，不影响客户端当前登录态。`)) return;
    try {
      await api.traeRemoveAccount(clientKey, entry.id);
      toast.success("已从账号库移除", { description: entry.id });
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("移除失败", { description: api.asError(cause) });
    }
  }

  async function onImport() {
    if (!clientKey || !importText.trim()) return;
    try {
      const payload = JSON.parse(importText.trim());
      const res = await api.traeImportAccount(clientKey, payload, importName.trim() || undefined);
      setImportOpen(false);
      setImportText("");
      setImportName("");
      toast.success("导入成功", { description: `账号：${res.id}，文件 ${res.files} 个` });
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("导入失败", { description: api.asError(cause) });
    }
  }

  async function onHandoffPreview() {
    if (!clientKey) return;
    try {
      const res = await api.traeHandoffPreview({
        clientKey,
        projectPath: hoProject.trim() || undefined,
        sessionIds: hoSessions
          .split(/[\s,，]+/)
          .map((s) => s.trim())
          .filter(Boolean),
        nextSteps: hoSteps
          .split("\n")
          .map((s) => s.trim())
          .filter(Boolean),
        keyFiles: hoKeyFiles
          .split(/[\s,，]+/)
          .map((s) => s.trim())
          .filter(Boolean),
        note: hoNote.trim() || undefined,
      });
      setHoResult({ files: res.files ?? [], skipped: res.skipped ?? [] });
    } catch (cause) {
      toast.error("交接记忆预览失败", { description: api.asError(cause) });
    }
  }

  async function onHandoffWrite() {
    if (!clientKey || busy) return;
    setBusy(true);
    try {
      const res = await api.traeHandoffWrite({
        clientKey,
        projectPath: hoProject.trim() || undefined,
        sessionIds: hoSessions
          .split(/[\s,，]+/)
          .map((s) => s.trim())
          .filter(Boolean),
        nextSteps: hoSteps
          .split("\n")
          .map((s) => s.trim())
          .filter(Boolean),
        keyFiles: hoKeyFiles
          .split(/[\s,，]+/)
          .map((s) => s.trim())
          .filter(Boolean),
        note: hoNote.trim() || undefined,
      });
      setHoResult({ files: res.files ?? [], skipped: res.skipped ?? [] });
      toast.success("交接记忆已写入", {
        description: `落盘 ${res.files?.length ?? 0} 个文件${res.skipped?.length ? `，跳过 ${res.skipped.length} 项` : ""}`,
      });
    } catch (cause) {
      toast.error("交接记忆写入失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  const selected = clients.find((c) => c.key === clientKey);
  const live = overview?.live;

  // ---- 积分：与账号卡片融合显示（展示层与 WorkBuddy 侧同一套）----
  const credits = useTraeCredits(clientKey);

  /** 账号 id → 积分条目（后端就是按账号目录扫描的，id 与账号库一致）。 */
  const creditById = useMemo(() => {
    const m = new Map<string, TraeCreditEntry>();
    for (const a of credits.result?.accounts ?? []) m.set(a.id, a);
    return m;
  }, [credits.result]);

  const creditsLoading = credits.result === null && credits.error === null;

  /** 当前登录的 uid 是否已经在账号库里（不在就提示先备份）。 */
  const liveInVault = useMemo(() => {
    if (!overview?.loggedIn || !live?.uid) return true;
    return overview.vault.some(
      (v) => v.meta?.verified_uid === live.uid || v.oauth?.uid === live.uid,
    );
  }, [overview, live]);

  /** 底部「回滚」卡的下拉候选：整个账号库（名字优先取积分条目里的真实昵称）。 */
  const rollbackOptions = useMemo(() => {
    return (overview?.vault ?? []).map((v) => ({
      id: v.id,
      label: creditById.get(v.id)?.name || v.displayName || v.id,
    }));
  }, [overview, creditById]);

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">Trae 账号管理</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          冷切换：终止客户端进程 → 还原账号载体 → 重启 → 守护判定。切换会连带写入交接记忆，便于跨账号续接任务。
        </p>
      </div>

      {/* 客户端选择 */}
      {clients.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          {clients.map((c) => {
            const active = c.key === clientKey;
            return (
              <button
                key={c.key}
                type="button"
                disabled={!c.installed}
                onClick={() => setClientKey(c.key)}
                className={cn(
                  "flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors",
                  active
                    ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                    : "border-border bg-background hover:bg-foreground/[0.03]",
                  !c.installed && "cursor-not-allowed opacity-40",
                )}
              >
                {c.label}
                {c.installed && c.has_login ? (
                  <span className="size-2 rounded-full bg-emerald-500" />
                ) : null}
              </button>
            );
          })}
          <Badge variant="secondary" className="ml-auto">
            {overview?.running ? "客户端运行中" : "客户端未运行"}
          </Badge>
          <Button size="sm" variant="outline" onClick={() => setImportOpen(true)}>
            <FileUp className="size-4" />
            导入备份
          </Button>
          <Button size="sm" onClick={() => setBackupOpen(true)} disabled={!overview?.loggedIn || busy}>
            <UserPlus className="size-4" />
            备份当前登录态
          </Button>
        </div>
      )}

      {/* 网页（OAuth）登录 */}
      {selected ? (
        <Card>
          <CardHeader className="flex-row items-center justify-between pb-3">
            <div>
              <CardTitle className="flex items-center gap-2 text-base">
                <Globe className="size-4" />
                登录 Trae 账号
              </CardTitle>
              <CardDescription>
                网页授权登录：立即拿账号凭证，可查额度 / 签到 / 用量；不写客户端登录态，不产生可切换载体
              </CardDescription>
            </div>
            {loginOpen ? (
              <Button size="sm" variant="ghost" onClick={onOauthClose} title="关闭并停止监听">
                <X className="size-4" />关闭
              </Button>
            ) : null}
          </CardHeader>
          <CardContent>
            {!loginOpen ? (
              <div className="flex flex-wrap items-center gap-3">
                <Input
                  value={loginName}
                  onChange={(e) => setLoginName(e.target.value)}
                  placeholder="账号备注（可选）"
                  className="max-w-56"
                />
                <Button onClick={() => void onOauthStart()} disabled={busy}>
                  {busy ? <Loader2 className="size-4 animate-spin" /> : <LogIn className="size-4" />}
                  发起网页登录
                </Button>
                <Button variant="outline" onClick={() => void onImportLocal()} disabled={busy}>
                  <UserPlus className="size-4" />
                  导入本地登录态
                </Button>
                <Button variant="ghost" size="sm" onClick={() => void onImportAll()} disabled={busy}>
                  导入全部客户端
                </Button>
                <span className="text-xs text-muted-foreground">
                  「导入本地登录态」直接解密本机客户端已登录的账号，无需浏览器授权；网页登录用私密窗口可换号
                </span>
              </div>
            ) : (
              <div className="space-y-3">
                <div className="flex items-center gap-2 text-sm">
                  {["waiting", "callback_received", "processing"].includes(oauthStatus?.state ?? "") ? (
                    <Loader2 className="size-4 animate-spin text-muted-foreground" />
                  ) : oauthStatus?.state === "done" ? (
                    <CheckCircle2 className="size-4 text-emerald-500" />
                  ) : oauthStatus?.state === "error" || oauthStatus?.state === "stopped" ? (
                    <AlertTriangle className="size-4 text-amber-500" />
                  ) : null}
                  <span className="text-muted-foreground">
                    {oauthStatus?.message || oauthStateLabel(oauthStatus?.state ?? "waiting")}
                  </span>
                </div>

                {oauthLoginUrl ? (
                  <div className="flex flex-wrap items-center gap-2">
                    <Input readOnly value={oauthLoginUrl} className="min-w-0 flex-1 font-mono text-xs" />
                    <Button size="sm" variant="outline" onClick={() => void onOauthOpenUrl(false)}>
                      <ExternalLink className="size-4" />打开
                    </Button>
                    {oauthBrowsers.length > 0 ? (
                      <>
                        <select
                          value={oauthBrowserKey}
                          onChange={(e) => setOauthBrowserKey(e.target.value)}
                          className="h-8 rounded-md border border-input bg-background px-2 text-xs outline-none"
                        >
                          {oauthBrowsers.map((b) => (
                            <option key={b.key} value={b.key}>
                              {b.label}
                            </option>
                          ))}
                        </select>
                        <Button size="sm" variant="outline" onClick={() => void onOauthOpenUrl(true)}>
                          私密窗口
                        </Button>
                      </>
                    ) : null}
                  </div>
                ) : null}

                {["waiting", "callback_received", "processing"].includes(oauthStatus?.state ?? "") ? (
                  <div className="flex flex-wrap items-center gap-2">
                    <Button size="sm" variant="ghost" onClick={() => void onOauthStop()}>
                      停止监听
                    </Button>
                    <Button size="sm" variant="ghost" onClick={() => setManualOpen((v) => !v)}>
                      授权页没回跳？手动粘贴回调 URL
                    </Button>
                  </div>
                ) : null}

                {manualOpen ? (
                  <div className="flex flex-wrap items-center gap-2">
                    <Input
                      value={manualUrl}
                      onChange={(e) => setManualUrl(e.target.value)}
                      placeholder="http://127.0.0.1:17388/authorize?authCodeInfo=…"
                      className="min-w-0 flex-1 font-mono text-xs"
                    />
                    <Button size="sm" onClick={() => void onOauthManual()} disabled={!manualUrl.trim() || busy}>
                      {busy ? <Loader2 className="size-4 animate-spin" /> : null}完成登录
                    </Button>
                  </div>
                ) : null}

                {oauthStatus?.note ? (
                  <Alert>
                    <AlertTriangle className="size-4" />
                    <AlertTitle>注意</AlertTitle>
                    <AlertDescription>{oauthStatus.note}</AlertDescription>
                  </Alert>
                ) : null}
              </div>
            )}
          </CardContent>
        </Card>
      ) : null}

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>加载失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {loading ? (
        <div className="grid gap-4 md:grid-cols-2">
          <Skeleton className="h-40 rounded-xl" />
          <Skeleton className="h-40 rounded-xl" />
        </div>
      ) : selected ? (
        <>
          {/* 当前登录态不再单列一张卡：它就是账号库里带「当前」徽标的那张卡
              （与 WorkBuddy 账号管理页一致）。只有「当前账号还没入库」才需要提示。 */}
          {!overview?.loggedIn ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertTitle>未识别到登录态</AlertTitle>
              <AlertDescription>
                请先在 {selected.label} 中登录，再回来备份 / 切换账号。
                <span className="mt-1 block font-mono text-[11px] text-muted-foreground/70">
                  数据目录：{selected.user_data_dir}
                </span>
              </AlertDescription>
            </Alert>
          ) : !liveInVault ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertTitle>当前登录的账号还没加入账号库</AlertTitle>
              <AlertDescription>
                {live?.username ?? live?.email ?? "当前账号"}
                {live?.uid ? `（uid ${shortUid(live.uid)}）` : ""} 尚未备份。
                点右上角「备份当前登录态」把它收进来，之后就能像 WorkBuddy 那样一卡一账号地看积分与切换。
              </AlertDescription>
            </Alert>
          ) : null}

          {/* 账号库：一卡一账号（与 WorkBuddy 账号管理页同一套卡片） */}
          <section className="space-y-4">
            <TraeCreditsHeader
              credits={credits}
              accountCount={overview?.vault.length ?? 0}
              clientLabel={selected.label}
            />

            {overview?.vault.length ? (
              <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
                {overview.vault.map((entry) => {
                  const meta = entry.meta;
                  const oa = entry.oauth;
                  const isOauth = entry.kind === "oauth";
                  const hasCarrier = (meta?.files ?? []).some(
                    (f) => f.rel.replace(/\\/g, "/") === "User/globalStorage/storage.json",
                  );
                  const isLive =
                    overview.loggedIn && meta?.verified_uid != null && meta.verified_uid === live?.uid;
                  const name =
                    entry.displayName || (isOauth && oa?.displayName ? oa.displayName : null) || entry.id;
                  const uid = isOauth ? (oa?.uid ?? null) : (meta?.verified_uid ?? null);
                  const exp = tokenExpiryLabel(oa?.tokenExp);
                  const credit = creditById.get(entry.id) ?? null;
                  return (
                    <AccountCard
                      key={entry.id}
                      name={name}
                      avatarUrl={oa?.avatar ?? credit?.avatar ?? null}
                      identity={uid ? `uid ${shortUid(uid)}` : entry.id}
                      identityTitle={uid ?? entry.id}
                      identityMono
                      isCurrent={isLive}
                      chips={
                        <>
                          {isLive ? <Chip tone="ok">当前登录</Chip> : null}
                          {isOauth ? <Chip tone="outline">网页凭证</Chip> : null}
                          {credit?.mobile ? <Chip tone="muted">{credit.mobile}</Chip> : null}
                          {!isOauth && meta ? (
                            <Chip tone="muted">
                              {meta.file_count} 文件 · {formatBytes(meta.total_bytes)}
                            </Chip>
                          ) : null}
                          {isOauth && !hasCarrier ? <Chip tone="warn">首次切换自动合成载体</Chip> : null}
                          {exp ? <Chip tone="err">{exp}</Chip> : null}
                        </>
                      }
                      actions={
                        <>
                          <IconAction
                            icon={<Power className="size-4" />}
                            label={
                              isLive
                                ? "当前登录账号"
                                : isOauth && !hasCarrier
                                  ? "切换（首次会以当前登录态为骨架自动合成载体）"
                                  : "切换到这个账号"
                            }
                            active={isLive}
                            disabled={busy || isLive}
                            spinning={busy}
                            onClick={() => void onSwitch(entry)}
                          />
                          <IconAction
                            icon={<PencilLine className="size-4" />}
                            label="重命名"
                            onClick={() => {
                              setRenameTarget(entry);
                              setRenameName(entry.id);
                            }}
                          />
                          <IconAction
                            icon={<Trash2 className="size-4" />}
                            label="从账号库移除"
                            destructive
                            onClick={() => void onRemove(entry)}
                          />
                        </>
                      }
                    >
                      <TraeCreditBlock
                        entry={credit}
                        loading={creditsLoading}
                        onExpand={() => {
                          if (credit) credits.setExpand(credit);
                        }}
                      />
                    </AccountCard>
                  );
                })}
              </div>
            ) : (
              <Card>
                <CardContent className="py-10 text-center text-sm text-muted-foreground">
                  账号库为空。点「发起网页登录」扫码添加，或「备份当前登录态」把当前账号收进来。
                </CardContent>
              </Card>
            )}
          </section>

          {/* 回滚 / 导出：单独一张卡（与 WorkBuddy 账号页同位置、同外观），不再塞进账号卡的图标栏 */}
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-base">回滚</CardTitle>
              <CardDescription>
                结束客户端并把登录态重新写回指定账号，用于切换异常、客户端起不来，或想强制回到
                某个账号时。回滚前会先把当前登录态备份一份，写回失败会自动还原；
                点「导出该账号备份」可把选中账号的登录态复制到剪贴板，便于在别的机器导入。
              </CardDescription>
            </CardHeader>
            <CardContent className="flex flex-wrap items-center gap-2">
              <Select
                value={rollbackTarget ?? ""}
                onValueChange={setRollbackTarget}
                disabled={busy || !rollbackOptions.length}
              >
                <SelectTrigger className="w-full sm:w-72">
                  <SelectValue placeholder="选择账号…" />
                </SelectTrigger>
                <SelectContent>
                  {rollbackOptions.map((o) => (
                    <SelectItem key={o.id} value={o.id}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Button
                variant="outline"
                disabled={busy || !rollbackTarget}
                onClick={() => rollbackTarget && void onRollback(rollbackTarget)}
              >
                {busy ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <RotateCcw className="size-4" />
                )}
                回滚到该账号
              </Button>
              <Button
                variant="outline"
                disabled={busy || !rollbackTarget}
                onClick={() => rollbackTarget && void onExportAccount(rollbackTarget)}
              >
                <FileDown className="size-4" />
                导出该账号备份
              </Button>
            </CardContent>
          </Card>

          {/* 切换进度 */}
          {progress.length > 0 ? (
            <Card>
              <CardHeader className="pb-2">
                <CardTitle className="flex items-center gap-2 text-sm">
                  {busy ? <Loader2 className="size-4 animate-spin" /> : <CheckCircle2 className="size-4 text-emerald-500" />}
                  切换日志
                </CardTitle>
              </CardHeader>
              <CardContent>
                <pre
                  ref={progressRef}
                  className="max-h-52 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground"
                >
                  {progress.join("\n") || "（无输出）"}
                </pre>
              </CardContent>
            </Card>
          ) : null}

          {/* 交接记忆 */}
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="flex items-center gap-2 text-base">
                <ArchiveRestore className="size-4" />
                交接记忆
                <Button size="sm" variant="ghost" className="ml-auto" onClick={() => setHandoffOpen((v) => !v)}>
                  {handoffOpen ? "收起" : "展开"}
                </Button>
              </CardTitle>
              <CardDescription>
                从解密库自动提取会话要点 → 写入项目工作目录、项目规则与 Trae 记忆库，方便下一个账号接续任务。
              </CardDescription>
            </CardHeader>
            {handoffOpen ? (
              <CardContent className="grid gap-3">
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">项目工作目录（绝对路径，留空则跳过记忆库写入）</label>
                  <Input
                    value={hoProject}
                    onChange={(e) => setHoProject(e.target.value)}
                    placeholder="D:\projects\my-app"
                  />
                </div>
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">会话 ID（可选，逗号分隔；留空自动取解密库全部会话）</label>
                  <Input
                    value={hoSessions}
                    onChange={(e) => setHoSessions(e.target.value)}
                    placeholder="32 位十六进制会话 ID，多个用逗号分隔"
                  />
                </div>
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">下一步计划（每行一条）</label>
                  <textarea
                    value={hoSteps}
                    onChange={(e) => setHoSteps(e.target.value)}
                    rows={3}
                    className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring"
                    placeholder={"1. 修复登录页白屏\n2. 补单元测试"}
                  />
                </div>
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">关键文件（可选，逗号分隔）</label>
                  <Input
                    value={hoKeyFiles}
                    onChange={(e) => setHoKeyFiles(e.target.value)}
                    placeholder="src/lib/api.ts, src/pages/LoginPage.tsx"
                  />
                </div>
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">补充说明（可选）</label>
                  <textarea
                    value={hoNote}
                    onChange={(e) => setHoNote(e.target.value)}
                    rows={2}
                    className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring"
                  />
                </div>
                <div className="flex items-center gap-2">
                  <Button variant="outline" onClick={() => void onHandoffPreview()} disabled={busy}>
                    预览落点
                  </Button>
                  <Button onClick={() => void onHandoffWrite()} disabled={busy}>
                    <Save className="size-4" />写入交接记忆
                  </Button>
                  {busy ? <Loader2 className="size-4 animate-spin text-muted-foreground" /> : null}
                </div>
                {hoResult ? (
                  <div className="rounded-lg border border-border bg-muted/40 p-3 text-xs">
                    {hoResult.files.length ? (
                      <>
                        <div className="mb-1 font-medium">将写入 / 已写入：</div>
                        <ul className="space-y-0.5 font-mono text-muted-foreground">
                          {hoResult.files.map((f) => (
                            <li key={f} className="truncate">{f}</li>
                          ))}
                        </ul>
                      </>
                    ) : null}
                    {hoResult.skipped.length ? (
                      <>
                        <div className="mb-1 mt-2 font-medium text-amber-600">跳过：</div>
                        <ul className="space-y-0.5 text-amber-600/80">
                          {hoResult.skipped.map((s) => (
                            <li key={s}>{s}</li>
                          ))}
                        </ul>
                      </>
                    ) : null}
                  </div>
                ) : null}
              </CardContent>
            ) : null}
          </Card>
        </>
      ) : (
        <Alert>
          <AlertTriangle className="size-4" />
          <AlertTitle>未安装 Trae 客户端</AlertTitle>
          <AlertDescription>
            未检测到 Trae 客户端的数据目录。请先安装并登录 Trae CN 或 TRAE SOLO CN。
          </AlertDescription>
        </Alert>
      )}

      <Separator className="my-1" />

      {/* 备份对话框 */}
      <Dialog open={backupOpen} onOpenChange={setBackupOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>备份当前登录态</DialogTitle>
            <DialogDescription>为该账号取一个便于识别的名称，载体文件将完整复制进账号库。</DialogDescription>
          </DialogHeader>
          <Input
            value={backupName}
            onChange={(e) => setBackupName(e.target.value)}
            placeholder="例如：工作主账号"
            autoFocus
          />
          <DialogFooter>
            <Button variant="outline" onClick={() => setBackupOpen(false)}>取消</Button>
            <Button onClick={() => void onBackup()} disabled={!backupName.trim() || busy}>
              {busy ? <Loader2 className="size-4 animate-spin" /> : <Save className="size-4" />}备份
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 重命名对话框 */}
      <Dialog open={renameTarget !== null} onOpenChange={(v) => !v && setRenameTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>重命名账号</DialogTitle>
            <DialogDescription>改名只影响账号库条目，不影响客户端登录态。</DialogDescription>
          </DialogHeader>
          <Input value={renameName} onChange={(e) => setRenameName(e.target.value)} autoFocus />
          <DialogFooter>
            <Button variant="outline" onClick={() => setRenameTarget(null)}>取消</Button>
            <Button onClick={() => void onRename()} disabled={!renameName.trim() || renameName.trim() === renameTarget?.id}>
              确定
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 导入对话框 */}
      <Dialog open={importOpen} onOpenChange={setImportOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>导入账号备份</DialogTitle>
            <DialogDescription>粘贴此前导出的自包含 JSON（含 base64 载体文件）。</DialogDescription>
          </DialogHeader>
          <Input
            value={importName}
            onChange={(e) => setImportName(e.target.value)}
            placeholder="覆盖账号名（可选）"
          />
          <textarea
            value={importText}
            onChange={(e) => setImportText(e.target.value)}
            rows={8}
            className="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-xs shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring"
            placeholder='{"format":"trae-vault","files":[...]}'
          />
          <DialogFooter>
            <Button variant="outline" onClick={() => setImportOpen(false)}>取消</Button>
            <Button onClick={() => void onImport()} disabled={!importText.trim()}>导入</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 全部积分包 */}
      <TraeCreditsDialog entry={credits.expand} onClose={() => credits.setExpand(null)} />
    </div>
  );
}
