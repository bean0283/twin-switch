import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Loader2, Pencil, RefreshCw, Save, Trash2, XCircle } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  qoderSessionDeleteMany,
  qoderSessionDetail,
  qoderSessionRename,
  qoderSessionsList,
  qoderSessionUpdateMessage,
  qoderSessionDeleteMessage,
} from "@/lib/api";
import { cn } from "@/lib/utils";

type SessionRow = {
  id: string;
  title: string;
  cwd: string;
  model: string;
  createdAt: number;
  updatedAt: number;
  archived: boolean;
  messageCount: number;
  bytes: number;
  source: string;
  importedFrom: string | null;
};

type Message = {
  messageId: string;
  role: string;
  text: string;
  thinking: string | null;
  toolCount: number;
  createdAt: number;
  status: string;
};

type Detail = {
  id: string;
  meta: { title: string; cwd: string; model: string; createdAt: number; updatedAt: number } | null;
  messages: Message[];
};

const pad = (n: number) => String(n).padStart(2, "0");

function fmtTime(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 体积：与 WorkBuddy 会话页同一套单位（B / KB / MB / GB）。 */
function fmtSize(bytes: number): string {
  if (!bytes) return "—";
  const units = ["B", "KB", "MB", "GB"];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${units[i]}`;
}

const ROLE_META: Record<string, { label: string; className: string }> = {
  user: { label: "我", className: "border-sky-500/30 bg-sky-500/10 text-sky-700" },
  assistant: {
    label: "Qoder",
    className: "border-emerald-500/30 bg-emerald-500/10 text-emerald-700",
  },
  system: { label: "系统", className: "border-border bg-muted text-muted-foreground" },
  tool: { label: "工具", className: "border-border bg-muted text-muted-foreground" },
};

export default function QoderSessionsPage() {
  const [region, setRegion] = useState<"cn" | "global">("global");
  const [rows, setRows] = useState<SessionRow[]>([]);
  const [includeArchived, setIncludeArchived] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [open, setOpen] = useState(false);
  const [detail, setDetail] = useState<Detail | null>(null);
  /** 编辑器里的标题草稿（与详情分开，取消就丢掉）。 */
  const [titleDraft, setTitleDraft] = useState("");
  /** 每条消息的正文草稿：messageId → text。 */
  const [msgDraft, setMsgDraft] = useState<Record<string, string>>({});
  const [savingId, setSavingId] = useState<string | null>(null);
  /** 右栏正在编辑哪条消息（与 WorkBuddy 编辑器同一套「左列表 / 右编辑」布局）。 */
  const [selMsg, setSelMsg] = useState<string | null>(null);
  /** 待删除的会话（**弹窗二次确认**）。单条删除也走这个数组（长度 1）——
   *  不能出现「单条能删、批量删不了」两条路径。
   *  ⚠️ 不用 `window.confirm` —— Tauri 的 WebView 里它可能被拦掉直接返回 false，
   *     表现就是「点了删除没有任何反应、也没报错、会话还在」（踩过）。 */
  const [pending, setPending] = useState<SessionRow[]>([]);
  /** 勾选的会话 id（批量删除用）。 */
  const [selected, setSelected] = useState<Set<string>>(new Set());
  /** 表头复选框要能显示「部分选中」的横杠，React 只能通过 ref 设置。 */
  const allRef = useRef<HTMLInputElement>(null);
  /** 正在删（关客户端可能要十几秒，弹窗里必须有可见进度）。 */
  const [deleting, setDeleting] = useState(false);
  /** 删除失败原因，显示在弹窗内（不关弹窗，用户能看到到底为什么）。 */
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [loadingDetail, setLoadingDetail] = useState(false);
  const [cwd, setCwd] = useState<string>("");
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const list = await qoderSessionsList(region, includeArchived);
      setRows(Array.isArray(list) ? (list as SessionRow[]) : []);
    } catch (e) {
      setError(String(e));
      setRows([]);
    } finally {
      setBusy(false);
    }
  }, [region, includeArchived]);

  useEffect(() => {
    void load();
  }, [load]);

  const view = useCallback(
    async (row: SessionRow) => {
      setOpen(true);
      setLoadingDetail(true);
      setDetail(null);
      try {
        const d = (await qoderSessionDetail(region, row.id)) as Detail;
        setDetail(d);
        setTitleDraft(d.meta?.title ?? "");
        setMsgDraft({});
        setSelMsg(d.messages?.[0]?.messageId ?? null);
      } catch (e) {
        setError(String(e));
      } finally {
        setLoadingDetail(false);
      }
    },
    [region],
  );

  const messages = useMemo(() => detail?.messages ?? [], [detail]);

  const cwds = useMemo(() => {
    const set = new Set<string>();
    rows.forEach((r) => r.cwd && set.add(r.cwd));
    return Array.from(set).sort();
  }, [rows]);

  const shown = useMemo(() => (cwd ? rows.filter((r) => r.cwd === cwd) : rows), [rows, cwd]);

  const allChecked = shown.length > 0 && shown.every((r) => selected.has(r.id));
  const someChecked = shown.some((r) => selected.has(r.id));
  useEffect(() => {
    if (allRef.current) allRef.current.indeterminate = someChecked && !allChecked;
  }, [someChecked, allChecked]);
  // 换版本 / 换「含归档」/ 换工作目录 ⇒ 勾选清空：留着会删到屏幕上看不见的行。
  useEffect(() => {
    setSelected(new Set());
  }, [region, includeArchived, cwd]);

  const toggleAll = () => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (allChecked) {
        shown.forEach((r) => next.delete(r.id));
      } else {
        shown.forEach((r) => next.add(r.id));
      }
      return next;
    });
  };

  const toggleOne = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  /** 保存标题（编辑器里）。 */
  async function saveTitle() {
    if (!detail) return;
    const t = titleDraft.trim();
    if (!t) return;
    setError(null);
    try {
      await qoderSessionRename(region, detail.id, t);
      setNotice("标题已保存");
      await view({ id: detail.id } as SessionRow);
      await load();
    } catch (e) {
      setError(String(e));
    }
  }

  /** 保存某条消息的正文。 */
  async function saveMessage(m: Message) {
    if (!detail) return;
    const text = msgDraft[m.messageId] ?? m.text;
    setSavingId(m.messageId);
    setError(null);
    try {
      await qoderSessionUpdateMessage(region, detail.id, m.messageId, text);
      setNotice("消息已保存（写前已备份 main.sqlite）");
      await view({ id: detail.id } as SessionRow);
    } catch (e) {
      setError(String(e));
    } finally {
      setSavingId(null);
    }
  }

  /** 删除某条消息。 */
  async function removeMessage(m: Message) {
    if (!detail) return;
    // 不用 window.confirm（WebView 里可能被拦掉直接返回 false，点了像没反应）；
    // 编辑器右栏的按钮本身就是「明确动作」，写前有备份，删错可从备份恢复。
    setSavingId(m.messageId);
    setError(null);
    try {
      await qoderSessionDeleteMessage(region, detail.id, m.messageId);
      setNotice("消息已删除");
      await view({ id: detail.id } as SessionRow);
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setSavingId(null);
    }
  }



  /**
   * 删除（单条 / 批量**同一条路径**）。
   *
   * ⚠️ 一次调用处理完所有选中项：后端一次关客户端、一次备份、一个事务、一次重启。
   * 循环调单条接口会把客户端开关 N 次（每次十几秒），并产生 N 份全库备份。
   */
  async function doDeleteRows(targets: SessionRow[]) {
    if (targets.length === 0) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      const res = await qoderSessionDeleteMany(
        region,
        targets.map((r) => r.id),
      );
      const rl = res.clientRelaunch;
      const one = targets.length === 1;
      const skipped = res.missing?.length ?? 0;
      setPending([]);
      setSelected(new Set());
      setNotice(
        (one && targets[0].title
          ? `已删除「${targets[0].title}」：`
          : `已删除 ${res.deletedSessions ?? targets.length} 个会话：`) +
          `会话 ${res.deletedSessions ?? 0} 条、消息 ${res.deletedMessages ?? 0} 条` +
          (skipped > 0
            ? `；另有 ${skipped} 个在库里已不存在，已跳过`
            : "") +
          (res.backup ? `；备份在 ${res.backup}` : "") +
          (rl
            ? !rl.ok
              ? `；⚠️ 客户端没能启动：${rl.error ?? "原因未知"}`
              : rl.action === "already-running"
                ? "；客户端保持运行"
                : rl.action === "launched"
                  ? `；客户端已启动（${rl.exe ?? ""}）`
                  : ""
            : ""),
      );
      await load();
    } catch (e) {
      // 失败**留在弹窗里**，不关闭 —— 不能让「删失败」看起来像「删成功」。
      setDeleteError(String(e));
      setNotice(null);
    } finally {
      setDeleting(false);
    }
  }

  return (
    <div className="mx-auto flex max-w-6xl flex-col gap-4 px-6 py-6">
      <Card>
        <CardHeader className="pb-3">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <CardTitle className="text-lg">Qoder 会话记录</CardTitle>
              <p className="mt-1 text-xs text-muted-foreground">
                只读客户端本地的 <span className="font-mono">main.sqlite</span>
                （国内版 / 国际版各读各的库）。会话属于「当前登录账号」，没有按账号拆分的字段，
                因此这里只按版本筛选。
              </p>
            </div>
            <Button variant="outline" size="sm" onClick={() => void load()} className="gap-1.5">
              {busy ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
              刷新
            </Button>
          </div>
        </CardHeader>
      </Card>

      {notice && (
        <div className="flex items-start gap-2 rounded-xl border border-emerald-500/30 bg-emerald-500/5 px-4 py-3 text-xs text-emerald-700">
          <span className="break-all">{notice}</span>
        </div>
      )}

      {error && (
        <div className="flex items-start gap-2 rounded-xl border border-destructive/30 bg-destructive/5 px-4 py-3 text-xs text-destructive">
          <XCircle className="mt-0.5 size-3.5 shrink-0" />
          <span className="break-all">{error}</span>
        </div>
      )}

      <Card>
        <CardHeader className="pb-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <CardTitle className="text-base">会话列表</CardTitle>
            <div className="flex flex-wrap items-center gap-2">
              <label className="flex items-center gap-1.5 text-xs text-muted-foreground">
                <input
                  type="checkbox"
                  checked={includeArchived}
                  onChange={(e) => setIncludeArchived(e.target.checked)}
                />
                含归档
              </label>
              <select
                value={region}
                onChange={(e) => setRegion(e.target.value as "cn" | "global")}
                className="h-8 rounded-lg border bg-background px-2 text-xs outline-none focus:border-primary"
              >
                <option value="cn">国内版</option>
                <option value="global">国际版</option>
              </select>
              <select
                value={cwd}
                onChange={(e) => setCwd(e.target.value)}
                className="h-8 max-w-[220px] rounded-lg border bg-background px-2 text-xs outline-none focus:border-primary"
                title="按工作目录筛选"
              >
                <option value="">全部工作目录</option>
                {cwds.map((c) => (
                  <option key={c} value={c}>
                    {c}
                  </option>
                ))}
              </select>
              <Badge variant="outline">{shown.length} 个会话</Badge>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          <div className="overflow-hidden rounded-lg border">
            <table className="w-full text-sm">
              <thead className="bg-muted/40 text-xs text-muted-foreground">
                <tr>
                  <th className="w-9 px-2 py-2 text-left font-medium">
                    <input
                      ref={allRef}
                      type="checkbox"
                      aria-label="全选当前列表"
                      checked={allChecked}
                      onChange={toggleAll}
                      disabled={shown.length === 0 || deleting}
                      className="size-3.5 cursor-pointer align-middle accent-destructive disabled:cursor-not-allowed"
                    />
                  </th>
                  <th className="px-3 py-2 text-left font-medium">标题</th>
                  <th className="px-3 py-2 text-left font-medium whitespace-nowrap">更新时间</th>
                  <th className="px-3 py-2 text-right font-medium whitespace-nowrap">正文</th>
                  <th className="px-3 py-2 text-right font-medium whitespace-nowrap">操作</th>
                </tr>
              </thead>
              <tbody>
                {shown.length === 0 ? (
                  <tr>
                    <td colSpan={5} className="px-3 py-10 text-center text-xs text-muted-foreground">
                      {busy ? "读取中…" : "这个版本没有会话记录。"}
                    </td>
                  </tr>
                ) : (
                  shown.map((r) => (
                    <tr
                      key={r.id}
                      className={cn(
                        "border-t border-border/60 align-top",
                        selected.has(r.id) && "bg-destructive/[0.03]",
                      )}
                    >
                      <td className="px-2 py-2 align-middle">
                        <input
                          type="checkbox"
                          checked={selected.has(r.id)}
                          onChange={() => toggleOne(r.id)}
                          aria-label={`选择 ${r.title || "会话"}`}
                          className="size-3.5 cursor-pointer align-middle accent-destructive"
                        />
                      </td>
                      <td className="px-3 py-2">
                        <div className="flex flex-wrap items-center gap-1.5">
                          <span className="font-medium">{r.title || "（无标题）"}</span>
                          {r.archived && <Badge variant="outline">归档</Badge>}
                          {r.importedFrom && (
                            <Badge variant="outline" className="border-primary/30 bg-primary/5">
                              来自 {r.importedFrom}
                            </Badge>
                          )}
                          {!r.importedFrom && r.source && (
                            <span className="text-[11px] text-muted-foreground">{r.source}</span>
                          )}
                        </div>
                        <div className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground">
                          {r.cwd || "—"}
                        </div>
                      </td>
                      <td className="px-3 py-2 whitespace-nowrap text-xs text-muted-foreground">
                        {fmtTime(r.updatedAt)}
                        <div className="text-[11px]">{r.messageCount} 条消息</div>
                      </td>
                      <td className="px-3 py-2 text-right font-mono text-xs whitespace-nowrap">
                        {fmtSize(r.bytes)}
                      </td>
                      <td className="px-3 py-2 text-right whitespace-nowrap">
                        <div className="inline-flex gap-1">
                          <button
                            type="button"
                            onClick={() => void view(r)}
                            title="编辑会话（标题 / 消息正文）"
                            aria-label={`编辑 ${r.title || "会话"}`}
                            className="inline-flex size-8 items-center justify-center rounded-md border hover:bg-accent"
                          >
                            <Pencil className="size-4" />
                          </button>
                          <button
                            type="button"
                            disabled={deleting}
                            onClick={() => {
                              setDeleteError(null);
                              setPending([r]);
                            }}
                            title="删除（会弹窗确认）"
                            aria-label={`删除 ${r.title || "会话"}`}
                            className="inline-flex size-8 items-center justify-center rounded-md border text-destructive hover:bg-destructive/5 disabled:opacity-50"
                          >
                            <Trash2 className="size-4" />
                          </button>
                        </div>
                      </td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          </div>
          {selected.size > 0 && (
            <div className="mt-2 flex flex-wrap items-center justify-between gap-2 rounded-xl border bg-muted/30 px-3 py-2">
              <span className="text-xs text-muted-foreground">
                已选 {selected.size} 个会话
                {" · "}
                共{" "}
                {shown
                  .filter((r) => selected.has(r.id))
                  .reduce((n, r) => n + (r.messageCount || 0), 0)}{" "}
                条消息
              </span>
              <div className="flex items-center gap-2">
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 px-2 text-xs"
                  onClick={() => setSelected(new Set())}
                >
                  取消选择
                </Button>
                <Button
                  variant="destructive"
                  size="sm"
                  className="h-7 gap-1.5 px-2.5 text-xs"
                  onClick={() => {
                    setDeleteError(null);
                    setPending(shown.filter((r) => selected.has(r.id)));
                  }}
                >
                  <Trash2 className="size-3.5" />
                  删除所选
                </Button>
              </div>
            </div>
          )}

          <p className="mt-2 text-[11px] text-muted-foreground">
            编辑 / 删除会**写客户端库**：程序会先**主动关闭该版本 Qoder**（不要求你手动退），
            写前自动备份 <span className="font-mono">main.sqlite</span>，写完自动重启客户端；
            删除是**真删**（会话与消息一起移除，事务 + 删后复查，复查不过会报错而不是假装成功）。
          </p>
        </CardContent>
      </Card>

      {/* 删除确认：**弹窗**二次确认，说明与结果都写在弹窗内 */}
      <Dialog
        open={pending.length > 0}
        onOpenChange={(o) => {
          if (!o && !deleting) {
            setPending([]);
            setDeleteError(null);
          }
        }}
      >
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <Trash2 className="size-4 text-destructive" />
              {pending.length > 1
                ? `确认删除 ${pending.length} 个会话`
                : "确认删除会话"}
            </DialogTitle>
            <DialogDescription>这会改动 Qoder 客户端本地库，请确认。</DialogDescription>
          </DialogHeader>

          {pending.length > 0 && (
            <div className="space-y-3 text-xs">
              {pending.length === 1 ? (
                <div className="rounded-lg border border-border px-3 py-2">
                  <div className="font-medium break-all">
                    {pending[0].title || "（无标题会话）"}
                  </div>
                  <div className="mt-1 text-muted-foreground">
                    {pending[0].messageCount} 条消息 · {fmtSize(pending[0].bytes)} · 更新于{" "}
                    {fmtTime(pending[0].updatedAt)}
                  </div>
                </div>
              ) : (
                <>
                  <div className="rounded-lg border border-border px-3 py-2">
                    <div className="font-medium">
                      将删除 {pending.length} 个会话
                    </div>
                    <div className="mt-1 text-muted-foreground">
                      共{" "}
                      {pending.reduce((n, r) => n + (r.messageCount || 0), 0)}{" "}
                      条消息 ·{" "}
                      {fmtSize(pending.reduce((n, r) => n + (r.bytes || 0), 0))}
                    </div>
                  </div>
                  <ul className="max-h-40 space-y-0.5 overflow-y-auto rounded-lg border border-border px-3 py-2">
                    {pending.map((r) => (
                      <li key={r.id} className="truncate text-muted-foreground">
                        · {r.title || "（无标题会话）"}
                      </li>
                    ))}
                  </ul>
                </>
              )}
              <ul className="space-y-1 text-muted-foreground">
                <li>· 会从 Qoder 客户端库里**真删**（会话 + 全部消息），不是只隐藏</li>
                <li>· 写库前自动备份 main.sqlite，删错可从备份恢复</li>
                <li>· 会先**主动关闭**该版本 Qoder 客户端，写完**自动重启**</li>
              </ul>
              {deleteError && (
                <div className="flex items-start gap-2 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-destructive">
                  <XCircle className="mt-0.5 size-3.5 shrink-0" />
                  <span className="break-all">{deleteError}</span>
                </div>
              )}
            </div>
          )}

          <DialogFooter>
            <Button
              variant="outline"
              size="sm"
              disabled={deleting}
              onClick={() => {
                setPending([]);
                setDeleteError(null);
              }}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              size="sm"
              disabled={deleting}
              onClick={() => void doDeleteRows(pending)}
              className="gap-1.5"
            >
              {deleting ? (
                <>
                  <Loader2 className="size-3.5 animate-spin" /> 删除中（先关客户端）…
                </>
              ) : (
                <>
                  <Trash2 className="size-3.5" />
                  {pending.length > 1 ? `确认删除 ${pending.length} 个` : "确认删除"}
                </>
              )}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={open} onOpenChange={setOpen}>
        {/* 编辑会话要同时看「消息列表 + 正文」，窄了就是两栏互相挤 —— 用 6xl（1152px）。 */}
        <DialogContent className="max-w-6xl">
          <DialogHeader>
            <DialogTitle>编辑会话</DialogTitle>
            <DialogDescription className="break-all">
              {detail?.meta
                ? `${fmtTime(detail.meta.updatedAt)} · ${detail.meta.model || "—"} · ${detail.meta.cwd || "—"}`
                : ""}
            </DialogDescription>
          </DialogHeader>

          {/* 标题：随时可改，回车或点保存都行 */}
          <div className="flex items-center gap-2">
            <input
              value={titleDraft}
              onChange={(e) => setTitleDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void saveTitle();
              }}
              placeholder="会话标题"
              className="h-8 min-w-0 flex-1 rounded-lg border border-border bg-background px-2 text-sm outline-none focus:border-primary"
            />
            <button
              type="button"
              onClick={() => void saveTitle()}
              className="inline-flex h-8 items-center gap-1 rounded-lg border border-border px-2 text-xs hover:bg-accent"
            >
              <Save className="size-3.5" /> 保存标题
            </button>
          </div>

          <div className="grid min-h-0 grid-cols-[minmax(240px,340px)_minmax(0,1fr)] gap-3">
            {/* 左栏：消息列表（点选后在右栏编辑） */}
            <div className="max-h-[64vh] overflow-y-auto rounded-lg border">
              {loadingDetail ? (
                <div className="flex items-center justify-center py-10 text-xs text-muted-foreground">
                  <Loader2 className="mr-2 size-4 animate-spin" />
                  读取消息…
                </div>
              ) : messages.length === 0 ? (
                <p className="px-3 py-6 text-center text-xs text-muted-foreground">
                  这个会话没有可显示的正文消息
                </p>
              ) : (
                messages.map((m, i) => {
                  const rm = ROLE_META[m.role] ?? {
                    label: m.role || "未知",
                    className: "border-border bg-muted text-muted-foreground",
                  };
                  const active = selMsg === m.messageId;
                  return (
                    <button
                      key={m.messageId || i}
                      type="button"
                      onClick={() => setSelMsg(m.messageId)}
                      className={cn(
                        "flex w-full flex-col gap-1 border-b px-2.5 py-2 text-left last:border-b-0 hover:bg-accent",
                        active && "bg-primary/5",
                      )}
                    >
                      <span className="flex items-center gap-1.5">
                        <Badge variant="outline" className={cn("text-[10px]", rm.className)}>
                          {rm.label}
                        </Badge>
                        <span className="text-[10px] text-muted-foreground">
                          {fmtTime(m.createdAt)}
                        </span>
                      </span>
                      <span className="line-clamp-2 text-[11px] text-muted-foreground">
                        {m.text || "（空）"}
                      </span>
                    </button>
                  );
                })
              )}
            </div>

            {/* 右栏：选中消息的编辑器 */}
            <div className="max-h-[58vh] overflow-y-auto rounded-lg border p-3">
              {(() => {
                const m = messages.find((x) => x.messageId === selMsg) ?? null;
                if (!m) {
                  return (
                    <p className="py-10 text-center text-xs text-muted-foreground">
                      从左侧选一条消息开始编辑
                    </p>
                  );
                }
                const rm = ROLE_META[m.role] ?? {
                  label: m.role || "未知",
                  className: "border-border bg-muted text-muted-foreground",
                };
                return (
                  <div className="space-y-2">
                    <div className="flex flex-wrap items-center gap-2">
                      <Badge variant="outline" className={cn("text-[10px]", rm.className)}>
                        {rm.label}
                      </Badge>
                      <span className="text-[11px] text-muted-foreground">
                        {fmtTime(m.createdAt)}
                      </span>
                      {m.toolCount > 0 && (
                        <span className="text-[11px] text-muted-foreground">
                          {m.toolCount} 次工具调用
                        </span>
                      )}
                    </div>
                    <textarea
                      value={msgDraft[m.messageId] ?? m.text}
                      onChange={(e) =>
                        setMsgDraft((prev) => ({ ...prev, [m.messageId]: e.target.value }))
                      }
                      className="h-[46vh] w-full resize-none rounded-md border border-border bg-background px-2 py-1.5 font-mono text-[12px] leading-relaxed outline-none focus:border-primary"
                    />
                    <div className="flex flex-wrap items-center gap-1.5">
                      <button
                        type="button"
                        disabled={savingId === m.messageId}
                        onClick={() => void saveMessage(m)}
                        className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-[11px] hover:bg-accent disabled:opacity-50"
                      >
                        <Save className="size-3" /> 保存这条
                      </button>
                      <button
                        type="button"
                        disabled={savingId === m.messageId}
                        onClick={() => void removeMessage(m)}
                        className="inline-flex items-center gap-1 rounded-md border border-destructive/30 px-2 py-1 text-[11px] text-destructive hover:bg-destructive/5 disabled:opacity-50"
                      >
                        <Trash2 className="size-3" /> 删除这条
                      </button>
                      {msgDraft[m.messageId] !== undefined && msgDraft[m.messageId] !== m.text && (
                        <span className="text-[11px] text-amber-600">已改动，未保存</span>
                      )}
                    </div>
                    {m.thinking && (
                      <details>
                        <summary className="cursor-pointer text-[11px] text-muted-foreground">
                          思维链（{m.thinking.length} 字，只读）
                        </summary>
                        <div className="mt-1 max-h-40 overflow-y-auto rounded-md bg-muted/40 px-2 py-1.5 text-[11px] leading-relaxed whitespace-pre-wrap text-muted-foreground">
                          {m.thinking}
                        </div>
                      </details>
                    )}
                  </div>
                );
              })()}
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
