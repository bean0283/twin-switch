import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  CheckCircle2,
  Database,
  Eraser,
  FolderOpen,
  HardDrive,
  Loader2,
  RefreshCw,
  ShieldAlert,
  Trash2,
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
  TraeWbCleanupItem,
  TraeWbCleanupReport,
  TraeWbCleanupScan,
} from "@/lib/trae-types";

/** 缓存时间的人话描述：`2 分钟前`。 */
function agoLabel(ms: number): string {
  if (ms < 60_000) return "刚刚";
  const min = Math.floor(ms / 60_000);
  if (min < 60) return `${min} 分钟前`;
  return `${Math.floor(min / 60)} 小时前`;
}

function humanSize(bytes: number): string {
  if (bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function formatTime(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/**
 * WorkBuddy 本机垃圾清理。
 *
 * 导入列表里那些「无正文」「已删除」的会话，在 `workbuddy.db` 里都是**残留行**：
 * 占着列表位置，也占着磁盘。本页把它们分类列出，用户在逐项确认后清理。
 *
 * 两层保险：扫描本身只读；清除时先退出 WorkBuddy、整份备份数据库，文件默认**移入回收站**
 * 而不是直接删除，只有显式勾选「彻底删除」才真正 unlink。
 */
export default function WorkbuddyCleanupPage() {
  const [scan, setScan] = useState<TraeWbCleanupScan | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const [busy, setBusy] = useState(false);
  const [busyLabel, setBusyLabel] = useState("");
  const [progress, setProgress] = useState<string[]>([]);
  const [report, setReport] = useState<TraeWbCleanupReport | null>(null);

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [hard, setHard] = useState(false);

  useEffect(() => {
    // 与其它进度面板同样的 `listen()` 竞态：异步注册期间组件若已卸载，
    // 清理函数拿不到注销句柄，会残留监听器把每条进度投递两遍。
    let active = true;
    let un: (() => void) | undefined;
    void listen<{ line: string }>("trae-wb-cleanup-progress", (e) => {
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

  const load = useCallback(async (keepSelection = false, force = false) => {
    setLoading(true);
    setError(null);
    try {
      // force=false：**有缓存就用缓存，永不自动重扫**（哪怕缓存是几天前的）。
      // 进页面必须毫秒级出盘面；新鲜数据由「启动时首页后台扫」+ 这里的「重新扫描」
      // 按钮 + 清理后自动重扫这三条路径负责。
      const s = await api.traeWbCleanupScan(force);
      setScan(s);
      if (!keepSelection) {
        // 默认勾选「推荐清理」项（会话类残留恒推荐；日志类只推荐超过保留窗口的）
        const preset = new Set<string>();
        for (const c of s.categories) {
          for (const it of c.items) if (it.recommended) preset.add(it.id);
        }
        setSelected(preset);
      } else {
        setSelected((prev) => {
          const alive = new Set<string>();
          for (const c of s.categories) for (const it of c.items) alive.add(it.id);
          return new Set([...prev].filter((id) => alive.has(id)));
        });
      }
    } catch (cause) {
      setError(api.asError(cause));
      setScan(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const allItems = useMemo(
    () => (scan?.categories ?? []).flatMap((c) => c.items),
    [scan],
  );
  const itemById = useMemo(() => {
    const m = new Map<string, TraeWbCleanupItem>();
    for (const it of allItems) m.set(it.id, it);
    return m;
  }, [allItems]);

  const selectedItems = useMemo(
    () => [...selected].map((id) => itemById.get(id)).filter((x): x is TraeWbCleanupItem => !!x),
    [selected, itemById],
  );
  const selectedBytes = useMemo(
    () => selectedItems.reduce((n, it) => n + it.bytes, 0),
    [selectedItems],
  );
  const selectedSessions = useMemo(
    () => selectedItems.filter((it) => it.kind === "session").length,
    [selectedItems],
  );

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleCategory(catKey: string) {
    const cat = scan?.categories.find((c) => c.key === catKey);
    if (!cat) return;
    const ids = cat.items.map((i) => i.id);
    const allOn = ids.every((id) => selected.has(id));
    setSelected((prev) => {
      const next = new Set(prev);
      for (const id of ids) {
        if (allOn) next.delete(id);
        else next.add(id);
      }
      return next;
    });
  }

  function selectRecommended() {
    const preset = new Set<string>();
    for (const c of scan?.categories ?? []) {
      for (const it of c.items) if (it.recommended) preset.add(it.id);
    }
    setSelected(preset);
  }

  async function emptyTrash() {
    if (busy) return;
    setBusy(true);
    setBusyLabel("清空回收站");
    try {
      const r = await api.traeWbCleanupEmptyTrash();
      // ⚠️「清空」是**允许部分成功**的：后端逐项删 + 短退避重试 + 先清只读，真正删不掉的
      //（被某个进程长期占用）才会进 `failed`。这时候报「失败」是错的（能释放的已经释放了），
      // 报「成功」也是错的（会让人以为全清了、卡片却还占着地方）。所以单独一条 warning，
      // 并把前几条失败原因写出来 —— 2026-10-09 那次「清空回收站失败: 拒绝访问。 (os error 5)」
      // 就是整棵树一把梭导致的：一个条目撞上瞬时占用 ⇒ 一个都没删掉。
      if (r.failed_count > 0) {
        toast.warning(`已释放 ${humanSize(r.bytes)}，但有 ${r.failed_count} 项删不掉`, {
          description: `${r.failed.slice(0, 3).join("；")}${
            r.failed.length > 3 ? " …" : ""
          }（多半被其他程序占用，关掉对应程序后再点一次）`,
        });
      } else {
        toast.success(`回收站已清空，释放 ${humanSize(r.bytes)}`, {
          description: `移除了 ${r.removed} 个条目`,
        });
      }
      // force=true：刚清完，缓存里的数字已经不准了（后端也已失效缓存），必须重扫。
      await load(true, true);
    } catch (cause) {
      toast.error("清空回收站失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  async function run() {
    if (busy || selectedItems.length === 0) return;
    setBusy(true);
    setBusyLabel(hard ? "彻底删除" : "清理到回收站");
    setProgress([]);
    try {
      const r = await api.traeWbCleanupPurge([...selected], hard);
      setReport(r);
      setConfirmOpen(false);
      toast.success(
        hard
          ? `已彻底删除，释放 ${humanSize(r.reclaimed_bytes)}`
          : `已清理 ${r.purged} 项（可恢复）`,
        {
          description: `删除 ${r.rows_deleted} 条记录 · 处理 ${r.files_removed} 个文件/目录 · 余下 ${r.verified_sessions} 个会话${r.relaunched ? " · 已重启 WorkBuddy" : ""}`,
        },
      );
      // force=true：同上，清理后必须拿实时盘面。
      await load(true, true);
    } catch (cause) {
      // 失败原因（尤其是「退出客户端」那一步）可能是一整句带 PID 与命令返回的
      // 说明 —— toast 里会被截断，所以同时落到页面级 Alert 上，并把进度行留在
      // 面板里（后端每轮失败原因都会 emit 一行）。
      const msg = api.asError(cause);
      setError(msg);
      toast.error("清理失败", { description: msg });
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  const totals = scan?.totals;

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">WorkBuddy 本机清理</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          清掉 WorkBuddy 数据目录里的残留：导入列表里选不中的「无正文」会话、已删除但没删干净的会话、
          孤儿正文与陈旧快照，以及占地方的诊断日志。清理前会先退出 WorkBuddy 并整份备份数据库。
        </p>
      </div>

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>操作失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {/* 占用大头：解释为什么 ~/.workbuddy 这么大 */}
      <Card>
        <CardHeader className="pb-2">
          {/*
            卡头用 grid 而不是 `flex flex-wrap`：右列是固定宽的按钮，左列是**会折行的段落**。
            段落放进 flex-wrap 行里时，参与折行判定的是它的 **max-content 宽**（整段排一行有多宽），
            而不是可用宽度 —— 文案一长就把按钮整个顶到第二行，哪怕窗口有 1600 px。
            `minmax(0,1fr)` 让左列「占满剩余宽度、不够就折行」，按钮永远待在右上角。
          */}
          <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-2">
            <div className="min-w-0">
              <CardTitle className="flex items-center gap-2 text-base">
                <HardDrive className="size-4" />
                {scan?.data_root ?? "~/.workbuddy"}
              </CardTitle>
              <CardDescription className="break-words">
                {scan ? (
                  <>
                    当前会话表 {totals?.sessions ?? 0} 条；可清理 {totals?.count ?? 0} 项，合计{" "}
                    {humanSize(totals?.bytes ?? 0)}
                    {totals && totals.recommended_count > 0 ? (
                      <>
                        ，其中
                        <span className="font-medium text-foreground">
                          {" "}
                          推荐清理 {totals.recommended_count} 项 /{" "}
                          {humanSize(totals.recommended_bytes)}
                        </span>
                        （保留最近 {scan.log_keep_days} 天的日志）
                      </>
                    ) : null}
                    {scan.cached ? (
                      <span
                        className={`ml-1 ${scan.stale ? "text-amber-600" : "text-muted-foreground"}`}
                      >
                        ·{" "}
                        {scan.stale
                          ? `数据为 ${agoLabel(scan.ageMs ?? 0)}，点「重新扫描」更新`
                          : `缓存于 ${agoLabel(scan.ageMs ?? 0)}`}
                      </span>
                    ) : null}
                  </>
                ) : (
                  "正在扫描…"
                )}
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                disabled={busy || loading}
                onClick={() => void load(false, true)}
              >
                {loading ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
                重新扫描
              </Button>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          {scan?.running ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertDescription>
                WorkBuddy 正在运行。开始清理后它会被关闭（清完自动重新打开），请先保存好正在编辑的内容。
              </AlertDescription>
            </Alert>
          ) : null}

          {scan && scan.large_holdings.length > 0 ? (
            <div className="space-y-1 rounded-lg border p-2">
              <div className="px-1 pb-1 text-xs text-muted-foreground">
                占用大头（只读说明——「可清」的项在下面有清理入口，其余请勿手动删）
              </div>
              {scan.large_holdings.map((h) => (
                <div
                  key={h.name}
                  className="flex flex-wrap items-baseline justify-between gap-2 rounded-md px-2 py-1 text-sm"
                >
                  <span className="min-w-0 flex-1 truncate">
                    <span className="font-medium">{h.name}</span>
                    <span className="ml-2 text-xs text-muted-foreground">{h.note}</span>
                  </span>
                  <span className="flex shrink-0 items-center gap-2">
                    <span className="tabular-nums text-muted-foreground">{humanSize(h.bytes)}</span>
                    {h.cleanable ? (
                      <Badge className="bg-emerald-500/15 text-[10px] text-emerald-600">可清</Badge>
                    ) : (
                      <Badge variant="secondary" className="text-[10px]">
                        勿清
                      </Badge>
                    )}
                  </span>
                </div>
              ))}
            </div>
          ) : null}

          {scan && scan.trash.count > 0 ? (
            <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-muted/50 px-3 py-2 text-xs">
              <span className="min-w-0 flex-1 break-all text-muted-foreground">
                回收站里还有 {scan.trash.count} 个条目（{humanSize(scan.trash.bytes)}），未被彻底删除。
                搬回原处即恢复；不再需要时可清空以真正释放空间。
              </span>
              <Button
                size="sm"
                variant="outline"
                disabled={busy}
                onClick={() => void emptyTrash()}
              >
                <Eraser className="size-4" />
                清空回收站
              </Button>
            </div>
          ) : null}
        </CardContent>
      </Card>

      {/* 可清理项 */}
      <Card>
        <CardHeader className="pb-2">
          <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-2">
            <div className="min-w-0">
              <CardTitle className="text-base">可清理项</CardTitle>
              <CardDescription>
                已选 {selectedItems.length} / {allItems.length} 项，合计 {humanSize(selectedBytes)}
                {selectedSessions > 0 ? `（含 ${selectedSessions} 个会话记录）` : ""}
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={busy || !totals?.recommended_count}
                onClick={selectRecommended}
              >
                全选推荐项
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={busy || selected.size === 0}
                onClick={() => setSelected(new Set())}
              >
                清空选择
              </Button>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          {loading ? (
            <Skeleton className="h-40 rounded-xl" />
          ) : allItems.length === 0 ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <CheckCircle2 className="size-4 text-emerald-600" />
              没有发现可清理的残留，本机 WorkBuddy 数据目录已经很干净。
            </div>
          ) : (
            (scan?.categories ?? [])
              .filter((c) => c.count > 0)
              .map((cat) => {
                const isCollapsed = collapsed.has(cat.key);
                const onCount = cat.items.filter((i) => selected.has(i.id)).length;
                return (
                  <div key={cat.key} className="rounded-lg border">
                    <div className="flex flex-wrap items-center justify-between gap-2 border-b px-3 py-2">
                      <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2">
                        <Checkbox
                          checked={onCount === cat.count && cat.count > 0}
                          onCheckedChange={() => toggleCategory(cat.key)}
                        />
                        <span className="min-w-0">
                          <span className="flex flex-wrap items-center gap-1.5 text-sm font-medium">
                            {cat.title}
                            <Badge variant="secondary" className="text-[10px]">
                              {cat.count} 项
                            </Badge>
                            <span className="text-xs font-normal text-muted-foreground tabular-nums">
                              {humanSize(cat.bytes)}
                            </span>
                          </span>
                          <span className="mt-0.5 block text-xs text-muted-foreground">
                            {cat.desc}
                          </span>
                        </span>
                      </label>
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() =>
                          setCollapsed((prev) => {
                            const next = new Set(prev);
                            if (next.has(cat.key)) next.delete(cat.key);
                            else next.add(cat.key);
                            return next;
                          })
                        }
                      >
                        {isCollapsed ? "展开" : "收起"}
                      </Button>
                    </div>
                    {isCollapsed ? null : (
                      <div className="max-h-72 overflow-auto p-2">
                        {cat.items.map((it) => (
                          <label
                            key={it.id}
                            className={cn(
                              "flex items-start gap-2.5 rounded-md px-2 py-1.5 transition-colors",
                              busy ? "opacity-50" : "cursor-pointer hover:bg-foreground/[0.03]",
                            )}
                          >
                            <Checkbox
                              className="mt-0.5"
                              checked={selected.has(it.id)}
                              disabled={busy}
                              onCheckedChange={() => toggle(it.id)}
                            />
                            <span className="min-w-0 flex-1">
                              <span className="flex flex-wrap items-center gap-1.5">
                                <span className="truncate font-medium">{it.title}</span>
                                {it.recommended ? (
                                  <Badge className="bg-emerald-500/15 text-[10px] text-emerald-600">
                                    推荐
                                  </Badge>
                                ) : null}
                              </span>
                              <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                                {it.updated_at ? `${formatTime(it.updated_at)} · ` : ""}
                                {humanSize(it.bytes)}
                                {it.owner ? ` · ${it.owner}` : ""}
                              </span>
                              <span className="block text-[11px] text-muted-foreground/80">
                                {it.detail}
                              </span>
                              {it.paths.length ? (
                                <span className="mt-0.5 block font-mono text-[11px] text-muted-foreground/70">
                                  {it.paths.length === 1
                                    ? it.paths[0]
                                    : `将处理 ${it.paths.length} 个路径：${it.paths[0]} 等`}
                                </span>
                              ) : null}
                            </span>
                          </label>
                        ))}
                      </div>
                    )}
                  </div>
                );
              })
          )}
        </CardContent>
      </Card>

      {/* 操作条 */}
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border bg-muted/40 px-4 py-3">
        <div className="text-sm text-muted-foreground">
          已选 <span className="font-medium text-foreground">{selectedItems.length}</span> 项 ·{" "}
          {humanSize(selectedBytes)}
          {busy ? (
            <span className="ml-3 inline-flex items-center gap-1.5 text-xs">
              <Loader2 className="size-3.5 animate-spin" />
              {busyLabel || "处理中…"}
            </span>
          ) : null}
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            disabled={busy || selectedItems.length === 0}
            onClick={() => {
              setHard(true);
              setConfirmOpen(true);
            }}
          >
            <ShieldAlert className="size-4" />
            彻底删除…
          </Button>
          <Button
            disabled={busy || selectedItems.length === 0}
            onClick={() => {
              setHard(false);
              setConfirmOpen(true);
            }}
          >
            <Trash2 className="size-4" />
            清理 {selectedItems.length} 项
          </Button>
        </div>
      </div>

      {progress.length ? (
        <pre className="max-h-60 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
          {progress.join("\n")}
        </pre>
      ) : null}

      {report ? (
        <Alert>
          <CheckCircle2 className="size-4" />
          <AlertTitle>清理完成</AlertTitle>
          <AlertDescription className="space-y-1">
            <span className="block">
              处理 {report.purged} 项（会话 {report.sessions} 个），删除记录 {report.rows_deleted} 条，
              移动/删除文件 {report.files_removed} 个
            </span>
            <span className="block">
              {report.hard
                ? `彻底删除，释放 ${humanSize(report.reclaimed_bytes)}`
                : `已移入回收站 ${humanSize(report.planned_bytes)}（清空回收站后才会真正释放）`}
            </span>
            {report.trash_dir ? (
              <span className="block break-all font-mono text-xs">回收站：{report.trash_dir}</span>
            ) : null}
            <span className="block break-all font-mono text-xs">备份：{report.backup_dir}</span>
            <span className="block text-xs">
              清理后会话表剩余 {report.verified_sessions} 条
              {report.relaunched ? " · 已自动重启 WorkBuddy" : ""}
            </span>
            {report.failed.length ? (
              <span className="block text-xs text-amber-600">
                {report.failed.length} 项未能处理：{report.failed.slice(0, 3).join("；")}
                {report.failed.length > 3 ? " …" : ""}
              </span>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : null}

      {/* 确认弹窗：逐条列出将被处理的东西 */}
      <Dialog open={confirmOpen} onOpenChange={(v) => !busy && setConfirmOpen(v)}>
        <DialogContent className="max-h-[86vh] max-w-2xl grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              {hard ? <ShieldAlert className="size-4" /> : <Trash2 className="size-4" />}
              {hard ? "彻底删除确认" : "清理到回收站"}
            </DialogTitle>
            <DialogDescription className="break-words">
              清理期间会先退出 WorkBuddy，完成后自动重新打开；数据库会整份备份到工具目录。
            </DialogDescription>
          </DialogHeader>

          <div className="min-h-0 space-y-3 overflow-y-auto pr-1 text-sm">
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
              {[
                ["项目", String(selectedItems.length)],
                ["会话记录", String(selectedSessions)],
                ["占用", humanSize(selectedBytes)],
                ["方式", hard ? "彻底删除" : "移到回收站"],
              ].map(([k, v]) => (
                <div key={k} className="rounded-lg bg-muted/50 p-2.5">
                  <div className="text-xs text-muted-foreground">{k}</div>
                  <div className="font-medium tabular-nums">{v}</div>
                </div>
              ))}
            </div>

            {hard ? (
              <Alert variant="destructive">
                <AlertTriangle className="size-4" />
                <AlertTitle>⚠️ 此操作非常危险，可能导致不可逆的数据丢失！</AlertTitle>
                <AlertDescription className="leading-5">
                  彻底删除不会保留回收站副本，被删掉的会话正文、改动详情与工作区快照将
                  <span className="font-semibold">无法恢复</span>
                  （数据库备份里只有记录行，正文文件不在备份内）。如果只是想试试，请改用「清理到回收站」。
                </AlertDescription>
              </Alert>
            ) : (
              <p className="text-xs leading-5 text-muted-foreground">
                文件与目录会被
                <span className="font-medium text-foreground">移动</span>
                到工具回收站（保持原来的相对路径），随时可以搬回；记录行随数据库备份一起可回滚。
              </p>
            )}

            <div className="max-h-64 space-y-1 overflow-auto rounded-lg border p-2">
              {selectedItems.map((it) => (
                <div key={it.id} className="rounded-md px-2 py-1.5">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="truncate font-medium">{it.title}</span>
                    <Badge variant="secondary" className="text-[10px]">
                      {it.kind === "session" ? "会话记录" : "文件"}
                    </Badge>
                    <span className="text-xs text-muted-foreground tabular-nums">
                      {humanSize(it.bytes)}
                    </span>
                  </div>
                  {it.paths.length ? (
                    <div className="mt-0.5 space-y-0.5">
                      {it.paths.map((p) => (
                        <div key={p} className="break-all font-mono text-[11px] text-muted-foreground/80">
                          {p}
                        </div>
                      ))}
                    </div>
                  ) : (
                    <div className="mt-0.5 text-[11px] text-muted-foreground/80">
                      仅删除数据库记录行
                    </div>
                  )}
                </div>
              ))}
            </div>

            {scan?.running ? (
              <Alert>
                <AlertTriangle className="size-4" />
                <AlertDescription className="leading-5">
                  WorkBuddy 正在运行，开始后它会被关闭（清完自动打开）。请先保存好正在编辑的内容。
                </AlertDescription>
              </Alert>
            ) : null}

            {progress.length ? (
              <pre className="max-h-40 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
                {progress.join("\n")}
              </pre>
            ) : null}
          </div>

          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={() => setConfirmOpen(false)}>
              取消
            </Button>
            <Button
              variant={hard ? "destructive" : "default"}
              disabled={busy || selectedItems.length === 0}
              onClick={() => void run()}
            >
              {busy ? <Loader2 className="size-4 animate-spin" /> : <Trash2 className="size-4" />}
              {busy ? "清理中…" : hard ? "确认彻底删除" : "确认清理"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {!scan && !loading && !error ? (
        <Alert>
          <Database className="size-4" />
          <AlertTitle>未检测到 WorkBuddy 数据</AlertTitle>
          <AlertDescription>
            需要本机存在 <code className="font-mono">~/.workbuddy/projects/</code> 与{" "}
            <code className="font-mono">workbuddy.db</code>（即 WorkBuddy 桌面版至少用过一次）。
          </AlertDescription>
        </Alert>
      ) : null}

      <p className="text-xs text-muted-foreground">
        <FolderOpen className="mr-1 inline size-3.5" />
        扫描永远只读；只有点确认后才会写入，并且一定先备份数据库。
      </p>
    </div>
  );
}
