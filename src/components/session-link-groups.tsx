import { useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
  FolderGit2,
  Link2Off,
  Loader2,
  Plus,
  RefreshCw,
  ScrollText,
  Users,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import type { TraeLinkGroupRow, TraeLinkMemberRow } from "@/lib/trae-types";
import { cn } from "@/lib/utils";

/**
 * 「关联会话」—— Trae / Qoder 共用（WorkBuddy 用自己那份组件，**版式必须一致**）。
 *
 * 版式照搬 WorkBuddy 的关联会话页：顶部一排状态筛选 chip + 双列卡片；点卡片开详情弹窗。
 * 不另造一套行列表 —— 三个页面是同一件事，用户来回切时不该重新学。
 *
 * ⚠️ 两条纪律：
 *   - 状态由后端给（`status` / `relation`），界面不自己推断谁更新；
 *   - 判据各产品不同（Trae 只能比条数与时间），说明文字必须由调用方传入（`note`）。
 */

type Props = {
  /** 产品名（当前版式与 WorkBuddy 一致、不显示在标题里，留着给以后按产品微调文案用）。 */
  productLabel: string;
  accounts: { uid: string; label: string }[];
  groups: TraeLinkGroupRow[];
  loading: boolean;
  error: string | null;
  storePath?: string;
  busyGroup: string | null;
  busyNote?: string;
  note: string;
  onReload: () => void;
  /** 去「会话复制」新建一条关联（Trae/Qoder 不能凭空造关联，只能引导过去）。 */
  onNewLink?: () => void;
  onCopy: (group: TraeLinkGroupRow, targetUid: string) => void;
  onUnify: (group: TraeLinkGroupRow, sourceMember: TraeLinkMemberRow) => void;
  onUnlink: (group: TraeLinkGroupRow) => void;
  /** 把一个账号从组里拆出去（只删关联记录，不动会话数据）。 */
  onUnlinkMember: (group: TraeLinkGroupRow, member: TraeLinkMemberRow) => void;
  /**
   * 「查看内容」：按会话 id 读这份副本的对话（只读）。
   *
   * 由调用方传入（而不是组件里直接调 api）：客户端键在页面状态里，
   * 组件只负责展示 —— 这样 Qoder 复用本组件时不必改这里。
   */
  onViewTurns: (sessionId: string) => Promise<{
    ok: boolean;
    title: string;
    updated: string;
    turns: { role: string; text: string; tools: string[] }[];
  }>;
};

type Filter = "all" | "pending" | "diverged" | "consistent" | "missing" | "unknown";

/** `YYYY-MM-DD HH:MM:SS`（Trae）→ 相对时间；读不出来就原样显示。 */
function relativeTime(raw?: string | null): string {
  if (!raw) return "—";
  const d = new Date(raw.replace(" ", "T"));
  if (Number.isNaN(d.getTime())) return raw;
  const diff = Date.now() - d.getTime();
  if (diff < 0) return raw;
  const min = Math.floor(diff / 60000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const day = Math.floor(hour / 24);
  if (day < 30) return `${day} 天前`;
  return raw.slice(0, 10);
}

const STATUS_PILL: Record<string, { label: string; tone: string }> = {
  consistent: {
    label: "内容一致",
    tone: "border-emerald-500/30 bg-emerald-500/10 text-emerald-600",
  },
  pending: { label: "待同步", tone: "border-amber-500/30 bg-amber-500/10 text-amber-600" },
  diverged: {
    label: "有分歧",
    tone: "border-destructive/30 bg-destructive/10 text-destructive",
  },
  missing: { label: "内容缺失", tone: "border-amber-500/30 bg-amber-500/10 text-amber-700" },
  unknown: { label: "无法确认", tone: "border-border bg-muted text-muted-foreground" },
};

/** 成员关系文案（与 WorkBuddy 的 `RELATION` 同表，不另起一套说法）。 */
const RELATION: Record<string, string> = {
  latest: "最新",
  equal: "相同",
  behind: "落后",
  ahead: "超前",
  diverged: "分叉",
  "no-body": "无正文",
  stale: "有新改动",
  gone: "已失效",
  unreadable: "未解密",
  unknown: "未比对",
};

/** 成员关系药丸配色（与 WorkBuddy 的 `relPill` 同一套色）。 */
function relPill(relation: string): string {
  switch (relation) {
    case "latest":
      return "border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300";
    case "equal":
      return "border-emerald-500/20 bg-emerald-500/[0.06] text-emerald-700/80 dark:text-emerald-300/80";
    case "behind":
    case "stale":
      return "border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300";
    case "ahead":
    case "diverged":
      return "border-destructive/30 bg-destructive/10 text-destructive";
    default:
      return "border-border bg-muted/60 text-muted-foreground";
  }
}

/**
 * 一个账号副本的卡片（版式与 WorkBuddy 的 `MemberCard` **逐项一致**）。
 *
 * 「查看内容」与 WorkBuddy 同名同义：只读预览这份副本的对话，方便确认内容对不对。
 * Trae 这边**能看**：多账号共用一个库（靠 `project.user_id` 区分归属），
 * 只要库能解，任何账号（包括当前没登录的）的副本都读得出来。
 */
function MemberCard({
  m,
  busy,
  groupKey,
  canUnify,
  onUnify,
  onView,
  onUnlink,
}: {
  m: TraeLinkMemberRow;
  busy: string | null;
  groupKey: string;
  canUnify: boolean;
  onUnify: () => void;
  onView: () => void;
  onUnlink: () => void;
}) {
  const isBusy = busy === `unify:${groupKey}` || busy === `unlink:${groupKey}`;
  const issues = m.integrity ?? [];
  return (
    <div className="min-w-0 rounded-xl border bg-card p-2.5 transition hover:border-emerald-500/30">
      <div className="flex items-start justify-between gap-2">
        {/* ⚠️ `min-w-0` 不能省：flex 子元素的 min-width 默认是 `auto`，
            光有 `truncate` 也压不住长账号名，会把整列（进而整个弹窗）撑宽。 */}
        <span className="min-w-0 truncate text-[13px] font-semibold leading-tight">
          {m.accountName}
        </span>
        <span
          className={cn(
            "shrink-0 rounded-full border px-1.5 py-px text-[10px] font-medium leading-[15px]",
            relPill(m.relation ?? ""),
          )}
        >
          {m.contentState === "unreadable"
            ? RELATION.unreadable
            : !m.alive
              ? RELATION.gone
              : RELATION[m.relation ?? ""] || "—"}
        </span>
      </div>
      <div className="mt-1 text-[11px] text-muted-foreground">
        {typeof m.messages === "number"
          ? `${m.messages.toLocaleString("zh-CN")} 条内容`
          : "条数未知"}
      </div>
      <div className="text-[11px] text-muted-foreground/80">{m.updated || "—"}</div>
      {m.reason ? (
        <div className="mt-1 line-clamp-2 text-[11px] leading-relaxed text-muted-foreground/80">
          {m.reason}
        </div>
      ) : null}
      {issues.length > 0 ? (
        <div
          className="mt-1 rounded border border-destructive/30 bg-destructive/10 px-1.5 py-px text-[10px] text-destructive"
          title={issues.join("\n")}
        >
          副本数据错位
        </div>
      ) : null}
      <div className="mt-2.5 flex items-center gap-1">
        <button
          type="button"
          disabled={!canUnify || isBusy || !m.alive}
          title={
            !m.alive
              ? "这份副本已经失效，没有内容可作标准"
              : "用这份内容覆盖组内其它账号的副本（会写库并重启客户端）"
          }
          onClick={onUnify}
          className="rounded-md border border-emerald-500/30 bg-emerald-500/[0.06] px-2 py-0.5 text-[11px] font-medium text-emerald-700 transition hover:bg-emerald-500/10 disabled:opacity-40 dark:text-emerald-300"
        >
          以此为准
        </button>
        {/* 「查看内容」：只读预览这份副本的对话。样式与 WorkBuddy 一致（绿字 + 下箭头）。 */}
        <button
          type="button"
          disabled={!m.alive || isBusy}
          title={
            !m.alive
              ? "这份副本已经失效，没有内容可看"
              : "只读预览这份副本的对话内容"
          }
          onClick={onView}
          className="inline-flex items-center gap-0.5 rounded px-1.5 py-0.5 text-[11px] font-medium text-emerald-700 transition hover:bg-emerald-500/10 disabled:opacity-40 dark:text-emerald-300"
        >
          查看内容 <ChevronDown className="size-3" />
        </button>
        <button
          type="button"
          disabled={isBusy}
          title="把这个账号从这条关联里拆出去（只删关联记录，不动会话数据）"
          onClick={onUnlink}
          className="ml-auto rounded p-1 text-muted-foreground transition hover:bg-destructive/10 hover:text-destructive disabled:opacity-40"
        >
          {isBusy ? <Loader2 className="size-3.5 animate-spin" /> : <Link2Off className="size-3.5" />}
        </button>
      </div>
    </div>
  );
}

export function SessionLinkGroups({
  accounts,
  groups,
  loading,
  error,
  storePath,
  busyGroup,
  busyNote,
  note,
  onReload,
  onNewLink,
  onCopy,
  onUnify,
  onUnlink,
  onUnlinkMember,
  onViewTurns,
}: Props) {  const [filter, setFilter] = useState<Filter>("all");
  const [openKey, setOpenKey] = useState<string | null>(null);

  // 「查看内容」只读预览：正在看的那份副本 + 取回来的回合。
  const [viewer, setViewer] = useState<{
    member: TraeLinkMemberRow;
    title: string;
    updated: string;
    turns: { role: string; text: string; tools: string[] }[];
  } | null>(null);
  const [viewerLoading, setViewerLoading] = useState(false);

  /**
   * 打开「查看内容」：读这份副本的对话（只读，不写库）。
   *
   * 读不到就**照实报错并关掉弹窗**，不显示一个空列表 ——
   * 「没有内容」和「读失败」长得一样的话，用户会以为这份副本是空的。
   */
  const openViewer = async (member: TraeLinkMemberRow) => {
    setViewerLoading(true);
    setViewer({ member, title: "", updated: "", turns: [] });
    try {
      const d = await onViewTurns(member.sessionId);
      if (!d.ok) throw new Error("读不到这份副本的内容");
      setViewer({
        member,
        title: d.title,
        updated: d.updated,
        turns: d.turns ?? [],
      });
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
      setViewer(null);
    } finally {
      setViewerLoading(false);
    }
  };

  const counts = useMemo(() => {
    const c: Record<string, number> = {
      consistent: 0,
      pending: 0,
      diverged: 0,
      missing: 0,
      unknown: 0,
    };
    for (const g of groups) c[g.status] = (c[g.status] ?? 0) + 1;
    return c;
  }, [groups]);

  const shown = useMemo(
    () => (filter === "all" ? groups : groups.filter((g) => g.status === filter)),
    [groups, filter],
  );

  /** 矩阵行 = 账号清单 + 组里出现过但清单里没有的成员（否则那份副本会静默消失）。 */
  const accountRows = useMemo(() => {
    const map = new Map<string, { uid: string; label: string }>();
    for (const a of accounts) map.set(a.uid, a);
    for (const g of groups) {
      for (const m of g.members) {
        if (!map.has(m.uid)) map.set(m.uid, { uid: m.uid, label: m.accountName || m.uid });
      }
    }
    return [...map.values()];
  }, [accounts, groups]);

  const detail = groups.find((g) => g.key === openKey) ?? null;

  const filters: { key: Filter; label: string; n: number }[] = [
    { key: "all", label: "全部", n: groups.length },
    { key: "pending", label: "待同步", n: counts.pending },
    { key: "diverged", label: "有分歧", n: counts.diverged },
    { key: "consistent", label: "内容一致", n: counts.consistent },
    { key: "missing", label: "内容缺失", n: counts.missing },
    { key: "unknown", label: "无法确认", n: counts.unknown },
  ];

  return (
    <>
      <Card>
        <CardHeader className="pb-3">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div className="min-w-0 space-y-1">
              <CardTitle className="text-base">关联会话</CardTitle>
              <CardDescription>{note}</CardDescription>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <Button variant="outline" size="sm" onClick={onReload} disabled={loading}>
                {loading ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <RefreshCw className="size-4" />
                )}
                刷新
              </Button>
              {onNewLink ? (
                <Button size="sm" onClick={onNewLink}>
                  <Plus className="size-4" />
                  新增关联会话
                </Button>
              ) : null}
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          {error ? (
            <div className="flex items-start gap-2 rounded-xl border border-destructive/25 bg-destructive/[0.06] px-3.5 py-3 text-xs leading-5 text-destructive">
              <AlertTriangle className="mt-0.5 size-4 shrink-0" />
              <span className="min-w-0 flex-1 break-all">{error}</span>
            </div>
          ) : null}

          {/* 筛选 chip：数量与卡片同源（都从 `groups` 数），不另算一份。 */}
          <div className="flex flex-wrap items-center gap-1.5">
            {filters
              .filter((f) => f.key === "all" || f.n > 0)
              .map((f) => (
                <button
                  key={f.key}
                  type="button"
                  onClick={() => setFilter(f.key)}
                  className={cn(
                    "rounded-full border px-2.5 py-1 text-xs transition-colors",
                    filter === f.key
                      ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                      : "text-muted-foreground hover:bg-foreground/[0.03]",
                  )}
                >
                  {f.label} {f.n}
                </button>
              ))}
            <span className="ml-auto text-[11px] text-muted-foreground">
              共 {accountRows.length} 个账号参与比对
            </span>
          </div>

          {loading && groups.length === 0 ? (
            <Skeleton className="h-40 w-full rounded-xl" />
          ) : shown.length === 0 ? (
            <div className="space-y-1.5 py-8 text-center">
              <p className="text-sm text-muted-foreground">
                {groups.length === 0 ? "还没有关联记录" : "当前筛选下没有关联组"}
              </p>
              <p className="text-xs text-muted-foreground">
                {groups.length === 0
                  ? "在「复制与关联」里把会话复制给另一个账号，复制成功后会自动登记成一组。"
                  : "换一个筛选条件看看。"}
              </p>
            </div>
          ) : (
            <div className="grid gap-3 md:grid-cols-2">
              {shown.map((g) => {
                const pill = STATUS_PILL[g.status] ?? STATUS_PILL.unknown;
                const busy = busyGroup === g.key;
                return (
                  <button
                    key={g.key}
                    type="button"
                    onClick={() => setOpenKey(g.key)}
                    className="rounded-xl border px-3.5 py-3 text-left transition-colors hover:border-foreground/20 hover:bg-foreground/[0.02]"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="inline-flex min-w-0 items-center gap-1 text-[11px] text-muted-foreground">
                        <FolderGit2 className="size-3.5 shrink-0" />
                        <span className="truncate">{g.project || "未分组"}</span>
                      </span>
                      <span className="shrink-0 text-[11px] text-muted-foreground">
                        {busy ? (
                          <span className="inline-flex items-center gap-1">
                            <Loader2 className="size-3 animate-spin" />
                            处理中
                          </span>
                        ) : (
                          relativeTime(g.updatedAt)
                        )}
                      </span>
                    </div>
                    <div className="mt-1.5 line-clamp-2 text-[13px] font-medium leading-snug">
                      {g.title}
                    </div>
                    <div className="mt-2.5 flex items-center justify-between gap-2">
                      <span className="inline-flex items-center gap-1 text-[11px] text-muted-foreground">
                        <Users className="size-3.5" />
                        {g.memberCount} 个账号
                        {g.missingCount > 0 ? ` · ${g.missingCount} 个还没有` : ""}
                      </span>
                      <span
                        className={cn(
                          "shrink-0 rounded-full border px-1.5 py-px text-[10px] font-medium",
                          pill.tone,
                        )}
                      >
                        {pill.label}
                      </span>
                    </div>
                  </button>
                );
              })}
            </div>
          )}

          {storePath ? (
            <p className="text-[11px] leading-relaxed text-muted-foreground">
              关联记录：<code className="font-mono">{storePath}</code>（只删记录，不动会话数据）
            </p>
          ) : null}
        </CardContent>
      </Card>

      {/* ── 组详情：账号矩阵 + 三个操作 ─────────────────────────────
          ⚠️ 宽度必须与 WorkBuddy 一致（`max-w-3xl`）：双列卡片 + 中间枢纽在 2xl（672px）
          里挤不开，账号名一长就横向溢出（2026-10-31 用户截图）。 */}
      <Dialog
        // ⚠️ 判据是 `detail !== null` 而不是 `openKey !== null`：
        // 解除关联后组已从 `groups` 里消失，`openKey` 还留着 ⇒ 弹窗变成一个**空白白框**
        // 只剩关闭按钮（2026-10-31 用户截图）。跟着 detail 走，组没了弹窗自己关掉。
        open={detail !== null}
        onOpenChange={(v) => {
          if (!v) setOpenKey(null);
        }}
      >
        <DialogContent className="max-w-3xl overflow-x-hidden">
          {detail ? (
            <>
              <DialogHeader className="gap-1.5">
                <DialogTitle className="pr-6 text-base leading-snug">{detail.title}</DialogTitle>
                <DialogDescription asChild>
                  <div className="flex flex-wrap items-center gap-2 text-xs">
                    <span className="inline-flex max-w-[16rem] items-center gap-1 rounded-md border bg-muted/50 px-1.5 py-0.5">
                      <FolderGit2 className="size-3 shrink-0" />
                      <span className="truncate">{detail.project || "未分组"}</span>
                    </span>
                    <span>
                      {detail.memberCount} 个账号 · 共 {accountRows.length} 个账号参与比对
                    </span>
                    <span
                      className={cn(
                        "rounded-full border px-1.5 py-px text-[10px] font-medium",
                        (STATUS_PILL[detail.status] ?? STATUS_PILL.unknown).tone,
                      )}
                    >
                      {(STATUS_PILL[detail.status] ?? STATUS_PILL.unknown).label}
                    </span>
                  </div>
                </DialogDescription>
              </DialogHeader>

              {/* `overflow-x-hidden`：宁可截断也不让弹窗出现横向滚动条。
                  纵向滚动保留（组多时仍要能滚）。 */}
              <div className="-mr-2 max-h-[62vh] space-y-3 overflow-x-hidden overflow-y-auto pr-2">
                {detail.status === "missing" ? (
                  <div className="flex items-start gap-2.5 rounded-xl border border-amber-500/25 bg-amber-500/[0.06] px-3.5 py-3">
                    <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-600" />
                    <div className="min-w-0 flex-1 text-xs leading-relaxed text-amber-700">
                      有账号的副本已经找不到了（客户端里删过 / 换过库）。可以「补一份」，
                      或者解除这一组关联。
                    </div>
                  </div>
                ) : detail.status === "pending" ? (
                  <div className="flex items-start gap-2.5 rounded-xl border border-amber-500/25 bg-amber-500/[0.06] px-3.5 py-3">
                    <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-600" />
                    <div className="min-w-0 flex-1">
                      <div className="text-[13px] font-semibold text-amber-700 dark:text-amber-300">
                        有副本落后了，可以安全快进
                      </div>
                      <div className="mt-1 text-xs leading-relaxed text-amber-700/80 dark:text-amber-300/80">
                        在下方点「以此为准」，会用它覆盖组内其它账号的副本（保持会话 id 不变）。
                      </div>
                    </div>
                  </div>
                ) : null}

                {/* 关联图：与 WorkBuddy **同一版式**（双列卡片 + 中间枢纽 + 连线）。
                    ⚠️ 判据各产品不同（Trae 只比条数与时间），但版式必须一致 ——
                    用户来回切产品时不该重新学一遍。 */}
                <div>
                  <div className="mb-2 flex items-center justify-between gap-2">
                    <span className="text-[13px] font-semibold">会话关联图</span>
                    <span className="rounded-full border bg-muted/50 px-2 py-0.5 text-[10px] text-muted-foreground">
                      {detail.memberCount} 个账号
                    </span>
                  </div>

                  <div className="rounded-2xl border border-emerald-500/15 bg-emerald-500/[0.04] p-3.5">
                    {/* ⚠️ 必须用 `minmax(0,1fr)` 而不是 `1fr`：`1fr` 的最小宽度是 `auto`，
                        卡片里一有长账号名/长原因文本，列就会被撑宽，整个弹窗横向溢出
                        （2026-10-31 用户截图：底部出现横向滚动条）。 */}
                    <div className="grid items-center gap-4 sm:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)]">
                      <div className="min-w-0 space-y-2.5">
                        {detail.members
                          .filter((_, i) => i % 2 === 0)
                          .map((m) => (
                            <div key={m.uid + m.sessionId} className="relative">
                              <MemberCard
                                m={m}
                                busy={busyGroup}
                                groupKey={detail.key}
                                canUnify={detail.canUnify && detail.pairwise !== false}
                                onUnify={() => onUnify(detail, m)}
                                onView={() => void openViewer(m)}
                                onUnlink={() => onUnlinkMember(detail, m)}
                              />
                              <span className="pointer-events-none absolute top-1/2 -right-4 hidden h-px w-3.5 bg-emerald-500/30 sm:block" />
                            </div>
                          ))}
                      </div>

                      <div className="mx-auto flex w-[132px] flex-col items-center justify-center rounded-xl border border-emerald-500/20 bg-card px-3 py-4 text-center shadow-sm">
                        <span className="grid size-9 place-items-center rounded-lg bg-emerald-500/10 ring-1 ring-emerald-500/15">
                          <ScrollText className="size-4 text-emerald-600 dark:text-emerald-400" />
                        </span>
                        <div className="mt-2 text-[13px] font-semibold leading-tight">
                          同一会话
                        </div>
                        <div className="mt-0.5 text-[10px] text-muted-foreground">
                          {detail.memberCount} 个关联账号
                        </div>
                      </div>

                      <div className="min-w-0 space-y-2.5">
                        {detail.members
                          .filter((_, i) => i % 2 === 1)
                          .map((m) => (
                            <div key={m.uid + m.sessionId} className="relative">
                              <span className="pointer-events-none absolute top-1/2 -left-4 hidden h-px w-3.5 bg-emerald-500/30 sm:block" />
                              <MemberCard
                                m={m}
                                busy={busyGroup}
                                groupKey={detail.key}
                                canUnify={detail.canUnify && detail.pairwise !== false}
                                onUnify={() => onUnify(detail, m)}
                                onView={() => void openViewer(m)}
                                onUnlink={() => onUnlinkMember(detail, m)}
                              />
                            </div>
                          ))}
                      </div>
                    </div>
                  </div>
                </div>

                {/* 补一份：组里没有副本的账号，一键以新 id 复制进来并登记关联 */}
                {accountRows.filter((a) => !detail.members.some((m) => m.uid === a.uid)).length > 0 && (
                  <div className="flex flex-wrap items-center gap-2 rounded-xl border border-dashed px-3 py-2.5">
                    <span className="text-[11px] text-muted-foreground">补到：</span>
                    {accountRows
                      .filter((a) => !detail.members.some((m) => m.uid === a.uid))
                      .map((a) => (
                        <button
                          key={a.uid}
                          type="button"
                          disabled={busyGroup !== null || !detail.canCopy}
                          onClick={() => onCopy(detail, a.uid)}
                          title="把这段对话以新 id 复制到这个账号，并登记关联"
                          className="inline-flex max-w-full items-center gap-1 rounded-full border border-dashed px-2.5 py-1 text-[11px] font-medium text-muted-foreground transition hover:border-solid hover:bg-muted/60 hover:text-foreground disabled:opacity-50"
                        >
                          <Plus className="size-3 shrink-0" />
                          {/* 账号名带 uid 尾号，长起来会顶出弹窗；截断 + title 保住完整值 */}
                          <span className="truncate">{a.label}</span>
                        </button>
                      ))}
                  </div>
                )}

                <div className="flex flex-wrap items-center gap-2">
                  <button
                    type="button"
                    onClick={() => {
                      setOpenKey(null);
                      onNewLink?.();
                    }}
                    className="inline-flex items-center gap-1 rounded-full border border-dashed px-3 py-1.5 text-[11px] font-medium text-muted-foreground transition hover:border-solid hover:bg-muted/60 hover:text-foreground"
                  >
                    <Plus className="size-3" />
                    关联新账号
                  </button>
                  <span className="ml-auto text-[11px] text-muted-foreground">
                    在上方选择一个副本作为标准，会覆盖其它账号
                  </span>
                </div>

                <p className="text-[11px] leading-relaxed text-muted-foreground">
                  连线只表示这些账号属于同一段对话，不会自动选择要保留的内容；
                  「以此为准」才会改动内容，且覆盖前会把每个被改的副本各备份一份。
                  {detail.status === "consistent" ? (
                    <>
                      {" "}
                      <span className="inline-flex items-center gap-1 text-emerald-600">
                        <CheckCircle2 className="size-3" />
                        当前各账号副本一致
                      </span>
                    </>
                  ) : null}
                </p>

                <div className="flex flex-wrap items-center justify-end gap-2 border-t pt-2">
                  {busyGroup === detail.key && busyNote ? (
                    <span className="inline-flex items-center gap-1.5 text-[11px] text-muted-foreground">
                      <Loader2 className="size-3 animate-spin" />
                      {busyNote}
                    </span>
                  ) : null}
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 px-2 text-[11px] text-muted-foreground"
                    disabled={busyGroup !== null}
                    title="只删这条关联记录，不动任何会话数据"
                    onClick={() => onUnlink(detail)}
                  >
                    <Link2Off className="size-3.5" />
                    解除这一组
                  </Button>
                </div>
              </div>
            </>
          ) : null}
        </DialogContent>
      </Dialog>

      {/* ── 查看内容（只读预览） ───────────────────────────────────
          版式照 WorkBuddy 的同名弹窗：标题「某账号 的副本」+ 只读说明 + 逐轮正文。 */}
      <Dialog
        open={viewer !== null}
        onOpenChange={(v) => {
          if (!v) setViewer(null);
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle className="pr-6">
              {viewer?.member.accountName ?? ""} 的副本
            </DialogTitle>
            <DialogDescription>
              {typeof viewer?.member.messages === "number"
                ? `${viewer.member.messages.toLocaleString("zh-CN")} 条内容`
                : "条数未知"}
              {" · "}
              只读预览
            </DialogDescription>
          </DialogHeader>
          <div className="max-h-[60vh] space-y-3 overflow-y-auto pr-1">
            {viewerLoading ? (
              <Skeleton className="h-32 w-full" />
            ) : (viewer?.turns.length ?? 0) === 0 ? (
              <p className="py-8 text-center text-sm text-muted-foreground">
                这份副本没有可显示的对话内容。
              </p>
            ) : (
              viewer?.turns.map((t, i) => (
                <div key={i} className="space-y-1">
                  <div className="flex items-center gap-2">
                    <span
                      className={cn(
                        "rounded-full border px-1.5 py-px text-[10px] font-medium",
                        t.role === "user"
                          ? "border-sky-500/30 bg-sky-500/10 text-sky-700 dark:text-sky-300"
                          : "border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300",
                      )}
                    >
                      {t.role === "user" ? "我" : "助手"}
                    </span>
                    {t.tools.length > 0 ? (
                      <span className="truncate text-[10px] text-muted-foreground">
                        {t.tools.join(" · ")}
                      </span>
                    ) : null}
                  </div>
                  <div className="whitespace-pre-wrap break-words rounded-lg border bg-muted/30 px-2.5 py-2 text-xs leading-relaxed">
                    {t.text || "（空）"}
                  </div>
                </div>
              ))
            )}
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
