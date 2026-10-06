import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  CreditBlock as CreditBlockView,
  CreditsDialog as CreditsDialogView,
  CreditsSummaryBar,
  credits,
  type CreditPack,
  type CreditView,
} from "@/components/credits-ui";
import { creditResourceName } from "@/lib/credit-package-names";
import * as api from "@/lib/api";
import type { WbCreditItem, WbCreditsResult } from "@/lib/trae-types";

// ---------------------------------------------------------------------------
// 适配：WorkBuddy 的接口数据 → 共用展示模型
// ---------------------------------------------------------------------------

/**
 * 把一条 WorkBuddy 积分结果转成 [`CreditView`]。
 *
 * 展示层（账号卡上的积分块 / 全部积分包弹窗）由 `credits-ui` 与 Trae 侧共用，
 * 两侧的差别只在这一个函数里。
 */
export function wbItemToView(it: WbCreditItem): CreditView {
  // 后端已经把「还有剩余、按到期升序」算成 activeResources；没有才回落到全量 resources。
  const src = it.activeResources ?? it.resources ?? [];
  const packs: CreditPack[] = src.map((r) => ({
    key: r.packageCode ?? `${r.packageName ?? "pack"}-${r.expireAt ?? "none"}`,
    name: creditResourceName(r),
    total: r.total,
    used: r.used,
    remaining: r.remaining,
    expireAt: r.expireAt,
    expired: r.expired,
    expiringSoon: r.expiringSoon,
  }));

  const notes: string[] = [];
  if (it.account.origin === "ref") notes.push("积分来自参考工具库");
  if (it.source === "legacy") notes.push("旧接口回退（三路新接口不可用），时间字段可能不全");

  return {
    ok: it.ok,
    error: it.error ?? it.account.blockedReason ?? null,
    remaining: it.totalRemaining ?? null,
    total: it.totalCapacity ?? null,
    packCount: it.activePackageCount ?? packs.filter((p) => (p.remaining ?? 0) > 0).length,
    packs,
    updatedAt: it.updatedAt ?? null,
    expiringSoonRemaining: it.expiringSoonRemaining ?? null,
    notes,
  };
}

// ---------------------------------------------------------------------------
// 状态 hook
// ---------------------------------------------------------------------------

export interface CreditsState {
  meta: { count: number; queryable: number; referenceStore: string } | null;
  result: WbCreditsResult | null;
  error: string | null;
  busy: boolean;
  refresh: () => Promise<void>;
  expand: WbCreditItem | null;
  setExpand: (v: WbCreditItem | null) => void;
}

/**
 * 积分数据状态：首屏只读缓存（不发请求，含上次运行落盘的磁盘缓存），
 * 进页面后自动查一次（未命中 5 分钟缓存才真的联网）。
 *
 * 账号来源 = 本工具账号库（可写）+ **只读**借用的参考工具账号库。
 */
export function useWorkbuddyCredits(): CreditsState {
  const [result, setResult] = useState<WbCreditsResult | null>(null);
  const [meta, setMeta] = useState<{ count: number; queryable: number; referenceStore: string } | null>(
    null,
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expand, setExpand] = useState<WbCreditItem | null>(null);

  const loadMeta = useCallback(async () => {
    try {
      const m = await api.workbuddyCreditsAccounts();
      setMeta({ count: m.count, queryable: m.queryable, referenceStore: m.referenceStore });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    // 先上缓存（毫秒级、不联网），再后台查一次 —— 界面从一开始就有数字。
    void loadMeta();
    void (async () => {
      try {
        const cached = await api.workbuddyCreditsCached();
        if (!cached.empty) setResult(cached);
        const fresh = await api.workbuddyCreditsQuery(false);
        setResult(fresh);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    })();
  }, [loadMeta]);

  const refresh = useCallback(async () => {
    setBusy(true);
    try {
      const res = await api.workbuddyCreditsQuery(true);
      setResult(res);
      toast.success(`积分已更新 ${res.summary.succeeded}/${res.summary.queried}`, {
        description: `合计剩余 ${credits(res.summary.totalRemaining)}`,
      });
    } catch (e) {
      toast.error("积分查询失败", { description: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  }, []);

  return { meta, result, error, busy, refresh, expand, setExpand };
}

// ---------------------------------------------------------------------------
// 顶部工具条：标题 + 账号数 + 合计 + 刷新
// ---------------------------------------------------------------------------

/**
 * 积分工具条。刻意做得**很薄**（参考实现的顶部条）：
 * 每张账号卡自己就带积分，这里只需要给出总数与刷新入口，不重复摆一排数字。
 */
export function CreditsHeader({
  credits: c,
  accountCount,
}: {
  credits: CreditsState;
  /** 本工具账号库里的账号数（卡片数量），与本模块的 `meta.count` 可能不同。 */
  accountCount: number;
}) {
  const { meta, result, error, busy, refresh } = c;
  const loaded = result && !result.empty ? result : null;
  return (
    <CreditsSummaryBar
      count={accountCount}
      summary={loaded ? loaded.summary : meta ? { succeeded: 0, queried: meta.count, failed: 0, totalRemaining: 0 } : null}
      updatedAt={loaded?.updatedAt ?? null}
      cached={loaded?.cached}
      hint={
        meta ? (
          <>
            账号来源：本工具账号库 + 只读借用{" "}
            <code className="rounded bg-muted px-1 break-all">{meta.referenceStore}</code>
          </>
        ) : null
      }
      busy={busy}
      onRefresh={() => void refresh()}
      refreshHint="逐个账号拉取积分包明细；刚查过的账号会直接复用缓存。"
      error={error}
    />
  );
}

// ---------------------------------------------------------------------------
// 单个账号的积分块：嵌进账号卡（卡片右侧放切换 / 改名 / 删除按钮）
// ---------------------------------------------------------------------------

export function CreditBlock({
  item,
  error,
  loading,
  onExpand,
}: {
  item: WbCreditItem | null;
  /** 该账号的查询失败原因（来自 result.errors）。 */
  error?: string | null;
  loading?: boolean;
  onExpand: () => void;
}) {
  const view = useMemo<CreditView | null>(() => {
    if (error) {
      return { ok: false, error, remaining: null, packCount: 0, packs: [] };
    }
    return item ? wbItemToView(item) : null;
  }, [item, error]);

  return <CreditBlockView item={view} loading={loading} onExpand={onExpand} />;
}

// ---------------------------------------------------------------------------
// 全部积分包弹窗
// ---------------------------------------------------------------------------

export function CreditsDialog({
  item,
  onClose,
}: {
  item: WbCreditItem | null;
  onClose: () => void;
}) {
  const view = useMemo(() => (item ? wbItemToView(item) : null), [item]);
  return <CreditsDialogView item={view} name={item?.account.name ?? ""} onClose={onClose} />;
}
