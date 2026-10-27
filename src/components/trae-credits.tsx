import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";

import {
  CreditBlock as CreditBlockView,
  CreditsDialog as CreditsDialogView,
  CreditsSummaryBar,
  credits,
  type CreditPack,
  type CreditView,
} from "@/components/credits-ui";
import * as api from "@/lib/api";
import type { TraeCreditEntry, TraeCreditsResult } from "@/lib/trae-types";

/**
 * Trae 侧的积分展示：与 WorkBuddy 用**同一套** `credits-ui` 组件，
 * 差别只在这个适配函数里。
 */
export function traeEntryToView(e: TraeCreditEntry): CreditView {
  // 后端的积分包判据是「带 `credits_limit`」，所以这里的 `total` / `remaining`
  // 直接就是数字 —— 不再有「不限量」这一说（那行假数据见 `trae-profile.rs` 的注释）。
  const packs: CreditPack[] = (e.packs ?? []).map((p) => ({
    key: p.key,
    name: p.group ? `${p.name} · ${p.group}` : p.name,
    total: p.total,
    used: p.used,
    remaining: p.remaining,
    expireAt: p.expire_at,
  }));

  // 没有网页凭证的账号（切换载体来的）**根本查不了**积分 —— 这是「不适用」，
  // 不是「出错了」，所以走中性说明而不是红色报错。
  if (!e.queryable) {
    return {
      ok: true,
      unavailable: "该账号是切换载体，没有网页凭证，无法查询积分。用「扫码添加账号」添加的账号才有额度明细。",
      remaining: null,
      packCount: 0,
      packs: [],
    };
  }

  // 「N 个积分包」说的是**还有得用**的包数：Trae 接口会把历次签到包全列出来
  // （实测两个账号各 30+ 个，绝大多数已经用完），直接报总数会显得很唬人。
  const now = Date.now();
  const activeCount = packs.filter((p) => {
    const expired = p.expireAt != null && p.expireAt <= now;
    return !expired && (p.remaining == null || p.remaining > 0);
  }).length;

  return {
    ok: e.ok,
    error: e.error,
    remaining: e.remaining,
    total: e.total,
    used: e.used,
    packCount: activeCount,
    packs,
    updatedAt: e.updatedAt,
  };
}

/** 单账号积分块（嵌进账号卡）。 */
export function TraeCreditBlock({
  entry,
  loading,
  onExpand,
  onRefresh,
  refreshing,
}: {
  entry: TraeCreditEntry | null;
  loading?: boolean;
  onExpand: () => void;
  /** 只刷新这个账号；不传就不画按钮。 */
  onRefresh?: () => void;
  refreshing?: boolean;
}) {
  return (
    <CreditBlockView
      item={entry ? traeEntryToView(entry) : null}
      loading={loading}
      onExpand={onExpand}
      onRefresh={onRefresh}
      refreshing={refreshing}
      refreshDisabled={entry ? !entry.queryable : false}
      refreshHint={
        entry && !entry.queryable
          ? "该账号是切换载体，没有网页凭证，无法查询积分"
          : "重新查询这个账号的积分"
      }
    />
  );
}

/** 全部积分包弹窗。 */
export function TraeCreditsDialog({
  entry,
  onClose,
}: {
  entry: TraeCreditEntry | null;
  onClose: () => void;
}) {
  return (
    <CreditsDialogView
      item={entry ? traeEntryToView(entry) : null}
      name={entry?.name ?? ""}
      onClose={onClose}
    />
  );
}

export interface TraeCreditsState {
  result: TraeCreditsResult | null;
  error: string | null;
  busy: boolean;
  refresh: () => Promise<void>;
  /**
   * 只刷新一个账号（卡片上的小刷新按钮）。
   *
   * 成功后把**这一条**替换进 `result.accounts` 并重算 `summary` —— 不整批重拉，
   * 所以点了哪个号就只等那一个号的接口。失败只提示，不动其它账号的数字。
   */
  refreshOne: (id: string) => Promise<void>;
  /** 正在单独刷新的账号 id（该卡片的按钮转圈）。 */
  refreshingId: string | null;
  expand: TraeCreditEntry | null;
  setExpand: (v: TraeCreditEntry | null) => void;
}

/**
 * 账号库积分状态：首屏读离线快照（各账号的 profile.json，毫秒级），
 * 随后自动查一次（未命中 5 分钟缓存才会真的打接口）。
 *
 * 换客户端 / 账号库变化时传新的 `clientKey` 会重新拉一遍。
 */
export function useTraeCredits(clientKey: string | null): TraeCreditsState {
  const [result, setResult] = useState<TraeCreditsResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** 正在单独刷新的账号（同一时刻只允许一个，避免连点打一串接口）。 */
  const [refreshingId, setRefreshingId] = useState<string | null>(null);
  const [expand, setExpand] = useState<TraeCreditEntry | null>(null);

  useEffect(() => {
    let alive = true;
    setResult(null);
    setError(null);
    void (async () => {
      try {
        const cached = await api.traeCreditsCached(clientKey);
        if (!alive) return;
        setResult(cached);
        const fresh = await api.traeCreditsQuery(clientKey, false);
        if (!alive) return;
        setResult(fresh);
      } catch (e) {
        if (alive) setError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [clientKey]);

  const refresh = useCallback(async () => {
    setBusy(true);
    try {
      const res = await api.traeCreditsQuery(clientKey, true);
      setResult(res);
      toast.success(`积分已更新 ${res.summary.succeeded}/${res.summary.queried}`, {
        description: `合计剩余 ${credits(res.summary.totalRemaining)}`,
      });
    } catch (e) {
      toast.error("积分查询失败", { description: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }, [clientKey]);

  /**
   * 单账号刷新。
   *
   * ⚠️ 与 `refresh`（整批）**不是同一条链路**：整批会写整份磁盘缓存并重拉所有账号；
   * 这里只打这一个号，然后把结果**原地替换**进现有 result。这样：
   * ① 只等一个请求；② 其它账号的数字与时间戳不会被无关地改动。
   */
  const refreshOne = useCallback(
    async (id: string) => {
      if (refreshingId) return; // 已有一个在刷（连点保护）
      setRefreshingId(id);
      try {
        const one = (await api.traeCreditsOne(clientKey, id)) as TraeCreditEntry;
        if (one && one.ok === false && one.error) {
          // 失败：保留旧数字，只把原因说出来（不能悄悄不动，用户会以为刷新没生效）
          toast.error("这个账号的积分没刷到", { description: one.error });
          return;
        }
        setResult((prev) => {
          if (!prev) return prev;
          const accounts = prev.accounts.map((x) => (x.id === id ? one : x));
          const succeeded = accounts.filter((x) => x.ok).length;
          const totalRemaining = accounts.reduce(
            (n, x) => n + (x.ok ? Number(x.remaining ?? 0) : 0),
            0,
          );
          return {
            ...prev,
            accounts,
            // 与后端 `summarize()` 同口径：只统计成功的账号。
            summary: {
              queried: accounts.length,
              succeeded,
              failed: accounts.length - succeeded,
              totalRemaining,
            },
            updatedAt: Date.now(),
            cached: false,
          };
        });
      } catch (e) {
        toast.error("积分查询失败", { description: e instanceof Error ? e.message : String(e) });
      } finally {
        setRefreshingId(null);
      }
    },
    [clientKey, refreshingId],
  );

  return { result, error, busy, refresh, refreshOne, refreshingId, expand, setExpand };
}

/** 顶部工具条（与 WorkBuddy 侧同一套）。 */
export function TraeCreditsHeader({
  credits: c,
  accountCount,
  clientLabel,
}: {
  credits: TraeCreditsState;
  accountCount: number;
  clientLabel?: string | null;
}) {
  const { result, error, busy, refresh } = c;
  return (
    <CreditsSummaryBar
      count={accountCount}
      summary={result ? result.summary : null}
      updatedAt={result?.updatedAt ?? null}
      cached={result?.cached}
      hint={
        <>
          积分包来自 Trae 官方额度接口{clientLabel ? ` · ${clientLabel}` : ""}；
          只有用「扫码添加账号」添加的账号才有网页凭证，才能查积分。
        </>
      }
      busy={busy}
      onRefresh={() => void refresh()}
      refreshLabel="刷新积分"
      refreshHint="逐个账号拉取真实昵称 / 头像 / 手机号与积分包明细；5 分钟内重复点会直接复用缓存。"
      error={error}
    />
  );
}
