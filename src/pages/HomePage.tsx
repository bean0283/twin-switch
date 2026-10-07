import { useCallback, useEffect, useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { toast } from "sonner";
import {
  AlertTriangle,
  ArrowRight,
  Coins,
  Cpu,
  Eraser,
  HardDrive,
  HardDriveDownload,
  HardDriveUpload,
  KeyRound,
  Loader2,
  MessageSquare,
  RefreshCw,
  Sparkles,
  Trash2,
  Users,
} from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { TraeClientSwitcher, type TraeClientOption } from "@/components/trae-client-switcher";
import * as api from "@/lib/api";
import { creditResourceName } from "@/lib/credit-package-names";
import { refreshOverview, rememberReclaim, useOverviewState } from "@/lib/overview-store";
import { cn } from "@/lib/utils";
import type { OverviewTraeClient, WbCreditResource, WbCreditsResult } from "@/lib/trae-types";

/**
 * 可回收空间的「保鲜期」：超过这个时间才在后台重扫。
 *
 * 两侧清理扫描都要递归统计 GB 级目录，属于本页最贵的一步；
 * 缓存里有 30 分钟内的结果就直接用，不再拖慢启动。
 */
const RECLAIM_TTL_MS = 30 * 60 * 1000;

/** 字节 → 人类可读；与清理页保持同一口径。 */
function human(bytes: number): string {
  if (!bytes) return "0 B";
  const mb = bytes / 1024 / 1024;
  if (mb < 1) return `${(bytes / 1024).toFixed(0)} KB`;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

function fmtTime(ms: number | null | undefined): string {
  if (!ms) return "—";
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fmtDay(ms: number | null | undefined): string {
  if (!ms) return "长期有效";
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fmtCredits(v: number | undefined): string {
  if (v == null) return "—";
  return v.toLocaleString("zh-CN", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** 「刚刚 / 3 分钟前 / 2 小时前」——用于说明缓存有多旧。 */
function ago(ms: number | null | undefined): string {
  if (!ms) return "—";
  const diff = Date.now() - ms;
  if (diff < 60_000) return "刚刚";
  const min = Math.floor(diff / 60_000);
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  return `${Math.floor(hour / 24)} 天前`;
}

/** 状态点：运行中 = 绿，未运行 = 灰。 */
function Dot({ on, label }: { on: boolean; label: string }) {
  return (
    <span className="inline-flex items-center gap-1.5 text-xs">
      <span
        className={cn("size-2 rounded-full", on ? "bg-emerald-500" : "bg-muted-foreground/40")}
        aria-hidden="true"
      />
      <span className={on ? "text-foreground" : "text-muted-foreground"}>{label}</span>
    </span>
  );
}

function Stat({
  icon,
  label,
  value,
  sub,
  loading,
}: {
  icon: React.ReactNode;
  label: string;
  value: string;
  sub?: string;
  loading?: boolean;
}) {
  return (
    <Card className="gap-0 py-0">
      <CardContent className="flex items-start gap-3 px-4 py-3.5">
        <span className="mt-0.5 flex size-8 shrink-0 items-center justify-center rounded-lg bg-foreground/[0.05] text-muted-foreground">
          {icon}
        </span>
        <div className="min-w-0">
          <div className="text-xs text-muted-foreground">{label}</div>
          {loading ? (
            <Skeleton className="mt-1 h-6 w-16" />
          ) : (
            <div className="mt-0.5 truncate text-xl leading-6 font-semibold tabular-nums">
              {value}
            </div>
          )}
          {sub ? <div className="mt-0.5 truncate text-xs text-muted-foreground">{sub}</div> : null}
        </div>
      </CardContent>
    </Card>
  );
}

/** 一行「标签 — 值」，用于状态卡里的信息罗列。 */
function Row({
  label,
  children,
  mono,
}: {
  label: string;
  children: React.ReactNode;
  mono?: boolean;
}) {
  return (
    <div className="flex items-baseline justify-between gap-3 py-1 text-sm">
      <span className="shrink-0 text-xs text-muted-foreground">{label}</span>
      <span className={cn("min-w-0 truncate text-right", mono && "font-mono text-xs")}>
        {children}
      </span>
    </div>
  );
}

/** 积分包一行（名称 + 剩余 + 到期）。 */
function PackageLine({ r }: { r: WbCreditResource }) {
  const pct = r.total > 0 ? Math.max(0, Math.min(100, (r.remaining / r.total) * 100)) : 0;
  const tone = r.expired
    ? "text-destructive"
    : r.expiringSoon
      ? "text-amber-600"
      : "text-muted-foreground";
  return (
    <div className="space-y-1">
      <div className="flex items-baseline justify-between gap-2">
        <span className="min-w-0 truncate text-xs" title={r.packageCode ?? ""}>
          <span className="font-medium tabular-nums">{fmtCredits(r.remaining)}</span>
          <span className="ml-1 text-muted-foreground">积分</span>
          <span className="ml-1.5 text-muted-foreground">{creditResourceName(r)}</span>
        </span>
        <span className={cn("shrink-0 text-xs tabular-nums", tone)}>
          {r.expired ? "已过期" : fmtDay(r.expireAt)}
        </span>
      </div>
      <div className="h-1 overflow-hidden rounded-full bg-muted">
        <div
          className={cn("h-full rounded-full", r.expired ? "bg-destructive/60" : "bg-emerald-500")}
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}

/**
 * Trae 单个客户端一块：账号库 / 会话 / 解密库 / 积分**全部只算它自己**。
 *
 * ⚠️ 首页用 `<TraeClientSwitcher>` 切换「看哪一个客户端」，但**数字永远不许合并**：
 *    Trae 的 4 个客户端各有独立的 `database.db` 与独立的账号库，
 *    合并出来的数字谁也不对应。切换只决定看哪一块，每一块的数字仍只由它自己的 key 算出。
 *
 * ⚠️ 标题与「常用」徽标由切换条承担（单一出口），这一块里不再重复，只保留明细行。
 */
function TraeClientBlock({
  client,
  creditBusy,
  onQueryCredits,
}: {
  client: OverviewTraeClient;
  creditBusy: boolean;
  onQueryCredits: () => void;
}) {
  const d = client.decrypted;
  const cr = client.credits;
  return (
    <div className="rounded-lg border">
      <div className="divide-y px-3">
        <Row label="运行状态">
          <Dot
            on={client.running}
            label={client.running ? `${client.processCount} 个进程` : "已退出"}
          />
        </Row>
        <Row label="登录状态">
          {client.hasLogin ? (
            <span className="text-emerald-600">
              已登录{client.loginLabel ? ` · ${client.loginLabel}` : ""}
            </span>
          ) : (
            <span className="text-muted-foreground">未登录</span>
          )}
        </Row>
        <Row label="账号库">
          {client.accounts} 个 · 其中 {cr.queryable} 个可查积分
        </Row>
        <Row label="会话">
          {d.sessionCount != null
            ? `${d.sessionCount} 个（本客户端库）`
            : "未解密，读不到会话数"}
        </Row>
        <Row label="解密库">
          {d.exists ? (
            d.current ? (
              <span className="text-emerald-600">最新（与实时库一致）</span>
            ) : (
              <span className="text-amber-600">待刷新</span>
            )
          ) : (
            <span className="text-muted-foreground">不存在，需先解密</span>
          )}
        </Row>
        <Row label="客户端路径" mono>
          {client.exe ?? "未定位到"}
        </Row>
      </div>

      {/* 积分：离线读该客户端账号库里的 profile.json，点按钮才联网。 */}
      <div className="border-t p-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="flex items-center gap-1.5 text-sm font-medium">
            <Coins className="size-4 text-muted-foreground" />
            积分
          </span>
          <Button
            variant="ghost"
            size="sm"
            className="h-7 gap-1 px-2 text-xs"
            onClick={onQueryCredits}
            disabled={creditBusy || cr.queryable === 0}
            title={
              cr.queryable === 0
                ? "这个客户端的账号库里没有带网页凭证的账号，查不了积分"
                : "只查这个客户端账号库里的账号，不影响其它客户端"
            }
          >
            {creditBusy ? (
              <Loader2 className="size-3.5 animate-spin" />
            ) : (
              <RefreshCw className="size-3.5" />
            )}
            查询积分
          </Button>
        </div>
        {cr.withData > 0 ? (
          <div className="mt-1.5 flex items-baseline gap-2">
            <span className="text-lg leading-6 font-semibold tabular-nums">
              {fmtCredits(cr.totalRemaining)}
            </span>
            <span className="text-xs text-muted-foreground">
              合计剩余 · {cr.withData}/{cr.accountCount} 个账号
              {cr.updatedAt ? ` · ${fmtTime(cr.updatedAt)}` : ""}
            </span>
          </div>
        ) : (
          <p className="mt-1.5 text-xs text-muted-foreground">
            {cr.accountCount === 0
              ? "这个客户端还没有账号，先到「Trae 账号管理」添加。"
              : cr.queryable === 0
                ? "账号库里只有切换载体账号（没有网页凭证），查不了积分。"
                : "尚未查询。点「查询积分」拉取这个客户端的账号额度。"}
          </p>
        )}
      </div>
    </div>
  );
}

export default function HomePage() {
  /**
   * 总览数据来自**进程内共享 store**（App 启动时已经预热过一遍）。
   * 因此这里没有「首次加载」的概念：要么直接拿到缓存，要么拿到实时结果，
   * 页面从一开始就是可操作的，不再整页骨架屏。
   */
  const { overview: ov, reclaim, refreshing, fromCache, generatedAt, error } = useOverviewState();

  /** 手动「查询积分」的结果覆盖（只影响本页积分摘要，不动总览缓存）。 */
  const [creditsOverride, setCreditsOverride] = useState<WbCreditsResult | null>(null);
  const [creditsBusy, setCreditsBusy] = useState(false);
  const [reclaimBusy, setReclaimBusy] = useState(false);
  /** 正在联网查积分的 Trae 客户端 key（一次只查一个客户端，互不影响）。 */
  const [traeCreditBusy, setTraeCreditBusy] = useState<string | null>(null);
  /**
   * 首页正在看哪一个 Trae 客户端。
   *
   * ⚠️ 这里只决定「看哪一块」，不参与任何统计：每个客户端的账号 / 会话 / 积分
   *    都由后端按它自己的 key 算好。默认值必须是列表第一项 —— 后端已按
   *    `client_usage::sort_installed()` 排过序，前端别再写死某个 key。
   */
  const [traeClientKey, setTraeClientKey] = useState<string | null>(null);

  const credits = creditsOverride ?? ov?.credits ?? null;

  /**
   * 可回收空间：两侧清理扫描都要递归统计几 GB 目录，**必须放后台**。
   * 缓存里 30 分钟内的结果直接用，避免每次进首页都重扫。
   */
  const loadReclaim = useCallback(async (force = true) => {
    setReclaimBusy(true);
    try {
      // force=true：这条是**全应用唯一主动重扫的入口**（启动后台扫 + 首页手动刷新）。
      // 清理页那两处只读缓存，所以新鲜数据的责任全在这里。本函数被上面那个
      // RECLAIM_TTL_MS 闸门限流（30 分钟最多一次），不会造成反复重扫。
      const [trae, wb] = await Promise.allSettled([
        api.traeCleanupScan(force),
        api.traeWbCleanupScan(force),
      ]);
      const traeBytes =
        trae.status === "fulfilled"
          ? (trae.value.categories ?? [])
              .flatMap((c) => c.items ?? [])
              .filter((i) => i.recommended)
              .reduce((s, i) => s + (i.bytes ?? 0), 0)
          : 0;
      const wbBytes =
        wb.status === "fulfilled" ? (wb.value.totals?.recommended_bytes ?? 0) : 0;
      // 立刻更新界面并写回后端缓存，下次启动连这块都是热的
      rememberReclaim(traeBytes, wbBytes);
    } finally {
      setReclaimBusy(false);
    }
  }, []);

  useEffect(() => {
    // 「启动时后台自动扫一遍」就靠这里：前端记着上次扫的时间（30 分钟内不重复扫，
    // 避免重启一下应用就白跑一次几 GB 的遍历）。清理页自己不再重扫。
    if (reclaim && Date.now() - reclaim.at < RECLAIM_TTL_MS) return;
    // 错开启动那一瞬间：首屏渲染、缓存回读、总览重算都在开局抢磁盘与 CPU，
    // 把这一条最贵的扫描往后压半秒，开局更干净。
    // （扫描本身已经在 Rust 的后台线程池里跑，这里纯粹是让 I/O 别挤在一起。）
    const timer = window.setTimeout(() => void loadReclaim(), 600);
    return () => window.clearTimeout(timer);
    // reclaim?.at 变了说明扫完了，此时不该再触发一次 —— 依赖里只放 reclaim
  }, [reclaim, loadReclaim]);

  const refreshAll = useCallback(async () => {
    await Promise.allSettled([refreshOverview(), loadReclaim()]);
  }, [loadReclaim]);

  const refreshCredits = useCallback(async () => {
    setCreditsBusy(true);
    try {
      const res = await api.workbuddyCreditsQuery(true);
      setCreditsOverride(res);
      toast.success(`积分已更新 ${res.summary.succeeded}/${res.summary.queried}`, {
        description: `合计剩余 ${fmtCredits(res.summary.totalRemaining)}`,
      });
    } catch (e) {
      toast.error("积分查询失败", { description: e instanceof Error ? e.message : String(e) });
    } finally {
      setCreditsBusy(false);
    }
  }, []);

  const wb = ov?.workbuddy;
  const trae = ov?.trae;
  const totalSessions = (trae?.sessionTotal ?? 0) + (wb?.sessionCount ?? 0);
  const reclaimTotal = reclaim?.totalBytes ?? 0;

  /** 本机已安装的 Trae 客户端（后端已按使用记忆排序，`[0]` 就是默认要看的那一个）。 */
  const traeClients = useMemo(() => trae?.clients ?? [], [trae]);

  /**
   * 切换条的入参；`top` 只认后端下发的 `topPick`，前端不另算「谁最常用」。
   * 同一个组件也被「Trae 会话记录」用着，这里只做字段翻译。
   */
  const traeClientOptions = useMemo<TraeClientOption[]>(
    () =>
      traeClients.map((c) => ({
        key: c.key,
        label: c.label,
        installed: c.installed,
        hasLogin: c.hasLogin,
        top: c.key === trae?.topPick,
      })),
    [traeClients, trae?.topPick],
  );

  /**
   * 选中的客户端。列表首项兜底 + 已被卸载/消失时自动落回首项，
   * 因此下拉不会出现「选中态指向一个不存在的客户端」。
   */
  const activeTraeClient = useMemo(
    () => traeClients.find((c) => c.key === traeClientKey) ?? traeClients[0] ?? null,
    [traeClients, traeClientKey],
  );

  useEffect(() => {
    if (traeClients.length === 0) {
      if (traeClientKey !== null) setTraeClientKey(null);
      return;
    }
    if (!traeClients.some((c) => c.key === traeClientKey)) {
      setTraeClientKey(traeClients[0].key);
    }
  }, [traeClients, traeClientKey]);

  /**
   * 查询**单个 Trae 客户端**账号库的积分。
   *
   * 查完直接重算总览 —— 不在这里自己拼一份积分摘要：摘要的唯一出口是后端的
   * `app_overview::trae_client_credits`，在两端各推一遍迟早会对不上。
   * 代价是多跑一次总览（和点「刷新」同一条路径），换来的是首页数字永远同源。
   */
  const refreshTraeCredits = useCallback(async (clientKey: string, label: string) => {
    setTraeCreditBusy(clientKey);
    try {
      const res = await api.traeCreditsQuery(clientKey, true);
      toast.success(`${label}：积分已更新 ${res.summary.succeeded}/${res.summary.queried}`, {
        description: `合计剩余 ${fmtCredits(res.summary.totalRemaining)}`,
      });
      await refreshOverview();
    } catch (e) {
      toast.error("积分查询失败", { description: e instanceof Error ? e.message : String(e) });
    } finally {
      setTraeCreditBusy(null);
    }
  }, []);

  /** 首页只展示「近期到期」的前 3 个包，完整列表在「WorkBuddy 账号管理」页。 */
  const expiring = useMemo(() => {
    return (credits?.accounts ?? [])
      .flatMap((a) =>
        (a.activeResources ?? [])
          .filter((r) => r.expireAt != null)
          .slice(0, 2)
          .map((r) => ({ name: a.account.name, r })),
      )
      .sort((x, y) => (x.r.expireAt ?? 0) - (y.r.expireAt ?? 0))
      .slice(0, 3);
  }, [credits]);

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h1 className="flex flex-wrap items-center gap-2 text-xl font-semibold tracking-tight">
            首页 · 本机概览
            {fromCache ? (
              <Badge variant="secondary" className="gap-1 text-[11px] font-normal">
                <HardDrive className="size-3" />
                缓存 · {ago(generatedAt)}
              </Badge>
            ) : null}
          </h1>
          <p className="mt-1 text-sm text-muted-foreground">
            Trae 与 WorkBuddy 的账号、会话、客户端与可回收空间的汇总。本页全部只读，不会改动任何客户端数据。
            {generatedAt ? <span className="ml-2 text-xs">采集于 {fmtTime(generatedAt)}</span> : null}
          </p>
        </div>
        <Button variant="outline" onClick={() => void refreshAll()} disabled={refreshing}>
          {refreshing ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
          {refreshing ? "刷新中…" : "刷新"}
        </Button>
      </div>

      {error ? (
        <Alert variant={ov ? "default" : "destructive"}>
          <AlertTriangle className="size-4" />
          <AlertTitle>{ov ? "本次刷新失败（下面显示的是上一次的数据）" : "加载失败"}</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {/* 四个大数字 */}
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <Stat
          icon={<Users className="size-4" />}
          label="Trae 账号"
          value={String(trae?.accountTotal ?? 0)}
          sub={`${trae?.installedClients ?? 0} 个客户端已安装`}
          loading={!trae}
        />
        <Stat
          icon={<Users className="size-4" />}
          label="WorkBuddy 账号"
          value={String(wb?.accountCount ?? 0)}
          sub={`${wb?.queryableCount ?? 0} 个可查积分`}
          loading={!wb}
        />
        <Stat
          icon={<MessageSquare className="size-4" />}
          label="会话总数"
          value={totalSessions.toLocaleString("zh-CN")}
          sub={
            trae?.anyDecrypted
              ? `Trae 已解密客户端 ${trae.sessionTotal} · WorkBuddy ${wb?.sessionCount ?? "—"}`
              : `Trae 未解密 · WorkBuddy ${wb?.sessionCount ?? "—"}`
          }
          loading={!ov}
        />
        <Stat
          icon={<Eraser className="size-4" />}
          label="可回收空间"
          value={reclaim ? human(reclaimTotal) : reclaimBusy ? "统计中…" : "—"}
          sub={
            reclaim
              ? `Trae ${human(reclaim.traeBytes)} · WorkBuddy ${human(reclaim.wbBytes)}${
                  reclaimBusy ? " · 更新中…" : ""
                }`
              : undefined
          }
          loading={!reclaim && reclaimBusy}
        />
      </div>

      {/* 双栏状态卡。
          ⚠️ `items-start`：Trae 侧是「每个客户端一块」，客户端多的时候会比 WorkBuddy
          侧高出一大截；默认的 `stretch` 会把矮的那张拉成同高，中间留一块空白。 */}
      <div className="grid items-start gap-4 lg:grid-cols-2">
        {/* Trae */}
        <Card className="flex flex-col">
          <CardHeader className="pb-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div>
                <CardTitle className="flex items-center gap-2 text-base">
                  <Cpu className="size-4" />
                  Trae 客户端
                </CardTitle>
                <CardDescription>
                  {trae
                    ? `${trae.runningClients}/${trae.installedClients} 个客户端在运行 · 每个客户端的账号库与会话库各自独立`
                    : "加载中…"}
                </CardDescription>
              </div>
              <Dot
                on={(trae?.runningClients ?? 0) > 0}
                label={(trae?.runningClients ?? 0) > 0 ? "运行中" : "已退出"}
              />
            </div>
          </CardHeader>
          <CardContent className="flex flex-1 flex-col gap-3">
            {!trae ? (
              <Skeleton className="h-24 rounded-lg" />
            ) : traeClients.length === 0 ? (
              <p className="text-sm text-muted-foreground">本机没有检测到已安装的 Trae 客户端。</p>
            ) : (
              <>
                {/* 本机有几个客户端就几个标签，与「Trae 会话记录」用的是同一个组件。 */}
                <TraeClientSwitcher
                  clients={traeClientOptions}
                  value={activeTraeClient?.key ?? null}
                  onChange={setTraeClientKey}
                  disabled={traeCreditBusy !== null}
                  label="Trae 客户端（首页）"
                />
                {activeTraeClient ? (
                  <TraeClientBlock
                    key={activeTraeClient.key}
                    client={activeTraeClient}
                    creditBusy={traeCreditBusy === activeTraeClient.key}
                    onQueryCredits={() =>
                      void refreshTraeCredits(activeTraeClient.key, activeTraeClient.label)
                    }
                  />
                ) : null}
              </>
            )}

            <div className="mt-auto flex flex-wrap gap-2 pt-1">
              <Button asChild variant="outline" size="sm">
                <Link to="/trae-records">
                  <ArrowRight className="size-3.5" />
                  会话记录
                </Link>
              </Button>
              <Button asChild variant="outline" size="sm">
                <Link to="/trae-switch">账号管理</Link>
              </Button>
            </div>
          </CardContent>
        </Card>

        {/* WorkBuddy */}
        <Card className="flex flex-col">
          <CardHeader className="pb-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div>
                <CardTitle className="flex items-center gap-2 text-base">
                  <Cpu className="size-4" />
                  WorkBuddy 客户端
                </CardTitle>
                <CardDescription>
                  {wb ? `数据根 ~/.workbuddy · 会话库 ${human(wb.dbBytes)}` : "加载中…"}
                </CardDescription>
              </div>
              <Dot on={wb?.running ?? false} label={wb?.running ? `${wb.processCount} 个进程` : "已退出"} />
            </div>
          </CardHeader>
          <CardContent className="flex flex-1 flex-col gap-3">
            {!wb ? (
              <Skeleton className="h-24 rounded-lg" />
            ) : (
              <div className="divide-y rounded-lg border px-3">
                <Row label="登录状态">
                  {wb.loggedIn ? (
                    <span className="text-emerald-600">
                      已登录{wb.currentLabel ? ` · ${wb.currentLabel}` : ""}
                    </span>
                  ) : (
                    <span className="text-muted-foreground">未登录</span>
                  )}
                </Row>
                <Row label="账号库">
                  {wb.accountCount} 个 · 其中 {wb.queryableCount} 个可查积分
                </Row>
                <Row label="会话">
                  {wb.sessionCount ?? "—"} 个未删除
                  {wb.deletedSessionCount ? ` · 已删除 ${wb.deletedSessionCount} 个` : ""}
                </Row>
                <Row label="会话正文">{wb.bodyFileCount} 个 jsonl 文件</Row>
                <Row label="登录态文件" mono>
                  {wb.authFileExists ? human(wb.authFileBytes) : "不存在"}
                </Row>
                <Row label="客户端路径" mono>
                  {wb.exe ?? "未定位到"}
                </Row>
              </div>
            )}

            {/* 积分摘要（只读缓存；完整积分包见「WorkBuddy 账号管理」） */}
            <div className="rounded-lg border p-3">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="flex items-center gap-1.5 text-sm font-medium">
                  <Coins className="size-4 text-muted-foreground" />
                  积分
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 gap-1 px-2 text-xs"
                  onClick={() => void refreshCredits()}
                  disabled={creditsBusy}
                >
                  {creditsBusy ? (
                    <Loader2 className="size-3.5 animate-spin" />
                  ) : (
                    <RefreshCw className="size-3.5" />
                  )}
                  查询积分
                </Button>
              </div>
              {credits && credits.summary.queried > 0 ? (
                <>
                  <div className="mt-1.5 flex items-baseline gap-2">
                    <span className="text-lg leading-6 font-semibold tabular-nums">
                      {fmtCredits(credits.summary.totalRemaining)}
                    </span>
                    <span className="text-xs text-muted-foreground">
                      合计剩余 · {credits.summary.succeeded}/{credits.summary.queried} 个账号
                      {credits.updatedAt ? ` · ${fmtTime(credits.updatedAt)}` : ""}
                      {credits.cached ? "（缓存）" : ""}
                    </span>
                  </div>
                  {expiring.length ? (
                    <div className="mt-2.5 space-y-2">
                      <div className="text-[11px] font-medium text-muted-foreground">近期到期</div>
                      {expiring.map(({ name, r }, i) => (
                        <div key={`${name}-${i}`}>
                          <div className="truncate text-[11px] text-muted-foreground">{name}</div>
                          <PackageLine r={r} />
                        </div>
                      ))}
                    </div>
                  ) : null}
                </>
              ) : (
                <p className="mt-1.5 text-xs text-muted-foreground">
                  尚未查询。点「查询积分」拉取（需要明文凭据账号；导入本机登录态的账号是加密信封，查不了）。
                </p>
              )}
              {credits && credits.errors.length ? (
                <p className="mt-2 text-xs text-amber-600">
                  {credits.errors.length} 个账号查询失败：
                  {credits.errors.map((e) => e.name).join("、")}
                </p>
              ) : null}
            </div>

            <div className="mt-auto flex flex-wrap gap-2 pt-1">
              <Button asChild variant="outline" size="sm">
                <Link to="/workbuddy-records">
                  <ArrowRight className="size-3.5" />
                  会话记录
                </Link>
              </Button>
              <Button asChild variant="outline" size="sm">
                <Link to="/workbuddy-switch">
                  <Coins className="size-3.5" />
                  账号与积分
                </Link>
              </Button>
            </div>
          </CardContent>
        </Card>
      </div>

      {/* 底部：可回收空间 + 注意事项 + 快捷入口 */}
      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader className="pb-3">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div>
                <CardTitle className="flex items-center gap-2 text-base">
                  <HardDrive className="size-4" />
                  可回收空间
                </CardTitle>
                <CardDescription>
                  建议清理项（缓存、备份批次、临时解密快照、空会话等）
                  {reclaim ? ` · 扫描于 ${ago(reclaim.at)}` : ""}
                </CardDescription>
              </div>
              <span className="flex items-center gap-2 text-lg font-semibold tabular-nums">
                {reclaim ? human(reclaimTotal) : reclaimBusy ? "统计中…" : "—"}
                {reclaimBusy ? <Loader2 className="size-4 animate-spin text-muted-foreground" /> : null}
              </span>
            </div>
          </CardHeader>
          <CardContent className="space-y-2">
            <div className="flex items-center justify-between gap-3 rounded-lg border px-3 py-2 text-sm">
              <span className="flex items-center gap-2">
                <Eraser className="size-4 text-muted-foreground" />
                Trae 侧建议清理
              </span>
              <span className="flex items-center gap-2">
                <span className="tabular-nums">{reclaim ? human(reclaim.traeBytes) : "…"}</span>
                <Button asChild variant="ghost" size="sm" className="h-7 px-2 text-xs">
                  <Link to="/trae-cleanup">去清理</Link>
                </Button>
              </span>
            </div>
            <div className="flex items-center justify-between gap-3 rounded-lg border px-3 py-2 text-sm">
              <span className="flex items-center gap-2">
                <Trash2 className="size-4 text-muted-foreground" />
                WorkBuddy 侧建议清理
              </span>
              <span className="flex items-center gap-2">
                <span className="tabular-nums">{reclaim ? human(reclaim.wbBytes) : "…"}</span>
                <Button asChild variant="ghost" size="sm" className="h-7 px-2 text-xs">
                  <Link to="/workbuddy-cleanup">去清理</Link>
                </Button>
              </span>
            </div>
            <p className="text-xs text-muted-foreground">
              清理默认移入本工具回收站（可勾选彻底删除），不会动登录态、网络配置与全局设置。
            </p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="flex items-center gap-2 text-base">
              <Sparkles className="size-4" />
              提示与快捷入口
            </CardTitle>
            <CardDescription>
              {ov?.notes?.length ? `${ov.notes.length} 条注意事项` : "一切正常"}
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-3">
            {(ov?.notes ?? []).length ? (
              <ul className="space-y-1.5 text-xs leading-5 text-amber-700">
                {(ov?.notes ?? []).map((n, i) => (
                  <li key={i} className="flex gap-1.5">
                    <KeyRound className="mt-0.5 size-3.5 shrink-0" />
                    <span>{n}</span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="text-xs text-muted-foreground">
                没有需要提醒的事项。会话、账号与客户端状态都正常。
              </p>
            )}
            {/*
              跨工具迁移：**文案、分组名、图标三样都与侧栏「数据迁移」区域严格一致**。
              ⚠️ 同一个功能在首页叫「导入 / 导出」、在侧栏叫「A → B」，用户就得猜是不是
                 同一件事；这里统一到侧栏那套「谁 → 谁」的说法，页面标题也是这么写的。
            */}
            <div className="space-y-2 pt-1">
              <div className="flex items-center gap-2">
                <span className="text-[11px] font-medium tracking-wide text-muted-foreground/70">
                  数据迁移
                </span>
                <span className="h-px flex-1 bg-border" />
              </div>
              <p className="text-xs leading-5 text-muted-foreground">
                <span className="font-medium text-foreground/80">WorkBuddy → Trae</span> 把 WorkBuddy
                的明文会话转成 Trae 的库结构后加密写入指定账号；
                <span className="font-medium text-foreground/80"> Trae → WorkBuddy</span>{" "}
                把 Trae 的会话导出成本机 WorkBuddy 的明文记录。
              </p>
              <div className="flex flex-wrap gap-2">
                <Button asChild variant="outline" size="sm">
                  <Link to="/workbuddy-import">
                    <HardDriveDownload className="size-3.5" />
                    WorkBuddy → Trae
                  </Link>
                </Button>
                <Button asChild variant="outline" size="sm">
                  <Link to="/workbuddy-export">
                    <HardDriveUpload className="size-3.5" />
                    Trae → WorkBuddy
                  </Link>
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
