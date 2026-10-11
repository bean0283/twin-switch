import * as React from "react";
import {
  AlertTriangle,
  Bot,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  Copy,
  Loader2,
  Pencil,
  Plus,
  User as UserIcon,
  Wrench,
  X,
} from "lucide-react";

import { JsonForm, RowForm, type JsonRefMeta, type RowColumnMeta } from "@/components/json-form";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import type { TraeEditDoc, TraeEditTable } from "@/lib/trae-types";
import {
  bodyOf,
  messageRefs,
  readAtPath,
  rowSummary,
  segmentsOf,
  tableOf,
  type RowRef,
  type Segment,
  type TableGroup,
  type TurnMessage,
  type TurnView,
} from "@/lib/trae-turn-model";
import { cn } from "@/lib/utils";

/**
 * T46 · 回合详情面板（**会话式视图**）。
 *
 * 用户原话：「显示的太细致了，要求跟正常会话的显示一样就行，每段话独立，点击可以编辑」。
 *
 * ⇒ 右栏第一屏**不再是表 / 列 / 原始行**，而是这条回合的**对话正文流**：
 *   用户提问一段、助手说明一段、工具调用一行、助手答复一段 —— 每段都能点开原地改。
 *   原始的「全部列 + 全部关联表」收进底部的「底层字段」折叠块（默认关），
 *   结构、id、时间戳这类东西仍然改得到（否则就违反 T46 已确认的「全字段可编辑」）。
 *
 * 纯展示 + 回调：所有状态都在 `TraeSessionEditor` 里，这里不自己改 doc。
 *
 * ⚠️ 两条必须遵守的渲染约束：
 *   1. **行数不可控** —— 真机上 `history_v2` 一条会话 1531 行，全渲染 RowForm 会当场卡死。
 *      所以每个行组默认只渲染前几行，其余用「再显示 N 行」放出来。
 *   2. **引用缺失是正常状态** —— `chat_turn` 里的 `reply_to_message_id` 在真机上存在
 *      指向不存在消息的情况。要显式渲染成「引用缺失」，不能当异常、更不能崩。
 */

/** 每个行组初始渲染几行；点「显示更多」每次加这么多。 */
const GROUP_PAGE = 8;

function toRowColumns(t: TraeEditTable): RowColumnMeta[] {
  return t.columns.map((c) => ({
    name: c.name,
    type: c.type,
    notNull: c.notNull,
    pk: c.pk,
    json: c.json,
    readOnly: c.readOnly,
  }));
}

/** 折叠块。刻意不用 `<details>/<summary>`：`summary` 里的按钮点击会连带开合，很难按。 */
function Collapse({
  title,
  subtitle,
  badge,
  actions,
  defaultOpen = false,
  children,
}: {
  title: React.ReactNode;
  subtitle?: React.ReactNode;
  badge?: React.ReactNode;
  actions?: React.ReactNode;
  defaultOpen?: boolean;
  children: React.ReactNode;
}) {
  const [open, setOpen] = React.useState(defaultOpen);
  return (
    <div className="rounded-md border">
      {/* ⚠️ 用 grid 而不是 flex-wrap：这一行含一整段可省略的文字，flex-wrap 会让它
          在窄屏下被挤成「一个字一行」（T44 的坑）。左列必须 min-w-0。 */}
      <div className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-2 px-2 py-1.5">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="size-6 shrink-0 p-0"
          title={open ? "收起这一块" : "展开这一块"}
          // ⚠️ 这个按钮只有图标，没有可读文本 ⇒ 必须给无障碍名（项目规矩：图标按钮必须带 title）。
          //    同时它让渲染自查能**唯一**地数到「本组件的折叠块」——json-form 里还有一层
          //    同名的展开/收起按钮，只按 title 数会把它们混进来（一度把 3 个记成 34 个）。
          aria-label={open ? "收起这一块" : "展开这一块"}
          onClick={() => setOpen((v) => !v)}
        >
          <ChevronDown className={cn("size-3 transition-transform", !open && "-rotate-90")} />
        </Button>
        <button
          type="button"
          className="min-w-0 text-left"
          onClick={() => setOpen((v) => !v)}
          title={typeof title === "string" ? title : undefined}
        >
          <div className="flex min-w-0 flex-wrap items-center gap-1">
            <span className="font-mono text-[11px] font-medium">{title}</span>
            {badge}
          </div>
          {subtitle ? (
            <div className="text-muted-foreground truncate text-[10px]">{subtitle}</div>
          ) : null}
        </button>
        <div className="ml-auto flex shrink-0 flex-wrap items-center justify-end gap-1">
          {actions}
        </div>
      </div>
      {open ? <div className="min-w-0 space-y-2 border-t p-2">{children}</div> : null}
    </div>
  );
}

/**
 * 一段话 —— 会话视图的最小可编辑单元。
 *
 * ⚠️ 结构上分两层：**表头是 `<div>`、正文才是 `<button>`**。
 *    不能把「删除这一段」的按钮塞进那个 `<button>` 里（button 套 button 是非法 HTML，
 *    React 会警告、浏览器会把内层拆出去，点击行为随即错乱）。
 */
function Paragraph({
  tone,
  label,
  text,
  icon,
  note,
  onSave,
  onDelete,
}: {
  tone: "user" | "assistant";
  label: React.ReactNode;
  text: string;
  icon?: React.ReactNode;
  note?: React.ReactNode;
  onSave: (value: string) => void;
  onDelete?: () => void;
}) {
  const [editing, setEditing] = React.useState(false);
  const [draft, setDraft] = React.useState(text);
  const taRef = React.useRef<HTMLTextAreaElement | null>(null);

  // 进入编辑时把草稿拍成当前文本；**不能**在 `text` 变化时同步草稿（会在打字中途覆盖）。
  const begin = () => {
    setDraft(text);
    setEditing(true);
  };
  React.useEffect(() => {
    if (editing) taRef.current?.focus();
  }, [editing]);

  if (editing) {
    return (
      <div className="border-primary/60 bg-background space-y-1 rounded-lg border-2">
        <div className="text-muted-foreground flex items-center gap-1 px-3 pt-2 text-[10px]">
          {icon}
          <span>{label}</span>
        </div>
        <div className="px-3">
          <textarea
            ref={taRef}
            value={draft}
            rows={Math.min(24, Math.max(3, draft.split("\n").length + 1))}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setEditing(false);
              if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                onSave(draft);
                setEditing(false);
              }
            }}
            className="border-input bg-background focus-visible:ring-ring w-full resize-y rounded-md border px-2 py-1.5 text-xs leading-relaxed focus-visible:ring-1 focus-visible:outline-none"
          />
        </div>
        <div className="flex flex-wrap items-center justify-end gap-1 px-3 pb-2">
          <span className="text-muted-foreground mr-auto text-[10px]">Ctrl/⌘ + Enter 保存，Esc 取消</span>
          <Button type="button" variant="ghost" size="sm" className="h-7 gap-1 px-2 text-[11px]" onClick={() => setEditing(false)}>
            <X className="size-3" />
            取消
          </Button>
          <Button
            type="button"
            size="sm"
            className="h-7 gap-1 px-2 text-[11px]"
            onClick={() => {
              onSave(draft);
              setEditing(false);
            }}
          >
            <Check className="size-3" />
            保存这一段
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div
      className={cn(
        "rounded-lg border transition-colors",
        tone === "user" ? "bg-primary/5 border-primary/20" : "bg-muted/40",
      )}
    >
      <div className="text-muted-foreground flex items-center gap-1 px-3 pt-1.5 text-[10px]">
        {icon}
        <span className="min-w-0 truncate">{label}</span>
        <div className="ml-auto flex shrink-0 items-center gap-0.5">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="size-6 p-0"
            title="编辑这一段"
            onClick={begin}
          >
            <Pencil className="size-3" />
          </Button>
          {onDelete ? (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="text-destructive size-6 p-0"
              title="删除这一段"
              onClick={onDelete}
            >
              <X className="size-3" />
            </Button>
          ) : null}
        </div>
      </div>
      <button
        type="button"
        onClick={begin}
        title="点击编辑这一段"
        className="block w-full px-3 pt-0.5 pb-2 text-left"
      >
        <span
          className={cn(
            "block text-xs leading-relaxed break-words whitespace-pre-wrap",
            !text && "text-muted-foreground italic",
          )}
        >
          {text || "（这一段是空的，点击填写）"}
        </span>
        {note}
      </button>
    </div>
  );
}

/** 一次工具调用：一行摘要 + 展开改参数。 */
function ToolRow({
  name,
  summary,
  params,
  onParamsChange,
  onDelete,
}: {
  name: string;
  summary: string;
  params: unknown;
  onParamsChange: (v: unknown) => void;
  onDelete?: () => void;
}) {
  const [open, setOpen] = React.useState(false);
  return (
    <div className="bg-muted/20 rounded-lg border border-dashed">
      {/* ⚠️ 左边三样（图标 / 工具名 / 参数摘要）放 grid 的前两列；参数摘要是可省略的长串，
          必须 min-w-0 + truncate，不能用 flex-wrap（窄屏会被挤成一个字一行）。 */}
      <div className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2 px-3 py-1.5">
        <div className="text-muted-foreground flex shrink-0 items-center gap-1 text-[10px]">
          <Wrench className="size-3" />
          <span className="font-mono font-medium">{name}</span>
        </div>
        <div className="text-muted-foreground min-w-0 truncate font-mono text-[10px]" title={summary}>
          {summary || "（无参数）"}
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-6 px-2 text-[10px]"
            title="展开改这次工具调用的参数"
            onClick={() => setOpen((v) => !v)}
          >
            {open ? "收起参数" : "改参数"}
          </Button>
          {onDelete ? (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              className="text-destructive size-6 p-0"
              title="删除这一段"
              onClick={onDelete}
            >
              <X className="size-3" />
            </Button>
          ) : null}
        </div>
      </div>
      {open ? (
        <div className="border-t p-2">
          <JsonForm value={params} onChange={onParamsChange} />
        </div>
      ) : null}
    </div>
  );
}

/** 一条消息 = 一张气泡卡（正文流里的一「段」由它内部再拆）。 */
function ConversationCard({
  doc,
  msg,
  role,
  title,
  selected,
  onToggleMessage,
  onSaveSegment,
  onDeleteSegment,
  onAddSegment,
  onLoadBody,
  onRowDuplicate,
  onRowDelete,
}: {
  doc: TraeEditDoc;
  msg: TurnMessage;
  role: "user" | "assistant";
  title: string;
  selected: boolean;
  onToggleMessage: (id: string) => void;
  onSaveSegment: (ref: RowRef, path: (string | number)[], value: unknown) => void;
  onDeleteSegment: (ref: RowRef, path: (string | number)[]) => void;
  onAddSegment: (ref: RowRef, role: string) => void;
  onLoadBody: (ref: RowRef) => void;
  onRowDuplicate: (refs: RowRef[], blank: boolean) => void;
  onRowDelete: (refs: RowRef[]) => void;
}) {
  const body = bodyOf(doc, msg);
  const segs: Segment[] = body ? segmentsOf(msg.role, body.row) : [];
  const isUser = role === "user";

  return (
    <div
      className={cn(
        "space-y-2 rounded-xl border p-2",
        selected ? "border-primary bg-primary/5" : "",
      )}
    >
      <div className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2">
        <Checkbox
          checked={selected}
          onCheckedChange={() => onToggleMessage(msg.id)}
          aria-label={`勾选「${title}」以便批量修改`}
        />
        <div className="text-muted-foreground flex min-w-0 items-center gap-1 text-[10px]">
          {isUser ? <UserIcon className="size-3 shrink-0" /> : <Bot className="size-3 shrink-0" />}
          <span className="shrink-0 font-medium">{title}</span>
          {msg.index !== null ? <span className="shrink-0">· 第 {msg.index} 条</span> : null}
          <span className="min-w-0 truncate font-mono" title={msg.id}>
            {msg.id}
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-0.5">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-6 gap-1 px-2 text-[10px]"
            title="复制这条消息（换新 id，清空正文）"
            onClick={() => onRowDuplicate(messageRefs(msg), true)}
          >
            <Copy className="size-3" />
            复制
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="text-destructive h-6 gap-1 px-2 text-[10px]"
            title="删除这条消息（连同它名下所有关联表的行）"
            onClick={() => onRowDelete(messageRefs(msg))}
          >
            <X className="size-3" />
            删除
          </Button>
        </div>
      </div>

      {msg.message === null ? (
        <Alert>
          <AlertTriangle className="size-3" />
          <AlertTitle className="text-xs">引用缺失（不是数据被删）</AlertTitle>
          <AlertDescription className="text-[11px]">
            回合指向的 `message_id` 在这条会话的 `chat_message` 里找不到 ——
            真机上存在这种悬空引用。这里如实显示，不做任何自动修补。
          </AlertDescription>
        </Alert>
      ) : body === null ? (
        <p className="text-muted-foreground px-1 text-[11px]">
          这条消息在 doc 里没有正文行（`{isUser ? "chat_message_general" : "chat_message_task"}`）。
          要补一行可以展开下面的「底层字段」。
        </p>
      ) : body.byRef ? (
        <Alert>
          <AlertTriangle className="size-3" />
          <AlertTitle className="text-xs">正文是大字段，还没取回来</AlertTitle>
          <AlertDescription className="text-[11px]">
            这一行的 `content` 是按引用返回的（真机上 82 条助手消息里有 53 条如此，
            最大单行 9.8 MB）。取回来之后就能像普通消息一样逐段编辑。
            <Button
              type="button"
              variant="outline"
              size="sm"
              className="mt-2 h-7 gap-1 px-2 text-[11px]"
              onClick={() => onLoadBody(body.ref)}
            >
              展开正文
            </Button>
          </AlertDescription>
        </Alert>
      ) : (
        <div className={cn("space-y-1.5", isUser ? "pl-0" : "pl-0")}>
          {segs.map((s) => {
            if (s.kind === "tool") {
              return (
                <ToolRow
                  key={s.key}
                  name={s.label}
                  summary={s.text}
                  params={readAtPath(body.row["content"], s.path)}
                  onParamsChange={(v) => {
                    // 参数是 JSON 结构，不是一段纯文本 ⇒ 走「整段替换」这条口子。
                    onSaveSegment(body.ref, s.path, v);
                  }}
                  onDelete={() => onDeleteSegment(body.ref, s.path)}
                />
              );
            }
            if (s.kind === "other" && !s.plain) {
              return (
                <div key={s.key} className="rounded-lg border border-dashed p-2">
                  <div className="text-muted-foreground mb-1 flex items-center gap-1 text-[10px]">
                    <Wrench className="size-3" />
                    <span>{s.label}</span>
                  </div>
                  <JsonForm
                    value={readAtPath(body.row["content"], s.path)}
                    onChange={(v) => onSaveSegment(body.ref, s.path, v)}
                  />
                </div>
              );
            }
            return (
              <Paragraph
                key={s.key}
                tone={role}
                icon={isUser ? <UserIcon className="size-3" /> : <Bot className="size-3" />}
                label={s.label}
                text={s.text}
                onSave={(v) => onSaveSegment(body.ref, s.path, v)}
                onDelete={() => onDeleteSegment(body.ref, s.path)}
              />
            );
          })}
          {segs.length === 0 ? (
            <p className="text-muted-foreground px-1 text-[11px]">
              这条消息的正文里没有可读内容（没有说明、没有工具调用、也没有答复）。
            </p>
          ) : null}
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="h-7 w-full gap-1 text-[11px]"
            title={isUser ? "在这条提问后面再加一段文本" : "在助手回答末尾再加一段（默认是一段空答复）"}
            onClick={() => onAddSegment(body.ref, msg.role)}
          >
            <Plus className="size-3" />
            {isUser ? "再加一段提问文本" : "再加一段回答"}
          </Button>
        </div>
      )}
    </div>
  );
}

/** 一张表里的一行（= 一个全字段表单）。 */
function RowBlock({
  doc,
  ref_,
  label,
  defaultOpen,
  onRowChange,
  onRowDuplicate,
  onRowDelete,
  onLoadRef,
}: {
  doc: TraeEditDoc;
  ref_: RowRef;
  label?: string;
  defaultOpen?: boolean;
  onRowChange: (ref: RowRef, row: Record<string, unknown>) => void;
  onRowDuplicate: (refs: RowRef[], blank: boolean) => void;
  onRowDelete: (refs: RowRef[]) => void;
  onLoadRef: (ref: RowRef, column: string, meta: JsonRefMeta) => Promise<void>;
}) {
  const t = tableOf(doc, ref_.table);
  const row = t?.rows[ref_.index];
  if (!t || !row) {
    return (
      <div className="text-muted-foreground rounded-md border border-dashed p-2 text-[11px]">
        行已不存在（{ref_.table} #{ref_.index}）
      </div>
    );
  }
  return (
    <Collapse
      defaultOpen={defaultOpen}
      title={label ? `${label} · ${t.name}` : t.name}
      subtitle={rowSummary(t, row)}
      badge={
        <>
          {t.derived ? (
            <Badge variant="outline" className="text-[9px]">
              检索镜像
            </Badge>
          ) : null}
          {t.refCount > 0 ? (
            <span
              className="text-amber-600 dark:text-amber-400 text-[10px]"
              title={`本表有 ${t.refCount} 个大字段按需加载`}
            >
              ◆{t.refCount}
            </span>
          ) : null}
        </>
      }
      actions={
        <>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="h-6 px-2 text-[11px]"
            title="复制这一行（换新 id，清空正文）"
            onClick={() => onRowDuplicate([ref_], true)}
          >
            <Copy className="size-3" />
            复制
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="text-destructive h-6 px-2 text-[11px]"
            title="删除这一行"
            onClick={() => onRowDelete([ref_])}
          >
            <X className="size-3" />
            删除
          </Button>
        </>
      }
    >
      <RowForm
        row={row}
        columns={toRowColumns(t)}
        derived={t.derived}
        onChange={(next) => onRowChange(ref_, next)}
        onLoadRef={(meta, column) => onLoadRef(ref_, column, meta)}
      />
    </Collapse>
  );
}

/** 一张表下的若干行（一个行组）。 */
function GroupBlock({
  doc,
  group,
  title,
  onRowChange,
  onRowDuplicate,
  onRowDelete,
  onLoadRef,
}: {
  doc: TraeEditDoc;
  group: TableGroup;
  title?: string;
  onRowChange: (ref: RowRef, row: Record<string, unknown>) => void;
  onRowDuplicate: (refs: RowRef[], blank: boolean) => void;
  onRowDelete: (refs: RowRef[]) => void;
  onLoadRef: (ref: RowRef, column: string, meta: JsonRefMeta) => Promise<void>;
}) {
  const [limit, setLimit] = React.useState(GROUP_PAGE);
  const shown = group.refs.slice(0, limit);
  const rest = group.refs.length - shown.length;
  const t = tableOf(doc, group.table);
  return (
    <div className="space-y-1">
      {title ? (
        <div className="text-muted-foreground flex flex-wrap items-center gap-2 text-[10px]">
          <span>{title}</span>
          <span className="font-mono">{group.table}</span>
          <span>{group.refs.length} 行</span>
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="ml-auto h-6 px-2 text-[11px]"
            title="按这张表的列结构新增一行（复制同表末行的形状，清空正文）"
            onClick={() => {
              const last = group.refs[group.refs.length - 1];
              if (last && t) onRowDuplicate([last], true);
            }}
          >
            <Plus className="size-3" />
            新增一行
          </Button>
        </div>
      ) : null}
      {shown.map((r) => (
        <RowBlock
          key={`${r.table}#${r.index}`}
          doc={doc}
          ref_={r}
          onRowChange={onRowChange}
          onRowDuplicate={onRowDuplicate}
          onRowDelete={onRowDelete}
          onLoadRef={onLoadRef}
        />
      ))}
      {rest > 0 ? (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="h-7 w-full gap-1 text-[11px]"
          onClick={() => setLimit((v) => v + GROUP_PAGE * 2)}
        >
          再显示 {Math.min(rest, GROUP_PAGE * 2)} 行（还有 {rest} 行未渲染）
        </Button>
      ) : null}
    </div>
  );
}

export interface TraeTurnDetailProps {
  doc: TraeEditDoc;
  mode: "turn" | "orphans" | "session";
  turn: TurnView | null;
  orphans: TurnMessage[];
  sessionScope: TableGroup[];
  /** 已勾选的消息 id。 */
  selected: Set<string>;
  onToggleMessage: (id: string) => void;
  onRowChange: (ref: RowRef, row: Record<string, unknown>) => void;
  onRowDuplicate: (refs: RowRef[], blank: boolean) => void;
  onRowDelete: (refs: RowRef[]) => void;
  onLoadRef: (ref: RowRef, column: string, meta: JsonRefMeta) => Promise<void>;
  /** 改一段正文（纯文本或 JSON，取决于该段的原文形态）。 */
  onSaveSegment: (ref: RowRef, path: (string | number)[], value: unknown) => void;
  /** 删一段正文。 */
  onDeleteSegment: (ref: RowRef, path: (string | number)[]) => void;
  /** 在正文末尾新增一段。 */
  onAddSegment: (ref: RowRef, role: string) => void;
  /** 「展开正文」——大字段按引用返回时把它取回来。 */
  onLoadBody: (ref: RowRef) => void;
  onPrev: () => void;
  onNext: () => void;
  hasPrev: boolean;
  hasNext: boolean;
  /** 批量操作条（由外层提供：它才知道选中了什么、以及有哪些目标回合）。 */
  toolbar?: React.ReactNode;
  busy?: boolean;
}

/** 底层字段（原始表 / 全部列）—— 默认收起，会话视图不显示这些。 */
function RawFields({
  doc,
  turn,
  onRowChange,
  onRowDuplicate,
  onRowDelete,
  onLoadRef,
}: {
  doc: TraeEditDoc;
  turn: TurnView;
  onRowChange: (ref: RowRef, row: Record<string, unknown>) => void;
  onRowDuplicate: (refs: RowRef[], blank: boolean) => void;
  onRowDelete: (refs: RowRef[]) => void;
  onLoadRef: (ref: RowRef, column: string, meta: JsonRefMeta) => Promise<void>;
}) {
  const extra = [
    turn.turn ? { ref_: turn.turn, label: "回合本体" } : null,
    turn.user?.message ? { ref_: turn.user.message, label: "用户消息主行" } : null,
    turn.assistant?.message ? { ref_: turn.assistant.message, label: "助手消息主行" } : null,
  ].filter((x): x is { ref_: RowRef; label: string } => x !== null);
  const groups: TableGroup[] = [
    ...(turn.user?.attachments ?? []),
    ...(turn.assistant?.attachments ?? []),
    ...turn.extra,
  ];
  return (
    <Collapse
      title="底层字段（原始表 / 全部列）"
      subtitle="会话视图不显示这些；改 id、时间戳、结构或关联表时才用得上"
    >
      <div className="space-y-2">
        {extra.map((x) => (
          <RowBlock
            key={`${x.ref_.table}#${x.ref_.index}`}
            doc={doc}
            ref_={x.ref_}
            label={x.label}
            onRowChange={onRowChange}
            onRowDuplicate={onRowDuplicate}
            onRowDelete={onRowDelete}
            onLoadRef={onLoadRef}
          />
        ))}
        {groups.map((g) => (
          <GroupBlock
            key={g.table}
            doc={doc}
            group={g}
            title="关联表"
            onRowChange={onRowChange}
            onRowDuplicate={onRowDuplicate}
            onRowDelete={onRowDelete}
            onLoadRef={onLoadRef}
          />
        ))}
      </div>
    </Collapse>
  );
}

export function TraeTurnDetail({
  doc,
  mode,
  turn,
  orphans,
  sessionScope,
  selected,
  onToggleMessage,
  onRowChange,
  onRowDuplicate,
  onRowDelete,
  onLoadRef,
  onSaveSegment,
  onDeleteSegment,
  onAddSegment,
  onLoadBody,
  onPrev,
  onNext,
  hasPrev,
  hasNext,
  toolbar,
  busy,
}: TraeTurnDetailProps) {
  const card = (m: TurnMessage, role: "user" | "assistant", title: string) => (
    <ConversationCard
      key={m.id}
      doc={doc}
      msg={m}
      role={role}
      title={title}
      selected={selected.has(m.id)}
      onToggleMessage={onToggleMessage}
      onSaveSegment={onSaveSegment}
      onDeleteSegment={onDeleteSegment}
      onAddSegment={onAddSegment}
      onLoadBody={onLoadBody}
      onRowDuplicate={onRowDuplicate}
      onRowDelete={onRowDelete}
    />
  );

  return (
    <div className="flex min-h-0 min-w-0 flex-col gap-2">
      <div className="grid shrink-0 grid-cols-[minmax(0,1fr)_auto] items-start gap-2 border-b pb-2">
        <div className="min-w-0">
          {mode === "turn" && turn ? (
            <>
              <div className="flex min-w-0 flex-wrap items-center gap-1">
                <span className="text-xs font-medium">回合 {turn.seq}</span>
                <Badge variant={turn.status === "completed" ? "secondary" : "outline"} className="text-[9px]">
                  {turn.status || "无状态"}
                </Badge>
                {turn.dangling ? (
                  <Badge variant="outline" className="text-[9px]">
                    引用缺失
                  </Badge>
                ) : null}
              </div>
              <div className="text-muted-foreground truncate font-mono text-[10px]" title={turn.turnId ?? undefined}>
                {turn.turnId ?? "（回合行缺失）"}
              </div>
            </>
          ) : mode === "orphans" ? (
            <>
              <div className="text-xs font-medium">散落消息</div>
              <div className="text-muted-foreground text-[10px]">
                这些消息没有任何 `chat_turn` 指向它们 —— 通常是被删过回合或会话写坏时的残留。
              </div>
            </>
          ) : (
            <>
              <div className="text-xs font-medium">会话级数据</div>
              <div className="text-muted-foreground text-[10px]">
                不属于任何回合的行（`chat_session` / `session_project` / `worktree` / 检索镜像…）。
              </div>
            </>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {busy ? <Loader2 className="text-muted-foreground size-3 animate-spin" /> : null}
          {mode === "turn" ? (
            <>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="size-7 p-0"
                title="上一个回合"
                disabled={!hasPrev}
                onClick={onPrev}
              >
                <ChevronLeft className="size-3" />
              </Button>
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="size-7 p-0"
                title="下一个回合"
                disabled={!hasNext}
                onClick={onNext}
              >
                <ChevronRight className="size-3" />
              </Button>
            </>
          ) : null}
        </div>
      </div>

      {toolbar}

      <div className="min-h-0 flex-1 space-y-3 overflow-auto pr-1">
        {mode === "turn" && turn ? (
          <>
            {turn.user ? card(turn.user, "user", "用户提问") : (
              <p className="text-muted-foreground text-[11px]">这个回合没有用户提问（引用为空）。</p>
            )}
            {turn.assistant ? card(turn.assistant, "assistant", "助手回答") : (
              <p className="text-muted-foreground text-[11px]">这个回合没有助手回答（引用为空）。</p>
            )}
            {turn.turn ? (
              <RawFields
                doc={doc}
                turn={turn}
                onRowChange={onRowChange}
                onRowDuplicate={onRowDuplicate}
                onRowDelete={onRowDelete}
                onLoadRef={onLoadRef}
              />
            ) : (
              <Alert>
                <AlertTriangle className="size-3" />
                <AlertTitle className="text-xs">没有回合行</AlertTitle>
                <AlertDescription className="text-[11px]">
                  这条消息组没有对应的 `chat_turn` 行。
                </AlertDescription>
              </Alert>
            )}
          </>
        ) : mode === "orphans" ? (
          orphans.length === 0 ? (
            <p className="text-muted-foreground text-xs">没有散落消息。</p>
          ) : (
            orphans.map((m) =>
              card(m, m.role === "assistant" ? "assistant" : "user", `散落消息 · ${m.role || "未知角色"}`),
            )
          )
        ) : sessionScope.length === 0 ? (
          <p className="text-muted-foreground text-xs">没有会话级数据。</p>
        ) : (
          sessionScope.map((g) => (
            <GroupBlock
              key={g.table}
              doc={doc}
              group={g}
              title="会话级"
              onRowChange={onRowChange}
              onRowDuplicate={onRowDuplicate}
              onRowDelete={onRowDelete}
              onLoadRef={onLoadRef}
            />
          ))
        )}
      </div>
    </div>
  );
}
