import * as React from "react";
import {
  ArrowDown,
  ArrowUp,
  Braces,
  ChevronDown,
  ChevronRight,
  Database,
  List,
  Loader2,
  Plus,
  Trash2,
  Type,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { cn } from "@/lib/utils";

/**
 * 递归结构化表单：把任意深度的 JSON 值渲染成一层层可编辑控件。
 *
 * 为什么必须递归而不是逐字段写控件：Trae 的表结构**随客户端版本变化**，
 * 而且 `content` / `plan_item` / `tool_call_info` 这类字段层层嵌套（实测 6 层以上）。
 * 手写控件既跟不上版本，也覆盖不到「全字段」。
 *
 * 类型 ↔ 控件：
 *
 * | 类型 | 控件 |
 * | --- | --- |
 * | string | 短文本 `<input>`；长文本 / 含换行 → `<textarea>` |
 * | number | 数字输入，自动保留整数 / 小数 |
 * | boolean | `Switch` |
 * | null | `null` 标记 + 「改为 …」类型选择 |
 * | array | 每项一行（序号 / 上移 / 下移 / 删除）+ 递归 + 「新增一项」 |
 * | object | 每个键一行（键名 + 递归 + 删除该键）+「新增键」 |
 */

type Kind = "null" | "boolean" | "number" | "string" | "array" | "object";

/** 后端「大字段引用」的形状（`$jsonRef`）。 */
export interface JsonRefMeta {
  pk: number | string;
  bytes: number;
  preview: string;
}

/**
 * 是不是一个大字段引用。
 *
 * 超过阈值的 JSON 单元格后端只回引用不回内容（真机实测一条会话 90 MB，
 * 全量塞进来会把渲染冻死）。**不展开就原样保留** —— 后端写回时按 pk 回查原文。
 */
export function readJsonRef(v: unknown): JsonRefMeta | null {
  if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  const m = (v as Record<string, unknown>).$jsonRef;
  if (!m || typeof m !== "object" || Array.isArray(m)) return null;
  const o = m as Record<string, unknown>;
  if (typeof o.pk !== "number" && typeof o.pk !== "string") return null;
  return {
    pk: o.pk,
    bytes: typeof o.bytes === "number" ? o.bytes : 0,
    preview: typeof o.preview === "string" ? o.preview : "",
  };
}

/** 点「展开并编辑」时回调；父组件负责拉取内容并替换掉这个引用。 */
export type JsonRefLoader = (meta: JsonRefMeta) => void | Promise<void>;

function formatBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / 1048576).toFixed(1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}

export function kindOf(v: unknown): Kind {
  if (v === null || v === undefined) return "null";
  if (typeof v === "boolean") return "boolean";
  if (typeof v === "number") return "number";
  if (typeof v === "string") return "string";
  if (Array.isArray(v)) return "array";
  if (typeof v === "object") return "object";
  return "string";
}

/** 空值模板：从「null」改成某种类型时的初始值。 */
function blank(k: Kind): unknown {
  switch (k) {
    case "string":
      return "";
    case "number":
      return 0;
    case "boolean":
      return false;
    case "array":
      return [];
    case "object":
      return {};
    default:
      return null;
  }
}

const KIND_LABEL: Record<Kind, string> = {
  null: "空 null",
  boolean: "真假",
  number: "数字",
  string: "文本",
  array: "列表",
  object: "对象",
};

/** 数字输入：本地保留草稿，允许输入过程中出现 `-` / `1.` 这类中间态。 */
function NumberField({
  value,
  onCommit,
  className,
  disabled,
}: {
  value: number;
  onCommit: (n: number) => void;
  className?: string;
  disabled?: boolean;
}) {
  const [draft, setDraft] = React.useState(String(value));
  React.useEffect(() => {
    // 外部值变了（例如撤销、切换会话）时同步草稿；数值相等就不打断用户输入。
    if (Number(draft) !== value) setDraft(String(value));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value]);
  return (
    <Input
      className={cn("h-8 font-mono text-xs", className)}
      inputMode="decimal"
      value={draft}
      disabled={disabled}
      onChange={(e) => {
        const raw = e.target.value;
        setDraft(raw);
        const n = Number(raw);
        if (raw.trim() !== "" && Number.isFinite(n)) onCommit(n);
      }}
      onBlur={() => {
        if (!Number.isFinite(Number(draft)) || draft.trim() === "") setDraft(String(value));
      }}
    />
  );
}

/** 短/长文本输入：超过 80 字符或含换行改用多行框。 */
function StringField({
  value,
  onCommit,
  disabled,
  className,
  rows,
}: {
  value: string;
  onCommit: (s: string) => void;
  disabled?: boolean;
  className?: string;
  rows?: number;
}) {
  const multiline = rows !== undefined ? rows > 1 : value.length > 80 || value.includes("\n");
  if (!multiline) {
    return (
      <Input
        className={cn("h-8 font-mono text-xs", className)}
        value={value}
        disabled={disabled}
        onChange={(e) => onCommit(e.target.value)}
      />
    );
  }
  return (
    <textarea
      className={cn(
        "border-input placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-ring/50 w-full min-w-0 rounded-md border bg-transparent px-3 py-1.5 font-mono text-xs shadow-xs transition-[color,box-shadow] outline-none focus-visible:ring-[3px] disabled:cursor-not-allowed disabled:opacity-50",
        className,
      )}
      rows={rows ?? Math.min(16, Math.max(3, value.split("\n").length))}
      value={value}
      disabled={disabled}
      onChange={(e) => onCommit(e.target.value)}
    />
  );
}

/** 「改为 …」：把 null（或任意值）换成另一种类型。 */
function KindPicker({ onPick }: { onPick: (k: Kind) => void }) {
  const kinds: Kind[] = ["string", "number", "boolean", "array", "object", "null"];
  return (
    <div className="flex flex-wrap items-center gap-1">
      {kinds.map((k) => (
        <button
          key={k}
          type="button"
          title={`改为 ${KIND_LABEL[k]}`}
          className="text-muted-foreground hover:text-foreground hover:bg-accent rounded border px-1.5 py-0.5 text-[11px]"
          onClick={() => onPick(k)}
        >
          {KIND_LABEL[k]}
        </button>
      ))}
    </div>
  );
}

/** 新增键：内联输入键名（不用 window.prompt —— 它在本机的 WebView 里表现不可控）。 */
function AddKey({ onAdd }: { onAdd: (k: string) => void }) {
  const [name, setName] = React.useState("");
  return (
    <div className="flex items-center gap-1">
      <Input
        className="h-7 w-48 font-mono text-xs"
        placeholder="新键名"
        value={name}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && name.trim()) {
            onAdd(name.trim());
            setName("");
          }
        }}
      />
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="h-7 gap-1 px-2 text-xs"
        disabled={!name.trim()}
        onClick={() => {
          if (name.trim()) {
            onAdd(name.trim());
            setName("");
          }
        }}
      >
        <Plus className="size-3" />
        加键
      </Button>
    </div>
  );
}

/** 大字段卡片：不加载内容，只给体积、开头预览和「展开并编辑」。 */
function JsonRefCard({
  meta,
  label,
  hint,
  onLoad,
  className,
}: {
  meta: JsonRefMeta;
  label?: React.ReactNode;
  hint?: React.ReactNode;
  onLoad?: JsonRefLoader;
  className?: string;
}) {
  const [busy, setBusy] = React.useState(false);
  const [err, setErr] = React.useState("");
  return (
    <div className={cn("min-w-0 space-y-1", className)}>
      <div className="flex min-w-0 flex-wrap items-center gap-1">
        {label !== undefined && (
          <span className="text-muted-foreground shrink-0 font-mono text-xs">{label}</span>
        )}
        {hint !== undefined && (
          <span className="shrink-0 text-[10px] text-muted-foreground/70">{hint}</span>
        )}
        <Badge variant="secondary" className="shrink-0 px-1.5 py-0 text-[10px]">
          大字段 · {formatBytes(meta.bytes)}
        </Badge>
      </div>
      <div className="min-w-0 space-y-1.5 rounded-md border border-dashed px-2 py-1.5">
        <p className="text-muted-foreground text-[11px]">
          体积较大，没有随会话一起加载；不展开就原样保留，展开后才能改。
        </p>
        {meta.preview && (
          <pre className="text-muted-foreground max-h-16 overflow-hidden font-mono text-[10px] break-all whitespace-pre-wrap">
            {meta.preview}
          </pre>
        )}
        {err && <p className="text-destructive text-[11px]">{err}</p>}
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="h-7 gap-1 px-2 text-xs"
          disabled={busy || !onLoad}
          title={onLoad ? `拉取这 ${formatBytes(meta.bytes)} 的内容并编辑` : "当前上下文不支持加载"}
          onClick={async () => {
            if (!onLoad) return;
            setBusy(true);
            setErr("");
            try {
              await onLoad(meta);
            } catch (e) {
              setErr(e instanceof Error ? e.message : String(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? <Loader2 className="size-3 animate-spin" /> : <Database className="size-3" />}
          {busy ? "加载中…" : "展开并编辑"}
        </Button>
      </div>
    </div>
  );
}

export interface JsonFormProps {
  value: unknown;
  onChange: (v: unknown) => void;
  /** 键名 / 序号，显示在控件左侧。 */
  label?: React.ReactNode;
  /** 类型提示（列声明类型），显示在键名旁。 */
  hint?: React.ReactNode;
  /** 只读（例如自增主键 `id`）。 */
  readOnly?: boolean;
  /** 键名后的额外操作（例如「删除该键」「上移」）。 */
  actions?: React.ReactNode;
  /** 值右侧的额外内容。 */
  trailer?: React.ReactNode;
  /** 遇到 `$jsonRef` 时点「展开并编辑」的回调（一路递归传下去）。 */
  onLoadRef?: JsonRefLoader;
  depth?: number;
  className?: string;
}

export function JsonForm({
  value,
  onChange,
  label,
  hint,
  readOnly,
  actions,
  trailer,
  onLoadRef,
  depth = 0,
  className,
}: JsonFormProps) {
  const kind = kindOf(value);
  const [collapsed, setCollapsed] = React.useState(false);

  // ---- 大字段引用：不渲染成树，只给卡片（否则 9.8 MB 的行会当场冻住界面）----
  const refMeta = readJsonRef(value);
  if (refMeta) {
    return (
      <JsonRefCard
        meta={refMeta}
        label={label}
        hint={hint}
        onLoad={readOnly ? undefined : onLoadRef}
        className={className}
      />
    );
  }

  const head = (
    <div className="flex min-w-0 flex-wrap items-center gap-1">
      {label !== undefined && (
        <span className="text-muted-foreground shrink-0 font-mono text-xs">{label}</span>
      )}
      {hint !== undefined && <span className="shrink-0 text-[10px] text-muted-foreground/70">{hint}</span>}
      {kind === "array" && (
        <Badge variant="secondary" className="shrink-0 px-1.5 py-0 text-[10px]">
          {(value as unknown[]).length} 项
        </Badge>
      )}
      {kind === "object" && (
        <Badge variant="secondary" className="shrink-0 px-1.5 py-0 text-[10px]">
          {Object.keys(value as Record<string, unknown>).length} 键
        </Badge>
      )}
      {readOnly && (
        <Badge variant="outline" className="shrink-0 px-1.5 py-0 text-[10px]">
          只读
        </Badge>
      )}
      <span className="ml-auto flex shrink-0 items-center gap-1">
        {(kind === "array" || kind === "object") && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="size-6 p-0"
            title={collapsed ? "展开" : "收起"}
            onClick={() => setCollapsed((c) => !c)}
          >
            {collapsed ? <ChevronRight className="size-3" /> : <ChevronDown className="size-3" />}
          </Button>
        )}
        {actions}
      </span>
    </div>
  );

  // ---- 叶子：直接给控件 ----
  if (kind === "null" || kind === "boolean" || kind === "number" || kind === "string") {
    return (
      <div className={cn("min-w-0 space-y-1", className)}>
        {head}
        {kind === "null" ? (
          readOnly ? (
            <span className="text-muted-foreground font-mono text-xs">null</span>
          ) : (
            <KindPicker onPick={(k) => onChange(blank(k))} />
          )
        ) : kind === "boolean" ? (
          <Switch
            checked={value as boolean}
            disabled={readOnly}
            onCheckedChange={(v) => onChange(v)}
          />
        ) : kind === "number" ? (
          <NumberField value={value as number} disabled={readOnly} onCommit={onChange} />
        ) : (
          <StringField value={value as string} disabled={readOnly} onCommit={onChange} />
        )}
        {trailer}
      </div>
    );
  }

  // ---- 容器：数组 / 对象 ----
  const items = kind === "array" ? (value as unknown[]) : null;
  const entries = kind === "object" ? Object.entries(value as Record<string, unknown>) : [];

  const move = (from: number, to: number) => {
    if (!items || to < 0 || to >= items.length) return;
    const next = items.slice();
    const [x] = next.splice(from, 1);
    next.splice(to, 0, x);
    onChange(next);
  };

  return (
    <div className={cn("min-w-0 space-y-1", className)}>
      {head}
      <div
        className={cn(
          "min-w-0 space-y-1.5 border-l pl-2",
          depth === 0 ? "border-transparent pl-0" : "border-muted",
        )}
      >
        {!collapsed && (
          <>
            {kind === "array" &&
              (items ?? []).map((item, i) => (
                <JsonForm
                  key={i}
                  depth={depth + 1}
                  label={`[${i}]`}
                  value={item}
                  readOnly={readOnly}
                  onLoadRef={onLoadRef}
                  onChange={(v) => {
                    const next = (items ?? []).slice();
                    next[i] = v;
                    onChange(next);
                  }}
                  actions={
                    readOnly ? null : (
                      <>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          className="size-6 p-0"
                          title="上移"
                          disabled={i === 0}
                          onClick={() => move(i, i - 1)}
                        >
                          <ArrowUp className="size-3" />
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          className="size-6 p-0"
                          title="下移"
                          disabled={i === (items ?? []).length - 1}
                          onClick={() => move(i, i + 1)}
                        >
                          <ArrowDown className="size-3" />
                        </Button>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          className="text-destructive size-6 p-0"
                          title="删除该项"
                          onClick={() => {
                            const next = (items ?? []).slice();
                            next.splice(i, 1);
                            onChange(next);
                          }}
                        >
                          <Trash2 className="size-3" />
                        </Button>
                      </>
                    )
                  }
                />
              ))}

            {kind === "object" &&
              entries.map(([k, v]) => (
                <JsonForm
                  key={k}
                  depth={depth + 1}
                  label={k}
                  value={v}
                  readOnly={readOnly}
                  onLoadRef={onLoadRef}
                  onChange={(nv) => onChange({ ...(value as Record<string, unknown>), [k]: nv })}
                  actions={
                    readOnly ? null : (
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        className="text-destructive size-6 p-0"
                        title={`删除键 ${k}`}
                        onClick={() => {
                          const next = { ...(value as Record<string, unknown>) };
                          delete next[k];
                          onChange(next);
                        }}
                      >
                        <Trash2 className="size-3" />
                      </Button>
                    )
                  }
                />
              ))}

            {!readOnly && kind === "array" && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                className="h-7 gap-1 px-2 text-xs"
                onClick={() => onChange([...(items ?? []), ""])}
              >
                <Plus className="size-3" />
                新增一项
              </Button>
            )}
            {!readOnly && kind === "object" && (
              <AddKey
                onAdd={(k) => {
                  const next = { ...(value as Record<string, unknown>) };
                  if (!(k in next)) next[k] = "";
                  onChange(next);
                }}
              />
            )}
            {((kind === "array" && (items ?? []).length === 0) ||
              (kind === "object" && entries.length === 0)) && (
              <span className="text-muted-foreground text-xs">（空）</span>
            )}
          </>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// 表格行：按真实的列顺序渲染（而不是按 JSON 的键序）
// ---------------------------------------------------------------------------

export interface RowColumnMeta {
  name: string;
  type: string;
  notNull: boolean;
  pk: boolean;
  json: boolean;
  readOnly: boolean;
}

/** 一行表数据：键序用列元数据里的真实顺序，缺失的列不显示。 */
export function RowForm({
  row,
  columns,
  onChange,
  derived,
  onLoadRef,
}: {
  row: Record<string, unknown>;
  columns: RowColumnMeta[];
  onChange: (r: Record<string, unknown>) => void;
  /** `fts_*` 检索镜像：顶部给一句提醒。 */
  derived?: boolean;
  /** 遇到大字段引用时点「展开并编辑」的回调（带上列名，调用方要知道往哪儿回填）。 */
  onLoadRef?: (meta: JsonRefMeta, column: string) => void | Promise<void>;
}) {
  return (
    <div className="space-y-2">
      {derived && (
        <div className="text-muted-foreground rounded-md bg-amber-500/10 px-2 py-1 text-[11px]">
          fts_* 是检索镜像：改动只影响搜索命中，不影响会话内容
        </div>
      )}
      {columns.map((c) => {
        const v = row[c.name];
        const present = c.name in row;
        if (!present) {
          return (
            <div key={c.name} className="flex items-center gap-2">
              <span className="text-muted-foreground shrink-0 font-mono text-xs">{c.name}</span>
              <span className="text-[10px] text-muted-foreground/70">该行没有这一列（写回时跳过）</span>
            </div>
          );
        }
        return (
          <JsonForm
            key={c.name}
            label={c.name}
            hint={[c.type || "—", c.pk ? "主键" : "", c.notNull ? "必填" : "", c.json ? "JSON" : ""]
              .filter(Boolean)
              .join(" · ")}
            value={v}
            readOnly={c.readOnly}
            onLoadRef={onLoadRef ? (m) => onLoadRef(m, c.name) : undefined}
            onChange={(nv) => onChange({ ...row, [c.name]: nv })}
            className="rounded-md border px-2 py-1.5"
          />
        );
      })}
    </div>
  );
}

/** 类型图标（表头用）。 */
export function KindIcon({ k }: { k: Kind }) {
  if (k === "array") return <List className="size-3" />;
  if (k === "object") return <Braces className="size-3" />;
  return <Type className="size-3" />;
}

/** 折叠三角（供外部复用同一套视觉）。 */
export function CollapseToggle({
  collapsed,
  onToggle,
}: {
  collapsed: boolean;
  onToggle: () => void;
}) {
  return (
    <Button
      type="button"
      variant="ghost"
      size="sm"
      className="size-6 p-0"
      title={collapsed ? "展开" : "收起"}
      onClick={onToggle}
    >
      {collapsed ? <ChevronRight className="size-3" /> : <ChevronDown className="size-3" />}
    </Button>
  );
}
