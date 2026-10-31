import { describe, expect, test } from "vitest";

import { groupSizes, isOlderDup, newestIdByGroup } from "./wb-dup";

const s = (id: string, dupGroup: string, updatedAt: number) => ({ id, dupGroup, updatedAt });

describe("同标题重复组：按本账号挑代表", () => {
  test("组内取 updatedAt 最大的一条，且每组恰好留一条", () => {
    const rows = [
      s("a", "每日 ai 新闻推送", 100),
      s("b", "每日 ai 新闻推送", 300),
      s("c", "每日 ai 新闻推送", 200),
      s("x", "", 999), // 标题唯一，不参与分组
    ];
    const newest = newestIdByGroup(rows);
    expect(newest.get("每日 ai 新闻推送")).toBe("b");
    expect(rows.filter((r) => r.dupGroup && !isOlderDup(r, newest))).toHaveLength(1);
    expect(isOlderDup(rows[3], newest)).toBe(false);
  });

  test("后端全局 isNewest 全为 false 时，仍然每组留一条（不然整组会被折叠掉）", () => {
    // 真机场景：账号 e925618e / fbd4139a 的这一组，跨账号的「最新」在别的号里。
    const rows = [s("p", "每日 ai 新闻推送", 500), s("q", "每日 ai 新闻推送", 400)];
    const newest = newestIdByGroup(rows);
    const visible = rows.filter((r) => !isOlderDup(r, newest));
    expect(visible).toHaveLength(1);
    expect(visible[0].id).toBe("p");
  });

  test("时间相同取先出现的，不抖动", () => {
    const rows = [s("first", "t", 1000), s("second", "t", 1000)];
    expect(newestIdByGroup(rows).get("t")).toBe("first");
  });

  test("组大小按全量算，不受筛选影响", () => {
    const rows = [s("a", "t", 3), s("b", "t", 2), s("c", "t", 1), s("d", "", 9)];
    const sizes = groupSizes(rows);
    expect(sizes.get("t")).toBe(3);
    expect(sizes.has("")).toBe(false);
  });
});
