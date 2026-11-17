import { useCallback, useEffect, useMemo, useState } from "react";
import {
  BarChart3,
  CalendarRange,
  LineChart as LineChartIcon,
  Loader2,
  PackageOpen,
  RefreshCw,
  TriangleAlert,
} from "lucide-react";
import {
  Bar,
  CartesianGrid,
  ComposedChart,
  Line,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import * as api from "@/lib/api";

/**
 * 每日积分收支趋势（Trae / WorkBuddy / Qoder 三个平台一起算）。
 *
 * 数据口径（与后端 `qoder_credit_history` 一致，**只做真实快照差值，不编数**）：
 * - 数据来自本机积分快照流水（每次成功查询积分后落一笔，约 10 分钟节流）；
 * - 相邻快照做差：额度减少记「支出」，增加（充值 / 签到奖励 / 周期重置）记「收入」；
 * - 不是官方用量，没查询过的日子就没有记录 —— 所以空态要把话说清楚。
 *
 * 配色按中文习惯：红收、绿支、净变化蓝线。
 */

const C_IN = "#ef4444"; // 收入（红）
const C_OUT = "#10b981"; // 支出（绿）
const C_NET = "#3b82f6"; // 净变化（蓝）

type HistoryData = Awaited<ReturnType<typeof api.qoderCreditHistory>>;
type DayRow = HistoryData["days"][number];

/** 平台筛选：all = 三个平台一起算。 */
const PLATFORMS: { value: api.CreditPlatform; label: string }[] = [
  { value: "all", label: "全部" },
  { value: "trae", label: "Trae" },
  { value: "workbuddy", label: "WorkBuddy" },
  { value: "qoder", label: "Qoder" },
];

const PLATFORM_LABELS: Record<string, string> = {
  trae: "Trae",
  workbuddy: "WorkBuddy",
  qoder: "Qoder",
};

/** 范围筛选：快捷档位 / 自定义日期（YYYY-MM-DD，含端点）。 */
type RangeState =
  | { mode: "days"; days: 7 | 30 | 90 }
  | { mode: "custom"; from: string; to: string };

const PRESETS: { days: 7 | 30 | 90; label: string }[] = [
  { days: 7, label: "近 7 天" },
  { days: 30, label: "近 30 天" },
  { days: 90, label: "近 90 天" },
];

const nf = new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 1 });
function fmt(v: number): string {
  return nf.format(v);
}
/** 带符号：正数显式 +。 */
function fmtSigned(v: number): string {
  return v > 0 ? `+${fmt(v)}` : fmt(v);
}

function todayStr(offsetDays = 0): string {
  const d = new Date();
  d.setDate(d.getDate() + offsetDays);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** 悬停明细：当日收入 / 支出 / 净变化，多平台时带平台拆分，全 0 时明说「无记录」。 */
function TrendTooltip({
  active,
  payload,
}: {
  active?: boolean;
  payload?: Array<{ payload: DayRow }>;
}) {
  if (!active || !payload?.length) return null;
  const row = payload[0]?.payload;
  if (!row) return null;
  const rows: { label: string; value: number; color: string; signed: boolean }[] = [
    { label: "收入", value: row.income, color: C_IN, signed: true },
    { label: "支出", value: row.expense, color: C_OUT, signed: true },
    { label: "净变化", value: row.net, color: C_NET, signed: true },
  ];
  const empty = row.income === 0 && row.expense === 0 && row.net === 0;
  const parts = Object.entries(row.parts ?? {}).filter(
    ([, p]) => p.income !== 0 || p.expense !== 0,
  );
  return (
    <div className="rounded-lg border bg-background/95 px-3 py-2 text-xs shadow-md backdrop-blur">
      <p className="font-medium">{row.date}</p>
      {empty ? (
        <p className="mt-1 text-muted-foreground">当日无收支记录</p>
      ) : (
        <div className="mt-1.5 space-y-1">
          {rows.map((r) => (
            <div key={r.label} className="flex items-center justify-between gap-4">
              <span className="inline-flex items-center gap-1.5 text-muted-foreground">
                <span className="h-2 w-2 rounded-full" style={{ background: r.color }} />
                {r.label}
              </span>
              <span className="font-mono tabular-nums" style={{ color: r.color }}>
                {r.signed ? fmtSigned(r.value) : fmt(r.value)}
              </span>
            </div>
          ))}
          {parts.length > 1 && (
            <>
              <div className="mt-1 border-t pt-1 text-[11px] text-muted-foreground">平台明细</div>
              {parts.map(([platform, p]) => (
                <div key={platform} className="flex items-center justify-between gap-4">
                  <span className="text-muted-foreground">{PLATFORM_LABELS[platform] ?? platform}</span>
                  <span className="font-mono tabular-nums">
                    <span style={{ color: C_IN }}>+{fmt(p.income)}</span>
                    {" / "}
                    <span style={{ color: C_OUT }}>-{fmt(p.expense)}</span>
                  </span>
                </div>
              ))}
            </>
          )}
        </div>
      )}
    </div>
  );
}

function LegendDot({ color, label }: { color: string; label: string }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span className="h-2 w-2 rounded-full" style={{ background: color }} />
      {label}
    </span>
  );
}

export function CreditTrendCard() {
  const [range, setRange] = useState<RangeState>({ mode: "days", days: 30 });
  const [platform, setPlatform] = useState<api.CreditPlatform>("all");
  const [chartType, setChartType] = useState<"bar" | "line">("bar");
  const [data, setData] = useState<HistoryData | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const customInvalid =
    range.mode === "custom" &&
    range.from !== "" &&
    range.to !== "" &&
    range.from > range.to;

  const load = useCallback(async (r: RangeState, pf: api.CreditPlatform) => {
    // 自定义范围没选完整 / 起止颠倒：不打请求，保留当前画面等用户补齐。
    if (r.mode === "custom" && (!r.from || !r.to || r.from > r.to)) return;
    setLoading(true);
    setError(null);
    try {
      const res =
        r.mode === "days"
          ? await api.qoderCreditHistory(r.days, null, null, pf)
          : await api.qoderCreditHistory(30, r.from, r.to, pf);
      setData(res);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(range, platform);
  }, [load, range, platform]);

  const chartData = useMemo(
    () => (data?.days ?? []).slice().sort((a, b) => a.date.localeCompare(b.date)),
    [data],
  );

  /** 空态归类：样本不足 / 范围内无记录 / 完全没有快照。 */
  const emptyKind: "no-snapshot" | "not-enough" | "range-empty" | null = useMemo(() => {
    if (!data) return null;
    if (!data.enoughData) return "no-snapshot";
    if (chartData.length === 0) return "range-empty";
    return null;
  }, [data, chartData]);

  const totals = data
    ? { income: data.totalIncome, expense: data.totalUsed, net: data.totalNet }
    : null;

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <div>
            <CardTitle className="text-base">每日积分收支趋势</CardTitle>
            <CardDescription className="mt-1">
              Trae / WorkBuddy / Qoder 三个平台账号一起算：本机快照差值，额度减少记支出、增加（充值 / 签到 / 重置）记收入，非官方用量。
            </CardDescription>
          </div>
          <div className="flex items-center gap-1.5">
            <Button
              variant="outline"
              size="icon"
              className="size-7"
              title={chartType === "bar" ? "切换为折线图" : "切换为柱状图"}
              onClick={() => setChartType(chartType === "bar" ? "line" : "bar")}
            >
              {chartType === "bar" ? (
                <LineChartIcon className="size-3.5" />
              ) : (
                <BarChart3 className="size-3.5" />
              )}
            </Button>
            <Button
              variant="outline"
              size="icon"
              className="size-7"
              title="重新读取"
              disabled={loading}
              onClick={() => void load(range, platform)}
            >
              {loading ? (
                <Loader2 className="size-3.5 animate-spin" />
              ) : (
                <RefreshCw className="size-3.5" />
              )}
            </Button>
          </div>
        </div>

        {/* 平台 + 范围筛选 */}
        <div className="mt-2 flex flex-wrap items-center gap-2">
          <span className="text-xs text-muted-foreground">平台</span>
          {PLATFORMS.map((p) => (
            <Button
              key={p.value}
              variant={platform === p.value ? "default" : "outline"}
              size="sm"
              className="h-7 px-2.5 text-xs"
              onClick={() => setPlatform(p.value)}
            >
              {p.label}
            </Button>
          ))}
          <span className="mx-1 h-4 w-px bg-border" aria-hidden />
          {PRESETS.map((p) => (
            <Button
              key={p.days}
              variant={range.mode === "days" && range.days === p.days ? "default" : "outline"}
              size="sm"
              className="h-7 px-2.5 text-xs"
              onClick={() => setRange({ mode: "days", days: p.days })}
            >
              {p.label}
            </Button>
          ))}
          <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
            <CalendarRange className="size-3.5" />
            自定义
          </span>
          <Input
            type="date"
            className="h-7 w-[130px] px-2 text-xs"
            value={range.mode === "custom" ? range.from : todayStr(-30)}
            onChange={(e) => {
              const from = e.target.value;
              setRange(
                range.mode === "custom"
                  ? { ...range, from }
                  : { mode: "custom", from, to: todayStr() },
              );
            }}
          />
          <span className="text-xs text-muted-foreground">至</span>
          <Input
            type="date"
            className="h-7 w-[130px] px-2 text-xs"
            value={range.mode === "custom" ? range.to : todayStr()}
            onChange={(e) => {
              const to = e.target.value;
              setRange(
                range.mode === "custom"
                  ? { ...range, to }
                  : { mode: "custom", from: todayStr(-30), to },
              );
            }}
          />
          {customInvalid && (
            <span className="text-xs text-destructive">开始日期不能晚于结束日期</span>
          )}
        </div>
      </CardHeader>

      <CardContent className="pt-0">
        {/* 汇总条 */}
        {totals && !error && (
          <div className="mb-2 flex flex-wrap items-center gap-x-5 gap-y-1 text-xs">
            <span className="inline-flex items-center gap-1.5">
              <span className="h-2 w-2 rounded-full" style={{ background: C_IN }} />
              区间收入
              <strong className="font-mono tabular-nums" style={{ color: C_IN }}>
                +{fmt(totals.income)}
              </strong>
            </span>
            <span className="inline-flex items-center gap-1.5">
              <span className="h-2 w-2 rounded-full" style={{ background: C_OUT }} />
              区间支出
              <strong className="font-mono tabular-nums" style={{ color: C_OUT }}>
                -{fmt(totals.expense)}
              </strong>
            </span>
            <span className="inline-flex items-center gap-1.5">
              <span className="h-2 w-2 rounded-full" style={{ background: C_NET }} />
              净变化
              <strong className="font-mono tabular-nums" style={{ color: C_NET }}>
                {fmtSigned(totals.net)}
              </strong>
            </span>
          </div>
        )}

        {error ? (
          // 刷新失败：保留旧图并明说，不静默。
          <div className="flex h-44 items-center justify-center gap-2 text-xs text-destructive">
            <TriangleAlert className="size-4" />
            读取失败：{error}
            <Button variant="outline" size="sm" className="h-6 px-2 text-xs" onClick={() => void load(range, platform)}>
              重试
            </Button>
          </div>
        ) : loading && !data ? (
          <div className="flex h-44 items-end gap-1.5 px-2" aria-hidden>
            {[38, 62, 45, 80, 55, 70, 42, 66, 50, 74, 58, 46].map((h, i) => (
              <Skeleton key={i} className="flex-1" style={{ height: `${h}%` }} />
            ))}
          </div>
        ) : emptyKind ? (
          <div className="flex h-44 flex-col items-center justify-center gap-1.5 text-center">
            <PackageOpen className="size-6 text-muted-foreground/60" />
            <p className="text-sm font-medium text-muted-foreground">
              {emptyKind === "no-snapshot"
                ? "暂无足够的快照数据"
                : emptyKind === "range-empty"
                  ? "所选日期范围内无收支记录"
                  : "暂无收支记录"}
            </p>
            <p className="max-w-md text-xs text-muted-foreground/80">
              {emptyKind === "no-snapshot"
                ? "趋势来自相邻两次积分查询的差值：同一账号至少查询过两次才能画出曲线。在 Trae / WorkBuddy / Qoder 账号页点「刷新积分」积累快照后再来看。"
                : "这个范围里没有积分变动。试着放宽日期范围或切换平台，或先去各账号页刷新积分积累快照。"}
            </p>
          </div>
        ) : (
          <>
            <div className="h-44 w-full">
              <ResponsiveContainer width="100%" height="100%">
                {chartType === "bar" ? (
                  <ComposedChart data={chartData} margin={{ top: 6, right: 4, bottom: 0, left: -14 }}>
                    <CartesianGrid stroke="hsl(var(--border))" strokeDasharray="2 4" vertical={false} />
                    <XAxis
                      dataKey="day"
                      tick={{ fontSize: 10 }}
                      stroke="hsl(var(--muted-foreground))"
                      interval="preserveStartEnd"
                      minTickGap={18}
                    />
                    <YAxis
                      tick={{ fontSize: 10 }}
                      stroke="hsl(var(--muted-foreground))"
                      width={46}
                      tickFormatter={(v: number) => fmt(v)}
                    />
                    <Tooltip content={<TrendTooltip />} cursor={{ fill: "hsl(var(--muted))", opacity: 0.4 }} />
                    <Bar dataKey="income" name="收入" fill={C_IN} radius={[2, 2, 0, 0]} maxBarSize={16} isAnimationActive={false} />
                    <Bar dataKey="expense" name="支出" fill={C_OUT} radius={[2, 2, 0, 0]} maxBarSize={16} isAnimationActive={false} />
                    <Line
                      type="monotone"
                      dataKey="net"
                      name="净变化"
                      stroke={C_NET}
                      strokeWidth={1.6}
                      dot={false}
                      isAnimationActive={false}
                    />
                  </ComposedChart>
                ) : (
                  <ComposedChart data={chartData} margin={{ top: 6, right: 4, bottom: 0, left: -14 }}>
                    <CartesianGrid stroke="hsl(var(--border))" strokeDasharray="2 4" vertical={false} />
                    <XAxis
                      dataKey="day"
                      tick={{ fontSize: 10 }}
                      stroke="hsl(var(--muted-foreground))"
                      interval="preserveStartEnd"
                      minTickGap={18}
                    />
                    <YAxis
                      tick={{ fontSize: 10 }}
                      stroke="hsl(var(--muted-foreground))"
                      width={46}
                      tickFormatter={(v: number) => fmt(v)}
                    />
                    <Tooltip content={<TrendTooltip />} cursor={{ stroke: "hsl(var(--border))" }} />
                    <Line type="monotone" dataKey="income" name="收入" stroke={C_IN} strokeWidth={1.6} dot={false} isAnimationActive={false} />
                    <Line type="monotone" dataKey="expense" name="支出" stroke={C_OUT} strokeWidth={1.6} dot={false} isAnimationActive={false} />
                    <Line type="monotone" dataKey="net" name="净变化" stroke={C_NET} strokeWidth={1.6} strokeDasharray="4 3" dot={false} isAnimationActive={false} />
                  </ComposedChart>
                )}
              </ResponsiveContainer>
            </div>
            <div className="mt-1 flex flex-wrap items-center gap-4 text-[11px] text-muted-foreground">
              <LegendDot color={C_IN} label="收入" />
              <LegendDot color={C_OUT} label="支出" />
              <LegendDot color={C_NET} label="净变化" />
              {data && (
                <span className="ml-auto">
                  {[
                    platform !== "all" ? (PLATFORM_LABELS[platform] ?? platform) : null,
                    data.rangeMode === "custom" && data.from && data.to
                      ? `${data.from} 至 ${data.to}`
                      : `近 ${data.windowDays} 天`,
                    `${data.points} 组快照差值`,
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </span>
              )}
            </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}

export default CreditTrendCard;
