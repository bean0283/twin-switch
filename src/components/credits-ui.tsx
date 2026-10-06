import { useMemo } from "react";
import { AlertTriangle, ArrowRight, Clock3, Coins, Loader2, RefreshCw, Sparkles } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ResizableDialogContent } from "@/components/ui/resizable-dialog-content";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

/**
 * 积分展示的**统一模型**。
 *
 * WorkBuddy（积分包来自官方资源接口）与 Trae（积分包来自
 * `user_current_entitlement_list`）的数据形状不一样，但用户要看的东西是同一件事：
 * 「这个账号还剩多少、由哪几个包组成、哪个先到期」。
 *
 * 所以两边各自把自己的原始数据**适配**成这里的 `CreditView`，
 * 展示层只认这一种形状 —— 账号卡在两侧长得一模一样是结构性保证，而不是靠对照抄样式。
 */
export interface CreditPack {
  /** 稳定 key（WorkBuddy 用 packageCode，Trae 用 entitlement_id）。 */
  key: string;
  name: string;
  /** 总额度；`null` = 接口没给额度（不限量包）。 */
  total: number | null;
  used: number;
  /** 剩余；`null` = 不限量。 */
  remaining: number | null;
  /** 到期时间（毫秒）；`null` = 长期有效。 */
  expireAt: number | null;
  /** 已过期。缺省时按 `expireAt` 现算。 */
  expired?: boolean;
  /** 7 天内到期。缺省时按 `expireAt` 现算。 */
  expiringSoon?: boolean;
}

export interface CreditView {
  ok: boolean;
  /** 查询失败原因（`ok = false` 时展示成红色错误）。 */
  error?: string | null;
  /**
   * 中性说明：这个账号**根本没法查积分**（例如 Trae 侧的切换载体账号没有网页凭证）。
   *
   * 与 `error` 的区别是语气 —— 前者是「本来就不适用」，后者是「出错了」。
   */
  unavailable?: string | null;
  /** 剩余合计。 */
  remaining: number | null;
  total?: number | null;
  used?: number | null;
  /** 有效积分包个数（与 `packs.length` 可能不同：这里说「有几个还能用」）。 */
  packCount: number;
  packs: CreditPack[];
  /** 数据更新时间（毫秒）。 */
  updatedAt?: number | null;
  /** 近期到期金额（后端算好的；缺省时按 7 天内到期的包现算）。 */
  expiringSoonRemaining?: number | null;
  /** 卡片底部的补充说明（来源 / 回退提示）。 */
  notes?: string[];
}

/** 7 天 = 快到期的界。 */
const SOON_MS = 7 * 86400_000;

/** 积分保留两位小数（官方数据本身就是高精度小数，四舍五入反而对不上界面）。 */
export function credits(v: number | null | undefined): string {
  if (v == null) return "—";
  return v.toLocaleString("zh-CN", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** 到期时间：`10/13 09:00`；长期有效回落到文案。 */
export function fmtExpiry(ms: number | null): string {
  if (!ms) return "长期有效";
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 完整日期时间：`2026-10-13 09:00`（弹窗里用，卡片上放不下）。 */
function fmtFullDateTime(ms: number | null): string {
  if (!ms) return "长期有效";
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 只要时分：卡片右上角的「11:16 更新」。 */
export function fmtUpdated(ms: number | null | undefined): string {
  if (!ms) return "—";
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 补齐由 `expireAt` 推导的状态（后端已经算过的沿用后端口径）。 */
function decorate(p: CreditPack): CreditPack {
  if (p.expired !== undefined && p.expiringSoon !== undefined) return p;
  const left = p.expireAt == null ? null : p.expireAt - Date.now();
  return {
    ...p,
    expired: p.expired ?? (left != null && left <= 0),
    expiringSoon: p.expiringSoon ?? (left != null && left > 0 && left <= SOON_MS),
  };
}

/** 还有剩余的包：按到期时间升序（长期有效排最后）。 */
export function activeOf(view: CreditView | null): CreditPack[] {
  if (!view) return [];
  return view.packs
    .map(decorate)
    .filter((p) => !p.expired && (p.remaining == null || p.remaining > 0))
    .sort((a, b) => (a.expireAt ?? Number.POSITIVE_INFINITY) - (b.expireAt ?? Number.POSITIVE_INFINITY));
}

/** 进度条：正常=绿、7 天内到期=琥珀、已过期=红。 */
function progressPct(p: CreditPack): number {
  if (p.total == null || p.total <= 0 || p.remaining == null) return 0;
  return Math.max(0, Math.min(100, (p.remaining / p.total) * 100));
}

function barClass(p: CreditPack): string {
  if (p.expired) return "bg-destructive/60";
  if (p.expiringSoon) return "bg-amber-500/80";
  return "bg-emerald-500";
}

function expiryTone(p: CreditPack): string {
  if (p.expired) return "text-destructive";
  if (p.expiringSoon) return "text-amber-600";
  return "text-muted-foreground";
}

/** 卡片里的紧凑积分包行：`100 积分 · 签到奖励` + 右侧到期 + 进度条。 */
function PackageRow({ p }: { p: CreditPack }) {
  return (
    <div className="min-w-0 space-y-1.5">
      <div className="flex min-w-0 items-baseline justify-between gap-2">
        <span className="flex min-w-0 items-baseline gap-1.5">
          <span className="shrink-0 text-xs font-medium tabular-nums">
            {p.remaining == null ? "不限量" : `${credits(p.remaining)} 积分`}
          </span>
          <span className="min-w-0 truncate text-[11px] text-muted-foreground" title={p.name}>
            {p.name}
          </span>
        </span>
        <span className={cn("shrink-0 text-[11px] tabular-nums", expiryTone(p))}>
          {p.expired ? "已到期" : fmtExpiry(p.expireAt)}
        </span>
      </div>
      {p.total == null ? (
        <div className="h-1.5 rounded-full bg-muted" aria-hidden="true" />
      ) : (
        <div className="h-1.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
          <div className={cn("h-full rounded-full", barClass(p))} style={{ width: `${progressPct(p)}%` }} />
        </div>
      )}
    </div>
  );
}

/** 弹窗里的详细行：名称 + 剩余/总额度 + 已用 + 到期。 */
function PackageDetailRow({ p }: { p: CreditPack }) {
  return (
    <div className="min-w-0 py-3 first:pt-0 last:pb-0">
      <div className="flex min-w-0 items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="truncate text-sm font-medium">{p.name}</div>
          <div className="mt-1 text-[11px] text-muted-foreground">
            {p.expired ? "已到期" : p.expireAt ? `到期 ${fmtFullDateTime(p.expireAt)}` : "长期有效"}
          </div>
        </div>
        <div className="shrink-0 text-right text-xs">
          <div className="font-medium tabular-nums">
            {p.remaining == null ? "不限量" : `${credits(p.remaining)} / ${credits(p.total)}`}
          </div>
          <div className="mt-1 text-[11px] text-muted-foreground tabular-nums">已用 {credits(p.used)}</div>
        </div>
      </div>
      {p.total == null ? (
        <div className="mt-2 h-1.5 rounded-full bg-muted" aria-hidden="true" />
      ) : (
        <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
          <div className={cn("h-full rounded-full", barClass(p))} style={{ width: `${progressPct(p)}%` }} />
        </div>
      )}
    </div>
  );
}

/**
 * 单个账号的积分块：嵌进账号卡（卡片右上角放切换 / 改名 / 删除按钮）。
 *
 * 三种状态：加载中骨架 / 失败原因 / 正常数字。
 */
export function CreditBlock({
  item,
  loading,
  onExpand,
}: {
  item: CreditView | null;
  loading?: boolean;
  onExpand: () => void;
}) {
  const active = useMemo(() => activeOf(item), [item]);
  const shown = active.slice(0, 2);

  if (loading && !item) {
    return (
      <div className="space-y-2.5 pt-1">
        <Skeleton className="h-5 w-32" />
        <Skeleton className="h-4 w-full" />
        <Skeleton className="h-4 w-full" />
      </div>
    );
  }

  if (!item) {
    return <p className="pt-1 text-xs text-muted-foreground">未纳入积分统计。</p>;
  }

  if (item.unavailable) {
    return <p className="pt-1 text-xs leading-5 text-muted-foreground">{item.unavailable}</p>;
  }

  if (!item.ok) {
    return (
      <div className="flex items-start gap-2 pt-1 text-xs leading-5 text-destructive">
        <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
        <span>{item.error ?? "积分查询失败"}</span>
      </div>
    );
  }

  const soonAmount =
    item.expiringSoonRemaining ??
    active.filter((p) => p.expiringSoon).reduce((n, p) => n + (p.remaining ?? 0), 0);

  return (
    <div className="min-w-0">
      {/* 大数字 + 包数 + 更新时间 */}
      <div className="flex flex-wrap items-baseline gap-x-2.5 gap-y-1">
        <span className="flex items-baseline gap-1.5">
          <Sparkles className="size-4 shrink-0 translate-y-0.5 text-muted-foreground" aria-hidden="true" />
          <strong className="text-[22px] leading-none font-semibold tabular-nums tracking-[-0.02em]">
            {credits(item.remaining)}
          </strong>
        </span>
        <span className="text-xs text-muted-foreground">
          {item.packCount} 个积分包
        </span>
        <span className="ml-auto flex items-center gap-1 text-[11px] text-muted-foreground">
          <Clock3 className="size-3.5 shrink-0" />
          <span className="whitespace-nowrap tabular-nums">{fmtUpdated(item.updatedAt)} 更新</span>
        </span>
      </div>

      {/* 近期到期 + 查看全部 */}
      <div className="mt-3 flex items-center justify-between gap-2">
        <span className="text-[11px] font-medium text-muted-foreground">
          {soonAmount > 0
            ? `近期到期 ${credits(soonAmount)} 积分`
            : shown[0]?.expireAt
              ? `最近到期 ${fmtExpiry(shown[0].expireAt)}`
              : "当前积分长期有效"}
        </span>
        {active.length > 2 ? (
          <button
            type="button"
            onClick={onExpand}
            className="inline-flex shrink-0 cursor-pointer items-center gap-1 text-[11px] font-medium text-primary transition-colors hover:text-primary/80"
          >
            查看全部积分包
            <ArrowRight className="size-3.5" />
          </button>
        ) : null}
      </div>

      {/* 前两个包 */}
      <div className="mt-2.5 space-y-2.5">
        {shown.length === 0 ? (
          <p className="text-xs text-muted-foreground">暂无可用积分。</p>
        ) : (
          shown.map((p, i) => <PackageRow key={`${p.key}-${p.expireAt ?? "none"}-${i}`} p={p} />)
        )}
      </div>

      {/* 来源 / 回退提示（只在需要说明时出现） */}
      {item.notes && item.notes.length > 0 ? (
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          {item.notes.map((n) => (
            <Badge key={n} variant="outline" className="text-[10px]">
              {n}
            </Badge>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/** 全部积分包弹窗（两侧共用）。 */
export function CreditsDialog({
  item,
  name,
  onClose,
}: {
  item: CreditView | null;
  /** 账号名，进标题。 */
  name: string;
  onClose: () => void;
}) {
  const all = item ? activeOf(item) : [];
  return (
    <Dialog open={item !== null} onOpenChange={(o) => !o && onClose()}>
      <ResizableDialogContent storageKey="credits-packages">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Coins className="size-4" />
            全部积分包 · {name}
          </DialogTitle>
          <DialogDescription>
            共 {all.length} 个还有剩余的积分包（已用完的不列出），按到期时间升序；
            最后一个「长期有效」的可能排在末尾。
          </DialogDescription>
        </DialogHeader>
        <div className="min-h-0 divide-y divide-border/60 overflow-y-auto pr-1">
          {all.map((p, i) => (
            <PackageDetailRow key={`${p.key}-${p.expireAt ?? "none"}-${i}`} p={p} />
          ))}
        </div>
      </ResizableDialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// 顶部工具条（两侧共用）
// ---------------------------------------------------------------------------

/**
 * 积分工具条。刻意做得**很薄**：每张账号卡自己就带积分，
 * 这里只给出总数、更新时间与刷新入口，不重复摆一排数字。
 */
export function CreditsSummaryBar({
  title = "账号",
  count,
  summary,
  updatedAt,
  cached,
  hint,
  busy,
  onRefresh,
  refreshLabel = "刷新积分",
  refreshHint,
  error,
}: {
  title?: string;
  /** 卡片数量。 */
  count: number;
  /** 汇总（`totalRemaining` 允许缺省：个别后端没给这个字段）。 */
  summary?: {
    succeeded: number;
    queried: number;
    failed: number;
    totalRemaining?: number | null;
  } | null;
  updatedAt?: number | null;
  cached?: boolean;
  /** 副标题行（账号来源 / 额外说明）。 */
  hint?: React.ReactNode;
  busy: boolean;
  onRefresh: () => void;
  refreshLabel?: string;
  /** 刷新按钮的悬停说明（讲清这一次刷新顺带更新了什么）。 */
  refreshHint?: string;
  error?: string | null;
}) {
  return (
    <section className="space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <div className="flex min-w-0 items-center gap-2">
          <h2 className="text-base font-semibold">{title}</h2>
          <Badge variant="secondary" className="rounded-full px-2 tabular-nums">
            {count}
          </Badge>
          {summary ? (
            <span className="ml-1 text-xs text-muted-foreground">
              合计剩余{" "}
              <span className="font-medium tabular-nums text-foreground">
                {credits(summary.totalRemaining)}
              </span>
              {` · ${summary.succeeded}/${summary.queried} 个账号可查`}
              {summary.failed ? (
                <span className="text-amber-600">{`（${summary.failed} 个失败）`}</span>
              ) : null}
              {updatedAt ? ` · ${fmtUpdated(updatedAt)} 更新` : ""}
              {cached ? "（缓存）" : ""}
            </span>
          ) : (
            <span className="ml-1 text-xs text-muted-foreground">读取账号中…</span>
          )}
        </div>
        <Button
          variant="outline"
          size="sm"
          onClick={onRefresh}
          disabled={busy}
          title={refreshHint}
        >
          {busy ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
          {busy ? "查询中…" : refreshLabel}
        </Button>
      </div>

      {hint ? <p className="text-[11px] text-muted-foreground">{hint}</p> : null}

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>积分模块不可用</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}
    </section>
  );
}
