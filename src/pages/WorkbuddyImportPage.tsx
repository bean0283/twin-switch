import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  ArrowRight,
  CheckCircle2,
  Database,
  HardDriveDownload,
  KeyRound,
  Loader2,
  RefreshCw,
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
import { TraeClientSwitcher, type TraeClientOption } from "@/components/trae-client-switcher";
import { cn } from "@/lib/utils";
import * as api from "@/lib/api";
import type {
  TraeImportCandidate,
  TraeImportInspect,
  TraeInstalledClient,
  TraeWorkbuddyList,
  TraeWorkbuddyPreviewItem,
  TraeWorkbuddyReport,
  TraeWorkbuddySession,
} from "@/lib/trae-types";

/** 「3 分钟前」「昨天 22:03」这类相对时间——重复副本的时间差只有几分钟，绝对时间看不出先后。 */
function relativeTime(ms: number): string {
  if (!ms) return "—";
  const diff = Date.now() - ms;
  if (diff < 0) return formatTime(ms);
  const min = Math.floor(diff / 60000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const day = Math.floor(hour / 24);
  if (day < 30) return `${day} 天前`;
  return formatTime(ms);
}

/** 字节数 → 人类可读。 */
function humanSize(bytes: number): string {
  if (bytes <= 0) return "无正文";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function formatTime(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/**
 * WorkBuddy → Trae 会话导入（`WorkbuddyExportPage` 的反向）。
 *
 * WorkBuddy 的记录是**明文**（`~/.workbuddy/projects/*.jsonl` + `workbuddy.db`），与 Trae 的
 * 加密关系库格式完全不同，因此这里做的是「读取 → 转换 → 加密写入目标账号本地库」，包含提问、
 * 最终回答，以及思考与工具调用过程。
 *
 * ⚠️ 版式必须与 {@link WorkbuddyExportPage} **逐块对应**（T39）：两张卡（目标 / 来源）+
 *    一条操作条 + 进度与结果，确认详情放弹窗里。两个方向是同一件事的两面，用户来回切的时候
 *    不该重新学一遍界面。改动其中一页时请顺手看一眼另一页。
 */
export default function WorkbuddyImportPage() {
  const [list, setList] = useState<TraeWorkbuddyList | null>(null);
  const [clients, setClients] = useState<TraeInstalledClient[]>([]);
  /** 排在最前且**确有使用历史**的客户端（全 0 分时是 null）——只用来打「常用」徽标。 */
  const [usageTopPick, setUsageTopPick] = useState<string | null>(null);
  const [candidates, setCandidates] = useState<TraeImportCandidate[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [dstClient, setDstClient] = useState<string | null>(null);
  const [dstKey, setDstKey] = useState<string | null>(null);
  const [inspect, setInspect] = useState<TraeImportInspect | null>(null);

  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [showDeleted, setShowDeleted] = useState(false);
  const [filter, setFilter] = useState("");

  const [busy, setBusy] = useState(false);
  const [busyLabel, setBusyLabel] = useState("");
  const [progress, setProgress] = useState<string[]>([]);
  const [report, setReport] = useState<TraeWorkbuddyReport | null>(null);

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [preview, setPreview] = useState<TraeWorkbuddyPreviewItem[] | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);

  useEffect(() => {
    // `listen()` 是异步的：若在它 resolve 之前组件就卸载了（React StrictMode 在开发模式下
    // 会「挂载 → 卸载 → 再挂载」跑两遍 effect），清理函数拿到的 `un` 仍是 undefined，
    // 第一个监听器就永远注销不掉 —— 于是每条进度事件都被投递两次，日志成对重复。
    // 这里用一个 `active` 标记兜住：已经卸载就立刻注销刚拿到的监听器。
    let active = true;
    let un: (() => void) | undefined;
    void listen<{ line: string }>("trae-workbuddy-progress", (e) => {
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

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [src, cand, cl] = await Promise.all([
        api.traeWorkbuddyList(),
        api.traeImportCandidates(""),
        api.traeListClients(),
      ]);
      setList(src);
      setCandidates(cand.candidates);
      setClients(cl.clients);
      setUsageTopPick(cl.usage?.topPick ?? null);
      // ⚠️ 列表刷新后必须摘掉已经不在列表里的勾选（T36）：会话可能被 WorkBuddy 删掉、
      //    正文文件可能被清掉，留着 id 会让「已选 N 个」虚高、确认弹窗里少几行。
      const alive = new Set(src.sessions.map((s) => s.id));
      setSelected((prev) => new Set([...prev].filter((id) => alive.has(id))));
      // 默认目标 = 候选里排最前的那个客户端（后端按「使用记忆」排序，且只产出**已安装**客户端）。
      // 一个候选都没有时退回第一个已安装客户端，好让「该客户端下没有可用账号」的提示显出来。
      setDstClient(
        (prev) => prev ?? cand.candidates[0]?.client_key ?? cl.clients.find((c) => c.installed)?.key ?? null,
      );
    } catch (cause) {
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 客户端切换条（顺序 / 「常用」徽标 / 未安装置灰都只由后端与共用组件决定）。 */
  const clientOptions = useMemo<TraeClientOption[]>(
    () =>
      clients.map((c) => ({
        key: c.key,
        label: c.label,
        installed: c.installed,
        hasLogin: c.has_login,
        top: c.key === usageTopPick,
      })),
    [clients, usageTopPick],
  );

  /** uid → `昵称（uid …97eac1）`：多账号时尾部 6 位根本分不清谁是谁。 */
  const accountLabels = useMemo(() => {
    const map = new Map<string, string>();
    for (const a of list?.accounts ?? []) map.set(a.uid, a.label);
    return map;
  }, [list]);

  const ownerOf = useCallback(
    (uid: string) =>
      (uid && accountLabels.get(uid)) || (uid ? `uid …${uid.slice(-6)}` : "未知账号"),
    [accountLabels],
  );

  const dstCandidates = useMemo(
    () => candidates.filter((c) => c.client_key === dstClient),
    [candidates, dstClient],
  );

  const target = useMemo(
    () =>
      dstCandidates.find((c) => `${c.client_key}::${c.account_id}` === dstKey) ?? null,
    [dstCandidates, dstKey],
  );

  // 目标账号默认取该客户端下「有会话库」的第一个：库不存在时导入必然失败，
  // 默认落在它上面等于让用户点一次才发现。切客户端时同步换掉已经不属于该客户端的旧目标。
  useEffect(() => {
    const pick = dstCandidates.find((c) => c.db_exists) ?? dstCandidates[0] ?? null;
    const key = pick ? `${pick.client_key}::${pick.account_id}` : null;
    setDstKey((prev) =>
      prev && dstCandidates.some((c) => `${c.client_key}::${c.account_id}` === prev) ? prev : key,
    );
  }, [dstCandidates]);

  // 目标就绪状态（只读探测）：运行中 / 有没有会话库 / 密钥在不在。
  // 「密钥不在且客户端也没跑」这一种情况导入必然失败，必须在点之前就说清楚。
  useEffect(() => {
    if (!target) {
      setInspect(null);
      return;
    }
    let active = true;
    setInspect(null);
    void api
      .traeImportInspect(target.client_key, target.account_id)
      .then((v) => {
        if (active) setInspect(v);
      })
      .catch(() => {
        if (active) setInspect(null);
      });
    return () => {
      active = false;
    };
  }, [target]);

  const shown = useMemo(() => {
    const all = list?.sessions ?? [];
    const visible = all.filter((s) => (showDeleted ? true : !s.deleted));
    const q = filter.trim().toLowerCase();
    if (!q) return visible;
    return visible.filter(
      (s) =>
        s.title.toLowerCase().includes(q) ||
        s.cwd.toLowerCase().includes(q) ||
        s.id.toLowerCase().includes(q),
    );
  }, [list, showDeleted, filter]);

  const importable = useMemo(() => shown.filter((s) => s.has_body), [shown]);
  const deletedCount = useMemo(
    () => (list?.sessions ?? []).filter((s) => s.deleted).length,
    [list],
  );
  const selectedSessions = useMemo(
    () => importable.filter((s) => selected.has(s.id)),
    [importable, selected],
  );

  /**
   * 已勾选、但当前**看不见**的会话（被筛选或「隐藏已删除」藏起来了）。
   *
   * ⚠️ 勾选与可见行不是一回事（T36）：全选只作用于可见行，但已勾中的行不会因为筛选就被取消，
   *    它们**照样会被导入**。所以数量必须在操作条上显式说出来，不能静默吃掉。
   */
  const hiddenSelected = useMemo(() => {
    const visible = new Set(shown.map((s) => s.id));
    return selectedSessions.filter((s) => !visible.has(s.id));
  }, [selectedSessions, shown]);

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleAll() {
    setSelected((prev) => {
      const allShown = importable.length > 0 && importable.every((s) => prev.has(s.id));
      const next = new Set(prev);
      for (const s of importable) {
        if (allShown) next.delete(s.id);
        else next.add(s.id);
      }
      return next;
    });
  }

  /**
   * 同标题重复副本只留最新的一份（其余账号的旧副本不勾）。
   * 多账号场景下同一段对话往往有几份，全导入会在目标库里留下重复会话。
   */
  function selectNewestOnly() {
    setSelected(new Set(importable.filter((s) => s.is_newest).map((s) => s.id)));
  }

  const dupGroupCount = useMemo(
    () => new Set(importable.filter((s) => s.dup_group).map((s) => s.dup_group)).size,
    [importable],
  );

  /** 会话 id → 标题：预览项只回 `ai_title`，取不到时用来源标题兜底。 */
  const titleOf = useMemo(() => {
    const map = new Map<string, string>();
    for (const s of list?.sessions ?? []) map.set(s.id, s.title);
    return map;
  }, [list]);

  const previewTotal = useMemo(
    () =>
      (preview ?? []).reduce(
        (acc, p) => ({
          turns: acc.turns + p.turns,
          steps: acc.steps + p.tool_steps,
        }),
        { turns: 0, steps: 0 },
      ),
    [preview],
  );

  async function openConfirm() {
    if (!target || selectedSessions.length === 0) return;
    setConfirmOpen(true);
    setPreview(null);
    setPreviewBusy(true);
    setReport(null);
    setProgress([]);
    try {
      const r = await api.traeWorkbuddyPreview(selectedSessions.map((s) => s.id));
      setPreview(r.preview);
    } catch (cause) {
      toast.error("预览失败", { description: api.asError(cause) });
    } finally {
      setPreviewBusy(false);
    }
  }

  async function run() {
    if (!target || selectedSessions.length === 0 || busy) return;
    setBusy(true);
    setBusyLabel("导入到 Trae");
    setProgress([]);
    try {
      const r = await api.traeWorkbuddyImport(
        target.client_key,
        target.uid,
        selectedSessions.map((s) => s.id),
      );
      setReport(r);
      setConfirmOpen(false);
      setSelected(new Set());
      toast.success(`已导入 ${r.written_sessions} 个会话`, {
        description: `${r.turns} 个回合 / ${r.tool_steps} 个工具步骤 → ${r.target_label}`,
      });
      await load();
    } catch (cause) {
      toast.error("导入失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">WorkBuddy → Trae</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          读取本机 WorkBuddy 的明文会话（JSONL），转换成 Trae 的关系库结构后加密写入指定账号的本地库——
          包含提问、最终回答，以及思考与工具调用过程。
        </p>
      </div>

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>操作失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {!list?.available && !loading ? (
        <Alert>
          <Database className="size-4" />
          <AlertTitle>未检测到 WorkBuddy 数据</AlertTitle>
          <AlertDescription>
            需要本机存在 <code className="font-mono">~/.workbuddy/projects/</code> 与{" "}
            <code className="font-mono">workbuddy.db</code>（即 WorkBuddy 桌面版至少用过一次）。
          </AlertDescription>
        </Alert>
      ) : null}

      {/* 导入目标（Trae 侧） */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="flex items-center gap-2 text-base">
            <HardDriveDownload className="size-4" />
            导入目标：Trae 客户端
            {!dstClient ? (
              <Badge variant="secondary">未检测到</Badge>
            ) : inspect ? (
              inspect.running ? (
                <Badge className="bg-amber-500/15 text-amber-600">运行中（导入时会自动退出）</Badge>
              ) : inspect.db_exists ? (
                <Badge className="bg-emerald-500/15 text-emerald-600">已就绪</Badge>
              ) : (
                <Badge variant="secondary">无会话库（需先登录一次）</Badge>
              )
            ) : null}
          </CardTitle>
          <CardDescription>
            选好客户端与账号后，勾选的 WorkBuddy 会话会被转换成 Trae 的表结构，加密写入该账号的本地库。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="text-xs text-muted-foreground">写入哪个客户端</div>
          <TraeClientSwitcher
            clients={clientOptions}
            value={dstClient}
            onChange={setDstClient}
            disabled={busy}
            label="Trae 客户端（导入目标）"
          />

          <div className="text-xs text-muted-foreground">写入哪个账号</div>
          {dstCandidates.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              该客户端下没有可用账号（需先在本机登录过）。
            </p>
          ) : (
            <div className="flex flex-wrap gap-1.5">
              {dstCandidates.map((c) => {
                const key = `${c.client_key}::${c.account_id}`;
                return (
                  <button
                    key={key}
                    type="button"
                    disabled={busy}
                    onClick={() => setDstKey(key)}
                    className={cn(
                      "rounded-lg border px-2.5 py-1.5 text-left transition-colors",
                      dstKey === key
                        ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                        : "border-border hover:bg-foreground/[0.03]",
                      !c.db_exists && "opacity-60",
                    )}
                  >
                    {/* 主行给「谁」：后端已经拼成 `昵称（uid …尾号）`；副行给「能不能写」 */}
                    <span className="block">{c.label}</span>
                    <span className="block text-xs text-muted-foreground">
                      {c.db_exists ? "有会话库" : "无会话库（需先登录一次）"}
                      {c.is_current ? " · 当前登录" : ""}
                      {c.is_source ? " · 源账号" : ""}
                    </span>
                  </button>
                );
              })}
            </div>
          )}

          <p className="text-xs text-muted-foreground">
            导入会先退出目标客户端（写完自动拉起），目标库会先整份备份；原始 WorkBuddy 记录只读、不会改动。
          </p>

          {inspect && !inspect.key_ready ? (
            <div className="flex flex-wrap items-center gap-2 rounded-lg bg-amber-500/10 px-3 py-2 text-xs leading-5 text-amber-700">
              <KeyRound className="size-3.5 shrink-0" />
              <span className="min-w-0 flex-1">
                {inspect.key_source === "scan_available"
                  ? "该账号没有存盘密钥，导入时会从正在运行的客户端进程内存里扫描——所以开始前别手动关掉客户端。"
                  : "该账号没有存盘密钥，且客户端当前没在运行，导入会失败。请先打开客户端，或到「Trae 账号管理」扫描密钥并解密。"}
              </span>
            </div>
          ) : null}
        </CardContent>
      </Card>

      {/* 来源（WorkBuddy） */}
      <Card>
        <CardHeader className="pb-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <CardTitle className="text-base">选择要导入的 WorkBuddy 会话</CardTitle>
              <CardDescription className="flex flex-wrap items-center gap-x-2">
                <span>
                  {list
                    ? `共 ${importable.length} 个可导入${filter ? `，筛选出 ${shown.length} 个` : ""}；已选 ${selectedSessions.length} 个`
                    : "正在读取…"}
                </span>
                <span className="break-all font-mono text-xs text-muted-foreground/80">
                  {list?.data_root ?? ""}
                </span>
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => void load()} disabled={busy || loading}>
                {loading ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
                刷新
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={toggleAll}
                disabled={importable.length === 0 || busy}
              >
                {importable.length > 0 && importable.every((s) => selected.has(s.id))
                  ? "全部取消"
                  : "全选"}
              </Button>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="flex flex-wrap items-center gap-2">
            <input
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="按标题 / 工作目录 / 会话 id 筛选…"
              className="h-8 min-w-52 flex-1 rounded-md border border-border bg-background px-2.5 text-xs outline-none focus-visible:ring-2 focus-visible:ring-sidebar-ring/40"
            />
            <label className="flex cursor-pointer items-center gap-1.5 text-xs text-muted-foreground">
              <Checkbox checked={showDeleted} onCheckedChange={(v) => setShowDeleted(Boolean(v))} />
              显示已删除（{deletedCount}）
            </label>
            {dupGroupCount > 0 ? (
              <Button
                size="sm"
                variant="outline"
                disabled={importable.length === 0 || busy}
                onClick={selectNewestOnly}
                title="同标题的多份副本里，只勾选内容最新的那一份"
              >
                每组只留最新（{dupGroupCount} 组）
              </Button>
            ) : null}
          </div>

          {loading && !list ? (
            <Skeleton className="h-40 rounded-xl" />
          ) : shown.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {list?.sessions.length ? "没有符合条件的会话。" : "本机暂无可导入的 WorkBuddy 会话。"}
            </p>
          ) : (
            <div className="max-h-96 space-y-1 overflow-auto rounded-lg border p-2">
              {shown.map((s) => (
                <SessionRow
                  key={s.id}
                  session={s}
                  owner={ownerOf(s.user_id)}
                  checked={selected.has(s.id)}
                  disabled={!s.has_body || busy}
                  onToggle={() => toggle(s.id)}
                />
              ))}
            </div>
          )}

          <p className="text-xs text-muted-foreground">
            归属账号显示为「用户名/手机号（uid …尾号）」；同一段对话在不同账号下会有多份，带
            <span className="font-medium text-foreground">「最新」</span>
            标记的那份包含的内容最全（按正文逐行摘要比对，不只看时间）。
            「无正文」的会话无法导入（WorkBuddy 里只留了元数据）。
          </p>
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
          {hiddenSelected.length ? (
            <span className="mt-1 block text-xs text-amber-600">
              其中 {hiddenSelected.length} 个被当前筛选或「隐藏已删除」藏起来了，仍会一起导入。
            </span>
          ) : null}
        </div>
        <Button
          disabled={busy || !list?.available || !target || selectedSessions.length === 0}
          onClick={() => void openConfirm()}
        >
          <HardDriveDownload className="size-4" />
          导入到 Trae
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
          <AlertTitle>导入完成</AlertTitle>
          <AlertDescription className="space-y-1">
            <span className="block">
              {report.written_sessions} 个会话 / {report.turns} 个回合 / {report.tool_steps} 个工具步骤 →
              {" "}
              {report.target_label}
            </span>
            {report.changed_pages != null ? (
              <span className="block">
                加密回写：全库 {report.pages} 页，只重写了{" "}
                <span className="font-medium text-foreground">{report.changed_pages}</span> 页（
                {report.changed_mb ?? 0} MB，其中新增 {report.appended_pages ?? 0} 页），其余{" "}
                {report.pages - report.changed_pages} 页直接沿用原密文
                {report.write_ms != null ? ` · ${report.write_ms} ms` : ""}
              </span>
            ) : null}
            <span className="block break-all font-mono text-xs">备份：{report.backup_dir}</span>
            <span className="block text-xs">
              库内现有会话：{report.verified_sessions}
              {report.relaunched ? " · 已自动重启客户端" : " · 请手动启动客户端查看"}
            </span>
          </AlertDescription>
        </Alert>
      ) : null}

      {/* 确认 + 进度 */}
      <Dialog open={confirmOpen} onOpenChange={(v) => !busy && setConfirmOpen(v)}>
        <DialogContent className="max-h-[86vh] max-w-2xl grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <HardDriveDownload className="size-4" />
              WorkBuddy → Trae
            </DialogTitle>
            <DialogDescription className="break-words">
              导入期间会先退出目标 Trae 客户端，写入完成后自动重启。原始 WorkBuddy 记录只读、不会改动；
              目标库会先整份备份。
            </DialogDescription>
          </DialogHeader>

          <div className="min-h-0 space-y-3 overflow-y-auto pr-1 text-sm">
            <div className="rounded-lg bg-muted/50 p-3">
              <div className="text-xs text-muted-foreground">写入目标</div>
              <div className="mt-0.5">
                {target?.client_label ?? "—"}
                <span className="text-muted-foreground"> · {target?.label ?? "未选择账号"}</span>
              </div>
            </div>

            {previewBusy ? (
              <div className="flex items-center gap-2 text-muted-foreground">
                <Loader2 className="size-4 animate-spin" />
                正在预览转换结果…
              </div>
            ) : preview ? (
              <>
                <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                  {[
                    ["会话", String(preview.length)],
                    ["回合", String(previewTotal.turns)],
                    ["工具步骤", String(previewTotal.steps)],
                    [
                      "正文体积",
                      humanSize(
                        selectedSessions.reduce((acc, s) => acc + (s.body_bytes || 0), 0),
                      ),
                    ],
                  ].map(([k, v]) => (
                    <div key={k} className="rounded-lg bg-muted/50 p-2.5">
                      <div className="text-xs text-muted-foreground">{k}</div>
                      <div className="font-medium tabular-nums">{v}</div>
                    </div>
                  ))}
                </div>
                <div className="max-h-56 space-y-1 overflow-auto rounded-lg border p-2">
                  {preview.map((p) => (
                    <div key={p.session_id} className="rounded-md px-2 py-1.5">
                      <div className="truncate font-medium">
                        {p.ai_title || titleOf.get(p.session_id) || p.session_id}
                      </div>
                      <div className="truncate text-xs text-muted-foreground">
                        {p.turns} 回合 / {p.tool_steps} 工具步骤
                      </div>
                      <div className="truncate font-mono text-[11px] text-muted-foreground/80">
                        {p.session_id}
                      </div>
                    </div>
                  ))}
                </div>
              </>
            ) : null}

            {progress.length ? (
              <pre className="max-h-40 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
                {progress.join("\n")}
              </pre>
            ) : null}

            {hiddenSelected.length ? (
              <div className="rounded-lg bg-amber-500/10 p-2.5 text-xs leading-5 text-amber-700">
                另有 {hiddenSelected.length} 个已选会话被列表的筛选条件藏着，本次同样会写入。
              </div>
            ) : null}

            {inspect?.running ? (
              <Alert>
                <AlertTriangle className="size-4" />
                <AlertDescription className="leading-5">
                  {inspect.client_label} 正在运行，开始导入后它会被关闭（写完自动重新打开）。请先保存好正在编辑的内容。
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
              {busy ? "导入中…" : "确认导入"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function SessionRow({
  session,
  owner,
  checked,
  disabled,
  onToggle,
}: {
  session: TraeWorkbuddySession;
  /** 归属账号的显示名（`昵称（uid …97eac1）`）。 */
  owner: string;
  checked: boolean;
  disabled: boolean;
  onToggle: () => void;
}) {
  const dup = Boolean(session.dup_group);
  // 重复组里的旧副本整体压暗一档，让「最新」那份一眼看见
  const dimmed = disabled || (dup && !session.is_newest);
  return (
    <label
      className={cn(
        "flex items-start gap-2.5 rounded-md px-2 py-1.5 transition-colors",
        dimmed ? "opacity-60" : "cursor-pointer hover:bg-foreground/[0.03]",
        !disabled && "cursor-pointer",
      )}
    >
      <Checkbox
        className="mt-0.5"
        checked={checked}
        disabled={disabled}
        onCheckedChange={() => onToggle()}
      />
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-center gap-1.5">
          <span className="truncate font-medium">{session.title}</span>
          {dup && session.is_newest ? (
            <Badge className="bg-emerald-500/15 text-[10px] text-emerald-600">最新</Badge>
          ) : null}
          {dup && !session.is_newest ? (
            <Badge variant="secondary" className="text-[10px]">
              较旧
            </Badge>
          ) : null}
          {session.deleted ? (
            <Badge variant="secondary" className="text-[10px]">
              已删除
            </Badge>
          ) : null}
          {!session.has_body ? (
            <Badge variant="outline" className="text-[10px]">
              无正文
            </Badge>
          ) : null}
        </span>
        <span className="mt-0.5 block truncate text-xs text-muted-foreground">
          {/* 相对时间在前：重复副本的绝对时间只差一两分钟，看不出谁靠后 */}
          {relativeTime(session.updated_at)}（{formatTime(session.updated_at)}） · {owner} ·{" "}
          {humanSize(session.body_bytes)}
          {session.body_lines > 0 ? ` · ${session.body_lines} 条记录` : ""}
          {session.model ? ` · ${session.model}` : ""}
        </span>
        <span className="block truncate font-mono text-[11px] text-muted-foreground/80">
          {session.cwd || "(无工作目录)"}
        </span>
        {session.dup_note ? (
          <span
            className={cn(
              "mt-0.5 block truncate text-[11px]",
              dup && !session.is_newest ? "text-amber-600" : "text-muted-foreground/80",
            )}
          >
            {session.dup_group}
            {dup ? "｜" : ""}
            {session.dup_note}
          </span>
        ) : null}
      </span>
    </label>
  );
}
