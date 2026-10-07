import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  ArrowRight,
  CheckCircle2,
  Database,
  HardDriveUpload,
  KeyRound,
  Loader2,
  RefreshCw,
  ScanSearch,
} from "lucide-react";
import { listen } from "@tauri-apps/api/event";

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
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import * as api from "@/lib/api";
import type {
  TraeInstalledClient,
  TraeWbExportReport,
  TraeWbPreview,
  TraeWbSourceSession,
  TraeWbTarget,
} from "@/lib/trae-types";

function formatTime(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function humanSize(bytes: number): string {
  if (bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/**
 * Trae → WorkBuddy 会话导出（`WorkbuddyImportPage` 的反向）。
 *
 * 把 Trae 加密库里的会话转换成 WorkBuddy 的明文记录，写到 `~/.workbuddy`：
 * 正文落 `projects/{工作区key}/{会话id}.jsonl`，元数据插 `workbuddy.db` 的 `sessions` 表。
 * 因为要改客户端正在用的库，导出前会自动退出 WorkBuddy、写完再拉起。
 */
export default function WorkbuddyExportPage() {
  const [target, setTarget] = useState<TraeWbTarget | null>(null);
  const [clients, setClients] = useState<TraeInstalledClient[]>([]);
  /** 排在最前且**确有使用历史**的客户端（全 0 分时是 null）——只用来打「常用」徽标。 */
  const [usageTopPick, setUsageTopPick] = useState<string | null>(null);
  const [clientKey, setClientKey] = useState<string | null>(null);
  const [sessions, setSessions] = useState<TraeWbSourceSession[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [hint, setHint] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [filter, setFilter] = useState("");

  const [uid, setUid] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [busyLabel, setBusyLabel] = useState("");
  const [progress, setProgress] = useState<string[]>([]);
  const [report, setReport] = useState<TraeWbExportReport | null>(null);

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [preview, setPreview] = useState<TraeWbPreview | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);

  useEffect(() => {
    // 与 workbuddy-import-card 同样的 `listen()` 竞态：异步注册期间若组件已卸载，
    // 清理函数拿不到注销句柄，会残留一个监听器把每条进度投递两遍。
    let active = true;
    let un: (() => void) | undefined;
    void listen<{ line: string }>("trae-wb-export-progress", (e) => {
      setProgress((prev) => [...prev, e.payload.line]);
    }).then((u) => {
      if (active) un = u;
      else u();
    });
    return () => {
      active = false;
      un?.();
    };
  }, []);

  const loadTarget = useCallback(async () => {
    try {
      const t = await api.traeWbExportTarget();
      setTarget(t);
      setUid((prev) => prev || t.default_uid || t.accounts[0]?.uid || "");
    } catch (cause) {
      setError(api.asError(cause));
    }
  }, []);

  const loadClients = useCallback(async () => {
    try {
      const { clients, usage } = await api.traeListClients();
      setClients(clients);
      setUsageTopPick(usage?.topPick ?? null);
      // ⚠️ 顺序**已经由后端按「使用记忆」排好**（默认 `solo-cn` 第一）⇒ 直接取第一个。
      //    前端别再自己写一套「trae-cn 优先」的规则，否则各页面顺序会不一致。
      const installed = clients.filter((c) => c.installed);
      setClientKey((prev) => prev ?? installed[0]?.key ?? null);
    } catch (cause) {
      setError(api.asError(cause));
    }
  }, []);

  useEffect(() => {
    void loadTarget();
    void loadClients();
  }, [loadTarget, loadClients]);

  /** 读取可导出会话。快照与实时库不一致时先确保解密（一致则零解密直接复用）。 */
  const loadSessions = useCallback(async (key: string) => {
    setLoading(true);
    setError(null);
    setHint("");
    setSelected(new Set());
    try {
      const st = await api.traeDecryptedStatus(key);
      if (!st.exists) {
        setSessions(null);
        setHint("该客户端还没有解密快照，请先点「扫描密钥并解密」。");
        return;
      }
      try {
        await api.traeEnsureDecrypted(key);
      } catch (cause) {
        // 无已存密钥 / 密钥过期：沿用既有明文快照，列表可能不是最新
        setHint(api.asError(cause));
      }
      const list = await api.traeWbExportSessions(key);
      setSessions(list.sessions);
    } catch (cause) {
      setError(api.asError(cause));
      setSessions(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (clientKey) void loadSessions(clientKey);
  }, [clientKey, loadSessions]);

  const shown = useMemo(() => {
    const all = sessions ?? [];
    const q = filter.trim().toLowerCase();
    if (!q) return all;
    return all.filter(
      (s) =>
        s.title.toLowerCase().includes(q) ||
        s.cwd.toLowerCase().includes(q) ||
        s.id.toLowerCase().includes(q),
    );
  }, [sessions, filter]);

  const selectedSessions = useMemo(
    () => (sessions ?? []).filter((s) => selected.has(s.id)),
    [sessions, selected],
  );

  /** 选中会话将落到哪些 WorkBuddy 工作区（本地推算，便于直接看到影响面）。 */
  const selectedWorkspaces = useMemo(() => {
    const keys = new Set(selectedSessions.map((s) => s.workspace_key || "(未知工作区)"));
    return [...keys];
  }, [selectedSessions]);

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleAll() {
    setSelected((prev) =>
      prev.size === shown.length ? new Set() : new Set(shown.map((s) => s.id)),
    );
  }

  async function onScanAndDecrypt() {
    if (!clientKey || busy) return;
    setBusy(true);
    setBusyLabel("扫描进程内存并解密");
    try {
      const res = await api.traeScanAndDecrypt(clientKey);
      toast.success(res.report.hmac_ok ? "解密成功" : "密钥校验未通过", {
        description: `${res.report.pages} 页 · ${res.report.tables.length} 张表`,
      });
      await loadSessions(clientKey);
    } catch (cause) {
      toast.error("扫描并解密失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  async function openConfirm() {
    if (!clientKey || selectedSessions.length === 0) return;
    setConfirmOpen(true);
    setPreview(null);
    setPreviewBusy(true);
    setReport(null);
    setProgress([]);
    try {
      setPreview(await api.traeWbExportPreview(clientKey, selectedSessions.map((s) => s.id)));
    } catch (cause) {
      toast.error("预览失败", { description: api.asError(cause) });
    } finally {
      setPreviewBusy(false);
    }
  }

  async function run() {
    if (!clientKey || !uid || selectedSessions.length === 0 || busy) return;
    setBusy(true);
    setBusyLabel("导出到 WorkBuddy");
    setProgress([]);
    try {
      const r = await api.traeWbExportRun(clientKey, uid, selectedSessions.map((s) => s.id));
      setReport(r);
      setConfirmOpen(false);
      setSelected(new Set());
      toast.success(`已导出 ${r.written_sessions} 个会话到 WorkBuddy`, {
        description: `${r.turns} 个回合 / ${r.tool_steps} 个工具步骤 → 账号 ${r.target_label}${r.relaunched ? " · 已自动重启 WorkBuddy" : ""}`,
      });
      await loadTarget();
      if (clientKey) await loadSessions(clientKey);
    } catch (cause) {
      toast.error("导出失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  const accounts = target?.accounts ?? [];
  const previewTotal = preview?.preview.reduce(
    (acc, p) => ({
      turns: acc.turns + p.turns,
      answered: acc.answered + p.answered,
      steps: acc.steps + p.tool_steps,
      bytes: acc.bytes + p.bytes,
    }),
    { turns: 0, answered: 0, steps: 0, bytes: 0 },
  );

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">Trae → WorkBuddy</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          把 Trae 的会话导出成本机 WorkBuddy 的明文记录：正文写入{" "}
          <code className="font-mono text-xs">projects/&lt;工作区&gt;/&lt;会话&gt;.jsonl</code>
          ，元数据写入{" "}
          <code className="font-mono text-xs">workbuddy.db</code> 的 <code className="font-mono text-xs">sessions</code>{" "}
          表。提问、最终回答、思考与工具调用都会还原。
        </p>
      </div>

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>操作失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {/* WorkBuddy 目标 */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="flex items-center gap-2 text-base">
            <HardDriveUpload className="size-4" />
            导出目标：本机 WorkBuddy
            {target?.available ? (
              target.running ? (
                <Badge className="bg-amber-500/15 text-amber-600">运行中（导出时会自动退出）</Badge>
              ) : (
                <Badge className="bg-emerald-500/15 text-emerald-600">已就绪</Badge>
              )
            ) : (
              <Badge variant="secondary">未检测到</Badge>
            )}
          </CardTitle>
          <CardDescription className="break-all font-mono text-xs">
            {target?.data_root ?? "—"}
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          {!target?.available ? (
            <Alert>
              <Database className="size-4" />
              <AlertTitle>未检测到 WorkBuddy 数据</AlertTitle>
              <AlertDescription>
                需要本机存在 <code className="font-mono">~/.workbuddy/projects/</code> 与{" "}
                <code className="font-mono">workbuddy.db</code>（即 WorkBuddy 桌面版至少用过一次）。
              </AlertDescription>
            </Alert>
          ) : (
            <>
              <div className="space-y-2">
                <div className="text-xs text-muted-foreground">写入哪个账号</div>
                {accounts.length === 0 ? (
                  <p className="text-sm text-muted-foreground">
                    未识别到本机 WorkBuddy 账号（需要在 WorkBuddy 里登录过）。
                  </p>
                ) : (
                  <div className="flex flex-wrap gap-1.5">
                    {accounts.map((a) => (
                      <button
                        key={a.uid}
                        type="button"
                        disabled={busy}
                        onClick={() => setUid(a.uid)}
                        className={cn(
                          "rounded-lg border px-2.5 py-1.5 text-left transition-colors",
                          uid === a.uid
                            ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                            : "border-border hover:bg-foreground/[0.03]",
                        )}
                      >
                        {/* 主行给「谁」：用户名/手机号；副行给「哪个」：uid 尾号 + 版本 + 会话数 */}
                        <span className="block">{a.name || `uid …${a.uid.slice(-6)}`}</span>
                        <span className="block text-xs text-muted-foreground">
                          uid …{a.uid.slice(-6)}
                          {a.meta ? ` · ${a.meta}` : ""} · {a.sessions} 个会话
                          {a.is_current ? " · 最近活动" : ""}
                        </span>
                      </button>
                    ))}
                  </div>
                )}
              </div>
              <p className="text-xs text-muted-foreground">
                导出会先退出 WorkBuddy 桌面版（写完自动拉起），原库会先整份备份到工具目录。
                {target.exe ? "" : " 未探测到主程序路径，若 WorkBuddy 原本在运行需要手动重新打开。"}
              </p>
            </>
          )}
        </CardContent>
      </Card>

      {/* 来源（Trae） */}
      <Card>
        <CardHeader className="pb-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <CardTitle className="text-base">选择要导出的 Trae 会话</CardTitle>
              <CardDescription>
                {sessions
                  ? `共 ${sessions.length} 个会话${filter ? `，筛选出 ${shown.length} 个` : ""}；已选 ${selectedSessions.length} 个`
                  : "正在读取…"}
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() => clientKey && void loadSessions(clientKey)}
                disabled={busy}
              >
                <RefreshCw className="size-4" />刷新
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={toggleAll}
                disabled={shown.length === 0 || busy}
              >
                {selected.size === shown.length && shown.length > 0 ? "全部取消" : "全选"}
              </Button>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          {/* 客户端选择：顺序由「使用记忆」决定（默认 `solo-cn` 第一） */}
          {clients.length > 0 ? (
            <div className="flex flex-wrap items-center gap-2">
              {clients.map((c) => {
                const active = c.key === clientKey;
                return (
                  <button
                    key={c.key}
                    type="button"
                    disabled={!c.installed || busy}
                    title={
                      c.key === usageTopPick
                        ? "按使用记忆自动排在最前（切换账号次数 ×3 + 打开页面次数）"
                        : undefined
                    }
                    onClick={() => setClientKey(c.key)}
                    className={cn(
                      "flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors",
                      active
                        ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                        : "border-border bg-background hover:bg-foreground/[0.03]",
                      (!c.installed || busy) && "cursor-not-allowed opacity-40",
                    )}
                  >
                    {c.label}
                    {c.key === usageTopPick ? (
                      <span className="rounded bg-foreground/[0.08] px-1.5 py-0.5 text-[10px] font-normal text-muted-foreground">
                        常用
                      </span>
                    ) : null}
                    {c.installed && c.has_login ? (
                      <span className="size-2 rounded-full bg-emerald-500" />
                    ) : null}
                  </button>
                );
              })}
            </div>
          ) : null}

          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="按标题 / 工作目录 / 会话 id 筛选…"
            className="h-8 w-full rounded-md border border-border bg-background px-2.5 text-xs outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring/40"
          />

          {hint ? (
            <div className="flex flex-wrap items-center gap-2 rounded-lg bg-amber-500/10 px-3 py-2 text-xs leading-5 text-amber-700">
              <KeyRound className="size-3.5 shrink-0" />
              <span className="min-w-0 flex-1">{hint}</span>
              {!sessions ? (
                <Button
                  size="sm"
                  variant="outline"
                  className="h-6 gap-1 px-2 text-xs"
                  disabled={busy || !clientKey}
                  onClick={() => void onScanAndDecrypt()}
                >
                  {busy ? <Loader2 className="size-3.5 animate-spin" /> : <ScanSearch className="size-3.5" />}
                  扫描密钥并解密
                </Button>
              ) : null}
            </div>
          ) : null}

          {loading ? (
            <Skeleton className="h-40 rounded-xl" />
          ) : shown.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {sessions ? "没有符合条件的会话。" : "该客户端暂无可导出的会话。"}
            </p>
          ) : (
            <div className="max-h-96 space-y-1 overflow-auto rounded-lg border p-2">
              {shown.map((s) => (
                <label
                  key={s.id}
                  className={cn(
                    "flex items-start gap-2.5 rounded-md px-2 py-1.5 transition-colors",
                    busy ? "opacity-50" : "cursor-pointer hover:bg-foreground/[0.03]",
                  )}
                >
                  <Checkbox
                    className="mt-0.5"
                    checked={selected.has(s.id)}
                    disabled={busy}
                    onCheckedChange={() => toggle(s.id)}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate font-medium">{s.title}</span>
                    <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                      {formatTime(s.updated_ms)} · {s.turns} 轮 · {s.owner_label}
                    </span>
                    <span className="block truncate font-mono text-[11px] text-muted-foreground/80">
                      {s.cwd || "(无工作目录)"} → 工作区 {s.workspace_key || "?"}
                    </span>
                  </span>
                </label>
              ))}
            </div>
          )}

          {selectedWorkspaces.length ? (
            <p className="text-xs text-muted-foreground">
              将写入工作区：
              {selectedWorkspaces.map((w) => (
                <code key={w} className="ml-1 font-mono">
                  {w}
                </code>
              ))}
            </p>
          ) : null}
        </CardContent>
      </Card>

      {/* 操作条 */}
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border bg-muted/40 px-4 py-3">
        <div className="text-sm text-muted-foreground">
          已选 <span className="font-medium text-foreground">{selectedSessions.length}</span> 个会话
          {busy ? (
            <span className="ml-3 inline-flex items-center gap-1.5 text-xs">
              <Loader2 className="size-3.5 animate-spin" />
              {busyLabel || "处理中…"}
            </span>
          ) : null}
        </div>
        <Button
          disabled={busy || !target?.available || !uid || selectedSessions.length === 0}
          onClick={() => void openConfirm()}
        >
          <HardDriveUpload className="size-4" />
          导出到 WorkBuddy
        </Button>
      </div>

      {progress.length ? (
        <pre className="max-h-60 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
          {progress.join("\n")}
        </pre>
      ) : null}

      {report ? (
        <Alert>
          <CheckCircle2 className="size-4" />
          <AlertTitle>导出完成</AlertTitle>
          <AlertDescription className="space-y-1">
            <span className="block">
              {report.written_sessions} 个会话 / {report.turns} 个回合 / {report.tool_steps} 个工具步骤 →
              账号 {report.target_label}
            </span>
            <span className="block">
              工作区：{report.workspaces.join("、") || "—"}
            </span>
            <span className="block break-all font-mono text-xs">
              备份：{report.backup_dir}
            </span>
            <span className="block text-xs">
              库内现有会话：{report.verified_sessions}
              {report.relaunched
                ? " · 已自动重启 WorkBuddy"
                : report.was_running
                  ? " · 请手动启动 WorkBuddy"
                  : " · WorkBuddy 原本未运行，打开即可看到"}
            </span>
          </AlertDescription>
        </Alert>
      ) : null}

      {/* 确认 + 进度 */}
      <Dialog open={confirmOpen} onOpenChange={(v) => !busy && setConfirmOpen(v)}>
        <DialogContent className="max-h-[86vh] max-w-2xl grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <HardDriveUpload className="size-4" />
              Trae → WorkBuddy
            </DialogTitle>
            <DialogDescription className="break-words">
              导出期间会先退出 WorkBuddy 桌面版，写完自动拉起。原始 Trae 会话只读、不会改动；
              WorkBuddy 会话库会先整份备份。
            </DialogDescription>
          </DialogHeader>

          <div className="min-h-0 space-y-3 overflow-y-auto pr-1 text-sm">
            {previewBusy ? (
              <div className="flex items-center gap-2 text-muted-foreground">
                <Loader2 className="size-4 animate-spin" />
                正在预览转换结果…
              </div>
            ) : preview && previewTotal ? (
              <>
                <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                  {[
                    ["会话", String(preview.preview.length)],
                    ["回合", String(previewTotal.turns)],
                    ["工具步骤", String(previewTotal.steps)],
                    ["正文体积", humanSize(previewTotal.bytes)],
                  ].map(([k, v]) => (
                    <div key={k} className="rounded-lg bg-muted/50 p-2.5">
                      <div className="text-xs text-muted-foreground">{k}</div>
                      <div className="font-medium tabular-nums">{v}</div>
                    </div>
                  ))}
                </div>
                {previewTotal.turns > previewTotal.answered ? (
                  <p className="text-xs text-muted-foreground">
                    其中 {previewTotal.turns - previewTotal.answered} 个回合在 Trae 里本就没有回答（被中断或
                    重试的回合），导出后同样为空。
                  </p>
                ) : null}
                <div className="max-h-56 space-y-1 overflow-auto rounded-lg border p-2">
                  {preview.preview.map((p) => (
                    <div key={p.trae_id} className="rounded-md px-2 py-1.5">
                      <div className="truncate font-medium">{p.title}</div>
                      <div className="truncate text-xs text-muted-foreground">
                        {p.turns} 回合 / {p.tool_steps} 工具步骤 / {p.events} 事件 / {humanSize(p.bytes)}
                        {p.cwd ? ` · ${p.cwd}` : ""}
                      </div>
                      <div className="truncate font-mono text-[11px] text-muted-foreground/80">
                        {p.trae_id} → {p.workbuddy_id} · 工作区 {p.workspace_key}
                      </div>
                    </div>
                  ))}
                </div>
                {preview.skipped.length ? (
                  <div className="space-y-0.5 rounded-lg bg-amber-500/10 p-2.5 text-xs leading-5 text-amber-700">
                    {preview.skipped.map((s) => (
                      <div key={s.session_id} className="break-all">
                        跳过 {s.session_id}：{s.reason}
                      </div>
                    ))}
                  </div>
                ) : null}
              </>
            ) : null}

            {progress.length ? (
              <pre className="max-h-40 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
                {progress.join("\n")}
              </pre>
            ) : null}

            {target?.running ? (
              <Alert>
                <AlertTriangle className="size-4" />
                <AlertDescription className="leading-5">
                  WorkBuddy 正在运行，开始导出后它会被关闭（写完自动重新打开）。请先保存好正在编辑的内容。
                </AlertDescription>
              </Alert>
            ) : null}
          </div>

          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={() => setConfirmOpen(false)}>
              取消
            </Button>
            <Button disabled={busy || !preview} onClick={() => void run()}>
              {busy ? <Loader2 className="size-4 animate-spin" /> : <ArrowRight className="size-4" />}
              {busy ? "导出中…" : "确认导出"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

    </div>
  );
}
