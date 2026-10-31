import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  ArrowRight,
  CheckCircle2,
  Database,
  FolderOpen,
  HardDriveUpload,
  KeyRound,
  Layers,
  LayoutList,
  Loader2,
  Pencil,
  RefreshCw,
  ScanSearch,
  Search,
  Users,
  X,
} from "lucide-react";
import { listen } from "@tauri-apps/api/event";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { TraeSessionEditor } from "@/components/trae-session-editor";
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
import { Skeleton } from "@/components/ui/skeleton";
import { Input } from "@/components/ui/input";
import { TraeClientSwitcher, type TraeClientOption } from "@/components/trae-client-switcher";
import { cn } from "@/lib/utils";
import * as api from "@/lib/api";
import { resolveSelectableKey } from "@/lib/trae-client-list";
import type {
  CrossWriteTarget,
  TraeInstalledClient,
  TraeWbExportReport,
  TraeWbPreview,
  TraeWbSourceSession,
  TraeWbTarget,
} from "@/lib/trae-types";
import { crossKeyOf } from "@/lib/trae-types";

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
  /**
   * 账号 / 工作区 / 分组（与「WorkBuddy → Trae」导入页、会话管理页同一套交互）。
   *
   * 这里的「账号」是**源侧 Trae 账号**（`owner_uid`）：同一个 Trae 客户端下登录过多个账号时，
   * 会话是混在一起的；导出到 WorkBuddy 只写目标那个号，所以先按账号看清再勾。
   */
  const [acctView, setAcctView] = useState("");
  /** 工作区筛选：键用 `workspace_key`（就是正文落盘的 `projects/{key}/` 目录名）。 */
  const [wsView, setWsView] = useState("");
  const [groupBy, setGroupBy] = useState<"account" | "workspace" | "flat">("account");

  const [uid, setUid] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [busyLabel, setBusyLabel] = useState("");
  const [progress, setProgress] = useState<string[]>([]);
  const [report, setReport] = useState<TraeWbExportReport | null>(null);

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [preview, setPreview] = useState<TraeWbPreview | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  // T46 · S5：导出前先改一改。这里列的是 **Trae** 会话，所以开 Trae 侧编辑器
  // （WorkBuddy 侧的编辑器在「WorkBuddy 会话记录」与「WorkBuddy → Trae」两处）。
  const [editSeed, setEditSeed] = useState<string | null>(null);

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
      // ⚠️ 顺序**已经由后端排好**（使用记忆 + 能不能跑起来，降序）⇒ 取可见列表第一个。
      //    前端别再自己写一套「trae-cn 优先」的规则，否则各页面顺序会不一致。
      //    ⚠️ 必须走 `resolveSelectableKey`：记忆里可能还留着已被摘掉的客户端
      //    （合并后的 `solo-cn`），写 `prev ?? first` 会把它一直用下去。
      setClientKey((prev) => resolveSelectableKey(prev, clients));
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

  /** 工作区展示名：优先用后端 `workspace_of()` 给的，旧后端缺字段时退回目录名。 */
  const wsLabelOf = useCallback((s: TraeWbSourceSession) => {
    if (s.workspace) return s.workspace;
    const key = s.workspace_key || "";
    if (!key) return "（未归属）";
    return key;
  }, []);

  /** 账号分布（数量与列表同源）：最近活跃的排前面。 */
  const acctOptions = useMemo(() => {
    const map = new Map<string, { uid: string; label: string; count: number; last: number }>();
    for (const s of sessions ?? []) {
      const cur =
        map.get(s.owner_uid) ?? { uid: s.owner_uid, label: s.owner_label, count: 0, last: 0 };
      cur.count += 1;
      cur.last = Math.max(cur.last, s.updated_ms || 0);
      map.set(s.owner_uid, cur);
    }
    return [...map.values()].sort((a, b) => b.last - a.last || a.label.localeCompare(b.label));
  }, [sessions]);

  /** 账号筛选后的范围（工作区下拉从这里算，所以换账号时空间列表跟着变）。 */
  const scopeSessions = useMemo(
    () => (sessions ?? []).filter((s) => !acctView || s.owner_uid === acctView),
    [sessions, acctView],
  );

  /** 工作区分布（当前范围内）：最近活跃的排前面。 */
  const wsOptions = useMemo(() => {
    const map = new Map<
      string,
      { key: string; label: string; path: string; count: number; last: number }
    >();
    for (const s of scopeSessions) {
      const key = s.workspace_key ?? "";
      const cur = map.get(key);
      if (cur) {
        cur.count += 1;
        cur.last = Math.max(cur.last, s.updated_ms || 0);
      } else {
        map.set(key, {
          key,
          label: wsLabelOf(s),
          path: (s.workspace_path ?? s.cwd).replace(/\\/g, "/"),
          count: 1,
          last: s.updated_ms || 0,
        });
      }
    }
    return [...map.values()].sort((a, b) => b.last - a.last || a.label.localeCompare(b.label));
  }, [scopeSessions, wsLabelOf]);

  const shown = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return scopeSessions
      .filter((s) => {
        if (wsView && (s.workspace_key ?? "") !== wsView) return false;
        if (!q) return true;
        return [s.title, s.cwd, s.id, s.owner_label, s.workspace, s.workspace_key].some((v) =>
          (v ?? "").toLowerCase().includes(q),
        );
      })
      .slice()
      .sort((a, b) => (b.updated_ms || 0) - (a.updated_ms || 0));
  }, [scopeSessions, wsView, filter]);

  /** 列表要渲染的「块」：按账号 / 按工作区 / 平铺（勾选集合始终是全局的，不随分组变化）。 */
  const groups = useMemo(() => {
    type Block = {
      key: string;
      label: string;
      sub: string;
      count: number;
      items: TraeWbSourceSession[];
      filter: { kind: "account" | "workspace"; value: string } | null;
    };
    if (groupBy === "flat") {
      return [
        { key: "__flat__", label: "", sub: "", count: shown.length, items: shown, filter: null },
      ] satisfies Block[];
    }
    const map = new Map<string, Block>();
    for (const s of shown) {
      if (groupBy === "account") {
        const uid = s.owner_uid;
        const cur = map.get(uid);
        if (cur) cur.items.push(s);
        else
          map.set(uid, {
            key: uid,
            label: s.owner_label,
            sub: "",
            count: 0,
            items: [s],
            filter: { kind: "account", value: uid },
          });
      } else {
        const key = s.workspace_key ?? "";
        const cur = map.get(key);
        if (cur) cur.items.push(s);
        else
          map.set(key, {
            key,
            label: wsLabelOf(s),
            // 副标题给落盘目录名 —— 这才是它在 WorkBuddy 里真正的工作区
            sub: key || "（正文落 projects/ 根）",
            count: 0,
            items: [s],
            filter: { kind: "workspace", value: key },
          });
      }
    }
    const blocks = [...map.values()];
    for (const b of blocks) b.count = b.items.length;
    const order =
      groupBy === "account"
        ? new Map(acctOptions.map((a, i) => [a.uid, i]))
        : new Map(wsOptions.map((w, i) => [w.key, i]));
    blocks.sort(
      (a, b) =>
        (order.get(a.key) ?? 999) - (order.get(b.key) ?? 999) || a.label.localeCompare(b.label),
    );
    return blocks;
  }, [groupBy, shown, acctOptions, wsOptions, wsLabelOf]);

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
    // 只作用于**当前可见**的行；已勾选但被筛掉的行保持原样（操作条会说清有几个）。
    setSelected((prev) => {
      const allShown = shown.length > 0 && shown.every((s) => prev.has(s.id));
      const next = new Set(prev);
      for (const s of shown) {
        if (allShown) next.delete(s.id);
        else next.add(s.id);
      }
      return next;
    });
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

  // ⚠️ `target?.accounts ?? []` 每次渲染都是新数组 ⇒ `crossWrite` 的 useMemo 会白算，
  //    而且会让编辑器的目标账号 effect 每渲染都跑一遍。这里钉住引用。
  const accounts = useMemo(() => target?.accounts ?? [], [target]);

  /**
   * T46 · S5：编辑器页脚的**跨客户端**写入目标 = **WorkBuddy** 的账号（不是 Trae 的）。
   *
   * 这个页面要的是把 Trae 会话导出到 WorkBuddy；摆一份 Trae 账号清单在这儿等于把用户绕回去
   * （用户报的「怎么还是 trae 到 trae」）。
   */
  const crossWrite = useMemo<CrossWriteTarget>(
    () => ({
      toLabel: "WorkBuddy",
      // WorkBuddy 只有一个客户端（`clientKey` 用空串），所以编辑器页脚不会出现「选客户端」那一行。
      accounts: accounts.map((a) => ({
        uid: a.uid,
        clientKey: "",
        clientLabel: "WorkBuddy",
        label: a.label,
      })),
      accountsError: null,
      blocked: !target?.available
        ? "本机没有 WorkBuddy 数据（先安装并登录一次 WorkBuddy）"
        : accounts.length === 0
          ? "本机没有可写入的 WorkBuddy 账号"
          : null,
      defaultKey: uid ? crossKeyOf({ clientKey: "", uid }) : null,
      push: async (acc) => {
        if (!clientKey || !editSeed) throw new Error("没有选中要搬走的会话");
        const r = await api.traeWbExportRun(clientKey, acc.uid, [editSeed]);
        return `${r.turns} 个回合 / ${r.tool_steps} 个工具步骤 → 账号 ${r.target_label}${
          r.relaunched ? " · 已自动重启 WorkBuddy" : ""
        }`;
      },
    }),
    [accounts, target?.available, uid, clientKey, editSeed],
  );

  /**
   * 客户端切换条的入参：本页只做一次字段名翻译（`has_login` → `hasLogin`、
   * `usage.topPick` → `top`），顺序与徽标规则全在 `TraeClientSwitcher` 里（T38/T39）。
   */
  const clientOptions = useMemo<TraeClientOption[]>(
    () =>
      clients.map((c) => ({
        key: c.key,
        label: c.label,
        installed: c.installed,
        hasLogin: c.has_login,
        // 合并后的客户端（如 SOLO → Trae CN）要戴「已并入」徽标。
        mergedInto: c.merged_into,
        runnable: c.runnable,
        top: c.key === usageTopPick,
      })),
    [clients, usageTopPick],
  );
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
                  ? `共 ${sessions.length} 个会话${
                      acctView || wsView || filter ? `，筛选出 ${shown.length} 条` : ""
                    }；已选 ${selectedSessions.length} 个`
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
          {/* 客户端选择：与首页 / 会话记录 / WorkBuddy → Trae 共用同一个组件——
              顺序、「常用」徽标、未安装置灰只有那一处实现，两个迁移方向因此长得一样。 */}
          <TraeClientSwitcher
            clients={clientOptions}
            value={clientKey}
            onChange={setClientKey}
            disabled={busy}
            label="Trae 客户端（导出源）"
          />

          {/* 筛选条一：账号 → 工作区 → 搜索（数量与列表同源，都从当前 `sessions` 数出来） */}
          <div className="flex flex-wrap items-center gap-2">
            <select
              className="h-8 min-w-[170px] max-w-[240px] rounded-md border border-input bg-background px-2 text-xs"
              value={acctView}
              onChange={(e) => {
                setAcctView(e.target.value);
                // 换账号 = 换一批工作区，旧的工作区键在这里没有意义。
                setWsView("");
              }}
              title="按 Trae 账号筛选（会话的归属 uid）"
            >
              <option value="">全部账号（{sessions?.length ?? 0}）</option>
              {acctOptions.map((a) => (
                <option key={a.uid} value={a.uid}>
                  {a.label} · {a.count}
                </option>
              ))}
            </select>

            <select
              className="h-8 min-w-[160px] max-w-[220px] rounded-md border border-input bg-background px-2 text-xs"
              value={wsView}
              onChange={(e) => setWsView(e.target.value)}
              title="按工作区筛选（= 正文落盘目录 projects/{key}）"
            >
              <option value="">全部工作区（{wsOptions.length}）</option>
              {wsOptions.map((w) => (
                <option key={w.key} value={w.key}>
                  {w.label} · {w.count}
                </option>
              ))}
            </select>

            <div className="relative min-w-48 flex-1">
              <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="搜索标题 / 工作目录 / 账号 / 会话 id"
                className="h-8 pl-7 pr-7 text-xs"
              />
              {filter ? (
                <button
                  type="button"
                  title="清除搜索"
                  className="absolute top-1/2 right-2 -translate-y-1/2 text-muted-foreground"
                  onClick={() => setFilter("")}
                >
                  <X className="size-3.5" />
                </button>
              ) : null}
            </div>
          </div>

          {/* 筛选条二：分组视图 + 同源计数 */}
          <div className="flex flex-wrap items-center gap-2">
            <div className="flex items-center gap-0.5 rounded-md border p-0.5">
              {(
                [
                  { key: "account", label: "按账号", icon: Users },
                  { key: "workspace", label: "按工作区", icon: Layers },
                  { key: "flat", label: "平铺", icon: LayoutList },
                ] as const
              ).map((v) => (
                <button
                  key={v.key}
                  type="button"
                  className={cn(
                    "flex items-center gap-1 rounded px-2 py-0.5 text-xs",
                    groupBy === v.key
                      ? "bg-muted font-medium text-foreground"
                      : "text-muted-foreground hover:bg-muted/60",
                  )}
                  onClick={() => setGroupBy(v.key)}
                >
                  <v.icon className="size-3" />
                  {v.label}
                </button>
              ))}
            </div>
            {(acctView || wsView || filter) && (
              <Button
                size="sm"
                variant="ghost"
                className="h-7 text-xs"
                onClick={() => {
                  setAcctView("");
                  setWsView("");
                  setFilter("");
                }}
              >
                清除筛选
              </Button>
            )}
            <Badge variant="secondary" className="ml-auto">
              {shown.length === scopeSessions.length
                ? `${shown.length} 条`
                : `${shown.length} / ${scopeSessions.length} 条`}
            </Badge>
            {acctOptions.length > 1 ? (
              <Badge variant="outline">{acctOptions.length} 个账号</Badge>
            ) : null}
            {wsOptions.length > 0 ? (
              <Badge variant="outline">{wsOptions.length} 个工作区</Badge>
            ) : null}
          </div>

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
              {sessions
                ? `没有匹配的会话 —— 筛选后 0 条，该客户端共 ${sessions.length} 条。`
                : "该客户端暂无可导出的会话。"}
            </p>
          ) : (
            <div className="max-h-[min(62vh,34rem)] overflow-auto rounded-lg border">
              <table className="w-full text-sm">
                <thead className="sticky top-0 z-10 border-b bg-muted/60 text-xs text-muted-foreground backdrop-blur">
                  <tr>
                    <th className="w-10 px-3 py-2">
                      <input
                        type="checkbox"
                        className="size-4 align-middle"
                        title="全选当前可见的会话"
                        checked={shown.length > 0 && shown.every((s) => selected.has(s.id))}
                        onChange={toggleAll}
                      />
                    </th>
                    <th className="px-3 py-2 text-left font-medium">会话</th>
                    <th className="px-3 py-2 text-left font-medium">工作区</th>
                    <th className="px-3 py-2 text-left font-medium">账号</th>
                    <th className="w-12 px-3 py-2" />
                  </tr>
                </thead>
                <tbody>
                  {groups.map((g) => (
                    <Fragment key={g.key}>
                      {groupBy !== "flat" ? (
                        <tr className="bg-muted/50">
                          <td colSpan={5} className="px-3 py-1.5">
                            <button
                              type="button"
                              className="flex w-full items-center gap-2 text-left"
                              title="只看这一组"
                              onClick={() => {
                                if (!g.filter) return;
                                if (g.filter.kind === "account") {
                                  setAcctView((prev) =>
                                    prev === g.filter!.value ? "" : g.filter!.value,
                                  );
                                  setWsView("");
                                } else {
                                  setWsView((prev) =>
                                    prev === g.filter!.value ? "" : g.filter!.value,
                                  );
                                }
                              }}
                            >
                              {groupBy === "account" ? (
                                <Users className="size-3.5 shrink-0 text-muted-foreground" />
                              ) : (
                                <FolderOpen className="size-3.5 shrink-0 text-muted-foreground" />
                              )}
                              <span className="shrink-0 text-xs font-semibold">{g.label}</span>
                              <Badge variant="secondary" className="shrink-0">
                                {g.count}
                              </Badge>
                              {g.sub ? (
                                <span className="truncate font-mono text-[11px] text-muted-foreground">
                                  {g.sub}
                                </span>
                              ) : null}
                            </button>
                          </td>
                        </tr>
                      ) : null}
                      {g.items.map((s) => (
                        <tr
                          key={s.id}
                          className={cn(
                            "border-b transition-colors last:border-0",
                            busy ? "opacity-50" : "hover:bg-muted/30",
                          )}
                        >
                          <td className="px-3 py-2 align-top">
                            <input
                              type="checkbox"
                              className="size-4"
                              checked={selected.has(s.id)}
                              disabled={busy}
                              onChange={() => toggle(s.id)}
                              aria-label={`选择 ${s.title}`}
                            />
                          </td>
                          <td className="max-w-[420px] px-3 py-2 align-top">
                            <div className="truncate font-medium">{s.title}</div>
                            <div className="mt-0.5 truncate text-xs text-muted-foreground">
                              {formatTime(s.updated_ms)} · {s.turns} 轮
                            </div>
                          </td>
                          <td className="max-w-[200px] px-3 py-2 align-top">
                            <div
                              className="truncate text-xs text-muted-foreground"
                              title={(s.workspace_path ?? s.cwd).replace(/\\/g, "/")}
                            >
                              {wsLabelOf(s)}
                            </div>
                            <div className="truncate font-mono text-[11px] text-muted-foreground/70">
                              {s.workspace_key || "（根目录）"}
                            </div>
                          </td>
                          <td className="max-w-[180px] px-3 py-2 align-top">
                            <div className="truncate text-xs text-muted-foreground" title={s.owner_label}>
                              {s.owner_label}
                            </div>
                          </td>
                          <td className="px-3 py-2 align-top">
                            <button
                              type="button"
                              title="编辑这条 Trae 会话（导出前可先改）"
                              aria-label="编辑这条 Trae 会话"
                              className="text-muted-foreground hover:text-foreground shrink-0 rounded-sm p-1 disabled:pointer-events-none disabled:opacity-40"
                              disabled={busy || !clientKey}
                              onClick={() => setEditSeed(s.id)}
                            >
                              <Pencil className="size-3.5" />
                            </button>
                          </td>
                        </tr>
                      ))}
                    </Fragment>
                  ))}
                </tbody>
              </table>
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

      {/* T46 · S5：Trae → WorkBuddy 导出前，就地改这条 Trae 会话；页脚直接写入到 **WorkBuddy** 账号。 */}
      {clientKey ? (
        <TraeSessionEditor
          open={editSeed !== null}
          onOpenChange={(o) => {
            if (!o) setEditSeed(null);
          }}
          clientKey={clientKey}
          clientLabel={clients.find((c) => c.key === clientKey)?.label ?? clientKey}
          sessionId={editSeed}
          onChanged={() => {
            if (clientKey) void loadSessions(clientKey);
          }}
          cross={crossWrite}
        />
      ) : null}
    </div>
  );
}
