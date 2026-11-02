import { describe, expect, it } from "vitest";

import { pickDefaultStandard, sessionLinkNote } from "./wb-switch-note";
import type { WorkbuddySwitchResult } from "./trae-types";

type Link = NonNullable<WorkbuddySwitchResult["sessionLink"]>;

/**
 * 2026-11-02 用户报：「明明没有分歧，切换的时候自动同步也不行」。
 *
 * 根因是后端把「两边已经一致」也塞进了 `refreshSkipped`，而前端把
 * `refreshSkipped` 一律读成「有分歧或缺正文」。两侧都改了，这里锁住前端。
 */
describe("sessionLinkNote", () => {
  it("已一致时不提分歧（回归：这就是用户看到的误报）", () => {
    const link: Link = { ok: true, refreshSame: 3 };
    expect(sessionLinkNote(link)).toBe("会话3 份已是最新");
    expect(sessionLinkNote(link)).not.toContain("分歧");
    expect(sessionLinkNote(link)).not.toContain("未同步");
  });

  it("分歧单独说，并且说清是在等用户确认", () => {
    const link: Link = {
      ok: true,
      copied: 1,
      diverged: [
        { id: "a", targetSessionId: "b", reason: "两边都有新内容（分歧），未自动覆盖" },
      ],
    };
    const note = sessionLinkNote(link);
    expect(note).toContain("新增 1 个副本");
    expect(note).toContain("1 份有分歧（待你确认）");
    // 「分歧」不能被说成「未同步」——前者等用户裁决，后者是工具没做成。
    expect(note).not.toContain("未同步");
  });

  it("真失败说成未同步，并点明原因类别", () => {
    const link: Link = {
      ok: true,
      refreshSkipped: [{ id: "a", targetSessionId: "b", reason: "目标副本正文不在磁盘上" }],
    };
    expect(sessionLinkNote(link)).toContain("1 份未同步（正文缺失或读取失败）");
  });

  it("有实际问题时不拿「已一致」占字数", () => {
    const link: Link = { ok: true, refreshSame: 5, refreshSkipped: [{ id: "a", reason: "x" }] };
    const note = sessionLinkNote(link);
    expect(note).toContain("未同步");
    expect(note).not.toContain("已是最新");
  });

  it("ok:false 必须照实说会话没同步（不许吞成成功）", () => {
    expect(sessionLinkNote({ ok: false, error: "库被占用" })).toBe("会话未同步：库被占用");
  });

  it("notNeeded 不占位", () => {
    expect(sessionLinkNote({ ok: true, notNeeded: true, reason: "只有一个账号" })).toBe("");
  });

  it("首次运行给的是可执行的下一步", () => {
    expect(sessionLinkNote({ ok: true, firstRun: true })).toContain("复制与关联");
  });

  it("什么都没发生时返回空串（不硬凑一行字）", () => {
    expect(sessionLinkNote({ ok: true })).toBe("");
  });
});

/**
 * 默认选哪份作为标准。
 *
 * 用户 2026-11-02 反馈的第二条：「不能仅判断这两个账号，有可能有多个关联，
 * 最新版出现在第三个账号内」——所以这里必须认 `isNewest`，而不是「源/目标」。
 */
describe("pickDefaultStandard", () => {
  const mk = (over: Partial<Parameters<typeof pickDefaultStandard>[0]["members"][number]>) => ({
    uid: "u",
    accountName: "a",
    sessionId: "s",
    title: "t",
    project: "p",
    updatedAt: 0,
    recordCount: null,
    contentState: "ready" as const,
    relation: "",
    isNewest: false,
    digest: "",
    reason: "",
    ...over,
  });

  const group = (members: ReturnType<typeof mk>[], sourceSessionId = "") =>
    ({
      key: "link:g",
      groupId: "g",
      title: "会话",
      project: "p",
      updatedAt: 0,
      memberCount: members.length,
      accountNames: members.map((m) => m.accountName),
      status: "diverged",
      statusText: "",
      statusReason: "",
      behindCount: 0,
      staleCount: 0,
      sourceSessionId,
      canUnify: true,
      missingAccounts: [],
      missingCount: 0,
      members,
    }) as Parameters<typeof pickDefaultStandard>[0];

  it("选 isNewest，哪怕它在第三个账号上", () => {
    const g = group([
      mk({ uid: "a", sessionId: "sa", accountName: "A" }),
      mk({ uid: "b", sessionId: "sb", accountName: "B" }),
      mk({ uid: "c", sessionId: "sc", accountName: "C", isNewest: true }),
    ]);
    expect(pickDefaultStandard(g)).toBe("sc");
  });

  it("没有 isNewest 时退到 sourceSessionId", () => {
    const g = group(
      [mk({ sessionId: "sa" }), mk({ sessionId: "sb" })],
      "sb",
    );
    expect(pickDefaultStandard(g)).toBe("sb");
  });

  it("都没有时取第一个有正文的，绝不取已失效/缺正文的", () => {
    const g = group([
      mk({ sessionId: "gone", contentState: "gone" }),
      mk({ sessionId: "missing", contentState: "missing" }),
      mk({ sessionId: "ok", contentState: "ready" }),
    ]);
    expect(pickDefaultStandard(g)).toBe("ok");
  });

  /**
   * 回归：2026-11-02 用户截图 —— 22:19 才写过的那份（`stale`，未重新比对）
   * 被排到最后、默认没选上，而它明显是最新的。
   */
  it("没有比对结论时按修改时间选最新，而不是选 isNewest 那个旧的", () => {
    const g = group([
      mk({ sessionId: "a", updatedAt: 1793700000000, isNewest: false, relation: "equal" }),
      mk({ sessionId: "b", updatedAt: 1793700001000, isNewest: true, relation: "latest" }),
      // 最后用过的那份：时间最晚，但因为「未重新比对」不带任何 relation
      mk({ sessionId: "c", updatedAt: 1793705400000, relation: "stale" }),
    ]);
    // 后端没给 content 结论时，pick 走 mtime
    expect(pickDefaultStandard(g)).toBe("c");
  });

  it("后端给了 newestSessionId（强制比对后的结论）就以它为准", () => {
    const g = {
      ...group([
        mk({ sessionId: "a", updatedAt: 1793700000000 }),
        mk({ sessionId: "b", updatedAt: 1793705400000 }),
      ]),
      newestSessionId: "a",
      newestBasis: "content" as const,
    };
    // 内容比对说 a 最新，即使 b 的修改时间更晚 —— 权威结论优先
    expect(pickDefaultStandard(g)).toBe("a");
  });

  it("整组都没有可用正文时返回空串（界面据此置灰，而不是乱选一个）", () => {
    const g = group([
      mk({ sessionId: "", contentState: "gone" }),
      mk({ sessionId: "m", contentState: "missing" }),
    ]);
    expect(pickDefaultStandard(g)).toBe("");
  });
});
