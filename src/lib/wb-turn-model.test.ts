import { describe, expect, test } from "vitest";

import {
  applyText,
  countKind,
  editCount,
  emptyEdit,
  hasEdits,
  kindOfJson,
  lineText,
  newAssistantLine,
  newLineId,
  newUserLine,
  oneLine,
  opsFromEdit,
  ranges,
  readPath,
  replaceUserQuery,
  usersAfterEdit,
  usersAfterTurnRemoved,
  usersAfterTurnsRemoved,
  writePath,
} from "@/lib/wb-turn-model";
import type { WbLineKind } from "@/lib/trae-types";

describe("replaceUserQuery：护住 <user_query> 壳", () => {
  test("只换内层，前面的 system-reminder 块一字不动", () => {
    const raw =
      '<system-reminder data-role="user-context">\n<user_info>\nOS Version: win32\n</user_info>\n</system-reminder>\n<user_query>原来的问题</user_query>';
    const out = replaceUserQuery(raw, "新的问题");
    expect(out).toContain("<system-reminder");
    expect(out).toContain("OS Version: win32");
    expect(out).toContain("<user_query>新的问题</user_query>");
    expect(out).not.toContain("原来的问题");
  });

  test("后面还挂着附加数据块时不会被吃掉", () => {
    const raw = "<user_query>问题</user_query>\n<tail>尾巴</tail>";
    expect(replaceUserQuery(raw, "改")).toBe("<user_query>改</user_query>\n<tail>尾巴</tail>");
  });

  test("没有壳时退回整段替换（不丢用户输入）", () => {
    expect(replaceUserQuery("裸文本", "改")).toBe("改");
    expect(replaceUserQuery("<user_query>只有开标签", "改")).toBe("改");
  });

  test("取的是第一个 <user_query>（与后端 extract_user_text 同口径）", () => {
    const raw = "<user_query>第一</user_query> 中间 <user_query>第二</user_query>";
    expect(replaceUserQuery(raw, "X")).toBe("<user_query>X</user_query> 中间 <user_query>第二</user_query>");
  });
});

describe("readPath / writePath", () => {
  const line = { content: [{ type: "output_text", text: "原回答" }] };

  test("按路径读写数组下标", () => {
    expect(readPath(line, ["content", 0, "text"])).toBe("原回答");
    const next = writePath(line, ["content", 0, "text"], "新回答") as typeof line;
    expect(next.content[0].text).toBe("新回答");
  });

  test("写的是副本，原对象不动", () => {
    const next = writePath(line, ["content", 0, "text"], "新回答");
    expect(line.content[0].text).toBe("原回答");
    expect(next).not.toBe(line);
  });

  test("兄弟节点保持同一引用（不做整份深拷）", () => {
    const big = { other: { huge: true }, content: [{ text: "a" }] };
    const next = writePath(big, ["content", 0, "text"], "b") as typeof big;
    expect(next.other).toBe(big.other);
  });

  test("路径与实际结构不符时不硬塞", () => {
    expect(writePath({ a: 1 }, ["a", "b", "c"], 2)).toEqual({ a: 1 });
    expect(readPath({ a: 1 }, ["a", "b"])).toBeUndefined();
    expect(readPath({ a: 1 }, null)).toBeUndefined();
  });
});

describe("applyText / lineText", () => {
  test("用户提问：自动补回壳", () => {
    const json = {
      type: "message",
      role: "user",
      content: [{ type: "input_text", text: "<user_query>旧</user_query>" }],
    };
    const next = applyText(json, ["content", 0, "text"], "新", true);
    expect(readPath(next, ["content", 0, "text"])).toBe("<user_query>新</user_query>");
  });

  test("助手回答：直接替换", () => {
    const json = { content: [{ type: "output_text", text: "旧" }] };
    const next = applyText(json, ["content", 0, "text"], "新", false);
    expect(readPath(next, ["content", 0, "text"])).toBe("新");
  });

  test("lineText 在路径取不到时退回 fallback", () => {
    expect(lineText({}, ["content", 0, "text"], "兜底")).toBe("兜底");
    expect(lineText({ content: [{ text: "真" }] }, ["content", 0, "text"], "兜底")).toBe("真");
  });
});

describe("opsFromEdit：只回传改动过的行", () => {
  test("空编辑不产出任何 op", () => {
    const e = emptyEdit();
    expect(hasEdits(e)).toBe(false);
    expect(editCount(e)).toBe(0);
    expect(opsFromEdit(e, 10)).toEqual([]);
  });

  test("replace / delete / insert 各自成形，顺序稳定", () => {
    const e = emptyEdit();
    e.deleted.add(3);
    e.replaced.set(1, { type: "message", n: 2 });
    e.inserted.push({ key: "k1", at: 5, json: { type: "message", n: 9 } });
    const ops = opsFromEdit(e, 10);
    expect(ops).toEqual([
      { op: "delete", indexes: [3] },
      { op: "replace", index: 1, json: { type: "message", n: 2 } },
      { op: "insert", at: 5, json: { type: "message", n: 9 } },
    ]);
  });

  test("既删又改的行只报 delete（两条都报会被后端判「重复修改」整批拒写）", () => {
    const e = emptyEdit();
    e.deleted.add(2);
    e.replaced.set(2, { type: "message" });
    expect(opsFromEdit(e, 10)).toEqual([{ op: "delete", indexes: [2] }]);
  });

  test("新增行的 at 会被夹到 [0, lineCount]", () => {
    const e = emptyEdit();
    e.inserted.push({ key: "a", at: 999, json: { t: 1 } });
    e.inserted.push({ key: "b", at: -5, json: { t: 2 } });
    const ops = opsFromEdit(e, 10);
    expect(ops).toContainEqual({ op: "insert", at: 10, json: { t: 1 } });
    expect(ops).toContainEqual({ op: "insert", at: 0, json: { t: 2 } });
  });

  test("同一位置的多条新增保持提交顺序", () => {
    const e = emptyEdit();
    e.inserted.push({ key: "a", at: 4, json: { n: 1 } });
    e.inserted.push({ key: "b", at: 4, json: { n: 2 } });
    expect(opsFromEdit(e, 10)).toEqual([
      { op: "insert", at: 4, json: { n: 1 } },
      { op: "insert", at: 4, json: { n: 2 } },
    ]);
  });
});

describe("新增行的形状", () => {
  const now = 1791553991394;

  test("用户提问带 <user_query> 壳、sessionId、cwd", () => {
    const l = newUserLine("sid-1", "d:\\htw\\test", "你好", now);
    expect(l.type).toBe("message");
    expect(l.role).toBe("user");
    expect(l.sessionId).toBe("sid-1");
    expect(l.cwd).toBe("d:\\htw\\test");
    expect(readPath(l, ["content", 0, "text"])).toBe("<user_query>你好</user_query>");
    expect(readPath(l, ["content", 0, "type"])).toBe("input_text");
  });

  test("助手回答用 output_text + status=completed", () => {
    const l = newAssistantLine("sid-1", "d:\\htw\\test", "好的", now);
    expect(l.role).toBe("assistant");
    expect(l.status).toBe("completed");
    expect(readPath(l, ["content", 0, "type"])).toBe("output_text");
    expect(readPath(l, ["content", 0, "text"])).toBe("好的");
  });

  test("生成的 id 是 UUID 形态且互不相同", () => {
    const a = newLineId();
    const b = newLineId();
    expect(a).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(b).not.toBe(a);
  });
});

describe("小工具", () => {
  test("countKind 数种类", () => {
    expect(countKind(["user", "assistant", "user"], "user")).toBe(2);
    expect(countKind([], "user")).toBe(0);
  });
  test("oneLine 压平空白并截断", () => {
    expect(oneLine("  多行\n内容  ")).toBe("多行 内容");
    expect(oneLine("x".repeat(200), 10)).toBe(`${"x".repeat(10)}…`);
  });

  test("ranges 把散落的行号切成连续区间", () => {
    expect(ranges([1, 2, 3, 7, 9, 10])).toEqual([
      { start: 1, end: 4 },
      { start: 7, end: 8 },
      { start: 9, end: 11 },
    ]);
    expect(ranges([])).toEqual([]);
  });
});

describe("kindOfJson：本次新增的行算哪一类", () => {
  test("message + role 决定 user / assistant，其它一律 unknown", () => {
    expect(kindOfJson({ type: "message", role: "user" })).toBe("user");
    expect(kindOfJson({ type: "message", role: "assistant" })).toBe("assistant");
    expect(kindOfJson({ type: "reasoning" })).toBe("unknown");
    expect(kindOfJson({})).toBe("unknown");
  });
});

describe("usersAfterEdit / usersAfterTurnRemoved：空会话硬拦的同一口径", () => {
  /** 3 行：meta / user / assistant —— 一个只含一条用户提问的最小会话。 */
  const kinds: WbLineKind[] = ["meta", "user", "assistant"];

  test("没有改动时就是原始条数", () => {
    expect(usersAfterEdit(kinds, emptyEdit())).toBe(1);
  });

  test("删掉那条用户提问 ⇒ 0（后端会硬拦）", () => {
    const e = emptyEdit();
    e.deleted.add(1);
    expect(usersAfterEdit(kinds, e)).toBe(0);
  });

  test("替换那条用户提问 ⇒ 仍是 1（改写不是删除）", () => {
    const e = emptyEdit();
    e.replaced.set(1, { type: "message", role: "user" });
    expect(usersAfterEdit(kinds, e)).toBe(1);
  });

  test("新增一条用户提问 ⇒ 2", () => {
    const e = emptyEdit();
    e.inserted.push({ key: "n1", at: 3, json: { type: "message", role: "user" } });
    expect(usersAfterEdit(kinds, e)).toBe(2);
    // 新增助手回答不算
    const e2 = emptyEdit();
    e2.inserted.push({ key: "n2", at: 3, json: { type: "message", role: "assistant" } });
    expect(usersAfterEdit(kinds, e2)).toBe(1);
  });

  test("整回合删除：只含这一条提问的回合被删 ⇒ 0", () => {
    // 回合 = 第 1..2 行（user + assistant）
    expect(usersAfterTurnRemoved(kinds, emptyEdit(), { start: 1, end: 3 })).toBe(0);
  });

  test("❌ 刚新增一条提问又整回合删掉 ⇒ 不能算出 1（否则骗过硬拦）", () => {
    const e = emptyEdit();
    e.inserted.push({ key: "n1", at: 3, json: { type: "message", role: "user" } });
    expect(usersAfterEdit(kinds, e)).toBe(2);
    // 新增行 at=3 落在 [1,3] 区间里 ⇒ 会跟着回合一起被删
    expect(usersAfterTurnRemoved(kinds, e, { start: 1, end: 3 })).toBe(0);
  });

  test("别处的用户提问不受影响", () => {
    const k2: WbLineKind[] = ["user", "assistant", "user", "assistant"];
    // 删第 2..3 行那个回合 ⇒ 还剩第 0 行的 1 条
    expect(usersAfterTurnRemoved(k2, emptyEdit(), { start: 2, end: 4 })).toBe(1);
    // 删第 0..1 行那个回合 ⇒ 还剩第 2 行的 1 条
    expect(usersAfterTurnRemoved(k2, emptyEdit(), { start: 0, end: 2 })).toBe(1);
  });

  test("新增在别的回合里的用户提问不会被误扣", () => {
    const k2: WbLineKind[] = ["user", "assistant", "user", "assistant"];
    const e = emptyEdit();
    // at=4（末尾）不属于 [0,2)
    e.inserted.push({ key: "n1", at: 4, json: { type: "message", role: "user" } });
    expect(usersAfterTurnRemoved(k2, e, { start: 0, end: 2 })).toBe(2);
  });

  test("插在回合第一行之前的新增行属于「会话开头」，不跟着这个回合走", () => {
    const k2: WbLineKind[] = ["user", "assistant", "user", "assistant"];
    const e = emptyEdit();
    // at === turn.start：它是「会话开头」那一档加的行（`addLine` 插在 `range.to`）
    e.inserted.push({ key: "n1", at: 2, json: { type: "message", role: "user" } });
    // 删 [2,4) 只剩第 0 行那 1 条 + 留在 head 的这 1 条 ⇒ 2，而不是 1
    expect(usersAfterTurnRemoved(k2, e, { start: 2, end: 4 })).toBe(2);
  });
});

describe("usersAfterTurnsRemoved：批量删回合必须与单回合同一口径", () => {
  /** 4 行 = 两个回合：行 0..1 / 行 2..3。 */
  const k: WbLineKind[] = ["user", "assistant", "user", "assistant"];
  const t0 = { start: 0, end: 2 };
  const t1 = { start: 2, end: 4 };

  test("空数组 = 没勾任何回合 ⇒ 等价于 usersAfterEdit", () => {
    expect(usersAfterTurnsRemoved(k, emptyEdit(), [])).toBe(2);
    const e = emptyEdit();
    e.deleted.add(0);
    expect(usersAfterTurnsRemoved(k, e, [])).toBe(usersAfterEdit(k, e));
  });

  test("❌ 一次勾两个回合全删 ⇒ 0（逐个算都 ≥1，合起来才归零）", () => {
    // 这正是批量删除必须**按集合**算的原因：逐回合调用永远看不出「一起删光」，
    // 两个回合都放过 ⇒ 写出空壳会话 ⇒ 客户端永远卡在「环境创建中」。
    expect(usersAfterTurnRemoved(k, emptyEdit(), t0)).toBe(1);
    expect(usersAfterTurnRemoved(k, emptyEdit(), t1)).toBe(1);
    expect(usersAfterTurnsRemoved(k, emptyEdit(), [t0, t1])).toBe(0);
  });

  test("只勾一个 ⇒ 另一个还在", () => {
    expect(usersAfterTurnsRemoved(k, emptyEdit(), [t0])).toBe(1);
    expect(usersAfterTurnsRemoved(k, emptyEdit(), [t1])).toBe(1);
  });

  test("单回合是它的退化情形（两个函数必须同解，含已有删除）", () => {
    const e = emptyEdit();
    e.deleted.add(0);
    expect(usersAfterTurnsRemoved(k, e, [t1])).toBe(usersAfterTurnRemoved(k, e, t1));
  });

  test("新增行落在**任一**被删回合区间里都一起扣", () => {
    const e = emptyEdit();
    // at=4 ∈ (2,4] ⇒ 属于 t1；只删 t0 时必须留着
    e.inserted.push({ key: "n1", at: 4, json: { type: "message", role: "user" } });
    expect(usersAfterTurnsRemoved(k, e, [t0])).toBe(2);
    expect(usersAfterTurnsRemoved(k, e, [t0, t1])).toBe(0);
  });

  test("at === turn.start 属于「会话开头」，批量时同规则不跟着走", () => {
    const e = emptyEdit();
    e.inserted.push({ key: "n1", at: 2, json: { type: "message", role: "user" } });
    // t1 = [2,4)：at=2 是插在它第一行**之前** ⇒ 只删 t1 时它留下
    expect(usersAfterTurnsRemoved(k, e, [t1])).toBe(2);
  });
});
