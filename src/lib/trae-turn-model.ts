// ---------------------------------------------------------------------------
// T46 · 回合视图模型
// ---------------------------------------------------------------------------
//
// 为什么单独一层：Trae 的会话不是「一个标题 + 一串消息」，而是**一组按逻辑 id 相互引用的表**。
// 「一个回合」在库里对应 `chat_turn` 一行，它用 `reply_to_message_id` /
// `response_message_id` 指向两条 `chat_message`（用户提问 + 助手回答），这两条消息又各自
// 挂着 `chat_message_general` / `chat_message_task` / `history_v2` / `history_todo_list` …
//
// 这一层只做**派生与纯变换**（不碰 React、不调后端），所以可以被单测覆盖；
// 组件只负责渲染与调用。所有编辑动作都是不可变更新，靠 `(表名, 行号)` 定位。
//
// ⚠️ 两条真实数据特征（核对过本机库，不是推测）：
//   1. 一条 `chat_message` 只会挂在一张正文表上：`user` → `chat_message_general`，
//      `assistant` → `chat_message_task`。不是「每条消息两张表都有」。
//   2. `chat_turn` 的 `reply_to_message_id` / `response_message_id` **并非都能在
//      `chat_message` 里找到**（本机 83 个回合里有悬空引用）。所以「引用缺失」必须
//      当成一种正常状态渲染，不能当异常崩掉。
// ---------------------------------------------------------------------------

import type { TraeEditColumn, TraeEditDoc, TraeEditTable } from "@/lib/trae-types";

/** doc 里一行的位置。 */
export interface RowRef {
  table: string;
  index: number;
}

export function refKey(r: RowRef): string {
  return `${r.table}#${r.index}`;
}

/** 同一张表里的若干行（保持行序）。 */
export interface TableGroup {
  table: string;
  refs: RowRef[];
}

/** 一条消息（`chat_message` 一行）连同它名下所有附件行。 */
export interface TurnMessage {
  /** `message_id`。 */
  id: string;
  /** `message_role` 原样（`user` / `assistant` / 其它）。 */
  role: string;
  /** `chat_message` 那一行；**可能为 null** —— 悬空引用（见文件头 ⚠️ 2）。 */
  message: RowRef | null;
  /** 这条消息名下的行，按表分组（`chat_message_general` / `_task` / `history_v2` …）。 */
  attachments: TableGroup[];
  /** `message_index`（拿不到时为 null）。 */
  index: number | null;
}

/** 一个回合。 */
export interface TurnView {
  /** 稳定 key：优先 `turn_id`，其次 `#行号`。 */
  key: string;
  /** 展示序号（1 起，按时间序）。 */
  seq: number;
  turnId: string | null;
  /** `chat_turn` 那一行（消息存在但没有对应回合时为 null）。 */
  turn: RowRef | null;
  status: string;
  /** 用户提问的纯文本摘要（列表里显示）。 */
  question: string;
  user: TurnMessage | null;
  assistant: TurnMessage | null;
  /** 归属该回合、但不挂在两条消息上的行（按 `turn_id` 匹配到的那种）。 */
  extra: TableGroup[];
  /** 回合引用的消息在 doc 里找不到。 */
  dangling: boolean;
}

export interface SessionView {
  turns: TurnView[];
  /** 有消息、但没有任何 `chat_turn` 指向它的「散落消息」。 */
  orphans: TurnMessage[];
  /** 会话级行（`chat_session` / `session_project` / `worktree` / `fts_session_title` …）。 */
  sessionScope: TableGroup[];
}

// ---------------------------------------------------------------------------
// 取值小工具
// ---------------------------------------------------------------------------

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

/** 数字或「纯数字字符串」→ number；其余 null。`chat_turn.created_at` 真机上是**字符串**。 */
function numLike(v: unknown): number | null {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && /^\d+$/.test(v.trim())) return Number(v);
  return null;
}

/** 20~24 位 hex，或 36 位 UUID —— 与后端 `is_hex_id_like` / `is_uuid_like` 同口径。 */
export function isIdLike(s: string): boolean {
  if (s.length === 36 && s.split("-").length === 5) return true;
  return s.length >= 20 && s.length <= 24 && /^[0-9a-fA-F]+$/.test(s);
}

/** 该列是不是这张表的**身份列**（与后端 `is_id_defining` 同口径，改的时候要一起改）。 */
export function isIdentityColumn(c: TraeEditColumn): boolean {
  if (c.name === "project_id" || c.name === "user_id") return false;
  if (c.name === "session_id") return true;
  const isInt = c.type.toUpperCase().includes("INT");
  if (c.pk && !isInt) return true;
  if (c.pk && c.name !== "id") return true;
  return c.unique === true && !isInt;
}

/**
 * 把一个单元格的值拍成纯文本（摘要展示 + 查找替换用）。
 *
 * 结构化正文（`content` 那种 JSON）优先取常见的正文字段，取不到才把所有字符串拼接 ——
 * 直接把整棵树拼起来会把 `agent_type` 之类的元数据也带进摘要里，列表就没法看了。
 */
export function textOf(v: unknown): string {
  if (v === null || v === undefined) return "";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  if (Array.isArray(v)) return v.map(textOf).filter(Boolean).join("\n");
  if (typeof v === "object") {
    const o = v as Record<string, unknown>;
    const ref = o.$jsonRef;
    if (ref && typeof ref === "object") {
      const p = (ref as Record<string, unknown>).preview;
      return typeof p === "string" ? `${p}…` : "（大字段未加载）";
    }
    if (typeof o.$blob === "string") return "（二进制）";
    for (const k of ["text_content", "text", "summary", "content", "name"]) {
      const s = o[k];
      if (typeof s === "string" && s) return s;
    }
    return Object.values(o).map(textOf).filter(Boolean).join(" ");
  }
  return "";
}

export function tableOf(doc: TraeEditDoc, name: string): TraeEditTable | null {
  return doc.tables.find((t) => t.name === name) ?? null;
}

/** 一行的摘要文案（列表 / 折叠标题用）。 */
export function rowSummary(t: TraeEditTable, row: Record<string, unknown>): string {
  const pk = t.columns.find((c) => c.pk && !c.readOnly);
  if (pk) {
    const v = row[pk.name];
    if (typeof v === "string" && v) return v;
    if (typeof v === "number") return String(v);
  }
  for (const c of t.columns) {
    const v = row[c.name];
    if (typeof v === "string" && v && v.length <= 60) return v;
    if (typeof v === "number") return String(v);
  }
  return "（空行）";
}

// ---------------------------------------------------------------------------
// 派生：把 doc 变成回合视图
// ---------------------------------------------------------------------------

const MESSAGE_TABLE = "chat_message";
const TURN_TABLE = "chat_turn";

export function buildSessionView(doc: TraeEditDoc): SessionView {
  const cm = tableOf(doc, MESSAGE_TABLE);
  const ct = tableOf(doc, TURN_TABLE);

  // 1) message_id → 行位置
  const msgRow = new Map<string, RowRef>();
  if (cm) {
    cm.rows.forEach((r, i) => {
      const id = str(r.message_id);
      if (id && !msgRow.has(id)) msgRow.set(id, { table: MESSAGE_TABLE, index: i });
    });
  }
  const msgIndexOf = (mid: string): number | null => {
    const rr = mid ? msgRow.get(mid) : undefined;
    if (!rr || !cm) return null;
    return numLike(cm.rows[rr.index]?.message_index);
  };

  // 2) 回合骨架
  interface Skeleton {
    key: string;
    turnId: string | null;
    turn: RowRef;
    userId: string;
    asstId: string;
    status: string;
    ts: number | null;
    docIndex: number;
    msgIndex: number | null;
  }
  const sk: Skeleton[] = [];
  if (ct) {
    ct.rows.forEach((r, i) => {
      const userId = str(r.reply_to_message_id);
      sk.push({
        key: str(r.turn_id) || `#${i}`,
        turnId: str(r.turn_id) || null,
        turn: { table: TURN_TABLE, index: i },
        userId,
        asstId: str(r.response_message_id),
        status: str(r.turn_status),
        ts: numLike(r.created_at),
        docIndex: i,
        msgIndex: msgIndexOf(userId),
      });
    });
  }

  // 排序：**不能混着比** —— `message_index` 是 1..N，`created_at` 是 unix 秒，
  // 混用会让排序整个反过来。所以按「全体是否都有 message_index」二选一。
  const allIndexed = sk.length > 0 && sk.every((s) => s.msgIndex !== null);
  const allTimed = sk.length > 0 && sk.every((s) => s.ts !== null);
  if (allIndexed) sk.sort((a, b) => (a.msgIndex as number) - (b.msgIndex as number));
  else if (allTimed) sk.sort((a, b) => (a.ts as number) - (b.ts as number));
  else sk.sort((a, b) => a.docIndex - b.docIndex);

  // 3) 消息 id → 它挂在哪张表上（用于归属其它表的行）
  const referenced = new Set<string>();
  for (const s of sk) {
    if (s.userId) referenced.add(s.userId);
    if (s.asstId) referenced.add(s.asstId);
  }
  const turnIdSet = new Set<string>();
  for (const s of sk) if (s.turnId) turnIdSet.add(s.turnId);

  // 4) 其它表的每一行归到：某回合的 extra / 某条消息的附件 / 会话级
  const msgAttach = new Map<string, TableGroup[]>(); // message_id → groups
  const turnExtra = new Map<string, TableGroup[]>(); // turn_id → groups
  const scope: TableGroup[] = [];

  const pushGroup = (list: TableGroup[], table: string, ref: RowRef) => {
    const last = list[list.length - 1];
    if (last && last.table === table) last.refs.push(ref);
    else list.push({ table, refs: [ref] });
  };

  for (const t of doc.tables) {
    if (t.name === MESSAGE_TABLE || t.name === TURN_TABLE) continue;
    t.rows.forEach((row, i) => {
      const ref: RowRef = { table: t.name, index: i };
      const tid = str(row.turn_id);
      if (tid && turnIdSet.has(tid)) {
        const arr = turnExtra.get(tid) ?? [];
        pushGroup(arr, t.name, ref);
        turnExtra.set(tid, arr);
        return;
      }
      const mid = str(row.message_id);
      // ⚠️ 判据是「这条 message_id 在 `chat_message` 里存在」，**不是**「被某个回合引用」。
      //    否则散落消息（没有回合指向它）的正文表会被拆到「会话级」去 ——
      //    用户看到的就成了「消息在、内容不见了」，而这正是这次改版要消灭的东西。
      if (mid && msgRow.has(mid)) {
        const arr = msgAttach.get(mid) ?? [];
        pushGroup(arr, t.name, ref);
        msgAttach.set(mid, arr);
        return;
      }
      pushGroup(scope, t.name, ref);
    });
  }

  // 5) 组装消息
  const buildMessage = (id: string): TurnMessage | null => {
    if (!id) return null;
    const ref = msgRow.get(id) ?? null;
    const row = ref && cm ? cm.rows[ref.index] : undefined;
    return {
      id,
      role: row ? str(row.message_role) : "",
      message: ref,
      attachments: msgAttach.get(id) ?? [],
      index: msgIndexOf(id),
    };
  };

  // 正文摘要：用户提问优先取 `chat_message_general.content`
  const questionOf = (userMsgId: string): string => {
    if (!userMsgId) return "";
    for (const g of msgAttach.get(userMsgId) ?? []) {
      if (g.table !== "chat_message_general") continue;
      const gt = tableOf(doc, g.table);
      if (!gt) continue;
      for (const r of g.refs) {
        const s = textOf(gt.rows[r.index]?.content).trim();
        if (s) return s.replace(/\s+/g, " ");
      }
    }
    return "";
  };

  const turns: TurnView[] = sk.map((s, i) => {
    const user = buildMessage(s.userId);
    const assistant = buildMessage(s.asstId);
    const q = questionOf(s.userId);
    // ⚠️ 「指针为空」**不是**悬空引用：真机上 `canceled` 的回合本来就常常没有
    //    `response_message_id`（统计过：83 个回合里 5 个 canceled）。只有
    //    **非空**、但在 `chat_message` 里找不到对应行时才算真的指向了不存在的东西 ——
    //    把空指针也算进去会给一大批完全正常的回合打上「引用缺失」。
    const missing = (id: string, m: TurnMessage | null) => id !== "" && (m === null || m.message === null);
    return {
      key: s.key,
      seq: i + 1,
      turnId: s.turnId,
      turn: s.turn,
      status: s.status,
      question: q || (user ? "（空提问）" : "（无用户消息）"),
      user,
      assistant,
      extra: turnExtra.get(s.turnId ?? "") ?? [],
      dangling: missing(s.userId, user) || missing(s.asstId, assistant),
    };
  });

  const orphans: TurnMessage[] = [];
  if (cm) {
    cm.rows.forEach((r, i) => {
      const id = str(r.message_id);
      if (!id || referenced.has(id)) return;
      const ref: RowRef = { table: MESSAGE_TABLE, index: i };
      orphans.push({
        id,
        role: str(r.message_role),
        message: ref,
        attachments: msgAttach.get(id) ?? [],
        index: numLike(r.message_index),
      });
    });
  }

  return { turns, orphans, sessionScope: scope };
}

/** 一个回合名下**全部行**的位置（回合本体 → 用户消息组 → 助手消息组 → 其它）。 */
export function turnRefs(turn: TurnView): RowRef[] {
  const out: RowRef[] = [];
  if (turn.turn) out.push(turn.turn);
  for (const m of [turn.user, turn.assistant]) {
    if (!m) continue;
    if (m.message) out.push(m.message);
    for (const g of m.attachments) out.push(...g.refs);
  }
  for (const g of turn.extra) out.push(...g.refs);
  return out;
}

/** 一条消息名下全部行（含 `chat_message` 自己）。 */
export function messageRefs(m: TurnMessage): RowRef[] {
  const out: RowRef[] = [];
  if (m.message) out.push(m.message);
  for (const g of m.attachments) out.push(...g.refs);
  return out;
}

/**
 * 删掉这些回合之后**还剩几个回合**。
 *
 * ⚠️⚠️ 批量删除的空会话判定必须按「**一次要删的全部回合**」算。
 * 逐个问「剩几个」时每次剩下的都 ≥1、合起来却归 0 —— 那就直接写出一个空壳会话：
 * 客户端打开它永远卡在「环境创建中」、发不出任何消息，且没有自愈路径。
 * 所以这个数字与 `askDeleteTurns` 里「left === 0 ⇒ 改道删整条会话」是**同源**的。
 *
 * 单个删除是它的退化情形（`keys` 只有一个）—— 两处**必须同解**，不许各写一遍。
 */
export function turnsLeftAfterRemoving(turns: TurnView[], keys: Iterable<string>): number {
  const gone = new Set(keys);
  let left = 0;
  for (const t of turns) {
    if (!gone.has(t.key)) left += 1;
  }
  return left;
}

// ---------------------------------------------------------------------------
// 纯变换：新增 / 复制 / 删除 / 批量改
// ---------------------------------------------------------------------------

function replaceTable(doc: TraeEditDoc, name: string, t: TraeEditTable): TraeEditDoc {
  return { ...doc, tables: doc.tables.map((x) => (x.name === name ? t : x)) };
}

/** 每条身份列的值 → 需要生成几个新 id（交给后端生成，前端不许自己造）。 */
export function identityValuesToRemap(doc: TraeEditDoc, refs: RowRef[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const ref of refs) {
    const t = tableOf(doc, ref.table);
    const row = t?.rows[ref.index];
    if (!t || !row) continue;
    for (const c of t.columns) {
      // ⚠️ `session_id` 不换：复制物留在**同一条会话**里，换了会话 id 就等于另起一条会话。
      if (c.name === "session_id") continue;
      if (!isIdentityColumn(c)) continue;
      const v = row[c.name];
      if (typeof v === "string" && isIdLike(v) && !seen.has(v)) {
        seen.add(v);
        out.push(v);
      }
    }
  }
  return out;
}

/** 递归替换：**整串等于旧 id** 才换；JSON 内部也走一遍；`$jsonRef` 整块跳过（回查键不能动）。 */
function rewriteWithMap(v: unknown, map: Record<string, string>): unknown {
  if (v === null || typeof v !== "object") {
    if (typeof v === "string" && map[v] !== undefined) return map[v];
    return v;
  }
  if (Array.isArray(v)) return v.map((x) => rewriteWithMap(x, map));
  const o = v as Record<string, unknown>;
  if (o.$jsonRef) return v; // 大字段引用：pk 必须保持原值
  const out: Record<string, unknown> = {};
  for (const [k, x] of Object.entries(o)) out[k] = rewriteWithMap(x, map);
  return out;
}

/** 该表里下一个可用的 `message_index`（新建消息要接在末尾）。 */
export function nextMessageIndex(doc: TraeEditDoc): number {
  const cm = tableOf(doc, MESSAGE_TABLE);
  let max = 0;
  for (const r of cm?.rows ?? []) {
    const n = numLike(r.message_index);
    if (n !== null && n > max) max = n;
  }
  return max + 1;
}

/** 复制时要清空的「正文列」（保守白名单 —— 只清一眼就知道是内容的列）。 */
const CONTENT_COLUMNS = new Set([
  "content",
  "summary",
  "messages",
  "rewritten_user_message",
  "requirement_id",
  "accumulated_summary",
  "thought",
  "user_intent",
  "guidelines",
  "error_message",
  "objective",
]);

/** 清空一个单元格：按列声明类型给「空值」，**不是**给 null（这些列基本都 NOT NULL）。 */
export function emptyValue(col: TraeEditColumn, template: unknown): unknown {
  const ty = (col.type || "").toUpperCase();
  if (ty.includes("INT") || ty.includes("REAL") || ty.includes("NUM") || ty === "BOOLEAN") return 0;
  if (col.json) {
    if (Array.isArray(template)) return [];
    if (template && typeof template === "object") return {};
    return "";
  }
  return "";
}

export interface DuplicateResult {
  doc: TraeEditDoc;
  /** 新增行的位置（拼在各自表尾）。 */
  newRefs: RowRef[];
  idMap: Record<string, string>;
}

/**
 * 复制若干行（同表内追加到末尾），并给身份列换新 id。
 *
 * ⚠️ `freshIds` 必须至少 `identityValuesToRemap(doc, refs).length` 个 ——
 * 数目不够会有身份列保持原值，撞 UNIQUE 索引（后端校验会拦下，但别指望它）。
 * ⚠️ `blank`：清空正文列。id 映射是**先按旧值去重再替换**，所以
 * 「同一条消息的 `chat_message` 行与它的 `chat_message_general` 行」会拿到**同一个**新 id ——
 * 这正是消息组必须整体复制的原因，逐行复制会把它们拆散。
 */
export function duplicateRows(
  doc: TraeEditDoc,
  refs: RowRef[],
  freshIds: string[],
  opts: { blank?: boolean } = {},
): DuplicateResult {
  const olds = identityValuesToRemap(doc, refs);
  const idMap: Record<string, string> = {};
  olds.forEach((o, i) => {
    if (freshIds[i]) idMap[o] = freshIds[i];
  });

  const batch = new Map<string, Record<string, unknown>[]>();
  for (const ref of refs) {
    const t = tableOf(doc, ref.table);
    const row = t?.rows[ref.index];
    if (!t || !row) continue;
    let copy = {
      ...(rewriteWithMap(structuredClone(row), idMap) as Record<string, unknown>),
    };
    // 自增主键交给 SQLite 分配：留着这个键会与源行撞主键。
    delete copy.id;
    if (opts.blank) {
      for (const c of t.columns) {
        if (CONTENT_COLUMNS.has(c.name) && c.name in copy) {
          copy[c.name] = emptyValue(c, copy[c.name]);
        }
      }
    }
    const arr = batch.get(t.name) ?? [];
    arr.push(copy);
    batch.set(t.name, arr);
  }

  let out = doc;
  const newRefs: RowRef[] = [];
  for (const [name, rows] of batch) {
    const t = tableOf(out, name);
    if (!t) continue;
    // `chat_message` 的副本统一重排 `message_index`：接着最大值往后排（否则新消息会插到
    // 会话中间），且两条副本不能撞同一个序号 —— 客户端按这个序号排消息。
    if (name === MESSAGE_TABLE) {
      const start = nextMessageIndex(out);
      rows.forEach((r, i) => {
        r.message_index = start + i;
      });
    }
    const at = t.rows.length;
    out = replaceTable(out, name, { ...t, rows: [...t.rows, ...rows] });
    rows.forEach((_, i) => newRefs.push({ table: name, index: at + i }));
  }

  return { doc: out, newRefs, idMap };
}

/** 删掉若干行（按表分组，一次过滤）。 */
export function deleteRowRefs(doc: TraeEditDoc, refs: RowRef[]): TraeEditDoc {
  const drop = new Map<string, Set<number>>();
  for (const r of refs) {
    const s = drop.get(r.table) ?? new Set<number>();
    s.add(r.index);
    drop.set(r.table, s);
  }
  return {
    ...doc,
    tables: doc.tables.map((t) => {
      const s = drop.get(t.name);
      if (!s || s.size === 0) return t;
      return { ...t, rows: t.rows.filter((_, i) => !s.has(i)) };
    }),
  };
}

export interface ColumnChoice {
  name: string;
  type: string;
  json: boolean;
  /** 该名字在所有选中行所属表里都存在（否则统一改字段会漏掉一部分行）。 */
  everyRow: boolean;
}

/** 选中行涉及的表里，列名的**并集**（附「是否每一行都有」）。 */
export function columnChoices(doc: TraeEditDoc, refs: RowRef[]): ColumnChoice[] {
  const tables = new Set(refs.map((r) => r.table));
  const total = refs.length;
  const count = new Map<string, number>();
  const meta = new Map<string, ColumnChoice>();
  for (const name of tables) {
    const t = tableOf(doc, name);
    if (!t) continue;
    for (const c of t.columns) {
      if (c.readOnly) continue;
      const n = refs.filter((r) => r.table === name).length;
      count.set(c.name, (count.get(c.name) ?? 0) + n);
      if (!meta.has(c.name)) {
        meta.set(c.name, { name: c.name, type: c.type, json: c.json, everyRow: false });
      }
    }
  }
  return [...meta.values()]
    .map((m) => ({ ...m, everyRow: (count.get(m.name) ?? 0) === total }))
    .sort((a, b) => {
      if (a.everyRow !== b.everyRow) return a.everyRow ? -1 : 1;
      return a.name.localeCompare(b.name);
    });
}

/** 把一个输入的字符串按列类型转成该写的值。 */
export function parseByColumn(col: ColumnChoice, raw: string): unknown {
  const ty = (col.type || "").toUpperCase();
  if (ty.includes("INT") || ty.includes("REAL") || ty.includes("NUM") || ty === "BOOLEAN") {
    if (raw.trim() === "") return 0;
    const n = Number(raw);
    return Number.isFinite(n) ? n : raw;
  }
  if (raw === "null") return null;
  if (!col.json) return raw;
  // JSON 列：能解析就解析成树（这样才与 `edit_doc` 读出来的形态一致）
  const s = raw.trim();
  if (s === "") return "";
  if ((s.startsWith("{") && s.endsWith("}")) || (s.startsWith("[") && s.endsWith("]"))) {
    try {
      return JSON.parse(s) as unknown;
    } catch {
      return raw;
    }
  }
  return raw;
}

/** 批量把某列设成同一个值（只作用在真的存在该列的行上）。 */
export function batchSetColumn(
  doc: TraeEditDoc,
  refs: RowRef[],
  column: string,
  value: unknown,
): TraeEditDoc {
  const byTable = new Map<string, Set<number>>();
  for (const r of refs) {
    const s = byTable.get(r.table) ?? new Set<number>();
    s.add(r.index);
    byTable.set(r.table, s);
  }
  return {
    ...doc,
    tables: doc.tables.map((t) => {
      const s = byTable.get(t.name);
      if (!s || s.size === 0) return t;
      if (!t.columns.some((c) => c.name === column)) return t;
      return {
        ...t,
        rows: t.rows.map((row, i) => (s.has(i) ? { ...row, [column]: value } : row)),
      };
    }),
  };
}

function replaceInValue(v: unknown, find: string, repl: string, cs: boolean): { v: unknown; n: number } {
  if (typeof v === "string") {
    if (!find) return { v, n: 0 };
    if (cs) {
      const n = v.split(find).length - 1;
      return { v: v.split(find).join(repl), n };
    }
    const lower = v.toLowerCase();
    const f = find.toLowerCase();
    let n = 0;
    let out = "";
    let i = 0;
    for (;;) {
      const at = lower.indexOf(f, i);
      if (at < 0) break;
      out += v.slice(i, at) + repl;
      i = at + f.length;
      n += 1;
    }
    out += v.slice(i);
    return { v: out, n };
  }
  if (Array.isArray(v)) {
    let n = 0;
    const arr = v.map((x) => {
      const r = replaceInValue(x, find, repl, cs);
      n += r.n;
      return r.v;
    });
    return { v: arr, n };
  }
  if (v && typeof v === "object") {
    const o = v as Record<string, unknown>;
    if (o.$jsonRef || typeof o.$blob === "string") return { v, n: 0 };
    let n = 0;
    const out: Record<string, unknown> = {};
    for (const [k, x] of Object.entries(o)) {
      const r = replaceInValue(x, find, repl, cs);
      n += r.n;
      out[k] = r.v;
    }
    return { v: out, n };
  }
  return { v, n: 0 };
}

/**
 * 批量查找替换。
 *
 * `column === null` = 该行所属表里的**全部文本列**（含 JSON 列内部的字符串，会递归进去）。
 * 返回替换次数，界面要如实报出来 ——「说是替换了但一个都没中」是最容易被当成功的一种失败。
 */
export function batchReplaceText(
  doc: TraeEditDoc,
  refs: RowRef[],
  column: string | null,
  find: string,
  repl: string,
  caseSensitive: boolean,
): { doc: TraeEditDoc; replaced: number } {
  if (!find) return { doc, replaced: 0 };
  const byTable = new Map<string, Set<number>>();
  for (const r of refs) {
    const s = byTable.get(r.table) ?? new Set<number>();
    s.add(r.index);
    byTable.set(r.table, s);
  }
  let replaced = 0;
  const tables = doc.tables.map((t) => {
    const sel = byTable.get(t.name);
    if (!sel || sel.size === 0) return t;
    const targets = t.columns.filter((c) => {
      if (c.readOnly) return false;
      if (column !== null) return c.name === column;
      const ty = (c.type || "").toUpperCase();
      return c.json || (!ty.includes("INT") && !ty.includes("REAL") && !ty.includes("NUM") && ty !== "BOOLEAN");
    });
    if (targets.length === 0) return t;
    return {
      ...t,
      rows: t.rows.map((row, i) => {
        if (!sel.has(i)) return row;
        let next = row;
        for (const c of targets) {
          if (!(c.name in row)) continue;
          const r = replaceInValue(row[c.name], find, repl, caseSensitive);
          if (r.n === 0) continue;
          replaced += r.n;
          if (next === row) next = { ...row };
          next[c.name] = r.v;
        }
        return next;
      }),
    };
  });
  return { doc: { ...doc, tables }, replaced };
}

/** 当前 unix 秒（Trae 的时间列是秒；字符串形态的那种也在写入时按字符串给）。 */
export function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

/** 把一行的 `created_at` / `updated_at` 置为现在（保持原值的形态：数字还是字符串）。 */
export function stampRow(row: Record<string, unknown>, cols: TraeEditColumn[], secs: number): void {
  for (const c of cols) {
    if (c.name !== "created_at" && c.name !== "updated_at") continue;
    if (typeof row[c.name] === "string") row[c.name] = String(secs);
    else row[c.name] = secs;
  }
}

// ---------------------------------------------------------------------------
// 会话式正文：把一条消息拆成「一段一段」
// ---------------------------------------------------------------------------
//
// 为什么要拆：右栏直接摊开 `content` 的 JSON 树，用户看到的是一堆 `messages[0]
// .plan_item.tool_call_info.params.blocking` —— 那是数据库视角，不是会话视角
// （用户原话：「显示的太细致了，要求跟正常会话的显示一样就行」）。
//
// 真机 82 条消息实测出来的正文构成（不是推演）：
//   · **用户提问** = `chat_message_general.content` 的**数组**，每一项 `{type, text_content}`，
//     实测 82/82 项都是 `type == "text"`。
//   · **助手回答** = `chat_message_task.content.messages[]`，每项 `{id, type:"plan_item", plan_item}`；
//     可读成分只有三种：`plan_item.thought`（实测 13 条非空）、工具调用
//     （`plan_item.tool_call_info.name`，实测 `finish` 24 / `Read` 17 / `RunCommand` 9 / `Edit` 5 …）、
//     以及 `name == "finish"` 的 `params.summary`（实测 17 非空 / 7 空）。
//   ⇒ 所以「一段话」= 一个 `thought`、或一个 `finish.summary`、或一个工具调用行。
//
// ⚠️ `chat_message_task.content` 有 **53/82 条是按引用返回的**（`$jsonRef`）。
//    按引用返回时**内容不在本地**，段列表只能给出占位，必须先展开才能编辑。

/** 正文表：`user` 挂 `chat_message_general`，`assistant` 挂 `chat_message_task`。 */
export const BODY_TABLE_OF: Record<string, string> = {
  user: "chat_message_general",
  assistant: "chat_message_task",
};

export interface BodyOf {
  table: string;
  ref: RowRef;
  row: Record<string, unknown>;
  /** `content` 列被按引用返回（`$jsonRef`）—— 要展开后才能读段落。 */
  byRef: boolean;
}

/** 一条消息的正文行（`content` 所在的那一行）。没有正文表时返回 null。 */
export function bodyOf(doc: TraeEditDoc, m: TurnMessage): BodyOf | null {
  const name = BODY_TABLE_OF[m.role];
  if (!name) return null;
  const g = m.attachments.find((x) => x.table === name);
  const ref = g?.refs[0];
  if (!ref) return null;
  const row = tableOf(doc, name)?.rows[ref.index];
  if (!row) return null;
  return { table: name, ref, row, byRef: isJsonRef(row["content"]) };
}

/** 这个值是不是「按引用返回的大字段」。 */
export function isJsonRef(v: unknown): boolean {
  if (!v || typeof v !== "object" || Array.isArray(v)) return false;
  const r = (v as Record<string, unknown>).$jsonRef;
  return !!r && typeof r === "object";
}

export type SegmentKind = "text" | "think" | "tool" | "final" | "other";

/**
 * 正文里的一段话。**每段独立**：各自带自己的 `path`，点它就能原地改。
 */
export interface Segment {
  key: string;
  kind: SegmentKind;
  /** 段的小标签（「提问」/「说明」/工具名/「答复」）。 */
  label: string;
  /** 可读正文（`think` / `final` / `text` 是纯文本；工具行是单行摘要）。 */
  text: string;
  /** 相对该行 **`content` 列的值** 的路径（`readAtPath(row.content, path)`）。 */
  path: (string | number)[];
  /** 段的原文是纯文本（可原地改）还是 JSON 结构（走结构化表单）。 */
  plain: boolean;
  /** 工具调用名（`kind === "tool"`）。 */
  tool?: string;
  /** 工具调用的 `params` 路径（`kind === "tool"`），点开改参数用。 */
  paramsPath?: (string | number)[];
}

/**
 * 每种工具「最能说明它在干什么」的那个参数。
 *
 * ⚠️ 顺序有意义：先按工具名挑，挑不到再按通用列表兜底。
 *    真机 24 种工具调用里 `Read` / `Edit` / `Write` 用的是 `file_path`，
 *    `Grep` / `Glob` 用 `pattern`，`RunCommand` 用 `command` —— 只看通用列表也能命中，
 *    但把工具的专属参数放前面，能避免某些工具恰好同时带 `path` 与更有信息量的字段时挑错。
 */
const TOOL_PARAM_PREFERENCE: Record<string, string[]> = {
  RunCommand: ["command"],
  StopCommand: ["command"],
  Read: ["file_path", "path"],
  Write: ["file_path", "path"],
  Edit: ["file_path", "path"],
  Grep: ["pattern", "query"],
  Glob: ["pattern"],
  OpenPreview: ["url", "path"],
};

/** 工具调用的一行摘要：优先取最能说明「它在干什么」的那个参数。 */
export function toolSummary(name: string, params: unknown): string {
  const p = (params && typeof params === "object" ? params : {}) as Record<string, unknown>;
  const pick = [
    ...(TOOL_PARAM_PREFERENCE[name] ?? []),
    "command", "file_path", "path", "pattern", "query", "url", "target_file", "notebook_path",
  ];
  for (const k of pick) {
    const v = p[k];
    if (typeof v === "string" && v.trim()) return v.trim();
  }
  const s = textOf(params);
  return s.length > 160 ? `${s.slice(0, 160)}…` : s;
}

/**
 * 把一条消息的正文拆成段。`row` 必须是 `bodyOf()` 给的那一行。
 *
 * ⚠️ 顺序按 Trae 自己的时间轴：**先 `thought`（说明）再动作**（工具调用 / finish）。
 *    真机 `messages[]` 里一个元素同时带 `thought` 与 `tool_call_info`，两者是同一时刻的
 *    「想」和「做」，把 thought 排在后面会让读起来像「先做了再解释」。
 */
export function segmentsOf(role: string, row: Record<string, unknown>): Segment[] {
  const content = row["content"];
  if (isJsonRef(content)) {
    return [
      {
        key: "ref",
        kind: "other",
        label: "正文未加载",
        text: "这是一个大字段（按引用返回）。点「展开正文」把它取回来，取回来之后才能改。",
        path: [],
        plain: false,
      },
    ];
  }

  if (role === "user") {
    const items = Array.isArray(content) ? content : [];
    return items.map((it, i) => {
      const o = (it && typeof it === "object" ? it : {}) as Record<string, unknown>;
      const t = str(o.type) || "text";
      if (t === "text") {
        return {
          key: `u${i}`,
          kind: "text" as SegmentKind,
          label: "提问",
          text: str(o.text_content),
          path: [i, "text_content"],
          plain: true,
        };
      }
      return {
        key: `u${i}`,
        kind: "other" as SegmentKind,
        label: t,
        text: textOf(o),
        path: [i],
        plain: false,
      };
    });
  }

  if (role !== "assistant") return [];
  const root = (content && typeof content === "object" ? content : {}) as Record<string, unknown>;
  const msgs = Array.isArray(root.messages) ? (root.messages as unknown[]) : [];
  const out: Segment[] = [];
  msgs.forEach((raw, i) => {
    const m = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
    const pi = (m.plan_item && typeof m.plan_item === "object" ? m.plan_item : {}) as Record<string, unknown>;
    const tci = (pi.tool_call_info && typeof pi.tool_call_info === "object" ? pi.tool_call_info : {}) as Record<string, unknown>;
    const name = str(tci.name);
    const base: (string | number)[] = ["messages", i, "plan_item"];

    const thought = str(pi.thought);
    if (thought.trim()) {
      out.push({
        key: `t${i}`,
        kind: "think",
        label: "说明",
        text: thought,
        path: [...base, "thought"],
        plain: true,
      });
    }

    if (name === "finish") {
      const params = (tci.params && typeof tci.params === "object" ? tci.params : {}) as Record<string, unknown>;
      out.push({
        key: `f${i}`,
        kind: "final",
        label: "答复",
        text: str(params.summary),
        path: [...base, "tool_call_info", "params", "summary"],
        plain: true,
      });
    } else if (name) {
      out.push({
        key: `c${i}`,
        kind: "tool",
        label: name,
        text: toolSummary(name, tci.params),
        path: [...base, "tool_call_info", "params"],
        paramsPath: [...base, "tool_call_info", "params"],
        plain: false,
      });
    }
  });
  return out;
}

/** 按路径取值（路径不存在 → undefined）。 */
export function readAtPath(root: unknown, path: (string | number)[]): unknown {
  let cur: unknown = root;
  for (const k of path) {
    if (cur === null || cur === undefined) return undefined;
    if (typeof k === "number") {
      if (!Array.isArray(cur)) return undefined;
      cur = cur[k];
    } else {
      if (typeof cur !== "object" || Array.isArray(cur)) return undefined;
      cur = (cur as Record<string, unknown>)[k];
    }
  }
  return cur;
}

/**
 * 按路径写值，返回**新的根**（不改原对象）。
 *
 * ⚠️ 路径上原本不存在的容器按「下一步是数字 → 数组，否则对象」补出来 ——
 *    真机上 `plan_item.thought` 可能是 `null` 或缺键，用户改这一段时要能补出来，
 *    不能因为「父级不是对象」直接放弃。
 */
export function setAtPath(root: unknown, path: (string | number)[], value: unknown): unknown {
  if (path.length === 0) return value;
  const head = path[0];
  const rest = path.slice(1);
  if (typeof head === "number") {
    const arr = Array.isArray(root) ? root.slice() : [];
    arr[head] = setAtPath(arr[head], rest, value);
    return arr;
  }
  const obj: Record<string, unknown> =
    root && typeof root === "object" && !Array.isArray(root) ? { ...(root as Record<string, unknown>) } : {};
  obj[head] = setAtPath(obj[head], rest, value);
  return obj;
}

/** 把改过的 `content` 值写回某一行的某一列，返回新的 doc。 */
export function setCell(doc: TraeEditDoc, ref: RowRef, column: string, value: unknown): TraeEditDoc {
  return {
    ...doc,
    tables: doc.tables.map((t) => {
      if (t.name !== ref.table) return t;
      const rows = t.rows.slice();
      const row = rows[ref.index];
      if (!row) return t;
      rows[ref.index] = { ...row, [column]: value };
      return { ...t, rows };
    }),
  };
}

/**
 * 按路径删掉一项（数组 → 摘掉那一项；对象 → 去掉那个键），返回新的根。
 *
 * ⚠️ 数组删空后**不补占位**：真机上 `content` / `messages` 都允许是空数组，
 *    硬塞一个空项反而会写出一条客户端看不懂的脏数据。
 */
export function removeAtPath(root: unknown, path: (string | number)[]): unknown {
  if (path.length === 0) return root;
  const parentPath = path.slice(0, -1);
  const last = path[path.length - 1];
  const parent = readAtPath(root, parentPath);
  let nextParent: unknown;
  if (typeof last === "number") {
    if (!Array.isArray(parent)) return root;
    nextParent = parent.filter((_, i) => i !== last);
  } else {
    if (!parent || typeof parent !== "object" || Array.isArray(parent)) return root;
    const o = { ...(parent as Record<string, unknown>) };
    delete o[last];
    nextParent = o;
  }
  return setAtPath(root, parentPath, nextParent);
}

/** 在某个容器末尾追加一项（容器不存在时按「叶子是对象」补出来）。 */
export function appendAtPath(root: unknown, parentPath: (string | number)[], item: unknown): unknown {
  const arr = readAtPath(root, parentPath);
  const next = Array.isArray(arr) ? [...arr, item] : [item];
  return setAtPath(root, parentPath, next);
}

/**
 * 新增一段正文时用的空项。
 *
 * ⚠️ 助手侧的空项**必须是一段 `finish`（带空 `summary`）**，不能是空对象：
 *    客户端读 `messages[]` 时按 `plan_item.tool_call_info` 分发，遇到没有
 *    `tool_call_info` 的项会当成未知类型 —— 会话在客户端里会少一段、甚至报错。
 */
export function emptySegmentItem(role: string, id: string): unknown {
  if (role === "user") {
    return { type: "text", text_content: "" };
  }
  return {
    id,
    type: "plan_item",
    plan_item: {
      thought: "",
      tool_call_info: { id, name: "finish", params: { summary: "" } },
    },
  };
}

/**
 * 「加一段」时该往哪个容器里追加。
 * 用户侧 = `content` 数组本身（`[]`）；助手侧 = `content.messages`（`["messages"]`）。
 */
export function segmentContainerPath(role: string): (string | number)[] {
  return role === "user" ? [] : ["messages"];
}
