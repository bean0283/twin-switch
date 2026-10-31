/**
 * 「同标题重复组」的显示口径 —— 只回答一个问题：**这个账号里每组该显示哪一条**。
 *
 * ⚠️ 为什么不能直接用后端的 `isNewest`：它是**跨账号的全局标记**。同一段对话在多账号各有一份，
 *    全局只有一份「最新」，于是某个账号里那一组可能一条都不是最新（真机实测：
 *    「每日 AI 新闻推送」在账号 e925618e / fbd4139a 里 2～6 条，`isNewest` 全为 false）。
 *    拿它折叠会把整组折叠掉 —— 列表一条不剩，用户看到的是「数据没了」。
 *
 * 所以这里按**本账号**的 `updatedAt` 取最大当代表，保证每组至少留一条；
 * 后端那个全局标记继续用于「与最新副本内容是否相同」的说明文案，不参与显隐。
 */

export interface DupCandidate {
  id: string;
  /** 后端给的重复组键（空串 = 标题唯一，不参与分组）。 */
  dupGroup: string;
  updatedAt: number;
}

/** 每个重复组里本账号内最新的一条 id（组键 → 会话 id）。 */
export function newestIdByGroup(items: readonly DupCandidate[]): Map<string, string> {
  const idOf = new Map<string, string>();
  const atOf = new Map<string, number>();
  for (const s of items) {
    if (!s.dupGroup) continue;
    const prev = atOf.get(s.dupGroup);
    // 时间相同时保留先出现的（列表本身已按更新时间倒序，先出现的更靠前）。
    if (prev === undefined || (s.updatedAt || 0) > prev) {
      atOf.set(s.dupGroup, s.updatedAt || 0);
      idOf.set(s.dupGroup, s.id);
    }
  }
  return idOf;
}

/** 组内条目数（组键 → 条数）：只有 > 1 的才算「重复组」。 */
export function groupSizes(items: readonly DupCandidate[]): Map<string, number> {
  const size = new Map<string, number>();
  for (const s of items) {
    if (s.dupGroup) size.set(s.dupGroup, (size.get(s.dupGroup) ?? 0) + 1);
  }
  return size;
}

/** 该条是否「同组较旧副本」（本账号口径）。无重复组时恒为 false。 */
export function isOlderDup(
  s: DupCandidate,
  newest: Map<string, string>,
): boolean {
  if (!s.dupGroup) return false;
  return newest.get(s.dupGroup) !== s.id;
}
