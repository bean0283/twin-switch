import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  Archive,
  ArrowLeft,
  ArrowRight,
  ChevronDown,
  ChevronRight,
  Copy,
  Database,
  FileDown,
  FolderOpen,
  HardDrive,
  Import,
  KeyRound,
  Layers,
  LayoutList,
  Link2Off,
  Loader2,
  Pencil,
  RefreshCw,
  ScanSearch,
  Search,
  Trash2,
  Wrench,
  X,
} from "lucide-react";
import { listen } from "@tauri-apps/api/event";

import { TraeClientSwitcher, type TraeClientOption } from "@/components/trae-client-switcher";
import { TraeSessionEditor } from "@/components/trae-session-editor";
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
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { ResizableDialogContent } from "@/components/ui/resizable-dialog-content";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import * as api from "@/lib/api";
import { resolveSelectableKey } from "@/lib/trae-client-list";
import { defaultTargetClient, targetClientOptions } from "@/lib/trae-import-target";
import { readStored, STORAGE_PREFIX, writeStored } from "@/lib/storage-keys";
import { cn } from "@/lib/utils";
import type {
  TraeBatchDeleteReport,
  TraeCloudDeleteInfo,
  TraeDecryptedStatus,
  TraeDeleteInfo,
  TraeImportCandidate,
  TraeImportInspect,
  TraeImportReport,
  TraeInstalledClient,
  TraeLinkGroup,
  TraeLinksPreview,
  TraeSessionInfo,
} from "@/lib/trae-types";

/** 关联组状态 → 文案与配色（与 WorkBuddy 侧保持一致，改一处即两端同步）。 */
const VERDICT: Record<string, { label: string; tone: string }> = {
  linked: { label: "两端都在", tone: "bg-emerald-500/10 text-emerald-600 border-emerald-500/30" },
  targetMissing: { label: "目标缺副本", tone: "bg-amber-500/10 text-amber-600 border-amber-500/30" },
  sourceMissing: { label: "源已失效", tone: "bg-muted text-muted-foreground" },
  gone: { label: "两端都不在", tone: "bg-muted text-muted-foreground" },
};

/**
 * 分叉状态展示：复制之后两端各自继续使用，内容会分叉。
 * `none` / `unknown` 不给徽章（前者无信息量，后者是读不到、不该暗示「可同步」）。
 */
const DIVERGENCE: Record<string, { label: string; tone: string; hint: string }> = {
  sourceAhead: {
    label: "源更新",
    tone: "bg-sky-500/10 text-sky-600 border-sky-500/30",
    hint: "源账号那份更新，可同步给目标账号",
  },
  targetAhead: {
    label: "目标更新",
    tone: "bg-violet-500/10 text-violet-600 border-violet-500/30",
    hint: "目标账号那份更新，可同步回源账号",
  },
};

/** 「副本已分叉」提醒的「不再提示」记忆键（值 `"1"` = 静音）。 */
const SK_DIVERGENCE_MUTED = `${STORAGE_PREFIX}.trae-divergence-muted`;

/** 长 id 的缩写（仅用于展示会话 id；账号名一律走 owner_label）。 */
function shortId(id: string): string {
  return id.length > 10 ? `${id.slice(0, 8)}…` : id;
}

export default function TraeRecordsPage() {
  const [clients, setClients] = useState<TraeInstalledClient[]>([]);
  /** 排在最前且**确有使用历史**的客户端（全 0 分时是 null）——只用来打「常用」徽标。 */
  const [usageTopPick, setUsageTopPick] = useState<string | null>(null);
  const [clientKey, setClientKey] = useState<string | null>(null);
  const [status, setStatus] = useState<TraeDecryptedStatus | null>(null);
  const [sessions, setSessions] = useState<TraeSessionInfo[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [busyLabel, setBusyLabel] = useState("");
  const [decryptProgress, setDecryptProgress] = useState<string[]>([]);
  // 快照无法与实时库对齐时的提示（例如没有已存密钥）
  const [decryptHint, setDecryptHint] = useState("");
  // 归属账号筛选（会话列表）
  const [ownerFilter, setOwnerFilter] = useState("");
  // 搜索 / 工作空间（与 WorkBuddy 会话页同一套交互）：
  // `wsFilter === ""` = 全部空间；`byWorkspace` = 按空间分组视图（默认开，和客户端列表一致）。
  const [query, setQuery] = useState("");
  const [wsFilter, setWsFilter] = useState("");
  const [byWorkspace, setByWorkspace] = useState(true);
  // 导出目录（默认路径提示 + 打开按钮）
  const [exportDir, setExportDir] = useState("");
  // 解密库的表清单默认收起（~180 张表的标签墙会占掉半屏）
  const [tablesOpen, setTablesOpen] = useState(false);
  const tableRows = useMemo(
    () => (status?.tables ?? []).reduce((sum, t) => sum + (t.count ?? 0), 0),
    [status],
  );

  // 删除 对话框
  // T46 会话编辑器：`editSeed` = 要点编辑的会话（列表行上点「编辑」时带过来）。
  const [editOpen, setEditOpen] = useState(false);
  const [editSeed, setEditSeed] = useState<string | null>(null);
  /**
   * 待删除的会话。
   *
   * 1 条 = 单条删除，先走 `trae_delete_info` 逐表预检；
   * 多条 = 批量删除，整批只做**一趟**（一次解密、一次增量回写、**一份**整库备份），
   * 所以不做逐条预检，改为在弹窗里汇总说明。
   */
  const [deleteTargets, setDeleteTargets] = useState<TraeSessionInfo[]>([]);
  const [deleteInfo, setDeleteInfo] = useState<TraeDeleteInfo | null>(null);
  const [deleteProgress, setDeleteProgress] = useState<string[]>([]);
  const [batchReport, setBatchReport] = useState<TraeBatchDeleteReport | null>(null);

  /**
   * 会话列表的勾选（批量删除用）。
   *
   * 不叫 `selected`：本页已有的 `selected` 是「当前选中的客户端」，同名会撞。
   */
  const [recordSel, setRecordSel] = useState<Set<string>>(new Set());

  // 跨账号导入
  const [importTarget, setImportTarget] = useState<TraeSessionInfo | null>(null);
  const [importCandidates, setImportCandidates] = useState<TraeImportCandidate[]>([]);
  const [importHints, setImportHints] = useState<string[]>([]);
  const [importDstClient, setImportDstClient] = useState<string | null>(null);
  const [importDst, setImportDst] = useState<string | null>(null);
  const [importUid, setImportUid] = useState<string | null>(null);
  const [importInspect, setImportInspect] = useState<TraeImportInspect | null>(null);
  const [importProgress, setImportProgress] = useState<string[]>([]);
  const [importBusy, setImportBusy] = useState(false);
  const [importReport, setImportReport] = useState<TraeImportReport | null>(null);

  // 跨账号关联（同客户端、两个账号之间的会话副本关系）
  const [linkSrcUid, setLinkSrcUid] = useState<string>("");
  const [linkDstUid, setLinkDstUid] = useState<string>("");
  const [links, setLinks] = useState<TraeLinksPreview | null>(null);
  const [linksLoading, setLinksLoading] = useState(false);

  // 「复制与关联」页签里的批量会话复制（同客户端跨账号）。
  // 与 WorkBuddy 侧同构：源账号勾选会话 → 以新 id 复制给目标账号，源账号一行不动。
  const [copySel, setCopySel] = useState<Set<string>>(new Set());
  const [copyBusy, setCopyBusy] = useState(false);
  const [copyReport, setCopyReport] = useState<TraeImportReport | null>(null);
  const [copyProgress, setCopyProgress] = useState<string[]>([]);

  // 工程归属自愈（历史副本遗留的 session_project.project_id 不一致）
  const [healBusy, setHealBusy] = useState<"check" | "heal" | null>(null);
  const [healReport, setHealReport] = useState<string>("");

  // 差异同步：把关联组里较新一端的内容就地覆盖到较旧一端（写库，会重启客户端）
  const [syncBusy, setSyncBusy] = useState<string | null>(null);

  // 进入「复制与关联」时，若有已分叉的副本，弹一次提醒（可勾选不再提示）。
  const [divergencePrompt, setDivergencePrompt] = useState<
    { groups: TraeLinkGroup[] } | null
  >(null);
  const [divergencePromptMuted, setDivergencePromptMuted] = useState<boolean>(
    () => readStored(SK_DIVERGENCE_MUTED) === "1",
  );
  // 每次进页面只提醒一次（同一轮会话内不重复弹）。
  const divergencePrompted = useRef(false);

  useEffect(() => {
    // 同 workbuddy-import-card：`listen()` 异步，StrictMode 下首轮清理拿不到 un，
    // 会残留一个监听器把每条进度事件投递两遍。
    let active = true;
    let un: (() => void) | undefined;
    void listen<{ line: string }>("trae-import-progress", (e) => {
      // 同一个事件源同时喂单会话导入与批量复制两条进度；两边都收，各自只在运行中展示。
      setImportProgress((prev) => [...prev, e.payload.line]);
      setCopyProgress((prev) => [...prev, e.payload.line]);
    }).then((u) => {
      if (active) un = u;
      else u();
    });
    return () => {
      active = false;
      un?.();
    };
  }, []);

  async function openImport(s: TraeSessionInfo) {
    if (!clientKey) return;
    // ⚠️ 不能直接用「正在浏览的客户端」当目标：2026-10 官方把 TRAE SOLO 并进 Trae CN 之后，
    //    SOLO 已经没有自己的程序了，把会话导进它的库**不报错、也不会生效**
    //    （客户端永远不会去读那个目录）。落到承载方，见 `defaultTargetClient`。
    setImportDstClient(defaultTargetClient(clientKey, clients));
    setImportDst(null);
    setImportUid(null);
    setImportInspect(null);
    setImportProgress([]);
    setImportReport(null);
    setImportHints([]);
    setImportTarget(s);
    try {
      const { candidates, hints } = await api.traeImportCandidates(clientKey, s.id);
      setImportCandidates(candidates);
      setImportHints(hints);
      if (candidates.length === 1) {
        void inspectDst(candidates[0]);
      }
    } catch (cause) {
      toast.error("读取可导入目标失败", { description: api.asError(cause) });
    }
  }

  async function inspectDst(c: TraeImportCandidate) {
    setImportDst(`${c.client_key}::${c.account_id}`);
    setImportUid(c.uid);
    setImportInspect(null);
    try {
      setImportInspect(await api.traeImportInspect(c.client_key, c.account_id));
    } catch (cause) {
      toast.error("目标探测失败", { description: api.asError(cause) });
    }
  }

  async function confirmImport() {
    if (!clientKey || !importTarget || !importDst || !importUid || importBusy) return;
    setImportBusy(true);
    setImportProgress([]);
    setImportReport(null);
    try {
      const res = await api.traeImportRun(clientKey, importDst.split("::")[0], importUid, [importTarget.id]);
      setImportReport(res);
      toast.success(`已导入「${importTarget.title || "（无标题）"}」`, {
        description: `目标：${res.target_label} · 复制 ${res.copied_rows} 行 · 校验会话 ${res.verified_sessions} 个 · ${res.relaunched ? "已自动重启客户端" : "客户端未自动重启"}`,
      });
    } catch (cause) {
      toast.error("导入失败", { description: api.asError(cause) });
    } finally {
      setImportBusy(false);
    }
  }

  async function loadClients() {
    try {
      const { clients, usage } = await api.traeListClients();
      setClients(clients);
      setUsageTopPick(usage?.topPick ?? null);
      // ⚠️ 顺序**已经由后端排好**（使用记忆 + 能不能跑起来，降序）⇒ 取可见列表第一个。
      //    前端不要再自己写一套「trae-cn 优先」的规则：同一件事被两处推导必然不一致。
      //    ⚠️ 必须走 `resolveSelectableKey`：记忆里可能还留着已被摘掉的客户端
      //    （2026-10 SOLO 并入 Trae CN 后 `solo-cn` 的使用分仍最高，但它没有自己的程序、
      //    已不进标签条），写 `prev ?? first` 会把它一直用下去，选中态与标签条对不上。
      setClientKey((prev) => resolveSelectableKey(prev, clients));
    } catch (cause) {
      setError(api.asError(cause));
    }
  }

  useEffect(() => {
    void loadClients();
    api
      .traeExportDir()
      .then(setExportDir)
      .catch(() => setExportDir(""));
  }, []);

  async function refresh(key: string) {
    setLoading(true);
    setError(null);
    setDecryptHint("");
    try {
      // 快照与实时库一致时**零解密**直接复用；只有确实不一致（客户端写过且已 checkpoint）
      // 才重新解密。早期版本每次刷新都无条件整库解密 ~279 MB 并逐表 count(*)，
      // 那正是「会话记录加载很慢」的主因。
      let st = await api.traeDecryptedStatus(key);
      if (st.exists) {
        try {
          const res = await api.traeEnsureDecrypted(key);
          st = { ...st, tables: res.tables, pages: res.pages, current: true };
        } catch (cause) {
          // 无已存密钥 / 密钥过期：沿用现有明文快照（列表可能不是最新）
          setDecryptHint(api.asError(cause));
        }
        const { sessions } = await api.traeListSessions(key);
        setSessions(sessions);
      } else {
        setSessions(null);
      }
      setStatus(st);
    } catch (cause) {
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (clientKey) void refresh(clientKey);
  }, [clientKey]);

  async function runBusy(label: string, fn: () => Promise<void>) {
    setBusy(true);
    setBusyLabel(label);
    try {
      await fn();
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  async function onScanAndDecrypt() {
    if (!clientKey || busy) return;
    setDecryptProgress([]);
    await runBusy("扫描进程内存并解密", async () => {
      try {
        const res = await api.traeScanAndDecrypt(clientKey);
        setDecryptProgress(res.progress ?? []);
        const ok = res.report.hmac_ok;
        toast.success(ok ? "解密成功" : "密钥校验未通过", {
          description: `${res.report.pages} 页 · ${res.report.tables.length} 张表 · ${(res.report.elapsed_ms / 1000).toFixed(1)}s`,
        });
        await refresh(clientKey);
      } catch (cause) {
        toast.error("扫描并解密失败", { description: api.asError(cause) });
      }
    });
  }

  async function onDecryptSaved() {
    if (!clientKey || busy) return;
    await runBusy("使用已存密钥解密", async () => {
      try {
        const res = await api.traeDecryptWithSavedKey(clientKey);
        toast.success("解密成功", {
          description: `${res.report.pages} 页 · ${res.report.tables.length} 张表`,
        });
        await refresh(clientKey);
      } catch (cause) {
        toast.error("解密失败", { description: api.asError(cause) });
      }
    });
  }

  async function onExportAll() {
    if (!clientKey || busy) return;
    await runBusy("批量导出 ZIP", async () => {
      try {
        const res = await api.traeExportAll([clientKey]);
        toast.success(`已导出 ${res.ok}/${res.total} 个会话`, {
          description: res.filename,
          action: { label: "打开所在目录", onClick: () => void api.traeRevealPath(res.path) },
        });
      } catch (cause) {
        toast.error("批量导出失败", { description: api.asError(cause) });
      }
    });
  }

  async function onExportSession(s: TraeSessionInfo) {
    if (!clientKey || busy) return;
    await runBusy("导出 MD", async () => {
      try {
        const res = await api.traeExportSession(clientKey, s.id);
        toast.success("已导出会话", {
          description: `${res.filename}（${res.size_kb} KB）\n${res.path}`,
          action: { label: "打开所在目录", onClick: () => void api.traeRevealPath(res.path) },
        });
      } catch (cause) {
        toast.error("导出失败", { description: api.asError(cause) });
      }
    });
  }

  /** 回收工具自己的工作文件：旧整库备份 + 可再生的解密快照。 */
  async function onCleanup() {
    if (busy) return;
    await runBusy("清理工作文件", async () => {
      try {
        const res = await api.traeCleanupWorkingFiles();
        if (res.freed_mb <= 0) {
          toast.info("没有可清理的旧文件", {
            description: "备份只保留最新 1 批，已是精简状态。",
          });
          return;
        }
        toast.success(`已回收 ${res.freed_mb} MB`, {
          description: [
            ...res.details.map((d) => `${d.name}：-${d.freedMb} MB`),
            "解密快照已删除，需要查看会话时再点一次「扫描并解密」即可重建。",
          ].join("\n"),
        });
        // 快照被删了，刷新后列表会回到「未解密」状态（需重新解密）
        if (clientKey) await refresh(clientKey);
      } catch (cause) {
        toast.error("清理失败", { description: api.asError(cause) });
      }
    });
  }

  /**
   * 打开删除确认。
   *
   * 单条会先做一次逐表预检（读解密库，只读）；批量不做 —— N 条就是 N 次预检，
   * 而批量删除本来就是「一趟走完」，逐条预检既慢又给不出更有用的信息。
   */
  async function openDelete(targets: TraeSessionInfo[]) {
    if (!clientKey || targets.length === 0) return;
    setDeleteProgress([]);
    setDeleteTargets(targets);
    setDeleteInfo(null);
    if (targets.length !== 1) return;
    try {
      const info = await api.traeDeleteInfo(clientKey, targets[0].id);
      setDeleteInfo(info);
    } catch (cause) {
      toast.error("删除预检失败", { description: api.asError(cause) });
      setDeleteTargets([]);
    }
  }

  function closeDelete() {
    setDeleteTargets([]);
    setDeleteInfo(null);
  }

  async function confirmDelete() {
    if (!clientKey || deleteTargets.length === 0 || busy) return;
    const targets = deleteTargets;
    setBusy(true);
    try {
      if (targets.length === 1) {
        const res = await api.traeDeleteSession(clientKey, targets[0].id);
        setDeleteProgress((res.progress ?? []).map((l) => String(l)));
        const cloud = res.cloud as TraeCloudDeleteInfo | undefined;
        const relaunched = res.relaunched ? " · 已自动重启客户端" : " · 客户端未自动重启";
        if (cloud?.attempted && cloud.ok === false) {
          toast.warning(`已删除本地会话「${targets[0].title}」`, {
            description: `云端任务列表删除失败（不影响本地结果）：${cloud.error ?? "未知原因"}${relaunched}`,
          });
        } else {
          toast.success(`已彻底删除会话「${targets[0].title}」`, {
            description: `实时库与解密库已同步删除，文件已移入回收站目录（可恢复）。${relaunched}`,
          });
        }
      } else {
        const res = await api.traeDeleteSessions(
          clientKey,
          targets.map((t) => t.id),
        );
        setBatchReport(res);
        setDeleteProgress((res.progress ?? []).map((l) => String(l)));
        const cloudNote = res.cloud.deleted
          ? ` · 云端任务列表已清 ${res.cloud.deleted} 条`
          : "";
        const cloudFail = res.cloud.failed.length
          ? ` · 云端失败 ${res.cloud.failed.length} 条（不影响本地结果）`
          : "";
        toast.success(`已彻底删除 ${res.count} 个会话`, {
          description: `整批只做了一趟备份 / 解密 / 回写，文件已移入回收站（可恢复）${cloudNote}${cloudFail}`,
        });
      }
      closeDelete();
      setRecordSel(new Set());
      await refresh(clientKey);
    } catch (cause) {
      toast.error("删除失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  const selected = clients.find((c) => c.key === clientKey);

  /**
   * 切换条的入参。字段名在这里做一次「线上结构 → 组件结构」的翻译，
   * 免得共用组件被 `trae_list_clients` 的 snake_case 绑住。
   * 「常用」只认后端下发的那一个 key，前端不另算。
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

  const ownerOptions = sessions
    ? Array.from(new Set(sessions.map((s) => s.owner_label))).sort()
    : [];

  // ---------------------------------------------------------------------------
  // 工作空间：归并 / 搜索 / 分组（与 WorkBuddy 会话页同一套口径）
  // ---------------------------------------------------------------------------
  //
  // 空间键与展示名都由后端 `modules::workspace` 给（Trae 优先用 `project.name`），
  // 前端只按 `workspace_key` 分组、用 `workspace` 显示 —— 不重复实现路径归一化，
  // 否则同一目录在不同页面会被拆成两个空间。

  /** 当前客户端涉及的工作空间（最近活动的排前面）。 */
  const wsOptions = useMemo(() => {
    const map = new Map<
      string,
      { key: string; label: string; path: string; count: number; last: string }
    >();
    for (const s of sessions ?? []) {
      const key = s.workspace_key ?? "";
      // Trae 的时间是 `YYYY-MM-DD HH:mm:ss` 字符串，可以直接比字典序
      const at = s.updated || "";
      const cur = map.get(key);
      if (cur) {
        cur.count += 1;
        if (at > cur.last) cur.last = at;
      } else {
        map.set(key, {
          key,
          label: s.workspace || "（未归属）",
          path: s.workspace_path || "",
          count: 1,
          last: at,
        });
      }
    }
    const list = [...map.values()];
    // 同名空间（两个不同目录都叫 `bin`）补一级目录区分
    const dup = new Map<string, number>();
    for (const w of list) dup.set(w.label, (dup.get(w.label) ?? 0) + 1);
    return list
      .map((w) => {
        if ((dup.get(w.label) ?? 0) <= 1) return w;
        const segs = (w.path || "").split("/").filter(Boolean);
        return { ...w, label: segs.slice(-2).join("/") || w.label };
      })
      .sort((a, b) => b.last.localeCompare(a.last) || a.label.localeCompare(b.label));
  }, [sessions]);

  const visibleSessions = useMemo(() => {
    const q = query.trim().toLowerCase();
    return (sessions ?? []).filter((s) => {
      if (ownerFilter && s.owner_label !== ownerFilter) return false;
      if (wsFilter && (s.workspace_key ?? "") !== wsFilter) return false;
      if (!q) return true;
      return [s.title, s.workspace, s.workspace_path, s.id, s.owner_label].some((v) =>
        (v ?? "").toLowerCase().includes(q),
      );
    });
  }, [sessions, ownerFilter, wsFilter, query]);

  /** 按空间分组（分组顺序 = 组内第一条的时间，列表本来就按最后活动倒序）。 */
  const wsGroups = useMemo(() => {
    const map = new Map<
      string,
      { key: string; label: string; path: string; items: TraeSessionInfo[] }
    >();
    for (const s of visibleSessions) {
      const key = s.workspace_key ?? "";
      const g = map.get(key);
      if (g) g.items.push(s);
      else
        map.set(key, {
          key,
          label: s.workspace || "（未归属）",
          path: s.workspace_path || "",
          items: [s],
        });
    }
    const order = new Map(wsOptions.map((w, i) => [w.key, i]));
    return [...map.values()].sort(
      (a, b) => (order.get(a.key) ?? 999) - (order.get(b.key) ?? 999) || a.label.localeCompare(b.label),
    );
  }, [visibleSessions, wsOptions]);

  /** 表格要渲染的「块」：分组视图按空间分块；平铺视图合成一块。 */
  const renderGroups = byWorkspace
    ? wsGroups
    : [{ key: "__flat__", label: "", path: "", items: visibleSessions }];

  // ---------------------------------------------------------------------------
  // 批量删除的勾选
  // ---------------------------------------------------------------------------
  //
  // ⚠️ 勾选与「可见行」是两件事：账号筛选会把一部分勾中的行藏起来。
  //    所以：① 表头「全选」只作用于**当前可见**的行；② 切换筛选 / 切客户端一律清空勾选，
  //    否则用户会在看不见那些行的情况下把它们删掉。
  const pickedSessions = useMemo(
    () => (visibleSessions ?? []).filter((s) => recordSel.has(s.id)),
    [visibleSessions, recordSel],
  );
  const allVisiblePicked =
    (visibleSessions?.length ?? 0) > 0 && pickedSessions.length === visibleSessions?.length;

  function toggleRecordSel(id: string) {
    setRecordSel((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  /** 表头全选：未全选 ⇒ 选中当前可见的全部；已全选 ⇒ 清空。 */
  function toggleSelectAll() {
    setRecordSel(allVisiblePicked ? new Set() : new Set((visibleSessions ?? []).map((s) => s.id)));
  }

  // 切客户端 / 换筛选后清空勾选（理由见上）。依赖里放的是「会改变可见集合」的那两个。
  useEffect(() => {
    setRecordSel(new Set());
  }, [clientKey, ownerFilter, wsFilter, query]);

  // 列表刷新后把已经不存在的会话 id 从勾选里摘掉（例如在别处删过了）。
  useEffect(() => {
    if (!sessions) return;
    const alive = new Set(sessions.map((s) => s.id));
    setRecordSel((prev) => {
      const next = new Set([...prev].filter((id) => alive.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [sessions]);

  // 批量删除弹窗里的汇总：按归属账号分组 + 正文条数合计。
  const batchOwners = useMemo(() => {
    const m = new Map<string, number>();
    for (const s of deleteTargets) {
      const k = s.owner_label || "（无归属）";
      m.set(k, (m.get(k) ?? 0) + 1);
    }
    return [...m.entries()].sort((a, b) => b[1] - a[1]);
  }, [deleteTargets]);
  const batchMessages = deleteTargets.reduce((n, s) => n + (s.messages ?? 0), 0);

  /**
   * 关联用的账号列表：需要 **uid**（不是 label），因为关联表按 uid 记录。
   * 一个账号可能没有会话（或会话都无归属），那些空 uid 一律剔除——
   * 拿空 uid 去查关联只会得到空表，还不如不出现在下拉里。
   */
  const linkAccounts = useMemo(() => {
    const seen = new Map<string, string>();
    for (const s of sessions ?? []) {
      const uid = s.owner_uid?.trim();
      if (!uid) continue;
      if (!seen.has(uid)) seen.set(uid, s.owner_label || `uid ${uid.slice(0, 8)}…`);
    }
    return Array.from(seen, ([uid, label]) => ({ uid, label })).sort((a, b) =>
      a.label.localeCompare(b.label, "zh-CN"),
    );
  }, [sessions]);

  // 下拉默认值：有至少两个账号时自动选中前两个，省掉用户两次点击。
  useEffect(() => {
    if (linkAccounts.length === 0) return;
    setLinkSrcUid((prev) => (prev && linkAccounts.some((a) => a.uid === prev) ? prev : linkAccounts[0].uid));
    setLinkDstUid((prev) => {
      if (prev && linkAccounts.some((a) => a.uid === prev)) return prev;
      return (linkAccounts[1] ?? linkAccounts[0]).uid;
    });
  }, [linkAccounts]);

  const loadLinks = useCallback(async () => {
    if (!clientKey || !linkSrcUid || !linkDstUid) {
      setLinks(null);
      return;
    }
    if (linkSrcUid === linkDstUid) {
      setLinks(null);
      return;
    }
    setLinksLoading(true);
    try {
      const res = await api.traeSessionLinksPreview(clientKey, linkSrcUid, linkDstUid);
      setLinks(res);
    } catch (cause) {
      setLinks({ ok: false, error: api.asError(cause), sourceUid: linkSrcUid, targetUid: linkDstUid, count: 0, groups: [] });
    } finally {
      setLinksLoading(false);
    }
  }, [clientKey, linkSrcUid, linkDstUid]);

  // 切到「复制与关联」页签、或改了账号才去查——这是个读库操作，不该在每次
  // 会话列表刷新时顺手跑一遍（同 workbuddy 侧的做法）。
  const [tab, setTab] = useState("records");
  useEffect(() => {
    if (tab !== "links") return;
    void loadLinks();
  }, [tab, loadLinks]);

  // 关联结果回来后，若发现已分叉的副本就弹一次提醒。
  //
  // ⚠️ 只在**关联数据刚加载完**时判一次（`divergencePrompted` 挡住后续刷新重复弹）：
  // 用户可能只是点了「刷新」看一眼，不该每次都被弹窗打断。
  // 用户勾了「不再提示」则彻底静音。
  useEffect(() => {
    if (!links?.ok || divergencePrompted.current || divergencePromptMuted) return;
    const diverged = links.groups.filter(
      (g) => g.divergence === "sourceAhead" || g.divergence === "targetAhead",
    );
    if (diverged.length === 0) return;
    divergencePrompted.current = true;
    setDivergencePrompt({ groups: diverged });
  }, [links, divergencePromptMuted]);

  async function unlink(group: TraeLinkGroup) {
    try {
      const res = await api.traeSessionUnlink(group.groupId);
      toast.success("已解除关联", { description: `剩余 ${res.remaining} 组` });
      await loadLinks();
    } catch (cause) {
      toast.error("解除关联失败", { description: api.asError(cause) });
    }
  }

  /** 源账号名下的会话（列表已含全部账号，这里按 owner_uid 过滤）。 */
  const copySrcSessions = useMemo(
    () => (sessions ?? []).filter((s) => (s.owner_uid ?? "").trim() === linkSrcUid),
    [sessions, linkSrcUid],
  );

  function toggleCopy(id: string) {
    setCopySel((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  /**
   * 可复制的会话 id（「无正文」的复制过去也是空壳，不参与全选）。
   *
   * 用户反馈：几百条会话一条条点不动 —— 全选必须只选**真能复制**的那些，
   * 否则一次全选就会带上一堆必然跳过的空会话，选完还得手动剔。
   */
  const copyableIds = useMemo(
    () => copySrcSessions.filter((s) => (s.messages ?? 0) > 0).map((s) => s.id),
    [copySrcSessions],
  );
  const allCopyPicked = copyableIds.length > 0 && copyableIds.every((id) => copySel.has(id));
  const someCopyPicked = !allCopyPicked && copyableIds.some((id) => copySel.has(id));

  /** 全选 / 取消全选（只动可复制的那些，其它行的勾选状态保持原样）。 */
  function toggleAllCopy() {
    setCopySel((prev) => {
      if (allCopyPicked) {
        const next = new Set(prev);
        for (const id of copyableIds) next.delete(id);
        return next;
      }
      return new Set([...prev, ...copyableIds]);
    });
  }

  /**
   * 批量复制：同客户端跨账号 ⇒ src == dst == 当前 clientKey，只把目标账号 uid 传下去
   * （后端据此判定同库复制、生成新 id）。进度走 `trae-import-progress` 事件。
   */
  async function runCopy() {
    if (!clientKey || copySel.size === 0 || linkSrcUid === linkDstUid || copyBusy) return;
    setCopyBusy(true);
    setCopyProgress([]);
    setCopyReport(null);
    try {
      const res = await api.traeImportRun(clientKey, clientKey, linkDstUid, Array.from(copySel));
      setCopyReport(res);
      setCopySel(new Set());
      toast.success(`已复制 ${res.verified_sessions} 个会话`, {
        description: `目标：${res.target_label} · 写入 ${res.copied_rows} 行 · 登记关联 ${res.linked ?? 0} 组 · ${res.relaunched ? "已自动重启客户端" : "客户端未自动重启"}`,
      });
      await loadLinks();
    } catch (cause) {
      toast.error("复制失败", { description: api.asError(cause) });
    } finally {
      setCopyBusy(false);
    }
  }

  /** 只读检查：有多少会话的工程归属与关联表不一致。 */
  async function runHealCheck() {
    if (!clientKey || healBusy) return;
    setHealBusy("check");
    setHealReport("");
    try {
      const res = await api.traeFindMisalignedProjects(clientKey);
      if (res.count === 0) {
        setHealReport("检查完成：所有会话的工程归属都一致，无需修复。");
        toast.success("归属一致，无需修复");
      } else {
        setHealReport(
          `发现 ${res.count} 条归属不一致的会话（这些在 Trae 里可能删不掉、重启后复活）：` +
            res.sessions.map((s) => shortId(s.session_id)).join("、"),
        );
        toast.warning(`发现 ${res.count} 条归属不一致`);
      }
    } catch (cause) {
      setHealReport(`检查失败：${api.asError(cause)}`);
      toast.error("检查失败", { description: api.asError(cause) });
    } finally {
      setHealBusy(null);
    }
  }

  /** 一键修复：备份整库 → 对齐工程归属 → 增量回写 → 重启客户端。 */
  async function runHeal() {
    if (!clientKey || healBusy) return;
    setHealBusy("heal");
    setHealReport("");
    setCopyProgress([]);
    try {
      const res = await api.traeHealSessionProjects(clientKey);
      const msg =
        res.fixed_sessions === 0
          ? "检查完成：没有需要修复的会话。"
          : `已修复 ${res.fixed_sessions} 条会话的工程归属` +
            (res.fixed_contexts > 0 ? `、${res.fixed_contexts} 条会话上下文` : "") +
            `。${res.relaunched ? "客户端已自动重启。" : "请手动启动客户端查看。"}`;
      setHealReport(msg);
      if (res.fixed_sessions === 0) {
        toast.success("没有需要修复的会话");
      } else {
        toast.success("修复完成", { description: msg });
      }
      await refresh(clientKey);
    } catch (cause) {
      setHealReport(`修复失败：${api.asError(cause)}`);
      toast.error("修复失败", { description: api.asError(cause) });
    } finally {
      setHealBusy(null);
    }
  }

  /**
   * 同步差异：把较新一端的内容就地覆盖到较旧一端（**写库**，会重启客户端）。
   *
   * `direction` 指的是「以哪一端为准」：`sourceToTarget` = 把源的内容刷到目标，
   * `targetToSource` = 反过来。与界面上的「源更新 / 目标更新」徽章对应。
   */
  async function runSync(
    g: TraeLinkGroup,
    direction: "sourceToTarget" | "targetToSource",
  ) {
    if (!clientKey || syncBusy) return;
    const fromLabel = direction === "sourceToTarget" ? "源账号" : "目标账号";
    const toLabel = direction === "sourceToTarget" ? "目标账号" : "源账号";
    setSyncBusy(g.groupId);
    setCopyProgress([]);
    try {
      const res = await api.traeSessionSyncGroup(clientKey, g.groupId, direction);
      toast.success("同步完成", {
        description:
          `已用${fromLabel}的内容覆盖${toLabel}（会话 id 保持 ${shortId(res.keptSid)}）` +
          ` · 重写 ${res.writtenRows} 行 · ${res.relaunched ? "客户端已自动重启" : "请手动启动客户端"}`,
      });
      await loadLinks();
    } catch (cause) {
      toast.error("同步失败", { description: api.asError(cause) });
    } finally {
      setSyncBusy(null);
    }
  }

  /** 勾选「不再提示」：写进 localStorage，下次进页面直接静音。 */
  function muteDivergencePrompt() {
    writeStored(SK_DIVERGENCE_MUTED, "1");
    setDivergencePromptMuted(true);
  }

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold tracking-tight">Trae 会话记录</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            从客户端进程内存提取 SQLCipher 密钥并解密本地库 → 查看、导出 MD / ZIP、彻底删除会话（删除前整库备份，需在「设置 → 写库前整库备份」里开启；默认关闭）。
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button variant="outline" size="sm" onClick={() => clientKey && void refresh(clientKey)} disabled={busy}>
            <RefreshCw className="size-4" />刷新
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void onCleanup()}
            disabled={busy}
            title="清理工具产生的旧整库备份与可再生的解密快照，回收磁盘"
          >
            <HardDrive className="size-4" />清理工作文件
          </Button>
          <Button size="sm" onClick={() => void onExportAll()} disabled={busy || !status?.exists || !sessions?.length}>
            <Archive className="size-4" />导出全部 ZIP
          </Button>
        </div>
      </div>

      {/* 导出目录提示（默认路径 + 打开按钮） */}
      {exportDir ? (
        <div className="flex flex-wrap items-center gap-2 rounded-lg border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
          <span>
            导出目录：
            <code className="ml-1 font-mono">{exportDir}</code>
          </span>
          <Button
            variant="outline"
            size="sm"
            className="h-6 gap-1 px-2 text-xs"
            onClick={() => void api.traeRevealPath(exportDir)}
          >
            <FolderOpen className="size-3.5" />打开
          </Button>
        </div>
      ) : null}

      {/* 客户端选择：顺序只由后端排序决定（使用记忆 + 能不能跑起来）。
          ⚠️ 与首页共用 `<TraeClientSwitcher>` —— 顺序 /「常用」徽标 / 置灰规则，
          以及「已并入且本机跑不起来的客户端不进标签条」，都只有那一处实现。
          耗时写库期间整条禁用：切到一半换客户端会让进度与结果对不上。 */}
      <TraeClientSwitcher
        clients={clientOptions}
        value={clientKey}
        onChange={setClientKey}
        disabled={busy}
        label="Trae 客户端（会话记录）"
      />

      <Tabs value={tab} onValueChange={setTab} className="flex flex-col gap-4">
        <TabsList className="self-start">
          <TabsTrigger value="records">会话记录</TabsTrigger>
          <TabsTrigger value="links">复制与关联</TabsTrigger>
        </TabsList>

        <TabsContent value="records" className="mt-0 flex flex-col gap-4">

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>加载失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {/* 解密状态 */}
      {loading ? (
        <Skeleton className="h-24 rounded-xl" />
      ) : status ? (
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="flex items-center gap-2 text-base">
              <Database className="size-4" />
              解密库状态
              {status.exists ? (
                <Badge className="bg-emerald-500/15 text-emerald-600">已解密</Badge>
              ) : (
                <Badge variant="secondary">未解密</Badge>
              )}
            </CardTitle>
            <CardDescription className="break-all font-mono text-xs">{status.path}</CardDescription>
          </CardHeader>
          <CardContent className="space-y-3">
            {decryptHint ? (
              <p className="text-xs text-amber-600">
                已沿用上次的解密快照（{status.pages ?? 0} 页），列表可能不是最新：{decryptHint}
              </p>
            ) : null}
            {status.exists ? (
              <Collapsible open={tablesOpen} onOpenChange={setTablesOpen}>
                {/* 表清单默认收起：~180 张表的标签墙会占掉半屏，而它只在排查时才看得上。
                    收起时用一行摘要交代规模，需要时一键展开。 */}
                <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
                  <span className="text-xs text-muted-foreground tabular-nums">
                    {status.tables.length} 张表 · {tableRows.toLocaleString("zh-CN")} 行
                  </span>
                  <CollapsibleTrigger asChild>
                    <Button variant="ghost" size="sm" className="h-7 gap-1 px-2 text-xs">
                      {tablesOpen ? (
                        <ChevronDown className="size-3.5" />
                      ) : (
                        <ChevronRight className="size-3.5" />
                      )}
                      {tablesOpen ? "收起明细" : "展开明细"}
                    </Button>
                  </CollapsibleTrigger>
                </div>
                <CollapsibleContent>
                  <div className="mt-2 flex max-h-60 flex-wrap gap-1.5 overflow-y-auto rounded-lg border bg-muted/30 p-2.5">
                    {status.tables.map((t) => (
                      <Badge key={t.name} variant="outline">
                        {t.name}：{t.count}
                      </Badge>
                    ))}
                  </div>
                </CollapsibleContent>
              </Collapsible>
            ) : (
              <Alert>
                <KeyRound className="size-4" />
                <AlertTitle>尚未解密</AlertTitle>
                <AlertDescription>
                  需要提取 SQLCipher 密钥。请先启动并登录 {selected?.label}，再执行一键解密。
                </AlertDescription>
              </Alert>
            )}
            <div className="flex flex-wrap items-center gap-2">
              <Button onClick={() => void onScanAndDecrypt()} disabled={busy}>
                {busy ? <Loader2 className="size-4 animate-spin" /> : <ScanSearch className="size-4" />}
                扫描密钥并解密
              </Button>
              <Button variant="outline" onClick={() => void onDecryptSaved()} disabled={busy}>
                用已存密钥解密
              </Button>
              {busy ? (
                <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                  <Loader2 className="size-3.5 animate-spin" />
                  {busyLabel || "处理中…"}
                </span>
              ) : null}
            </div>
            {decryptProgress.length ? (
              <pre className="max-h-40 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
                {decryptProgress.join("\n")}
              </pre>
            ) : null}
          </CardContent>
        </Card>
      ) : null}

      {/* 会话列表 */}
      {!loading && status?.exists ? (
        <Card>
          <CardHeader className="pb-2">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div>
                <CardTitle className="text-base">会话列表</CardTitle>
                <CardDescription>
                  {sessions
                    ? `共 ${sessions.length} 个会话${
                        visibleSessions.length !== sessions.length
                          ? `（当前筛选 ${visibleSessions.length} 个）`
                          : ""
                      }，按最后活动倒序${
                        wsFilter
                          ? `；空间：${wsOptions.find((w) => w.key === wsFilter)?.label ?? "（已不存在）"}`
                          : ""
                      }`
                    : "正在读取…"}
                </CardDescription>
              </div>
              <div className="flex flex-wrap items-center gap-2">
                {ownerOptions.length > 1 ? (
                  <select
                    value={ownerFilter}
                    onChange={(e) => setOwnerFilter(e.target.value)}
                    className="h-8 rounded-md border border-border bg-background px-2 text-xs"
                  >
                    <option value="">全部账号</option>
                    {ownerOptions.map((o) => (
                      <option key={o} value={o}>
                        {o}
                      </option>
                    ))}
                  </select>
                ) : null}

                {/* 搜索：只搜元数据（正文在 SQLite 里，逐条读代价太高，占位符已写明） */}
                <div className="relative">
                  <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    placeholder="搜索标题 / 空间 / 会话 id"
                    className="h-8 w-[230px] pl-7 pr-7 text-xs"
                  />
                  {query ? (
                    <button
                      type="button"
                      title="清除搜索"
                      className="absolute top-1/2 right-1.5 -translate-y-1/2 text-muted-foreground"
                      onClick={() => setQuery("")}
                    >
                      <X className="size-3.5" />
                    </button>
                  ) : null}
                </div>

                {/* 工作空间筛选（数量与分组表头同源：都来自当前 sessions） */}
                <select
                  value={wsFilter}
                  onChange={(e) => setWsFilter(e.target.value)}
                  className="h-8 max-w-[220px] rounded-md border border-border bg-background px-2 text-xs"
                  title="按工作空间筛选"
                >
                  <option value="">全部工作空间（{wsOptions.length}）</option>
                  {wsOptions.map((w) => (
                    <option key={w.key} value={w.key}>
                      {w.label} · {w.count}
                    </option>
                  ))}
                </select>

                {/* 视图切换：按空间分组 / 平铺 */}
                <div className="flex items-center gap-0.5 rounded-md border p-0.5">
                  <button
                    type="button"
                    className={cn(
                      "flex items-center gap-1 rounded px-1.5 py-0.5 text-xs",
                      byWorkspace ? "bg-muted font-medium" : "text-muted-foreground hover:bg-muted/60",
                    )}
                    onClick={() => setByWorkspace(true)}
                  >
                    <Layers className="size-3" />
                    按空间
                  </button>
                  <button
                    type="button"
                    className={cn(
                      "flex items-center gap-1 rounded px-1.5 py-0.5 text-xs",
                      !byWorkspace ? "bg-muted font-medium" : "text-muted-foreground hover:bg-muted/60",
                    )}
                    onClick={() => setByWorkspace(false)}
                  >
                    <LayoutList className="size-3" />
                    平铺
                  </button>
                </div>
              </div>
            </div>
          </CardHeader>
          <CardContent>
            {visibleSessions && visibleSessions.length ? (
              // 列结构对齐 WorkBuddy 会话列表：勾选 / 标题 / 更新时间 / 正文 / 操作。
              // 归属账号、轮数、最后活动、创建时间都挪进详情弹窗（列表越干净越好扫）。
              // 列宽策略也照 WorkBuddy：**只给操作列定宽、标题列限上限**，其余自由伸缩 ——
              // 多给几个固定宽会让窄窗口下总宽下不来，操作按钮被挤出可视区。
              <div className="space-y-3">
                <div className="rounded-lg border border-border">
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead className="w-10">
                          {/* 全选只作用于**当前可见**的行（筛选后看不见的行不该被顺手删掉）。 */}
                          <input
                            type="checkbox"
                            className="size-4 cursor-pointer align-middle"
                            aria-label="全选当前列表"
                            title="全选当前列表"
                            checked={allVisiblePicked}
                            ref={(el) => {
                              if (el) el.indeterminate = pickedSessions.length > 0 && !allVisiblePicked;
                            }}
                            onChange={toggleSelectAll}
                          />
                        </TableHead>
                        <TableHead>标题</TableHead>
                        <TableHead>更新时间</TableHead>
                        <TableHead>正文</TableHead>
                        <TableHead className="w-40 text-right">操作</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {renderGroups.map((g) => (
                        <Fragment key={g.key}>
                          {/* 分组表头（只有「按空间」视图才画）。点一下 = 只看这个空间。 */}
                          {byWorkspace ? (
                            <TableRow className="bg-muted/50">
                              <TableCell colSpan={5} className="py-1.5">
                                <button
                                  type="button"
                                  className="flex w-full items-center gap-2 text-left"
                                  title="只看这个工作空间"
                                  onClick={() => setWsFilter(g.key)}
                                >
                                  <FolderOpen className="size-3.5 shrink-0 text-muted-foreground" />
                                  <span className="shrink-0 text-xs font-semibold">{g.label}</span>
                                  <Badge variant="secondary" className="shrink-0">
                                    {g.items.length}
                                  </Badge>
                                  <span className="truncate text-xs text-muted-foreground" title={g.path}>
                                    {g.path}
                                  </span>
                                </button>
                              </TableCell>
                            </TableRow>
                          ) : null}
                          {g.items.map((s) => (
                        <TableRow key={s.id} data-picked={recordSel.has(s.id) ? "1" : undefined}>
                          <TableCell className="w-10">
                            <input
                              type="checkbox"
                              className="size-4 cursor-pointer align-middle"
                              aria-label={`选择「${s.title || "无标题"}」`}
                              checked={recordSel.has(s.id)}
                              onChange={() => toggleRecordSel(s.id)}
                            />
                          </TableCell>
                          <TableCell className="max-w-[420px]">
                            <div className="truncate font-medium" title={s.title}>
                              {s.title || "（无标题）"}
                            </div>
                            <div className="flex items-center gap-1.5">
                              {!byWorkspace ? (
                                <span className="inline-flex shrink-0 items-center gap-1 rounded border px-1 py-0.5 text-[11px] text-muted-foreground">
                                  <FolderOpen className="size-3" />
                                  {s.workspace || "（未归属）"}
                                </span>
                              ) : null}
                              <span className="truncate font-mono text-[11px] text-muted-foreground">
                                {s.id}
                              </span>
                            </div>
                          </TableCell>
                          <TableCell className="whitespace-nowrap text-xs text-muted-foreground">
                            {s.updated || "—"}
                          </TableCell>
                          <TableCell className="whitespace-nowrap text-xs text-muted-foreground">
                            {s.messages ? `${s.messages.toLocaleString("zh-CN")} 条` : "无正文"}
                          </TableCell>
                          <TableCell>
                            <div className="flex items-center justify-end gap-1">
                              {/* 「查看详情」已按要求换成**编辑会话**：点哪条就编哪条。
                                  编辑器里能看到的内容本来就和详情一样（左栏回合 / 右栏会话式正文），
                                  再单独留一个只读详情弹窗就是两个入口指向同一件事。 */}
                              <Button
                                size="sm"
                                variant="ghost"
                                onClick={() => {
                                  setEditSeed(s.id);
                                  setEditOpen(true);
                                }}
                                title="编辑这条会话（左栏回合 / 右栏会话式正文，可改任意字段、增删回合与消息）"
                              >
                                <Pencil className="size-4" />
                              </Button>
                              <Button size="sm" variant="ghost" onClick={() => void onExportSession(s)} disabled={busy} title="导出 MD">
                                <FileDown className="size-4" />
                              </Button>
                              <Button
                                size="sm"
                                variant="ghost"
                                onClick={() => void openImport(s)}
                                disabled={busy}
                                title="导入到其他账号的本地库"
                              >
                                <Import className="size-4" />
                              </Button>
                              <Button
                                size="sm"
                                variant="ghost"
                                className="text-destructive"
                                onClick={() => void openDelete([s])}
                                title="彻底删除（若已开启「写库前整库备份」则先备份一份）"
                              >
                                <Trash2 className="size-4" />
                              </Button>
                            </div>
                          </TableCell>
                        </TableRow>
                          ))}
                        </Fragment>
                      ))}
                    </TableBody>
                  </Table>
                </div>

                {/* 批量操作条：有勾选才出现（与 WorkBuddy 会话列表同款）。 */}
                {pickedSessions.length > 0 ? (
                  <div className="flex flex-wrap items-center justify-between gap-2 rounded-md border bg-muted/40 px-3 py-2 text-sm">
                    <span>
                      已选 {pickedSessions.length} 个会话
                      {recordSel.size > pickedSessions.length ? (
                        <span className="ml-1 text-xs text-muted-foreground">
                          （另有 {recordSel.size - pickedSessions.length} 个被当前筛选藏起，已忽略）
                        </span>
                      ) : null}
                    </span>
                    <div className="flex items-center gap-2">
                      <Button variant="ghost" size="sm" onClick={() => setRecordSel(new Set())} disabled={busy}>
                        取消选择
                      </Button>
                      <Button
                        variant="destructive"
                        size="sm"
                        disabled={busy}
                        onClick={() => void openDelete(pickedSessions)}
                        title="整批只做一趟：一次解密、一次回写、最多一份整库备份（取决于「写库前整库备份」开关）"
                      >
                        {busy ? <Loader2 className="size-4 animate-spin" /> : <Trash2 className="size-4" />}
                        删除所选
                      </Button>
                    </div>
                  </div>
                ) : null}
              </div>
            ) : (
              /* ⚠️「筛选后为空」与「本来就没有会话」必须长得不一样：前者给清除入口。 */
              <div className="space-y-2">
                <p className="text-sm text-muted-foreground">
                  {sessions?.length
                    ? `没有匹配的会话 —— 筛选后 0 条，该客户端共 ${sessions.length} 条。`
                    : "解密库中没有会话记录。"}
                </p>
                {sessions?.length ? (
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => {
                      setQuery("");
                      setWsFilter("");
                      setOwnerFilter("");
                    }}
                  >
                    清除搜索与筛选
                  </Button>
                ) : null}
              </div>
            )}
          </CardContent>
        </Card>
      ) : null}

      {/* 上次批量删除的结果（与 WorkBuddy 会话列表同款留存）。 */}
      {batchReport ? (
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm">上次批量删除结果</CardTitle>
          </CardHeader>
          <CardContent className="space-y-1 text-xs text-muted-foreground">
            <p>
              删除 {batchReport.count} 个会话 · 共 {batchReport.deleted_rows_total} 行 · 移入回收站{" "}
              {batchReport.sessions.reduce((n, s) => n + (s.moved_files ?? 0), 0)} 个文件
            </p>
            <p className="break-all">
              备份：
              {batchReport.backup.length
                ? batchReport.backup.join("、")
                : "未备份（「设置 → 写库前整库备份」已关闭）"}
            </p>
            <p className="break-all">回收站：{batchReport.trash_dirs.join("、") || "—"}（文件可还原）</p>
            <p>
              云端任务列表：已清 {batchReport.cloud.deleted} 条
              {batchReport.cloud.failed.length ? ` · 失败 ${batchReport.cloud.failed.length} 条` : ""}
              {batchReport.cloud.skipped.length
                ? ` · 跳过 ${batchReport.cloud.skipped.length} 条（无云端凭证）`
                : ""}
            </p>
            {batchReport.cloud.failed.length ? (
              <p className="text-destructive">
                {batchReport.cloud.failed
                  .slice(0, 3)
                  .map((f) => `${f.sessionId.slice(0, 8)}…：${f.error}`)
                  .join("；")}
              </p>
            ) : null}
            <p>{batchReport.relaunched ? "客户端已自动重启。" : "客户端未自动重启，可手动启动。"}</p>
          </CardContent>
        </Card>
      ) : null}

        </TabsContent>

        <TabsContent value="links" className="mt-0 flex flex-col gap-4">
          {linkAccounts.length < 2 ? (
            <Alert>
              <Link2Off className="size-4" />
              <AlertTitle>账号不足</AlertTitle>
              <AlertDescription>
                复制与关联需要同一个客户端下至少两个有会话的账号。当前只识别到{" "}
                {linkAccounts.length} 个{linkAccounts.length === 1 ? `（${linkAccounts[0].label}）` : ""}
                ，请先在另一个账号上登录 {selected?.label ?? "Trae"} 并产生会话记录。
              </AlertDescription>
            </Alert>
          ) : (
            <>
              <Card>
                <CardHeader className="pb-3">
                  <CardTitle className="text-base">会话复制</CardTitle>
                  <CardDescription>
                    把源账号勾选的会话以<b>新 id</b> 复制给目标账号：源账号数据一行不改，
                    副本正文里的会话 id 同步替换为新 id，复制成功后自动登记跨账号关联。
                  </CardDescription>
                </CardHeader>
                <CardContent className="space-y-3">
                  <div className="flex flex-wrap items-center gap-3">
                    <label className="flex items-center gap-2 text-sm">
                      <span className="text-muted-foreground">源账号</span>
                      <select
                        value={linkSrcUid}
                        onChange={(e) => {
                          setLinkSrcUid(e.target.value);
                          setCopySel(new Set());
                        }}
                        className="h-8 max-w-56 rounded-md border border-border bg-background px-2 text-xs"
                      >
                        {linkAccounts.map((a) => (
                          <option key={a.uid} value={a.uid}>
                            {a.label}
                          </option>
                        ))}
                      </select>
                    </label>
                    <span className="text-muted-foreground">→</span>
                    <label className="flex items-center gap-2 text-sm">
                      <span className="text-muted-foreground">目标账号</span>
                      <select
                        value={linkDstUid}
                        onChange={(e) => {
                          setLinkDstUid(e.target.value);
                          setCopySel(new Set());
                        }}
                        className="h-8 max-w-56 rounded-md border border-border bg-background px-2 text-xs"
                      >
                        {linkAccounts.map((a) => (
                          <option key={a.uid} value={a.uid}>
                            {a.label}
                          </option>
                        ))}
                      </select>
                    </label>
                    <Button
                      variant="outline"
                      size="sm"
                      className="ml-auto"
                      onClick={() => void loadLinks()}
                      disabled={linksLoading || linkSrcUid === linkDstUid}
                    >
                      {linksLoading ? (
                        <Loader2 className="size-4 animate-spin" />
                      ) : (
                        <RefreshCw className="size-4" />
                      )}
                      刷新
                    </Button>
                  </div>

                  {linkSrcUid === linkDstUid && (
                    <Alert>
                      <AlertTriangle className="size-4" />
                      <AlertTitle>源账号与目标账号相同</AlertTitle>
                      <AlertDescription>请选择一个不同的目标账号。</AlertDescription>
                    </Alert>
                  )}

                  {copySrcSessions.length === 0 ? (
                    <p className="py-4 text-center text-sm text-muted-foreground">
                      该账号名下没有会话可复制
                    </p>
                  ) : (
                    <div className="max-h-80 overflow-auto rounded-md border">
                      <table className="w-full text-sm">
                        <thead className="sticky top-0 border-b bg-muted/40 text-xs text-muted-foreground">
                          <tr>
                            <th className="w-10 px-3 py-2">
                              {/* 全选框：只选「可复制」的会话，半选态表示部分勾选 */}
                              <input
                                type="checkbox"
                                className="size-4 cursor-pointer disabled:cursor-not-allowed"
                                aria-label={allCopyPicked ? "取消全选" : "全选可复制的会话"}
                                title={
                                  allCopyPicked
                                    ? `取消全选（${copyableIds.length} 条可复制）`
                                    : `全选可复制的 ${copyableIds.length} 条`
                                }
                                disabled={copyableIds.length === 0 || copyBusy}
                                checked={allCopyPicked}
                                ref={(el) => {
                                  if (el) el.indeterminate = someCopyPicked;
                                }}
                                onChange={toggleAllCopy}
                              />
                            </th>
                            <th className="px-3 py-2 text-left font-medium">会话</th>
                            <th className="w-32 px-3 py-2 text-left font-medium">复制状态</th>
                          </tr>
                        </thead>
                        <tbody>
                          {copySrcSessions.map((s) => {
                            const disabled = !s.messages;
                            return (
                              <tr key={s.id} className="border-b last:border-0 hover:bg-muted/30">
                                <td className="px-3 py-2">
                                  <input
                                    type="checkbox"
                                    className="size-4"
                                    disabled={disabled}
                                    checked={copySel.has(s.id)}
                                    onChange={() => toggleCopy(s.id)}
                                  />
                                </td>
                                <td className="max-w-[420px] px-3 py-2">
                                  <div className="truncate font-medium" title={s.title}>
                                    {s.title || "(无标题)"}
                                  </div>
                                  <div className="truncate text-xs text-muted-foreground">
                                    {s.updated} · {s.messages.toLocaleString("zh-CN")} 条
                                  </div>
                                </td>
                                <td className="px-3 py-2 text-xs">
                                  {!s.messages ? (
                                    <span className="text-muted-foreground">无正文</span>
                                  ) : (
                                    <span className="text-muted-foreground">可复制</span>
                                  )}
                                </td>
                              </tr>
                            );
                          })}
                        </tbody>
                      </table>
                    </div>
                  )}

                  <div className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-md border bg-muted/40 px-3 py-2 text-sm">
                    <span>已选 {copySel.size} 个会话</span>
                    <button
                      type="button"
                      onClick={toggleAllCopy}
                      disabled={copyableIds.length === 0 || copyBusy}
                      className="text-xs text-primary underline-offset-2 hover:underline disabled:cursor-not-allowed disabled:text-muted-foreground disabled:no-underline"
                    >
                      {allCopyPicked ? "取消全选" : `全选可复制的 ${copyableIds.length} 条`}
                    </button>
                    <Button
                      size="sm"
                      className="ml-auto"
                      disabled={
                        copyBusy || copySel.size === 0 || linkSrcUid === linkDstUid
                      }
                      onClick={() => void runCopy()}
                    >
                      {copyBusy ? (
                        <Loader2 className="size-4 animate-spin" />
                      ) : (
                        <Copy className="size-4" />
                      )}
                      复制到目标账号
                    </Button>
                  </div>

                  {copyProgress.length > 0 && (
                    <div className="max-h-40 overflow-auto rounded-md border bg-muted/30 p-3 font-mono text-xs text-muted-foreground">
                      {copyProgress.map((line, i) => (
                        <div key={i}>{line}</div>
                      ))}
                    </div>
                  )}

                  {copyReport && (
                    <Alert>
                      <Copy className="size-4" />
                      <AlertTitle>复制完成</AlertTitle>
                      <AlertDescription>
                        目标：{copyReport.target_label} · 写入 {copyReport.copied_rows} 行 ·
                        校验 {copyReport.verified_sessions} 个会话 · 登记关联{" "}
                        {copyReport.linked ?? 0} 组 ·{" "}
                        {copyReport.relaunched ? "已自动重启客户端" : "客户端未自动重启"}
                        {copyReport.skipped.length > 0 && ` · 跳过 ${copyReport.skipped.length} 个`}
                      </AlertDescription>
                    </Alert>
                  )}
                </CardContent>
              </Card>

              {linkSrcUid === linkDstUid ? (
                <Alert>
                  <AlertTriangle className="size-4" />
                  <AlertDescription>源账号与目标账号相同，请选择不同的两个账号。</AlertDescription>
                </Alert>
              ) : (
                <Card>
                  <CardHeader className="pb-3">
                    <CardTitle className="text-base">跨账号关联</CardTitle>
                    <CardDescription>
                      复制过的会话会自动登记成一个关联组。复制之后两端各自继续聊天会
                      <b>分叉</b>，这里会标出「源更新 / 目标更新」，并可<b>就地同步</b>
                      （覆盖内容但保持会话 id 不变）。
                    </CardDescription>
                  </CardHeader>
                  <CardContent className="space-y-3">
                    {linksLoading && !links ? (
                      <Skeleton className="h-24 rounded-lg" />
                    ) : links?.error ? (
                      <Alert variant="destructive">
                        <AlertTriangle className="size-4" />
                        <AlertDescription>{links.error}</AlertDescription>
                      </Alert>
                    ) : !links || links.groups.length === 0 ? (
                      <p className="py-4 text-center text-sm text-muted-foreground">
                        这两个账号之间还没有关联记录
                      </p>
                    ) : (
                      <div className="overflow-x-auto rounded-md border">
                        <table className="w-full text-sm">
                          <thead className="border-b bg-muted/40 text-xs text-muted-foreground">
                            <tr>
                              <th className="px-3 py-2 text-left font-medium">会话</th>
                              <th className="w-44 px-3 py-2 text-left font-medium">状态</th>
                              <th className="w-36 px-3 py-2 text-left font-medium">源副本</th>
                              <th className="w-36 px-3 py-2 text-left font-medium">目标副本</th>
                              <th className="w-24 px-3 py-2 text-right font-medium">操作</th>
                            </tr>
                          </thead>
                          <tbody>
                            {links.groups.map((g) => {
                              const v = VERDICT[g.verdict] ?? VERDICT.gone;
                              const dv = g.divergence ? DIVERGENCE[g.divergence] : undefined;
                              const syncing = syncBusy === g.groupId;
                              // 数据自检：写坏的副本在客户端里**渲染不出来**。
                              // 它与「分叉」正交 —— 两端条数可能完全相等，分叉徽章不亮，
                              // 用户就卡在「同步过了，客户端还是显示不全」。
                              const srcIssues = g.source.integrity ?? [];
                              const tgtIssues = g.target.integrity ?? [];
                              const broken =
                                g.broken ?? (srcIssues.length > 0 || tgtIssues.length > 0);
                              const brokenHint = [
                                srcIssues.length > 0
                                  ? `源账号：${srcIssues.join("；")}`
                                  : "",
                                tgtIssues.length > 0
                                  ? `目标账号：${tgtIssues.join("；")}`
                                  : "",
                              ]
                                .filter(Boolean)
                                .join("\n");
                              return (
                                <tr key={g.groupId} className="border-b last:border-0">
                                  <td className="max-w-[300px] px-3 py-2">
                                    <div className="truncate font-medium" title={String(g.title ?? "")}>
                                      {(g.title as string) || "(无标题)"}
                                    </div>
                                    <div className="truncate font-mono text-xs text-muted-foreground">
                                      {shortId(g.source.sessionId)}
                                    </div>
                                  </td>
                                  <td className="px-3 py-2">
                                    <div className="flex flex-wrap items-center gap-1">
                                      <span className={cn("inline-block whitespace-nowrap rounded border px-2 py-0.5 text-xs", v.tone)}>
                                        {v.label}
                                      </span>
                                      {/* 分叉徽章：两端都在时才有意义；`none`/`unknown` 不显示（不给错误暗示）。 */}
                                      {dv && g.verdict === "linked" && (
                                        <span
                                          className={cn("inline-block whitespace-nowrap rounded border px-2 py-0.5 text-xs", dv.tone)}
                                          title={dv.hint}
                                        >
                                          {dv.label}
                                        </span>
                                      )}
                                      {broken && (
                                        <span
                                          className="inline-block whitespace-nowrap rounded border border-destructive/30 bg-destructive/10 px-2 py-0.5 text-xs text-destructive"
                                          title={brokenHint}
                                        >
                                          副本数据错位
                                        </span>
                                      )}
                                    </div>
                                  </td>
                                  <td className="px-3 py-2 text-xs text-muted-foreground">
                                    {g.source.alive ? (
                                      <>
                                        <div>{g.source.updated || "—"}</div>
                                        <div className="text-[11px]">
                                          {typeof g.source.messages === "number"
                                            ? `${g.source.messages.toLocaleString("zh-CN")} 条`
                                            : "—"}
                                        </div>
                                        {srcIssues.length > 0 && (
                                          <div
                                            className="text-[11px] text-destructive"
                                            title={srcIssues.join("\n")}
                                          >
                                            ⚠️ {srcIssues.join("；")}
                                          </div>
                                        )}
                                      </>
                                    ) : (
                                      "已失效"
                                    )}
                                  </td>
                                  <td className="px-3 py-2 text-xs text-muted-foreground">
                                    {g.target.alive ? (
                                      <>
                                        <div>{g.target.updated || "—"}</div>
                                        <div className="text-[11px]">
                                          {typeof g.target.messages === "number"
                                            ? `${g.target.messages.toLocaleString("zh-CN")} 条`
                                            : "—"}
                                        </div>
                                        {tgtIssues.length > 0 && (
                                          <div
                                            className="text-[11px] text-destructive"
                                            title={tgtIssues.join("\n")}
                                          >
                                            ⚠️ {tgtIssues.join("；")}
                                          </div>
                                        )}
                                      </>
                                    ) : (
                                      "无副本"
                                    )}
                                  </td>
                                  <td className="px-3 py-2">
                                    <div className="flex items-center justify-end gap-1">
                                      {/* 已分叉 ⇒ 提供「以某一端为准」的同步按钮（写库、会重启客户端）。
                                          ⚠️ 副本**数据错位**时也必须给：错位时两端条数可能完全相等
                                          （分叉判据失效），智能按钮不亮用户就**无法自救**。 */}
                                      {g.canRebuild && (g.divergence === "sourceAhead" || broken) && (
                                        <Button
                                          variant="ghost"
                                          size="sm"
                                          disabled={syncBusy !== null}
                                          title={
                                            broken
                                              ? "用源账号的内容重建目标副本（修复数据错位，会话 id 不变）"
                                              : "用源账号的内容覆盖目标账号（会话 id 不变）"
                                          }
                                          onClick={() => void runSync(g, "sourceToTarget")}
                                        >
                                          {syncing ? (
                                            <Loader2 className="size-4 animate-spin" />
                                          ) : (
                                            <ArrowRight className="size-4" />
                                          )}
                                        </Button>
                                      )}
                                      {g.canRebuild && (g.divergence === "targetAhead" || broken) && (
                                        <Button
                                          variant="ghost"
                                          size="sm"
                                          disabled={syncBusy !== null}
                                          title={
                                            broken
                                              ? "用目标账号的内容重建源副本（修复数据错位，会话 id 不变）"
                                              : "用目标账号的内容覆盖源账号（会话 id 不变）"
                                          }
                                          onClick={() => void runSync(g, "targetToSource")}
                                        >
                                          {syncing ? (
                                            <Loader2 className="size-4 animate-spin" />
                                          ) : (
                                            <ArrowLeft className="size-4" />
                                          )}
                                        </Button>
                                      )}
                                      <Button
                                        variant="ghost"
                                        size="sm"
                                        title="解除关联（只删记录，不动会话数据）"
                                        onClick={() => void unlink(g)}
                                      >
                                        <Link2Off className="size-4" />
                                      </Button>
                                    </div>
                                  </td>
                                </tr>
                              );
                            })}
                          </tbody>
                        </table>
                      </div>
                    )}
                    {links?.storePath && (
                      <p className="text-xs text-muted-foreground">
                        关联记录：{links.storePath}（只删记录，不动会话数据）
                      </p>
                    )}
                  </CardContent>
                </Card>
              )}

              {/* 工程归属自愈：早前版本的同库复制漏改 session_project.project_id，
                  副本被挂在源账号的旧项目下 ⇒ 在 Trae 客户端里删不掉、重启后记录复活。
                  复制逻辑已修，这里负责把**历史副本**的脏数据一次清干净。 */}
              <Card>
                <CardHeader className="pb-3">
                  <CardTitle className="text-base">副本工程归属自愈</CardTitle>
                  <CardDescription>
                    检查副本记录的项目归属是否与其所属账号一致。不一致会导致在 Trae
                    客户端里删不掉、重启后记录又出现。
                  </CardDescription>
                </CardHeader>
                <CardContent className="space-y-3">
                  <div className="flex flex-wrap items-center gap-2">
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={healBusy !== null || !clientKey}
                      onClick={() => void runHealCheck()}
                    >
                      {healBusy === "check" ? (
                        <Loader2 className="size-4 animate-spin" />
                      ) : (
                        <Search className="size-4" />
                      )}
                      检查归属
                    </Button>
                    <Button
                      size="sm"
                      disabled={healBusy !== null || !clientKey}
                      title="会（视「写库前整库备份」开关）备份整库后回写，并自动重启客户端"
                      onClick={() => void runHeal()}
                    >
                      {healBusy === "heal" ? (
                        <Loader2 className="size-4 animate-spin" />
                      ) : (
                        <Wrench className="size-4" />
                      )}
                      一键修复归属
                    </Button>
                  </div>
                  {healReport && (
                    <p className="text-xs text-muted-foreground">{healReport}</p>
                  )}
                </CardContent>
              </Card>
            </>
          )}
        </TabsContent>
      </Tabs>

      {/* ⚠️ 这里原来有个**只读的「会话详情」弹窗**（元信息 + 逐回合正文 + 「编辑这条会话」）。
          它的唯一入口是列表里的「查看详情」按钮，而那个按钮已按要求改成**编辑会话** ⇒
          详情弹窗再无入口，留着就是死代码（而且它展示的内容编辑器里全都有）。
          删掉的是：本弹窗、`detail` state、`openDetail()`、`Eye` / `TraeSessionDetail` 两个 import。
          导出 MD 的能力没丢 —— 列表行上那个按钮还在。 */}

      {/* 彻底删除（单次确认；1 条走逐表预检，多条走整批说明） */}
      <Dialog
        open={deleteTargets.length > 0}
        onOpenChange={(v) => {
          if (!v && !busy) closeDelete();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2 text-destructive">
              <AlertTriangle className="size-4" />
              {deleteTargets.length > 1
                ? `彻底删除 ${deleteTargets.length} 个会话`
                : "彻底删除会话"}
            </DialogTitle>
            <DialogDescription className="break-words">
              {deleteTargets.length > 1
                ? `整批一次完成：只解密一趟、只回写一趟、最多留一份整库备份（视「写库前整库备份」开关；逐条删会让备份成倍膨胀）。`
                : `「${deleteTargets[0]?.title || "（无标题）"}」`}
            </DialogDescription>
          </DialogHeader>

          {deleteTargets.length > 1 ? (
            <div className="space-y-3 text-sm">
              <div className="grid grid-cols-2 gap-2">
                <div className="rounded-lg bg-muted/50 p-3">
                  <div className="text-xs text-muted-foreground">会话数</div>
                  <div className="font-medium tabular-nums">{deleteTargets.length} 个</div>
                </div>
                <div className="rounded-lg bg-muted/50 p-3">
                  <div className="text-xs text-muted-foreground">正文合计</div>
                  <div className="font-medium tabular-nums">
                    {batchMessages.toLocaleString("zh-CN")} 条
                  </div>
                </div>
              </div>
              <div className="rounded-lg border border-border p-3">
                <div className="mb-1.5 text-xs text-muted-foreground">归属账号</div>
                <div className="flex flex-wrap gap-1.5">
                  {batchOwners.map(([label, n]) => (
                    <Badge key={label} variant="outline">
                      {label}：{n} 个
                    </Badge>
                  ))}
                </div>
                <p className="mt-2 text-xs text-muted-foreground">
                  有云端凭证的账号会同步清掉任务列表里的对应记录；没有凭证的只删本地。
                </p>
              </div>
              <div className="max-h-40 space-y-1 overflow-auto rounded-lg border border-border p-3">
                {deleteTargets.map((s) => (
                  <p key={s.id} className="truncate text-xs" title={s.title}>
                    · {s.title || "（无标题）"}
                  </p>
                ))}
              </div>
            </div>
          ) : deleteInfo ? (
            <div className="space-y-3 text-sm">
              <div className="grid grid-cols-2 gap-2">
                <div className="rounded-lg bg-muted/50 p-3">
                  <div className="text-xs text-muted-foreground">归属账号</div>
                  <div className="font-medium">{deleteInfo.owner_label || "（无归属）"}</div>
                </div>
                <div className="rounded-lg bg-muted/50 p-3">
                  <div className="text-xs text-muted-foreground">关联文件</div>
                  <div className="font-medium">{deleteInfo.files.length ? `${deleteInfo.files.length} 个（移入回收站）` : "无"}</div>
                </div>
              </div>
              <Alert>
                <Database className="size-4" />
                <AlertDescription>{deleteInfo.note}</AlertDescription>
              </Alert>
              <div className="rounded-lg border border-border p-3">
                <div className="mb-1 text-xs text-muted-foreground">云端任务列表</div>
                <div className="flex items-center gap-2">
                  {deleteInfo.cloud_credential ? (
                    <>
                      <Badge className="bg-emerald-500/15 text-emerald-600">将同步删除</Badge>
                      <span className="text-xs text-muted-foreground">
                        删除本会话在归属账号任务列表中的记录（失败时仅提示，不影响本地删除）
                      </span>
                    </>
                  ) : (
                    <>
                      <Badge variant="secondary">仅本地</Badge>
                      <span className="text-xs text-muted-foreground">
                        该归属账号无云端凭证，不尝试同步删除任务列表
                      </span>
                    </>
                  )}
                </div>
              </div>
              <div className="flex flex-wrap gap-1.5">
                {deleteInfo.tables.map((t) => (
                  <Badge key={t.name} variant="outline">
                    {t.name}：{t.count}
                  </Badge>
                ))}
              </div>
            </div>
          ) : (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" />正在预检…
            </div>
          )}

          {deleteProgress.length ? (
            <pre className="max-h-40 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
              {deleteProgress.join("\n")}
            </pre>
          ) : null}

          {/* ⚠️ 长文案不要放进 `DialogFooter`：它在 ≥640px 会变成横排（`sm:flex-row`），
              没有伸缩余量的整段文字会被压成一行一个字。 */}
          <p className="text-xs text-muted-foreground">
            删除会先把会话文件移入本工具回收站（可恢复），期间会结束并自动重启客户端。是否在动库前留一份整库备份，取决于「设置 → 写库前整库备份」开关（默认关闭）。
          </p>

          <DialogFooter>
            <Button variant="outline" onClick={closeDelete} disabled={busy}>
              取消
            </Button>
            <Button
              variant="destructive"
              disabled={busy || (deleteTargets.length === 1 && !deleteInfo)}
              onClick={() => void confirmDelete()}
            >
              {busy ? <Loader2 className="size-4 animate-spin" /> : <Trash2 className="size-4" />}
              {deleteTargets.length > 1 ? `确认删除 ${deleteTargets.length} 个` : "确认删除"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 跨账号导入 */}
      <Dialog
        open={importTarget !== null}
        onOpenChange={(v) => {
          if (!v && !importBusy) {
            setImportTarget(null);
            setImportDstClient(null);
            setImportDst(null);
            setImportInspect(null);
            setImportReport(null);
            setImportProgress([]);
          }
        }}
      >
        <ResizableDialogContent storageKey="trae-import-account">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Import className="size-4" />
              导入到其他账号
            </DialogTitle>
            <DialogDescription className="break-words">
              「{importTarget?.title || "（无标题）"}」→ 目标账号本地库
              <span className="block font-mono text-xs">{importTarget?.id}</span>
            </DialogDescription>
          </DialogHeader>

          {importCandidates.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              本机没有可导入账号（无已登录或留有解密库的账号）。
            </p>
          ) : (
            <div className="min-h-0 space-y-3 overflow-y-auto pr-1 text-sm">
              {/* 目标客户端维度：仅展示所选客户端下的账号 */}
              {clients.length > 0 ? (
                <div>
                  <div className="mb-1 text-xs text-muted-foreground">目标客户端</div>
                  <div className="flex flex-wrap gap-1.5">
                    {targetClientOptions(clients)
                      .map((c) => {
                        const active = c.key === importDstClient;
                        return (
                          <button
                            key={c.key}
                            type="button"
                            disabled={importBusy}
                            onClick={() => {
                              setImportDstClient(c.key);
                              setImportDst(null);
                              setImportInspect(null);
                              setImportReport(null);
                            }}
                            className={cn(
                              "rounded-lg border px-2.5 py-1.5 transition-colors",
                              active
                                ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                                : "border-border hover:bg-foreground/[0.03]",
                            )}
                          >
                            {c.label}
                          </button>
                        );
                      })}
                  </div>
                </div>
              ) : null}

              {(() => {
                const shown = importCandidates.filter((c) => c.client_key === importDstClient);
                if (shown.length === 0) {
                  return (
                    <p className="text-sm text-muted-foreground">
                      该客户端下没有可导入账号（需先在本机登录过或留有解密库）。
                    </p>
                  );
                }
                return (
                  <div>
                    <div className="mb-1 text-xs text-muted-foreground">选择目标账号</div>
                    <div className="flex flex-wrap gap-1.5">
                      {shown.map((c) => {
                        const key = `${c.client_key}::${c.account_id}`;
                        return (
                          <button
                            key={key}
                            type="button"
                            disabled={importBusy}
                            onClick={() => void inspectDst(c)}
                            title={
                              c.is_source
                                ? "导入到原账号 = 同库复制一份（生成新 id），原记录保留"
                                : undefined
                            }
                            className={cn(
                              "rounded-lg border px-2.5 py-1.5 transition-colors",
                              importDst === key
                                ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                                : "border-border hover:bg-foreground/[0.03]",
                              !c.db_exists && "opacity-60",
                            )}
                          >
                            <span className="block">{c.label}</span>
                            <span className="block text-[11px] text-muted-foreground">
                              {c.client_label}
                              {c.is_current ? " · 当前登录" : ""}
                              {c.is_source ? " · 原账号（同库复制）" : ""}
                              {!c.db_exists ? " · 未登录过" : ""}
                            </span>
                          </button>
                        );
                      })}
                    </div>
                  </div>
                );
              })()}

              {importHints.length > 0 ? (
                <div className="space-y-1 rounded-lg bg-amber-500/10 p-3 text-xs leading-5 text-amber-700">
                  {importHints.map((h, i) => (
                    <div key={i}>{h}</div>
                  ))}
                </div>
              ) : null}

              {importInspect ? (
                <div className="flex flex-wrap gap-1.5">
                  <Badge variant={importInspect.db_exists ? "default" : "secondary"}>
                    {importInspect.db_exists ? "已有本地库" : "无本地库"}
                  </Badge>
                  <Badge variant={importInspect.key_ready ? "default" : "secondary"}>
                    密钥：{importInspect.key_ready ? "可用" : importInspect.key_source === "scan_available" ? "需扫描" : "缺失"}
                  </Badge>
                  <Badge variant={importInspect.running ? "secondary" : "outline"}>
                    {importInspect.running ? "客户端运行中（将自动退出）" : "客户端已退出"}
                  </Badge>
                  <Badge variant="outline">现有 {importInspect.sessions_now} 个会话</Badge>
                </div>
              ) : null}

              <Alert>
                <AlertTriangle className="size-4" />
                <AlertDescription className="leading-5">
                  导入会把该会话写进目标账号的本地加密库（原库是否先备份到工具目录，取决于「设置 → 写库前整库备份」开关；默认关闭）。写入前会自动退出目标客户端；
                  目标账号须已在本机登录过。同客户端跨账号（如 Trae CN 的 A→B）会在库内复制会话并生成新 id，源账号记录保留。
                  与云端任务列表的同步行为未定义，若云端清理本地孤儿行，导入的会话可能被覆盖。
                </AlertDescription>
              </Alert>

              {importProgress.length ? (
                <pre className="max-h-40 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
                  {importProgress.join("\n")}
                </pre>
              ) : null}

              {importReport ? (
                <div className="space-y-1.5 rounded-lg bg-emerald-500/10 p-3 text-xs text-emerald-700">
                  <div>复制 {importReport.copied_rows} 行（{importReport.sessions_requested} 个会话），回写 {importReport.pages} 页</div>
                  <div>新库自检会话数：{importReport.verified_sessions}</div>
                  {importReport.skipped.length ? (
                    <div>跳过（目标库已存在）：{importReport.skipped.join(", ")}</div>
                  ) : null}
                  <div className="break-all font-mono text-emerald-600/80">
                    备份：
                    {importReport.backup_dir || "未备份（「设置 → 写库前整库备份」已关闭）"}
                  </div>
                </div>
              ) : null}
            </div>
          )}

          <DialogFooter>
            <Button variant="outline" onClick={() => setImportTarget(null)} disabled={importBusy}>
              取消
            </Button>
            <Button
              disabled={!importDst || !importInspect?.db_exists || importBusy}
              onClick={() => void confirmImport()}
            >
              {importBusy ? <Loader2 className="size-4 animate-spin" /> : <Import className="size-4" />}
              开始导入
            </Button>
          </DialogFooter>
        </ResizableDialogContent>
      </Dialog>

      {/* 副本已分叉提醒：进「复制与关联」时若有分叉就弹一次。
          ⚠️ 只在打开本工具时能检测到 —— Trae 客户端里发起的对话我们无法实时感知，
          所以这里说明清楚「这是上次打开之后的差异」。 */}
      <Dialog
        open={divergencePrompt !== null}
        onOpenChange={(v) => !v && setDivergencePrompt(null)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <AlertTriangle className="size-5 text-amber-500" />
              发现 {divergencePrompt?.groups.length ?? 0} 个副本已分叉
            </DialogTitle>
            <DialogDescription>
              下列会话在复制之后两端各自有了新内容（一边聊得更靠前了）。
              可以在「跨账号关联」表格里点箭头按钮<b>就地同步</b>：覆盖内容但保持会话 id 不变。
            </DialogDescription>
          </DialogHeader>
          <div className="max-h-64 space-y-2 overflow-y-auto py-1">
            {divergencePrompt?.groups.map((g) => {
              const dv = DIVERGENCE[g.divergence ?? ""] ?? DIVERGENCE.sourceAhead;
              return (
                <div
                  key={g.groupId}
                  className="flex items-center justify-between gap-3 rounded-md border px-3 py-2 text-sm"
                >
                  <span className="min-w-0 flex-1 truncate" title={String(g.title ?? "")}>
                    {(g.title as string) || "(无标题)"}
                  </span>
                  <span className={cn("shrink-0 rounded border px-2 py-0.5 text-xs", dv.tone)}>
                    {dv.label}
                  </span>
                </div>
              );
            })}
          </div>
          <DialogFooter className="flex-wrap items-center gap-2 sm:justify-between">
            <Button
              variant="ghost"
              size="sm"
              className="text-muted-foreground"
              onClick={muteDivergencePrompt}
            >
              不再提示
            </Button>
            <Button size="sm" onClick={() => setDivergencePrompt(null)}>
              知道了，稍后处理
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* T46 会话编辑器：**只编 `editSeed` 指向的那一条**（列表行上点「编辑」带过来）。
          ⚠️ 原来还会把整份 `sessions` 传进去、让编辑器左栏下拉切换会话 —— 已按要求取消：
          入口是「点哪条编哪条」，再传一份列表只会多一个跟当前动作没关系的出口。 */}
      {clientKey ? (
        <TraeSessionEditor
          open={editOpen}
          onOpenChange={setEditOpen}
          clientKey={clientKey}
          clientLabel={clientOptions.find((c) => c.key === clientKey)?.label ?? clientKey}
          sessionId={editSeed}
          onChanged={() => {
            if (clientKey) void refresh(clientKey);
          }}
        />
      ) : null}
    </div>
  );
}
