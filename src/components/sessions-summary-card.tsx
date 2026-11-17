import { useCallback, useEffect, useState } from "react";
import { AlertTriangle, Loader2, MessagesSquare, RefreshCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import * as api from "@/lib/api";
import type { SessionsOverview, SessionsOverviewPlatform } from "@/lib/trae-types";

/**
 * 会话管理首页的「会话概要」。
 *
 * ## 为什么值得有
 *
 * 三个会话页各自只回答「这条会话是什么」，没人回答「我这儿一共堆了多少、占多少地方、
 * 多久没动过」。而清理才是这个工具最常用的动作 —— 没有总量，用户不知道该不该动手。
 *
 * ## 口径（与后端 `sessions_overview` 严格一致）
 *
 * - **正文占用**只累加「会话正文」：WorkBuddy 的 `body_bytes` + Qoder 的 `payload_bytes`，
 *   两者都是上游自己记的正文体积，可以相加；
 * - **Trae 的占用是本工具的解密副本**（`~/.twin-switch/trae/decrypted/*.db`），
 *   既不是客户端原库、也不是正文，所以**不并入总量**，单独一列标注；
 * - **30 天未动**用会话自身更新时间判断（不用文件 mtime：备份工具会碰 mtime）。
 *
 * ## 空/失败纪律
 *
 * 读不到就显示「读取失败」并保留上次成功的数据；`note` 非空说明这一端的数字不完整，
 * 必须显示出来 —— 半截数字冒充全量是这个项目反复踩过的坑。
 */

const PLATFORM_NAME: Record<string, string> = {
  trae: "Trae",
  workbuddy: "WorkBuddy",
  qoder: "Qoder",
};

function compactBytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n) || n <= 0) return "—";
  const gb = n / 1024 ** 3;
  if (gb >= 1) return `${gb.toFixed(2)} GB`;
  const mb = n / 1024 ** 2;
  if (mb >= 1) return `${mb.toFixed(0)} MB`;
  return `${Math.max(1, Math.round(n / 1024))} KB`;
}

function ago(ms: number | null | undefined): string {
  if (!ms) return "—";
  const diff = Date.now() - ms;
  if (diff < 0) return "刚刚";
  const d = Math.floor(diff / 86_400_000);
  if (d <= 0) return "今天";
  if (d === 1) return "昨天";
  if (d < 30) return `${d} 天前`;
  return `${Math.floor(d / 30)} 个月前`;
}

function Tile({
  label,
  value,
  hint,
  tone,
}: {
  label: string;
  value: string;
  hint: string;
  tone?: "warn" | "ok";
}) {
  return (
    <div className="rounded-lg border border-border/70 px-3 py-2">
      <div className="text-[11px] text-muted-foreground">{label}</div>
      <div
        className={
          "mt-0.5 text-lg font-medium tabular-nums leading-6 " +
          (tone === "warn" ? "text-amber-600" : tone === "ok" ? "text-emerald-600" : "")
        }
      >
        {value}
      </div>
      <div className="text-[10px] text-muted-foreground/80">{hint}</div>
    </div>
  );
}

export function SessionsSummaryCard() {
  const [data, setData] = useState<SessionsOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async (force: boolean) => {
    if (force) setBusy(true);
    else setLoading(true);
    try {
      const r = await api.sessionsOverview();
      if (r?.ok === false) throw new Error(r.error || "读取会话概要失败");
      setData(r);
      setError(null);
    } catch (e) {
      // 失败不清空上次结果：清空会让人以为「本来就没有会话」。
      setError(api.asError(e).split("\n")[0]);
    } finally {
      setLoading(false);
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void load(false);
  }, [load]);

  const platforms = data?.platforms ?? [];
  const total = data?.total;
  const notes = platforms.filter((p) => p.note);

  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-start justify-between gap-2">
          <div>
            <CardTitle className="flex items-center gap-2 text-base">
              <MessagesSquare className="size-4" />
              会话概要
            </CardTitle>
            <CardDescription className="mt-1">
              三端本地会话的数量、占用与活跃度。进入页面时读取，不联网；
              「30 天未动」是可以优先清理的那批。
            </CardDescription>
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={() => void load(true)}
            title="重新统计三端会话"
            data-action="sessions-overview-refresh"
          >
            {busy ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
            {busy ? "统计中…" : "刷新"}
          </Button>
        </div>
      </CardHeader>
      <CardContent>
      {loading && !data ? (
        <div className="mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
          {[0, 1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-16" />
          ))}
        </div>
      ) : error && !data ? (
        <p className="mt-3 text-xs text-destructive">读取失败：{error}</p>
      ) : (
        <>
          <div className="mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
            <Tile
              label="会话总数"
              value={(total?.count ?? 0).toLocaleString("zh-CN")}
              hint={`Trae ${platforms.find((p) => p.platform === "trae")?.count ?? 0} · WorkBuddy ${
                platforms.find((p) => p.platform === "workbuddy")?.count ?? 0
              } · Qoder ${platforms.find((p) => p.platform === "qoder")?.count ?? 0}`}
            />
            <Tile
              label="正文占用"
              value={compactBytes(total?.contentBytes)}
              hint="WorkBuddy + Qoder 的会话正文"
            />
            <Tile
              label="近 7 天活跃"
              value={(total?.active7d ?? 0).toLocaleString("zh-CN")}
              hint="7 天内有更新的会话"
              tone="ok"
            />
            <Tile
              label="30 天未动"
              value={(total?.stale30d ?? 0).toLocaleString("zh-CN")}
              hint="可优先清理的那批"
              tone={(total?.stale30d ?? 0) > 0 ? "warn" : undefined}
            />
          </div>

          <div className="mt-3 overflow-x-auto">
            <table className="w-full text-xs">
              <thead className="text-muted-foreground">
                <tr className="border-b border-border/60">
                  <th className="py-1.5 pr-2 text-left font-medium">平台</th>
                  <th className="px-2 py-1.5 text-right font-medium">会话数</th>
                  <th className="px-2 py-1.5 text-right font-medium">占用</th>
                  <th className="px-2 py-1.5 text-right font-medium">近 7 天</th>
                  <th className="px-2 py-1.5 text-right font-medium">30 天未动</th>
                  <th className="px-2 py-1.5 text-right font-medium">最近更新</th>
                </tr>
              </thead>
              <tbody>
                {platforms.map((p: SessionsOverviewPlatform) => (
                  <tr key={p.platform} className="border-b border-border/40 last:border-b-0">
                    <td className="py-1.5 pr-2">{PLATFORM_NAME[p.platform] ?? p.platform}</td>
                    <td className="px-2 py-1.5 text-right tabular-nums">{p.count}</td>
                    <td className="px-2 py-1.5 text-right tabular-nums">
                      {p.platform === "trae" ? (
                        // Trae 的正文在 SQLite 里，只能给「本工具的解密副本」大小，
                        // 明确换一个说法，别让用户以为这是会话内容占用。
                        <span className="text-muted-foreground">
                          副本 {compactBytes(p.decryptedBytes)}
                        </span>
                      ) : (
                        compactBytes(p.bytes)
                      )}
                    </td>
                    <td className="px-2 py-1.5 text-right tabular-nums">{p.active7d}</td>
                    <td
                      className={
                        "px-2 py-1.5 text-right tabular-nums " +
                        (p.stale30d > 0 ? "text-amber-600" : "")
                      }
                    >
                      {p.stale30d}
                    </td>
                    <td className="px-2 py-1.5 text-right text-muted-foreground">
                      {ago(p.newestAt)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          {notes.length > 0 ? (
            <div className="mt-2 space-y-1">
              {notes.map((p) => (
                <p
                  key={p.platform}
                  className="flex items-start gap-1 text-[11px] text-amber-600"
                >
                  <AlertTriangle className="mt-0.5 size-3 shrink-0" />
                  <span>
                    {PLATFORM_NAME[p.platform] ?? p.platform}：{p.note}
                  </span>
                </p>
              ))}
            </div>
          ) : null}

          {error && data ? (
            <p className="mt-2 text-[11px] text-amber-600">
              上次刷新失败：{error}（下面是上次成功的数据）
            </p>
          ) : null}

          <p className="mt-2 text-[10px] leading-relaxed text-muted-foreground/80">
            口径：占用只算会话正文（WorkBuddy 的 body_bytes、Qoder 的 payload_bytes）；
            Trae 的正文在 SQLite 里没有单会话体积，那一列是本工具的解密副本大小，不计入总量。
            活跃度按会话自身更新时间算，不看文件 mtime。
          </p>
        </>
      )}
      </CardContent>
    </Card>
  );
}
