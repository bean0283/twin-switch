import { useCallback, useEffect, useState } from "react";
import { CheckCircle2, Loader2, RefreshCw, TriangleAlert, Wrench, XCircle } from "lucide-react";
import { toast } from "sonner";
import * as api from "@/lib/api";
import type { NetRepairResult, NetRepairStatus } from "@/lib/api";
import { useNetRepair } from "@/lib/net-repair";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

/**
 * 网络自修复窗口（补偿机制）。
 *
 * **显示条件**：积分/额度查询出现传输层失败（`isTransportError`）时由调用方打开；
 * 窗口内先跑一次诊断，**只有「检测到环境代理 && 代理端口不可达」才亮出
 * 「尝试修复」按钮** —— 代理可达或无代理配置时给出解释，不给按钮（修复无从谈起）。
 *
 * **点击后的行为**（后端 `net_repair_attempt`）：
 * 1. 重建共享 HTTP 客户端（丢弃启动时固化了失效代理的旧实例，等效重启网络层）；
 * 2. 复测上游连通；代理死了且直连可达时降级为直连客户端。
 *
 * **状态反馈**：诊断中 / 修复中（按钮禁用 + 转圈 + 文案）→ 成功（绿勾 + 自动
 * 重跑触发它的那个查询 + 自动关闭）/ 失败（红叉 + 逐步过程 + 人工处理建议）。
 */
export function NetRepairDialog() {
  const { open, errorMessage, onRepaired, close } = useNetRepair();
  const [phase, setPhase] = useState<
    "checking" | "ready" | "not-fixable" | "repairing" | "done" | "failed"
  >("checking");
  const [status, setStatus] = useState<NetRepairStatus | null>(null);
  const [result, setResult] = useState<NetRepairResult | null>(null);

  useEffect(() => {
    if (!open) return;
    setPhase("checking");
    setStatus(null);
    setResult(null);
    let cancelled = false;
    void (async () => {
      try {
        const s = await api.netRepairStatus();
        if (cancelled) return;
        setStatus(s);
        // 按钮显示条件：检测到代理 && 代理端口不可达（这正是「重启应用才恢复」的病根）。
        if (s.hasProxy && s.alive === false) {
          setPhase("ready");
        } else {
          setPhase("not-fixable");
        }
      } catch {
        if (!cancelled) setPhase("not-fixable");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open]);

  const runRepair = useCallback(async () => {
    setPhase("repairing");
    try {
      const r = await api.netRepairAttempt();
      setResult(r);
      if (r.ok) {
        setPhase("done");
        toast.success(r.message);
        onRepaired?.();
        // 成功态停留片刻让用户看到结论，然后自动收起。
        window.setTimeout(() => close(), 1800);
      } else {
        setPhase("failed");
      }
    } catch (e) {
      setPhase("failed");
      setResult({
        ok: false,
        mode: "fail",
        message: e instanceof Error ? e.message : String(e),
        steps: [],
        suggestion: "修复指令执行失败，请重启本应用后重试",
      });
    }
  }, [onRepaired, close]);

  const diag = (() => {
    if (!status) return null;
    if (!status.hasProxy) return "未检测到代理环境变量（HTTP(S)_PROXY / ALL_PROXY），这次失败与代理无关，多半是本机网络问题。";
    const aliveText =
      status.alive === true
        ? "端口可达"
        : status.alive === false
          ? `端口不可达${status.probeError ? `（${status.probeError}）` : ""}`
          : status.probeError ?? "无法探测";
    return `检测到代理 ${status.proxyUrl}（${status.envKey}）：${aliveText}。`;
  })();

  return (
    <Dialog open={open} onOpenChange={(v) => !v && phase !== "repairing" && close()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Wrench className="size-4" />
            网络连接修复
          </DialogTitle>
          <DialogDescription className="break-all">
            {errorMessage ?? "连接上游服务失败（error sending request）"}
          </DialogDescription>
        </DialogHeader>

        {phase === "checking" && (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" />
            正在检测代理状态…
          </div>
        )}

        {diag && phase !== "checking" && (
          <div className="flex items-start gap-2 rounded-md border bg-muted/40 px-3 py-2 text-sm">
            {phase === "ready" ? (
              <TriangleAlert className="mt-0.5 size-4 shrink-0 text-amber-500" />
            ) : (
              <XCircle className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
            )}
            <span>{diag}</span>
          </div>
        )}

        {phase === "ready" && (
          <div className="text-sm text-muted-foreground">
            这通常是因为本应用启动时代理工具还在线、运行中途代理失效，而网络客户端把当时的代理配置固化了下来。点击下方按钮重建网络客户端即可，无需重启应用。
          </div>
        )}

        {(phase === "repairing" || phase === "done" || phase === "failed") && result && (
          <div className="max-h-48 space-y-1 overflow-y-auto rounded-md border px-3 py-2 text-sm">
            {result.steps.map((s, i) => (
              <div key={i} className="flex items-start gap-2 text-muted-foreground">
                <span className="mt-0.5 shrink-0 text-xs">•</span>
                <span>{s}</span>
              </div>
            ))}
            {!result.steps.length && phase === "failed" && result.message && (
              <div className="text-muted-foreground">{result.message}</div>
            )}
          </div>
        )}

        {phase === "repairing" && (
          <div className="flex items-center gap-2 text-sm">
            <Loader2 className="size-4 animate-spin" />
            正在重建网络客户端并复测连通…
          </div>
        )}

        {phase === "done" && (
          <div className="flex items-center gap-2 text-sm text-emerald-600">
            <CheckCircle2 className="size-4" />
            {result?.message ?? "修复完成"}
          </div>
        )}

        {phase === "failed" && (
          <div className="space-y-2">
            <div className="flex items-center gap-2 text-sm text-red-600">
              <XCircle className="size-4" />
              {result?.message ?? "修复未成功"}
            </div>
            {result?.suggestion && (
              <div className="text-sm text-muted-foreground">{result.suggestion}</div>
            )}
          </div>
        )}

        <DialogFooter>
          {phase === "ready" && (
            <Button onClick={() => void runRepair()}>
              <RefreshCw className="size-4" />
              尝试修复
            </Button>
          )}
          {phase === "failed" && (
            <Button variant="outline" onClick={() => void runRepair()}>
              <RefreshCw className="size-4" />
              再试一次
            </Button>
          )}
          <Button
            variant={phase === "ready" ? "ghost" : "default"}
            onClick={close}
            disabled={phase === "repairing" || phase === "checking"}
          >
            {phase === "done" ? "关闭" : "知道了"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
