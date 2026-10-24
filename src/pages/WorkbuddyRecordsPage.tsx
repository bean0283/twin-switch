import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  Copy,
  Eye,
  FileDown,
  Link2,
  Link2Off,
  Loader2,
  Pencil,
  RefreshCw,
  ScrollText,
  Trash2,
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
  const [accounts, setAccounts] = useState<WbAccountGroup[]>([]);
  const [uid, setUid] = useState("");
  const [loading, setLoading] = useState(true);
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

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await api.workbuddySessionListByAccount();
      if (!res.ok) throw new Error(res.error || "读取会话列表失败");
      setAccounts(res.accounts || []);
      setUid((prev) => prev || res.accounts?.[0]?.uid || "");
      setSrcUid((prev) => prev || res.accounts?.[0]?.uid || "");
      setDstUid((prev) => prev || res.accounts?.[1]?.uid || res.accounts?.[0]?.uid || "");
      const ed = await api.workbuddySessionEdgeSyncDbs();
      setEdgeDbs(ed.items || []);
      // 预览与主流程并行、失败也不影响列表（但绝不显示旧/假数字：失败就清空）。
      void api
        .workbuddySessionLinkAllPreview()
        .then((p) => setLinkPreview(p.ok ? p : null))
        .catch(() => setLinkPreview(null));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
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

  /** 全选：会话列表当前账号下的全部行（与 Trae 会话记录页同款）。 */
  const allSelected = sessions.length > 0 && sessions.every((s) => selected.has(s.id));
  const toggleAll = () => {
    setSelected(allSelected ? new Set() : new Set(sessions.map((s) => s.id)));
  };

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
      await load();
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
      await load();
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
    await load();
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
          <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
            <RefreshCw className={cn("size-4", loading && "animate-spin")} />
            刷新
          </Button>
        </CardHeader>
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
              <div className="flex flex-wrap items-center gap-3">
                <span className="text-sm text-muted-foreground">账号</span>
                <select
                  className="h-9 min-w-[260px] rounded-md border border-input bg-background px-3 text-sm"
                  value={uid}
                  onChange={(e) => {
                    setUid(e.target.value);
                    setSelected(new Set());
                  }}
                >
                  {accounts.map((a) => (
                    <option key={a.uid} value={a.uid}>
                      {accountLabel(a, a.uid)} · {a.count} 个会话
                    </option>
                  ))}
                </select>
                <Badge variant="secondary">{sessions.length} 条</Badge>
              </div>
            </CardHeader>
            <CardContent className="space-y-3">
              {loading ? (
                <Skeleton className="h-40 w-full" />
              ) : sessions.length === 0 ? (
                <p className="py-6 text-center text-sm text-muted-foreground">
                  该账号下没有未删除的会话
                </p>
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
                              if (el) el.indeterminate = selected.size > 0 && !allSelected;
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
                      {sessions.map((s) => (
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
                            <div className="truncate text-xs text-muted-foreground">{s.cwd}</div>
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
          if (!o) void load();
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
