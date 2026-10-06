import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  ArrowRight,
  Database,
  FileJson,
  HardDriveDownload,
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
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ResizableDialogContent } from "@/components/ui/resizable-dialog-content";
import { cn } from "@/lib/utils";
import * as api from "@/lib/api";
import type {
  TraeImportCandidate,
  TraeWorkbuddyList,
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
 * WorkBuddy 会话 → Trae 导入卡片。
 *
 * WorkBuddy 的记录是**明文**（`~/.workbuddy/projects/*.jsonl` + `workbuddy.db`），与 Trae 的
 * 加密关系库格式完全不同，因此这里做的是「读取 → 转换 → 加密写入目标账号本地库」。
 * 目标账号沿用既有的导入候选（客户端 × 账号两个维度）。
 */
export function WorkbuddyImportCard({
  defaultClientKey,
}: {
  defaultClientKey: string | null;
}) {
  const [open, setOpen] = useState(false);
  const [list, setList] = useState<TraeWorkbuddyList | null>(null);
  const [loading, setLoading] = useState(false);
  const [candidates, setCandidates] = useState<TraeImportCandidate[]>([]);
  const [dstClient, setDstClient] = useState<string | null>(null);
  const [dstKey, setDstKey] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [showDeleted, setShowDeleted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<string[]>([]);
  const [report, setReport] = useState<TraeWorkbuddyReport | null>(null);

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
    try {
      const [src, cand] = await Promise.all([
        api.traeWorkbuddyList(),
        api.traeImportCandidates(""),
      ]);
      setList(src);
      setCandidates(cand.candidates);
      setDstClient((prev) => prev ?? defaultClientKey ?? cand.candidates[0]?.client_key ?? null);
    } catch (cause) {
      toast.error("读取 WorkBuddy 会话失败", { description: api.asError(cause) });
    } finally {
      setLoading(false);
    }
  }, [defaultClientKey]);

  useEffect(() => {
    void load();
  }, [load]);

  const clients = useMemo(() => {
    const map = new Map<string, string>();
    for (const c of candidates) map.set(c.client_key, c.client_label);
    return [...map.entries()].map(([key, label]) => ({ key, label }));
  }, [candidates]);

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

  const shown = useMemo(() => {
    const all = list?.sessions ?? [];
    return all.filter((s) => (showDeleted ? true : !s.deleted));
  }, [list, showDeleted]);

  const importable = useMemo(() => shown.filter((s) => s.has_body), [shown]);
  const deletedCount = useMemo(
    () => (list?.sessions ?? []).filter((s) => s.deleted).length,
    [list],
  );
  const selectedSessions = useMemo(
    () => importable.filter((s) => selected.has(s.id)),
    [importable, selected],
  );

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
      prev.size === importable.length ? new Set() : new Set(importable.map((s) => s.id)),
    );
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

  const dstCandidates = useMemo(
    () => candidates.filter((c) => c.client_key === dstClient),
    [candidates, dstClient],
  );

  async function run() {
    const target = dstCandidates.find((c) => `${c.client_key}::${c.account_id}` === dstKey);
    if (!target) {
      toast.error("请先选择目标账号");
      return;
    }
    if (selectedSessions.length === 0) {
      toast.error("请先勾选要导入的 WorkBuddy 会话");
      return;
    }
    setBusy(true);
    setProgress([]);
    setReport(null);
    try {
      const r = await api.traeWorkbuddyImport(
        target.client_key,
        target.uid,
        selectedSessions.map((s) => s.id),
      );
      setReport(r);
      toast.success(`已导入 ${r.written_sessions} 个会话`, {
        description: `${r.turns} 个回合 / ${r.tool_steps} 个工具步骤，目标：${r.target_label}`,
      });
    } catch (cause) {
      toast.error("导入失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="flex items-center gap-2 text-base">
            <HardDriveDownload className="size-4" />
            从 WorkBuddy 导入会话
            {list?.available ? (
              <Badge className="bg-emerald-500/15 text-emerald-600">
                {list.sessions.filter((s) => s.has_body && !s.deleted).length} 个可导入
              </Badge>
            ) : (
              <Badge variant="secondary">未检测到</Badge>
            )}
          </CardTitle>
          <CardDescription className="break-all font-mono text-xs">
            {list?.data_root ?? "—"}
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <p className="text-sm text-muted-foreground">
            WorkBuddy 的会话是明文 JSONL（与 Trae 的加密关系库格式不同），本功能会把它
            <span className="font-medium text-foreground">转换</span>
            成 Trae 的表结构后加密写入目标账号本地库——包含提问、最终回答，以及思考与工具调用过程。
          </p>
          <div className="flex flex-wrap items-center gap-2">
            <Button disabled={!list?.available || loading} onClick={() => setOpen(true)}>
              <FileJson className="size-4" />
              选择会话并导入
            </Button>
            <Button variant="outline" disabled={loading} onClick={() => void load()}>
              {loading ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <RefreshCw className="size-4" />
              )}
              重新扫描
            </Button>
          </div>
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
        </CardContent>
      </Card>

      <Dialog open={open} onOpenChange={(v) => !busy && setOpen(v)}>
        {/* 内容较长（目标账号 + 会话列表 + 进度 + 结果）：默认放大到屏幕的 90%×86%，
            右上右下角可拖拽调节并记住尺寸。头部/底部固定，中间会话列表吃掉剩余高度。 */}
        <ResizableDialogContent storageKey="wb-import-to-trae">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <HardDriveDownload className="size-4" />
              WorkBuddy 会话 → Trae
            </DialogTitle>
            <DialogDescription className="break-words">
              导入期间会先退出目标 Trae 客户端，写入完成后自动重启。原始 WorkBuddy 记录只读、不会改动。
            </DialogDescription>
          </DialogHeader>

          <div className="flex min-h-0 flex-col gap-4 text-sm">
            {/* 目标选择 */}
            <div className="shrink-0 space-y-2">
              <div className="text-xs text-muted-foreground">目标客户端</div>
              <div className="flex flex-wrap gap-1.5">
                {clients.map((c) => (
                  <button
                    key={c.key}
                    type="button"
                    disabled={busy}
                    onClick={() => {
                      setDstClient(c.key);
                      setDstKey(null);
                    }}
                    className={cn(
                      "rounded-lg border px-2.5 py-1.5 transition-colors",
                      c.key === dstClient
                        ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                        : "border-border hover:bg-foreground/[0.03]",
                    )}
                  >
                    {c.label}
                  </button>
                ))}
              </div>

              <div className="text-xs text-muted-foreground">目标账号</div>
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
                        <span className="block">{c.label}</span>
                        <span className="block text-xs text-muted-foreground">
                          {c.db_exists ? "有会话库" : "无会话库（需先登录一次）"}
                          {c.is_current ? " · 当前登录" : ""}
                        </span>
                      </button>
                    );
                  })}
                </div>
              )}
            </div>

            {/* 来源选择 */}
            <div className="flex min-h-0 flex-1 flex-col gap-2">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="text-xs text-muted-foreground">
                  选择要导入的 WorkBuddy 会话（已选 {selectedSessions.length} / {importable.length}）
                </div>
                <div className="flex items-center gap-3">
                  <label className="flex cursor-pointer items-center gap-1.5 text-xs text-muted-foreground">
                    <Checkbox
                      checked={showDeleted}
                      onCheckedChange={(v) => setShowDeleted(Boolean(v))}
                    />
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
                  <Button
                    size="sm"
                    variant="outline"
                    disabled={importable.length === 0 || busy}
                    onClick={toggleAll}
                  >
                    {selected.size === importable.length && importable.length > 0
                      ? "全部取消"
                      : "全选"}
                  </Button>
                </div>
              </div>

              <div className="min-h-0 flex-1 space-y-1 overflow-auto rounded-lg border p-2">
                {shown.length === 0 ? (
                  <p className="p-2 text-sm text-muted-foreground">没有可显示的会话。</p>
                ) : (
                  shown.map((s) => (
                    <SessionRow
                      key={s.id}
                      session={s}
                      owner={ownerOf(s.user_id)}
                      checked={selected.has(s.id)}
                      disabled={!s.has_body || busy}
                      onToggle={() => toggle(s.id)}
                    />
                  ))
                )}
              </div>
              <p className="shrink-0 text-xs text-muted-foreground">
                归属账号显示为「用户名/手机号（uid …尾号）」；同一段对话在不同账号下会有多份，
                带<span className="font-medium text-foreground">「最新」</span>
                标记的那份包含的内容最全（按正文逐行摘要比对，不只看时间）。
                「无正文」的会话无法导入（WorkBuddy 里只留了元数据）。
              </p>
            </div>

            {progress.length ? (
              <pre className="max-h-36 shrink-0 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
                {progress.join("\n")}
              </pre>
            ) : null}

            {report ? (
              <Alert className="shrink-0">
                <ArrowRight className="size-4" />
                <AlertTitle>导入完成</AlertTitle>
                <AlertDescription className="space-y-1">
                  <span className="block">
                    {report.written_sessions} 个会话 / {report.turns} 个回合 /{" "}
                    {report.tool_steps} 个工具步骤 → {report.target_label}
                  </span>
                  {report.changed_pages != null ? (
                    <span className="block">
                      加密回写：全库 {report.pages} 页，只重写了{" "}
                      <span className="font-medium text-foreground">{report.changed_pages}</span> 页（
                      {report.changed_mb ?? 0} MB，其中新增 {report.appended_pages ?? 0} 页），
                      其余 {report.pages - report.changed_pages} 页直接沿用原密文
                      {report.write_ms != null ? ` · ${report.write_ms} ms` : ""}
                    </span>
                  ) : null}
                  <span className="block break-all font-mono text-xs">
                    备份：{report.backup_dir}
                  </span>
                  <span className="block text-xs">
                    新库会话数：{report.verified_sessions}
                    {report.relaunched ? " · 已自动重启客户端" : " · 请手动启动客户端查看"}
                  </span>
                </AlertDescription>
              </Alert>
            ) : null}
          </div>

          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={() => setOpen(false)}>
              关闭
            </Button>
            <Button
              disabled={busy || selectedSessions.length === 0 || !dstKey}
              onClick={() => void run()}
            >
              {busy ? <Loader2 className="size-4 animate-spin" /> : <HardDriveDownload className="size-4" />}
              {busy ? "导入中…" : `导入 ${selectedSessions.length} 个会话`}
            </Button>
          </DialogFooter>
        </ResizableDialogContent>
      </Dialog>
    </>
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
