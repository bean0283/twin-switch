import { useState } from "react";
import { toast } from "sonner";
import { Brush, Loader2, ShieldAlert, Users } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";

/**
 * 「清理登录痕迹」（issue #19）。
 *
 * ## 痕迹分两种，能力不一样
 *
 * | 位置 | 能按账号挑吗 |
 * | --- | --- |
 * | 会话库里带 `user_id` 的表（`user_configuration` / `model_config_cache` / `project` / 会话） | **能**，逐账号删 |
 * | `ModularData/ai-agent/command-execution/<hash>/execution.json` 的 `ownerHash` | **不能**，是盐化哈希，只能整体清 |
 *
 * 所以界面上分成两部分：上面是**账号清单**（可单选 / 全选），下面是**命令执行记录**（一个开关管全部）。
 * 不能把后者画成「按账号勾选」——那是在骗人。
 *
 * ## 为什么「连会话一起删」要单独勾
 *
 * 默认只清「痕迹行」：配置、模型缓存、附件索引。会话正文（`chat_session` / `chat_message` /
 * `chat_turn`）是另一回事，删了客户端里就真没了（库有备份，但恢复要手工）。
 */

type Scan = api.TraeTracesScan;

export function TraeLoginTraces({
  clientKey,
  onDone,
  bare = false,
}: {
  clientKey: string | null;
  onDone?: () => void;
  /** 嵌进药丸分区时用：不画卡片外壳，只留说明 + 按钮。 */
  bare?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [scan, setScan] = useState<Scan | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [withSessions, setWithSessions] = useState(false);
  const [withCommandLog, setWithCommandLog] = useState(true);
  const [busy, setBusy] = useState(false);
  const [lines, setLines] = useState<string[]>([]);
  const [result, setResult] = useState<string | null>(null);

  const openDialog = async () => {
    if (!clientKey) return;
    setOpen(true);
    setLoading(true);
    setError(null);
    setScan(null);
    setPicked(new Set());
    setLines([]);
    setResult(null);
    try {
      const s = await api.traeTracesScan(clientKey);
      setScan(s);
      // 默认不勾当前登录的：那是正在用的号，误删体验最差。
      setPicked(new Set(s.accounts.filter((a) => !a.isCurrent).map((a) => a.uid)));
    } catch (cause) {
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  };

  const accounts = scan?.accounts ?? [];
  const pickedList = accounts.filter((a) => picked.has(a.uid));
  const pickedSessions = pickedList.reduce((n, a) => n + a.sessions, 0);
  const allPicked = accounts.length > 0 && pickedList.length === accounts.length;

  const toggle = (uid: string) => {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(uid)) next.delete(uid);
      else next.add(uid);
      return next;
    });
  };

  const run = async () => {
    if (!clientKey || busy || pickedList.length === 0) return;
    setBusy(true);
    setLines([]);
    setResult(null);
    const { listen } = await import("@tauri-apps/api/event");
    const un = await listen<{ line: string }>("trae-traces-progress", (e) => {
      setLines((prev) => [...prev.slice(-300), e.payload.line]);
    });
    try {
      const r = await api.traeTracesPurge(
        clientKey,
        pickedList.map((a) => a.uid),
        withSessions,
        withCommandLog,
      );
      const parts = [
        `${pickedList.length} 个账号`,
        withSessions ? `删除 ${r.stats.sessions ?? 0} 个会话` : "保留会话",
        r.commandTracesRemoved ? `清掉 ${r.commandTracesRemoved} 条命令执行记录` : "",
      ].filter(Boolean);
      setResult(`已清理 ${parts.join(" · ")}。客户端会自动重启。`);
      toast.success("已清理登录痕迹", { description: parts.join(" · ") });
      onDone?.();
    } catch (cause) {
      toast.error("清理失败", { description: api.asError(cause) });
      setLines((prev) => [...prev, `失败：${api.asError(cause)}`]);
    } finally {
      un();
      setBusy(false);
    }
  };

  const head = (
    <>
      <CardDescription>
        这台机器登录过的账号会留在本地会话库里（配置、模型缓存、项目、命令执行记录）。
        可以挑账号清掉，客户端里就再也看不出用过它。账号档案本身不动，切回去照样能用。
      </CardDescription>
      <div className={cn("flex flex-wrap items-center gap-2", bare && "mt-3")}>
        <Button variant="outline" size="sm" onClick={() => void openDialog()} disabled={!clientKey}>
          <Users className="size-4" />
          清点登录过的账号…
        </Button>
        <span className="text-[11px] text-muted-foreground">清点只读；清理会先整库备份</span>
      </div>
    </>
  );

  return (
    <>
      {bare ? (
        head
      ) : (
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="flex items-center gap-2 text-base">
              <Brush className="size-4" />
              登录痕迹
              <Badge variant="secondary" className="text-[10px]">
                按账号清
              </Badge>
            </CardTitle>
            {head}
          </CardHeader>
        </Card>
      )}

      <Dialog open={open} onOpenChange={(v) => !busy && setOpen(v)}>
        <DialogContent className="max-h-[86vh] max-w-2xl grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Brush className="size-4" />
              清理 {scan?.clientLabel ?? "Trae"} 的登录痕迹
            </DialogTitle>
            <DialogDescription>
              勾选要清掉的账号。默认勾选「除了当前登录之外」的全部 —— 当前在用的号留着，
              免得清完还要重新配置。
            </DialogDescription>
          </DialogHeader>

          <div className="min-h-0 space-y-3 overflow-y-auto pr-1 text-sm">
            {loading ? (
              <Skeleton className="h-32 w-full rounded-lg" />
            ) : error ? (
              <Alert variant="destructive">
                <ShieldAlert className="size-4" />
                <AlertTitle>清点失败</AlertTitle>
                <AlertDescription className="break-all">{error}</AlertDescription>
              </Alert>
            ) : scan ? (
              <>
                <div className="flex items-center justify-between text-xs">
                  <span className="text-muted-foreground">
                    本机检出 <b className="text-foreground">{scan.total}</b> 个登录过的账号
                  </span>
                  <label className="flex cursor-pointer items-center gap-1.5">
                    <input
                      type="checkbox"
                      className="size-3.5"
                      checked={allPicked}
                      onChange={(e) =>
                        setPicked(e.target.checked ? new Set(accounts.map((a) => a.uid)) : new Set())
                      }
                      disabled={busy}
                    />
                    全选
                  </label>
                </div>

                <div className="max-h-64 overflow-auto rounded-lg border">
                  <table className="w-full text-xs">
                    <thead className="sticky top-0 border-b bg-muted/60 text-muted-foreground">
                      <tr>
                        <th className="w-8 px-2 py-1.5" />
                        <th className="px-2 py-1.5 text-left font-medium">账号</th>
                        <th className="w-16 px-2 py-1.5 text-right font-medium">会话</th>
                        <th className="w-16 px-2 py-1.5 text-right font-medium">配置</th>
                        <th className="w-20 px-2 py-1.5 text-right font-medium">模型缓存</th>
                      </tr>
                    </thead>
                    <tbody>
                      {accounts.map((a) => (
                        <tr
                          key={a.uid}
                          className={cn(
                            "border-b last:border-0",
                            picked.has(a.uid) && "bg-primary/[0.04]",
                          )}
                        >
                          <td className="px-2 py-1.5">
                            <input
                              type="checkbox"
                              className="size-3.5 cursor-pointer"
                              checked={picked.has(a.uid)}
                              onChange={() => toggle(a.uid)}
                              disabled={busy}
                            />
                          </td>
                          <td className="px-2 py-1.5">
                            <div className="flex flex-wrap items-center gap-1.5">
                              <span className="font-medium">{a.label || `uid …${a.suffix}`}</span>
                              {a.isCurrent ? (
                                <Badge className="bg-emerald-600/15 text-[10px] text-emerald-700 dark:text-emerald-400">
                                  当前登录
                                </Badge>
                              ) : null}
                            </div>
                            <div className="font-mono text-[10px] text-muted-foreground">{a.uid}</div>
                          </td>
                          <td className="px-2 py-1.5 text-right tabular-nums">{a.sessions}</td>
                          <td className="px-2 py-1.5 text-right tabular-nums">{a.config}</td>
                          <td className="px-2 py-1.5 text-right tabular-nums">{a.modelCache}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>

                <div className="space-y-1.5 rounded-lg border p-2.5">
                  <label className="flex cursor-pointer items-start gap-2 text-xs">
                    <input
                      type="checkbox"
                      className="mt-0.5 size-3.5"
                      checked={withCommandLog}
                      onChange={(e) => setWithCommandLog(e.target.checked)}
                      disabled={busy}
                    />
                    <span>
                      同时清掉 AI 命令执行记录
                      {scan.commandTraces.dirs > 0 ? `（${scan.commandTraces.dirs} 条）` : ""}
                      <span className="mt-0.5 block text-[11px] text-muted-foreground">
                        那个目录里每条都带 <code className="font-mono">ownerHash</code>（账号指纹）。
                        它是盐化哈希、反推不出 uid，所以<b>只能整体清</b>，不跟上面的勾选走。
                      </span>
                    </span>
                  </label>
                  <label className="flex cursor-pointer items-start gap-2 text-xs">
                    <input
                      type="checkbox"
                      className="mt-0.5 size-3.5"
                      checked={withSessions}
                      onChange={(e) => setWithSessions(e.target.checked)}
                      disabled={busy}
                    />
                    <span>
                      连会话一起删
                      {pickedSessions > 0 ? `（${pickedSessions} 个）` : ""}
                      <span className="mt-0.5 block text-[11px] text-amber-700">
                        不勾就只清「登录痕迹」那几行（配置 / 模型缓存 / 附件索引），
                        会话正文原样保留 —— 痕迹没了，但记录还在。
                      </span>
                    </span>
                  </label>
                </div>

                {lines.length > 0 && (
                  <div className="max-h-32 overflow-y-auto rounded-md border bg-muted/40 px-2 py-1.5 font-mono text-[11px] leading-relaxed text-muted-foreground">
                    {lines.map((l, i) => (
                      <div key={i}>{l}</div>
                    ))}
                  </div>
                )}

                {result && (
                  <div className="rounded-md border border-emerald-500/30 bg-emerald-500/5 px-3 py-2 text-xs text-emerald-700 dark:text-emerald-300">
                    {result}
                  </div>
                )}

                <p className="text-[11px] leading-relaxed text-muted-foreground">
                  写库前会<b>结束客户端</b>并<b>整库备份</b>，写完自动重启。
                  本工具的账号档案不受影响 —— 清完照样能把原账号切回来。
                </p>
              </>
            ) : null}
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)} disabled={busy}>
              {result ? "关闭" : "取消"}
            </Button>
            <Button
              variant="destructive"
              onClick={() => void run()}
              disabled={busy || pickedList.length === 0}
            >
              {busy && <Loader2 className="size-4 animate-spin" />}
              {busy ? "正在清理…" : `清理选中的 ${pickedList.length} 个账号`}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
