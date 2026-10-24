import { useCallback, useEffect, useMemo, useState } from "react";
import {
  AlertTriangle,
  Archive,
  CheckCircle2,
  DatabaseBackup,
  Eraser,
  FileText,
  HardDrive,
  Layers,
  Loader2,
  RefreshCw,
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
import { Checkbox } from "@/components/ui/checkbox";
import { Skeleton } from "@/components/ui/skeleton";
import { qoderCleanupEmptyTrash, qoderCleanupRun, qoderCleanupScan } from "@/lib/api";

type Item = {
  key: string;
  label: string;
  path: string;
  bytes: number;
  defaultOn: boolean;
  note: string;
};
type Side = {
  region: string;
  label: string;
  dataRoot: string | null;
  bytes: number;
  items: Item[];
};
type Scan = {
  sides: Side[];
  trash: { path: string; bytes: number; items: number };
  totals: { count: number; bytes: number; recommendedCount: number; recommendedBytes: number };
  scannedAt: number;
};

const ICON: Record<string, typeof HardDrive> = {
  cache: HardDrive,
  code_cache: Layers,
  gpu_cache: Layers,
  dawn_cache: Layers,
  cached_data: Layers,
  partitions: Layers,
  crashpad: AlertTriangle,
  logs: FileText,
  backups: Archive,
  client_baks: DatabaseBackup,
};

function human(n: number): string {
  if (n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return v.toFixed(v >= 10 || i === 0 ? 0 : 1) + " " + units[i];
}

/** 选中项的唯一 id：`区域:key`（两个区域可能有同名 key，不能只用 key）。 */
const idOf = (region: string, key: string) => region + ":" + key;

export default function QoderCleanupPage() {
  const [scan, setScan] = useState<Scan | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [busyLabel, setBusyLabel] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    setBusy(true);
    setBusyLabel("扫描中");
    setError(null);
    try {
      const r = (await qoderCleanupScan()) as unknown as Scan;
      setScan(r);
      const next = new Set<string>();
      (r.sides ?? []).forEach((s) => {
        s.items.forEach((i) => {
          if (i.defaultOn) next.add(idOf(s.region, i.key));
        });
      });
      setSelected(next);
      setCollapsed(new Set((r.sides ?? []).map((s) => s.region)));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const allItems = useMemo(() => {
    const out: { id: string; region: string; regionLabel: string; item: Item }[] = [];
    (scan?.sides ?? []).forEach((s) => {
      s.items.forEach((i) =>
        out.push({ id: idOf(s.region, i.key), region: s.region, regionLabel: s.label, item: i }),
      );
    });
    return out;
  }, [scan]);

  const selectedItems = useMemo(
    () => allItems.filter((x) => selected.has(x.id)),
    [allItems, selected],
  );
  const selectedBytes = useMemo(
    () => selectedItems.reduce((a, b) => a + b.item.bytes, 0),
    [selectedItems],
  );
  const recommended = useMemo(() => allItems.filter((x) => x.item.defaultOn), [allItems]);

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleSide(side: Side) {
    setSelected((prev) => {
      const next = new Set(prev);
      const ids = side.items.map((i) => idOf(side.region, i.key));
      const allOn = ids.every((id) => next.has(id));
      ids.forEach((id) => (allOn ? next.delete(id) : next.add(id)));
      return next;
    });
  }

  async function runSelected() {
    if (selectedItems.length === 0) return;
    setBusy(true);
    setBusyLabel("清理中");
    setError(null);
    setNotice(null);
    try {
      // 按区域分组调用（后端按区域取数据目录，不跨区兜底）。
      const bySide = new Map<string, string[]>();
      selectedItems.forEach((x) => {
        const list = bySide.get(x.region) ?? [];
        list.push(x.item.key);
        bySide.set(x.region, list);
      });
      const pieces: string[] = [];
      const failures: string[] = [];
      for (const [region, keys] of bySide) {
        const r = (await qoderCleanupRun(region, keys)) as {
          removed?: { bytes: number; purged?: boolean }[];
          failed?: string[];
        };
        const moved = (r.removed ?? []).filter((x) => !x.purged);
        if (moved.length > 0) {
          pieces.push(
            "已移入回收站 " + human(moved.reduce((a, b) => a + b.bytes, 0)) + "（" + moved.length + " 项）",
          );
        }
        if (r.failed?.length) failures.push(...r.failed);
      }
      setNotice(pieces.join("；") + (failures.length ? "；" + failures.length + " 项失败：" + failures[0] : ""));
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  async function emptyTrash() {
    setBusy(true);
    setBusyLabel("清空回收站");
    setError(null);
    setNotice(null);
    try {
      const r = await qoderCleanupEmptyTrash();
      setNotice("回收站已清空，彻底释放 " + human(r.freed ?? 0));
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">Qoder 本机清理</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          回收本机上跟 Qoder 有关的空间：缓存、崩溃转储、本工具的写库备份，以及回收站。扫描只读；
          <span className="font-medium text-foreground">默认把内容移入回收站而非直接删除</span>
          ，确认无误后再清空。
        </p>
      </div>

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>操作失败</AlertTitle>
          <AlertDescription className="break-all">{error}</AlertDescription>
        </Alert>
      ) : null}
      {notice ? (
        <Alert>
          <CheckCircle2 className="size-4 text-emerald-600" />
          <AlertDescription className="break-all">{notice}</AlertDescription>
        </Alert>
      ) : null}

      {/* 可回收空间 */}
      <Card>
        <CardHeader className="pb-2">
          <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-2">
            <div className="min-w-0">
              <CardTitle className="flex items-center gap-2 text-base">
                <HardDrive className="size-4" />
                可回收空间
              </CardTitle>
              <CardDescription className="break-words">
                {scan ? (
                  <>
                    共扫描到 <span className="font-medium text-foreground">{scan.totals.count}</span> 项，合计{" "}
                    <span className="font-medium text-foreground">{human(scan.totals.bytes)}</span>
                    {scan.totals.recommendedCount > 0 ? (
                      <>，其中推荐清理 {scan.totals.recommendedCount} 项</>
                    ) : null}
                    <span className="mt-1 block font-mono text-[11px] text-muted-foreground/70">
                      国内版 {scan.sides.find((s) => s.region === "cn")?.dataRoot ?? "未安装"} · 国际版{" "}
                      {scan.sides.find((s) => s.region === "global")?.dataRoot ?? "未安装"}
                    </span>
                  </>
                ) : (
                  "正在扫描…"
                )}
              </CardDescription>
            </div>
            <Button variant="outline" size="sm" disabled={busy} onClick={() => void load()}>
              {busy && busyLabel === "扫描中" ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <RefreshCw className="size-4" />
              )}
              重新扫描
            </Button>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          {!scan ? (
            <Skeleton className="h-24 rounded-xl" />
          ) : (
            <div className="grid gap-2 sm:grid-cols-2">
              {allItems.map(({ id, regionLabel, item }) => {
                const Icon = ICON[item.key] ?? HardDrive;
                return (
                  <div key={id} className="flex items-center gap-2 rounded-lg border px-3 py-2">
                    <Icon className="size-4 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 flex-1 truncate text-sm">
                      <span className="mr-1.5 rounded bg-muted px-1 text-[10px] text-muted-foreground">
                        {regionLabel}
                      </span>
                      {item.label}
                    </span>
                    <span className="shrink-0 text-xs tabular-nums text-muted-foreground">
                      1 项 · {human(item.bytes)}
                    </span>
                  </div>
                );
              })}
            </div>
          )}

          {scan && scan.trash.bytes > 0 ? (
            <div className="flex flex-wrap items-center justify-between gap-2 rounded-lg bg-muted/50 px-3 py-2 text-xs">
              <span className="min-w-0 flex-1 break-all text-muted-foreground">
                回收站里有 {human(scan.trash.bytes)} 内容尚未真正删除（{scan.trash.items} 项）。搬回原处即恢复；
                不再需要时清空以释放空间。
              </span>
              <Button size="sm" variant="outline" disabled={busy} onClick={() => void emptyTrash()}>
                {busy && busyLabel === "清空回收站" ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <Eraser className="size-4" />
                )}
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
                已选 {selectedItems.length} / {allItems.length} 项，合计 {human(selectedBytes)}
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={busy || recommended.length === 0}
                onClick={() => setSelected(new Set(recommended.map((x) => x.id)))}
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
          {busy && !scan ? (
            <Skeleton className="h-40 rounded-xl" />
          ) : allItems.length === 0 ? (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <CheckCircle2 className="size-4 text-emerald-600" />
              没有发现可清理的内容，本机 Qoder 数据目录已经很干净。
            </div>
          ) : (
            (scan?.sides ?? [])
              .filter((s) => s.items.length > 0)
              .map((side) => {
                const isCollapsed = collapsed.has(side.region);
                const onCount = side.items.filter((i) => selected.has(idOf(side.region, i.key))).length;
                const sideBytes = side.items.reduce((a, b) => a + b.bytes, 0);
                return (
                  <div key={side.region} className="rounded-lg border">
                    <div className="flex flex-wrap items-center justify-between gap-2 border-b px-3 py-2">
                      <label className="flex min-w-0 flex-1 cursor-pointer items-center gap-2">
                        <Checkbox
                          checked={onCount === side.items.length && side.items.length > 0}
                          onCheckedChange={() => toggleSide(side)}
                        />
                        <span className="min-w-0">
                          <span className="flex flex-wrap items-center gap-1.5 text-sm font-medium">
                            {side.label}
                            <Badge variant="secondary" className="text-[10px]">
                              {side.items.length} 项
                            </Badge>
                            <span className="text-xs font-normal tabular-nums text-muted-foreground">
                              {human(sideBytes)}
                            </span>
                          </span>
                          <span className="mt-0.5 block break-all font-mono text-xs text-muted-foreground">
                            {side.dataRoot}
                          </span>
                        </span>
                      </label>
                      <div className="flex shrink-0 items-center gap-1">
                        <Button
                          size="sm"
                          variant="ghost"
                          disabled={busy}
                          onClick={() =>
                            setSelected((prev) => {
                              const next = new Set(prev);
                              side.items.forEach((i) => {
                                if (i.defaultOn) next.add(idOf(side.region, i.key));
                              });
                              return next;
                            })
                          }
                        >
                          选推荐
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() =>
                            setCollapsed((prev) => {
                              const next = new Set(prev);
                              if (next.has(side.region)) next.delete(side.region);
                              else next.add(side.region);
                              return next;
                            })
                          }
                        >
                          {isCollapsed ? "展开" : "收起"}
                        </Button>
                      </div>
                    </div>
                    {!isCollapsed ? (
                      <div className="max-h-[340px] divide-y overflow-y-auto">
                        {side.items.map((item) => {
                          const id = idOf(side.region, item.key);
                          const Icon = ICON[item.key] ?? HardDrive;
                          return (
                            <label
                              key={item.key}
                              className="flex cursor-pointer items-start gap-2 px-3 py-2 text-sm"
                            >
                              <Checkbox
                                className="mt-0.5"
                                checked={selected.has(id)}
                                disabled={busy}
                                onCheckedChange={() => toggle(id)}
                              />
                              <span className="min-w-0 flex-1">
                                <span className="flex flex-wrap items-center gap-1.5">
                                  <Icon className="size-3.5 text-muted-foreground" />
                                  <span className="font-medium">{item.label}</span>
                                  {item.defaultOn ? (
                                    <Badge className="bg-emerald-500/15 text-[10px] text-emerald-600">
                                      推荐
                                    </Badge>
                                  ) : null}
                                  <span className="text-xs tabular-nums text-muted-foreground">
                                    {human(item.bytes)}
                                  </span>
                                </span>
                                <span className="mt-0.5 block text-xs text-muted-foreground">
                                  {item.note}
                                </span>
                                <span className="mt-0.5 block truncate font-mono text-[11px] text-muted-foreground/70">
                                  {item.path}
                                </span>
                              </span>
                            </label>
                          );
                        })}
                      </div>
                    ) : null}
                  </div>
                );
              })
          )}
        </CardContent>
      </Card>

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
              已选 {selectedItems.length} 项 · {human(selectedBytes)}
            </span>
          )}
        </div>
        <Button disabled={busy || selectedItems.length === 0} onClick={() => void runSelected()}>
          <Trash2 className="size-4" />
          清理选中项
        </Button>
      </div>
    </div>
  );
}
