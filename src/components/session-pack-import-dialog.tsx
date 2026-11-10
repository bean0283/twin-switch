/**
 * 会话包导入弹窗（Trae / WorkBuddy 共用）。
 *
 * ## 为什么两个产品共用一套
 *
 * 包的**元信息与解包规则**在后端就是共用的（`modules/session_pack.rs`），
 * 前端再各写一份「选文件 → 预检 → 选账号 → 导入 → 报结果」必然跑偏一次。
 * 差异只在两处：目标账号从哪来、以及要不要提示「工作空间路径不存在」。
 *
 * ## 为什么必须先预检再导入
 *
 * 导入是**写库**（结束客户端 → 备份 → 回写 → 重启），几十秒且会关掉用户正在用的客户端。
 * 预检是只读的，能在动手前把「包里有几个会话、哪些在本机没有正文、哪些路径不存在」讲清楚，
 * 让用户在**关客户端之前**就能决定要不要继续。
 */

import { useCallback, useEffect, useState } from "react";
import { FolderOpen, Loader2, PackageOpen, TriangleAlert } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import * as api from "@/lib/api";
import { toast } from "sonner";

/** 目标账号选项（两个产品都只需要 uid + 名字）。 */
export interface PackTarget {
  uid: string;
  name: string;
  /** 当前登录的那个：默认选中。 */
  isCurrent?: boolean;
}

interface Props {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  /** `trae` / `workbuddy` / `qoder`：决定调哪套预检与导入命令。 */
  kind: "trae" | "workbuddy" | "qoder";
  /** Trae 导入需要客户端 key；WorkBuddy 忽略。 */
  clientKey?: string;
  targets: PackTarget[];
  /** 导入成功后的回调（刷新列表）。 */
  onImported?: () => void;
}

export function SessionPackImportDialog({
  open,
  onOpenChange,
  kind,
  clientKey,
  targets,
  onImported,
}: Props) {
  const [zipPath, setZipPath] = useState<string | null>(null);
  const [preview, setPreview] = useState<api.SessionPackPreview | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [dstUid, setDstUid] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [lines, setLines] = useState<string[]>([]);
  const [result, setResult] = useState<string | null>(null);

  // 每次打开都从干净状态开始：上一次的预检结果留在界面上会让人以为已经选过包了。
  useEffect(() => {
    if (!open) return;
    setZipPath(null);
    setPreview(null);
    setPreviewError(null);
    setLines([]);
    setResult(null);
    setBusy(false);
    const cur = targets.find((t) => t.isCurrent);
    setDstUid(cur?.uid ?? targets[0]?.uid ?? "");
  }, [open, targets]);

  const pickFile = useCallback(async () => {
    try {
      const picked = await openDialog({
        multiple: false,
        directory: false,
        filters: [{ name: "会话包", extensions: ["zip"] }],
      });
      const p = typeof picked === "string" ? picked : null;
      if (!p) return;
      setZipPath(p);
      setPreview(null);
      setPreviewError(null);
      setResult(null);
      setPreviewBusy(true);
      try {
        const prev =
          kind === "trae"
            ? await api.traeSessionPackPreview(p)
            : kind === "qoder"
              ? await api.qoderSessionPackPreview(p)
              : await api.wbSessionPackPreview(p);
        setPreview(prev);
      } catch (cause) {
        setPreviewError(api.asError(cause));
      } finally {
        setPreviewBusy(false);
      }
    } catch (cause) {
      // 用户取消选择不算错误，静默返回；真正的失败（插件没启用等）要报出来。
      const msg = api.asError(cause);
      if (!/cancel|取消/i.test(msg)) {
        toast.error("选择文件失败", { description: msg });
      }
    }
  }, [kind]);

  async function runImport() {
    if (!zipPath || !dstUid || busy) return;
    setBusy(true);
    setLines([]);
    setResult(null);
    // 后端进度事件：Trae / WorkBuddy 共用 `trae-import-progress`，Qoder 自己一路
    // （它走的是另一套写库流程，混在一路会让两边的进度条互相串台）。
    const { listen } = await import("@tauri-apps/api/event");
    const evt = kind === "qoder" ? "qoder-import-progress" : "trae-import-progress";
    const un = await listen<{ line: string }>(evt, (e) => {
      setLines((prev) => [...prev.slice(-400), e.payload.line]);
    });
    try {
      if (kind === "trae") {
        if (!clientKey) throw new Error("缺少客户端");
        const r = await api.traeSessionPackImport(zipPath, clientKey, dstUid);
        setResult(
          `已导入 ${r.verified_sessions} 个会话` +
            (r.relaunched ? "，客户端已重启。" : "。") +
            (r.backupMode === "skipped" ? "（未备份：设置里已关闭写库前备份）" : ""),
        );
        toast.success("导入完成", { description: `已写入 ${r.verified_sessions} 个会话` });
      } else if (kind === "qoder") {
        const r = await api.qoderSessionPackImport(zipPath, dstUid);
        setResult(
          `已导入 ${r.imported} 个会话（${r.messages} 条消息）` +
            (r.skipped.length > 0 ? `，${r.skipped.length} 个被跳过` : "") +
            "。Qoder 客户端会自动重启。",
        );
        toast.success("导入完成", {
          description: `已写入 ${r.imported} 个会话 · 备份 ${r.backup.split(/[\/]/).pop() ?? ""}`,
        });
      } else {
        const r = await api.wbSessionPackImport(zipPath, dstUid);
        const n = Array.isArray(r.imported) ? r.imported.length : 0;
        const noBody = r.noBody?.length ?? 0;
        setResult(
          `已导入 ${n} 个会话` +
            (noBody > 0 ? `，${noBody} 个因缺少正文被跳过` : "") +
            "。",
        );
        toast.success("导入完成", { description: `已写入 ${n} 个会话` });
      }
      onImported?.();
    } catch (cause) {
      toast.error("导入失败", { description: api.asError(cause) });
      setLines((prev) => [...prev, `失败：${api.asError(cause)}`]);
    } finally {
      un();
      setBusy(false);
    }
  }

  const missingBody = preview?.missingBody?.length ?? 0;
  const missingWs = preview?.workspacesMissing?.length ?? 0;

  return (
    <Dialog open={open} onOpenChange={(v) => !busy && onOpenChange(v)}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            <PackageOpen className="mr-1.5 inline size-4" />
            导入会话包
          </DialogTitle>
          <DialogDescription>
            把一个会话包（.zip）里的会话写入本机，用于跨电脑迁移。
            包内是<b>明文对话内容</b>，请妥善保管。
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          {/* 选文件 */}
          <div className="flex items-center gap-2">
            <Button variant="outline" onClick={() => void pickFile()} disabled={busy || previewBusy}>
              <FolderOpen className="size-4" />
              选择会话包…
            </Button>
            <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground" title={zipPath ?? ""}>
              {zipPath ?? "还没选择文件"}
            </span>
          </div>

          {previewBusy && (
            <div className="flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" />
              正在读取包内清单…
            </div>
          )}

          {previewError && (
            <div className="rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
              {previewError}
            </div>
          )}

          {/* 预检结果 */}
          {preview && (
            <div className="space-y-2 rounded-md border bg-muted/30 px-3 py-2">
              <div className="text-sm font-medium">
                {preview.sessionCount} 个会话
                <span className="ml-2 text-xs font-normal text-muted-foreground">
                  导出于 {preview.manifest.exported_at?.slice(0, 19).replace("T", " ")} ·{" "}
                  来自 {preview.manifest.source_label || preview.manifest.source_client}
                </span>
              </div>
              <div className="max-h-32 space-y-0.5 overflow-y-auto text-xs text-muted-foreground">
                {preview.manifest.sessions.slice(0, 40).map((s) => (
                  <div key={s.sessionId} className="truncate">
                    · {s.title || "(无标题)"}
                    {s.workspace ? ` — ${s.workspace}` : ""}
                  </div>
                ))}
                {preview.manifest.sessions.length > 40 && (
                  <div>…还有 {preview.manifest.sessions.length - 40} 个</div>
                )}
              </div>
              {(missingBody > 0 || missingWs > 0) && (
                <div className="space-y-1 border-t pt-2 text-xs">
                  {missingBody > 0 && (
                    <div className="flex items-start gap-1.5 text-amber-700 dark:text-amber-300">
                      <TriangleAlert className="mt-px size-3.5 shrink-0" />
                      <span>{missingBody} 个会话没有正文，导入后会是空壳。</span>
                    </div>
                  )}
                  {missingWs > 0 && (
                    <div className="flex items-start gap-1.5 text-muted-foreground">
                      <TriangleAlert className="mt-px size-3.5 shrink-0" />
                      <span>
                        {missingWs} 个工作空间路径在本机不存在（仍按原路径写入，客户端里可能显示为未打开）。
                      </span>
                    </div>
                  )}
                </div>
              )}
            </div>
          )}

          {/* 目标账号 */}
          <div className="space-y-1.5">
            <div className="text-sm font-medium">
              {kind === "qoder" ? "导入到哪个版本" : "导入到哪个账号"}
            </div>
            {targets.length === 0 ? (
              <div className="text-xs text-destructive">
                没有可用账号。请先在该产品里登录一次，再回来导入。
              </div>
            ) : (
              <select
                className="w-full rounded-md border bg-background px-2 py-1.5 text-sm"
                value={dstUid}
                onChange={(e) => setDstUid(e.target.value)}
                disabled={busy}
              >
                {targets.map((t) => (
                  <option key={t.uid} value={t.uid}>
                    {t.name}
                    {t.isCurrent ? "（当前登录）" : ""}
                    {kind === "qoder" ? "" : ` · uid …${t.uid.slice(-6)}`}
                  </option>
                ))}
              </select>
            )}
            <p className="text-xs text-muted-foreground">
              导入会在目标账号下<b>新建一份副本</b>（新会话 id），不会覆盖本机已有会话。
            </p>
          </div>

          {/* 进度 */}
          {lines.length > 0 && (
            <div className="max-h-40 overflow-y-auto rounded-md border bg-muted/40 px-2 py-1.5 font-mono text-[11px] leading-relaxed text-muted-foreground">
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

          <p className="text-xs text-muted-foreground">
            ⚠️ 导入会写入客户端会话库：会先<b>结束客户端</b>、<b>整库备份</b>，写完自动重启。
            请先保存手上正在编辑的内容。
          </p>
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
            {result ? "关闭" : "取消"}
          </Button>
          <Button
            onClick={() => void runImport()}
            disabled={!zipPath || !preview || !dstUid || busy}
          >
            {busy && <Loader2 className="size-4 animate-spin" />}
            {busy ? "正在导入…" : "开始导入"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
