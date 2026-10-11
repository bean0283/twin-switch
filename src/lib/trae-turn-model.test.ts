import { describe, expect, test } from "vitest";

import type { TraeEditDoc } from "@/lib/trae-types";
import {
  buildSessionView,
  batchReplaceText,
  batchSetColumn,
  appendAtPath,
  bodyOf,
  columnChoices,
  deleteRowRefs,
  duplicateRows,
  emptySegmentItem,
  identityValuesToRemap,
  isIdentityColumn,
  isJsonRef,
  messageRefs,
  parseByColumn,
  readAtPath,
  removeAtPath,
  segmentContainerPath,
  segmentsOf,
  setAtPath,
  setCell,
  tableOf,
  textOf,
  toolSummary,
  turnRefs,
  turnsLeftAfterRemoving,
  type RowRef,
} from "@/lib/trae-turn-model";

// ---------------------------------------------------------------------------
// 这一层是纯函数，所以能直接单测 —— 这正是把「回合派生 + 增删改」从组件里拆出来的原因：
// 这些规则一旦错了，界面看起来完全正常，但写进库里的会话是坏的。
// ---------------------------------------------------------------------------

/** 造一份最小但**结构真实**的 doc：1 个回合 + 2 条消息 + 正文表 + 一张会话级表。 */
function doc(opts: { orphanBody?: boolean } = {}): TraeEditDoc {
  const SID = "6a24f7e84c1b864afff8b578";
  const userMsg = "6a24fa175e4559ee81f5d25e";
  const asstMsg = "6a24fa174c1b864afff8b57a";
  return {
    clientKey: "trae-cn",
    clientLabel: "Trae CN",
    sessionId: SID,
    title: "测试会话",
    ownerUid: "925158341882023",
    counts: {},
    refCount: 0,
    warnings: [],
    tables: [
      {
        name: "chat_session",
        scope: "session",
        editable: true,
        derived: false,
        refCount: 0,
        columns: [
          { name: "session_id", type: "TEXT", notNull: true, pk: true, unique: false, dflt: null, json: false, readOnly: false },
          { name: "session_title", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
        ],
        rows: [{ session_id: SID, session_title: "测试会话" }],
      },
      {
        name: "chat_turn",
        scope: "session",
        editable: true,
        derived: false,
        refCount: 0,
        columns: [
          { name: "id", type: "INTEGER", notNull: true, pk: true, unique: false, dflt: null, json: false, readOnly: true },
          { name: "session_id", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "turn_id", type: "TEXT", notNull: true, pk: false, unique: true, dflt: null, json: false, readOnly: false },
          { name: "reply_to_message_id", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "response_message_id", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "turn_status", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "created_at", type: "bigint", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
        ],
        rows: [
          {
            id: 18,
            session_id: SID,
            turn_id: "6a24fa174c1b864afff8b57e",
            reply_to_message_id: userMsg,
            response_message_id: asstMsg,
            turn_status: "completed",
            created_at: "1780808215", // ⚠️ 真机上是**字符串**
          },
        ],
      },
      {
        name: "chat_message",
        scope: "session",
        editable: true,
        derived: false,
        refCount: 0,
        columns: [
          { name: "id", type: "INTEGER", notNull: true, pk: true, unique: false, dflt: null, json: false, readOnly: true },
          { name: "session_id", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "message_id", type: "TEXT", notNull: true, pk: false, unique: true, dflt: null, json: false, readOnly: false },
          { name: "message_role", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "message_index", type: "INTEGER", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "reply_to_message_id", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "created_at", type: "bigint", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
        ],
        rows: [
          { id: 35, session_id: SID, message_id: userMsg, message_role: "user", message_index: 1, reply_to_message_id: "", created_at: 1780808215 },
          { id: 36, session_id: SID, message_id: asstMsg, message_role: "assistant", message_index: 2, reply_to_message_id: userMsg, created_at: 1780808215 },
        ],
      },
      {
        name: "chat_message_general",
        scope: "message",
        editable: true,
        derived: false,
        refCount: 0,
        columns: [
          { name: "id", type: "INTEGER", notNull: true, pk: true, unique: false, dflt: null, json: false, readOnly: true },
          { name: "message_id", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          // ⚠️ 真机上 `content` 是**存 JSON 的 TEXT 列**（`json: true`），清空时要给 `[]`
          { name: "content", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: true, readOnly: false },
        ],
        rows: [
          {
            id: 90,
            message_id: opts.orphanBody ? "ffffffffffffffffffffffff" : userMsg,
            content: [{ type: "text", text_content: "帮我看看这个报错" }],
          },
        ],
      },
      {
        name: "history_v2",
        scope: "session",
        editable: true,
        derived: false,
        refCount: 0,
        columns: [
          { name: "id", type: "INTEGER", notNull: true, pk: true, unique: false, dflt: null, json: false, readOnly: true },
          { name: "session_id", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "history_v2_id", type: "TEXT", notNull: true, pk: false, unique: true, dflt: null, json: false, readOnly: false },
          { name: "message_id", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: false, readOnly: false },
          { name: "messages", type: "TEXT", notNull: true, pk: false, unique: false, dflt: null, json: true, readOnly: false },
        ],
        rows: [
          {
            id: 500,
            session_id: SID,
            history_v2_id: "6a24fa174c1b864afff8b580",
            message_id: asstMsg,
            messages: [{ role: "user", text: "帮我看看这个报错" }],
          },
        ],
      },
    ],
  };
}

describe("buildSessionView", () => {
  test("chat_turn 一行 = 一个回合，两条消息按角色归位", () => {
    const v = buildSessionView(doc());
    expect(v.turns).toHaveLength(1);
    const t = v.turns[0];
    expect(t.seq).toBe(1);
    expect(t.turnId).toBe("6a24fa174c1b864afff8b57e");
    expect(t.status).toBe("completed");
    expect(t.user?.role).toBe("user");
    expect(t.assistant?.role).toBe("assistant");
    // 提问摘要取自 chat_message_general.content 里的正文，而不是整棵 JSON
    expect(t.question).toBe("帮我看看这个报错");
    expect(t.dangling).toBe(false);
    expect(v.orphans).toHaveLength(0);
  });

  test("附件按 message_id 归属：general 挂用户、history_v2 挂助手", () => {
    const v = buildSessionView(doc());
    const t = v.turns[0];
    expect(t.user?.attachments.map((g) => g.table)).toEqual(["chat_message_general"]);
    expect(t.assistant?.attachments.map((g) => g.table)).toEqual(["history_v2"]);
    // 会话级表不进任何回合
    expect(v.sessionScope.map((g) => g.table)).toEqual(["chat_session"]);
  });

  test("悬空引用是正常状态：回合指向的消息不在 chat_message 里也不崩", () => {
    const d = doc();
    const ct = d.tables.find((t) => t.name === "chat_turn")!;
    ct.rows[0].response_message_id = "ffffffffffffffffffffffff";
    const v = buildSessionView(d);
    const t = v.turns[0];
    expect(t.dangling).toBe(true);
    expect(t.assistant?.message).toBeNull();
    expect(t.user?.message).not.toBeNull();
    // 落单的原助手消息不会被丢掉：它变成「散落消息」，
    // 它名下的 history_v2 也跟着它走（否则那些行会变成无处可去的孤儿）。
    expect(v.orphans.map((m) => m.id)).toEqual(["6a24fa174c1b864afff8b57a"]);
    expect(v.orphans[0].attachments.map((g) => g.table)).toEqual(["history_v2"]);
  });

  test("指针为空**不算**悬空引用（真机上一批 canceled 回合就是这样）", () => {
    const d = doc();
    const ct = d.tables.find((t) => t.name === "chat_turn")!;
    ct.rows[0].response_message_id = "";
    const v = buildSessionView(d);
    expect(v.turns[0].dangling).toBe(false);
    expect(v.turns[0].assistant).toBeNull();
  });

  test("正文表指向不存在的消息时，该行进「会话级」而不是被丢掉", () => {
    const v = buildSessionView(doc({ orphanBody: true }));
    expect(v.turns[0].dangling).toBe(false);
    expect(v.sessionScope.some((g) => g.table === "chat_message_general")).toBe(true);
  });

  test("没有 chat_turn 指向的消息进「散落消息」", () => {
    const d = doc();
    const cm = d.tables.find((t) => t.name === "chat_message")!;
    cm.rows.push({
      id: 37,
      session_id: d.sessionId,
      message_id: "6a24fbb05e4559ee81f5d25f",
      message_role: "user",
      message_index: 3,
      reply_to_message_id: "",
      created_at: 1780808624,
    });
    const v = buildSessionView(d);
    expect(v.turns).toHaveLength(1);
    expect(v.orphans.map((m) => m.id)).toEqual(["6a24fbb05e4559ee81f5d25f"]);
  });

  test("回合按 message_index 排序，不会与 created_at（unix 秒）混着比", () => {
    const d = doc();
    const ct = d.tables.find((t) => t.name === "chat_turn")!;
    const cm = d.tables.find((t) => t.name === "chat_message")!;
    // 再加一个回合，时间戳更大但 message_index 更小
    ct.rows.push({
      id: 19,
      session_id: d.sessionId,
      turn_id: "6a24fbb04c1b864afff8b5b1",
      reply_to_message_id: "6a24fbb05e4559ee81f5d25f",
      response_message_id: "6a24fbb04c1b864afff8b5ae",
      turn_status: "canceled",
      created_at: "9999999999",
    });
    cm.rows.push(
      { id: 37, session_id: d.sessionId, message_id: "6a24fbb05e4559ee81f5d25f", message_role: "user", message_index: 0, reply_to_message_id: "", created_at: 1780808624 },
      { id: 38, session_id: d.sessionId, message_id: "6a24fbb04c1b864afff8b5ae", message_role: "assistant", message_index: 1, reply_to_message_id: "6a24fbb05e4559ee81f5d25f", created_at: 1780808624 },
    );
    const v = buildSessionView(d);
    expect(v.turns.map((t) => t.seq)).toEqual([1, 2]);
    expect(v.turns[0].turnId).toBe("6a24fbb04c1b864afff8b5b1");
  });
});

describe("identity / 复制", () => {
  test("身份列口径：session_id 算身份列，project_id / user_id 不算（与后端一致）", () => {
    const mk = (name: string, type: string, pk = false, unique = false) => ({
      name,
      type,
      notNull: false,
      pk,
      unique,
      dflt: null,
      json: false,
      readOnly: false,
    });
    expect(isIdentityColumn(mk("session_id", "TEXT"))).toBe(true);
    expect(isIdentityColumn(mk("message_id", "TEXT", false, true))).toBe(true);
    expect(isIdentityColumn(mk("id", "INTEGER", true))).toBe(false);
    expect(isIdentityColumn(mk("message_index", "INTEGER"))).toBe(false);
    expect(isIdentityColumn(mk("project_id", "TEXT", false, true))).toBe(false);
    expect(isIdentityColumn(mk("user_id", "TEXT", false, true))).toBe(false);
  });

  test("复制消息组：新 message_id 一致地换到所有行，且不换 session_id", () => {
    const d = doc();
    const refs = messageRefs(buildSessionView(d).turns[0].user!);
    const olds = identityValuesToRemap(d, refs);
    // 用户消息组只有 chat_message.message_id 是身份列
    expect(olds).toEqual(["6a24fa175e4559ee81f5d25e"]);

    const fresh = "aaaaaaaaaaaaaaaaaaaaaaaa";
    const r = duplicateRows(d, refs, [fresh], { blank: false });
    expect(r.idMap["6a24fa175e4559ee81f5d25e"]).toBe(fresh);

    const cm = r.doc.tables.find((t) => t.name === "chat_message")!;
    const gen = r.doc.tables.find((t) => t.name === "chat_message_general")!;
    // 副本追加在表尾，自增主键必须被剥掉（留给 SQLite）
    const newMsg = cm.rows[cm.rows.length - 1];
    expect(newMsg.message_id).toBe(fresh);
    expect("id" in newMsg).toBe(false);
    expect(newMsg.session_id).toBe(d.sessionId);
    // ⚠️ 关键：正文行的 message_id 必须跟着换成新 id，否则副本会指向**原消息**
    const newGen = gen.rows[gen.rows.length - 1];
    expect(newGen.message_id).toBe(fresh);
    // 新消息的序号接在最大值之后
    expect(newMsg.message_index).toBe(3);
  });

  test("复制整个回合：turn_id 与两条 message_id 都换，且内部引用跟着改", () => {
    const d = doc();
    const refs = turnRefs(buildSessionView(d).turns[0]);
    const olds = identityValuesToRemap(d, refs).sort();
    expect(olds).toEqual(
      ["6a24fa174c1b864afff8b57e", "6a24fa174c1b864afff8b57a", "6a24fa175e4559ee81f5d25e", "6a24fa174c1b864afff8b580"].sort(),
    );

    const fresh = ["id-turn", "id-asst", "id-user", "id-hist"];
    const r = duplicateRows(d, refs, fresh, { blank: true });
    const ct = r.doc.tables.find((t) => t.name === "chat_turn")!;
    const cm = r.doc.tables.find((t) => t.name === "chat_message")!;
    const newTurn = ct.rows[ct.rows.length - 1];
    expect(newTurn.turn_id).toBe(r.idMap["6a24fa174c1b864afff8b57e"]);
    // 回合指针必须指到**副本自己的**两条消息，而不是原回合的
    expect(newTurn.reply_to_message_id).toBe(r.idMap["6a24fa175e4559ee81f5d25e"]);
    expect(newTurn.response_message_id).toBe(r.idMap["6a24fa174c1b864afff8b57a"]);
    // 助手的 reply_to_message_id 也要跟着换（同一批次共用一张映射表）
    const copiedAsst = cm.rows[cm.rows.length - 1];
    expect(copiedAsst.message_id).toBe(r.idMap["6a24fa174c1b864afff8b57a"]);
    expect(copiedAsst.reply_to_message_id).toBe(r.idMap["6a24fa175e4559ee81f5d25e"]);

    // blank：正文清空，但结构列（角色 / 状态 / 时间）保留
    const gen = r.doc.tables.find((t) => t.name === "chat_message_general")!;
    expect(gen.rows[gen.rows.length - 1].content).toEqual([]);
    expect(newTurn.turn_status).toBe("completed");
    expect(newTurn.created_at).toBe("1780808215");
  });

  test("复制后不会出现重复身份值（后端那条唯一列校验会当场拦下）", () => {
    const d = doc();
    const refs = turnRefs(buildSessionView(d).turns[0]);
    const r = duplicateRows(d, refs, ["a", "b", "c", "d"], { blank: true });
    const cm = r.doc.tables.find((t) => t.name === "chat_message")!;
    const ids = cm.rows.map((x) => x.message_id);
    expect(new Set(ids).size).toBe(ids.length);
    const ct = r.doc.tables.find((t) => t.name === "chat_turn")!;
    const tids = ct.rows.map((x) => x.turn_id);
    expect(new Set(tids).size).toBe(tids.length);
  });
});

describe("批量修改与删除", () => {
  test("统一改字段只动确实有这一列的表", () => {
    const d = doc();
    const refs: RowRef[] = [
      { table: "chat_message", index: 0 },
      { table: "chat_message", index: 1 },
    ];
    const out = batchSetColumn(d, refs, "message_role", "user");
    const cm = out.tables.find((t) => t.name === "chat_message")!;
    expect(cm.rows[0].message_role).toBe("user");
    expect(cm.rows[1].message_role).toBe("user");
    // 别的表一行不动
    const gen = out.tables.find((t) => t.name === "chat_message_general")!;
    expect(gen.rows[0].content).toEqual(d.tables.find((t) => t.name === "chat_message_general")!.rows[0].content);
  });

  test("查找替换会递归进 JSON 列，并如实返回替换次数", () => {
    const d = doc();
    const refs: RowRef[] = [
      { table: "chat_message_general", index: 0 },
      { table: "history_v2", index: 0 },
    ];
    // 全文替换：两处「报错」分别住在 general.content 与 history_v2.messages 里
    const r = batchReplaceText(d, refs, null, "报错", "异常", false);
    expect(r.replaced).toBe(2);
    const gen = r.doc.tables.find((t) => t.name === "chat_message_general")!;
    expect(JSON.stringify(gen.rows[0].content)).toContain("异常");
    // 没命中时要能被调用方发现（返回 0），不能假装成功
    const r2 = batchReplaceText(d, refs, null, "不存在的词", "x", false);
    expect(r2.replaced).toBe(0);
  });

  test("区分大小写开关真的生效", () => {
    const d = doc();
    const cm = d.tables.find((t) => t.name === "chat_message")!;
    cm.rows[0].message_role = "User";
    const refs: RowRef[] = [{ table: "chat_message", index: 0 }];
    expect(batchReplaceText(d, refs, "message_role", "user", "x", false).replaced).toBe(1);
    expect(batchReplaceText(d, refs, "message_role", "user", "x", true).replaced).toBe(0);
  });

  test("删除行按表分组过滤，其它表不受影响", () => {
    const d = doc();
    const out = deleteRowRefs(d, [
      { table: "chat_message", index: 0 },
      { table: "history_v2", index: 0 },
    ]);
    expect(out.tables.find((t) => t.name === "chat_message")!.rows).toHaveLength(1);
    expect(out.tables.find((t) => t.name === "history_v2")!.rows).toHaveLength(0);
    expect(out.tables.find((t) => t.name === "chat_turn")!.rows).toHaveLength(1);
  });

  test("可改列的候选：标出「部分行没有这一列」", () => {
    const d = doc();
    const refs: RowRef[] = [
      { table: "chat_message", index: 0 },
      { table: "chat_message_general", index: 0 },
    ];
    const c = columnChoices(d, refs);
    expect(c.find((x) => x.name === "message_id")?.everyRow).toBe(true);
    expect(c.find((x) => x.name === "content")?.everyRow).toBe(false);
    // 自增主键不出现在候选里
    expect(c.some((x) => x.name === "id")).toBe(false);
  });

  test("按列类型解析输入值", () => {
    const num = { name: "message_index", type: "INTEGER", json: false, everyRow: true };
    expect(parseByColumn(num, "12")).toBe(12);
    expect(parseByColumn(num, "")).toBe(0);
    const json = { name: "content", type: "TEXT", json: true, everyRow: true };
    expect(parseByColumn(json, '[{"a":1}]')).toEqual([{ a: 1 }]);
    expect(parseByColumn(json, "随便一段文本")).toBe("随便一段文本");
    const txt = { name: "summary", type: "TEXT", json: false, everyRow: true };
    expect(parseByColumn(txt, "null")).toBeNull();
  });

  test("textOf：把 JSON 正文拍成可读文本，$jsonRef 只给预览", () => {
    expect(textOf([{ type: "text", text_content: "你好" }])).toBe("你好");
    expect(textOf({ $jsonRef: { pk: 1, bytes: 100, preview: "abc" } })).toBe("abc…");
    expect(textOf({ $blob: "AAAA" })).toBe("（二进制）");
    expect(textOf(null)).toBe("");
  });
});

// ---------------------------------------------------------------------------
// 会话式正文（把一条消息拆成「一段一段」）
// ---------------------------------------------------------------------------

const SID2 = "6a24f7e84c1b864afff8b578";
const UID2 = "6a24fa175e4559ee81f5d25e";
const AID2 = "6a24fa174c1b864afff8b57a";

/** 正文行夹具：assistant 侧带上真机同款的 `messages[]` 形状。 */
function bodyDoc(
  opts: {
    user?: unknown;
    msgs?: unknown[];
    taskByRef?: boolean;
    noTask?: boolean;
    noTurnRow?: boolean;
  } = {},
): TraeEditDoc {
  const col = (name: string, extra: Partial<Record<string, unknown>> = {}) => ({
    name,
    type: "TEXT",
    notNull: true,
    pk: false,
    unique: false,
    dflt: null,
    json: false,
    readOnly: false,
    ...extra,
  });
  const cols = [
    { ...col("id"), type: "INTEGER", pk: true, readOnly: true },
    col("message_id"),
    col("content", { json: true }),
  ];
  const tables: TraeEditDoc["tables"] = [
    {
      name: "chat_message",
      scope: "message",
      editable: true,
      derived: false,
      refCount: 0,
      columns: [
        { ...col("id"), type: "INTEGER", pk: true, readOnly: true },
        col("session_id"),
        col("message_id", { unique: true }),
        col("message_role"),
        { ...col("message_index"), type: "INTEGER" },
      ],
      rows: [
        { id: 35, session_id: SID2, message_id: UID2, message_role: "user", message_index: 1 },
        { id: 36, session_id: SID2, message_id: AID2, message_role: "assistant", message_index: 2 },
      ],
    },
    {
      name: "chat_message_general",
      scope: "message",
      editable: true,
      derived: false,
      refCount: 0,
      columns: cols,
      rows: [{
        id: 90,
        message_id: UID2,
        content: opts.user ?? [{ type: "text", text_content: "帮我看看这个报错" }],
      }],
    },
  ];
  if (!opts.noTask) {
    tables.push({
      name: "chat_message_task",
      scope: "message",
      editable: true,
      derived: false,
      refCount: opts.taskByRef ? 1 : 0,
      columns: cols,
      rows: [{
        id: 91,
        message_id: AID2,
        content: opts.taskByRef
          ? { $jsonRef: { pk: 91, bytes: 400000, preview: '{"task_id":"6a24…' } }
          : {
              messages: opts.msgs ?? [
                {
                  id: "m0",
                  type: "plan_item",
                  plan_item: {
                    thought: "我先读一下这个文件。",
                    tool_call_info: { name: "Read", params: { file_path: "d:\\ysq\\a.xlsx" } },
                  },
                },
                {
                  id: "m1",
                  type: "plan_item",
                  plan_item: {
                    thought: "",
                    tool_call_info: { name: "RunCommand", params: { blocking: true, command: "pip install openpyxl" } },
                  },
                },
                {
                  id: "m2",
                  type: "plan_item",
                  plan_item: {
                    thought: "",
                    tool_call_info: { name: "finish", params: { summary: "已经跑完了。" } },
                  },
                },
              ],
              task_id: "t1",
              user_message_id: UID2,
            },
      }],
    });
  }
  if (!opts.noTurnRow) {
    tables.push({
      name: "chat_turn",
      scope: "session",
      editable: true,
      derived: false,
      refCount: 0,
      columns: [
        { ...col("id"), type: "INTEGER", pk: true, readOnly: true },
        col("session_id"),
        col("turn_id", { unique: true }),
        col("reply_to_message_id"),
        col("response_message_id"),
        col("turn_status"),
      ],
      rows: [{
        id: 18,
        session_id: SID2,
        turn_id: "6a2501e34c1b864afff8b630",
        reply_to_message_id: UID2,
        response_message_id: AID2,
        turn_status: "completed",
      }],
    });
  }
  return {
    clientKey: "trae-cn",
    clientLabel: "Trae CN",
    sessionId: SID2,
    title: "测试会话",
    ownerUid: "925158341882023",
    counts: {},
    refCount: 0,
    warnings: [],
    tables,
  };
}

describe("会话式正文（segmentsOf / bodyOf）", () => {
  test("正文表按角色归位：user → general，assistant → task", () => {
    const d = bodyDoc();
    const v = buildSessionView(d);
    const t = v.turns[0];
    const ub = bodyOf(d, t.user!);
    const ab = bodyOf(d, t.assistant!);
    expect(ub?.table).toBe("chat_message_general");
    expect(ab?.table).toBe("chat_message_task");
    expect(ub?.byRef).toBe(false);
    expect(ab?.byRef).toBe(false);
  });

  test("用户消息 = 一段「提问」", () => {
    const d = bodyDoc();
    const t = buildSessionView(d).turns[0];
    const segs = segmentsOf("user", bodyOf(d, t.user!)!.row);
    expect(segs).toHaveLength(1);
    expect(segs[0].kind).toBe("text");
    expect(segs[0].text).toBe("帮我看看这个报错");
    expect(segs[0].path).toEqual([0, "text_content"]);
    expect(segs[0].plain).toBe(true);
  });

  test("助手消息按「先说明、后动作」排序，空 thought 不产生段", () => {
    const d = bodyDoc();
    const t = buildSessionView(d).turns[0];
    const segs = segmentsOf("assistant", bodyOf(d, t.assistant!)!.row);
    // m0 的 thought + Read，m1 的 RunCommand（thought 为空 ⇒ 不出段），m2 的 finish
    expect(segs.map((s) => s.kind)).toEqual(["think", "tool", "tool", "final"]);
    expect(segs.map((s) => s.label)).toEqual(["说明", "Read", "RunCommand", "答复"]);
    expect(segs[0].text).toBe("我先读一下这个文件。");
    expect(segs[2].text).toBe("pip install openpyxl");
    expect(segs[3].text).toBe("已经跑完了。");
    // 每段的 path 必须精确指向自己那一段，不能都指到同一个地方
    expect(segs[0].path).toEqual(["messages", 0, "plan_item", "thought"]);
    expect(segs[2].path).toEqual([
      "messages", 1, "plan_item", "tool_call_info", "params",
    ]);
    expect(segs[3].path).toEqual([
      "messages", 2, "plan_item", "tool_call_info", "params", "summary",
    ]);
  });

  test("finish 的 summary 为空也要出段（用户要能把它补上）", () => {
    const d = bodyDoc({
      msgs: [
        { id: "m0", type: "plan_item", plan_item: { thought: "", tool_call_info: { name: "finish", params: { summary: "" } } } },
      ],
    });
    const t = buildSessionView(d).turns[0];
    const segs = segmentsOf("assistant", bodyOf(d, t.assistant!)!.row);
    expect(segs).toHaveLength(1);
    expect(segs[0].kind).toBe("final");
    expect(segs[0].text).toBe("");
  });

  test("按引用返回的正文只给占位，不给假段落", () => {
    const d = bodyDoc({ taskByRef: true });
    const t = buildSessionView(d).turns[0];
    const b = bodyOf(d, t.assistant!);
    expect(b?.byRef).toBe(true);
    const segs = segmentsOf("assistant", b!.row);
    expect(segs).toHaveLength(1);
    expect(segs[0].label).toBe("正文未加载");
    expect(segs[0].plain).toBe(false);
    expect(segs[0].path).toEqual([]);
  });

  test("没有正文表的消息 → bodyOf 返回 null（不是造一个空行）", () => {
    const d = bodyDoc({ noTask: true });
    const t = buildSessionView(d).turns[0];
    expect(bodyOf(d, t.assistant!)).toBeNull();
  });

  test("toolSummary 优先取最能说明意图的参数", () => {
    expect(toolSummary("RunCommand", { blocking: true, command: "pip install x" })).toBe("pip install x");
    expect(toolSummary("Read", { file_path: "d:\\a.xlsx" })).toBe("d:\\a.xlsx");
    expect(toolSummary("Grep", { pattern: "TODO" })).toBe("TODO");
    expect(toolSummary("X", { other: 1 })).toBe("1");
    expect(toolSummary("X", null)).toBe("");
  });
});

describe("按路径改正文（readAtPath / setAtPath / setCell）", () => {
  const root = { messages: [{ plan_item: { thought: "a", tool_call_info: { name: "Read" } } }] };

  test("读得到；路径不存在 → undefined", () => {
    expect(readAtPath(root, ["messages", 0, "plan_item", "thought"])).toBe("a");
    expect(readAtPath(root, ["messages", 9, "plan_item"])).toBeUndefined();
    expect(readAtPath(root, ["messages", 0, "plan_item", "nope"])).toBeUndefined();
  });

  test("写回不改原对象，且只动那一条路径", () => {
    const next = setAtPath(root, ["messages", 0, "plan_item", "thought"], "b") as typeof root;
    expect(next.messages[0].plan_item.thought).toBe("b");
    expect(next.messages[0].plan_item.tool_call_info.name).toBe("Read");
    expect(root.messages[0].plan_item.thought).toBe("a"); // 原对象没被改
  });

  test("缺键/父级为 null 时按路径补出容器", () => {
    const got = setAtPath({ messages: [{ plan_item: { thought: null } }] },
      ["messages", 0, "plan_item", "thought"], "补上") as { messages: { plan_item: { thought: string } }[] };
    expect(got.messages[0].plan_item.thought).toBe("补上");
    const got2 = setAtPath({}, ["messages", 0, "plan_item", "thought"], "x") as Record<string, unknown>;
    expect(readAtPath(got2, ["messages", 0, "plan_item", "thought"])).toBe("x");
  });

  test("数组下标超出长度时补成数组而不是对象", () => {
    const got = setAtPath({}, [1, "text_content"], "hi") as Record<string, unknown>[];
    expect(Array.isArray(got)).toBe(true);
    expect((got[1] as { text_content: string }).text_content).toBe("hi");
  });

  test("setCell 把改过的 content 写回对应行的对应列", () => {
    const d = bodyDoc();
    const t = buildSessionView(d).turns[0];
    const b = bodyOf(d, t.user!)!;
    const nextContent = setAtPath(b.row["content"], [0, "text_content"], "改过了");
    const d2 = setCell(d, b.ref, "content", nextContent);
    const t2 = buildSessionView(d2).turns[0];
    expect(segmentsOf("user", bodyOf(d2, t2.user!)!.row)[0].text).toBe("改过了");
    // 别的行一行没动
    expect(tableOf(d2, "chat_message_general")!.rows[0]["message_id"]).toBe(UID2);
  });

  test("isJsonRef 只认对象里的 $jsonRef，不认数组和字符串", () => {
    expect(isJsonRef({ $jsonRef: { pk: 1 } })).toBe(true);
    expect(isJsonRef([{ $jsonRef: {} }])).toBe(false);
    expect(isJsonRef("x")).toBe(false);
    expect(isJsonRef(null)).toBe(false);
  });

  test("removeAtPath：数组摘项、对象去键；容器对不上就原样返回", () => {
    const arr = [{ a: 1 }, { a: 2 }, { a: 3 }];
    expect(removeAtPath(arr, [1])).toEqual([{ a: 1 }, { a: 3 }]);
    const obj = { messages: [{ id: "x" }], task_id: "t" };
    expect(removeAtPath(obj, ["task_id"])).toEqual({ messages: [{ id: "x" }] });
    // 路径指到字符串上 → 不动
    expect(removeAtPath({ a: "s" }, ["a", "b"])).toEqual({ a: "s" });
  });

  test("removeAtPath：助手侧摘掉整个 messages[i] 后，剩下的段落顺序不乱", () => {
    const d = bodyDoc();
    const t = buildSessionView(d).turns[0];
    const b = bodyOf(d, t.assistant!)!;
    const cut = removeAtPath(b.row["content"], ["messages", 1]);
    const segs = segmentsOf("assistant", { content: cut });
    expect(segs.map((s) => s.label)).toEqual(["说明", "Read", "答复"]);
  });

  test("appendAtPath：往 messages 末尾加一段空答复，客户端认得这种形状", () => {
    const d = bodyDoc();
    const t = buildSessionView(d).turns[0];
    const b = bodyOf(d, t.assistant!)!;
    const item = emptySegmentItem("assistant", "ffffffffffffffffffffffff");
    const next = appendAtPath(b.row["content"], segmentContainerPath("assistant"), item);
    const segs = segmentsOf("assistant", { content: next });
    expect(segs.map((s) => s.kind)).toEqual(["think", "tool", "tool", "final", "final"]);
    expect(segs[4].text).toBe("");
    // 新项必须自带 tool_call_info.name=finish，否则客户端读不出这一段
    const nm = (next as { messages: { plan_item: { tool_call_info: { name: string } } }[] })
      .messages.slice(-1)[0].plan_item.tool_call_info.name;
    expect(nm).toBe("finish");
  });

  test("appendAtPath：用户侧加到 content 数组；数组缺失时补成数组", () => {
    const got = appendAtPath(undefined, segmentContainerPath("user"), emptySegmentItem("user", "")) as unknown[];
    expect(Array.isArray(got)).toBe(true);
    expect(got).toEqual([{ type: "text", text_content: "" }]);
    const segs = segmentsOf("user", { content: got });
    expect(segs).toHaveLength(1);
    expect(segs[0].text).toBe("");
  });
});

// ---------------------------------------------------------------------------
// 批量删回合：空会话判定
//
// 这一组测的是**同一条硬规则的两种情形**，不是两套实现：
// 「删完还剩几个回合」必须按「一次要删的全部回合」算。
// 逐个问时每次剩下的都 ≥1、合起来却归 0 —— 那就写出空壳会话了
// （客户端打开永远卡「环境创建中」、发不出消息、无自愈路径）。
// ---------------------------------------------------------------------------
describe("批量删回合", () => {
  /** 在 `doc()` 基础上再追加到共 n 个回合。 */
  function withTurns(n: number): TraeEditDoc {
    const d = doc();
    const SID = d.sessionId;
    const turns = tableOf(d, "chat_turn");
    const msgs = tableOf(d, "chat_message");
    const gens = tableOf(d, "chat_message_general");
    if (!turns || !msgs || !gens) throw new Error("fixture 缺表");
    for (let i = 2; i <= n; i += 1) {
      const suffix = String(10 + i);
      const u = `6a24fa174c1b864afff8b6${suffix}`;
      const a = `6a24fa174c1b864afff8b7${suffix}`;
      turns.rows.push({
        id: 10 + i,
        session_id: SID,
        turn_id: `6a24fa174c1b864afff8b5${suffix}`,
        reply_to_message_id: u,
        response_message_id: a,
        turn_status: "completed",
        created_at: String(1780808215 + i),
      });
      msgs.rows.push(
        {
          id: 100 + i * 2,
          session_id: SID,
          message_id: u,
          message_role: "user",
          message_index: i * 2 - 1,
          reply_to_message_id: "",
          created_at: 1780808215 + i,
        },
        {
          id: 101 + i * 2,
          session_id: SID,
          message_id: a,
          message_role: "assistant",
          message_index: i * 2,
          reply_to_message_id: u,
          created_at: 1780808215 + i,
        },
      );
      gens.rows.push({
        id: 200 + i,
        message_id: u,
        content: [{ type: "text", text_content: `第 ${i} 个提问` }],
      });
    }
    return d;
  }

  test("删掉唯一的回合 ⇒ 剩 0，必须改道「删整条会话」", () => {
    const view = buildSessionView(withTurns(1));
    expect(view.turns).toHaveLength(1);
    expect(turnsLeftAfterRemoving(view.turns, [view.turns[0].key])).toBe(0);
  });

  test("⚠️ 逐个问都「还剩 ≥1」，合起来却是 0 —— 所以只能按整批算", () => {
    const view = buildSessionView(withTurns(3));
    expect(view.turns).toHaveLength(3);
    const keys = view.turns.map((t) => t.key);
    for (const k of keys) {
      // 单看每一个都不该改道 —— 这正是逐个判断会把问题放过去的原因。
      expect(turnsLeftAfterRemoving(view.turns, [k])).toBeGreaterThan(0);
    }
    // 整批算归 0 ⇒ 必须走「删整条会话」，否则写出的就是空壳会话。
    expect(turnsLeftAfterRemoving(view.turns, keys)).toBe(0);
  });

  test("删一部分 ⇒ 剩下的数目对；键不存在时按「一个都没删」算", () => {
    const view = buildSessionView(withTurns(3));
    const keys = view.turns.map((t) => t.key);
    expect(turnsLeftAfterRemoving(view.turns, [keys[1]])).toBe(2);
    expect(turnsLeftAfterRemoving(view.turns, [keys[0], keys[2]])).toBe(1);
    expect(turnsLeftAfterRemoving(view.turns, [])).toBe(3);
    expect(turnsLeftAfterRemoving(view.turns, ["这个 key 不存在"])).toBe(3);
  });

  test("批量删 = 「这些回合名下的全部行」，会话级行一行不许动", () => {
    const d = withTurns(3);
    const view = buildSessionView(d);
    expect(view.turns).toHaveLength(3);
    // 挑第 2、3 个回合（第 1 个回合名下挂着 history_v2 行，留着它验证会话级行不动）。
    const picks = [view.turns[1], view.turns[2]];
    const after = deleteRowRefs(
      d,
      picks.flatMap((t) => turnRefs(t)),
    );
    expect(tableOf(after, "chat_turn")?.rows).toHaveLength(1);
    expect(tableOf(after, "chat_message")?.rows).toHaveLength(2);
    expect(tableOf(after, "chat_message_general")?.rows).toHaveLength(1);
    // 会话级 / 未被删回合名下的行，一行都不许被顺手带走。
    expect(tableOf(after, "chat_session")?.rows).toHaveLength(1);
    expect(tableOf(after, "history_v2")?.rows).toHaveLength(1);
    const left = buildSessionView(after);
    expect(left.turns).toHaveLength(1);
    expect(left.turns[0].user).not.toBeNull();
    expect(left.turns[0].assistant).not.toBeNull();
  });

  test("两个回合的引用有重叠时按 (表,行号) 去重，不会误删别人的行", () => {
    const d = withTurns(2);
    const view = buildSessionView(d);
    const refs = view.turns.flatMap((t) => turnRefs(t));
    // 故意把同一个回合的引用塞两遍 —— 去重靠 `deleteRowRefs`，不是靠调用方。
    const after = deleteRowRefs(d, [...refs, ...turnRefs(view.turns[0])]);
    expect(tableOf(after, "chat_turn")?.rows).toHaveLength(0);
    expect(tableOf(after, "chat_message")?.rows).toHaveLength(0);
    expect(tableOf(after, "chat_message_general")?.rows).toHaveLength(0);
    expect(buildSessionView(after).turns).toHaveLength(0);
  });
});
