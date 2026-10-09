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
  MessagesSquare,
  MonitorCog,
  Package,
  RefreshCw,
  Recycle,
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
  TraeCleanupCategory,
  TraeCleanupItem,
  TraeCleanupReport,
  TraeCleanupScan,
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

/** 分类图标：四类各配一个，扫一眼就知道在哪一类。 */
const CATEGORY_ICON: Record<string, typeof Database> = {
  sessions: MessagesSquare,
  tool_residue: Package,
  client_residue: MonitorCog,
  recycle: Recycle,
};

/**
 * Trae 本机清理。
 *
 * 把「这台机器上跟 Trae 有关、且可以安全回收」的东西分成四类列出来：
 * ① Trae 库里的会话；② 本工具目录的残留（整库备份 / 解密快照 / 导入中间产物）；
 * ③ Trae 客户端的可再生缓存；④ 两处回收站。
 *
 * 三层保险：扫描只读；文件默认**移入回收站**而不是删除（可原样搬回）；
 * 删会话走与「记录」页同一条成熟链路（整库备份 → 增量加密回写 → 原子替换），
 * 并且批量删除只做一次回写、只留一份备份。
 */
export default function TraeCleanupPage() {
  const [scan, setScan] = useState<TraeCleanupScan | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set(["sessions"]));

  const [busy, setBusy] = useState(false);
  const [busyLabel, setBusyLabel] = useState("");
  const [progress, setProgress] = useState<string[]>([]);
  const [report, setReport] = useState<TraeCleanupReport | null>(null);

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [hard, setHard] = useState(false);

  useEffect(() => {
    // 与其它进度面板同样的 `listen()` 竞态：异步注册期间组件若已卸载，
    // 清理函数拿不到注销句柄，会残留监听器把每条进度投递两遍。
    let active = true;
    let un: (() => void) | undefined;
    void listen<{ line: string }>("trae-cleanup-progress", (e) => {
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
      const s = await api.traeCleanupScan(force);
      setScan(s);
      if (!keepSelection) {
        // 默认勾选「推荐清理」项：临时中间产物、可自动重建的快照、除最新一批外的旧备份
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
    const m = new Map<string, TraeCleanupItem>();
    for (const it of allItems) m.set(it.id, it);
    return m;
  }, [allItems]);

  const selectedItems = useMemo(
    () =>
      [...selected]
        .map((id) => itemById.get(id))
        .filter((x): x is TraeCleanupItem => !!x),
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
  const selectedNeedsStop = useMemo(
    () => selectedItems.filter((it) => it.needs_client_stop).length,
    [selectedItems],
  );

  const recommended = useMemo(() => {
    const ids = new Set<string>();
    for (const c of scan?.categories ?? []) {
      for (const it of c.items) if (it.recommended) ids.add(it.id);
    }
    return ids;
  }, [scan]);

  const recycleCategory = scan?.categories.find((c) => c.id === "recycle");
  const trashItem = recycleCategory?.items.find((i) => i.title === "清理回收站");

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleCategory(cat: TraeCleanupCategory) {
    const ids = cat.items.map((i) => i.id);
    if (ids.length === 0) return;
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

  async function emptyTrash() {
    if (busy) return;
    setBusy(true);
    setBusyLabel("清空回收站");
    try {
      const r = await api.traeCleanupEmptyTrash();
      // ⚠️ 与 WorkBuddy 清理页同构：后端逐项删 + 先清只读 + 短退避重试，
      // `failed_count > 0` 说明还有条目被占用删不掉 —— 既不能报「失败」（能删的已删掉），
      // 也不能报「已清空」（会让人以为干净了）。原来这里是**静默**吞掉的。
      if (r.failed_count > 0) {
        toast.warning(`已释放 ${humanSize(r.freed_bytes)}，但有 ${r.failed_count} 项删不掉`, {
          description: `${r.failed.slice(0, 3).join("；")}${
            r.failed.length > 3 ? " …" : ""
          }（多半被其他程序占用，关掉对应程序后再点一次）`,
        });
      } else {
        toast.success(`回收站已清空，释放 ${humanSize(r.freed_bytes)}`, {
          description: `移除了 ${r.removed} 个条目`,
        });
      }
      // force=true：刚清完，缓存里的数字已经不准了，这里必须重扫一遍。
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
      const r = await api.traeCleanupPurge([...selected], hard);
      setReport(r);
      setConfirmOpen(false);
      if (r.errors.length > 0) {
        toast.warning(`已处理 ${r.removed} 项，但有 ${r.errors.length} 项失败`, {
          description: r.errors.slice(0, 3).join("；"),
        });
      } else {
        toast.success(
          hard
            ? `已彻底删除，释放 ${humanSize(r.freed_bytes)}`
            : `已清理 ${r.removed} 项（可恢复），涉及 ${humanSize(r.freed_bytes)}`,
          {
            description:
              (r.session_count > 0 ? `删除 ${r.session_count} 个会话 · ` : "") +
              (hard ? "内容已直接从磁盘移除" : "内容已移入回收站，确认无误后可清空"),
          },
        );
      }
      // force=true：同上，清理后必须拿实时盘面（缓存也已被后端失效）。
      await load(true, true);
    } catch (cause) {
      toast.error("清理失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">Trae 本机清理</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          回收本机上跟 Trae 有关的空间：Trae 库里的会话、本工具的整库备份与解密快照、
          Trae 客户端的可再生缓存，以及两处回收站。扫描只读；
          <span className="font-medium text-foreground">默认把内容移入回收站而非直接删除</span>
          ，确认无误后再清空。
        </p>
      </div>

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>操作失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

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
                可回收空间
              </CardTitle>
              <CardDescription className="break-words">
                {scan ? (
                  <>
                    共扫描到 <span className="font-medium text-foreground">{allItems.length}</span> 项，
                    合计{" "}
                    <span className="font-medium text-foreground">
                      {humanSize(scan.total_bytes)}
                    </span>
                    {recommended.size > 0 ? <>，其中推荐清理 {recommended.size} 项</> : null}
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
                    <span className="mt-1 block font-mono text-[11px] text-muted-foreground/70">
                      {scan.store_root}
                    </span>
                  </>
                ) : (
                  "正在扫描…"
                )}
              </CardDescription>
            </div>
            <Button
              variant="outline"
              size="sm"
              disabled={busy || loading}
              onClick={() => void load(false, true)}
            >
              {loading ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <RefreshCw className="size-4" />
              )}
              重新扫描
            </Button>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          {(scan?.notes ?? []).map((n) => (
            <Alert key={n}>
              <AlertTriangle className="size-4" />
              <AlertDescription>{n}</AlertDescription>
            </Alert>
          ))}

          {/* 分类概览 */}
          {scan ? (
            <div className="grid gap-2 sm:grid-cols-2">
              {scan.categories.map((c) => {
                const Icon = CATEGORY_ICON[c.id] ?? Database;
                return (
                  <div
                    key={c.id}
                    className="flex items-center gap-2 rounded-lg border px-3 py-2"
                  >
                    <Icon className="size-4 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 flex-1 truncate text-sm">{c.title}</span>
                    <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
                      {c.count} 项 · {humanSize(c.bytes)}
                    </span>
                  </div>
                );
              })}
            </div>
          ) : null}

          {trashItem ? (
            <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-muted/50 px-3 py-2 text-xs">
              <span className="min-w-0 flex-1 break-all text-muted-foreground">
                回收站里有 {humanSize(trashItem.bytes)} 内容尚未真正删除。搬回原处即恢复；
                不再需要时清空以释放空间。
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
                已选 {selectedItems.length} / {allItems.length} 项，合计{" "}
                {humanSize(selectedBytes)}
                {selectedSessions > 0 ? `（含 ${selectedSessions} 个会话）` : ""}
                {selectedNeedsStop > 0 ? `（${selectedNeedsStop} 项需先关闭客户端）` : ""}
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={busy || recommended.size === 0}
                onClick={() => setSelected(new Set(recommended))}
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
              没有发现可清理的内容，本机 Trae 相关数据已经很干净。
            </div>
          ) : (
            (scan?.categories ?? [])
              .filter((c) => c.count > 0)
              .map((cat) => {
                const isCollapsed = collapsed.has(cat.id);
                const onCount = cat.items.filter((i) => selected.has(i.id)).length;
                const Icon = CATEGORY_ICON[cat.id] ?? Database;
                return (
                  <div key={cat.id} className="rounded-lg border">
                    <div className="flex flex-wrap items-center justify-between gap-2 border-b px-3 py-2">
                      <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2">
                        <Checkbox
                          checked={onCount === cat.count && cat.count > 0}
                          onCheckedChange={() => toggleCategory(cat)}
                        />
                        <span className="min-w-0">
                          <span className="flex flex-wrap items-center gap-1.5 text-sm font-medium">
                            <Icon className="size-3.5 text-muted-foreground" />
                            {cat.title}
                            <Badge variant="secondary" className="text-[10px]">
                              {cat.count} 项
                            </Badge>
                            <span className="text-xs font-normal tabular-nums text-muted-foreground">
                              {humanSize(cat.bytes)}
                            </span>
                          </span>
                          <span className="mt-0.5 block text-xs text-muted-foreground">
                            {cat.desc}
                          </span>
                        </span>
                      </label>
                      <div className="flex shrink-0 items-center gap-1">
                        {cat.recommended_count > 0 ? (
                          <Button
                            size="sm"
                            variant="ghost"
                            disabled={busy}
                            onClick={() =>
                              setSelected((prev) => {
                                const next = new Set(prev);
                                for (const it of cat.items) if (it.recommended) next.add(it.id);
                                return next;
                              })
                            }
                          >
                            选推荐
                          </Button>
                        ) : null}
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() =>
                            setCollapsed((prev) => {
                              const next = new Set(prev);
                              if (next.has(cat.id)) next.delete(cat.id);
                              else next.add(cat.id);
                              return next;
                            })
                          }
                        >
                          {isCollapsed ? "展开" : "收起"}
                        </Button>
                      </div>
                    </div>
                    {isCollapsed ? null : (
                      <div className="max-h-80 overflow-auto p-2">
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
                                {it.needs_client_stop ? (
                                  <Badge variant="secondary" className="text-[10px]">
                                    需关客户端
                                  </Badge>
                                ) : null}
                                {it.kind === "session" ? (
                                  <Badge variant="secondary" className="text-[10px]">
                                    {it.turns} 轮
                                  </Badge>
                                ) : null}
                              </span>
                              <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                                {it.kind === "session"
                                  ? `${it.client_label} · ${it.updated}`
                                  : humanSize(it.bytes)}
                                {it.owner ? ` · ${it.owner}` : ""}
                              </span>
                              {it.detail ? (
                                <span className="block text-[11px] text-muted-foreground/80">
                                  {it.detail}
                                </span>
                              ) : null}
                              {it.paths.length && it.kind !== "session" ? (
                                <span className="mt-0.5 block truncate font-mono text-[11px] text-muted-foreground/70">
                                  {it.paths.length === 1
                                    ? it.paths[0]
                                    : `${it.paths[0]} 等 ${it.paths.length} 个文件`}
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

      {/* 结果 */}
      {report ? (
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="flex items-center gap-2 text-base">
              <CheckCircle2 className="size-4 text-emerald-600" />
              上次清理结果
            </CardTitle>
            <CardDescription>
              处理 {report.removed} 项 · {humanSize(report.freed_bytes)}
              {report.session_count > 0 ? ` · 删除 ${report.session_count} 个会话` : ""}
              {report.hard ? " · 彻底删除" : " · 已移入回收站"}
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-2">
            {report.errors.length > 0 ? (
              <Alert variant="destructive">
                <ShieldAlert className="size-4" />
                <AlertTitle>{report.errors.length} 项未能处理</AlertTitle>
                <AlertDescription className="space-y-0.5">
                  {report.errors.slice(0, 8).map((e) => (
                    <span key={e} className="block break-all font-mono text-[11px]">
                      {e}
                    </span>
                  ))}
                </AlertDescription>
              </Alert>
            ) : null}
            {report.cloud && report.cloud.length > 0 ? (
              <div className="rounded-lg border p-2 text-xs">
                <div className="px-1 pb-1 text-muted-foreground">云端记录同步</div>
                {report.cloud.map((c) => (
                  <div key={c.session_id} className="flex items-center gap-2 px-1 py-0.5">
                    <span className="font-mono text-[11px]">{c.session_id}</span>
                    <span className="text-muted-foreground">
                      {!c.attempted
                        ? c.reason === "no_credential"
                          ? "账号无云端凭证，仅删本地"
                          : "归属未知，仅删本地"
                        : c.ok
                          ? "已同步删除"
                          : `云端失败：${c.error ?? ""}`}
                    </span>
                  </div>
                ))}
              </div>
            ) : null}
            {report.trash_root && !report.hard ? (
              <div className="flex items-center gap-2 text-[11px] text-muted-foreground">
                <FolderOpen className="size-3.5" />
                <span className="break-all font-mono">{report.trash_root}</span>
              </div>
            ) : null}
          </CardContent>
        </Card>
      ) : null}

      {/* 底部操作栏 */}
      <div className="sticky bottom-0 -mx-6 mt-auto flex flex-wrap items-center justify-between gap-3 border-t bg-background/95 px-6 py-3 backdrop-blur">
        <div className="min-w-0 text-sm">
          {busy ? (
            <span className="flex items-center gap-2 text-muted-foreground">
              <Loader2 className="size-4 animate-spin" />
              {busyLabel}…
            </span>
          ) : (
            <span className="text-muted-foreground">
              已选 {selectedItems.length} 项 · {humanSize(selectedBytes)}
            </span>
          )}
        </div>
        <Button
          disabled={busy || selectedItems.length === 0 || loading}
          onClick={() => setConfirmOpen(true)}
        >
          <Trash2 className="size-4" />
          清理选中项
        </Button>
      </div>

      {/* 确认弹窗 */}
      <Dialog open={confirmOpen} onOpenChange={(v) => !busy && setConfirmOpen(v)}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>确认清理</DialogTitle>
            <DialogDescription>
              共 {selectedItems.length} 项 · 约 {humanSize(selectedBytes)}。
              请逐条确认下列内容确实不再需要。
            </DialogDescription>
          </DialogHeader>

          <div className="max-h-72 space-y-2 overflow-auto rounded-lg border p-3 text-sm">
            {(["sessions", "tool_residue", "client_residue", "recycle"] as const).map((cid) => {
              const cat = scan?.categories.find((c) => c.id === cid);
              const items = (cat?.items ?? []).filter((i) => selected.has(i.id));
              if (items.length === 0) return null;
              return (
                <div key={cid}>
                  <div className="mb-1 text-xs font-medium text-muted-foreground">
                    {cat?.title}（{items.length}）
                  </div>
                  {items.map((it) => (
                    <div
                      key={it.id}
                      className="flex items-baseline justify-between gap-3 border-b py-1 last:border-0"
                    >
                      <span className="min-w-0 flex-1 truncate">{it.title}</span>
                      <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
                        {it.kind === "session" ? `${it.turns} 轮` : humanSize(it.bytes)}
                      </span>
                    </div>
                  ))}
                </div>
              );
            })}
          </div>

          <label className="flex cursor-pointer items-start gap-2.5 rounded-lg border border-destructive/30 bg-destructive/5 p-3">
            <Checkbox
              className="mt-0.5"
              checked={hard}
              disabled={busy}
              onCheckedChange={(v) => setHard(v === true)}
            />
            <span className="text-sm">
              <span className="font-medium text-destructive">彻底删除，不进回收站</span>
              <span className="mt-0.5 block text-xs text-muted-foreground">
                不勾选时内容会移入回收站，可原样搬回。勾选后直接 unlink，
                <span className="text-foreground">无法恢复</span>。
                会话无论如何都会先做一份整库备份。
              </span>
            </span>
          </label>

          {selectedNeedsStop > 0 && !hard ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertDescription>
                有 {selectedNeedsStop} 项属于客户端缓存，需要先关闭 Trae 才能清理，
                否则文件被进程占用会失败（其余项不受影响）。
              </AlertDescription>
            </Alert>
          ) : null}

          {progress.length > 0 ? (
            <div className="max-h-32 overflow-auto rounded-lg bg-muted/60 p-2 font-mono text-[11px] text-muted-foreground">
              {progress.map((line, i) => (
                <div key={`${i}-${line}`}>{line}</div>
              ))}
            </div>
          ) : null}

          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={() => setConfirmOpen(false)}>
              取消
            </Button>
            <Button variant={hard ? "destructive" : "default"} disabled={busy} onClick={() => void run()}>
              {busy ? <Loader2 className="size-4 animate-spin" /> : <Trash2 className="size-4" />}
              {busy ? busyLabel : hard ? "确认彻底删除" : "移入回收站"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
