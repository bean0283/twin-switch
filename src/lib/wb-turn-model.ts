/**
 * T46 · S4 WorkBuddy 会话编辑器的**纯函数层**。
 *
 * 这里只放「不依赖 React、不碰 IPC」的逻辑，好让单测把边界钉死。
 * 三条必须守住的约定：
 *
 * 1. **改动只回传改动过的行**。后端收到 `ops` 后在内存里对**原文**应用，
 *    没被本次改动的行走 `OutLine::Orig` ⇒ **逐字节不变**。所以这里只允许产出
 *    `replace`（改动过的原行）/ `delete`（删掉的原行）/ `insert`（新增的行）。
 * 2. **`at` 是「插到哪一行之前」**（原行号），`at === lineCount` 表示追加到末尾 ——
 *    与后端 `apply_ops` 的语义逐字对应。
 * 3. **用户提问的 `<user_query>` 壳必须护住**：正文里 `content[0].text` 的形态是
 *    `[system-reminder 块…][<user_query>真实提问</user_query>]`，界面上只让用户改里面那句话，
 *    存回去时要把壳补回来，否则客户端不认这条消息。
 */

import type { WbEditOp, WbLineKind } from "@/lib/trae-types";

/** 编辑状态（跨回合保留 —— 回合是按需分页加载的，不能只存在当前回合里）。 */
export interface WbRowEdit {
  /** 原行号 → 改动后的**整行 JSON**。 */
  replaced: Map<number, Record<string, unknown>>;
  /** 被删掉的原行号。 */
  deleted: Set<number>;
  /** 新增的行。`at` = 插到哪一行之前；`at >= lineCount` 表示追加。 */
  inserted: Array<{ key: string; at: number; json: Record<string, unknown> }>;
}

export function emptyEdit(): WbRowEdit {
  return { replaced: new Map(), deleted: new Set(), inserted: [] };
}

export function editCount(e: WbRowEdit): number {
  return e.replaced.size + e.deleted.size + e.inserted.length;
}

export function hasEdits(e: WbRowEdit): boolean {
  return editCount(e) > 0;
}

/**
 * 把 `content[0].text` 里 `<user_query>…</user_query>` 的**内层**换成新提问。
 *
 * 与后端 `workbuddy_source::extract_user_text` 同一口径：取**第一个** `<user_query>`
 * 到其后第一个 `</user_query>`（注入块只会在最前面，且只有真提问带这个壳）。
 * 找不到壳时（不该发生）退回「整段替换」，至少不丢用户的输入。
 */
export function replaceUserQuery(raw: string, next: string): string {
  const openTag = "<user_query>";
  const closeTag = "</user_query>";
  const open = raw.indexOf(openTag);
  if (open < 0) return next;
  const close = raw.indexOf(closeTag, open + openTag.length);
  if (close < 0) return next;
  return raw.slice(0, open + openTag.length) + next + raw.slice(close);
}

/** 读 `path` 处的值（`path` 为 null / 越界时返回 undefined）。 */
export function readPath(root: unknown, path: (string | number)[] | null): unknown {
  if (!path) return undefined;
  let cur: unknown = root;
  for (const p of path) {
    if (cur === null || typeof cur !== "object") return undefined;
    cur = (cur as Record<string | number, unknown>)[p];
  }
  return cur;
}

/**
 * 把 `path` 处的值改成 `value`（**写副本**，不改原对象）。
 *
 * 写成浅拷贝链：`{...}` / `[...]` 逐层复制到叶子，未经过的兄弟节点仍然是同一引用
 * ——对「只改一个字段」来说这足够了，也不会把大对象整份深拷一遍。
 */
export function writePath(
  root: unknown,
  path: (string | number)[],
  value: unknown,
): unknown {
  if (path.length === 0) return value;
  const [head, ...rest] = path;
  if (Array.isArray(root)) {
    const copy = root.slice();
    copy[head as number] = writePath(copy[head as number], rest, value);
    return copy;
  }
  if (root !== null && typeof root === "object") {
    const copy: Record<string, unknown> = { ...(root as Record<string, unknown>) };
    copy[head as string] = writePath(copy[head as string], rest, value);
    return copy;
  }
  // 路径与实际结构不符（后端分类与前端结构漂了）：不硬塞，保持原样由用户去改原始 JSON。
  return root;
}

/** 一行在右栏显示用的正文（改过的用改后的值）。 */
export function lineText(
  json: Record<string, unknown> | null,
  path: (string | number)[] | null,
  fallback: string,
): string {
  const v = readPath(json, path);
  return typeof v === "string" ? v : fallback;
}

/** 改一行里的正文（返回新的整行 JSON）。`wrapped` 时自动补回 `<user_query>` 壳。 */
export function applyText(
  json: Record<string, unknown>,
  path: (string | number)[],
  next: string,
  wrapped: boolean,
): Record<string, unknown> {
  const raw = readPath(json, path);
  const value = wrapped && typeof raw === "string" ? replaceUserQuery(raw, next) : next;
  return writePath(json, path, value) as Record<string, unknown>;
}

/**
 * 编辑状态 → 后端 `ops`。
 *
 * ⚠️ 顺序无关（后端三种操作各建各的表），但**同一个原行号不能既 replace 又 delete**
 * —— 这里由数据结构保证（`deleted` 的行不会同时留在 `replaced` 里）。
 */
export function opsFromEdit(e: WbRowEdit, lineCount: number): WbEditOp[] {
  const ops: WbEditOp[] = [];
  const del = [...e.deleted].sort((a, b) => a - b);
  if (del.length > 0) ops.push({ op: "delete", indexes: del });
  for (const index of [...e.replaced.keys()].sort((a, b) => a - b)) {
    const json = e.replaced.get(index);
    // 被删掉的行不再单独提交 replace：两边都提交会被后端判「重复修改」而整批拒写。
    if (e.deleted.has(index) || json === undefined) continue;
    ops.push({ op: "replace", index, json });
  }
  const byAt = new Map<number, Record<string, unknown>[]>();
  for (const ins of e.inserted) {
    const at = Math.max(0, Math.min(ins.at, lineCount));
    const list = byAt.get(at) ?? [];
    list.push(ins.json);
    byAt.set(at, list);
  }
  for (const at of [...byAt.keys()].sort((a, b) => a - b)) {
    for (const json of byAt.get(at) ?? []) ops.push({ op: "insert", at, json });
  }
  return ops;
}

/** 会话里出现过的所有行种类（用于「有没有用户提问」这类兜底展示）。 */
export function countKind(kinds: WbLineKind[], kind: WbLineKind): number {
  let n = 0;
  for (const k of kinds) if (k === kind) n += 1;
  return n;
}

/** 一行 JSON 属于哪一类（只区分「本次新增」要用到的那几类）。 */
export function kindOfJson(json: Record<string, unknown>): WbLineKind {
  if (json.type !== "message") return "unknown";
  return json.role === "user" ? "user" : "assistant";
}

/**
 * 改完之后还剩几条**用户提问**。
 *
 * ⚠️ 这是后端 `validate_submit` 空会话硬拦的**同一口径** —— 一条不剩就会被拒写
 * （空壳会话会让客户端永远卡在「环境创建中」）。前端必须能在**点击之前**算出这个数，
 * 才能把「删最后一个回合」改道成「删整条会话」。
 *
 * - `kinds` 是 `edit_doc` 给的行种类表（**原始行**，不随改动变化）；
 * - 被删的行（`deleted`，含整回合删除）要扣掉；本次新增的用户行要加上。
 */
export function usersAfterEdit(kinds: WbLineKind[], e: WbRowEdit): number {
  let n = 0;
  for (let i = 0; i < kinds.length; i += 1) {
    if (kinds[i] === "user" && !e.deleted.has(i)) n += 1;
  }
  for (const ins of e.inserted) if (kindOfJson(ins.json) === "user") n += 1;
  return n;
}

/**
 * 「把某个回合整段删掉」之后还剩几条用户提问。
 *
 * 与 `usersAfterEdit` 的差别：**这个回合的行还没进 `e.deleted`**，要在计算时先扣掉；
 * 另外落在这个区间里的本次新增行也会被一起删（`removeTurn` 的语义），
 * 所以它们同样不算数 —— 否则「刚新增一条提问又整回合删掉」会算出 `1`，骗过硬拦。
 */
export function usersAfterTurnRemoved(
  kinds: WbLineKind[],
  e: WbRowEdit,
  turn: { start: number; end: number },
): number {
  return usersAfterTurnsRemoved(kinds, e, [turn]);
}

/**
 * 「把**若干**个回合整段删掉」之后还剩几条用户提问（`removeTurns` 的语义）。
 *
 * 单回合版本是它的退化情形；批量删除必须走同一个口径 ——
 * 否则「一次勾 5 个回合全删光」会绕过空会话硬拦（客户端打开空壳会话永远卡在环境创建中）。
 *
 * 两个区间的方向必须与 `removeTurns` **逐字一致**：
 * - 行：`[start, end)`（左闭右开）
 * - 本次新增行：`at ∈ (start, end]`（左开右闭）—— `at === start` 是插在回合第一行**之前**，
 *   属「会话开头」那一档，不跟着走。
 */
export function usersAfterTurnsRemoved(
  kinds: WbLineKind[],
  e: WbRowEdit,
  turns: ReadonlyArray<{ start: number; end: number }>,
): number {
  if (turns.length === 0) return usersAfterEdit(kinds, e);
  const coversLine = (i: number) => turns.some((t) => i >= t.start && i < t.end);
  const coversIns = (at: number) => turns.some((t) => at > t.start && at <= t.end);
  let n = 0;
  for (let i = 0; i < kinds.length; i += 1) {
    if (kinds[i] !== "user" || e.deleted.has(i)) continue;
    if (coversLine(i)) continue;
    n += 1;
  }
  for (const ins of e.inserted) {
    if (coversIns(ins.at)) continue;
    if (kindOfJson(ins.json) === "user") n += 1;
  }
  return n;
}

/** 新 id：WorkBuddy 用的是标准 UUID v4（与客户端同构）。 */
export function newLineId(): string {
  const c = globalThis.crypto as Crypto | undefined;
  if (c && typeof c.randomUUID === "function") return c.randomUUID();
  // 兜底（老 WebView2）：手工拼 v4
  const hex = "0123456789abcdef";
  let s = "";
  for (let i = 0; i < 36; i += 1) {
    if (i === 8 || i === 13 || i === 18 || i === 23) s += "-";
    else if (i === 14) s += "4";
    else if (i === 19) s += hex[8 + Math.floor(Math.random() * 4)];
    else s += hex[Math.floor(Math.random() * 16)];
  }
  return s;
}

const USER_CONTENT_TYPE = "input_text";
const ASSISTANT_CONTENT_TYPE = "output_text";

/**
 * 新增一条**用户提问**行。
 *
 * ⚠️ 必须带 `sessionId`（客户端靠它筛选本会话的事件）与 `<user_query>` 壳，
 * 否则客户端不认这条提问 —— 后端全量校验会直接拒写。
 */
export function newUserLine(
  sessionId: string,
  cwd: string,
  text: string,
  now: number,
): Record<string, unknown> {
  return {
    id: newLineId(),
    timestamp: now,
    type: "message",
    role: "user",
    content: [{ type: USER_CONTENT_TYPE, text: `<user_query>${text}</user_query>` }],
    providerData: { startsNewUserRequest: true, agent: "cli" },
    sessionId,
    cwd,
  };
}

/** 新增一条**助手回答**行。 */
export function newAssistantLine(
  sessionId: string,
  cwd: string,
  text: string,
  now: number,
): Record<string, unknown> {
  return {
    id: newLineId(),
    timestamp: now,
    type: "message",
    role: "assistant",
    status: "completed",
    content: [{ type: ASSISTANT_CONTENT_TYPE, text }],
    sessionId,
    cwd,
  };
}

/** 一行在列表里的单行摘要（工具卡片收起态用）。 */
export function oneLine(text: string, max = 120): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}

/** 回合之外的「散落行」分组：把连续行号切成区间，供左栏展示。 */
export function ranges(indexes: number[]): Array<{ start: number; end: number }> {
  const sorted = [...indexes].sort((a, b) => a - b);
  const out: Array<{ start: number; end: number }> = [];
  for (const i of sorted) {
    const last = out[out.length - 1];
    if (last && i === last.end) last.end = i + 1;
    else out.push({ start: i, end: i + 1 });
  }
  return out;
}
