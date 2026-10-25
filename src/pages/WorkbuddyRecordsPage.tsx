import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  Copy,
  Eye,
  FileDown,
  FolderOpen,
  LayoutList,
  Layers,
  Link2,
  Link2Off,
  Loader2,
  Pencil,
  RefreshCw,
  ScrollText,
  Search,
  Trash2,
  X,
} from "lucide-react";

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
import { ResizableDialogContent } from "@/components/ui/resizable-dialog-content";
import { WorkbuddySessionEditor } from "@/components/workbuddy-session-editor";
import { WorkbuddySessionGroups } from "@/components/workbuddy-session-groups";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { Input } from "@/components/ui/input";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import type {
  WbLinkGroup,
  WbLinksPreview,
  WbSessionCopyPreview,
  WbSessionCopyReport,
  WbSessionDeleteReport,
  WbSessionDetail,
  WbSessionItem,
  WbSessionLinkAllReport,
} from "@/lib/trae-types";

function humanSize(bytes: number): string {
  if (!bytes) return "—";
  const mb = bytes / 1024 / 1024;
  if (mb >= 1) return `${mb.toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

function timeLabel(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 账号下拉标签：后端给出的显示名（昵称/手机号 + uid 尾号）。 */
type WbAccountGroup = { uid: string; label?: string; count: number; sessions: WbSessionItem[] };

/**
 * 页面级快照缓存（**模块作用域**，跨「离开页面再回来」保留）。
 *
 * 为什么需要：原来每次进入页面都会 `loading=true` → 整表变成骨架屏，等于"重载"。
 * 现在：有缓存 ⇒ 立刻用缓存渲染，同时在后台刷新（stale-while-revalidate）；
 * 没有缓存 ⇒ 才显示骨架屏。
 *
 * ⚠️ 两条纪律：
 * ① 列表本身永远来自后端（不在这里拼接、不本地推算数量），缓存只是"上一次的结果 + 时间"；
 * ② 后台刷新失败**不许静默**：保留旧数据的同时必须写明「这是几点读的、刷新失败了」。
 */
type WbAccountsCache = { at: number; accounts: WbAccountGroup[] };
let wbAccountsCache: WbAccountsCache | null = null;

/** 长 id 的缩写（仅用于展示会话 id，账号名一律走 `accountLabel`）。 */
function shortId(id: string): string {
  return id.length > 10 ? `${id.slice(0, 8)}…` : id;
}

/** 显示名兜底：后端没给 label 时才退化成 uid 前缀。 */
function accountLabel(a: WbAccountGroup | undefined, uid: string): string {
  const label = a?.label?.trim();
  if (label) return label;
  if (!uid) return "（未知账号）";
  return `uid ${uid.slice(0, 8)}…`;
}

const VERDICT: Record<string, { label: string; tone: string }> = {
  linked: { label: "两端都在", tone: "bg-emerald-500/10 text-emerald-600 border-emerald-500/30" },
  targetMissing: { label: "目标缺副本", tone: "bg-amber-500/10 text-amber-600 border-amber-500/30" },
  sourceMissing: { label: "源已失效", tone: "bg-muted text-muted-foreground" },
  gone: { label: "两端都不在", tone: "bg-muted text-muted-foreground" },
};

export default function WorkbuddyRecordsPage() {
  // 会话记录
  // 初始值直接吃模块级缓存 ⇒ 从别的页面切回来时**立刻有内容**，不再闪骨架屏。
  const [accounts, setAccounts] = useState<WbAccountGroup[]>(
    () => wbAccountsCache?.accounts ?? [],
  );
  const [loadedAt, setLoadedAt] = useState<number>(() => wbAccountsCache?.at ?? 0);
  const [uid, setUid] = useState("");
  const [loading, setLoading] = useState<boolean>(() => wbAccountsCache === null);
  /** 后台刷新中（已有缓存时不显示骨架屏，只让刷新图标转）。 */
  const [refreshing, setRefreshing] = useState(false);
  /** 后台刷新失败：保留旧数据，但必须说清「这是几点读的、刷新失败了」。 */
  const [staleNote, setStaleNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [detail, setDetail] = useState<WbSessionDetail | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<WbSessionItem[] | null>(null);
  const [deleteReport, setDeleteReport] = useState<WbSessionDeleteReport | null>(null);
  // T46 · S4：会话编辑器。入口就是下面表格行里的 ✏️ —— `editSeed` 直接给该行的会话 id，
  // 编辑器打开即载入（不再有「先开空编辑器再挑会话」那一步）。
  const [editSeed, setEditSeed] = useState<string | null>(null);
  const [editOpen, setEditOpen] = useState(false);

  // 搜索 / 工作空间（用户需求：会话可搜索、可查看归属空间、可按空间分类查看）
  // `wsFilter === ""` = 全部空间；`byWorkspace` = 按空间分组视图（默认开，和客户端列表一致）。
  const [query, setQuery] = useState("");
  const [wsFilter, setWsFilter] = useState("");
  const [byWorkspace, setByWorkspace] = useState(true);

  // 复制与关联
  const [srcUid, setSrcUid] = useState("");
  const [dstUid, setDstUid] = useState("");
  const [copySel, setCopySel] = useState<Set<string>>(new Set());
  const [preview, setPreview] = useState<WbSessionCopyPreview | null>(null);
  const [links, setLinks] = useState<WbLinksPreview | null>(null);
  const [copyReport, setCopyReport] = useState<WbSessionCopyReport | null>(null);
  const [edgeDbs, setEdgeDbs] = useState<{ name: string; path: string; size: number }[]>([]);

  // 全账号互通（一键 + 切号自动跑）。`autoLink === null` = 设置还没读到/读失败，
  // 此时开关**禁用**并保持未知 —— 不能显示成「关着」（那是两种不同的状态）。
  const [linkAllBusy, setLinkAllBusy] = useState(false);
  const [linkAllReport, setLinkAllReport] = useState<WbSessionLinkAllReport | null>(null);
  const [linkPreview, setLinkPreview] = useState<WbSessionLinkAllReport | null>(null);
  const [confirmLink, setConfirmLink] = useState(false);
  const [autoLink, setAutoLink] = useState<boolean | null>(null);

  /**
   * 读会话列表。
   *
   * `force = false`（进入页面 / 后台刷新）：有缓存就**不显示骨架屏**，只让刷新图标转；
   * `force = true`（点「刷新」/ 做完删除、复制、互通之后）：数据一定变了，必须重新读。
   */
  const load = useCallback(async (force = false) => {
    const hasCache = wbAccountsCache !== null;
    if (force || !hasCache) setLoading(true);
    else setRefreshing(true);
    setError(null);
    try {
      const res = await api.workbuddySessionListByAccount();
      if (!res.ok) throw new Error(res.error || "读取会话列表失败");
      const list = res.accounts || [];
      setAccounts(list);
      wbAccountsCache = { at: Date.now(), accounts: list };
      setLoadedAt(wbAccountsCache.at);
      setStaleNote(null);
      setUid((prev) => prev || list[0]?.uid || "");
      setSrcUid((prev) => prev || list[0]?.uid || "");
      setDstUid((prev) => prev || list[1]?.uid || list[0]?.uid || "");
      const ed = await api.workbuddySessionEdgeSyncDbs();
      setEdgeDbs(ed.items || []);
      // 预览与主流程并行、失败也不影响列表（但绝不显示旧/假数字：失败就清空）。
      void api
        .workbuddySessionLinkAllPreview()
        .then((p) => setLinkPreview(p.ok ? p : null))
        .catch(() => setLinkPreview(null));
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      // ⚠️ 有缓存时不能把列表清空（那会变成「数据没了」的假象），
      //    而是保留旧数据 + 说明这是几点读的、这次为什么没刷新成功。
      if (hasCache || wbAccountsCache !== null) setStaleNote(msg);
      else setError(msg);
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    void load(false);
  }, [load]);

  // 读「切号自动同步会话」开关的当前值（真实值来自后端 settings.json）。
  useEffect(() => {
    void (async () => {
      try {
        const s = await api.settingsGet();
        setAutoLink(!!s.autoLinkWorkbuddySessions);
      } catch {
        // 读不出来就让开关保持未知（禁用），不要假装它是「关着的」。
        setAutoLink(null);
      }
    })();
  }, []);

  const sessions = useMemo(
    () => accounts.find((a) => a.uid === uid)?.sessions ?? [],
    [accounts, uid],
  );

  // ---------------------------------------------------------------------------
  // 工作空间：归并 / 搜索 / 分组
  // ---------------------------------------------------------------------------
  //
  // ⚠️ 三个派生值全部只吃 `sessions` 这一份数据（不另查后端）：
  //    下拉里的「N 个」、分组表头里的「N 个」、右上角的总数，都是同一批行算出来的，
  //    所以不会出现「下拉说 5 个、列表只有 4 个」这种两处不同源的问题。
  //    工作空间的键 / 展示名由后端 `workspace_of()` 给（cwd 归一化后小写），
  //    前端不重复实现那套规则，否则 `D:/x` 与 `d:\x` 又会被拆成两个空间。

  /** 当前账号涉及的工作空间（最近活动的排前面）。 */
  const wsOptions = useMemo(() => {
    const map = new Map<
      string,
      { key: string; label: string; path: string; count: number; lastAt: number }
    >();
    for (const s of sessions) {
      const key = s.workspaceKey ?? "";
      const cur = map.get(key);
      if (cur) {
        cur.count += 1;
        cur.lastAt = Math.max(cur.lastAt, s.updatedAt || 0);
      } else {
        map.set(key, {
          key,
          label: s.workspace || "（未归属）",
          path: s.workspacePath || "",
          count: 1,
          lastAt: s.updatedAt || 0,
        });
      }
    }
    const list = [...map.values()].sort(
      (a, b) => b.lastAt - a.lastAt || a.label.localeCompare(b.label),
    );
    // 同名空间（两个不同目录都叫 `bin`）在下拉里必须能区分 —— 否则会出现两行一模一样的字，
    // 只能靠顺序猜哪个是哪个。重名时补上一级目录（`old/bin` / `new/bin`）。
    const dupLabel = new Map<string, number>();
    for (const w of list) dupLabel.set(w.label, (dupLabel.get(w.label) ?? 0) + 1);
    return list.map((w) => {
      if ((dupLabel.get(w.label) ?? 0) <= 1) return w;
      const segs = (w.path || "").split("/").filter(Boolean);
      const tail = segs.slice(-2).join("/");
      return tail ? { ...w, label: tail } : w;
    });
  }, [sessions]);

  /**
   * 搜索 + 空间筛选后的行。
   *
   * 搜的是**元数据**（标题 / 工作空间名 / 路径 / 会话 id / 模型）——正文在
   * `projects/*.jsonl` 里，要搜正文得逐个读文件，不放在这个输入框里（占位符已写明）。
   */
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return sessions
      .filter((s) => {
        if (wsFilter && (s.workspaceKey ?? "") !== wsFilter) return false;
        if (!q) return true;
        return [s.title, s.workspace, s.workspacePath, s.id, s.model].some((v) =>
          (v ?? "").toLowerCase().includes(q),
        );
      })
      .slice()
      .sort((a, b) => (b.updatedAt || 0) - (a.updatedAt || 0));
  }, [sessions, query, wsFilter]);

  /** 按工作空间分组（分组顺序 = 组内最新一条的时间降序）。 */
  const wsGroups = useMemo(() => {
    const map = new Map<
      string,
      { key: string; label: string; path: string; items: WbSessionItem[] }
    >();
    for (const s of filtered) {
      const key = s.workspaceKey ?? "";
      const g = map.get(key);
      if (g) g.items.push(s);
      else
        map.set(key, {
          key,
          label: s.workspace || "（未归属）",
          path: s.workspacePath || "",
          items: [s],
        });
    }
    return [...map.values()].sort((a, b) => {
      const at = a.items[0]?.updatedAt ?? 0;
      const bt = b.items[0]?.updatedAt ?? 0;
      return bt - at || a.label.localeCompare(b.label);
    });
  }, [filtered]);

  const loadLinks = useCallback(async () => {
    if (!srcUid || !dstUid || srcUid === dstUid) {
      setLinks(null);
      return;
    }
    try {
      setLinks(await api.workbuddySessionLinksPreview(srcUid, dstUid));
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  }, [srcUid, dstUid]);

  useEffect(() => {
    void loadLinks();
  }, [loadLinks]);

  // 源账号会话变化时刷新复制预检
  const srcSessions = useMemo(
    () => accounts.find((a) => a.uid === srcUid)?.sessions ?? [],
    [accounts, srcUid],
  );

  const refreshPreview = useCallback(async () => {
    if (!srcUid || !dstUid || srcUid === dstUid || copySel.size === 0) {
      setPreview(null);
      return;
    }
    try {
      setPreview(await api.workbuddySessionCopyPreview(srcUid, dstUid, [...copySel]));
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  }, [srcUid, dstUid, copySel]);

  useEffect(() => {
    void refreshPreview();
  }, [refreshPreview]);

  const toggle = (id: string, setter: typeof setSelected, value: Set<string>) => {
    const next = new Set(value);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setter(next);
  };

  /**
   * 全选：只作用于**当前可见**的行（搜索 / 空间筛选之后）。
   *
   * ⚠️ 勾选与「可见行」是两件事（Trae 会话页踩过的坑）：筛选会把一部分勾中的行藏起来，
   *    所以①表头全选只覆盖可见行；②改搜索词 / 换空间 / 换账号时清空勾选 ——
   *    否则用户会在看不见那些行的情况下把它们删掉。
   */
  const allSelected = filtered.length > 0 && filtered.every((s) => selected.has(s.id));
  /** 可见行里被勾中的数量（表头复选框的「半选」只看这个，不看被筛掉的勾选）。 */
  const pickedVisible = filtered.filter((s) => selected.has(s.id)).length;
  const toggleAll = () => {
    setSelected(allSelected ? new Set() : new Set(filtered.map((s) => s.id)));
  };

  /** 表格要渲染的「块」：分组视图按空间分块；平铺视图合成一块（表头行不渲染）。 */
  const renderGroups = byWorkspace
    ? wsGroups
    : [{ key: "__flat__", label: "", path: "", items: filtered }];

  const openDetail = async (item: WbSessionItem) => {
    setBusy(item.id);
    try {
      const d = await api.workbuddySessionDetail(uid, item.id);
      if (!d.ok) throw new Error(d.error || "读取详情失败");
      setDetail(d);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const exportOne = async (item: WbSessionItem) => {
    setBusy(item.id);
    try {
      const r = await api.workbuddySessionExportMd(uid, item.id);
      toast.success(`已导出 ${r.turns} 个回合`, { description: r.path });
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const runDelete = async () => {
    if (!confirmDelete) return;
    const ids = confirmDelete.map((s) => s.id);
    setBusy("delete");
    try {
      const r = await api.workbuddySessionDelete(uid, ids);
      setDeleteReport(r);
      toast.success(`已删除 ${r.count} 个会话`, {
        description: `正文已移入回收站，备份：${r.backup}`,
      });
      setConfirmDelete(null);
      setSelected(new Set());
      await load(true);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const runCopy = async () => {
    if (!srcUid || !dstUid || copySel.size === 0) return;
    setBusy("copy");
    try {
      const r = await api.workbuddySessionCopy(srcUid, dstUid, [...copySel]);
      setCopyReport(r);
      toast.success(`已复制 ${r.count} 个会话`, { description: "源账号数据未做任何改动" });
      setCopySel(new Set());
      await load(true);
      await loadLinks();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  /** 把一次互通的结果统一报出去（两个入口共用，避免同样的结果两种说法）。 */
  const reportLink = async (r: WbSessionLinkAllReport) => {
    setLinkAllReport(r);
    const adopted = r.adopted ?? 0;
    if (r.notNeeded) {
      toast.info(r.reason || "所有会话都已在各账号之间关联");
    } else if ((r.copied ?? 0) === 0 && adopted > 0) {
      // 只登记、没复制：目标账号本来就有同一段对话的副本。
      toast.success(`已登记 ${adopted} 组关联`, {
        description: "这些账号本来就有同一段对话的副本，没有复制新的",
      });
    } else {
      const extra = adopted > 0 ? `，另登记 ${adopted} 组已有副本` : "";
      toast.success(`已互通 ${r.copied} 个会话副本${extra}`, {
        description: `写到 ${r.byTarget?.length ?? 0} 个账号，源会话未做任何改动`,
      });
    }
    await load(true);
    await loadLinks();
  };

  /**
   * 一键互通：把本机所有会话补齐到所有账号（后端幂等，可反复点）。
   *
   * ⚠️ 这是**重操作**（本机实测一次 74 个副本 / 约 2.98 GB），所以入口先弹确认框
   * 把数字摆出来 —— 不能点一下就默默往磁盘写几个 GB。
   * `skipConfirm` 只在「预览显示没有要复制的」时用（那时只是登记，0 字节）。
   */
  const runLinkAll = async (skipConfirm = false) => {
    if (!skipConfirm && (linkPreview?.planned ?? 0) > 0) {
      setConfirmLink(true);
      return;
    }
    setConfirmLink(false);
    setLinkAllBusy(true);
    try {
      await reportLink(await api.workbuddySessionLinkAll());
    } catch (e) {
      // 最常见的就是「WorkBuddy 正在运行」—— 后端文案已经说清下一步，原样展示。
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setLinkAllBusy(false);
    }
  };

  /** 只登记「同一段对话已有副本」：0 字节、不碰会话库，客户端开着也能用。 */
  const runLinkExisting = async () => {
    setConfirmLink(false);
    setLinkAllBusy(true);
    try {
      await reportLink(await api.workbuddySessionLinkExisting());
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setLinkAllBusy(false);
    }
  };

  /** 开关直接落盘到 `settings.json`：切号流程每次都会问后端，不依赖页面是否打开。 */
  const toggleAutoLink = async (next: boolean) => {
    const prev = autoLink;
    setAutoLink(next); // 乐观更新，失败再滚回去
    try {
      const s = await api.settingsSet({ autoLinkWorkbuddySessions: next });
      setAutoLink(!!s.autoLinkWorkbuddySessions);
      toast.success(next ? "已开启：切号时自动同步会话" : "已关闭：切号时不再自动同步会话", {
        description: next
          ? "每次切换账号都会自动补一次（WorkBuddy 关闭期间写入）"
          : "仍可在本页手动点「一键互相关联」",
      });
    } catch (e) {
      setAutoLink(prev);
      toast.error(e instanceof Error ? e.message : String(e));
    }
  };

  const unlink = async (g: WbLinkGroup) => {
    try {
      const r = await api.workbuddySessionUnlink(g.groupId);
      toast.success("已解除关联", { description: `剩余 ${r.remaining} 组` });
      await loadLinks();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    }
  };

  const previewByid = useMemo(() => {
    const m = new Map<string, WbSessionCopyPreview["items"][number]>();
    for (const it of preview?.items ?? []) m.set(it.id, it);
    return m;
  }, [preview]);

  return (
    <div className="mx-auto w-full max-w-6xl space-y-4 px-4 py-6">
      <Card>
        <CardHeader className="flex flex-row items-start justify-between gap-4 space-y-0">
          <div className="space-y-1">
            <CardTitle className="flex items-center gap-2 text-base">
              <ScrollText className="size-4" />
              WorkBuddy 会话记录
            </CardTitle>
            <CardDescription>
              直接读 <code className="text-xs">~/.workbuddy/workbuddy.db</code> 的{" "}
              <code className="text-xs">sessions</code> 表与{" "}
              <code className="text-xs">projects/*.jsonl</code> 正文，按账号 uid 分组。
            </CardDescription>
          </div>
          <div className="flex shrink-0 flex-col items-end gap-1">
            <Button
              variant="outline"
              size="sm"
              onClick={() => void load(true)}
              disabled={loading || refreshing}
              title="重新读取会话库（会绕过缓存）"
            >
              <RefreshCw className={cn("size-4", (loading || refreshing) && "animate-spin")} />
              刷新
            </Button>
            {/* 缓存状态必须可见：用户得知道眼前这份是「刚读的」还是「上次的」。 */}
            {loadedAt > 0 && (
              <span className="text-xs text-muted-foreground">
                {refreshing ? "后台更新中…" : `读取于 ${new Date(loadedAt).toLocaleTimeString("zh-CN", { hour12: false })}`}
              </span>
            )}
          </div>
        </CardHeader>
        {staleNote && (
          <CardContent className="pb-0">
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertTitle>这次刷新失败，显示的是上一次读取的数据</AlertTitle>
              <AlertDescription className="break-all">
                {staleNote}
                {loadedAt > 0 &&
                  `（数据读取于 ${new Date(loadedAt).toLocaleString("zh-CN", { hour12: false })}）`}
              </AlertDescription>
            </Alert>
          </CardContent>
        )}
        {error && (
          <CardContent>
            <Alert variant="destructive">
              <AlertTriangle className="size-4" />
              <AlertTitle>读取失败</AlertTitle>
              <AlertDescription className="break-all">{error}</AlertDescription>
            </Alert>
          </CardContent>
        )}
      </Card>

      <Tabs defaultValue="records">
        <TabsList>
          <TabsTrigger value="records">会话记录</TabsTrigger>
          <TabsTrigger value="copy">复制与关联</TabsTrigger>
          <TabsTrigger value="groups">关联会话</TabsTrigger>
        </TabsList>

        {/* --------------------------------------------------------------- */}
        <TabsContent value="records" className="space-y-4">
          <Card>
            <CardHeader className="pb-3">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-sm text-muted-foreground">账号</span>
                <select
                  className="h-9 min-w-[220px] rounded-md border border-input bg-background px-3 text-sm"
                  value={uid}
                  onChange={(e) => {
                    setUid(e.target.value);
                    // 换账号 = 换一批工作空间，旧的筛选键在新账号里没有意义。
                    setWsFilter("");
                    setSelected(new Set());
                  }}
                >
                  {accounts.map((a) => (
                    <option key={a.uid} value={a.uid}>
                      {accountLabel(a, a.uid)} · {a.count} 个会话
                    </option>
                  ))}
                </select>

                {/* 搜索：只搜元数据，正文不在这里（占位符里写清） */}
                <div className="relative">
                  <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    value={query}
                    onChange={(e) => {
                      setQuery(e.target.value);
                      setSelected(new Set());
                    }}
                    placeholder="搜索标题 / 工作空间 / 会话 id"
                    className="h-9 w-[250px] pl-8 pr-8"
                  />
                  {query && (
                    <button
                      type="button"
                      title="清除搜索"
                      className="text-muted-foreground absolute top-1/2 right-2 -translate-y-1/2"
                      onClick={() => {
                        setQuery("");
                        setSelected(new Set());
                      }}
                    >
                      <X className="size-3.5" />
                    </button>
                  )}
                </div>

                {/* 工作空间筛选：数量来自当前列表（与分组表头同源） */}
                <select
                  className="h-9 min-w-[190px] rounded-md border border-input bg-background px-3 text-sm"
                  value={wsFilter}
                  onChange={(e) => {
                    setWsFilter(e.target.value);
                    setSelected(new Set());
                  }}
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
                <div className="ml-auto flex items-center gap-0.5 rounded-md border p-0.5">
                  <button
                    type="button"
                    className={cn(
                      "flex items-center gap-1 rounded px-2 py-1 text-xs",
                      byWorkspace
                        ? "bg-muted font-medium text-foreground"
                        : "text-muted-foreground hover:bg-muted/60",
                    )}
                    onClick={() => setByWorkspace(true)}
                  >
                    <Layers className="size-3.5" />
                    按空间
                  </button>
                  <button
                    type="button"
                    className={cn(
                      "flex items-center gap-1 rounded px-2 py-1 text-xs",
                      !byWorkspace
                        ? "bg-muted font-medium text-foreground"
                        : "text-muted-foreground hover:bg-muted/60",
                    )}
                    onClick={() => setByWorkspace(false)}
                  >
                    <LayoutList className="size-3.5" />
                    平铺
                  </button>
                </div>
                <Badge variant="secondary">
                  {filtered.length === sessions.length
                    ? `${sessions.length} 条`
                    : `${filtered.length} / ${sessions.length} 条`}
                </Badge>
                {wsOptions.length > 0 && (
                  <Badge variant="outline">{wsOptions.length} 个空间</Badge>
                )}
              </div>
            </CardHeader>
            <CardContent className="space-y-3">
              {loading ? (
                <Skeleton className="h-40 w-full" />
              ) : sessions.length === 0 ? (
                <p className="py-6 text-center text-sm text-muted-foreground">
                  该账号下没有未删除的会话
                </p>
              ) : filtered.length === 0 ? (
                /* ⚠️「筛选后为空」与「本来就没有会话」必须长得不一样（硬规则：空 ≠ 失败）：
                   前者给一个清除筛选的出口，后者说明库里确实没有。 */
                <div className="space-y-2 py-6 text-center">
                  <p className="text-sm text-muted-foreground">
                    没有匹配的会话 —— 筛选后 0 条，该账号共 {sessions.length} 条
                  </p>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      setQuery("");
                      setWsFilter("");
                      setSelected(new Set());
                    }}
                  >
                    清除搜索与筛选
                  </Button>
                </div>
              ) : (
                <div className="rounded-md border">
                  <table className="w-full text-sm">
                    <thead className="border-b bg-muted/40 text-xs text-muted-foreground">
                      <tr>
                        <th className="w-10 px-3 py-2">
                          <input
                            type="checkbox"
                            className="size-4 cursor-pointer align-middle"
                            aria-label="全选当前列表"
                            title="全选当前列表"
                            checked={allSelected}
                            ref={(el) => {
                              if (el) el.indeterminate = pickedVisible > 0 && !allSelected;
                            }}
                            onChange={toggleAll}
                          />
                        </th>
                        <th className="px-3 py-2 text-left font-medium">标题</th>
                        <th className="px-3 py-2 text-left font-medium">更新时间</th>
                        <th className="px-3 py-2 text-left font-medium">正文</th>
                        <th className="w-40 px-3 py-2 text-right font-medium">操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {renderGroups.map((g) => (
                        <Fragment key={g.key}>
                          {/* 分组表头（只有「按空间」视图才画）。点一下 = 只看这个空间，
                              相当于把上面的下拉设成它 —— 不用再去下拉里找。 */}
                          {byWorkspace && (
                            <tr className="border-b bg-muted/50">
                              <td colSpan={5} className="px-3 py-1.5">
                                <button
                                  type="button"
                                  className="flex w-full items-center gap-2 text-left"
                                  title="只看这个工作空间"
                                  onClick={() => {
                                    setWsFilter(g.key);
                                    setSelected(new Set());
                                  }}
                                >
                                  <FolderOpen className="size-3.5 shrink-0 text-muted-foreground" />
                                  <span className="shrink-0 text-xs font-semibold">{g.label}</span>
                                  <Badge variant="secondary" className="shrink-0">
                                    {g.items.length}
                                  </Badge>
                                  <span
                                    className="truncate text-xs text-muted-foreground"
                                    title={g.path}
                                  >
                                    {g.path}
                                  </span>
                                </button>
                              </td>
                            </tr>
                          )}
                          {g.items.map((s) => (
                            <tr key={s.id} className="border-b last:border-0 hover:bg-muted/30">
                              <td className="px-3 py-2">
                                <input
                                  type="checkbox"
                                  className="size-4"
                                  checked={selected.has(s.id)}
                                  onChange={() => toggle(s.id, setSelected, selected)}
                                />
                              </td>
                              <td className="max-w-[420px] px-3 py-2">
                                <div className="truncate font-medium">{s.title}</div>
                                <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
                                  {!byWorkspace && (
                                    <span className="inline-flex shrink-0 items-center gap-1 rounded border px-1 py-0.5">
                                      <FolderOpen className="size-3" />
                                      {s.workspace || "（未归属）"}
                                    </span>
                                  )}
                                  <span className="truncate" title={s.workspacePath}>
                                    {s.workspacePath || "（没有工作目录）"}
                                  </span>
                                </div>
                              </td>
                              <td className="whitespace-nowrap px-3 py-2 text-xs text-muted-foreground">
                                {timeLabel(s.updatedAt)}
                              </td>
                              <td className="whitespace-nowrap px-3 py-2 text-xs text-muted-foreground">
                                {s.hasBody ? humanSize(s.bodyBytes) : "无正文"}
                              </td>
                              <td className="px-3 py-2 text-right">
                                <div className="flex justify-end gap-1">
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    title="编辑这条会话"
                                    disabled={!s.hasBody || busy === s.id}
                                    onClick={() => {
                                      setEditSeed(s.id);
                                      setEditOpen(true);
                                    }}
                                  >
                                    <Pencil className="size-4" />
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    title="查看详情"
                                    disabled={busy === s.id}
                                    onClick={() => void openDetail(s)}
                                  >
                                    {busy === s.id ? (
                                      <Loader2 className="size-4 animate-spin" />
                                    ) : (
                                      <Eye className="size-4" />
                                    )}
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    disabled={!s.hasBody || busy === s.id}
                                    onClick={() => void exportOne(s)}
                                  >
                                    <FileDown className="size-4" />
                                  </Button>
                                  <Button
                                    variant="ghost"
                                    size="sm"
                                    className="text-destructive"
                                    onClick={() => setConfirmDelete([s])}
                                  >
                                    <Trash2 className="size-4" />
                                  </Button>
                                </div>
                              </td>
                            </tr>
                          ))}
                        </Fragment>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}

              {selected.size > 0 && (
                <div className="flex items-center justify-between rounded-md border bg-muted/40 px-3 py-2 text-sm">
                  <span>已选 {selected.size} 个会话</span>
                  <Button
                    variant="destructive"
                    size="sm"
                    disabled={busy === "delete"}
                    onClick={() =>
                      setConfirmDelete(sessions.filter((s) => selected.has(s.id)))
                    }
                  >
                    {busy === "delete" ? <Loader2 className="size-4 animate-spin" /> : <Trash2 className="size-4" />}
                    删除所选
                  </Button>
                </div>
              )}
            </CardContent>
          </Card>

          {deleteReport && (
            <Card>
              <CardHeader className="pb-2">
                <CardTitle className="text-sm">上次删除结果</CardTitle>
              </CardHeader>
              <CardContent className="space-y-1 text-xs text-muted-foreground">
                <p>删除 {deleteReport.count} 个，备份：{deleteReport.backup}</p>
                <p>回收站：{deleteReport.trashDir}（正文可还原）</p>
                {deleteReport.killed.length > 0 && <p>结束进程：{deleteReport.killed.join("、")}</p>}
                {deleteReport.moveErrors.length > 0 && (
                  <p className="text-destructive">正文移动失败：{deleteReport.moveErrors.join("；")}</p>
                )}
              </CardContent>
            </Card>
          )}
        </TabsContent>

        {/* --------------------------------------------------------------- */}
        <TabsContent value="copy" className="space-y-4">
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-base">会话复制</CardTitle>
              <CardDescription>
                把源账号勾选的会话以<b>新 id</b> 复制给目标账号：源账号数据一行不改，
                副本正文里的会话 id 同步替换为新 id。
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="flex flex-wrap items-center gap-3">
                <div className="flex items-center gap-2">
                  <span className="text-sm text-muted-foreground">源账号</span>
                  <select
                    className="h-9 min-w-[200px] rounded-md border border-input bg-background px-3 text-sm"
                    value={srcUid}
                    onChange={(e) => {
                      setSrcUid(e.target.value);
                      setCopySel(new Set());
                    }}
                  >
                    {accounts.map((a) => (
                      <option key={a.uid} value={a.uid}>
                        {accountLabel(a, a.uid)} · {a.count}
                      </option>
                    ))}
                  </select>
                </div>
                <span className="text-muted-foreground">→</span>
                <div className="flex items-center gap-2">
                  <span className="text-sm text-muted-foreground">目标账号</span>
                  <select
                    className="h-9 min-w-[200px] rounded-md border border-input bg-background px-3 text-sm"
                    value={dstUid}
                    onChange={(e) => setDstUid(e.target.value)}
                  >
                    {accounts.map((a) => (
                      <option key={a.uid} value={a.uid}>
                        {accountLabel(a, a.uid)} · {a.count}
                      </option>
                    ))}
                  </select>
                </div>
              </div>

              {srcUid && dstUid && srcUid === dstUid && (
                <Alert>
                  <AlertTriangle className="size-4" />
                  <AlertTitle>源账号与目标账号相同</AlertTitle>
                  <AlertDescription>请选择一个不同的目标账号。</AlertDescription>
                </Alert>
              )}

              {preview?.blocked && (
                <Alert variant="destructive">
                  <AlertTriangle className="size-4" />
                  <AlertTitle>已阻止写入</AlertTitle>
                  <AlertDescription>{preview.reason}</AlertDescription>
                </Alert>
              )}

              <div className="max-h-80 overflow-auto rounded-md border">
                <table className="w-full text-sm">
                  <thead className="sticky top-0 border-b bg-muted/40 text-xs text-muted-foreground">
                    <tr>
                      <th className="w-10 px-3 py-2" />
                      <th className="px-3 py-2 text-left font-medium">会话</th>
                      <th className="w-32 px-3 py-2 text-left font-medium">复制状态</th>
                    </tr>
                  </thead>
                  <tbody>
                    {srcSessions.map((s) => {
                      const p = previewByid.get(s.id);
                      const disabled = !s.hasBody || (p ? !p.available : false);
                      return (
                        <tr key={s.id} className="border-b last:border-0 hover:bg-muted/30">
                          <td className="px-3 py-2">
                            <input
                              type="checkbox"
                              className="size-4"
                              disabled={disabled}
                              checked={copySel.has(s.id)}
                              onChange={() => toggle(s.id, setCopySel, copySel)}
                            />
                          </td>
                          <td className="max-w-[420px] px-3 py-2">
                            <div className="truncate font-medium">{s.title}</div>
                            <div className="truncate text-xs text-muted-foreground">
                              {timeLabel(s.updatedAt)} · {humanSize(s.bodyBytes)}
                            </div>
                          </td>
                          <td className="px-3 py-2 text-xs">
                            {p?.alreadyLinked ? (
                              <Badge variant="secondary">已有副本</Badge>
                            ) : p && !p.available ? (
                              <span className="text-destructive">{p.reason}</span>
                            ) : !s.hasBody ? (
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

              <div className="flex items-center justify-between rounded-md border bg-muted/40 px-3 py-2 text-sm">
                <span>已选 {copySel.size} 个会话</span>
                <Button
                  size="sm"
                  disabled={
                    busy === "copy" ||
                    copySel.size === 0 ||
                    srcUid === dstUid ||
                    !!preview?.blocked
                  }
                  onClick={() => void runCopy()}
                >
                  {busy === "copy" ? <Loader2 className="size-4 animate-spin" /> : <Copy className="size-4" />}
                  复制到目标账号
                </Button>
              </div>
            </CardContent>
          </Card>

          {copyReport && (
            <Card>
              <CardHeader className="pb-2">
                <CardTitle className="text-sm">上次复制结果</CardTitle>
              </CardHeader>
              <CardContent className="space-y-1 text-xs text-muted-foreground">
                <p>复制 {copyReport.count} 个，备份：{copyReport.backup}</p>
                {copyReport.skipped.length > 0 && (
                  <p>跳过 {copyReport.skipped.length} 个（目标账号已有副本）</p>
                )}
                <p>{copyReport.note}</p>
                {copyReport.edgeSyncSkipped.length > 0 && (
                  <p>
                    未写入的云端映射库：
                    {copyReport.edgeSyncSkipped.map((d) => d.name).join("、")}
                  </p>
                )}
              </CardContent>
            </Card>
          )}

          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-base">跨账号关联</CardTitle>
              <CardDescription>
                复制过的会话会自动登记成一个关联组，这里可以看到各账号副本的当前状态。
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-3">
              {/* ── 全账号互通 ───────────────────────────────────────────
                  关联组 = 「同一段对话在各账号上的副本」。要把本机所有会话都做成关联，
                  只能靠复制补齐；这一块就是「不用再一个个手动选、手动复制」的入口。
                  开关落在 settings.json（后端 `auto_link_workbuddy_sessions`），
                  切号流程自己会问后端，页面关着也生效。 */}
              <div className="rounded-lg border bg-muted/30 px-3 py-3">
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="text-sm font-medium">全账号互通</div>
                    <p className="mt-1 text-xs text-muted-foreground">
                      把本机所有会话补一份到其它账号，并登记成关联组 —— 之后任意账号登录都能看到
                      同一批会话。已同步过的不会重复复制；在某个账号里主动删掉的副本不会被自动恢复。
                    </p>
                  </div>
                  <Button onClick={() => void runLinkAll()} disabled={linkAllBusy}>
                    {linkAllBusy ? (
                      <Loader2 className="size-4 animate-spin" />
                    ) : (
                      <Link2 className="size-4" />
                    )}
                    一键互相关联
                  </Button>
                </div>
                {/* 预览与真跑同源（后端同一个计划函数）：说 N 个就是 N 个。 */}
                {linkPreview && !linkPreview.notNeeded && (
                  <p className="mt-2 text-xs text-muted-foreground">
                    {(linkPreview.planned ?? 0) > 0 ? (
                      <>
                        将复制 {linkPreview.planned} 个副本
                        {(linkPreview.byTarget?.length ?? 0) > 0 &&
                          `（${linkPreview.byTarget?.map((t) => `${t.label} ${t.count} 个`).join(" · ")}）`}
                        {(linkPreview.totalBytes ?? 0) > 0 &&
                          `，约 ${humanSize(linkPreview.totalBytes ?? 0)}`}
                        {(linkPreview.adopted ?? 0) > 0 &&
                          `；另 ${linkPreview.adopted} 组已有副本只需登记`}
                      </>
                    ) : (linkPreview.adopted ?? 0) > 0 ? (
                      <>
                        {linkPreview.adopted} 组已有副本只需登记，不会复制新副本
                      </>
                    ) : (
                      "没有需要补的会话"
                    )}
                  </p>
                )}
                {linkPreview?.clientRunning && (
                  <p className="mt-1 text-xs text-amber-600">
                    WorkBuddy 正在运行 —— 这一步要写会话库，请先退出（用本工具切号时会自动关它）
                  </p>
                )}
                <label className="mt-3 flex cursor-pointer items-start gap-2.5 border-t pt-3">
                  <input
                    type="checkbox"
                    className="mt-0.5"
                    checked={autoLink ?? false}
                    disabled={autoLink === null}
                    onChange={(e) => void toggleAutoLink(e.target.checked)}
                  />
                  <span className="min-w-0">
                    <span className="text-sm font-medium">切换账号时自动同步所有账号</span>
                    <span className="mt-0.5 block text-xs text-muted-foreground">
                      开启后每次切号（WorkBuddy 已关闭、还没重启的那一刻）自动同步
                      <b className="font-medium text-foreground">之后新产生的会话</b>
                      —— 历史存量请先用上面的「一键互相关联」补齐一次（要复制几个 GB，
                      所以不放进切号这种高频动作里）。这一步会写会话库，默认关闭。
                      {autoLink === null && (
                        <span className="text-destructive">
                          {" "}
                          （设置读取失败，开关不可用 —— 这不是「已关闭」）
                        </span>
                      )}
                    </span>
                  </span>
                </label>
                {linkAllReport && (
                  <div className="mt-3 space-y-1 border-t pt-3 text-xs text-muted-foreground">
                    {linkAllReport.notNeeded ? (
                      <p>{linkAllReport.reason || "没有需要补的会话"}</p>
                    ) : (
                      <>
                        {(linkAllReport.copied ?? 0) > 0 && (
                          <p>
                            本次复制 {linkAllReport.copied} 个副本（跳过{" "}
                            {linkAllReport.skipped ?? 0} 个已同步的）
                          </p>
                        )}
                        {(linkAllReport.adopted ?? 0) > 0 && (
                          <p>
                            登记 {linkAllReport.adopted} 组已有副本
                            （目标账号本来就有同一段对话，没有复制）
                          </p>
                        )}
                        {(linkAllReport.alreadyThere ?? 0) > 0 && (
                          <p>
                            {linkAllReport.alreadyThere} 个账号的同内容副本已属于别的关联组，未改动
                          </p>
                        )}
                        {(linkAllReport.byTarget?.length ?? 0) > 0 && (
                          <p>
                            {linkAllReport.byTarget
                              ?.map((t) => `${t.label} ${t.count} 个`)
                              .join(" · ")}
                          </p>
                        )}
                        {(linkAllReport.noBody ?? 0) > 0 && (
                          <p>
                            {linkAllReport.noBody} 个会话没有正文文件，未复制（没有正文就搬不过去）
                          </p>
                        )}
                        {linkAllReport.backup && <p>备份：{linkAllReport.backup}</p>}
                      </>
                    )}
                  </div>
                )}
              </div>

              {links?.error ? (
                <Alert variant="destructive">
                  <AlertTriangle className="size-4" />
                  <AlertDescription>{links.error}</AlertDescription>
                </Alert>
              ) : !links || links.groups.length === 0 ? (
                <p className="py-4 text-center text-sm text-muted-foreground">
                  这两个账号之间还没有关联记录
                </p>
              ) : (
                <div className="rounded-md border">
                  <table className="w-full text-sm">
                    <thead className="border-b bg-muted/40 text-xs text-muted-foreground">
                      <tr>
                        <th className="px-3 py-2 text-left font-medium">会话</th>
                        <th className="w-32 px-3 py-2 text-left font-medium">状态</th>
                        <th className="w-40 px-3 py-2 text-left font-medium">源副本</th>
                        <th className="w-40 px-3 py-2 text-left font-medium">目标副本</th>
                        <th className="w-20 px-3 py-2 text-right font-medium">操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {links.groups.map((g) => {
                        const v = VERDICT[g.verdict] ?? VERDICT.gone;
                        return (
                          <tr key={g.groupId} className="border-b last:border-0">
                            <td className="max-w-[320px] px-3 py-2">
                              <div className="truncate font-medium">
                                {(g.source.title ?? g.target.title ?? "(无标题)") as string}
                              </div>
                              <div className="truncate text-xs text-muted-foreground">
                                {shortId(g.source.sessionId)}
                              </div>
                            </td>
                            <td className="px-3 py-2">
                              <span className={cn("inline-block whitespace-nowrap rounded border px-2 py-0.5 text-xs", v.tone)}>
                                {v.label}
                              </span>
                            </td>
                            <td className="px-3 py-2 text-xs text-muted-foreground">
                              {g.source.alive ? timeLabel(g.source.updatedAt) : "已失效"}
                            </td>
                            <td className="px-3 py-2 text-xs text-muted-foreground">
                              {g.target.alive ? timeLabel(g.target.updatedAt) : "无副本"}
                            </td>
                            <td className="px-3 py-2 text-right">
                              <Button
                                variant="ghost"
                                size="sm"
                                onClick={() => void unlink(g)}
                              >
                                <Link2Off className="size-4" />
                              </Button>
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

          {edgeDbs.length > 0 && (
            <Card>
              <CardHeader className="pb-2">
                <CardTitle className="text-sm">云端映射库（只读列出，从不写入）</CardTitle>
              </CardHeader>
              <CardContent className="space-y-1 text-xs text-muted-foreground">
                {edgeDbs.map((d) => (
                  <p key={d.path}>
                    {d.name} · {humanSize(d.size)}
                  </p>
                ))}
                <Separator className="my-2" />
                <p>
                  写入这些库会让 edge-sync 判定「已迁移」并跳过上传，云端将永久缺会话，
                  因此本工具对它们只读取、不写入。
                </p>
              </CardContent>
            </Card>
          )}
        </TabsContent>

        <TabsContent value="groups" className="space-y-4">
          {/* 按「同一段对话」聚合各账号副本：看谁落后、谁分叉，并直接补齐/统一。 */}
          <WorkbuddySessionGroups />
        </TabsContent>
      </Tabs>

      {/* 详情（会话正文很长，默认放大并可拖拽调节） */}
      <Dialog open={!!detail} onOpenChange={(o) => !o && setDetail(null)}>
        <ResizableDialogContent storageKey="wb-session-detail">
          <DialogHeader>
            <DialogTitle className="truncate">
              {detail?.session?.title ?? "会话详情"}
            </DialogTitle>
            <DialogDescription className="break-all text-xs">
              {detail?.session?.id}
            </DialogDescription>
          </DialogHeader>
          <div className="min-h-0 space-y-3 overflow-auto pr-1 text-sm">
            {detail?.bodyError && (
              <Alert variant="destructive">
                <AlertTriangle className="size-4" />
                <AlertDescription>{detail.bodyError}</AlertDescription>
              </Alert>
            )}
            <p className="text-xs text-muted-foreground">
              {detail?.turnCount ?? 0} 个回合 · {detail?.session?.cwd}
            </p>
            {(detail?.turns ?? []).slice(0, 50).map((t, i) => (
              <div key={i} className="rounded-md border p-3">
                <div className="mb-1 text-xs font-medium text-muted-foreground">回合 {i + 1}</div>
                <div className="mb-2 whitespace-pre-wrap">
                  <span className="text-xs text-muted-foreground">提问：</span>
                  {t.userText || "(无正文)"}
                </div>
                <div className="whitespace-pre-wrap">
                  <span className="text-xs text-muted-foreground">回答：</span>
                  {t.assistantText || "(无正文)"}
                </div>
                {t.toolCalls > 0 && (
                  <div className="mt-2 text-xs text-muted-foreground">
                    过程工具调用 {t.toolCalls} 次
                  </div>
                )}
              </div>
            ))}
            {(detail?.turns?.length ?? 0) > 50 && (
              <p className="text-xs text-muted-foreground">
                仅显示前 50 个回合，完整内容请用导出 Markdown。
              </p>
            )}
          </div>
        </ResizableDialogContent>
      </Dialog>

      {/* T46 · S4：WorkBuddy 会话编辑器（与 Trae 侧同构，但模型完全不同：
          元数据是 sessions 一张表，正文是一行一个事件的 JSONL）。 */}
      <WorkbuddySessionEditor
        open={editOpen}
        onOpenChange={(o) => {
          setEditOpen(o);
          // 关闭时重读列表：可能有覆写/新增，标题与行数都变了。
          if (!o) void load(true);
        }}
        uid={uid || null}
        sessionId={editSeed}
        onDeleted={() => {
          // 走到这里说明会话已经被整条删掉了（删「最后一条用户提问」所在回合时的改道）。
          // 种子里那条 id 已经不存在，必须清掉；列表由 onOpenChange(false) 那条路重读。
          setEditSeed(null);
        }}
      />

      {/* 删除确认 */}
      <Dialog open={!!confirmDelete} onOpenChange={(o) => !o && setConfirmDelete(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认删除 {confirmDelete?.length} 个会话？</DialogTitle>
            <DialogDescription>
              会话行会被软删（客户端立即看不到），正文文件移入本工具回收站，可随时还原。
              操作会先结束 WorkBuddy 进程并整份备份会话库。
            </DialogDescription>
          </DialogHeader>
          <div className="max-h-48 space-y-1 overflow-auto text-sm">
            {(confirmDelete ?? []).map((s) => (
              <p key={s.id} className="truncate">
                · {s.title}
              </p>
            ))}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmDelete(null)}>
              取消
            </Button>
            <Button variant="destructive" disabled={busy === "delete"} onClick={() => void runDelete()}>
              {busy === "delete" ? <Loader2 className="size-4 animate-spin" /> : <Trash2 className="size-4" />}
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* ── 一键互通的确认框 ────────────────────────────────────────────
          本机实测一次可能是「74 个副本 / 约 2.98 GB」。这种量级必须先把数字摆出来，
          再让用户选「复制」还是「只登记（0 字节）」。点一下就默默写几 GB 是不可接受的。 */}
      <Dialog open={confirmLink} onOpenChange={setConfirmLink}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>要把会话复制到其它账号吗？</DialogTitle>
            <DialogDescription>
              把本机会话补一份给其它账号，并登记成关联组 —— 之后任意账号登录都能看到同一批会话。
              源会话不会被改动；已同步过的不会重复复制。
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-1 rounded-md border bg-muted/40 px-3 py-2 text-sm">
            <p>
              将复制 <b>{linkPreview?.planned ?? 0}</b> 个会话副本
              {(linkPreview?.totalBytes ?? 0) > 0 && (
                <>（正文共约 <b>{humanSize(linkPreview?.totalBytes ?? 0)}</b>）</>
              )}
            </p>
            {(linkPreview?.byTarget?.length ?? 0) > 0 && (
              <p className="text-xs text-muted-foreground">
                写入：{linkPreview?.byTarget?.map((t) => `${t.label} ${t.count} 个`).join(" · ")}
              </p>
            )}
            {(linkPreview?.adopted ?? 0) > 0 && (
              <p className="text-xs text-muted-foreground">
                另有 {linkPreview?.adopted} 组「同一段对话已各有一份」，只需登记、不占空间。
              </p>
            )}
            <p className="text-xs text-muted-foreground">
              会先整份备份会话库；写入期间 WorkBuddy 必须处于关闭状态。
            </p>
          </div>
          <DialogFooter className="gap-2 sm:justify-between">
            <Button
              variant="outline"
              disabled={linkAllBusy}
              onClick={() => void runLinkExisting()}
              title="只把「同一段对话已有副本」登记成关联组，不复制任何文件"
            >
              只登记已有副本（0 字节）
            </Button>
            <div className="flex gap-2">
              <Button variant="ghost" onClick={() => setConfirmLink(false)}>
                取消
              </Button>
              <Button disabled={linkAllBusy} onClick={() => void runLinkAll(true)}>
                {linkAllBusy ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <Link2 className="size-4" />
                )}
                确认复制并关联
              </Button>
            </div>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
