import * as React from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  Check,
  Code2,
  Layers,
  ListChecks,
  Loader2,
  Plus,
  RefreshCw,
  RotateCcw,
  Save,
  Search,
  Trash2,
  Wand2,
} from "lucide-react";

import { TraeTurnDetail } from "@/components/trae-turn-detail";
import { readJsonRef, type JsonRefMeta } from "@/components/json-form";
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
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ResizableDialogContent } from "@/components/ui/resizable-dialog-content";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import * as api from "@/lib/api";
import { cn } from "@/lib/utils";
import type {
  CrossWriteAccount,
  CrossWriteTarget,
  TraeEditDoc,
  TraeEditReport,
  TraeEditTable,
  TraeEditTargetAccount,
} from "@/lib/trae-types";
import { crossKeyOf } from "@/lib/trae-types";
import {
  buildSessionView,
  batchReplaceText,
  batchSetColumn,
  appendAtPath,
  columnChoices,
  duplicateRows,
  deleteRowRefs,
  emptySegmentItem,
  identityValuesToRemap,
  messageRefs,
  nowSeconds,
  parseByColumn,
  removeAtPath,
  segmentContainerPath,
  setAtPath,
  stampRow,
  tableOf,
  turnRefs,
  turnsLeftAfterRemoving,
  type RowRef,
  type SessionView,
  type TurnMessage,
  type TurnView,
} from "@/lib/trae-turn-model";

/**
 * T46 · 会话编辑器（回合视图）。
 *
 * 视图范式：**左栏是该会话的回合，右栏是选中回合的详细内容**。
 * 之前那套「先选表、再看原始行」的界面能改的东西一样多，但看的人得先在脑子里
 * 把 20 多张表重新拼成一条对话 —— 这是它被换掉的唯一原因，不是功能不够。
 *
 * 四条设计约束（都来自踩过的坑）：
 *
 * 1. **校验只有一份实现**（后端 `trae_session_edit_validate`）。前端只显示
 *    `errors` / `warnings`，绝不重写规则 —— 同一套规则两处独立推导必然跑偏一次。
 * 2. **写库是重活**（结束客户端 → 备份 → 解密 → 合并 WAL → 改 → 增量回写 → 重启），
 *    所以按钮要有明确的进行中状态，且成功后**不静默关窗**（用户要看到备份路径）。
 * 3. **id 只在后端生成**（`trae_session_edit_new_ids`）。前端不自己造 id ——
 *    两套生成规则必然在某次调整里跑偏，而症状是「写完客户端打不开」。
 * 4. **增删行只改 doc，不直接写库**。写入仍然是既有那条路（`overwrite` 会
 *    「删掉脏表全部旧行 → 按 doc 重新插入」），所以前端的增删天然被支持，
 *    也能整体「撤销改动」。
 */

type Mode = "overwrite" | "insert";

/**
 * 「与源会话同一账号」在 Select 里的哨兵值。
 * ⚠️ Radix 的 `SelectItem` **不允许空字符串**（那是 placeholder 的保留值）。
 */
const SAME_ACCOUNT = "__same_account__";

/** 左栏当前看的是哪一类数据。 */
type PaneKind = "turn" | "orphans" | "session";

export interface TraeSessionEditorProps {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  clientKey: string;
  clientLabel: string;
  /**
   * 要编辑的会话 —— **就是记录页里点进来的那一条**。
   *
   * ⚠️ 这里原先是 `sessions: TraeSessionInfo[]` + 左栏一个下拉，可以就地切到别的会话。
   *    已按要求取消：入口改成「点哪条会话就编哪条」之后，再摆一个下拉只会让人怀疑
   *    「我点的到底是哪条」。要换会话就关掉、点另一行。
   *
   * ⚠️ 唯一例外：写入方式选「新增会话」并写入成功后，这里的选中会跟到**新生成的 id**
   *    （`picked` 是 state，不是直接读这个 prop）—— 否则用户接着改的其实是原会话，
   *    而他以为在改刚建出来的那条。
   */
  sessionId: string | null;
  /** 列表需要刷新时（写入后）通知外层。 */
  onChanged?: () => void;
  /**
   * T46 · S5 · 数据迁移页专用：**跨客户端**写入。
   *
   * 给了它，页脚就不再是「同客户端写入方式 / 写入到账号」，而是「写入到 <对面客户端> 账号」——
   * 因为「Trae → WorkBuddy」这个页面要的就是把这条 Trae 会话搬到 WorkBuddy 去。
   * 不给（会话记录页）就是原来的同账号 / 跨账号写入。
   */
  cross?: CrossWriteTarget | null;
}

function updateRow(
  doc: TraeEditDoc,
  ref: RowRef,
  fn: (row: Record<string, unknown>, t: TraeEditTable) => Record<string, unknown>,
): TraeEditDoc {
  return {
    ...doc,
    tables: doc.tables.map((t) => {
      if (t.name !== ref.table) return t;
      const row = t.rows[ref.index];
      if (!row) return t;
      const rows = t.rows.slice();
      rows[ref.index] = fn(row, t);
      return { ...t, rows };
    }),
  };
}

export function TraeSessionEditor({
  open,
  onOpenChange,
  clientKey,
  clientLabel,
  sessionId,
  onChanged,
  cross,
}: TraeSessionEditorProps) {
  const [picked, setPicked] = React.useState<string | null>(sessionId ?? null);

  const [doc, setDoc] = React.useState<TraeEditDoc | null>(null);
  const [baseDoc, setBaseDoc] = React.useState<string>("");
  const [loading, setLoading] = React.useState(false);
  const [loadError, setLoadError] = React.useState<string | null>(null);

  const [report, setReport] = React.useState<TraeEditReport | null>(null);
  const [checking, setChecking] = React.useState(false);

  /** 用户真正改过的表（增量标记）。⚠️ 不能用全量 JSON 比对：doc 实测 8.8 MB，会卡死输入。 */
  const [dirtyTables, setDirtyTables] = React.useState<Set<string>>(new Set());
  const [manualDirty, setManualDirty] = React.useState(false);

  const [mode, setMode] = React.useState<Mode>("overwrite");
  const [targetUid, setTargetUid] = React.useState<string>("");
  const [targets, setTargets] = React.useState<TraeEditTargetAccount[]>([]);
  const [targetsError, setTargetsError] = React.useState<string | null>(null);
  /**
   * 跨客户端写入（数据迁移页）选中的目标账号 —— 存的是**复合键** `clientKey::uid`。
   *
   * ⚠️ 不能只存 uid：同一个 uid 在 Trae CN 与 TRAE SOLO CN 下各有一份，
   * 只按 uid 反查会命中另一个客户端的同名账号（写错库）。
   */
  const [crossKey, setCrossKey] = React.useState<string>("");
  const [saving, setSaving] = React.useState(false);
  const [saveLog, setSaveLog] = React.useState<string[]>([]);
  const [rawView, setRawView] = React.useState(false);
  const [rawText, setRawText] = React.useState("");
  const [rawError, setRawError] = React.useState<string | null>(null);

  /** 左栏：当前看的是哪一类数据 + 选中的回合 key（用 `turn_id`，不是行号 —— 删行会移号）。 */
  const [pane, setPane] = React.useState<PaneKind>("turn");
  const [selTurn, setSelTurn] = React.useState<string | null>(null);
  const [turnFilter, setTurnFilter] = React.useState("");

  /** 右栏里勾选的消息 id（批量修改 / 批量删除的作用对象）。 */
  const [selMsg, setSelMsg] = React.useState<Set<string>>(new Set());

  /**
   * 左栏的多选态（批量删回合）。
   *
   * ⚠️ 与右栏的 `selMsg`（消息级勾选）是**两件事**：一个勾「回合」，一个勾「消息」。
   * 集合里存的是 `TurnView.key`（`turn_id`，不是行号）—— 删行会让行号整体前移，
   * 用行号当勾选键的话，删掉一个回合之后其余勾选会悄悄指向别的回合。
   */
  const [multi, setMulti] = React.useState(false);
  const [multiSel, setMultiSel] = React.useState<Set<string>>(new Set());

  /** 待确认的破坏性动作（删除行 / 删除回合）。 */
  const [confirm, setConfirm] = React.useState<{
    title: string;
    body: React.ReactNode;
    run: () => void;
  } | null>(null);
  const [busy, setBusy] = React.useState(false);

  const [batchOpen, setBatchOpen] = React.useState(false);
  const [batchColumn, setBatchColumn] = React.useState<string>("");
  const [batchValue, setBatchValue] = React.useState<string>("");
  const [batchFind, setBatchFind] = React.useState<string>("");
  const [batchRepl, setBatchRepl] = React.useState<string>("");
  const [batchCs, setBatchCs] = React.useState(false);
  const [batchTarget, setBatchTarget] = React.useState<string>("");
  const [batchHow, setBatchHow] = React.useState<"duplicate" | "move">("duplicate");

  // 打开时同步要编辑的会话；关闭时清掉临时选择（避免下次打开带着上一次的勾选）。
  React.useEffect(() => {
    if (open) {
      setPicked(sessionId ?? null);
    } else {
      setSelMsg(new Set());
      setMulti(false);
      setMultiSel(new Set());
      setConfirm(null);
      setSaveLog([]);
      setBatchOpen(false);
      setSelTurn(null);
      setPane("turn");
      setTurnFilter("");
    }
  }, [open, sessionId]);

  const loadDoc = React.useCallback(
    async (sid: string) => {
      setLoading(true);
      setLoadError(null);
      setDoc(null);
      setReport(null);
      setDirtyTables(new Set());
      setManualDirty(false);
      setSelMsg(new Set());
      setMultiSel(new Set());
      try {
        const d = await api.traeSessionEditDoc(clientKey, sid);
        setDoc(d);
        setBaseDoc(JSON.stringify(d));
      } catch (e) {
        // ⚠️ 「读取失败」与「没有数据」必须长得不一样：保留错误态，不摆空态卡。
        setLoadError(typeof e === "string" ? e : String(e));
      } finally {
        setLoading(false);
      }
    },
    [clientKey],
  );

  React.useEffect(() => {
    if (open && picked) void loadDoc(picked);
  }, [open, picked, loadDoc]);

  // S2：目标账号清单（只读）。打开时拉一次；失败**只提示不挡路** ——
  // 同账号写入不依赖它，别因为列不出账号就不让用户编辑。
  // ⚠️ 跨客户端模式（数据迁移页）下**不拉**：那份清单是 Trae 自己的账号，放在
  //    「Trae → WorkBuddy」页上只会让人以为要写回 Trae（用户报的就是这个）。
  React.useEffect(() => {
    if (!open || !clientKey || cross) return;
    let cancelled = false;
    setTargetsError(null);
    void (async () => {
      try {
        const r = await api.traeSessionEditTargets(clientKey);
        if (!cancelled) setTargets(r.accounts);
      } catch (e) {
        if (!cancelled) setTargetsError(typeof e === "string" ? e : String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [open, clientKey, cross]);

  // 跨客户端模式的默认目标账号：优先用页面给的（页面上已经选好的那个），其次第一个候选。
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
   * 目标账号按**客户端**分组（同一台机器上都装了 Trae CN 与 TRAE SOLO CN 时会有两组）。
   * 顺序 = `accounts` 里第一次出现的顺序，不重排。
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

  /** 选中的目标账号（复合键 `clientKey::uid` 定位，`clientKey` 由它带出来）。 */
  const crossTarget = React.useMemo(
    () => cross?.accounts.find((a) => crossKeyOf(a) === crossKey) ?? null,
    [cross, crossKey],
  );

  const crossClient = crossTarget?.clientKey ?? crossGroups[0]?.key ?? "";

  /** 当前客户端下的账号（下拉里只列这个客户端的）。 */
  const crossClientAccounts = React.useMemo(
    () => crossGroups.find((g) => g.key === crossClient)?.accounts ?? [],
    [crossGroups, crossClient],
  );

  /** 切客户端：落到该客户端下第一个账号上。 */
  const pickCrossClient = (key: string) => {
    const first = crossGroups.find((g) => g.key === key)?.accounts[0];
    if (first) setCrossKey(crossKeyOf(first));
  };

  // 切回覆写时必须清掉目标账号 —— 后端只在 insert 时接受它，留着会让下一次覆写莫名报错。
  React.useEffect(() => {
    if (mode !== "insert") setTargetUid("");
  }, [mode]);

  // 防抖校验：规则在后端，前端只显示。
  React.useEffect(() => {
    if (!doc) {
      setReport(null);
      return;
    }
    let alive = true;
    setChecking(true);
    const timer = window.setTimeout(() => {
      void api
        .traeSessionEditValidate(doc)
        .then((r) => {
          if (alive) setReport(r);
        })
        .catch((e) => {
          if (alive) {
            setReport({
              ok: false,
              errors: [`校验请求失败：${typeof e === "string" ? e : String(e)}`],
              warnings: [],
            });
          }
        })
        .finally(() => {
          if (alive) setChecking(false);
        });
    }, 350);
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [doc]);

  const view: SessionView | null = React.useMemo(
    () => (doc ? buildSessionView(doc) : null),
    [doc],
  );

  // 回合被删掉后，选中的那个可能已经不存在 ⇒ 退到第一个回合（别停在空白页上）。
  React.useEffect(() => {
    if (!view) return;
    if (pane !== "turn") return;
    if (selTurn && view.turns.some((t) => t.key === selTurn)) return;
    setSelTurn(view.turns[0]?.key ?? null);
  }, [view, pane, selTurn]);

  // 勾选的消息被删掉后必须摘掉：留着会让「已选 N 条」永远挂着一个删不掉的幽灵，
  // 用户看到的数字和实际发生的事就分家了。
  React.useEffect(() => {
    if (!view) return;
    const alive = new Set<string>();
    for (const t of view.turns) {
      if (t.user) alive.add(t.user.id);
      if (t.assistant) alive.add(t.assistant.id);
    }
    for (const m of view.orphans) alive.add(m.id);
    setSelMsg((prev) => {
      const next = new Set([...prev].filter((id) => alive.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [view]);

  // 勾过的**回合**被删掉后同样要摘掉：否则「已选 N」里挂着的那个 key 再也删不动，
  // 「删除选中（N）」点下去只会少删一个，用户看到的数字和实际发生的事就分家了。
  React.useEffect(() => {
    if (!view) return;
    const alive = new Set(view.turns.map((t) => t.key));
    setMultiSel((prev) => {
      const next = new Set([...prev].filter((k) => alive.has(k)));
      return next.size === prev.size ? prev : next;
    });
  }, [view]);

  const filteredTurns = React.useMemo(() => {
    if (!view) return [];
    const q = turnFilter.trim().toLowerCase();
    if (!q) return view.turns;
    return view.turns.filter(
      (t) =>
        t.question.toLowerCase().includes(q) ||
        (t.turnId ?? "").toLowerCase().includes(q) ||
        t.status.toLowerCase().includes(q),
    );
  }, [view, turnFilter]);

  const currentTurn: TurnView | null = React.useMemo(() => {
    if (!view) return null;
    if (pane !== "turn") return null;
    return view.turns.find((t) => t.key === selTurn) ?? view.turns[0] ?? null;
  }, [view, pane, selTurn]);

  // ---- 编辑动作：全部走不可变更新 ----
  const patchTable = React.useCallback((name: string, fn: (t: TraeEditTable) => TraeEditTable) => {
    setDirtyTables((prev) => (prev.has(name) ? prev : new Set(prev).add(name)));
    setDoc((d) =>
      d ? { ...d, tables: d.tables.map((t) => (t.name === name ? fn(t) : t)) } : d,
    );
  }, []);

  const setRow = (ref: RowRef, row: Record<string, unknown>) => {
    patchTable(ref.table, (t) => {
      const rows = t.rows.slice();
      rows[ref.index] = row;
      return { ...t, rows };
    });
  };

  const setCell = (ref: RowRef, column: string, value: unknown) => {
    patchTable(ref.table, (t) => {
      const rows = t.rows.slice();
      rows[ref.index] = { ...rows[ref.index], [column]: value };
      return { ...t, rows };
    });
  };

  // ---- 会话视图里「一段话」的增删改 ----
  //
  // 段的 `path` 一律**相对该行 `content` 列的值**（见 `trae-turn-model` 的 `Segment`）。
  // 这里只改 doc，不碰库 —— 写入仍然走既有那条 `edit_save`。

  /** 改一段。纯文本段给 string，工具参数段给整块 JSON —— 两者都只是「把那个位置换掉」。 */
  const saveSegment = React.useCallback(
    (ref: RowRef, path: (string | number)[], value: unknown) => {
      const row = doc ? tableOf(doc, ref.table)?.rows[ref.index] : undefined;
      if (!row) return;
      setCell(ref, "content", setAtPath(row["content"], path, value));
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [doc],
  );

  /**
   * 删一段。
   *
   * ⚠️ 三种段删法不一样，混用会连带删掉用户没打算删的东西：
   *   · **说明（`thought`）** ⇒ 只清空这个字段。它和同一条 `messages[i]` 里的工具调用
   *     是**同一个元素**，摘掉元素会把那次工具调用一起删掉。
   *   · **提问文本（`text_content`）** ⇒ 摘掉整个 `content[i]`。
   *   · **工具调用 / 答复** ⇒ 摘掉整个 `messages[i]`（那一段就是那个元素）。
   */
  const deleteSegment = React.useCallback(
    (ref: RowRef, path: (string | number)[]) => {
      const row = doc ? tableOf(doc, ref.table)?.rows[ref.index] : undefined;
      if (!row || path.length === 0) return;
      const last = path[path.length - 1];
      if (last === "thought") {
        setCell(ref, "content", setAtPath(row["content"], path, ""));
        return;
      }
      let target = path;
      if (last === "text_content") target = path.slice(0, -1);
      else if (path[0] === "messages" && typeof path[1] === "number") target = path.slice(0, 2);
      setCell(ref, "content", removeAtPath(row["content"], target));
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [doc],
  );

  /** 在正文末尾再加一段（助手侧默认给一段空答复，客户端认这种形状）。 */
  const addSegment = React.useCallback(
    async (ref: RowRef, role: string) => {
      const row = doc ? tableOf(doc, ref.table)?.rows[ref.index] : undefined;
      if (!row) return;
      try {
        const ids = await api.traeSessionEditNewIds(1);
        const item = emptySegmentItem(role, ids[0] ?? "");
        setCell(ref, "content", appendAtPath(row["content"], segmentContainerPath(role), item));
      } catch (e) {
        toast.error("新增一段失败", { description: typeof e === "string" ? e : String(e) });
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [doc],
  );

  const markDirty = (names: Iterable<string>) => {
    setDirtyTables((prev) => {
      const next = new Set(prev);
      let changed = false;
      for (const n of names) {
        if (!next.has(n)) {
          next.add(n);
          changed = true;
        }
      }
      return changed ? next : prev;
    });
  };

  /** 复制若干行（可顺便清空正文）→ 追加到各自表尾，身份列换新 id。 */
  const duplicateRefs = React.useCallback(
    async (refs: RowRef[], blank: boolean): Promise<Record<string, string>> => {
      if (!doc || refs.length === 0) return {};
      setBusy(true);
      try {
        const need = identityValuesToRemap(doc, refs).length;
        const ids = need > 0 ? await api.traeSessionEditNewIds(need) : [];
        const res = duplicateRows(doc, refs, ids, { blank });
        const secs = nowSeconds();
        let next = res.doc;
        for (const nr of res.newRefs) {
          next = updateRow(next, nr, (row, t) => {
            const copy = { ...row };
            stampRow(copy, t.columns, secs);
            return copy;
          });
        }
        markDirty(res.newRefs.map((r) => r.table));
        setDoc(next);
        return res.idMap;
      } catch (e) {
        toast.error("复制失败", { description: typeof e === "string" ? e : String(e) });
        return {};
      } finally {
        setBusy(false);
      }
    },
    [doc],
  );

  const deleteRefs = React.useCallback(
    (refs: RowRef[]) => {
      if (!doc || refs.length === 0) return;
      markDirty(refs.map((r) => r.table));
      setDoc(deleteRowRefs(doc, refs));
    },
    [doc],
  );

  const loadRefContent = React.useCallback(
    async (ref: RowRef, column: string, meta: JsonRefMeta) => {
      if (!doc) return;
      try {
        const r = await api.traeSessionEditCell(
          clientKey,
          doc.sessionId,
          ref.table,
          column,
          meta.pk,
        );
        // 内容原地写回 —— 这一格的引用消失，之后就能像普通字段一样改。
        setCell(ref, column, r.value);
        toast.success(`已加载 ${column}`, {
          description: `${(r.bytes / 1048576).toFixed(1)} MB，现在可以直接改`,
        });
      } catch (e) {
        toast.error("加载大字段失败", { description: typeof e === "string" ? e : String(e) });
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [clientKey, doc],
  );

  /** 「展开正文」：`content` 是按引用返回的大字段时，把它取回来。 */
  const loadBody = React.useCallback(
    (ref: RowRef) => {
      const row = doc ? tableOf(doc, ref.table)?.rows[ref.index] : undefined;
      if (!row) return;
      const meta = readJsonRef(row["content"]);
      if (meta) void loadRefContent(ref, "content", meta);
    },
    [doc, loadRefContent],
  );

  /** 新增回合 = 以最后一个回合为模板整组复制 + 清空正文。 */
  const addTurn = async () => {
    if (!view) return;
    const tpl = view.turns[view.turns.length - 1];
    if (!tpl) {
      toast.error("这条会话里还没有任何回合，没法构造新回合", {
        description:
          "新回合是「以最后一个回合为模板、整组复制后清空正文」得到的 —— 空会话没有可复制的表结构。",
      });
      return;
    }
    const map = await duplicateRefs(turnRefs(tpl), true);
    const newKey = tpl.turnId ? map[tpl.turnId] : null;
    if (newKey) {
      setPane("turn");
      setSelTurn(newKey);
    }
    toast.success("已新增一个回合", {
      description: "正文已清空、时间已置为当前；写回前可以先在右栏改任意字段。",
    });
  };

  /**
   * 删掉**整条会话**（不是删回合）。
   *
   * ⚠️ 为什么必须有这条路：真机取证（2026-10-10）——
   *    把一条会话的 `chat_turn` / `chat_message` 全部删光、只留下 `chat_session` 那一行，
   *    客户端打开它会一直卡在「环境创建中」并**发不出任何消息**
   *    （`Session messages load failed: 0 0` → `[MessageInput] Please wait for Env Creating`）。
   *    对照组：同一天删空之前每次启动 0 次该告警，删空之后 2~5 次。客户端没有自愈路径。
   *
   * 复用记录页那条已经过真机验证的删除链路（整库备份 → 删行 → 增量加密回写 → 文件进回收站），
   * 不在这里另写一套删库逻辑。
   */
  const deleteWholeSession = async () => {
    if (!picked) return;
    setBusy(true);
    try {
      const res = await api.traeDeleteSessions(clientKey, [picked]);
      toast.success("已删掉整条会话", {
        description: `共移除 ${res.deleted_rows_total} 行；已整库备份，客户端侧文件移入回收站。`,
      });
      onChanged?.();
      onOpenChange(false);
    } catch (e) {
      toast.error("删除会话失败", { description: typeof e === "string" ? e : String(e) });
    } finally {
      setBusy(false);
    }
  };

  /** 「删到空」这条路一律改道成「删整条会话」，并把原因讲清楚。 */
  const askDeleteWholeSession = (why: string) => {
    setConfirm({
      title: "不能把会话删空 —— 改成删掉整条会话？",
      body: (
        <div className="space-y-1.5 text-xs">
          <p>{why}</p>
          <p>
            把一条会话的回合 / 消息<b>全部删光、只留下一行会话记录</b>，客户端打开它会一直卡在
            「环境创建中」，<b>再也发不出任何消息</b>（客户端没有自愈路径，只能整条删掉）。
          </p>
          <p className="text-muted-foreground">
            所以这里不提供「删到空」这个选项：确认后走「彻底删除会话」——
            整库备份 → 删除该会话的全部数据行 → 加密回写 → 客户端侧文件移入回收站。
            <b>编辑器里未保存的改动会一并丢弃。</b>
          </p>
        </div>
      ),
      run: () => {
        void deleteWholeSession();
      },
    });
  };

  /**
   * 批量删回合（单个删除也走这里 —— 传一个 key 即可）。
   *
   * ⚠️ 「一个回合名下有哪些行」只许有一份推导（`turnRefs` → `deleteRowRefs`）：
   * 单回合版另写一遍的话，两处必然在某次调整里跑偏一次，而症状是「删完客户端里还留着一半」。
   */
  const deleteTurns = (keys: string[]) => {
    if (!doc || !view || keys.length === 0) return;
    const gone = new Set(keys);
    const picks = view.turns.filter((t) => gone.has(t.key));
    if (picks.length === 0) return;
    // 同一行可能被两个回合的 `extra` 同时指到 —— `deleteRowRefs` 按 (表, 行号) 去重，重复无害。
    deleteRefs(picks.flatMap((t) => turnRefs(t)));
    setMultiSel((prev) => {
      const n = new Set(prev);
      for (const k of keys) n.delete(k);
      return n;
    });
    // `selTurn` 指向被删的回合时不用手动兜底：上面那个 effect 会自动退到第一个回合。
  };

  const askDeleteTurns = (keys: string[]) => {
    if (!view || keys.length === 0) return;
    const gone = new Set(keys);
    const picks = view.turns.filter((t) => gone.has(t.key));
    if (picks.length === 0) return;
    // ⚠️⚠️ 空会话判定必须按「**一次要删的全部回合**」算，不能逐个问「剩几个」：
    //     逐个算时每次剩下的都 ≥1，合起来却归 0 —— 那就直接写出一个空壳会话了。
    //     空壳会话是客户端死穴（打开永远卡「环境创建中」、发不出消息、无自愈路径）。
    const left = turnsLeftAfterRemoving(view.turns, keys);
    if (left === 0) {
      askDeleteWholeSession(
        picks.length === 1
          ? `这条会话只剩回合 ${picks[0].seq} 这一个回合。`
          : `这次要删的 ${picks.length} 个回合，是这条会话剩下的全部回合。`,
      );
      return;
    }
    const refCount = picks.reduce((n, t) => n + turnRefs(t).length, 0);
    setConfirm({
      title:
        picks.length === 1 ? `删除回合 ${picks[0].seq}？` : `删除选中的 ${picks.length} 个回合？`,
      body: (
        <div className="space-y-1 text-xs">
          <p>
            会把
            {picks.length === 1
              ? `该回合名下的 ${refCount} 行`
              : `这 ${picks.length} 个回合名下的 ${refCount} 行`}
            一起从本次编辑里移除（含回合本体、用户提问、助手回答，以及挂在它们下面的关联表行）。
          </p>
          <p className="text-muted-foreground">
            这只是**改这份编辑结果**，不会立刻动库 —— 但写入之后就是真的删掉了。
            写入前仍可「撤销改动」。
          </p>
          {/* ⚠️ 提问原文可能很长（整段提示词 / 一长串附件名）：`break-all` 自适应换行，
              不能用 `truncate` —— AlertDialog 是 grid + auto 列，`nowrap` 的长行会把弹窗撑破。 */}
          <div className="max-h-24 space-y-0.5 overflow-auto font-mono text-[10px]">
            {picks.map((t) => (
              <div key={t.key} className="break-all whitespace-pre-wrap">
                #{t.seq} {t.question || "（空提问）"}
              </div>
            ))}
          </div>
        </div>
      ),
      run: () => deleteTurns(keys),
    });
  };

  /** 多选态：切换某个回合的勾选（顺带把右栏切过去，边勾边看）。 */
  const toggleTurn = (key: string) => {
    setMultiSel((prev) => {
      const n = new Set(prev);
      if (n.has(key)) n.delete(key);
      else n.add(key);
      return n;
    });
    setPane("turn");
    setSelTurn(key);
  };

  /** 单回合 🗑 与批量「删除选中」走的是同一条路（见 `deleteTurns` 的注释）。 */
  const askDeleteTurn = (turn: TurnView) => askDeleteTurns([turn.key]);

  const undoAll = () => {
    if (!baseDoc) return;
    setDoc(JSON.parse(baseDoc) as TraeEditDoc);
    setDirtyTables(new Set());
    setManualDirty(false);
    setRawError(null);
    setSelMsg(new Set());
    setMultiSel(new Set());
    toast.success("已撤销本次全部改动");
  };

  const applyRaw = () => {
    try {
      const parsed = JSON.parse(rawText) as TraeEditDoc;
      if (!parsed || typeof parsed !== "object" || !Array.isArray(parsed.tables)) {
        throw new Error("顶层必须是含 tables 数组的对象");
      }
      setDoc(parsed);
      setRawError(null);
      setRawView(false);
      // 整段 JSON 判断不出改了哪张表 ⇒ 退化成「全部重写」（安全但慢一点）。
      setManualDirty(true);
      toast.success("已按源码视图改写文档", {
        description: "整段改写无法定位到具体表，写入时会全部重写",
      });
    } catch (e) {
      setRawError(typeof e === "string" ? e : String(e));
    }
  };

  const dirty = manualDirty || dirtyTables.size > 0;
  /**
   * 是否有「待写入的动作」。
   * ⚠️ 与 `dirty` 不是一回事：**原样复制到另一个账号**是合法操作，此时一个字段都没改。
   * ⚠️ 跨客户端模式（数据迁移页）同理：原样搬过去也是合法操作，判据换成「文档读出来了 + 选好了目标账号」。
   */
  const pendingWrite = cross
    ? doc !== null && crossKey !== "" && !cross.blocked
    : dirty || (mode === "insert" && targetUid !== "");
  const dirtyHint = React.useMemo<string[] | undefined>(
    () => (manualDirty ? undefined : Array.from(dirtyTables)),
    [manualDirty, dirtyTables],
  );

  /**
   * 写回**源**会话（`overwrite` / `insert`）。返回是否成功。
   *
   * 跨客户端搬迁也复用它，只是固定按 `overwrite` 走 —— `trae_handoff` 读的是 Trae 库里的内容，
   * 绕不过去，必须先把改动落到库里。
   */
  const saveToSource = async (m: Mode): Promise<boolean> => {
    if (!doc) return false;
    if (report && report.errors.length > 0) {
      toast.error("校验未通过，未写入", { description: `${report.errors.length} 处硬性问题待修` });
      return false;
    }
    setSaving(true);
    setSaveLog([]);
    const crossAccount = m === "insert" && targetUid !== "";
    const t = toast.loading(
      m === "overwrite"
        ? "正在覆写会话（解密整库 + 增量回写，约几十秒）…"
        : crossAccount
          ? "正在写入到目标账号（会改写工程归属）…"
          : "正在写入新会话…",
    );
    try {
      const r = await api.traeSessionEditSave(
        doc,
        m,
        dirtyHint,
        crossAccount ? targetUid : undefined,
      );
      toast.dismiss(t);
      toast.success(m === "overwrite" ? `已覆写：${r.sessionId}` : `已新增会话：${r.sessionId}`, {
        description: [
          `写入 ${r.rows} 行 / 重写 ${r.changedPages} 页`,
          r.targetUid ? `归属账号已改写到 …${r.targetUid.slice(-6)}` : null,
          r.skippedTables.length > 0 ? `未改动的 ${r.skippedTables.length} 张表原样保留` : null,
          r.walMerged > 0 ? `合并 WAL ${r.walMerged} 帧` : null,
          r.backup.length > 0 ? `写入前已备份 ${r.backup.length} 个文件` : null,
          r.relaunched ? "客户端已重启" : "客户端未自动重启",
        ]
          .filter(Boolean)
          .join(" · "),
      });
      setSaveLog([
        `写入完成：${r.sessionId}`,
        `行数 ${r.rows}${r.removedRows > 0 ? `（先清除旧数据 ${r.removedRows} 行）` : ""}，重写 ${r.changedPages}/${r.pages} 页`,
        ...(r.targetUid ? [`归属账号：${r.targetUid}（工程归属已同步改写）`] : []),
        ...(r.skippedTables.length ? [`未改动、原样保留：${r.skippedTables.join(", ")}`] : []),
        ...(r.backup.length ? [`备份：${r.backup[0]}`] : []),
      ]);
      if (r.idRemapped) {
        setPicked(r.sessionId);
      } else {
        await loadDoc(r.sessionId);
      }
      onChanged?.();
      return true;
    } catch (e) {
      toast.dismiss(t);
      const msg = typeof e === "string" ? e : String(e);
      setSaveLog(msg.split("\n"));
      toast.error("写入失败", { description: msg.split("\n")[0] });
      return false;
    } finally {
      setSaving(false);
    }
  };

  /**
   * 跨客户端搬迁（数据迁移页）：① 有改动先写回源会话 ② 再走既有导出链路搬到目标账号。
   *
   * ⚠️ 顺序不能反：② 读的是 Trae 库里的内容，不先落 ① 就等于把**没改过的**版本搬过去。
   */
  const pushCross = async () => {
    if (!doc || !cross) return;
    // ⚠️ 按**复合键**取账号：同一 uid 在两个客户端下各有一份，只比 uid 会挑错客户端。
    const acc = crossTarget;
    if (!acc) {
      toast.error(`请先选择要写入的 ${cross.toLabel} 账号`);
      return;
    }
    setSaving(true);
    setSaveLog([`目标：${cross.toLabel} · ${acc.clientLabel} · ${acc.label}`]);
    try {
      if (dirty) {
        setSaveLog((prev) => [...prev, "① 先把改动写回这条 Trae 会话（导出读的是库里的内容）…"]);
        const ok = await saveToSource("overwrite");
        if (!ok) {
          setSaveLog((prev) => [...prev, "改动没写成功，已中止搬迁（源会话保持原样）"]);
          return;
        }
      } else {
        setSaveLog((prev) => [...prev, "① 没有任何改动，直接用库里的内容搬迁"]);
      }
      setSaveLog((prev) => [...prev, `② 正在导出到 ${cross.toLabel} · ${acc.label} …`]);
      const msg = await cross.push(acc);
      setSaveLog((prev) => [...prev, `② ${msg}`]);
      toast.success(`已写入到 ${acc.label}`, { description: msg });
      onChanged?.();
      await loadDoc(doc.sessionId);
    } catch (e) {
      const m = typeof e === "string" ? e : String(e);
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

  // ---- 批量操作 ----
  /** 勾选的消息对象（跨回合找）。 */
  const selectedMessages: TurnMessage[] = React.useMemo(() => {
    if (!view) return [];
    const all: TurnMessage[] = [];
    for (const t of view.turns) {
      if (t.user) all.push(t.user);
      if (t.assistant) all.push(t.assistant);
    }
    all.push(...view.orphans);
    return all.filter((m) => selMsg.has(m.id));
  }, [view, selMsg]);

  const selectedRefs: RowRef[] = React.useMemo(
    () => selectedMessages.flatMap((m) => messageRefs(m)),
    [selectedMessages],
  );

  const choices = React.useMemo(
    () => (doc ? columnChoices(doc, selectedRefs) : []),
    [doc, selectedRefs],
  );

  React.useEffect(() => {
    if (!batchOpen) return;
    setBatchColumn(choices.find((c) => c.everyRow)?.name ?? choices[0]?.name ?? "");
  }, [batchOpen, choices]);

  const runBatchSet = () => {
    if (!doc) return;
    const col = choices.find((c) => c.name === batchColumn);
    if (!col) {
      toast.error("先选一列");
      return;
    }
    setDoc(batchSetColumn(doc, selectedRefs, col.name, parseByColumn(col, batchValue)));
    markDirty(selectedRefs.map((r) => r.table));
    toast.success(`已把 ${selectedRefs.length} 行的 ${col.name} 改成同一个值`);
    setBatchOpen(false);
  };

  const runBatchReplace = () => {
    if (!doc) return;
    if (!batchFind) {
      toast.error("先填要查找的内容");
      return;
    }
    const r = batchReplaceText(
      doc,
      selectedRefs,
      batchColumn || null,
      batchFind,
      batchRepl,
      batchCs,
    );
    setDoc(r.doc);
    markDirty(selectedRefs.map((x) => x.table));
    // ⚠️ 必须如实报替换次数：「说替换了但一个都没中」是最容易被当成成功的一种失败。
    if (r.replaced === 0) {
      toast.warning("一处都没替换到", {
        description: "检查一下查找内容、以及选中的列（JSON 列只替换里面的字符串值）。",
      });
    } else {
      toast.success(`已替换 ${r.replaced} 处`);
    }
    setBatchOpen(false);
  };

  const runBatchMoveOrCopy = async () => {
    if (!doc || !view) return;
    const target = view.turns.find((t) => t.key === batchTarget);
    if (!target || !target.turn) {
      toast.error("先选目标回合");
      return;
    }
    // 只有 user / assistant 两种角色有明确的「挂载点」。
    const unknown = selectedMessages.find((m) => m.role !== "user" && m.role !== "assistant");
    if (unknown) {
      toast.error("有消息的角色不是 user / assistant，无法挂到回合上", {
        description: `message_id ${unknown.id} 的 message_role = ${unknown.role || "（空）"}`,
      });
      return;
    }
    // 目标回合对应的挂载点已被占用时**拒绝**，绝不静默覆盖 —— 这一步会丢内容。
    const occupied = selectedMessages.filter((m) => {
      const t = tableOf(doc, target.turn!.table);
      const row = t?.rows[target.turn!.index];
      if (!row) return true;
      const field = m.role === "user" ? "reply_to_message_id" : "response_message_id";
      const cur = row[field];
      return typeof cur === "string" && cur !== "" && cur !== m.id;
    });
    if (occupied.length > 0) {
      toast.error("目标回合的挂载点已被占用，未执行", {
        description: `回合 ${target.seq} 的 ${
          occupied[0].role === "user" ? "reply_to_message_id" : "response_message_id"
        } 已经是别的消息了。请先把它清掉，或换成另一个回合。`,
      });
      return;
    }

    // ⚠️ 这里**不能**用上面那个 `duplicateRefs`：它内部会 `setDoc`，而 React 的 setState
    // 是异步的 —— 循环里接着用外层那个已经过期的 `doc` 去改指针，会把刚追加的副本整批覆盖掉。
    // 所以这一趟自己维护 `cur`，最后一次性落回 state。
    let cur = doc;
    const touched = new Set<string>(["chat_turn"]);
    let moved = 0;
    for (const m of selectedMessages) {
      let mid = m.id;
      if (batchHow === "duplicate") {
        const refs = messageRefs(m);
        const need = identityValuesToRemap(cur, refs).length;
        const ids = need > 0 ? await api.traeSessionEditNewIds(need) : [];
        const res = duplicateRows(cur, refs, ids, { blank: false });
        const secs = nowSeconds();
        for (const nr of res.newRefs) {
          cur = updateRow(cur, nr, (row, t) => {
            const copy = { ...row };
            stampRow(copy, t.columns, secs);
            return copy;
          });
          touched.add(nr.table);
        }
        const fresh = res.idMap[m.id];
        if (!fresh) continue;
        mid = fresh;
      }
      const field = m.role === "user" ? "reply_to_message_id" : "response_message_id";
      cur = updateRow(cur, target.turn, (row) => ({ ...row, [field]: mid }));
      if (batchHow === "move") {
        // 源回合的挂载点清空（只清「本来就指向这条消息」的那一个，别动别人的）。
        const src = view.turns.find((t) => t.user?.id === m.id || t.assistant?.id === m.id);
        if (src?.turn && src.turn !== target.turn) {
          cur = updateRow(cur, src.turn, (row) =>
            row[field] === m.id ? { ...row, [field]: "" } : row,
          );
        }
      }
      moved += 1;
    }
    setDoc(cur);
    markDirty(touched);
    toast.success(
      batchHow === "duplicate"
        ? `已复制 ${moved} 条消息到回合 ${target.seq}`
        : `已移动 ${moved} 条消息到回合 ${target.seq}`,
      { description: "写回前可以先在右栏核对。" },
    );
    setBatchOpen(false);
    setSelMsg(new Set());
  };

  const askDeleteMessages = () => {
    if (selectedRefs.length === 0) return;
    setConfirm({
      title: `删除选中的 ${selectedMessages.length} 条消息？`,
      body: (
        <div className="space-y-1 text-xs">
          <p>
            会移除这 <b>{selectedRefs.length}</b> 行（消息主行 + 它名下所有关联表的行）。
          </p>
          <p className="text-muted-foreground">
            这是改这份编辑结果，写入之后才是真的从库里删掉。写入前仍可「撤销改动」。
            要在客户端里彻底删掉整条会话，去记录页用「删除所选」。
          </p>
          <pre className="bg-muted/50 max-h-24 overflow-auto rounded-md p-2 font-mono text-[10px]">
            {selectedMessages
              .slice(0, 8)
              .map((m) => `${m.role || "?"}  ${m.id}`)
              .join("\n")}
            {selectedMessages.length > 8 ? `\n…另有 ${selectedMessages.length - 8} 条` : ""}
          </pre>
        </div>
      ),
      run: () => deleteRefs(selectedRefs),
    });
  };

  const errorCount = report?.errors.length ?? 0;
  const warnCount = report?.warnings.length ?? 0;
  const docWarns = doc?.warnings ?? [];
  const turnIdx = view?.turns.findIndex((t) => t.key === currentTurn?.key) ?? -1;

  const toolbar =
    selectedMessages.length > 0 ? (
      <div className="bg-muted/40 flex flex-wrap items-center gap-2 rounded-md border px-2 py-1.5">
        <span className="text-xs">
          已选 <b>{selectedMessages.length}</b> 条消息 / {selectedRefs.length} 行
        </span>
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="h-7 gap-1 text-[11px]"
          onClick={() => setBatchOpen(true)}
        >
          <Wand2 className="size-3" />
          批量修改
        </Button>
        <Button
          type="button"
          variant="destructive"
          size="sm"
          className="h-7 gap-1 text-[11px]"
          onClick={askDeleteMessages}
        >
          <Trash2 className="size-3" />
          删除选中
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 text-[11px]"
          onClick={() => setSelMsg(new Set())}
        >
          取消勾选
        </Button>
      </div>
    ) : null;

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <ResizableDialogContent storageKey="trae-session-edit" className="sm:max-w-6xl">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Layers className="size-4" />
              会话编辑器
              <Badge variant="secondary" className="text-[10px]">
                {clientLabel || clientKey}
              </Badge>
              {/* ⚠️ 判据是 `dirty` 而不是 `pendingWrite`：跨客户端模式下「原样搬迁」也待写入，
                  但那不代表有改动，用 pendingWrite 会让徽标一直亮着说假话。 */}
              {dirty && (
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
                <Button
                  type="button"
                  variant={rawView ? "default" : "ghost"}
                  size="sm"
                  className="size-6 p-0"
                  title="源码视图（整份 doc 的 JSON 文本，可直接粘贴新结构）"
                  onClick={() => {
                    if (!doc) return;
                    if (!rawView) setRawText(JSON.stringify(doc, null, 2));
                    setRawError(null);
                    setRawView((v) => !v);
                  }}
                >
                  <Code2 className="size-3" />
                </Button>
              </span>
            </DialogTitle>
            <DialogDescription className="break-all font-mono text-xs">
              {doc?.sessionId ?? "—"}
              {doc?.ownerUid ? ` · 归属 uid ${doc.ownerUid}` : " · 无归属账号"}
              {view ? ` · ${view.turns.length} 个回合` : ""}
            </DialogDescription>
          </DialogHeader>

          {/* 两栏。⚠️ 两栏都要 min-w-0，否则长内容会把列压成 0 宽（症状：文字竖排）。 */}
          <div className="grid min-h-0 grid-cols-1 gap-3 md:grid-cols-[minmax(240px,320px)_minmax(0,1fr)]">
            {/* ---- 左栏：当前会话（只读显示）+ 回合列表 ---- */}
            <div className="flex min-h-0 min-w-0 flex-col gap-2 rounded-lg border p-2">
              {/* ⚠️ 这里原本是个会话下拉（可以就地切到别的会话），已按要求取消。
                  改成一行**只读**的会话标识 —— 保留它是为了让用户随时能确认
                  「我现在编的到底是哪条」，但不再提供切换动作（换会话＝关掉点另一行）。 */}
              <div className="flex shrink-0 items-start gap-1">
                <div className="min-w-0 flex-1">
                  <div className="truncate text-xs font-medium" title={doc?.title || undefined}>
                    {doc?.title || (picked ? "（无标题）" : "未指定会话")}
                  </div>
                  <div className="truncate font-mono text-[10px] text-muted-foreground" title={picked ?? undefined}>
                    {picked ?? "—"}
                  </div>
                </div>
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  className="size-8 shrink-0 p-0"
                  title="重新读取当前会话"
                  disabled={!picked || loading}
                  onClick={() => picked && void loadDoc(picked)}
                >
                  <RefreshCw className={cn("size-3", loading && "animate-spin")} />
                </Button>
              </div>

              <div className="relative shrink-0">
                <Search className="text-muted-foreground absolute top-1/2 left-2 size-3 -translate-y-1/2" />
                <Input
                  className="h-8 pl-7 text-xs"
                  placeholder="搜索回合（提问内容 / turn_id / 状态）"
                  value={turnFilter}
                  onChange={(e) => setTurnFilter(e.target.value)}
                />
              </div>

              {/* 批量删除回合：多选态下**点整行 = 切换勾选**（顺带把右栏切过去，边勾边看）。
                  逐个的 🗑 在多选态下同样可用，两者不互斥。
                  ⚠️「全选」只作用于**搜索结果里可见的**回合 —— 列表勾选 ≠ 可见行。 */}
              <div className="flex shrink-0 flex-wrap items-center gap-1.5">
                <Button
                  type="button"
                  variant={multi ? "default" : "outline"}
                  size="sm"
                  className="h-7 gap-1 px-2 text-[11px]"
                  title={multi ? "退出多选" : "勾选多个回合后一次性删除"}
                  disabled={!doc || !view || loading || busy || view.turns.length === 0}
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
                      disabled={filteredTurns.length === 0 || busy}
                      onClick={() => setMultiSel(new Set(filteredTurns.map((t) => t.key)))}
                    >
                      全选
                    </Button>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-[11px]"
                      disabled={multiSel.size === 0 || busy}
                      onClick={() => setMultiSel(new Set())}
                    >
                      清空
                    </Button>
                    <Button
                      type="button"
                      variant="destructive"
                      size="sm"
                      className="ml-auto h-7 gap-1 px-2 text-[11px]"
                      disabled={multiSel.size === 0 || busy}
                      onClick={() => askDeleteTurns([...multiSel])}
                    >
                      <Trash2 className="size-3" />
                      删除选中（{multiSel.size}）
                    </Button>
                  </>
                )}
              </div>

              <Button
                type="button"
                variant="outline"
                size="sm"
                className="w-full shrink-0 gap-1 text-xs"
                disabled={!doc || busy || loading}
                onClick={() => void addTurn()}
                title="以最后一个回合为模板整组复制，清空正文后追加到末尾"
              >
                <Plus className="size-3" />
                新增回合
              </Button>

              <div className="min-h-0 flex-1 space-y-1 overflow-auto pr-1">
                {loading ? (
                  <p className="text-muted-foreground p-2 text-xs">
                    正在读取（大字段按需加载）…
                  </p>
                ) : loadError ? (
                  <p className="text-destructive p-2 text-xs">读取失败，见右侧说明</p>
                ) : !view ? (
                  <p className="text-muted-foreground p-2 text-xs">
                    {picked ? "正在准备会话内容…" : "没有指定要编辑的会话。"}
                  </p>
                ) : view.turns.length === 0 ? (
                  <p className="text-muted-foreground p-2 text-xs">
                    这条会话没有任何 `chat_turn` 行。可以点上面的「新增回合」构造一个，
                    或者去右栏看「会话级数据」。
                  </p>
                ) : filteredTurns.length === 0 ? (
                  <p className="text-muted-foreground p-2 text-xs">
                    没有匹配「{turnFilter}」的回合。
                  </p>
                ) : (
                  filteredTurns.map((t) => (
                    <div
                      key={t.key}
                      className={cn(
                        "grid grid-cols-[minmax(0,1fr)_auto] items-start gap-1 rounded-md border p-1.5",
                        multi && multiSel.has(t.key)
                          ? "border-destructive/60 bg-destructive/[0.06]"
                          : pane === "turn" && currentTurn?.key === t.key
                            ? "border-primary bg-primary/5"
                            : "border-transparent",
                      )}
                    >
                      <button
                        type="button"
                        className="min-w-0 text-left"
                        onClick={() => {
                          if (multi) {
                            // 多选态下点整行 = 切换勾选（不退出多选态）。
                            toggleTurn(t.key);
                          } else {
                            setPane("turn");
                            setSelTurn(t.key);
                          }
                        }}
                      >
                        <div className="flex min-w-0 items-center gap-1">
                          {/* 勾选框画在**回合按钮内部**，但只是纯展示的 `span`（不是交互元素）——
                              这样点整行任意位置都能切换勾选，也不会有「按钮里套按钮」的非法结构。 */}
                          {multi && (
                            <span
                              aria-hidden
                              data-turn-check={multiSel.has(t.key) ? "1" : "0"}
                              className={cn(
                                "flex size-3.5 shrink-0 items-center justify-center rounded-[3px] border",
                                multiSel.has(t.key)
                                  ? "border-destructive bg-destructive text-white"
                                  : "border-muted-foreground/60",
                              )}
                            >
                              {multiSel.has(t.key) && <Check className="size-2.5" />}
                            </span>
                          )}
                          <span className="text-muted-foreground shrink-0 text-[10px]">
                            #{t.seq}
                          </span>
                          <span
                            className={cn(
                              "size-1.5 shrink-0 rounded-full",
                              t.status === "completed" ? "bg-emerald-500" : "bg-amber-500",
                            )}
                            title={`turn_status = ${t.status || "（空）"}`}
                          />
                          <span
                            className={cn(
                              "truncate text-xs",
                              t.dangling ? "text-muted-foreground italic" : "",
                            )}
                            title={t.question}
                          >
                            {t.question}
                          </span>
                        </div>
                        <div className="text-muted-foreground truncate font-mono text-[10px]">
                          {(t.turnId ?? "无回合行").slice(0, 12)}
                          {t.dangling ? " · 引用缺失" : ""}
                        </div>
                      </button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        className="text-destructive size-6 shrink-0 p-0"
                        title="删除这个回合（写回前可撤销）"
                        onClick={() => askDeleteTurn(t)}
                      >
                        <Trash2 className="size-3" />
                      </Button>
                    </div>
                  ))
                )}
              </div>

              {/* 散落消息 / 会话级数据 —— 不属于任何回合的行，给两个固定入口，别让它们无处可去。 */}
              {view && (view.orphans.length > 0 || view.sessionScope.length > 0) ? (
                <div className="shrink-0 space-y-1 border-t pt-2">
                  {view.orphans.length > 0 ? (
                    <Button
                      type="button"
                      variant={pane === "orphans" ? "default" : "outline"}
                      size="sm"
                      className="w-full justify-start gap-1 text-xs"
                      onClick={() => setPane("orphans")}
                    >
                      <AlertTriangle className="size-3" />
                      散落消息（{view.orphans.length}）
                    </Button>
                  ) : null}
                  <Button
                    type="button"
                    variant={pane === "session" ? "default" : "outline"}
                    size="sm"
                    className="w-full justify-start gap-1 text-xs"
                    onClick={() => setPane("session")}
                  >
                    <Layers className="size-3" />
                    会话级数据（{view.sessionScope.reduce((n, g) => n + g.refs.length, 0)} 行）
                  </Button>
                </div>
              ) : null}
            </div>

            {/* ---- 右栏 ---- */}
            <div className="flex min-h-0 min-w-0 flex-col gap-2">
              {loading ? (
                <div className="text-muted-foreground flex flex-1 items-center justify-center gap-2 text-sm">
                  <Loader2 className="size-4 animate-spin" /> 正在读取该会话的全部字段（大字段按需加载）…
                </div>
              ) : loadError ? (
                /* ⚠️ 读取失败必须与「空」区分开：不摆空态卡，直接说明这是读取失败。 */
                <Alert variant="destructive" className="shrink-0">
                  <AlertTriangle className="size-4" />
                  <AlertTitle>读取失败（不是数据被删）</AlertTitle>
                  <AlertDescription className="break-all text-xs">
                    {loadError}
                    <div className="mt-2">
                      <Button
                        type="button"
                        variant="outline"
                        size="sm"
                        onClick={() => picked && void loadDoc(picked)}
                      >
                        重试
                      </Button>
                    </div>
                  </AlertDescription>
                </Alert>
              ) : rawView ? (
                <>
                  <div className="flex flex-wrap items-center gap-2">
                    <Button type="button" size="sm" variant="outline" onClick={applyRaw}>
                      应用源码
                    </Button>
                    <Button
                      type="button"
                      size="sm"
                      variant="ghost"
                      onClick={() => setRawView(false)}
                    >
                      返回表单
                    </Button>
                    {rawError && <span className="text-destructive text-xs">JSON 有误：{rawError}</span>}
                  </div>
                  <textarea
                    className="border-input focus-visible:border-ring min-h-0 w-full flex-1 rounded-md border p-2 font-mono text-xs outline-none"
                    value={rawText}
                    spellCheck={false}
                    onChange={(e) => setRawText(e.target.value)}
                  />
                </>
              ) : !doc || !view ? (
                <div className="text-muted-foreground flex flex-1 items-center justify-center text-sm">
                  从左上角选一条会话开始编辑
                </div>
              ) : (
                <TraeTurnDetail
                  doc={doc}
                  mode={pane}
                  turn={currentTurn}
                  orphans={view.orphans}
                  sessionScope={view.sessionScope}
                  selected={selMsg}
                  onToggleMessage={(id) =>
                    setSelMsg((prev) => {
                      const next = new Set(prev);
                      if (next.has(id)) next.delete(id);
                      else next.add(id);
                      return next;
                    })
                  }
                  onRowChange={setRow}
                  onRowDuplicate={(refs, blank) => void duplicateRefs(refs, blank)}
                  onRowDelete={(refs) =>
                    setConfirm({
                      title: `删除这 ${refs.length} 行？`,
                      body: (
                        <p className="text-xs">
                          只从本次编辑结果里移除，写入后才是真的删掉；写入前可「撤销改动」。
                        </p>
                      ),
                      run: () => deleteRefs(refs),
                    })
                  }
                  onLoadRef={loadRefContent}
                  onSaveSegment={saveSegment}
                  onDeleteSegment={deleteSegment}
                  onAddSegment={(ref, role) => void addSegment(ref, role)}
                  onLoadBody={loadBody}
                  onPrev={() => {
                    if (turnIdx > 0) setSelTurn(view.turns[turnIdx - 1].key);
                  }}
                  onNext={() => {
                    if (turnIdx >= 0 && turnIdx < view.turns.length - 1) {
                      setSelTurn(view.turns[turnIdx + 1].key);
                    }
                  }}
                  hasPrev={turnIdx > 0}
                  hasNext={turnIdx >= 0 && turnIdx < view.turns.length - 1}
                  toolbar={toolbar}
                  busy={busy}
                />
              )}
            </div>
          </div>

          {/* 页脚上方：长文本（校验明细 / 写入日志）必须放这儿 —— 塞进 DialogFooter 在窄屏会压成「一个字一行」 */}
          {(errorCount > 0 || warnCount > 0 || docWarns.length > 0 || saveLog.length > 0) && (
            <div className="max-h-40 shrink-0 space-y-2 overflow-auto">
              {docWarns.length > 0 && (
                <div className="text-muted-foreground rounded-md border border-dashed px-2 py-1.5 text-[11px]">
                  {docWarns.map((w, i) => (
                    <div key={i} className="break-all">
                      {w}
                    </div>
                  ))}
                </div>
              )}
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

          {/* S2 · 跨账号写入必须有**独立提示**：这一步会同时改写工程归属，
              不能只在 Select 里悄悄换个值。 */}
          {doc && !cross && mode === "insert" && targetUid !== "" ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertTitle>将写入另一个账号</AlertTitle>
              <AlertDescription className="text-xs">
                新会话归属到「{targets.find((a) => a.uid === targetUid)?.label ?? targetUid}」：
                会复用（或新建）该账号下的同路径工程，并把 chat_session.project_id、
                session_project.project_id、context.last_real_project_id 三处一起改 ——
                漏一处客户端就删不掉这条会话。源会话不受影响。
              </AlertDescription>
            </Alert>
          ) : null}

          {/* T46 · S5 · 跨客户端搬迁（数据迁移页）：必须写明「源会话会被改写」——
              用户以为只是「把改过的那份搬过去」，其实导出链路读的是库里的内容。 */}
          {doc && cross && crossTarget ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertTitle>写入到另一个客户端：{cross.toLabel}</AlertTitle>
              <AlertDescription className="text-xs leading-5">
                {dirty
                  ? "会先把你的改动就地覆写回这条 Trae 会话（导出读的是库里的内容，绕不过去），"
                  : "没有任何改动，直接用库里的内容"}
                再把它导出到「{crossTarget.clientLabel} · {crossTarget.label}」。源会话不会被删，
                迁移过程中 {cross.toLabel} 会被退出、写完自动拉起。
              </AlertDescription>
            </Alert>
          ) : null}

          {/* 页脚：写入方式 + 动作 */}
          <div className="flex flex-wrap items-center justify-end gap-2 border-t pt-3">
            <div className="mr-auto flex flex-wrap items-center gap-3">
              {/* ⚠️ 跨客户端模式（数据迁移页）下**整块换掉**同客户端的写入方式 / 写入到账号：
                  那几个选项在这两个页面上只会把人绕回去（用户报的「怎么还是 trae 到 trae」）。 */}
              {cross ? (
                <>
                  {/* ⚠️⚠️ **客户端与账号分开选**：Trae 国内版与国际版是两个不同客户端，
                      同一台机器上可能都装了，且本机 3 个 uid 在两个客户端下各有一份。早先把
                      「客户端 · 账号」拼成一个字符串塞进同一个下拉（用户报「trae 不应该多选账号」），
                      还按 uid 反查 ⇒ 会命中另一个客户端的同名账号。
                      只有一个客户端时不显示这一行（WorkBuddy 侧就是这种情况）。 */}
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
                    <Select value={mode} onValueChange={(v) => setMode(v as Mode)}>
                      <SelectTrigger className="h-8 w-[min(16rem,100%)] text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="overwrite">
                          就地覆写（保持会话 ID，原会话内容被替换）
                        </SelectItem>
                        <SelectItem value="insert">新增会话（自动换 ID，原会话不动）</SelectItem>
                      </SelectContent>
                    </Select>
                  </div>
                  {/* ⚠️ 覆写不允许换账号 —— 那等于把一条会话从 A 账号悄悄搬到 B 账号。后端也会拒。 */}
                  {mode === "insert" ? (
                    <div className="min-w-0">
                      <Label className="text-xs">写入到账号</Label>
                      <Select
                        value={targetUid === "" ? SAME_ACCOUNT : targetUid}
                        onValueChange={(v) => setTargetUid(v === SAME_ACCOUNT ? "" : v)}
                      >
                        <SelectTrigger
                          className="h-8 w-[min(20rem,100%)] text-xs"
                          title={targetsError ?? "写入到别的账号会复用/新建目标账号的同路径工程"}
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
              disabled={!dirty}
              onClick={undoAll}
            >
              <RotateCcw className="size-3" />
              撤销改动
            </Button>
            {/* ⚠️ 「关闭编辑」按钮已按要求取消：关闭弹窗只保留右上角的 ✕。
                这里原本做的事（清 doc / 清脏标记 / 退出源码视图）对「关掉弹窗」毫无必要 ——
                ✕ 触发 `open=false`，上面的 effect 会清掉临时选择，下次打开再由 `loadDoc` 整份重读，
                不会带着上一次的编辑结果。 */}
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
                    ? "覆写写入"
                    : "新增写入"}
            </Button>
          </div>
        </ResizableDialogContent>
      </Dialog>

      {/* 批量修改：独立弹窗（与编辑弹窗**并列**，不做嵌套模态 —— 嵌套两层 Radix 模态
          会让焦点陷阱与遮罩互相打架）。 */}
      <Dialog open={batchOpen} onOpenChange={setBatchOpen}>
        <DialogContent className="max-h-[85vh] overflow-auto sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle className="text-sm">
              批量修改（已选 {selectedMessages.length} 条消息 / {selectedRefs.length} 行）
            </DialogTitle>
            <DialogDescription className="text-xs">
              三种动作只作用在勾选的消息上。改完仍然只是改这份编辑结果，点「写入」才落库。
            </DialogDescription>
          </DialogHeader>

          <div className="space-y-4 text-xs">
            <div className="space-y-2 rounded-md border p-2">
              <div className="font-medium">① 统一改字段</div>
              <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto]">
                <Select value={batchColumn || undefined} onValueChange={setBatchColumn}>
                  <SelectTrigger className="h-8 text-xs">
                    <SelectValue placeholder="选一列" />
                  </SelectTrigger>
                  <SelectContent>
                    {choices.map((c) => (
                      <SelectItem key={c.name} value={c.name} className="text-xs">
                        {c.name}
                        {c.everyRow ? "" : "（部分行没有这一列）"}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <Input
                  className="h-8 text-xs"
                  placeholder="新值（JSON 列可以直接粘 JSON）"
                  value={batchValue}
                  onChange={(e) => setBatchValue(e.target.value)}
                />
                <Button type="button" size="sm" className="h-8" onClick={runBatchSet}>
                  应用
                </Button>
              </div>
            </div>

            <div className="space-y-2 rounded-md border p-2">
              <div className="font-medium">② 正文查找替换</div>
              <div className="grid gap-2 sm:grid-cols-2">
                <Input
                  className="h-8 text-xs"
                  placeholder="查找"
                  value={batchFind}
                  onChange={(e) => setBatchFind(e.target.value)}
                />
                <Input
                  className="h-8 text-xs"
                  placeholder="替换为"
                  value={batchRepl}
                  onChange={(e) => setBatchRepl(e.target.value)}
                />
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <Select
                  value={batchColumn === "" ? "__all__" : batchColumn}
                  onValueChange={(v) => setBatchColumn(v === "__all__" ? "" : v)}
                >
                  <SelectTrigger className="h-8 w-[min(18rem,100%)] text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="__all__">全部文本列（含 JSON 内部的字符串）</SelectItem>
                    {choices.map((c) => (
                      <SelectItem key={c.name} value={c.name} className="text-xs">
                        {c.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <div className="flex items-center gap-2">
                  <Switch checked={batchCs} onCheckedChange={setBatchCs} />
                  <span>区分大小写</span>
                </div>
                <Button type="button" size="sm" className="h-8" onClick={runBatchReplace}>
                  替换
                </Button>
              </div>
            </div>

            <div className="space-y-2 rounded-md border p-2">
              <div className="font-medium">③ 复制 / 移动到别的回合</div>
              <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_auto_auto]">
                <Select value={batchTarget || undefined} onValueChange={setBatchTarget}>
                  <SelectTrigger className="h-8 text-xs">
                    <SelectValue placeholder="选目标回合" />
                  </SelectTrigger>
                  <SelectContent>
                    {(view?.turns ?? [])
                      .filter(
                        (t) =>
                          !selectedMessages.some((m) => t.user?.id === m.id || t.assistant?.id === m.id),
                      )
                      .map((t) => (
                        <SelectItem key={t.key} value={t.key} className="text-xs">
                          #{t.seq} {t.question.slice(0, 30)}
                        </SelectItem>
                      ))}
                  </SelectContent>
                </Select>
                <Select value={batchHow} onValueChange={(v) => setBatchHow(v as "duplicate" | "move")}>
                  <SelectTrigger className="h-8 w-[7rem] text-xs">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="duplicate">复制（新 id）</SelectItem>
                    <SelectItem value="move">移动（搬运）</SelectItem>
                  </SelectContent>
                </Select>
                <Button
                  type="button"
                  size="sm"
                  className="h-8"
                  disabled={busy}
                  onClick={() => void runBatchMoveOrCopy()}
                >
                  {busy ? <Loader2 className="size-3 animate-spin" /> : null}
                  执行
                </Button>
              </div>
              <p className="text-muted-foreground text-[11px]">
                目标回合的挂载点（`reply_to_message_id` / `response_message_id`）已被占用时会**拒绝执行**，
                不会静默覆盖 —— 覆盖等于丢内容。移动时源回合的挂载点会被清空。
              </p>
            </div>
          </div>
        </DialogContent>
      </Dialog>

      {/* 破坏性动作确认（删除行 / 删除回合 / 删除选中消息）。 */}
      <AlertDialog open={confirm !== null} onOpenChange={(v) => !v && setConfirm(null)}>
        {/* ⚠️ `max-w` + `min-w-0`：确认内容里有很长的单行（提问正文 / JSON 片段），
            AlertDialog 默认是 grid + auto 列，子元素 `min-width:auto` 会被长行撑破卡片。 */}
        <AlertDialogContent className="max-w-[min(44rem,calc(100vw-2rem))]">
          <AlertDialogHeader className="min-w-0">
            <AlertDialogTitle>{confirm?.title}</AlertDialogTitle>
            <AlertDialogDescription asChild>
              <div className="min-w-0">{confirm?.body}</div>
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
              onClick={(e) => {
                e.preventDefault();
                confirm?.run();
                setConfirm(null);
              }}
            >
              <Trash2 className="size-3" />
              确认删除
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}
