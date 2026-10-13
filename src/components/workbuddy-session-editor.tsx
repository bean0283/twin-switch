import * as React from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
  Code2,
  Layers,
  ListChecks,
  Loader2,
  MessageSquare,
  RefreshCw,
  RotateCcw,
  Save,
  Search,
  Sparkles,
  Trash2,
} from "lucide-react";

import { RowForm } from "@/components/json-form";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ResizableDialogContent } from "@/components/ui/resizable-dialog-content";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import * as api from "@/lib/api";
import type {
  CrossWriteAccount,
  CrossWriteTarget,
  WbEditDoc,
  WbEditLine,
  WbEditReport,
  WbEditTargetAccount,
  WbEditTurn,
  WbLineKind,
} from "@/lib/trae-types";
import { crossKeyOf } from "@/lib/trae-types";
import {
  applyText,
  editCount,
  emptyEdit,
  hasEdits,
  kindOfJson as kindOfNew,
  lineText,
  newAssistantLine,
  newUserLine,
  oneLine,
  opsFromEdit,
  usersAfterEdit,
  usersAfterTurnsRemoved,
  type WbRowEdit,
} from "@/lib/wb-turn-model";
import { cn } from "@/lib/utils";

/** 与后端 `MAX_LINES_PER_FETCH` 一致：一次最多取 400 行。 */
const FETCH_CHUNK = 400;
/** 校验防抖。 */
const VALIDATE_DEBOUNCE_MS = 400;
/** 只读展示时一段正文最多画多少字（超出的给「显示全部」）。 */
const READ_CAP = 6000;
/** 工具卡片收起态的一行摘要长度。 */
const SUMMARY_CAP = 140;

const SAME_ACCOUNT = "__same__";

/** 右栏一行（可能是原文行，也可能是本次新增的行）。 */
interface DisplayRow {
  key: string;
  /** 原行号；`null` = 本次新增。 */
  index: number | null;
  kind: WbLineKind;
  label: string;
  name: string;
  status: string;
  timestamp: number;
  bytes: number;
  textPath: (string | number)[] | null;
  wrapped: boolean;
  /** 当前值（改过就是改后的）。 */
  json: Record<string, unknown> | null;
  /** 原始可读正文（未改动的原文）。 */
  fallbackText: string;
  /** 是否被本次改动过。 */
  dirty: boolean;
}

/** 工具类行默认收起（正文可能 100 KB，全摊开会把 DOM 压垮）。 */
function defaultCollapsed(kind: WbLineKind): boolean {
  return kind === "tool_call" || kind === "tool_result" || kind === "meta";
}

const KIND_TONE: Record<string, string> = {
  user: "border-l-2 border-l-sky-500/70",
  auto: "border-l-2 border-l-sky-500/30",
  summary: "border-l-2 border-l-muted-foreground/30",
  assistant: "border-l-2 border-l-emerald-500/70",
  reasoning: "border-l-2 border-l-amber-500/60",
  tool_call: "",
  tool_result: "",
  meta: "",
};

export function WorkbuddySessionEditor({
  open,
  onOpenChange,
  uid,
  sessionId,
  cross,
  onDeleted,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  /** 归属账号 uid。 */
  uid: string | null;
  /** 要编辑的会话 id。 */
  sessionId: string | null;
  /**
   * 这条会话被**彻底删除**之后通知外层（列表要重读、编辑器该关掉）。
   *
   * 只有一条路会走到它：把会话里**最后一条用户提问**所在的回合一并删掉 ——
   * 那等于造一个空壳会话（客户端打开后永远卡在「环境创建中」），所以这里
   * 一律**改道成删整条会话**，与本仓既有的硬规则一致。
   */
  onDeleted?: () => void;
  /**
   * T46 · S5 · 数据迁移页专用：**跨客户端**写入。
   *
   * 给了它，页脚就不再是「同客户端写入方式 / 写入到账号」，而是「写入到 <对面客户端> 账号」——
   * 因为「WorkBuddy → Trae」这个页面要的就是把这条 WorkBuddy 会话搬到 Trae 去。
   * 不给（会话记录页）就是原来的同账号 / 跨账号写入。
   */
  cross?: CrossWriteTarget | null;
}) {
  const [doc, setDoc] = React.useState<WbEditDoc | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [loadError, setLoadError] = React.useState<string | null>(null);

  /** 当前右栏看的是哪一块：会话开头 / 第 N 个回合。 */
  const [sel, setSel] = React.useState<{ kind: "head" } | { kind: "turn"; index: number } | null>(
    null,
  );
  const [fetched, setFetched] = React.useState<WbEditLine[] | null>(null);
  const [rowsLoading, setRowsLoading] = React.useState(false);
  const [rowsError, setRowsError] = React.useState<string | null>(null);

  const [edit, setEdit] = React.useState<WbRowEdit>(() => emptyEdit());
  const [rowDraft, setRowDraft] = React.useState<Record<string, unknown> | null>(null);

  const [mode, setMode] = React.useState<"overwrite" | "insert">("overwrite");
  const [targetUid, setTargetUid] = React.useState("");
  const [targets, setTargets] = React.useState<WbEditTargetAccount[]>([]);
  const [targetsError, setTargetsError] = React.useState<string | null>(null);

  /**
   * 跨客户端写入（数据迁移页）选中的目标账号 —— 存的是**复合键** `clientKey::uid`。
   *
   * ⚠️ 不能只存 uid：同一个 uid 在 Trae CN 与 TRAE SOLO CN 下各有一份，
   * 只按 uid 反查会命中另一个客户端的同名账号（写错库）。
   */
  const [crossKey, setCrossKey] = React.useState("");

  const [report, setReport] = React.useState<WbEditReport | null>(null);
  const [checking, setChecking] = React.useState(false);
  const [saving, setSaving] = React.useState(false);
  const [saveLog, setSaveLog] = React.useState<string[]>([]);

  /** 展开的行 key（工具 / 元信息卡片）与「原始 JSON」开关。 */
  const [expanded, setExpanded] = React.useState<Set<string>>(new Set());
  const [rawOpen, setRawOpen] = React.useState<Set<string>>(new Set());
  const [rawDraft, setRawDraft] = React.useState<Record<string, string>>({});
  const [rawError, setRawError] = React.useState<Record<string, string>>({});

  /** 正在就地编辑正文的那一行 + 草稿。 */
  const [editing, setEditing] = React.useState<{ key: string; text: string } | null>(null);
  const [showAll, setShowAll] = React.useState<Set<string>>(new Set());

  const [headOpen, setHeadOpen] = React.useState(false);
  const [turnFilter, setTurnFilter] = React.useState("");
  /**
   * 被**整回合删掉**的原回合下标。
   *
   * ⚠️ 行号层面这些行已经进了 `edit.deleted`，这里只是让左栏不再列出它、
   * 并在选中项被删时把选择挪走。**回合下标始终是「原始下标」**——
   * 与 `ops` 的行号同一套坐标系（新增行不动别人的行号），所以删掉几个回合
   * 之后剩下的回合下标**不需要重排**。
   */
  const [removedTurns, setRemovedTurns] = React.useState<Set<number>>(new Set());
  /**
   * 批量删除回合：是否处于多选态，以及勾了哪些回合（**原始下标**）。
   *
   * 与 `sel`（右栏在看哪个回合）是两件事：`sel` 只有一个，`multiSel` 可以有一堆。
   * 多选态下点整行 = 切换勾选（同时把右栏切过去，边勾边看），删除按钮仍然逐个可用。
   */
  const [multi, setMulti] = React.useState(false);
  const [multiSel, setMultiSel] = React.useState<Set<number>>(new Set());
  const [pendingConfirm, setPendingConfirm] = React.useState<{
    title: string;
    body: React.ReactNode;
    run: () => void;
  } | null>(null);

  // -------------------------------------------------------------------------
  // 打开 / 关闭
  // -------------------------------------------------------------------------
  React.useEffect(() => {
    if (!open) {
      setFetched(null);
      setSel(null);
      setExpanded(new Set());
      setRawOpen(new Set());
      setEditing(null);
      setSaveLog([]);
      setPendingConfirm(null);
      setShowAll(new Set());
      setTurnFilter("");
      setRemovedTurns(new Set());
      setMulti(false);
      setMultiSel(new Set());
    }
  }, [open]);

  const loadDoc = React.useCallback(async (id: string, who: string) => {
    setLoading(true);
    setLoadError(null);
    setDoc(null);
    setFetched(null);
    setEdit(emptyEdit());
    setRowDraft(null);
    setReport(null);
    setSel(null);
    setRemovedTurns(new Set());
    try {
      const d = await api.workbuddySessionEditDoc(who, id);
      setDoc(d);
      setRowDraft({ ...d.row });
      setSel(d.turns.length > 0 ? { kind: "turn", index: 0 } : { kind: "head" });
    } catch (e) {
      // ⚠️「读取失败」与「没有数据」必须长得不一样：保留错误态，不摆空态卡。
      setLoadError(api.asError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  React.useEffect(() => {
    if (open && uid && sessionId) void loadDoc(sessionId, uid);
  }, [open, uid, sessionId, loadDoc]);

  // 目标账号清单（只读）。打开时拉一次；失败**只提示不挡路** —— 同账号写入不依赖它。
  // ⚠️ 跨客户端模式（数据迁移页）下**不拉**：那份清单是 WorkBuddy 自己的账号，放在
  //    「WorkBuddy → Trae」页上只会让人以为要写回 WorkBuddy（用户报的就是这个）。
  React.useEffect(() => {
    if (!open || cross) return;
    let cancelled = false;
    setTargetsError(null);
    void (async () => {
      try {
        const r = await api.workbuddySessionEditTargets();
        if (cancelled) return;
        if (r.ok) setTargets(r.accounts);
        else setTargetsError(r.error ?? "账号清单读取失败");
      } catch (e) {
        if (!cancelled) setTargetsError(api.asError(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, cross]);

  // 跨客户端模式的默认目标账号：优先用页面给的（页面上已经选好的那个），
  // 其次第一个候选。列表变了（账号被删 / 换了客户端）就重挑，不留悬空键。
  React.useEffect(() => {
    if (!open || !cross) return;
    const list = cross.accounts;
    setCrossKey((prev) => {
      if (prev && list.some((a) => crossKeyOf(a) === prev)) return prev;
      const want = cross.defaultKey ?? "";
      if (want && list.some((a) => crossKeyOf(a) === want)) return want;
      return list[0] ? crossKeyOf(list[0]) : "";
    });
  }, [open, cross]);

  /**
   * 目标账号按**客户端**分组（Trae 侧同一台机器上可能装了国内版与国际版）。
   *
   * 顺序 = `accounts` 里第一次出现的顺序，不重排 —— 与页面上客户端的排序保持一致。
   */
  const crossGroups = React.useMemo(() => {
    const out: Array<{ key: string; label: string; accounts: CrossWriteAccount[] }> = [];
    for (const a of cross?.accounts ?? []) {
      const hit = out.find((g) => g.key === a.clientKey);
      if (hit) hit.accounts.push(a);
      else out.push({ key: a.clientKey, label: a.clientLabel || a.clientKey, accounts: [a] });
    }
    return out;
  }, [cross]);

  /** 选中的目标账号（先按复合键定位，`clientKey` 由它带出来）。 */
  const crossAccount = React.useMemo(
    () => cross?.accounts.find((a) => crossKeyOf(a) === crossKey) ?? null,
    [cross, crossKey],
  );

  const crossClient = crossAccount?.clientKey ?? crossGroups[0]?.key ?? "";

  /** 当前客户端下的账号（下拉里**只列这一个客户端**的，不再混着列）。 */
  const crossClientAccounts = React.useMemo(
    () => crossGroups.find((g) => g.key === crossClient)?.accounts ?? [],
    [crossGroups, crossClient],
  );

  /** 切客户端：落到该客户端下第一个账号上。 */
  const pickCrossClient = (key: string) => {
    const first = crossGroups.find((g) => g.key === key)?.accounts[0];
    if (first) setCrossKey(crossKeyOf(first));
  };

  // -------------------------------------------------------------------------
  // 当前区间 / 拉完整行
  // -------------------------------------------------------------------------
  const range = React.useMemo(() => {
    if (!doc || !sel) return null;
    if (sel.kind === "head") {
      const first = doc.turns[0]?.start ?? doc.body.lineCount;
      return { from: 0, to: first };
    }
    const t = doc.turns[sel.index];
    return t ? { from: t.start, to: t.end } : null;
  }, [doc, sel]);

  const [reloadTick, setReloadTick] = React.useState(0);

  React.useEffect(() => {
    if (!doc || !uid || !range) {
      setFetched(null);
      return;
    }
    let cancelled = false;
    setRowsLoading(true);
    setRowsError(null);
    void (async () => {
      try {
        const out: WbEditLine[] = [];
        for (let from = range.from; from < range.to; from += FETCH_CHUNK) {
          const to = Math.min(from + FETCH_CHUNK, range.to);
          const r = await api.workbuddySessionEditLines(uid, doc.sessionId, from, to);
          out.push(...r.lines);
          if (cancelled) return;
        }
        if (!cancelled) setFetched(out);
      } catch (e) {
        if (!cancelled) {
          setRowsError(api.asError(e));
          setFetched(null);
        }
      } finally {
        if (!cancelled) setRowsLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [doc, uid, range, reloadTick]);

  // -------------------------------------------------------------------------
  // 展示行（合并本次改动）
  // -------------------------------------------------------------------------
  const rows: DisplayRow[] = React.useMemo(() => {
    if (!range || !fetched) return [];
    const lineOf = new Map<number, WbEditLine>();
    for (const l of fetched) lineOf.set(l.index, l);
    const insByAt = new Map<number, { key: string; json: Record<string, unknown> }[]>();
    for (const ins of edit.inserted) {
      if (ins.at < range.from || ins.at > range.to) continue;
      const list = insByAt.get(ins.at) ?? [];
      list.push({ key: ins.key, json: ins.json });
      insByAt.set(ins.at, list);
    }
    const out: DisplayRow[] = [];
    for (let i = range.from; i <= range.to; i += 1) {
      for (const ins of insByAt.get(i) ?? []) {
        out.push({
          key: ins.key,
          index: null,
          kind: kindOfNew(ins.json),
          label: kindOfNew(ins.json) === "user" ? "用户提问" : "助手回答",
          name: "",
          status: "",
          timestamp: 0,
          bytes: 0,
          textPath: ["content", 0, "text"],
          wrapped: kindOfNew(ins.json) === "user",
          json: ins.json,
          fallbackText: "",
          dirty: true,
        });
      }
      if (i === range.to) break;
      if (edit.deleted.has(i)) continue;
      const l = lineOf.get(i);
      if (!l) continue;
      const json = edit.replaced.get(i) ?? l.json;
      out.push({
        key: `o${i}`,
        index: i,
        kind: l.kind,
        label: l.label,
        name: l.name,
        status: l.status,
        timestamp: l.timestamp,
        bytes: l.bytes,
        textPath: l.textPath,
        wrapped: l.wrapped,
        json,
        fallbackText: l.text,
        dirty: edit.replaced.has(i),
      });
    }
    return out;
  }, [range, fetched, edit]);

  // -------------------------------------------------------------------------
  // 元数据改动
  // -------------------------------------------------------------------------
  const rowPatch: Record<string, unknown> = React.useMemo(() => {
    if (!doc || !rowDraft) return {};
    const out: Record<string, unknown> = {};
    for (const c of doc.columns) {
      if (c.readOnly) continue;
      const a = doc.row[c.name];
      const b = rowDraft[c.name];
      if (JSON.stringify(a ?? null) !== JSON.stringify(b ?? null)) out[c.name] = b ?? null;
    }
    return out;
  }, [doc, rowDraft]);

  const ops = React.useMemo(
    () => opsFromEdit(edit, doc?.body.lineCount ?? 0),
    [edit, doc?.body.lineCount],
  );
  const changed = hasEdits(edit) || Object.keys(rowPatch).length > 0;

  // -------------------------------------------------------------------------
  // 校验（后端唯一实现，防抖）
  // -------------------------------------------------------------------------
  const summary = React.useMemo(
    () =>
      doc
        ? { lineCount: doc.body.lineCount, counts: doc.counts, lineKinds: doc.lineKinds }
        : null,
    [doc],
  );

  React.useEffect(() => {
    if (!open || !doc || !summary || !sessionId) return;
    let cancelled = false;
    setChecking(true);
    const timer = window.setTimeout(() => {
      void (async () => {
        try {
          const r = await api.workbuddySessionEditValidate(
            ops,
            rowPatch,
            doc.columns,
            summary,
            doc.sessionId,
          );
          if (!cancelled) setReport(r);
        } catch (e) {
          // 校验失败不能假装「校验通过」：明确写成读取失败。
          if (!cancelled)
            setReport({
              errors: [`校验读取失败（不是数据有问题）：${api.asError(e)}`],
              warnings: [],
              errorCount: 1,
              warnCount: 0,
              stats: {},
            });
        } finally {
          if (!cancelled) setChecking(false);
        }
      })();
    }, VALIDATE_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, doc, summary, sessionId, JSON.stringify(ops), JSON.stringify(rowPatch)]);

  const errorCount = report?.errorCount ?? 0;
  const warnCount = report?.warnCount ?? 0;

  // -------------------------------------------------------------------------
  // 行操作
  // -------------------------------------------------------------------------
  const setLineJson = (row: DisplayRow, json: Record<string, unknown>) => {
    setEdit((prev) => {
      const next: WbRowEdit = {
        replaced: new Map(prev.replaced),
        deleted: new Set(prev.deleted),
        inserted: prev.inserted.map((i) => ({ ...i })),
      };
      if (row.index === null) {
        const at = prev.inserted.findIndex((i) => i.key === row.key);
        if (at >= 0) next.inserted[at] = { ...next.inserted[at], json };
      } else {
        next.replaced.set(row.index, json);
      }
      return next;
    });
  };

  const removeRow = (row: DisplayRow) => {
    setEdit((prev) => {
      const next: WbRowEdit = {
        replaced: new Map(prev.replaced),
        deleted: new Set(prev.deleted),
        inserted: prev.inserted.map((i) => ({ ...i })),
      };
      if (row.index === null) {
        next.inserted = next.inserted.filter((i) => i.key !== row.key);
      } else {
        next.deleted.add(row.index);
        next.replaced.delete(row.index);
      }
      return next;
    });
    setEditing(null);
  };

  const addLine = (kind: "user" | "assistant") => {
    if (!doc || !range) return;
    const cwd = String(doc.row.cwd ?? "");
    const now = Date.now();
    const json =
      kind === "user"
        ? newUserLine(doc.sessionId, cwd, "", now)
        : newAssistantLine(doc.sessionId, cwd, "", now);
    const key = `n${now}-${Math.random().toString(36).slice(2, 8)}`;
    setEdit((prev) => ({
      replaced: prev.replaced,
      deleted: prev.deleted,
      inserted: [...prev.inserted, { key, at: range.to, json }],
    }));
    setExpanded((prev) => new Set(prev).add(key));
    setEditing({ key, text: "" });
  };

  // -------------------------------------------------------------------------
  // 删除回合
  // -------------------------------------------------------------------------
  /**
   * 真正把**若干**个回合从本次编辑里移除：每个回合名下的**每一行**都进 `edit.deleted`。
   *
   * ⚠️ 行号用的是**原始行号**，所以删掉几个回合之后，其余回合的区间一个数都不用改 ——
   * 这正是 `ops` 模型（未改动行逐字节原样带回）的前提，别在这里做任何重排。
   * ⚠️ 批量删除**不是**「循环调用单个删除」：一次算完所有区间再落 state，
   * 否则中途的 `removedTurns` 闭包会把后面的判断带偏。
   */
  const removeTurns = (indexes: number[]) => {
    if (!doc || indexes.length === 0) return;
    const gone = new Set(indexes);
    const picks = indexes.map((i) => doc.turns[i]).filter((t): t is WbEditTurn => Boolean(t));
    if (picks.length === 0) return;
    setEdit((prev) => {
      const next: WbRowEdit = {
        replaced: new Map(prev.replaced),
        deleted: new Set(prev.deleted),
        inserted: prev.inserted.map((i) => ({ ...i })),
      };
      for (const t of picks) {
        for (let i = t.start; i < t.end; i += 1) {
          next.deleted.add(i);
          next.replaced.delete(i);
        }
      }
      // 本次新增的行 `at` = 「插到哪一行之前」，落在**任一**被删回合区间里的（含插在它末尾的）
      // 要一起走，否则会像幽灵一样挂到下一个回合的开头。
      // ⚠️ 左端开区间：`at === t.start` 是**插在回合第一行之前**（「会话开头」那一档加的行
      //    就是插在 `turns[0].start`），它属于 head，不该被这个回合带走。
      next.inserted = next.inserted.filter(
        (ins) => !picks.some((t) => ins.at > t.start && ins.at <= t.end),
      );
      return next;
    });
    setRemovedTurns((prev) => {
      const n = new Set(prev);
      for (const i of indexes) n.add(i);
      return n;
    });
    // 勾过的回合已经没了 ⇒ 从多选集合里剔掉，否则「删除选中」会带着幽灵下标。
    setMultiSel((prev) => {
      const n = new Set(prev);
      for (const i of indexes) n.delete(i);
      return n;
    });
    setEditing(null);
    // 选中的被删了 ⇒ 往后找第一个还在的回合，没有就往回找，再没有就回「会话开头」。
    setSel((prev) => {
      if (!(prev && prev.kind === "turn" && gone.has(prev.index))) return prev;
      const rest = doc.turns.filter((x) => !gone.has(x.index) && !removedTurns.has(x.index));
      const after =
        rest.find((x) => x.index > prev.index) ??
        [...rest].reverse().find((x) => x.index < prev.index);
      return after ? { kind: "turn", index: after.index } : { kind: "head" };
    });
  };

  /** 这条会话还剩几条用户提问（**把本次改动算进去**）。 */
  const usersLeftIfTurnsRemoved = (indexes: number[]) => {
    if (!doc || indexes.length === 0) return userCount;
    const picks = indexes.map((i) => doc.turns[i]).filter(Boolean);
    if (picks.length === 0) return userCount;
    return usersAfterTurnsRemoved(doc.lineKinds, edit, picks);
  };

  /**
   * 删掉整条会话（不是删回合）。
   *
   * ⚠️ 只有一条路会走到这里：把**最后一条用户提问**所在的回合也删掉。
   * 空壳会话是客户端的死穴（打开后永远卡在「环境创建中」，发不出任何消息），
   * 所以本仓的硬规则是「不给删到空」—— 与 Trae 侧那套 `askDeleteWholeSession` 同源。
   */
  const deleteWholeSession = async () => {
    if (!doc || !uid) return;
    setSaving(true);
    const t = toast.loading("正在彻底删除这条会话…");
    try {
      const r = await api.workbuddySessionDelete(uid, [doc.sessionId]);
      toast.dismiss(t);
      toast.success(`已删除 ${r.count} 条会话`, {
        description: `正文已移入回收站，库备份：${r.backup}`,
      });
      onOpenChange(false);
      onDeleted?.();
    } catch (e) {
      toast.dismiss(t);
      toast.error("删除失败", { description: api.asError(e).split("\n")[0] });
    } finally {
      setSaving(false);
    }
  };

  /**
   * 删除回合前的确认（单回合与多选共用一条）。
   *
   * ⚠️ 空会话硬拦在这里**必须按「一次要删的全部回合」算**：多选时逐个算都 > 0、
   * 合起来删完归 0 的情况完全可能（例如把剩下的提问全勾上）——
   * 那样就会绕过硬拦、写出客户端打不开的空壳会话。
   */
  const askDeleteTurns = (indexes: number[]) => {
    if (!doc) return;
    const picks = indexes
      .slice()
      .sort((a, b) => a - b)
      .map((i) => doc.turns[i])
      .filter((t): t is WbEditTurn => Boolean(t));
    if (picks.length === 0) return;
    const totalLines = picks.reduce((s, t) => s + (t.end - t.start), 0);
    if (usersLeftIfTurnsRemoved(picks.map((t) => t.index)) === 0) {
      setPendingConfirm({
        title: "不能把会话删空 —— 改成删掉整条会话？",
        body: (
          <div className="space-y-1.5 text-xs">
            <p>
              要删的{picks.length > 1 ? `${picks.length} 个回合` : `回合 ${picks[0].index + 1}`}
              里有这条会话**最后一条用户提问**。删完之后，会话里就一条用户提问都没有了。
            </p>
            <p>
              这种「空壳会话」客户端打开会一直卡在「环境创建中」，<b>再也发不出任何消息</b>
              （客户端没有自愈路径，只能整条删掉）。
            </p>
            <p className="text-muted-foreground">
              所以这里不提供「删到空」：确认后走「彻底删除这条会话」—— 整库备份 → 删除该会话的全部数据行 →
              正文文件移入回收站。<b>编辑器里未保存的改动会一并丢弃。</b>
            </p>
          </div>
        ),
        run: () => void deleteWholeSession(),
      });
      return;
    }
    setPendingConfirm({
      title:
        picks.length === 1 ? `删除回合 ${picks[0].index + 1}？` : `删除选中的 ${picks.length} 个回合？`,
      body: (
        <div className="space-y-1 text-xs">
          <p>
            会把
            {picks.length === 1
              ? `该回合名下的 ${totalLines} 行（第 ${picks[0].start}–${picks[0].end - 1} 行）`
              : `这 ${picks.length} 个回合名下的 ${totalLines} 行`}
            一起从本次编辑里移除，含用户提问、助手回答、思考与工具调用记录。
          </p>
          <p className="text-muted-foreground">
            这只是**改这份编辑结果**，不会立刻动库 —— 但写入之后就是真的删掉了。写入前仍可「撤销改动」。
          </p>
          {/* 待删回合的提问预览。
              ⚠️ 用 `break-all` **自适应换行**，不用 `truncate`：AlertDialog 是 grid + auto 列，
                  子元素默认 `min-width:auto`，`nowrap` 的长行会把整个弹窗横向撑破 ——
                  实测「提问里带一串 `@image#1:…` 附件名」时，文字直接越过卡片右边界。
                  换行 + 上面的 `min-w-0` 一起用，长内容才会老实待在卡片里。 */}
          <div className="max-h-24 space-y-0.5 overflow-auto font-mono text-[10px]">
            {picks.map((t) => (
              <div key={t.index} className="break-all whitespace-pre-wrap">
                #{t.index + 1} {t.userText || "（空提问）"}
              </div>
            ))}
          </div>
        </div>
      ),
      run: () => removeTurns(picks.map((t) => t.index)),
    });
  };

  /** 多选态：切换某个回合的勾选（顺带把右栏切过去，边勾边看）。 */
  const toggleTurn = (index: number) => {
    setMultiSel((prev) => {
      const n = new Set(prev);
      if (n.has(index)) n.delete(index);
      else n.add(index);
      return n;
    });
    setSel({ kind: "turn", index });
  };

  const undoAll = () => {
    setEdit(emptyEdit());
    setRowDraft(doc ? { ...doc.row } : null);
    setEditing(null);
    setRawOpen(new Set());
    setRawDraft({});
    setRemovedTurns(new Set());
    setMultiSel(new Set());
  };

  // -------------------------------------------------------------------------
  // 写入
  // -------------------------------------------------------------------------
  /**
   * 写回**源**会话（`overwrite` / `insert`）。返回是否成功。
   *
   * 跨客户端搬迁也复用它，只是固定按 `overwrite` 走 —— 导入链路读的是 WorkBuddy 库里的内容，
   * 绕不过去，必须先把改动落到库里。
   */
  const saveToSource = async (m: "overwrite" | "insert"): Promise<boolean> => {
    if (!doc || !uid) return false;
    setSaving(true);
    setSaveLog([`写入方式：${m === "overwrite" ? "就地改写" : "新增会话"}`,
      `改动：${editCount(edit)} 处行改动 / ${Object.keys(rowPatch).length} 个字段`,
    ]);
    try {
      const r = await api.workbuddySessionEditSave(
        uid,
        doc.sessionId,
        m,
        ops,
        rowPatch,
        m === "insert" && targetUid !== "" ? targetUid : undefined,
      );
      if (!r.ok) {
        // ⚠️ 写入前的**全量校验**没过：这是正常返回（不是异常），明细必须原样展示。
        setReport({
          errors: r.errors ?? ["写入被拒（后端未给出原因）"],
          warnings: r.warnings ?? [],
          errorCount: (r.errors ?? []).length || 1,
          warnCount: (r.warnings ?? []).length,
          stats: {},
        });
        setSaveLog((prev) => [...prev, r.stage ?? "校验未通过，未做任何改动"]);
        toast.error("没有写入：校验未通过", { description: r.stage ?? "见下方明细" });
        return false;
      }
      setSaveLog((prev) => [
        ...prev,
        `共写入 ${r.lines ?? 0} 行`,
        `正文本体：${r.bodyPath ?? ""}`,
        `正文备份：${r.bodyBackup ?? ""}`,
        `库备份：${r.dbBackup ?? ""}`,
      ]);
      for (const w of r.warnings ?? []) setSaveLog((prev) => [...prev, `提醒：${w}`]);
      toast.success(m === "overwrite" ? "已改写这条会话" : "已新增一条会话", {
        description: `会话 id ${r.sessionId ?? ""}`,
      });
      if (m === "insert" && r.sessionId) {
        await loadDoc(r.sessionId, r.ownerUid ?? uid);
      } else {
        await loadDoc(doc.sessionId, uid);
      }
      return true;
    } catch (e) {
      const msg = api.asError(e);
      setSaveLog((prev) => [...prev, msg]);
      toast.error("写入失败", { description: msg.split("\n")[0] });
      return false;
    } finally {
      setSaving(false);
    }
  };

  /**
   * 跨客户端搬迁（数据迁移页）：① 有改动先写回源会话 ② 再走既有导入链路搬到目标账号。
   *
   * ⚠️ 顺序不能反：② 读的是 WorkBuddy 库里的内容，不先落 ① 就等于把**没改过的**版本搬过去。
   */
  const pushCross = async () => {
    if (!doc || !uid || !cross) return;
    // ⚠️ 按**复合键**取账号：同一 uid 在两个客户端下各有一份，只比 uid 会挑错客户端。
    const acc = crossAccount;
    if (!acc) {
      toast.error(`请先选择要写入的 ${cross.toLabel} 账号`);
      return;
    }
    setSaving(true);
    setReport(null);
    setSaveLog([`目标：${cross.toLabel} · ${acc.clientLabel} · ${acc.label}`]);
    try {
      if (hasEdits(edit) || Object.keys(rowPatch).length > 0) {
        setSaveLog((prev) => [...prev, "① 先把改动写回这条 WorkBuddy 会话（导入读的是库里的内容）…"]);
        const ok = await saveToSource("overwrite");
        if (!ok) {
          setSaveLog((prev) => [...prev, "改动没写成功，已中止搬迁（源会话保持原样）"]);
          return;
        }
      } else {
        setSaveLog((prev) => [...prev, "① 没有任何改动，直接用库里的内容搬迁"]);
      }
      setSaveLog((prev) => [...prev, `② 正在导入到 ${cross.toLabel} · ${acc.clientLabel} · ${acc.label} …`]);
      const msg = await cross.push(acc);
      setSaveLog((prev) => [...prev, `② ${msg}`]);
      toast.success(`已写入到 ${acc.clientLabel} · ${acc.label}`, { description: msg });
      await loadDoc(doc.sessionId, uid);
    } catch (e) {
      const m = api.asError(e);
      setSaveLog((prev) => [...prev, `搬迁失败：${m}`]);
      toast.error(`${cross.toLabel} 写入失败`, { description: m.split("\n")[0] });
    } finally {
      setSaving(false);
    }
  };

  const save = async () => {
    if (cross) return pushCross();
    return saveToSource(mode);
  };

  // -------------------------------------------------------------------------
  // 渲染
  // -------------------------------------------------------------------------
  const turns = doc?.turns ?? [];
  /** 被整回合删掉的不再列出（下标仍是原始下标，别重排）。 */
  const liveTurns = React.useMemo(
    () => turns.filter((t) => !removedTurns.has(t.index)),
    [turns, removedTurns],
  );
  const filteredTurns = React.useMemo(() => {
    const q = turnFilter.trim().toLowerCase();
    if (!q) return liveTurns;
    return liveTurns.filter((t) => t.userText.toLowerCase().includes(q));
  }, [liveTurns, turnFilter]);

  /**
   * 还剩几条**用户提问**（= 后端空会话硬拦的同一口径）。
   *
   * ⚠️ 与 `doc.counts.user` 不同：这里要把本次改动算进去 ——
   * 行级删除（`edit.deleted`）与被删回合的行都要扣掉，新增的用户行要加上。
   * 后端 `validate_submit` 是不是拦，前端必须**先于点击**能看出来。
   */
  const userCount = React.useMemo(
    () => (doc ? usersAfterEdit(doc.lineKinds, edit) : 0),
    [doc, edit],
  );
  // ⚠️ 跨客户端模式（数据迁移页）下**原样搬迁是合法操作**（一个字段都没改也要能搬），
  //    所以判据不是「有没有改动」，而是「文档读出来了且选好了目标账号」。
  const pendingWrite = cross
    ? doc !== null && crossKey !== "" && !cross.blocked
    : changed || (mode === "insert" && targetUid !== "");

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <ResizableDialogContent storageKey="wb-session-edit" className="sm:max-w-6xl">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Layers className="size-4" />
              会话编辑器
              <Badge variant="secondary" className="text-[10px]">
                WorkBuddy
              </Badge>
              {/* ⚠️ 判据是 `changed` 而不是 `pendingWrite`：跨客户端模式下「原样搬迁」也待写入，
                  但那不代表有改动，用 pendingWrite 会让徽标一直亮着说假话。 */}
              {changed && (
                <Badge variant="outline" className="text-[10px]">
                  有未保存改动
                </Badge>
              )}
              <span className="ml-auto flex items-center gap-3 text-xs font-normal">
                {checking ? (
                  <span className="text-muted-foreground flex items-center gap-1">
                    <Loader2 className="size-3 animate-spin" /> 校验中
                  </span>
                ) : errorCount > 0 ? (
                  <span className="text-destructive flex items-center gap-1">
                    <AlertTriangle className="size-3" /> {errorCount} 处必须修
                  </span>
                ) : (
                  <span className="text-muted-foreground flex items-center gap-1">
                    <Check className="size-3" /> 校验通过
                  </span>
                )}
                {warnCount > 0 && (
                  <span className="text-amber-600 dark:text-amber-400">{warnCount} 处提醒</span>
                )}
              </span>
            </DialogTitle>
            <DialogDescription className="break-all font-mono text-xs">
              {doc?.sessionId ?? "—"}
              {doc?.ownerLabel ? ` · 归属 ${doc.ownerLabel}` : ""}
              {doc ? ` · ${turns.length} 个回合 / ${doc.body.lineCount} 行` : ""}
            </DialogDescription>
          </DialogHeader>

          {/* 两栏。⚠️ 两栏都要 min-w-0，否则长内容会把列压成 0 宽（症状：文字竖排）。 */}
          <div className="grid min-h-0 grid-cols-1 gap-3 md:grid-cols-[minmax(240px,320px)_minmax(0,1fr)]">
            {/* ---- 左栏：会话标识（只读）+ 回合列表 ---- */}
            <div className="flex min-h-0 min-w-0 flex-col gap-2 rounded-lg border p-2">
              <div className="flex shrink-0 items-start gap-1">
                <div className="min-w-0 flex-1">
                  <div className="truncate text-xs font-medium" title={doc?.title || undefined}>
                    {doc?.title || (loading ? "读取中…" : "（无标题）")}
                  </div>
                  <div
                    className="truncate font-mono text-[10px] text-muted-foreground"
                    title={doc?.body.workspace || undefined}
                  >
                    {doc?.body.workspace || "—"}
                  </div>
                </div>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  className="size-6 shrink-0 p-0"
                  title="重新读取这条会话（丢弃未保存的改动）"
                  disabled={!doc || saving}
                  onClick={() => {
                    if (!doc || !uid) return;
                    setPendingConfirm({
                      title: "重新读取？",
                      body: "未保存的改动会全部丢弃，库里的数据不受影响。",
                      run: () => void loadDoc(doc.sessionId, uid),
                    });
                  }}
                >
                  <RefreshCw className="size-3" />
                </Button>
              </div>

              <div className="text-muted-foreground flex shrink-0 flex-wrap gap-x-3 gap-y-0.5 text-[10px]">
                <span>用户提问 {userCount}</span>
                <span>助手 {doc?.counts.assistant ?? 0}</span>
                <span>思考 {doc?.counts.reasoning ?? 0}</span>
                <span>工具 {doc?.counts.toolCall ?? 0}</span>
                <span>{bytesLabel(doc)}</span>
              </div>

              {doc && doc.warnings.length > 0 && (
                <div className="max-h-24 shrink-0 overflow-auto rounded-md border border-amber-500/40 bg-amber-500/5 px-2 py-1 text-[10px] text-amber-700 dark:text-amber-400">
                  {doc.warnings.map((w, i) => (
                    <div key={i}>{w}</div>
                  ))}
                </div>
              )}

              <div className="relative shrink-0">
                <Search className="text-muted-foreground absolute top-1/2 left-2 size-3 -translate-y-1/2" />
                <Input
                  value={turnFilter}
                  onChange={(e) => setTurnFilter(e.target.value)}
                  placeholder="搜索回合…"
                  className="h-7 pl-7 text-xs"
                />
              </div>

              {/* 批量删除回合：多选态下**点整行 = 切换勾选**（同时把右栏切过去，边勾边看）。
                  逐个的 🗑 在多选态下同样可用，两者不互斥。 */}
              <div className="flex shrink-0 flex-wrap items-center gap-1.5">
                <Button
                  type="button"
                  variant={multi ? "default" : "outline"}
                  size="sm"
                  className="h-7 gap-1 px-2 text-[11px]"
                  title={multi ? "退出多选" : "勾选多个回合后一次性删除"}
                  disabled={!doc || liveTurns.length === 0 || saving}
                  onClick={() => {
                    setMulti((v) => !v);
                    setMultiSel(new Set());
                  }}
                >
                  <ListChecks className="size-3" />
                  {multi ? "退出多选" : "多选删除"}
                </Button>
                {multi && (
                  <>
                    <span className="text-muted-foreground text-[11px]">
                      已选 {multiSel.size} / {filteredTurns.length}
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-[11px]"
                      disabled={filteredTurns.length === 0 || saving}
                      onClick={() => setMultiSel(new Set(filteredTurns.map((t) => t.index)))}
                    >
                      全选
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-[11px]"
                      disabled={multiSel.size === 0 || saving}
                      onClick={() => setMultiSel(new Set())}
                    >
                      清空
                    </Button>
                    <Button
                      type="button"
                      variant="destructive"
                      size="sm"
                      className="ml-auto h-7 gap-1 px-2 text-[11px]"
                      disabled={multiSel.size === 0 || saving}
                      onClick={() => askDeleteTurns([...multiSel])}
                    >
                      <Trash2 className="size-3" />
                      删除选中（{multiSel.size}）
                    </Button>
                  </>
                )}
              </div>

              <div className="min-h-0 flex-1 space-y-1 overflow-auto">
                <button
                  type="button"
                  onClick={() => setSel({ kind: "head" })}
                  className={cn(
                    "w-full truncate rounded-md border px-2 py-1.5 text-left text-[11px]",
                    sel?.kind === "head"
                      ? "border-foreground/30 bg-foreground/[0.06]"
                      : "border-transparent hover:bg-foreground/[0.04]",
                  )}
                >
                  会话开头（会话元信息 / 自动标题）
                </button>
                {filteredTurns.map((t) => (
                  // ⚠️ 回合项与删除按钮是**兄弟**节点：`<button>` 里不能再套 `<button>`。
                  // 多选勾选框画在**回合按钮内部**（纯展示的 span，不是交互元素）——
                  // 这样点整行任意位置都能切换勾选，也不会有嵌套按钮的问题。
                  <div key={t.index} className="flex items-stretch gap-1">
                    <button
                      type="button"
                      onClick={() =>
                        multi ? toggleTurn(t.index) : setSel({ kind: "turn", index: t.index })
                      }
                      className={cn(
                        "min-w-0 flex-1 rounded-md border px-2 py-1.5 text-left",
                        multi && multiSel.has(t.index)
                          ? "border-destructive/60 bg-destructive/[0.06]"
                          : sel?.kind === "turn" && sel.index === t.index
                            ? "border-foreground/30 bg-foreground/[0.06]"
                            : "border-transparent hover:bg-foreground/[0.04]",
                      )}
                    >
                      <div className="flex items-center gap-1.5">
                        {multi && (
                          <span
                            aria-hidden
                            data-turn-check={multiSel.has(t.index) ? "1" : "0"}
                            className={cn(
                              "flex size-3.5 shrink-0 items-center justify-center rounded-[3px] border",
                              multiSel.has(t.index)
                                ? "border-destructive bg-destructive text-white"
                                : "border-muted-foreground/60",
                            )}
                          >
                            {multiSel.has(t.index) && <Check className="size-2.5" />}
                          </span>
                        )}
                        <span className="text-muted-foreground shrink-0 font-mono text-[10px]">
                          #{t.index + 1}
                        </span>
                        <span className="min-w-0 flex-1 truncate text-[11px]">
                          {t.userText || "（空提问）"}
                        </span>
                      </div>
                      <div className="text-muted-foreground mt-0.5 truncate text-[10px]">
                        行 {t.start}–{t.end} · {t.lines} 行
                        {t.tools.length > 0 ? ` · ${t.tools.length} 次工具` : ""}
                      </div>
                    </button>
                    {/* ⚠️ 删除按钮**常显**，不做 hover 才出现（与 Trae 侧一致）：
                        hover 才出现的破坏性按钮在触屏 / 键盘下等于不存在。 */}
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="text-destructive size-6 shrink-0 self-center p-0"
                      title="删除这个回合（写回前可撤销）"
                      disabled={saving}
                      onClick={() => askDeleteTurns([t.index])}
                    >
                      <Trash2 className="size-3" />
                    </Button>
                  </div>
                ))}
                {doc && filteredTurns.length === 0 && (
                  <div className="text-muted-foreground px-2 py-6 text-center text-[11px]">
                    {turns.length === 0 ? "这条会话没有任何回合" : "没有匹配的回合"}
                  </div>
                )}
              </div>
            </div>

            {/* ---- 右栏：会话式正文 ---- */}
            <div className="flex min-h-0 min-w-0 flex-col gap-2 rounded-lg border p-2">
              {loading ? (
                <div className="text-muted-foreground flex flex-1 items-center justify-center gap-2 text-xs">
                  <Loader2 className="size-4 animate-spin" /> 正在读取会话…
                </div>
              ) : loadError ? (
                <div className="flex flex-1 flex-col items-center justify-center gap-2 px-6 text-center">
                  <AlertTriangle className="text-destructive size-5" />
                  <div className="text-xs font-medium">读取失败（不是数据被删）</div>
                  <div className="text-muted-foreground break-all text-[11px]">{loadError}</div>
                </div>
              ) : rowsError ? (
                <div className="flex flex-1 flex-col items-center justify-center gap-2 px-6 text-center">
                  <AlertTriangle className="text-destructive size-5" />
                  <div className="text-xs font-medium">正文读取失败（不是数据被删）</div>
                  <div className="text-muted-foreground break-all text-[11px]">{rowsError}</div>
                  <Button size="sm" variant="outline" onClick={() => setReloadTick((v) => v + 1)}>
                    重试
                  </Button>
                </div>
              ) : !doc || !range ? (
                <div className="text-muted-foreground flex flex-1 items-center justify-center text-xs">
                  从左栏选一个回合开始编辑
                </div>
              ) : (
                <>
                  <div className="flex shrink-0 items-center gap-2 border-b pb-2">
                    <span className="text-xs font-medium">
                      {sel?.kind === "head"
                        ? "会话开头"
                        : `回合 ${(sel?.kind === "turn" ? sel.index : 0) + 1}`}
                    </span>
                    <span className="text-muted-foreground font-mono text-[10px]">
                      行 [{range.from}, {range.to})
                    </span>
                    {rowsLoading && <Loader2 className="size-3 animate-spin" />}
                    {sel?.kind === "turn" && (
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        className="text-destructive ml-auto h-6 shrink-0 gap-1 px-2 text-[11px]"
                        title="删除整个回合（写回前可撤销）"
                        disabled={saving}
                        onClick={() => askDeleteTurns([sel.index])}
                      >
                        <Trash2 className="size-3" />
                        删除本回合
                      </Button>
                    )}
                    <span
                      className={cn(
                        "text-muted-foreground text-[10px]",
                        sel?.kind === "turn" ? "" : "ml-auto",
                      )}
                    >
                      点正文即可改 · Ctrl/⌘+Enter 保存 · Esc 取消
                    </span>
                  </div>

                  <div className="min-h-0 flex-1 space-y-2 overflow-auto pr-1">
                    {rows.length === 0 && (
                      <div className="text-muted-foreground px-2 py-8 text-center text-[11px]">
                        这一块的行都被删掉了 —— 可以在下面新增，或直接写入
                      </div>
                    )}
                    {rows.map((row) => (
                      <RowCard
                        key={row.key}
                        row={row}
                        expanded={expanded.has(row.key)}
                        onToggleExpand={() =>
                          setExpanded((prev) => {
                            const n = new Set(prev);
                            if (n.has(row.key)) n.delete(row.key);
                            else n.add(row.key);
                            return n;
                          })
                        }
                        showAll={showAll.has(row.key)}
                        onShowAll={() => setShowAll((prev) => new Set(prev).add(row.key))}
                        editing={editing?.key === row.key ? editing.text : null}
                        onStartEdit={(text) => setEditing({ key: row.key, text })}
                        onCancelEdit={() => setEditing(null)}
                        onCommitEdit={(text) => {
                          if (!row.json || !row.textPath) return;
                          setLineJson(row, applyText(row.json, row.textPath, text, row.wrapped));
                          setEditing(null);
                        }}
                        rawOpen={rawOpen.has(row.key)}
                        rawDraft={rawDraft[row.key]}
                        rawError={rawError[row.key]}
                        onToggleRaw={() => {
                          setRawOpen((prev) => {
                            const n = new Set(prev);
                            if (n.has(row.key)) {
                              n.delete(row.key);
                            } else {
                              n.add(row.key);
                              setRawDraft((d) => ({
                                ...d,
                                [row.key]: JSON.stringify(row.json ?? {}, null, 2),
                              }));
                              setRawError((d) => ({ ...d, [row.key]: "" }));
                            }
                            return n;
                          });
                        }}
                        onRawDraft={(v) => setRawDraft((d) => ({ ...d, [row.key]: v }))}
                        onCommitRaw={() => {
                          try {
                            const v = JSON.parse(rawDraft[row.key] ?? "{}") as unknown;
                            if (v === null || typeof v !== "object" || Array.isArray(v)) {
                              throw new Error("整行必须是一个 JSON 对象");
                            }
                            setLineJson(row, v as Record<string, unknown>);
                            setRawError((d) => ({ ...d, [row.key]: "" }));
                          } catch (e) {
                            setRawError((d) => ({
                              ...d,
                              [row.key]: e instanceof Error ? e.message : String(e),
                            }));
                          }
                        }}
                        onDelete={() => {
                          if (row.index === null) {
                            removeRow(row);
                            return;
                          }
                          setPendingConfirm({
                            title: "删掉这一段？",
                            body: (
                              <>
                                第 {row.index} 行（{row.label}）会从会话里去掉。
                                <br />
                                <span className="text-muted-foreground">
                                  这一步只改本地草稿，点「写入」才落盘。
                                </span>
                              </>
                            ),
                            run: () => removeRow(row),
                          });
                        }}
                      />
                    ))}
                  </div>

                  <div className="flex shrink-0 flex-wrap items-center gap-2 border-t pt-2">
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="gap-1"
                      onClick={() => addLine("user")}
                    >
                      <MessageSquare className="size-3" /> 新增用户提问
                    </Button>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="gap-1"
                      onClick={() => addLine("assistant")}
                    >
                      <Sparkles className="size-3" /> 新增助手回答
                    </Button>
                    <span className="text-muted-foreground text-[10px]">
                      新行插在本回合末尾（{range.to} 行之前）
                    </span>
                  </div>

                  {/* 底层字段：sessions 表全字段 + 当前选中的原始行 */}
                  <div className="shrink-0">
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="gap-1 px-1 text-[11px]"
                      onClick={() => setHeadOpen((v) => !v)}
                    >
                      {headOpen ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
                      底层字段（sessions 表 · {doc.columns.length} 列）
                      {Object.keys(rowPatch).length > 0 && (
                        <Badge variant="outline" className="ml-1 text-[10px]">
                          {Object.keys(rowPatch).length} 个字段已改
                        </Badge>
                      )}
                    </Button>
                    {headOpen && rowDraft && (
                      <div className="mt-1 max-h-72 overflow-auto rounded-md border p-2">
                        <RowForm
                          row={rowDraft}
                          columns={doc.columns}
                          onChange={(r) => setRowDraft(r)}
                        />
                      </div>
                    )}
                  </div>
                </>
              )}
            </div>
          </div>

          {/* 页脚上方：长文本（校验明细 / 写入日志）必须放这儿 —— 塞进 DialogFooter 在窄屏会压成「一个字一行」 */}
          {(errorCount > 0 || warnCount > 0 || (doc?.warnings.length ?? 0) > 0 || saveLog.length > 0) && (
            <div className="max-h-40 shrink-0 space-y-2 overflow-auto">
              {errorCount > 0 && (
                <Alert variant="destructive">
                  <AlertTriangle className="size-4" />
                  <AlertTitle>必须修好才能写入（{errorCount}）</AlertTitle>
                  <AlertDescription className="text-xs">
                    <ul className="mt-1 list-disc space-y-0.5 pl-4">
                      {report?.errors.slice(0, 12).map((e, i) => (
                        <li key={i} className="break-all">
                          {e}
                        </li>
                      ))}
                    </ul>
                    {errorCount > 12 && <div className="mt-1">…另有 {errorCount - 12} 处</div>}
                  </AlertDescription>
                </Alert>
              )}
              {warnCount > 0 && (
                <Alert>
                  <AlertTriangle className="size-4" />
                  <AlertTitle>提醒（不阻止写入，共 {warnCount}）</AlertTitle>
                  <AlertDescription className="text-xs">
                    <ul className="mt-1 list-disc space-y-0.5 pl-4">
                      {report?.warnings.slice(0, 8).map((w, i) => (
                        <li key={i} className="break-all">
                          {w}
                        </li>
                      ))}
                    </ul>
                    {warnCount > 8 && <div className="mt-1">…另有 {warnCount - 8} 处</div>}
                  </AlertDescription>
                </Alert>
              )}
              {saveLog.length > 0 && (
                <pre className="bg-muted/50 max-h-24 overflow-auto rounded-md p-2 font-mono text-[11px] whitespace-pre-wrap">
                  {saveLog.slice(0, 40).join("\n")}
                </pre>
              )}
            </div>
          )}

          {/* 跨客户端搬迁（数据迁移页）：必须写明「源会话会被改写」——
              用户以为只是「把改过的那份搬过去」，其实导入链路读的是库里的内容。 */}
          {doc && cross && crossAccount ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertTitle>
                写入到另一个客户端：{cross.toLabel}
              </AlertTitle>
              <AlertDescription className="text-xs leading-5">
                {hasEdits(edit) || Object.keys(rowPatch).length > 0
                  ? "会先把你的改动就地写回这条 WorkBuddy 会话（导入读的是库里的内容，绕不过去），"
                  : "没有任何改动，直接用库里的内容"}
                再把它导入到「{crossAccount.clientLabel} · {crossAccount.label}」。源会话不会被删，
                迁移过程中 {cross.toLabel} 客户端会被退出、写完自动拉起。
              </AlertDescription>
            </Alert>
          ) : null}

          {/* 跨账号写入必须有**独立提示**：这一步会改会话的归属账号。 */}
          {doc && !cross && mode === "insert" && targetUid !== "" ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertTitle>将写入另一个账号</AlertTitle>
              <AlertDescription className="text-xs">
                新会话归属到「{targets.find((a) => a.uid === targetUid)?.label ?? targetUid}」，
                以新 UUID 插入 `sessions` 表并把正文里的会话 id 一并换掉。源会话一行都不动。
                该账号正在 WorkBuddy 中使用时会被拒绝写入。
              </AlertDescription>
            </Alert>
          ) : null}

          <div className="flex flex-wrap items-center justify-end gap-2 border-t pt-3">
            <div className="mr-auto flex flex-wrap items-center gap-3">
              {/* ⚠️ 跨客户端模式（数据迁移页）下**整块换掉**同客户端的写入方式 / 写入到账号：
                  那几个选项在这两个页面上只会把人绕回去（用户报的「怎么还是 workbuddy 到 workbuddy」）。 */}
              {cross ? (
                <>
                  {/* ⚠️⚠️ **客户端与账号必须分开选**：Trae 国内版与国际版是两个不同客户端，
                      同一台机器上各有一份账号档案。早先把「客户端 · 账号」
                      拼成一个字符串塞进同一个下拉，用户报「trae 不应该多选账号」说的就是这个；
                      而且老实现按 uid 反查会命中另一个客户端的同名账号。
                      只有一个客户端时（WorkBuddy 侧）不显示这一行，免得平白多一步。 */}
                  {crossGroups.length > 1 ? (
                    <div className="min-w-0">
                      <Label className="text-xs">写入到客户端</Label>
                      <div className="mt-1 flex flex-wrap gap-1.5">
                        {crossGroups.map((g) => (
                          <button
                            key={g.key}
                            type="button"
                            disabled={Boolean(cross.blocked)}
                            onClick={() => pickCrossClient(g.key)}
                            data-cross-client={g.key}
                            data-cross-client-active={g.key === crossClient ? "1" : "0"}
                            className={cn(
                              "rounded-md border px-2 py-1 text-xs transition-colors",
                              g.key === crossClient
                                ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                                : "border-border hover:bg-foreground/[0.03]",
                              cross.blocked && "opacity-60",
                            )}
                          >
                            {g.label}
                            <span className="text-muted-foreground"> · {g.accounts.length} 个账号</span>
                          </button>
                        ))}
                      </div>
                    </div>
                  ) : null}
                  <div className="min-w-0">
                    <Label className="text-xs">写入到 {cross.toLabel} 账号</Label>
                    <Select value={crossKey} onValueChange={setCrossKey} disabled={Boolean(cross.blocked)}>
                      <SelectTrigger
                        data-cross-account="1"
                        className="h-8 w-[min(24rem,100%)] text-xs"
                        title={cross.blocked ?? cross.accountsError ?? "把这条会话搬到对面客户端的某个账号"}
                      >
                        <SelectValue placeholder="选择目标账号" />
                      </SelectTrigger>
                      <SelectContent>
                        {crossClientAccounts.map((a) => (
                          <SelectItem key={crossKeyOf(a)} value={crossKeyOf(a)}>
                            {a.label}
                            {a.noDb ? " · 无会话库" : ""}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                </>
              ) : (
                <>
                  <div className="min-w-0">
                    <Label className="text-xs">写入方式</Label>
                    <Select value={mode} onValueChange={(v) => setMode(v as "overwrite" | "insert")}>
                      <SelectTrigger className="h-8 w-[min(17rem,100%)] text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="overwrite">
                          就地改写（保持会话 ID，原内容被替换）
                        </SelectItem>
                        <SelectItem value="insert">新增会话（新 UUID，原会话不动）</SelectItem>
                      </SelectContent>
                    </Select>
                  </div>
                  {mode === "insert" ? (
                    <div className="min-w-0">
                      <Label className="text-xs">写入到账号</Label>
                      <Select
                        value={targetUid === "" ? SAME_ACCOUNT : targetUid}
                        onValueChange={(v) => setTargetUid(v === SAME_ACCOUNT ? "" : v)}
                      >
                        <SelectTrigger
                          className="h-8 w-[min(20rem,100%)] text-xs"
                          title={targetsError ?? "写入到别的账号会以新 UUID 插入一条新会话"}
                        >
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value={SAME_ACCOUNT}>与源会话同一账号</SelectItem>
                          {targets.map((a) => (
                            <SelectItem key={a.uid} value={a.uid}>
                              {a.label} · {a.sessions} 条会话
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                  ) : null}
                  {targetsError ? (
                    <span className="text-muted-foreground text-[11px]">
                      账号清单读取失败（同账号写入不受影响）：{targetsError}
                    </span>
                  ) : null}
                </>
              )}
              {cross && (cross.blocked || cross.accountsError || cross.accounts.length === 0) ? (
                <span className="text-[11px] text-amber-600">
                  {cross.blocked ?? cross.accountsError ?? `没有可写入的 ${cross.toLabel} 账号`}
                </span>
              ) : null}
            </div>

            <Button
              type="button"
              variant="outline"
              size="sm"
              className="gap-1"
              disabled={!changed}
              onClick={undoAll}
            >
              <RotateCcw className="size-3" />
              撤销改动
            </Button>
            <Button
              type="button"
              size="sm"
              className="gap-1"
              disabled={!doc || saving || errorCount > 0 || !pendingWrite}
              onClick={() => void save()}
            >
              {saving ? <Loader2 className="size-3 animate-spin" /> : <Save className="size-3" />}
              {saving
                ? "写入中…"
                : cross
                  ? `写入到 ${cross.toLabel}`
                  : mode === "overwrite"
                    ? "就地改写"
                    : "新增写入"}
            </Button>
          </div>
        </ResizableDialogContent>
      </Dialog>

      <AlertDialog open={pendingConfirm !== null} onOpenChange={(v) => !v && setPendingConfirm(null)}>
        {/* ⚠️ 宽度与换行见下方 `min-w-0` 的注释：确认内容里会出现很长的单行（提问正文 /
            原始 JSON 片段），不约束宽度就会横向溢出到卡片外（用户报的「内容过长会超出显示」）。 */}
        <AlertDialogContent className="max-w-[min(44rem,calc(100vw-2rem))]">
          <AlertDialogHeader className="min-w-0">
            <AlertDialogTitle>{pendingConfirm?.title}</AlertDialogTitle>
            <AlertDialogDescription asChild>
              <div className="min-w-0 text-xs">{pendingConfirm?.body}</div>
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                pendingConfirm?.run();
                setPendingConfirm(null);
              }}
            >
              确认
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

/** 只按 JSON 判新增行的种类（新增的行一定是 message）。 */
/** 右栏里的一段。 */
function RowCard({
  row,
  expanded,
  onToggleExpand,
  showAll,
  onShowAll,
  editing,
  onStartEdit,
  onCancelEdit,
  onCommitEdit,
  rawOpen,
  rawDraft,
  rawError,
  onToggleRaw,
  onRawDraft,
  onCommitRaw,
  onDelete,
}: {
  row: DisplayRow;
  expanded: boolean;
  onToggleExpand: () => void;
  showAll: boolean;
  onShowAll: () => void;
  editing: string | null;
  onStartEdit: (text: string) => void;
  onCancelEdit: () => void;
  onCommitEdit: (text: string) => void;
  rawOpen: boolean;
  rawDraft: string | undefined;
  rawError: string | undefined;
  onToggleRaw: () => void;
  onRawDraft: (v: string) => void;
  onCommitRaw: () => void;
  onDelete: () => void;
}) {
  const collapsed = defaultCollapsed(row.kind) && !expanded;
  const text = lineText(row.json, row.textPath, row.fallbackText);
  const shown = !showAll && text.length > READ_CAP ? text.slice(0, READ_CAP) : text;
  const canEdit = row.textPath !== null && row.json !== null;

  return (
    <div
      className={cn(
        "rounded-lg border px-2 py-1.5",
        KIND_TONE[row.kind] ?? "",
        row.dirty && "border-dashed border-foreground/40",
      )}
    >
      <div className="flex flex-wrap items-center gap-2">
        <Badge variant={row.kind === "user" ? "default" : "secondary"} className="text-[10px]">
          {row.label}
        </Badge>
        {row.name && <span className="font-mono text-[10px]">{row.name}</span>}
        {row.status && (
          <span className="text-muted-foreground font-mono text-[10px]">{row.status}</span>
        )}
        {row.index !== null ? (
          <span className="text-muted-foreground font-mono text-[10px]">#{row.index}</span>
        ) : (
          <Badge variant="outline" className="text-[10px]">
            新增
          </Badge>
        )}
        {row.dirty && row.index !== null && (
          <Badge variant="outline" className="text-[10px]">
            已改
          </Badge>
        )}
        <span className="ml-auto flex items-center gap-1">
          {defaultCollapsed(row.kind) && (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="size-6 p-0"
              title={expanded ? "收起这一段" : "展开这一段"}
              aria-label={expanded ? "收起这一段" : "展开这一段"}
              onClick={onToggleExpand}
            >
              {expanded ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />}
            </Button>
          )}
          <Button
            type="button"
            variant={rawOpen ? "default" : "ghost"}
            size="sm"
            className="size-6 p-0"
            title="原始 JSON（整行的完整结构）"
            aria-label="原始 JSON"
            onClick={onToggleRaw}
          >
            <Code2 className="size-3" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="text-destructive size-6 p-0"
            title="删除这一段"
            aria-label="删除这一段"
            onClick={onDelete}
          >
            <Trash2 className="size-3" />
          </Button>
        </span>
      </div>

      {editing !== null ? (
        <div className="mt-1.5 space-y-1">
          <textarea
            autoFocus
            value={editing}
            onChange={(e) => onStartEdit(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                onCancelEdit();
              } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                e.preventDefault();
                onCommitEdit(editing);
              }
            }}
            className="bg-background min-h-[72px] w-full resize-y rounded-md border px-2 py-1 text-xs"
          />
          <div className="flex items-center gap-2">
            <Button size="sm" className="h-7 text-[11px]" onClick={() => onCommitEdit(editing)}>
              保存这一段
            </Button>
            <Button size="sm" variant="ghost" className="h-7 text-[11px]" onClick={onCancelEdit}>
              取消
            </Button>
            <span className="text-muted-foreground text-[10px]">Ctrl/⌘+Enter 保存 · Esc 取消</span>
          </div>
        </div>
      ) : collapsed ? (
        <button
          type="button"
          className="text-muted-foreground hover:text-foreground mt-1 block w-full truncate text-left text-[11px]"
          onClick={onToggleExpand}
          title="点开看完整内容"
        >
          {oneLine(text, SUMMARY_CAP) || "（空）"}
        </button>
      ) : (
        <button
          type="button"
          className={cn(
            "mt-1 block w-full text-left text-xs whitespace-pre-wrap",
            canEdit ? "hover:bg-foreground/[0.03] cursor-text rounded-sm" : "cursor-default",
          )}
          title={canEdit ? "点击编辑这一段" : "这一段没有可单独编辑的正文，用右上角「原始 JSON」改"}
          onClick={() => {
            if (canEdit) onStartEdit(text);
          }}
        >
          {shown || <span className="text-muted-foreground">（空）</span>}
        </button>
      )}

      {!showAll && text.length > READ_CAP && editing === null && (
        <button
          type="button"
          className="text-muted-foreground mt-0.5 text-[10px] underline"
          onClick={onShowAll}
        >
          显示全部（共 {text.length} 字）
        </button>
      )}

      {rawOpen && (
        <div className="mt-1.5 space-y-1">
          <textarea
            value={rawDraft ?? ""}
            onChange={(e) => onRawDraft(e.target.value)}
            className="bg-muted/40 min-h-[140px] w-full resize-y rounded-md border px-2 py-1 font-mono text-[11px]"
          />
          {rawError ? (
            <div className="text-destructive text-[10px] break-all">{rawError}</div>
          ) : null}
          <div className="flex items-center gap-2">
            <Button size="sm" className="h-7 text-[11px]" onClick={onCommitRaw}>
              保存原始 JSON
            </Button>
            <Button size="sm" variant="ghost" className="h-7 text-[11px]" onClick={onToggleRaw}>
              关闭
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

/** 正文体积标签（0 字节时不显示）。 */
function bytesLabel(doc: WbEditDoc | null): string {
  const b = doc?.body.bytes ?? 0;
  if (b <= 0) return "";
  if (b < 1024 * 1024) return `${Math.round(b / 1024)} KB`;
  return `${Math.round((b / 1048576) * 10) / 10} MB`;
}
