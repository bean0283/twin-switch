import { useState } from "react";
import { AlertTriangle, CalendarCheck, Loader2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import type { CheckinItem, CheckinResult, CheckinStatus } from "@/lib/trae-types";

/**
 * 签到按钮（首页「一键签到」/ 两个账号管理页各自的「签到」）。
 *
 * 调用同一个后端命令 `checkin_run`，只用 `scope`（签哪一边）与 `clientKey`
 * （Trae 侧限定客户端）区分 —— **一套实现，三处摆放**。分开写三份必然跑偏，
 * 这个仓库里已经吃过好几次这种教训。
 *
 * ⚠️ 结果里的六档状态**各上各的样式**，尤其「已跳过（token 过期）」与「失败」
 * 绝不能长得一样：前者一个请求都没发，后者是真出错了，用户要做的事也不同。
 */

/** 六档状态 → 徽标文案 + 配色。 */
const STATUS_META: Record<CheckinStatus, { label: string; className: string }> = {
  success: {
    label: "签到成功",
    className: "border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400",
  },
  already: {
    label: "今日已签",
    className: "border-sky-500/30 bg-sky-500/10 text-sky-700 dark:text-sky-400",
  },
  skipped: {
    label: "已跳过",
    className: "border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-400",
  },
  unavailable: {
    label: "不可签到",
    className: "border-border bg-muted text-muted-foreground",
  },
  inactive: {
    label: "无进行中活动",
    className: "border-border bg-muted text-muted-foreground",
  },
  // ⚠️ 必须与「无进行中活动」分开：那一档是"今天真没有"，这一档是"这台机器根本签不了"。
  // 合成一档，用户就会按"明天再来"处理，而明天照样失败。
  need_client: {
    label: "需装客户端",
    className: "border-amber-500/50 bg-amber-500/10 text-amber-700 dark:text-amber-400",
  },
  failed: {
    label: "失败",
    className: "border-destructive/40 bg-destructive/10 text-destructive",
  },
};

const CLIENT_LABEL: Record<CheckinItem["client"], string> = {
  trae: "Trae",
  workbuddy: "WorkBuddy",
  qoder: "Qoder",
};

/** 积分显示：`null` 是**未知**，不能画成 0。 */
function points(v: number | null): string {
  return v === null || v === undefined ? "未知" : String(v);
}

/**
 * 汇总一句话。**数字全部取自后端的 `summary`** —— 界面自己再数一遍就会和列表对不上
 * （同一屏两个数字必须同源）。
 */
function summarize(r: CheckinResult): string {
  const s = r.summary;
  const parts = [`成功 ${s.success}`, `已签 ${s.already}`];
  if (s.skipped) parts.push(`跳过 ${s.skipped}`);
  if (s.unavailable) parts.push(`不可签 ${s.unavailable}`);
  if (s.inactive) parts.push(`无活动 ${s.inactive}`);
  if (s.needClient) parts.push(`需装客户端 ${s.needClient}`);
  if (s.failed) parts.push(`失败 ${s.failed}`);
  return `共 ${s.total} 个账号：${parts.join(" · ")}，本次合计获得 ${s.gained} 积分`;
}

export interface CheckinButtonProps {
  /** 签哪一边。 */
  scope?: "all" | "trae" | "workbuddy" | "qoder";
  /** Trae 侧限定客户端；`null` / 不传 = 全部客户端。 */
  clientKey?: string | null;
  /** 按钮文案，默认按 `scope` 推。 */
  label?: string;
  variant?: "outline" | "default" | "secondary" | "ghost";
  size?: "sm" | "default" | "lg";
  className?: string;
  /** 有账号真的签到成功时回调（用于刷新页面上的积分数字，避免同屏新旧两个数）。 */
  onSuccess?: () => void;
}

export function CheckinButton({
  scope = "all",
  clientKey = null,
  label,
  variant = "outline",
  size = "sm",
  className,
  onSuccess,
}: CheckinButtonProps) {
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const [result, setResult] = useState<CheckinResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const text = label ?? (scope === "all" ? "一键签到" : "签到");

  async function run() {
    if (busy) return;
    setBusy(true);
    setError(null);
    setResult(null);
    try {
      const r = await api.checkinRun(scope, clientKey);
      setResult(r);
      setOpen(true);
      if (r.summary.success > 0) onSuccess?.();
    } catch (cause) {
      setError(api.asError(cause));
      setOpen(true);
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Button
        variant={variant}
        size={size}
        className={className}
        onClick={() => void run()}
        disabled={busy}
        title={
          scope === "all"
            ? "给 Trae 与 WorkBuddy 的全部账号签到。token 过期的账号会被跳过，不会发送任何请求。"
            : scope === "trae"
              ? "给 Trae 的账号签到。token 过期的账号会被跳过，不会发送任何请求。"
              : "给 WorkBuddy 的账号签到。token 过期的账号会被跳过，不会发送任何请求。"
        }
      >
        {busy ? (
          <Loader2 className="size-4 animate-spin" />
        ) : (
          <CalendarCheck className="size-4" />
        )}
        {busy ? "签到中…" : text}
      </Button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-w-[min(44rem,calc(100vw-2rem))]">
          <DialogHeader>
            <DialogTitle>签到结果</DialogTitle>
            <DialogDescription className="break-all">
              {error
                ? "这次签到没能跑完。"
                : result
                  ? summarize(result)
                  : ""}
            </DialogDescription>
          </DialogHeader>

          {/* 读取/调用失败与「一个账号都没有」必须分开说 —— 长得一样就会被当成没有账号。 */}
          {error ? (
            <div className="rounded-md border border-destructive/40 bg-destructive/10 p-3 text-sm break-all whitespace-pre-wrap">
              {error}
            </div>
          ) : result && result.items.length === 0 ? (
            <div className="rounded-md border p-3 text-sm text-muted-foreground">
              账号库里没有可签到的账号。先在账号管理页导入登录态或添加账号。
            </div>
          ) : (
            <div className="max-h-[55vh] space-y-2 overflow-y-auto pr-1">
              {/* ★ 环境级问题放在列表最上面：这是"整台机器签不了"，不是某条结果的细节。 */}
              {result && result.summary.needClient > 0 ? (
                <div className="rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-sm">
                  <div className="flex items-start gap-2">
                    <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-400" />
                    <div className="min-w-0 space-y-1">
                      <div className="font-medium">
                        本机取不到 Qoder 设备身份，{result.summary.needClient} 个 Qoder
                        账号这次签不了
                      </div>
                      <div className="text-xs text-muted-foreground">
                        这不是账号问题，也不是今天没有活动 —— 服务端只给带了设备身份的请求下发「每天 100
                        Credits」，而设备身份要由客户端现取。装一次 Qoder
                        并登录即可（装在非默认目录就设 <code className="text-[11px]">QODER_UMID_PATH</code>
                        ）；装不了客户端的机器，用「Qoder 机器身份」卡片导入账号包。下面每条结果里写了具体原因。
                      </div>
                    </div>
                  </div>
                </div>
              ) : null}
              {result?.items.map((it, i) => {
                const meta = STATUS_META[it.status] ?? STATUS_META.failed;
                return (
                  <div
                    key={`${it.client}-${it.clientKey}-${it.id}-${i}`}
                    className="flex min-w-0 items-start gap-2 rounded-md border p-2.5 text-sm"
                  >
                    <Badge variant="outline" className={cn("mt-0.5 shrink-0", meta.className)}>
                      {meta.label}
                    </Badge>
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5">
                        <span className="min-w-0 font-medium break-all">{it.name || it.id}</span>
                        <span className="text-xs text-muted-foreground">
                          {CLIENT_LABEL[it.client] ?? it.client}
                          {it.clientKey ? ` · ${it.clientKey}` : ""}
                        </span>
                        {it.status === "success" || it.status === "already" ? (
                          <span className="text-xs text-muted-foreground">
                            获得 {points(it.gained)} · 余额 {points(it.balance)}
                          </span>
                        ) : null}
                      </div>
                      <div className="mt-0.5 text-xs text-muted-foreground break-all whitespace-pre-wrap">
                        {it.message}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
