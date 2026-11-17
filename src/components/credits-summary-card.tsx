import { useCallback, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";
import { Loader2, RefreshCw, TriangleAlert } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import * as api from "@/lib/api";
import type { TraeCreditsResult, WbCreditsResult } from "@/lib/trae-types";

/**
 * 积分概要（账号管理页）。
 *
 * 全部走**现有离线数据**，不发网络请求：
 * - 积分总额：Trae `profile.json` 缓存 + WorkBuddy 积分缓存 + Qoder 账号库缓存；
 * - 近期到期：统一「7 天内到期」口径 —— WorkBuddy 用后端的 `expiringSoonRemaining`
 *   （后端就是 7 天窗），Trae 按 `pack.expire_at` 同窗过滤；Qoder 按周期重置、无到期概念；
 * - 今日新增 / 今日消耗：积分快照流水里今天那行（与趋势图同源）；
 * - 统计时间：各平台数据时间里最新的那个。
 *
 * 空值 / 零值 / 异常：未知显「—」，有记录的零值照常显 0，单个平台读取失败不拖垮整卡。
 */

/** 近期到期的窗口（与 WorkBuddy 后端 expiringSoon 一致）。 */
const EXPIRING_WINDOW_MS = 7 * 86_400_000;

const nf = new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 1 });
function fmt(v: number): string {
  return nf.format(v);
}

/** 占比：平台余额 ÷ 总余额；总数未知或为 0 时不给百分比。 */
function fmtPct(part: number, total: number): string {
  if (!(total > 0)) return "—";
  return `${nf.format((part / total) * 100)}%`;
}

function todayStr(): string {
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

function fmtTime(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

type PlatformStat = {
  /** 已知则给值；unknown = 该平台缓存里没有任何可用数字。 */
  known: boolean;
  total: number;
  /** 该平台最近一次数据更新时间；null = 没有可靠的时间信息。 */
  updatedAt: number | null;
};

type TodayPlatform = { income: number | null; expense: number | null };

type Summary = {
  total: { known: boolean; value: number; perPlatform: Record<string, PlatformStat> };
  /** 近期到期（7 天内）；soonAt = 最近一个到期时间，null = 无到期记录。 */
  expiring: { known: boolean; value: number; soonAt: number | null };
  /** 今日快照差值；null = 今天没有快照记录。perPlatform 同口径按平台拆分。 */
  today: { income: number | null; expense: number | null; perPlatform: Record<string, TodayPlatform> };
  /** 各平台数据时间取最新；null = 全都没有。 */
  sampledAt: number | null;
};

/** 单平台读取失败不影响其它平台：失败的平台按 unknown 记。 */
function gather(): Promise<{
  trae: TraeCreditsResult | null;
  wb: WbCreditsResult | null;
  qoder: Awaited<ReturnType<typeof api.qoderHomeSummary>> | null;
  history: Awaited<ReturnType<typeof api.qoderCreditHistory>> | null;
}> {
  const safe = <T,>(p: Promise<T>): Promise<T | null> =>
    p.catch(() => null);
  return Promise.all([
    safe(api.traeCreditsCached(null)),
    safe(api.workbuddyCreditsCached()),
    safe(api.qoderHomeSummary()),
    safe(api.qoderCreditHistory(1)),
  ]).then(([trae, wb, qoder, history]) => ({ trae, wb, qoder, history }));
}

function summarize(data: Awaited<ReturnType<typeof gather>>): Summary {
  const now = Date.now();
  const perPlatform: Record<string, PlatformStat> = {};
  let totalValue = 0;
  let totalKnown = false;
  let expiringValue = 0;
  let expiringKnown = false;
  let soonAt: number | null = null;
  let sampledAt: number | null = null;
  const bumpAt = (ms: number | null | undefined) => {
    if (typeof ms === "number" && ms > 0 && (sampledAt === null || ms > sampledAt)) {
      sampledAt = ms;
    }
  };
  /** 全局 + 平台两级同时记时间。 */
  const bumpPlatformAt = (key: string, ms: number | null | undefined) => {
    bumpAt(ms);
    if (typeof ms === "number" && ms > 0) {
      const prev = perPlatform[key];
      if (prev && (prev.updatedAt === null || ms > prev.updatedAt)) {
        perPlatform[key] = { ...prev, updatedAt: ms };
      }
    }
  };

  // Trae：账号缓存里的 remaining；到期按积分包 expire_at 过 7 天窗。
  {
    const accounts = data.trae?.accounts ?? [];
    let sum = 0;
    let known = false;
    let updatedAt: number | null = null;
    for (const a of accounts) {
      if (typeof a.updatedAt === "number" && a.updatedAt > 0 && (updatedAt === null || a.updatedAt > updatedAt)) {
        updatedAt = a.updatedAt;
      }
      if (typeof a.remaining === "number" && Number.isFinite(a.remaining)) {
        sum += a.remaining;
        known = true;
      }
      for (const p of a.packs ?? []) {
        if (
          typeof p.expire_at === "number" &&
          p.expire_at > now &&
          p.expire_at <= now + EXPIRING_WINDOW_MS &&
          p.remaining > 0
        ) {
          expiringValue += p.remaining;
          expiringKnown = true;
          if (soonAt === null || p.expire_at < soonAt) soonAt = p.expire_at;
        }
      }
    }
    perPlatform.trae = { known, total: sum, updatedAt };
    bumpPlatformAt("trae", updatedAt);
    if (known) {
      totalValue += sum;
      totalKnown = true;
    }
  }

  // WorkBuddy：缓存条目自带 totalRemaining / expiringSoonRemaining（后端 7 天窗）。
  {
    const accounts = data.wb?.accounts ?? [];
    let sum = 0;
    let known = false;
    let updatedAt: number | null = null;
    for (const a of accounts) {
      if (typeof a.updatedAt === "number" && a.updatedAt > 0 && (updatedAt === null || a.updatedAt > updatedAt)) {
        updatedAt = a.updatedAt;
      }
      if (typeof a.totalRemaining === "number" && Number.isFinite(a.totalRemaining)) {
        sum += a.totalRemaining;
        known = true;
      }
      if (typeof a.expiringSoonRemaining === "number" && a.expiringSoonRemaining > 0) {
        expiringValue += a.expiringSoonRemaining;
        expiringKnown = true;
        const at = a.soonestExpireAt;
        if (typeof at === "number" && (soonAt === null || at < soonAt)) soonAt = at;
      }
    }
    perPlatform.workbuddy = { known, total: sum, updatedAt };
    bumpPlatformAt("workbuddy", updatedAt);
    if (known) {
      totalValue += sum;
      totalKnown = true;
    }
  }

  // Qoder：首页概况里的缓存总额（周期重置，无到期概念）；时间取后端汇总的最近积分更新。
  {
    const credits = data.qoder?.credits;
    const known = typeof credits === "number" && Number.isFinite(credits);
    const updatedAt =
      typeof data.qoder?.creditsUpdatedAt === "number" && data.qoder.creditsUpdatedAt > 0
        ? data.qoder.creditsUpdatedAt
        : null;
    perPlatform.qoder = { known, total: known ? (credits as number) : 0, updatedAt };
    bumpPlatformAt("qoder", updatedAt);
    if (known) {
      totalValue += credits as number;
      totalKnown = true;
    }
  }

  // 今日：快照流水里今天那行；没有行 = 今日无记录（不是 0）。
  // 平台拆分同口径：有行则各平台都有数（parts 里没出现的平台 = 0），无行则全为 null。
  let income: number | null = null;
  let expense: number | null = null;
  const todayPerPlatform: Record<string, TodayPlatform> = {};
  const today = todayStr();
  const row = data.history?.days.find((d) => d.date === today);
  if (row) {
    income = row.income;
    expense = row.expense;
    for (const p of PLATFORM_ORDER) {
      const part = row.parts?.[p.key];
      todayPerPlatform[p.key] = {
        income: part?.income ?? 0,
        expense: part?.expense ?? 0,
      };
    }
  }
  bumpAt(data.history?.sampledAt);

  return {
    total: { known: totalKnown, value: totalValue, perPlatform },
    expiring: { known: expiringKnown, value: expiringValue, soonAt },
    today: { income, expense, perPlatform: todayPerPlatform },
    sampledAt,
  };
}

function StatTile({
  label,
  value,
  valueColor,
  sub,
  loading,
}: {
  label: string;
  value: string;
  valueColor?: string;
  sub?: ReactNode;
  loading: boolean;
}) {
  return (
    <div className="rounded-lg border bg-muted/30 px-3 py-2.5">
      <p className="text-xs text-muted-foreground">{label}</p>
      {loading ? (
        <Skeleton className="mt-1.5 h-6 w-16" />
      ) : (
        <p
          className="mt-1 font-mono text-lg font-semibold tabular-nums leading-6"
          style={valueColor ? { color: valueColor } : undefined}
        >
          {value}
        </p>
      )}
      {!loading && sub !== undefined && (
        <p className="mt-0.5 text-[11px] text-muted-foreground/80">{sub}</p>
      )}
    </div>
  );
}

export function CreditsSummaryCard() {
  const [data, setData] = useState<Awaited<ReturnType<typeof gather>> | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    setFailed(false);
    const res = await gather();
    setData(res);
    setFailed(res.trae === null && res.wb === null && res.qoder === null && res.history === null);
    setLoading(false);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const s = useMemo(() => (data ? summarize(data) : null), [data]);

  const platformSub = s
    ? PLATFORM_ORDER.filter((p) => s.total.perPlatform[p.key]?.known)
        .map((p) => `${p.label} ${fmt(s.total.perPlatform[p.key].total)}`)
        .join(" · ")
    : "";

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-start justify-between gap-2">
          <div>
            <CardTitle className="text-base">积分概要</CardTitle>
            <CardDescription className="mt-1">
              Trae / WorkBuddy / Qoder 三平台账号的缓存积分与今日快照差值，进入页面时读取，不联网。
            </CardDescription>
          </div>
          <Button
            variant="outline"
            size="icon"
            className="size-7"
            title="重新读取"
            disabled={loading}
            onClick={() => void load()}
          >
            {loading ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <RefreshCw className="size-3.5" />
            )}
          </Button>
        </div>
      </CardHeader>
      <CardContent className="pt-0">
        {failed ? (
          <div className="flex h-20 items-center justify-center gap-2 text-xs text-destructive">
            <TriangleAlert className="size-4" />
            积分数据读取失败
            <Button variant="outline" size="sm" className="h-6 px-2 text-xs" onClick={() => void load()}>
              重试
            </Button>
          </div>
        ) : (
          <>
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-5">
            <StatTile
              loading={loading}
              label="积分总额"
              value={s && s.total.known ? fmt(s.total.value) : "—"}
              sub={
                s && s.total.known && platformSub ? (
                  <span className="break-all">{platformSub}</span>
                ) : (
                  "暂无积分数据"
                )
              }
            />
            <StatTile
              loading={loading}
              label="近期到期积分（7 天内）"
              value={s && s.expiring.known ? fmt(s.expiring.value) : "—"}
              valueColor={s?.expiring.known && s.expiring.value > 0 ? "#f59e0b" : undefined}
              sub={
                s?.expiring.known && s.expiring.soonAt
                  ? `最近到期 ${fmtTime(s.expiring.soonAt)}`
                  : s?.expiring.known
                    ? "7 天内无到期"
                    : "无到期信息"
              }
            />
            <StatTile
              loading={loading}
              label="今日新增"
              value={s?.today.income != null ? `+${fmt(s.today.income)}` : "—"}
              valueColor={s?.today.income != null ? "#ef4444" : undefined}
              sub={s?.today.income != null ? "快照差值" : "今日无快照记录"}
            />
            <StatTile
              loading={loading}
              label="今日消耗"
              value={s?.today.expense != null ? `-${fmt(s.today.expense)}` : "—"}
              valueColor={s?.today.expense != null ? "#10b981" : undefined}
              sub={s?.today.expense != null ? "快照差值" : "今日无快照记录"}
            />
            <StatTile
              loading={loading}
              label="统计时间"
              value={s?.sampledAt ? fmtTime(s.sampledAt) : "—"}
              sub={s?.sampledAt ? "各平台最新数据时间" : "暂无数据时间"}
            />
          </div>

          {/* 平台明细：余额 / 占比来自各平台缓存，获取 / 消耗为今日快照差值（口径与上方总览一致）。 */}
          <div className="mt-3">
            <p className="mb-1.5 text-xs font-medium text-muted-foreground">平台明细</p>
            <Table>
              <TableHeader>
                <TableRow className="hover:bg-transparent">
                  <TableHead className="h-8 px-2 text-xs">平台</TableHead>
                  <TableHead className="h-8 px-2 text-right text-xs">余额</TableHead>
                  <TableHead className="h-8 px-2 text-right text-xs">占比</TableHead>
                  <TableHead className="h-8 px-2 text-right text-xs">今日获取</TableHead>
                  <TableHead className="h-8 px-2 text-right text-xs">今日消耗</TableHead>
                  <TableHead className="h-8 px-2 text-right text-xs">数据时间</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {loading
                  ? PLATFORM_ORDER.map((p) => (
                      <TableRow key={p.key}>
                        <TableCell className="h-9 px-2 py-1.5" colSpan={6}>
                          <Skeleton className="h-4 w-full max-w-md" />
                        </TableCell>
                      </TableRow>
                    ))
                  : PLATFORM_ORDER.map((p) => {
                      const st = s?.total.perPlatform[p.key];
                      const todayP = s?.today.perPlatform[p.key];
                      return (
                        <TableRow key={p.key}>
                          <TableCell className="h-9 px-2 py-1.5 text-xs font-medium">{p.label}</TableCell>
                          <TableCell className="h-9 px-2 py-1.5 text-right font-mono text-xs tabular-nums">
                            {st?.known ? fmt(st.total) : "—"}
                          </TableCell>
                          <TableCell className="h-9 px-2 py-1.5 text-right font-mono text-xs tabular-nums text-muted-foreground">
                            {st?.known && s ? fmtPct(st.total, s.total.value) : "—"}
                          </TableCell>
                          <TableCell className="h-9 px-2 py-1.5 text-right font-mono text-xs tabular-nums">
                            {todayP?.income != null ? (
                              <span style={{ color: todayP.income > 0 ? "#ef4444" : undefined }}>
                                +{fmt(todayP.income)}
                              </span>
                            ) : (
                              "—"
                            )}
                          </TableCell>
                          <TableCell className="h-9 px-2 py-1.5 text-right font-mono text-xs tabular-nums">
                            {todayP?.expense != null ? (
                              <span style={{ color: todayP.expense > 0 ? "#10b981" : undefined }}>
                                -{fmt(todayP.expense)}
                              </span>
                            ) : (
                              "—"
                            )}
                          </TableCell>
                          <TableCell className="h-9 px-2 py-1.5 text-right font-mono text-xs tabular-nums text-muted-foreground">
                            {st?.updatedAt ? fmtTime(st.updatedAt) : "—"}
                          </TableCell>
                        </TableRow>
                      );
                    })}
              </TableBody>
            </Table>
            <p className="mt-1.5 text-[11px] text-muted-foreground/70">
              余额与占比来自各平台缓存；今日获取 / 消耗为快照差值，今日无快照记录时显示 —。
            </p>
          </div>
          </>
        )}
      </CardContent>
    </Card>
  );
}

const PLATFORM_ORDER = [
  { key: "trae", label: "Trae" },
  { key: "workbuddy", label: "WB" },
  { key: "qoder", label: "Qoder" },
] as const;

export default CreditsSummaryCard;
