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
  const packs: CreditPack[] = (e.packs ?? []).map((p) => ({
    key: p.key,
    name: p.group ? `${p.name} · ${p.group}` : p.name,
    total: p.unlimited ? null : p.total,
    used: p.used,
    remaining: p.unlimited ? null : p.remaining,
    expireAt: p.expire_at,
  }));

  // 没有网页凭证的账号（切换载体来的）**根本查不了**积分 —— 这是「不适用」，
  // 不是「出错了」，所以走中性说明而不是红色报错。
  if (!e.queryable) {
    return {
      ok: true,
      unavailable: "该账号是切换载体，没有网页凭证，无法查询积分。用「发起网页登录」添加的账号才有额度明细。",
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
}: {
  entry: TraeCreditEntry | null;
  loading?: boolean;
  onExpand: () => void;
}) {
  return (
    <CreditBlockView
      item={entry ? traeEntryToView(entry) : null}
      loading={loading}
      onExpand={onExpand}
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

  return { result, error, busy, refresh, expand, setExpand };
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
          只有用「发起网页登录」添加的账号才有网页凭证，才能查积分。
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
