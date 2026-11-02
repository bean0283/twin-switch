import type { WbSessionGroup, WorkbuddySwitchResult } from "@/lib/trae-types";

/**
 * 切换后「会话同步」那一行的文案（纯函数，便于单测）。
 *
 * ## 三类结果必须分开说
 *
 * | 情况 | 含义 | 该怎么说 |
 * | --- | --- | --- |
 * | `refreshSame` | 两边**本来就一致** | 不是问题；全都没别的事时才说「N 份已是最新」 |
 * | `diverged` | 两边都有新内容，等用户裁决 | 「N 份有分歧（待你确认）」 |
 * | `refreshSkipped` | 真没同步成（正文缺失/读失败） | 「N 份未同步（正文缺失或读取失败）」 |
 *
 * ⚠️ 2026-11-02 用户报「明明没有分歧」：后端把「已一致」也塞进了 `refreshSkipped`，
 * 这里又统一读成「有分歧或缺正文」。两侧都已改，这个函数锁住前端这一侧。
 */
export function sessionLinkNote(
  link: NonNullable<WorkbuddySwitchResult["sessionLink"]>,
): string {
  if (link.ok === false) return `会话未同步：${link.error ?? "未知原因"}`;
  // 首次只登记、不搬历史（历史存量要人工确认后才搬，见会话页）。
  if (link.firstRun)
    return "会话自动同步已启用（首次只登记已有副本；历史会话请在「会话记录 → 复制与关联」点一键互通补齐）";
  // 没开开关 / 只有一个账号 / 已全部同步 —— 都不值得占一行。
  if (link.notNeeded) return "";

  const parts: string[] = [];
  const copied = link.copied ?? 0;
  const adopted = link.adopted ?? 0;
  const refreshed = link.refreshed ?? 0;
  const same = link.refreshSame ?? 0;
  const diverged = link.diverged?.length ?? 0;
  const bad = link.refreshSkipped?.length ?? 0;

  if (copied > 0) parts.push(`新增 ${copied} 个副本`);
  if (refreshed > 0) parts.push(`同步 ${refreshed} 份内容`);
  if (adopted > 0) parts.push(`登记 ${adopted} 组关联`);
  if (diverged > 0) parts.push(`${diverged} 份有分歧（待你确认）`);
  if (bad > 0) parts.push(`${bad} 份未同步（正文缺失或读取失败）`);
  // 「已一致」是兜底信息：有别的事要报时就不占字数（否则每行都挂一句「N 份已是最新」）。
  if (parts.length === 0 && same > 0) parts.push(`${same} 份已是最新`);

  return parts.length ? `会话${parts.join("、")}` : "";
}

/**
 * 分歧裁决弹窗里，某一组默认勾哪份副本。
 *
 * 优先级：
 * 1. `isNewest`（内容最新 —— 可能是**第三个**账号，不只是源/目标两方）
 * 2. 后端给的 `sourceSessionId`
 * 3. 第一个「有正文」的成员
 *
 * 都没有就返回空串 ⇒ 这组没法裁决（界面会把所有选项置灰）。
 * ⚠️ 不要退回「第一个成员」：那可能是已删除或没有正文的副本，
 * 拿它当标准会把别的好副本覆盖坏。
 */
export function pickDefaultStandard(group: WbSessionGroup): string {
  const has = (id?: string) => !!id && group.members.some((m) => m.sessionId === id);

  // 1) 后端给的「内容最新」。后端在**切换时已强制比对过**，所以这通常就是权威答案。
  if (has(group.newestSessionId)) return group.newestSessionId!;

  // 2) 组里只要有一份「上次比对之后又写了新内容」（`stale`），旧的「谁最新」结论就作废 ——
  //    那时按修改时间判，否则会一直推荐那份被超过的（用户 2026-11-02 实测）。
  const hasStale = group.members.some((m) => m.relation === "stale");
  if (!hasStale) {
    const newest = group.members.find((m) => m.isNewest && m.sessionId);
    if (newest?.sessionId) return newest.sessionId;
  }

  // 3) **按修改时间**：有 stale 时这是唯一可信信号；没有时也常是对的（最后用过的那份）。
  const byTime = group.members
    .filter((m) => m.sessionId && m.contentState === "ready")
    .reduce<typeof group.members[number] | null>(
      (best, m) => (!best || m.updatedAt > best.updatedAt ? m : best),
      null,
    );
  if (hasStale && byTime?.sessionId) return byTime.sessionId;

  // 4) 后端给的 sourceSessionId（它自己也是按「关系 + 更新时间」算的）
  const bySource = group.members.find(
    (m) => m.sessionId && m.sessionId === group.sourceSessionId,
  );
  if (bySource?.sessionId) return bySource.sessionId;

  if (byTime?.sessionId) return byTime.sessionId;

  const firstReady = group.members.find((m) => m.sessionId && m.contentState === "ready");
  return firstReady?.sessionId ?? "";
}
