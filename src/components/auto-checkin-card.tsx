import { useCallback, useEffect, useRef, useState } from "react";
import { CalendarCheck, CheckCircle2, CircleDashed, Loader2 } from "lucide-react";
import { toast } from "sonner";

import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import * as api from "@/lib/api";
import type { AutoCheckinState } from "@/lib/trae-types";
import { cn } from "@/lib/utils";

/**
 * 自动签到卡片（首页）。
 *
 * ## 它解决什么
 *
 * 签到是**每天一次**的动作，忘了就白丢积分。这里给一个开关：开了之后
 * 启动 8 秒（或打开首页时）自动把**还没签的平台**各签一次。
 *
 * ## 三条界面纪律
 *
 * 1. **「没跑过」和「跑了但全失败」必须长得不一样**：
 *    前者显示「今日未执行」，后者显示「已执行」+ 失败数。
 *    这两种情况下用户要做的事完全不同（一个等它跑，一个去看为什么失败）。
 * 2. **按平台分别显示**：早上手动签了 Trae，不代表 WorkBuddy 签过了。
 * 3. 关闭开关**不隐藏**当天已有的记录 —— 记录是事实，不是配置。
 */

const SCOPE_NAME: Record<string, string> = {
  trae: "Trae",
  workbuddy: "WorkBuddy",
  qoder: "Qoder",
};

function fmtTime(ms: number): string {
  try {
    return new Date(ms).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "—";
  }
}

export function AutoCheckinCard({ onRan }: { onRan?: () => void }) {
  const [state, setState] = useState<AutoCheckinState | null>(null);
  const [busy, setBusy] = useState(false);
  /** 已经自动问过一次就不再重复问（严格模式下 effect 会跑两遍）。 */
  const askedRef = useRef(false);

  const load = useCallback(async () => {
    try {
      setState(await api.autoCheckinState());
    } catch {
      // 读不到就保持 null，卡片显示「读取失败」——不拿默认值假装读到了。
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /**
   * 挂载时问一次「今天还缺什么」。
   *
   * 启动时后端也有一次（8 秒后），两处靠后端闸门保证只跑一份 ——
   * 这里存在的意义是「应用一直开着跨过零点」：那时启动任务早就跑完了。
   */
  useEffect(() => {
    if (askedRef.current) return;
    askedRef.current = true;
    let alive = true;
    void (async () => {
      try {
        const r = await api.autoCheckinMaybeRun();
        if (!alive) return;
        setState(r.state);
        if (r.ran.length > 0) {
          toast.success(`自动签到完成：${r.ran.map((s) => SCOPE_NAME[s] ?? s).join(" / ")}`);
          onRan?.();
        }
      } catch {
        // 自动签到失败不该打扰用户（它本来就是后台动作）；状态卡片会显示结果。
      }
    })();
    return () => {
      alive = false;
    };
  }, [onRan]);

  const toggle = async (on: boolean) => {
    setBusy(true);
    try {
      setState(await api.autoCheckinSet(on));
      // 刚打开开关：立刻补一次（用户期待的是「现在就去签」，不是「明天开始」）。
      if (on) {
        const r = await api.autoCheckinMaybeRun();
        setState(r.state);
        if (r.ran.length > 0) {
          toast.success(`自动签到完成：${r.ran.map((s) => SCOPE_NAME[s] ?? s).join(" / ")}`);
          onRan?.();
        }
      }
    } catch (cause) {
      toast.error("保存自动签到设置失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  };

  const totalGained = (state?.scopes ?? []).reduce(
    (sum, s) => sum + (s.record?.summary?.gained ?? 0),
    0,
  );

  return (
    <Card className="gap-0 py-0">
      <CardHeader className="pb-3 pt-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div>
            <CardTitle className="flex items-center gap-2 text-base">
              <CalendarCheck className="size-4" />
              自动签到
            </CardTitle>
            <CardDescription>
              开启后每天自动签一次；已经签过的平台不会再触发
            </CardDescription>
          </div>
          {totalGained > 0 ? (
            <Badge variant="secondary" className="tabular-nums">
              今日入账 +{Math.round(totalGained)}
            </Badge>
          ) : null}
        </div>
      </CardHeader>
      <CardContent className="pb-4">
        {state === null ? (
          <p className="text-xs text-muted-foreground">读取失败（不影响手动签到，右上角按钮仍可用）</p>
        ) : (
          <div className="space-y-3">
            <label className="flex w-fit cursor-pointer items-center gap-2 text-sm">
              <Checkbox
                checked={state.enabled}
                disabled={busy}
                onCheckedChange={(v) => void toggle(v === true)}
              />
              <span>开启自动签到</span>
              {busy ? <Loader2 className="size-3.5 animate-spin text-muted-foreground" /> : null}
              {!state.enabled ? (
                <span className="text-xs text-muted-foreground">
                  （关闭时不会发任何请求）
                </span>
              ) : null}
            </label>

            {/* 今日各平台一行一项：已执行 / 未执行 + 结果明细 */}
            <div className="grid gap-2 sm:grid-cols-3">
              {state.scopes.map((s) => {
                const sum = s.record?.summary;
                const failed = sum?.failed ?? 0;
                return (
                  <div
                    key={s.scope}
                    className={cn(
                      "rounded-lg border px-3 py-2",
                      s.done ? "border-border bg-muted/30" : "border-dashed border-border",
                    )}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="text-xs font-medium">{SCOPE_NAME[s.scope] ?? s.scope}</span>
                      {s.done ? (
                        <span className="flex items-center gap-1 text-[11px] text-emerald-600">
                          <CheckCircle2 className="size-3" />
                          已执行
                        </span>
                      ) : (
                        <span className="flex items-center gap-1 text-[11px] text-muted-foreground">
                          <CircleDashed className="size-3" />
                          今日未执行
                        </span>
                      )}
                    </div>
                    <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
                      {s.done && s.record ? (
                        <>
                          {fmtTime(s.record.at)} · 成功 {sum?.success ?? 0} · 已签 {sum?.already ?? 0}
                          {failed > 0 ? (
                            <span className="text-amber-600"> · 失败 {failed}</span>
                          ) : null}
                          {(sum?.needClient ?? 0) > 0 ? (
                            <span className="text-amber-600"> · 缺客户端 {sum?.needClient}</span>
                          ) : null}
                          {(sum?.inactive ?? 0) > 0 ? (
                            <span> · 无活动 {sum?.inactive}</span>
                          ) : null}
                        </>
                      ) : (
                        "还没有跑过；开启后会自动执行"
                      )}
                    </p>
                  </div>
                );
              })}
            </div>

            {state.history.length > 1 ? (
              <p className="text-[11px] text-muted-foreground">
                最近记录：
                {state.history
                  .slice(0, 5)
                  .map((h) => `${h.date.slice(5)}(${h.scopes.length})`)
                  .join(" · ")}
              </p>
            ) : null}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
