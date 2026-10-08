import { useEffect, useRef, useState } from "react";
import {
  CircleAlert,
  CircleCheck,
  Download,
  Loader2,
  RefreshCw,
  RotateCcw,
  Sparkles,
} from "lucide-react";
import { toast } from "sonner";

import * as api from "@/lib/api";
import type { UpdateSnapshot } from "@/lib/types";
import { openReleaseUrl } from "@/lib/update";
import { useUpdateState } from "@/lib/use-update-state";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

/** 侧栏底部那一行可读状态；没有值得展示的内容（idle / 已最新）时返回 null。 */
function statusLine(snapshot: UpdateSnapshot): string | null {
  switch (snapshot.phase) {
    case "checking":
      return "正在检查更新…";
    case "available":
      return snapshot.latest ? `发现 v${snapshot.latest}，点此升级` : "发现新版本";
    case "downloading":
      return snapshot.percent === null ? "正在下载更新…" : `正在下载 ${snapshot.percent}%`;
    case "readyToRestart":
      return snapshot.latest ? `v${snapshot.latest} 待安装，点此重启` : "更新待安装";
    case "error":
      return snapshot.message ?? "检查更新失败";
    default:
      return null;
  }
}

/** 状态行的着色：只有「有新版 / 待重启」用主色，错误用破坏色，其余保持安静。 */
function statusTone(snapshot: UpdateSnapshot): string {
  switch (snapshot.phase) {
    case "available":
    case "readyToRestart":
      return "font-medium text-sidebar-primary hover:underline";
    case "error":
      return "text-destructive hover:underline";
    default:
      return "text-sidebar-foreground/40 hover:text-sidebar-foreground/70";
  }
}

const PHASE_TEXT: Record<UpdateSnapshot["phase"], string> = {
  idle: "尚未检查更新。",
  checking: "正在检查更新…",
  upToDate: "已是最新版本。",
  available: "发现新版本，可以下载。",
  downloading: "正在下载更新包，下载完成后可以选择重启安装。",
  readyToRestart: "更新包已下载完成并校验通过，重启后完成安装。",
  error: "更新操作失败。",
};

/**
 * 侧栏底部的更新入口：版本号 + 一个「检查更新」按钮，加一行随状态变化的状态行。
 *
 * 布局用 `grid-cols-[minmax(0,1fr)_auto]` 而不是 `flex-wrap` —— 侧栏固定 220 px，
 * 行里有一整段文字 + 一个按钮时，`flex-wrap` 的折行判定按每项的 max-content 宽算，
 * 文案一长就会把按钮顶到第二行（本项目已经栽过一次，见任务书 T22）。
 */
export function UpdateEntry({ fallbackVersion }: { fallbackVersion?: string }) {
  const snapshot = useUpdateState();
  const [open, setOpen] = useState(false);
  /** 已经弹过提示的版本号：防止后台每次检查都弹一遍同一个版本。 */
  const announced = useRef<string | null>(null);

  const busy = snapshot.phase === "checking" || snapshot.phase === "downloading";
  const line = statusLine(snapshot);
  const version = snapshot.current || fallbackVersion || "0.2.12";

  useEffect(() => {
    if (snapshot.phase !== "available" || !snapshot.latest) return;
    if (announced.current === snapshot.latest) return;
    announced.current = snapshot.latest;
    toast(`发现新版本 v${snapshot.latest}`, {
      description: "TwinSwitch 有新版本可以更新。",
      action: { label: "查看", onClick: () => setOpen(true) },
    });
  }, [snapshot.phase, snapshot.latest]);

  return (
    <>
      <div className="mt-auto select-none px-1 pt-3">
        <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-1">
          <span
            className="truncate text-xs text-sidebar-foreground/40"
            title={`TwinSwitch v${version}`}
          >
            TwinSwitch v{version}
          </span>
          <button
            type="button"
            onClick={() => setOpen(true)}
            disabled={busy}
            title="检查更新"
            aria-label="检查更新"
            className={cn(
              "inline-flex size-6 shrink-0 items-center justify-center rounded-md outline-none transition-colors",
              "text-sidebar-foreground/40 hover:bg-foreground/[0.06] hover:text-sidebar-foreground/80",
              "focus-visible:ring-2 focus-visible:ring-sidebar-ring/50",
              "disabled:pointer-events-none disabled:opacity-60",
            )}
          >
            {busy ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <RefreshCw className="size-3.5" />
            )}
          </button>
        </div>
        {line ? (
          <button
            type="button"
            onClick={() => setOpen(true)}
            title={line}
            className={cn(
              "mt-1 block w-full truncate text-left text-[11px] leading-4 outline-none",
              "focus-visible:ring-2 focus-visible:ring-sidebar-ring/50",
              statusTone(snapshot),
            )}
          >
            {line}
          </button>
        ) : null}
      </div>
      <UpdateDialog open={open} onOpenChange={setOpen} snapshot={snapshot} />
    </>
  );
}

function UpdateDialog({
  open,
  onOpenChange,
  snapshot,
}: {
  open: boolean;
  onOpenChange: (next: boolean) => void;
  snapshot: UpdateSnapshot;
}) {
  const { phase, latest, percent, message, current } = snapshot;
  const busy = phase === "checking" || phase === "downloading";

  async function run(action: () => Promise<unknown>, failure: string) {
    try {
      await action();
    } catch (error) {
      toast.error(failure, { description: String(error) });
    }
  }

  return (
    <Dialog open={open} onOpenChange={(next) => !busy && onOpenChange(next)}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>软件更新</DialogTitle>
          <DialogDescription>
            {latest && (phase === "available" || phase === "downloading" || phase === "readyToRestart")
              ? `当前 v${current || "—"}　→　v${latest}`
              : `当前版本 v${current || "—"}`}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3 py-1">
          <div className="flex items-start gap-2 text-sm">
            <PhaseIcon phase={phase} />
            <div className="min-w-0 flex-1">
              <p className="text-foreground">{PHASE_TEXT[phase]}</p>
              {message ? (
                <p className="mt-1 break-words text-xs text-muted-foreground">{message}</p>
              ) : null}
            </div>
          </div>

          {phase === "downloading" ? (
            <div className="space-y-1.5">
              <div className="h-1.5 w-full overflow-hidden rounded-full bg-foreground/10">
                <div
                  className={cn(
                    "h-full rounded-full bg-primary transition-[width] duration-200",
                    percent === null && "w-1/3 animate-pulse",
                  )}
                  style={percent === null ? undefined : { width: `${percent}%` }}
                />
              </div>
              <p className="text-right text-xs tabular-nums text-muted-foreground">
                {percent === null ? "下载中，总大小未知" : `${percent}%`}
              </p>
            </div>
          ) : null}

          <p className="text-xs text-muted-foreground">
            更新包来自 GitHub Releases，安装前会校验签名；签名不符的包会被拒绝。
          </p>
        </div>

        <DialogFooter className="gap-2 sm:justify-between">
          <Button variant="ghost" size="sm" onClick={() => void openReleaseUrl()}>
            打开发布页
          </Button>
          <div className="flex gap-2">
            {phase === "available" ? (
              <Button
                size="sm"
                onClick={() =>
                  void run(api.updateDownload, "下载更新失败")
                }
              >
                <Download />
                下载更新
              </Button>
            ) : null}
            {phase === "readyToRestart" ? (
              <Button size="sm" onClick={() => void run(api.updateRestart, "安装更新失败")}>
                <RotateCcw />
                重启并安装
              </Button>
            ) : null}
            {phase === "downloading" ? (
              <Button size="sm" disabled>
                <Loader2 className="animate-spin" />
                下载中 {percent === null ? "" : `${percent}%`}
              </Button>
            ) : null}
            {phase === "idle" || phase === "upToDate" || phase === "error" ? (
              <Button
                size="sm"
                variant={phase === "error" ? "default" : "outline"}
                onClick={() => void run(() => api.updateCheck(true), "检查更新失败")}
              >
                <RefreshCw />
                检查更新
              </Button>
            ) : null}
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function PhaseIcon({ phase }: { phase: UpdateSnapshot["phase"] }) {
  const className = "mt-0.5 size-4 shrink-0";
  switch (phase) {
    case "checking":
    case "downloading":
      return <Loader2 className={cn(className, "animate-spin text-muted-foreground")} />;
    case "available":
      return <Sparkles className={cn(className, "text-primary")} />;
    case "readyToRestart":
      return <CircleCheck className={cn(className, "text-primary")} />;
    case "error":
      return <CircleAlert className={cn(className, "text-destructive")} />;
    default:
      return <CircleCheck className={cn(className, "text-muted-foreground")} />;
  }
}
