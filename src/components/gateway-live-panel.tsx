import { useCallback, useEffect, useMemo, useState } from "react";
import { Activity, AlertTriangle, Info, Radio } from "lucide-react";
import {
  Area,
  AreaChart,
  CartesianGrid,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

import { gatewayLiveStats, type GatewayLiveSnapshot } from "@/lib/api";
import { cn } from "@/lib/utils";

/** 输入/输出两条线的颜色（与页面其它图表区分开：输入=蓝，输出=青）。 */
const C_IN = "#185FA5";
const C_OUT = "#0F6E56";

const WINDOWS: { secs: 60 | 300 | 3600; label: string }[] = [
  { secs: 60, label: "60 秒" },
  { secs: 300, label: "5 分钟" },
  { secs: 3600, label: "1 小时" },
];

/** 12345 → 12.3k（界面上一律不显示原始长数字）。 */
function fmt(n: number): string {
  if (!Number.isFinite(n)) return "0";
  if (Math.abs(n) >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (Math.abs(n) >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  if (Math.abs(n) >= 10) return String(Math.round(n));
  return n.toFixed(1);
}

function bytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${Math.round(n)} B`;
}

function secs(ms: number): string {
  const s = ms / 1000;
  return s >= 60 ? `${Math.floor(s / 60)}m${Math.round(s % 60)}s` : `${s.toFixed(1)}s`;
}

/**
 * 积分共享 · 实时监控。
 *
 * ## 数据从哪来
 *
 * 每秒向后台要一次**内存快照**（`gateway_live_stats`），不读磁盘、不查上游。
 * 后台在请求进 / 出两个点维护秒桶，所以这里的数字是**真账**：
 * token 来自上游 usage（拿不到就是 0，不估），`gotBytes` 是上游原始流长度。
 *
 * ## 刻意不显示的东西
 *
 * 不显示「积分 / 金额消耗速率」：Trae / Qoder 的 credits 只能查接口（缓存 15 分钟），
 * 没有逐请求的积分单价，任何"每秒烧了多少积分"都是编的。宁可空着。
 */
export function GatewayLivePanel({ running }: { running: boolean }) {
  const [win, setWin] = useState<60 | 300 | 3600>(60);
  const [snap, setSnap] = useState<GatewayLiveSnapshot | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [beat, setBeat] = useState(0);

  const tick = useCallback(async (w: 60 | 300 | 3600, alive: () => boolean) => {
    // 页面在后台就别刷：IPC 虽便宜，也没必要在用户切走时继续跑。
    if (document.hidden) return;
    try {
      const s = await gatewayLiveStats(w);
      if (alive()) {
        setSnap(s);
        setErr(null);
        setBeat((b) => b + 1);
      }
    } catch (e) {
      if (alive()) setErr(String(e));
    }
  }, []);

  useEffect(() => {
    if (!running) {
      setSnap(null);
      return;
    }
    let alive = true;
    const isAlive = () => alive;
    void tick(win, isAlive);
    const id = window.setInterval(() => void tick(win, isAlive), 1000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [running, win, tick]);

  const chart = useMemo(
    () =>
      (snap?.series ?? []).map((p, i) => ({
        // 60/300 秒窗口按「几秒前」标；1 小时按「几分钟前」。
        label: win === 3600 ? `${snap!.series.length - i}m` : `${snap!.series.length - i}s`,
        inTok: p.inTok,
        outTok: p.outTok,
      })),
    [snap, win],
  );

  const kpi = snap?.kpi;
  const activeCount = snap?.active?.length ?? 0;
  const idle = running && kpi && kpi.requests === 0 && activeCount === 0;
  /** 有请求在跑、但窗口内一条都没结算：数字必然是 0，必须说明，否则像统计坏了。 */
  const settling = running && kpi && kpi.requests === 0 && activeCount > 0;
  /** 正在跑的请求用到的 Key（窗口内还没有完成请求时，按 Key 区也得有东西看）。 */
  const activeKeys = useMemo(() => {
    const set = new Set<string>();
    for (const a of snap?.active ?? []) if (a.key) set.add(a.key);
    return [...set];
  }, [snap]);
  const maxBytes = Math.max(1, ...(snap?.active ?? []).map((a) => a.gotBytes));

  return (
    <section className="rounded-xl border border-border bg-card p-5">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <Radio
            className={cn(
              "size-4",
              running ? "text-emerald-600 dark:text-emerald-400" : "text-muted-foreground",
            )}
          />
          <span className="text-sm font-medium">实时监控</span>
          <span className="text-[11px] text-muted-foreground">
            {running ? (err ? "数据中断，正在重试" : "每秒刷新 · 内存统计，不读磁盘") : "网关未监听"}
          </span>
        </div>
        <div className="flex items-center gap-1">
          {WINDOWS.map((w) => (
            <button
              key={w.secs}
              type="button"
              onClick={() => setWin(w.secs)}
              className={cn(
                "rounded-full border px-2.5 py-1 text-[11px] transition-colors",
                win === w.secs
                  ? "border-primary/40 bg-primary/10 text-foreground"
                  : "border-border text-muted-foreground hover:bg-accent",
              )}
            >
              {w.label}
            </button>
          ))}
        </div>
      </div>

      {!running ? (
        <p className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-xs text-muted-foreground">
          网关未监听。到「积分共享」页打开总开关后，这里才会出现实时数据。
        </p>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-2.5 sm:grid-cols-4">
            <Metric label="请求速率" value={fmt(kpi?.rpm ?? 0)} unit="req/min" sub={`窗口内 ${kpi?.requests ?? 0} 次`} />
            <Metric
              label="输入速率"
              value={fmt(kpi?.inTokPerSec ?? 0)}
              unit="tok/s"
              color={C_IN}
              sub={`首字均值 ${kpi?.ttftSamples ? `${(kpi!.ttftAvgMs / 1000).toFixed(1)}s` : "—"}`}
            />
            <Metric
              label="输出速率"
              value={fmt(kpi?.outTokPerSec ?? 0)}
              unit="tok/s"
              color={C_OUT}
              sub={kpi?.ttftSamples ? `${kpi.ttftSamples} 条有首字数据` : "流式请求才有首字"}
            />
            <Metric
              label="正在跑"
              value={String(kpi?.active ?? 0)}
              unit="条"
              sub={`失败 ${kpi?.failed ?? 0} · 中断 ${kpi?.aborted ?? 0} · 排队 ${kpi?.queued ?? 0}`}
            />
          </div>

          {(snap?.alerts?.length ?? 0) > 0 && (
            <div className="mt-2.5 flex flex-wrap gap-1.5">
              {snap!.alerts.map((a) => (
                <span
                  key={a.kind}
                  title={a.hint}
                  className={cn(
                    "inline-flex items-center gap-1 rounded-md border px-2 py-1 text-[11px]",
                    a.level === "error" && "border-destructive/40 bg-destructive/10 text-destructive",
                    a.level === "warn" &&
                      "border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-400",
                    a.level === "info" && "border-border bg-muted/40 text-muted-foreground",
                  )}
                >
                  {a.level === "error" ? (
                    <AlertTriangle className="size-3" />
                  ) : a.level === "warn" ? (
                    <AlertTriangle className="size-3" />
                  ) : (
                    <Info className="size-3" />
                  )}
                  {a.text}
                </span>
              ))}
            </div>
          )}

          <div className="mt-3 flex flex-wrap items-center gap-3 text-[11px] text-muted-foreground">
            <span className="inline-flex items-center gap-1.5">
              <span className="h-0.5 w-3 rounded" style={{ background: C_IN }} />
              输入 tok/s
            </span>
            <span className="inline-flex items-center gap-1.5">
              <span className="h-0.5 w-3 rounded" style={{ background: C_OUT }} />
              输出 tok/s
            </span>
            {beat > 0 && (
              <span className="ml-auto inline-flex items-center gap-1">
                <Activity className="size-3" />
                已采样 {beat} 次
              </span>
            )}
          </div>

          <div className="mt-1 h-40 w-full">
            <ResponsiveContainer width="100%" height="100%">
              <AreaChart data={chart} margin={{ top: 6, right: 4, bottom: 0, left: -18 }}>
                <CartesianGrid stroke="hsl(var(--border))" strokeDasharray="2 4" vertical={false} />
                <XAxis dataKey="label" hide />
                <YAxis
                  yAxisId="in"
                  tick={{ fontSize: 10 }}
                  stroke="hsl(var(--muted-foreground))"
                  tickFormatter={(v: number) => fmt(v)}
                  width={44}
                />
                <YAxis
                  yAxisId="out"
                  orientation="right"
                  tick={{ fontSize: 10 }}
                  stroke="hsl(var(--muted-foreground))"
                  tickFormatter={(v: number) => fmt(v)}
                  width={40}
                />
                <Tooltip
                  contentStyle={{ fontSize: 11, borderRadius: 8 }}
                  formatter={(v: number, n: string) => [fmt(v), n === "inTok" ? "输入 tok" : "输出 tok"]}
                  labelFormatter={(l) => `${l} 前`}
                />
                <Area
                  yAxisId="in"
                  type="monotone"
                  dataKey="inTok"
                  stroke={C_IN}
                  fill={C_IN}
                  fillOpacity={0.12}
                  strokeWidth={1.6}
                  isAnimationActive={false}
                />
                <Area
                  yAxisId="out"
                  type="monotone"
                  dataKey="outTok"
                  stroke={C_OUT}
                  fill={C_OUT}
                  fillOpacity={0.12}
                  strokeWidth={1.6}
                  isAnimationActive={false}
                />
              </AreaChart>
            </ResponsiveContainer>
          </div>

          {idle && (
            <p className="mt-1 text-center text-[11px] text-muted-foreground">
              最近{win === 3600 ? " 1 小时" : win === 300 ? " 5 分钟" : " 60 秒"}没有<strong className="font-medium">完成</strong>的请求（空闲不代表异常）
              {(kpi?.aborted ?? 0) > 0
                ? `；其中 ${kpi!.aborted} 次被中断（停止 / 断连），不产生 token`
                : ""}
            </p>
          )}

          {settling && (
            <p className="mt-1 text-center text-[11px] text-muted-foreground">
              {activeCount} 条请求正在跑 —— token 要等上游给出 usage（通常在流结束时）才计入，
              所以此刻的速率还是 0，不是统计丢了
            </p>
          )}

          <div className="mt-3 grid gap-3 md:grid-cols-2">
            <div className="rounded-lg border border-border p-3">
              <div className="mb-2 flex items-center justify-between text-[11px] text-muted-foreground">
                <span>正在跑</span>
                <span>{snap?.active?.length ?? 0} 条</span>
              </div>
              {(snap?.active?.length ?? 0) === 0 ? (
                <p className="py-2 text-[11px] text-muted-foreground">没有进行中的请求</p>
              ) : (
                <div className="space-y-2">
                  {snap!.active.slice(0, 6).map((a, i) => (
                    <div key={`${a.model}-${i}`}>
                      <div className="flex items-baseline justify-between gap-2 text-[11px]">
                        <span className="min-w-0 truncate">
                          {a.model}
                          {a.key ? <span className="text-muted-foreground"> · {a.key}</span> : null}
                        </span>
                        <span className="shrink-0 font-mono text-muted-foreground">
                          {secs(a.elapsedMs)}
                        </span>
                      </div>
                      <div className="mt-1 flex items-center gap-2">
                        <div className="h-[3px] flex-1 overflow-hidden rounded bg-muted">
                          <div
                            className={cn(
                              "h-full rounded",
                              a.phase === "streaming" ? "bg-emerald-600" : "bg-sky-600/60",
                            )}
                            style={{ width: `${Math.max(6, Math.round((a.gotBytes / maxBytes) * 100))}%` }}
                          />
                        </div>
                        <span className="w-14 shrink-0 text-right font-mono text-[10px] text-muted-foreground">
                          {a.phase === "streaming" ? bytes(a.gotBytes) : "等首字"}
                        </span>
                      </div>
                    </div>
                  ))}
                </div>
              )}
              <p className="mt-2 text-[10px] text-muted-foreground">
                「已收」是上游原始流字节（含协议），不等于 token；超过 20 分钟未收尾的条目会被自动清掉
              </p>
            </div>

            <div className="rounded-lg border border-border p-3">
              <div className="mb-2 flex items-center justify-between text-[11px] text-muted-foreground">
                <span>按 Key（窗口内）</span>
                <span>输入 / 输出 tok</span>
              </div>
              {(snap?.keys?.length ?? 0) === 0 ? (
                activeKeys.length > 0 ? (
                  /* 窗口内还没有完成的请求，但确实有一条在跑：把它的 Key 列出来，
                     否则这里一片空白，看着像 Key 统计坏了。 */
                  <div className="space-y-1.5">
                    {activeKeys.slice(0, 6).map((k) => (
                      <div key={k} className="flex items-center gap-2 text-[11px]">
                        <span className="w-20 shrink-0 truncate">{k}</span>
                        <span className="flex-1 font-mono text-muted-foreground">等待结算</span>
                        <span className="inline-flex items-center rounded bg-emerald-600/10 px-1.5 text-[10px] text-emerald-700 dark:text-emerald-400">
                          跑
                        </span>
                      </div>
                    ))}
                  </div>
                ) : (
                  <p className="py-2 text-[11px] text-muted-foreground">窗口内没有产生用量的 Key</p>
                )
              ) : (
                <div className="space-y-1.5">
                  {snap!.keys.slice(0, 6).map((k) => (
                    <div key={k.keyId} className="flex items-center gap-2 text-[11px]">
                      <span className="w-20 shrink-0 truncate">{k.key}</span>
                      <span className="flex-1 font-mono text-muted-foreground">
                        {fmt(k.promptTokens)} / {fmt(k.completionTokens)}
                      </span>
                      <span className="shrink-0 text-muted-foreground">{k.requests} 次</span>
                      {k.active > 0 && (
                        <span className="inline-flex items-center gap-1 rounded bg-emerald-600/10 px-1.5 text-[10px] text-emerald-700 dark:text-emerald-400">
                          {k.active} 跑
                        </span>
                      )}
                    </div>
                  ))}
                </div>
              )}
              <p className="mt-2 text-[10px] text-muted-foreground">
                只统计拿到上游 usage 的请求；被中断 / 失败的请求没有 usage，所以会出现「有请求、无 token」
              </p>
            </div>
          </div>
        </>
      )}
    </section>
  );
}

function Metric({
  label,
  value,
  unit,
  sub,
  color,
}: {
  label: string;
  value: string;
  unit: string;
  sub?: string;
  color?: string;
}) {
  return (
    <div className="rounded-lg bg-muted/40 px-3 py-2.5">
      <div className="text-[11px] text-muted-foreground">{label}</div>
      <div className="mt-0.5 flex items-baseline gap-1">
        <span className="font-mono text-lg font-medium" style={color ? { color } : undefined}>
          {value}
        </span>
        <span className="text-[10px] text-muted-foreground">{unit}</span>
      </div>
      {sub && <div className="mt-0.5 truncate text-[10px] text-muted-foreground">{sub}</div>}
    </div>
  );
}
