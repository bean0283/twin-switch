import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { ChevronDown, Coins, Loader2, RefreshCw, Sparkles, Zap } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
} from "@/components/ui/card";
import * as api from "@/lib/api";
import type { WbGrowthRunReport, WbGrowthTask } from "@/lib/api";
import { cn } from "@/lib/utils";

/** 收纳状态的存储键（与「记住上次状态」一一对应）。 */
const GROWTH_OPEN_KEY = "twin.workbuddy.growthOpen";

/** 任务行的状态：界面只认这三种 + 已完成，别自己再推一套。 */
type RowState = "claimed" | "claimable" | "auto" | "manual";

function rowState(t: WbGrowthTask): RowState {
  if (t.acceptStatus === "claimed") return "claimed";
  if (t.claimable) return "claimable";
  return t.supported ? "auto" : "manual";
}

const STATE_TEXT: Record<RowState, string> = {
  claimed: "已领取",
  claimable: "可领取",
  auto: "可自动完成",
  manual: "需手动",
};

const STATE_CLASS: Record<RowState, string> = {
  claimed: "bg-muted text-muted-foreground",
  claimable: "bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
  auto: "bg-sky-500/10 text-sky-700 dark:text-sky-300",
  manual: "bg-amber-500/10 text-amber-700 dark:text-amber-300",
};

/**
 * WorkBuddy 成长任务卡片。
 *
 * ## 为什么要做
 *
 * 成长中心那批「体验类」任务不会自己完成 —— 必须由客户端把行为事件上报上去。
 * 用户在网页上挨个点一遍很费时，这里一键把能自动做的做完并领奖。
 *
 * ## 三条界面纪律
 *
 * 1. **「需手动」必须出现**：我们做不到的任务不能藏起来，否则用户以为全做完了。
 * 2. **跑完要说清到账多少**：不能只说「完成」——用户关心的是积分有没有真的加。
 * 3. **进行中要有明确状态**：一批要几十秒（事件之间有间隔 + 等结算），
 *    没有转圈和禁用，用户会以为没点到而反复点。
 */
export function WorkbuddyGrowthCard({
  accounts,
  defaultAccountId,
}: {
  accounts: Array<{ id: string; name: string }>;
  defaultAccountId?: string;
}) {
  /**
   * 展开/收纳，**记住上次状态**。
   *
   * 默认第一次是展开（用户第一次看到需要知道这里有什么），
   * 之后按上次的选择走 —— 老用户不想每次都被这一大块挤走视线。
   */
  const [open, setOpen] = useState<boolean>(() => {
    try {
      const v = window.localStorage.getItem(GROWTH_OPEN_KEY);
      return v === null ? true : v === "1";
    } catch {
      return true; // 隐私模式 / 禁用 localStorage：退化成默认展开，不报错
    }
  });

  const toggleOpen = () => {
    setOpen((prev) => {
      const next = !prev;
      try {
        window.localStorage.setItem(GROWTH_OPEN_KEY, next ? "1" : "0");
      } catch {
        // 存不下不影响本次使用，静默忽略
      }
      return next;
    });
  };

  const [accountId, setAccountId] = useState(defaultAccountId ?? accounts[0]?.id ?? "");
  const [tasks, setTasks] = useState<WbGrowthTask[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [running, setRunning] = useState(false);
  const [lastRun, setLastRun] = useState<WbGrowthRunReport | null>(null);

  const load = useCallback(async (id: string) => {
    if (!id) return;
    setLoading(true);
    setError(null);
    try {
      const r = await api.workbuddyGrowthTasks(id);
      setTasks(r.tasks ?? []);
    } catch (e) {
      // 读失败必须说清「不是没有任务」，否则和「今天全做完了」长得一样。
      setTasks(null);
      setError(api.asError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!accountId && accounts[0]) setAccountId(accounts[0].id);
  }, [accounts, accountId]);

  useEffect(() => {
    if (accountId) void load(accountId);
  }, [accountId, load]);

  const summary = useMemo(() => {
    const list = tasks ?? [];
    const claimable = list.filter((t) => rowState(t) === "claimable");
    const auto = list.filter((t) => rowState(t) === "auto");
    return {
      claimable: claimable.length,
      auto: auto.length,
      pendingPoints: claimable.reduce((n, t) => n + t.rewardCredit, 0),
    };
  }, [tasks]);

  const run = async () => {
    if (!accountId || running) return;
    setRunning(true);
    setLastRun(null);
    try {
      const r = await api.workbuddyGrowthRun(accountId);
      setLastRun(r);
      const failed = r.runs.filter((x) => !x.ok);
      if (r.totalCredit > 0 || r.totalEnergy > 0) {
        toast.success(`本次到账 ${r.totalCredit} 积分、${r.totalEnergy} 能量`, {
          description:
            r.claimed.length > 0
              ? `已领取 ${r.claimed.length} 项${failed.length > 0 ? `，${failed.length} 项没成功` : ""}`
              : undefined,
        });
      } else if (failed.length > 0) {
        // 一分没拿到且有失败：把第一条原因说出来，别只报「完成」。
        toast.error("这次没有拿到积分", { description: failed[0]?.message });
      } else {
        toast.success("没有可自动完成的任务");
      }
      await load(accountId);
    } catch (e) {
      toast.error("执行失败", { description: api.asError(e) });
    } finally {
      setRunning(false);
    }
  };

  if (accounts.length === 0) return null;

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <button
              type="button"
              onClick={toggleOpen}
              title={open ? "收起" : "展开"}
              className="inline-flex items-center gap-2 text-base leading-none font-semibold"
            >
              <ChevronDown
                className={cn("size-4 transition-transform", !open && "-rotate-90")}
              />
              <Sparkles className="size-4 text-amber-500" />
              成长任务
            </button>
            <CardDescription className="mt-1">
              {open
                ? "自动完成「体验类」任务并领取积分；做不到的会标成「需手动」。"
                : tasks && tasks.length > 0
                  ? `已收起 · 可领 ${summary.claimable} 项${
                      summary.pendingPoints > 0 ? `（约 ${summary.pendingPoints} 分）` : ""
                    } · 共 ${tasks.length} 项`
                  : "已收起"}
            </CardDescription>
          </div>
          <div className="flex shrink-0 flex-wrap items-center gap-2">
            {accounts.length > 1 && (
              <select
                className="h-8 rounded-md border border-border bg-background px-2 text-xs"
                value={accountId}
                disabled={running}
                onChange={(e) => setAccountId(e.target.value)}
              >
                {accounts.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.name}
                  </option>
                ))}
              </select>
            )}
            <Button
              variant="outline"
              size="sm"
              className="shrink-0 whitespace-nowrap"
              disabled={running || loading || !accountId}
              onClick={() => void load(accountId)}
            >
              <RefreshCw className={cn("size-4", loading && "animate-spin")} />
              刷新
            </Button>
            <Button
              size="sm"
              className="shrink-0 whitespace-nowrap"
              disabled={running || loading || !accountId}
              onClick={() => void run()}
            >
              {running ? <Loader2 className="size-4 animate-spin" /> : <Zap className="size-4" />}
              {running ? "执行中…" : "一键完成"}
            </Button>
          </div>
        </div>
      </CardHeader>

      <CardContent className={cn("space-y-3", !open && "hidden")}>
        {error ? (
          <div className="rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-destructive">
            读取失败，不是「没有任务」：{error}
          </div>
        ) : null}

        {running ? (
          <div className="flex items-center gap-2 rounded-lg border border-border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
            <Loader2 className="size-3.5 animate-spin" />
            正在逐个上报并领奖，大约需要几十秒 —— 中途不要关窗。
          </div>
        ) : null}

        {lastRun ? (
          <div className="rounded-lg border border-border px-3 py-2 text-xs">
            <div className="font-medium">
              上次执行：+{lastRun.totalCredit} 积分、+{lastRun.totalEnergy} 能量
            </div>
            {lastRun.runs.length > 0 && (
              <div className="mt-1 space-y-0.5 text-muted-foreground">
                {lastRun.runs.map((r) => (
                  <div key={r.code} className={cn(!r.ok && "text-amber-600 dark:text-amber-400")}>
                    {r.code}：{r.message}
                  </div>
                ))}
              </div>
            )}
            {lastRun.manual.length > 0 && (
              <div className="mt-1 text-muted-foreground">
                还有 {lastRun.manual.length} 项需要你手动去客户端里点：
                {lastRun.manual.slice(0, 4).map((t) => t.title).join("、")}
                {lastRun.manual.length > 4 ? " 等" : ""}
              </div>
            )}
          </div>
        ) : null}

        {loading && !tasks ? (
          <div className="py-6 text-center text-xs text-muted-foreground">读取中…</div>
        ) : tasks && tasks.length > 0 ? (
          <>
            <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground">
              <span className="inline-flex items-center gap-1">
                <Coins className="size-3.5" />
                可领 {summary.claimable} 项{summary.pendingPoints > 0 ? `（约 ${summary.pendingPoints} 分）` : ""}
              </span>
              <span>可自动完成 {summary.auto} 项</span>
              <span>共 {tasks.length} 项</span>
            </div>
            <div className="divide-y divide-border rounded-lg border border-border">
              {tasks.map((t) => {
                const st = rowState(t);
                return (
                  <div key={t.code} className="flex flex-wrap items-center gap-2 px-3 py-2">
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm">{t.title}</div>
                      <div className="truncate text-[11px] text-muted-foreground">
                        {t.target > 0 ? `${t.current}/${t.target}` : "—"}
                        {t.desc ? ` · ${t.desc}` : ""}
                      </div>
                    </div>
                    <div className="shrink-0 text-[11px] text-muted-foreground">
                      +{t.rewardCredit}分{t.rewardEnergy > 0 ? ` +${t.rewardEnergy}能量` : ""}
                    </div>
                    <span
                      className={cn(
                        "shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium",
                        STATE_CLASS[st],
                      )}
                    >
                      {STATE_TEXT[st]}
                    </span>
                  </div>
                );
              })}
            </div>
          </>
        ) : tasks && tasks.length === 0 ? (
          <div className="py-6 text-center text-xs text-muted-foreground">
            这个账号当前没有成长任务（可能今天已经全部完成，或该账号是企业版）
          </div>
        ) : null}
      </CardContent>
    </Card>
  );
}
