import { useCallback, useEffect, useMemo, useState } from "react";
import { AlertTriangle, DatabaseZap, Loader2, RefreshCw } from "lucide-react";
import { toast } from "sonner";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { cacheHitStats, asError } from "@/lib/api";
import type { CacheHitSide, CacheHitStats } from "@/lib/trae-types";

/**
 * 缓存命中三端对照卡。
 *
 * ## 为什么这张卡把「不可用」画得和「0%」一样大
 *
 * 命中率是**一个除法**，只有「有分子、有分母、且上游确实上报过缓存字段」时才成立。
 * 本机实测（2026-11-13）：
 * - 网关账本与 WorkBuddy 会话日志都有真值 → 能算；
 * - Trae / Qoder 客户端日志里**根本没有缓存字段**（Qoder 的 token 全是 0，只有 credits 有值）
 *   → 算不出。
 *
 * 所以后两行显示「—」+ 原因，而不是 0%。把「算不出」画成「0%」会让用户去查一个
 * 并不存在的缓存问题 —— 这条与项目「空 ≠ 失败」是同一条规矩。
 *
 * ## 命中率在哪算
 *
 * 后端只给原始数（`input` / `cached` / `observed`），率在**前端**按唯一公式算：
 *
 * ```text
 * 有观测 && input > 0  ⇒  cached / input
 * 否则                 ⇒  null（显示「—」）
 * ```
 *
 * 后端不直接给率，是因为 `observed` 必须和率同屏可见：只有看到「1662 条全部上报过」，
 * 才能相信 0.0% 是「真的没命中」而不是「一条都没观测到」。
 */

/** 紧凑数字：1.2M / 3.4B，避免一长串数字把瓦片撑破。 */
function compact(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}K`;
  return String(n);
}

function rateOf(side: CacheHitSide | undefined): number | null {
  if (!side?.available) return null;
  const input = side.input ?? 0;
  const observed = side.observed ?? 0;
  if (observed <= 0 || input <= 0) return null;
  return (side.cached ?? 0) / input;
}

function Row({
  label,
  side,
  hint,
}: {
  label: string;
  side: CacheHitSide | undefined;
  hint?: string;
}) {
  const rate = rateOf(side);
  const available = side?.available === true;
  const observed = side?.observed ?? 0;
  const requests = side?.requests ?? 0;

  return (
    <div className="flex items-start justify-between gap-3 border-b py-2 last:border-b-0">
      <div className="min-w-0">
        <div className="flex items-center gap-1.5">
          <span className="text-sm font-medium">{label}</span>
          {available ? (
            <Badge variant="secondary" className="h-4 px-1 text-[10px]">
              {observed > 0
                ? `${observed.toLocaleString("zh-CN")}/${requests.toLocaleString("zh-CN")} 条有读数`
                : "无读数"}
            </Badge>
          ) : (
            <Badge variant="outline" className="h-4 px-1 text-[10px]">
              暂不可用
            </Badge>
          )}
        </div>
        {/* 不可用必须给原因，且原因要能解释「为什么算不出」，不是一句「暂无数据」。 */}
        <p className="mt-0.5 text-[11px] leading-relaxed text-muted-foreground">
          {available
            ? hint ??
              `命中 ${compact(side?.cached)} / 输入 ${compact(side?.input)}${
                observed > 0 && (side?.cached ?? 0) === 0
                  ? "（上游回报的命中为 0，不是统计缺失）"
                  : ""
              }`
            : side?.reason ?? "没有可用数据"}
        </p>
      </div>
      <div className="shrink-0 text-right">
        <div className="text-lg font-medium tabular-nums leading-6">
          {rate == null ? "—" : `${(rate * 100).toFixed(1)}%`}
        </div>
        <div className="text-[10px] text-muted-foreground">
          {rate != null
            ? "缓存命中"
            : !available
              ? "算不出"
              : observed <= 0
                ? "无缓存读数"
                : "无可判定输入"}
        </div>
      </div>
    </div>
  );
}

export function CacheHitCard({ days = 7 }: { days?: number }) {
  const [stats, setStats] = useState<CacheHitStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(
    async (refresh: boolean) => {
      if (refresh) setBusy(true);
      else setLoading(true);
      try {
        const r = await cacheHitStats(days, refresh);
        if (r.ok === false) throw new Error(r.error || "统计失败");
        setStats(r);
        setError(null);
      } catch (e) {
        // 失败不清空旧数据：清空会让人以为「本来就没有命中」。
        setError(asError(e).split("\n")[0]);
        if (refresh) toast.error("刷新缓存统计失败", { description: asError(e).split("\n")[0] });
      } finally {
        setLoading(false);
        setBusy(false);
      }
    },
    [days],
  );

  useEffect(() => {
    void load(false);
  }, [load]);

  const wb = stats?.workbuddy;
  const wbHint = useMemo(() => {
    if (!wb?.available) return undefined;
    const parts = [
      `命中 ${compact(wb.cached)} / 输入 ${compact(wb.input)}`,
      `扫了 ${(wb.files ?? 0).toLocaleString("zh-CN")} 个会话 · ${compact(wb.bytes)} 字节`,
    ];
    if (wb.fromCache) parts.push(`缓存于 ${wb.scannedAt ?? "刚才"}`);
    else if (wb.elapsedMs) parts.push(`用时 ${wb.elapsedMs} ms`);
    return parts.join(" · ");
  }, [wb]);

  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between gap-3 pb-2">
        <div className="min-w-0">
          <CardTitle className="flex items-center gap-2 text-sm">
            <DatabaseZap className="size-4" />
            缓存命中（近 {stats?.days ?? days} 天）
          </CardTitle>
          <p className="mt-1 text-[11px] leading-relaxed text-muted-foreground">
            命中率 = 缓存命中 token / 输入 token。只有上游真的上报过缓存字段才算得出；
            算不出的显示「—」而不是 0%。
          </p>
        </div>
        <Button
          variant="outline"
          size="sm"
          disabled={busy}
          onClick={() => void load(true)}
          title="重新扫描网关账本与 WorkBuddy 会话日志（1 GB 级，需数秒）"
          data-action="cache-hit-refresh"
        >
          {busy ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
          {busy ? "统计中…" : "重新统计"}
        </Button>
      </CardHeader>
      <CardContent className="pt-0">
        {loading ? (
          <p className="py-6 text-center text-xs text-muted-foreground">读取中…</p>
        ) : error && !stats ? (
          <p className="py-6 text-center text-xs text-destructive">读取失败：{error}</p>
        ) : (
          <>
            <Row
              label="网关（中继流量）"
              side={stats?.gateway}
              hint={
                stats?.gateway?.available
                  ? `命中 ${compact(stats.gateway.cached)} / 输入 ${compact(stats.gateway.input)} · 共 ${(
                      stats.gateway.requests ?? 0
                    ).toLocaleString("zh-CN")} 次请求`
                  : undefined
              }
            />
            <Row label="WorkBuddy 客户端" side={wb} hint={wbHint} />
            <Row label="Trae 客户端" side={stats?.trae} />
            <Row label="Qoder 客户端" side={stats?.qoder} />
            {error && stats ? (
              <p className="mt-2 flex items-center gap-1 text-[11px] text-amber-600">
                <AlertTriangle className="size-3" />
                上次刷新失败：{error}（下面是上次成功的数据）
              </p>
            ) : null}
          </>
        )}
      </CardContent>
    </Card>
  );
}
