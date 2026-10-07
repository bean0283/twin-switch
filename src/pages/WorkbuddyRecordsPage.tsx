import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  Copy,
  Eye,
  FileDown,
  Link2Off,
  Loader2,
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

  // 复制与关联
  const [srcUid, setSrcUid] = useState("");
  const [dstUid, setDstUid] = useState("");
  const [copySel, setCopySel] = useState<Set<string>>(new Set());
  const [preview, setPreview] = useState<WbSessionCopyPreview | null>(null);
  const [links, setLinks] = useState<WbLinksPreview | null>(null);
  const [copyReport, setCopyReport] = useState<WbSessionCopyReport | null>(null);
  const [edgeDbs, setEdgeDbs] = useState<{ name: string; path: string; size: number }[]>([]);

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
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

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
    </div>
  );
}
