import { useState } from "react";
import { toast } from "sonner";
import { AlertTriangle, KeyRound, Loader2, RotateCcw } from "lucide-react";

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
import { Skeleton } from "@/components/ui/skeleton";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";

/**
 * 「重置 Trae 登录态」（issue #10 的自救入口）。
 *
 * 场景：切到某个账号后能进界面、发消息却被拒 —— 客户端侧登录载体里混着上一个账号的残留
 * （`User/globalStorage` 不参与切换替换；网页凭证账号的载体又是按旧登录态合成的）。
 * 这时把客户端登录态清干净、重新登录一次，比拼配置快得多。
 *
 * ⚠️ 三条纪律：
 *   - **动手前列清单**：会动哪些文件、多大、当前登录是谁，全摆出来让用户确认；
 *   - **只清登录载体**：`storage.json` 的 `iCubeAuthInfo*` 键（可选 leveldb），
 *     机器指纹 / 版本号 / 界面偏好一律保留；
 *   - **先备份再动手**：整份搬到 `~/.twin-switch/trae/backups/login-reset-<时间戳>/`，可原样搬回。
 */

type Preview = Awaited<ReturnType<typeof api.traeResetLoginPreview>>;

function humanSize(bytes: number): string {
  if (!bytes) return "—";
  const mb = bytes / 1024 / 1024;
  return mb >= 1 ? `${mb.toFixed(1)} MB` : `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

export function TraeResetLogin({ clientKey }: { clientKey: string | null }) {
  const [open, setOpen] = useState(false);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** 连 `Local Storage/leveldb` 一起清（更彻底，会连带清掉界面偏好）。 */
  const [includeLeveldb, setIncludeLeveldb] = useState(false);
  /** 清完把客户端拉起来，让用户直接登录。 */
  const [relaunch, setRelaunch] = useState(true);

  const openDialog = async () => {
    if (!clientKey) return;
    setOpen(true);
    setLoading(true);
    setError(null);
    setPreview(null);
    try {
      const p = await api.traeResetLoginPreview(clientKey);
      if (!p.ok) throw new Error(p.error || "读取登录载体失败");
      setPreview(p);
    } catch (cause) {
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  };

  const run = async () => {
    if (!clientKey || busy) return;
    setBusy(true);
    try {
      const r = await api.traeResetLoginState(clientKey, includeLeveldb, relaunch);
      const parts = [
        r.removedKeys?.length ? `删除登录键 ${r.removedKeys.length} 个` : "没有登录键可删",
        r.removedDirs?.length ? `清除 ${r.removedDirs.length} 个 leveldb 目录` : "",
        r.clientStopped ? "已退出客户端" : "",
        r.clientRelaunched ? "已重新打开客户端" : "",
        r.relaunchError ? `客户端没能自动打开：${r.relaunchError}` : "",
      ].filter(Boolean);
      toast.success("已重置登录态", {
        description: `${parts.join(" · ")}${r.backupDir ? `；备份：${r.backupDir}` : ""}`,
      });
      setOpen(false);
    } catch (cause) {
      toast.error("重置失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <CardHeader className="pb-3">
        <CardTitle className="flex items-center gap-2 text-base">
          <RotateCcw className="size-4" />
          重置登录态
          <Badge variant="secondary" className="text-[10px]">
            自救
          </Badge>
        </CardTitle>
        <CardDescription>
          切到某个账号后能进界面、但发不了消息（或提示需要重新登录）时用：清掉客户端侧的登录载体，
          重新登录一次即可。账号档案与会话数据都不动，清完照样能把原来的账号切回来。
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-wrap items-center gap-2">
        <Button variant="outline" size="sm" onClick={() => void openDialog()} disabled={!clientKey}>
          <KeyRound className="size-4" />
          查看将清除的内容…
        </Button>
        <span className="text-[11px] text-muted-foreground">
          只清 storage.json 里的登录键；会先整份备份
        </span>
      </CardContent>

      <Dialog open={open} onOpenChange={(v) => !busy && setOpen(v)}>
        <DialogContent className="max-h-[86vh] max-w-2xl grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <RotateCcw className="size-4" />
              重置 {preview?.clientLabel ?? "Trae"} 登录态
            </DialogTitle>
            <DialogDescription className="break-words">
              动手前会把下面列出的文件整份备份到本地；清除后客户端下次启动会要求重新登录。
            </DialogDescription>
          </DialogHeader>

          <div className="min-h-0 space-y-3 overflow-y-auto pr-1 text-sm">
            {loading ? (
              <Skeleton className="h-28 w-full rounded-lg" />
            ) : error ? (
              <Alert variant="destructive">
                <AlertTriangle className="size-4" />
                <AlertTitle>读取失败</AlertTitle>
                <AlertDescription className="break-all">{error}</AlertDescription>
              </Alert>
            ) : preview ? (
              <>
                <div className="grid gap-2 sm:grid-cols-3">
                  <div className="rounded-lg bg-muted/50 p-2.5">
                    <div className="text-xs text-muted-foreground">当前登录</div>
                    <div className="truncate font-medium">
                      {preview.liveLabel || preview.liveUid || "未登录"}
                    </div>
                  </div>
                  <div className="rounded-lg bg-muted/50 p-2.5">
                    <div className="text-xs text-muted-foreground">客户端</div>
                    <div className="font-medium">{preview.running ? "运行中（会先退出）" : "未运行"}</div>
                  </div>
                  <div className="rounded-lg bg-muted/50 p-2.5">
                    <div className="text-xs text-muted-foreground">将被清除的登录键</div>
                    <div className="font-medium tabular-nums">{preview.authKeys?.length ?? 0} 个</div>
                  </div>
                </div>

                {preview.authKeys?.length ? (
                  <div className="rounded-lg border p-2">
                    <div className="mb-1 text-xs text-muted-foreground">
                      storage.json 中的键（共 {preview.authKeys.length} 个，全部清除）
                    </div>
                    <div className="flex flex-wrap gap-1">
                      {preview.authKeys.map((k) => (
                        <code
                          key={k}
                          className={cn(
                            "rounded px-1.5 py-0.5 font-mono text-[11px]",
                            // 设备绑定键单独标黄：它才是「新号沿用旧设备身份」的常见根因。
                            k.includes("icube-dc") ? "bg-amber-500/15 text-amber-700" : "bg-muted",
                          )}
                        >
                          {k}
                        </code>
                      ))}
                    </div>
                    {preview.authKeys.some((k) => k.includes("icube-dc")) ? (
                      <p className="mt-1.5 text-[11px] leading-relaxed text-amber-700">
                        其中 <code className="font-mono">iCubeAuthInfo://icube-dc:*</code> 是<b>设备绑定</b>：
                        切号时它若留在上一个账号身上，服务端会认为「这台机器已经在用别的会话」——
                        表现就是能进界面、发消息被拒。清掉后服务端可能要求重新验证本机，属正常。
                      </p>
                    ) : null}
                  </div>
                ) : (
                  <Alert>
                    <AlertTriangle className="size-4" />
                    <AlertTitle>没有发现登录键</AlertTitle>
                    <AlertDescription>
                      这个客户端当前本来就没登录（或登录态存在别处）。勾选「同时清理界面缓存」可能更有效。
                    </AlertDescription>
                  </Alert>
                )}

                <div className="max-h-52 overflow-auto rounded-lg border">
                  <table className="w-full text-xs">
                    <thead className="border-b bg-muted/40 text-muted-foreground">
                      <tr>
                        <th className="px-2 py-1.5 text-left font-medium">文件 / 目录</th>
                        <th className="w-20 px-2 py-1.5 text-right font-medium">大小</th>
                        <th className="w-24 px-2 py-1.5 text-left font-medium">动作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {(preview.entries ?? []).map((e) => (
                        <tr key={e.rel} className="border-b last:border-0">
                          <td className="px-2 py-1.5">
                            <div className="break-all font-mono text-[11px]">{e.rel}</div>
                            <div className="text-[10px] text-muted-foreground">
                              {e.kind === "dir" ? `${e.files} 个文件` : "文件"}
                            </div>
                          </td>
                          <td className="whitespace-nowrap px-2 py-1.5 text-right tabular-nums">
                            {humanSize(e.bytes)}
                          </td>
                          <td
                            className={cn(
                              "px-2 py-1.5 text-[11px]",
                              e.leveldb ? "text-amber-600" : "text-muted-foreground",
                            )}
                          >
                            {e.action}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>

                <p className="text-[11px] leading-relaxed text-muted-foreground">
                  备份位置：<code className="font-mono">{preview.backupRoot}</code>（每份独立目录，可原样搬回）
                  <br />
                  合计 {humanSize(preview.totalBytes ?? 0)}；账号档案（本工具的账号库）与会话数据**不受影响**。
                </p>

                <div className="space-y-1.5 rounded-lg border p-2.5">
                  <label className="flex cursor-pointer items-start gap-2 text-xs">
                    <input
                      type="checkbox"
                      className="mt-0.5 size-3.5"
                      checked={includeLeveldb}
                      onChange={(e) => setIncludeLeveldb(e.target.checked)}
                    />
                    <span>
                      同时清理界面缓存（各 <code className="font-mono">Local Storage/leveldb</code> 目录）
                      <span className="mt-0.5 block text-[11px] text-muted-foreground">
                        更彻底（登录态可能缓存在里面），但会一并清掉该客户端的界面偏好（布局、最近打开等）。
                        不影响会话与账号。
                      </span>
                    </span>
                  </label>
                  <label className="flex cursor-pointer items-start gap-2 text-xs">
                    <input
                      type="checkbox"
                      className="mt-0.5 size-3.5"
                      checked={relaunch}
                      onChange={(e) => setRelaunch(e.target.checked)}
                    />
                    <span>清除后自动打开客户端（方便直接重新登录）</span>
                  </label>
                </div>
              </>
            ) : null}
          </div>

          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)} disabled={busy}>
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={() => void run()}
              disabled={busy || loading || !preview?.ok}
            >
              {busy ? <Loader2 className="size-4 animate-spin" /> : <RotateCcw className="size-4" />}
              清除登录态
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Card>
  );
}
