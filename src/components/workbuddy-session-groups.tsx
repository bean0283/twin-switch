import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  Check,
  ChevronDown,
  FolderGit2,
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
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import * as api from "@/lib/api";
import type {
  WbGroupCandidate,
  WbGroupMember,
  WbSessionGroup,
  WbSessionGroupsReport,
  WbSessionTurn,
} from "@/lib/trae-types";
import { cn } from "@/lib/utils";

/**
 * 关联会话（第三个分页）。
 *
 * 会话在一行里只属于一个账号、正文里也写着自己的会话 id，所以「同一段对话」在
 * 每个账号下必然是独立副本。这个界面把副本按对话摆在一起，回答三个问题：
 *   1. 这段对话在哪些账号里有？
 *   2. 内容是不是同一份（谁落后、谁分叉）？
 *   3. 缺的账号怎么补齐、分叉的以谁为准？
 *
 * ⚠️ 状态 `unknown`（无法确认）必须与 `consistent`（内容一致）分开显示 ——
 * 本工具没做过比对就不能说「一样」。
 */

type Filter = "all" | "pending" | "diverged" | "consistent" | "missing";

/** 详情接口比列表项多带 accountTotal / clientRunning（与列表同源）。 */
type GroupDetail = WbSessionGroup & {
  accountTotal?: number;
  clientRunning?: boolean;
};

const STATUS: Record<string, { text: string; dot: string; tone: string }> = {
  consistent: {
    text: "内容一致",
    dot: "bg-emerald-500",
    tone: "text-emerald-600",
  },
  pending: { text: "待同步", dot: "bg-amber-500", tone: "text-amber-600" },
  diverged: { text: "有分歧", dot: "bg-destructive", tone: "text-destructive" },
  missing: {
    text: "内容缺失",
    dot: "bg-muted-foreground/60",
    tone: "text-muted-foreground",
  },
  unknown: {
    text: "无法确认",
    dot: "bg-muted-foreground/60",
    tone: "text-muted-foreground",
  },
};

const RELATION: Record<string, string> = {
  latest: "最新",
  equal: "相同",
  behind: "落后",
  ahead: "超前",
  diverged: "分叉",
  "no-body": "无正文",
  unknown: "未比对",
};

function relTime(ms: number): string {
  if (!ms) return "—";
  const diff = Date.now() - ms;
  if (diff < 60_000) return "刚刚";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)} 分钟前`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)} 小时前`;
  const d = new Date(ms);
  const clock = `${String(d.getHours()).padStart(2, "0")}:${String(
    d.getMinutes(),
  ).padStart(2, "0")}`;
  const md = `${d.getMonth() + 1}/${d.getDate()} ${clock}`;
  return d.getFullYear() === new Date().getFullYear()
    ? md
    : `${d.getFullYear()}/${md}`;
}

function statusOf(key: string) {
  return STATUS[key] ?? STATUS.unknown;
}

/**
 * 写入类操作的结果说明。
 *
 * ⚠️ 客户端是**我们关的**，就得说清「已经帮你开回来了」；只有真的没开成时才让人手动开。
 * 以前这里写死「需要你自己再打开」，同步完还得手动点一次（2026-10-24 用户反馈）。
 */
function clientNote(
  r: { clientStopped?: boolean; clientRestarted?: boolean; restartError?: string },
  fallback?: string,
): string | undefined {
  if (!r.clientStopped) return fallback;
  if (r.clientRestarted) {
    return fallback
      ? `已先退出 WorkBuddy 再写入，写完后自动重新启动。${fallback}`
      : "已先退出 WorkBuddy 再写入，写完后已自动重新启动";
  }
  return `已先退出 WorkBuddy 再写入，但自动启动失败${
    r.restartError ? `：${r.restartError}` : ""
  }，请手动打开客户端`;
}

/**
 * 扫描结果缓存：这个列表要读会话库 + 逐行摘要，本机实测约 4 秒。
 * 切到别的分页再切回来时不该重新等一遍（Radix 的 Tabs 切走会卸载内容）。
 * 只缓存 60 秒，且「刷新」按钮强制绕过 —— 会话是会被客户端改的，缓存不能当真。
 */
let CACHE: { at: number; data: WbSessionGroupsReport } | null = null;
const CACHE_TTL = 60_000;

export function WorkbuddySessionGroups() {
  const [data, setData] = useState<WbSessionGroupsReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<Filter>("all");

  const [openKey, setOpenKey] = useState<string | null>(null);
  const [detail, setDetail] = useState<GroupDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);

  const [viewer, setViewer] = useState<{
    member: WbGroupMember;
    turns: WbSessionTurn[];
    bodyPath: string | null;
  } | null>(null);
  const [viewerLoading, setViewerLoading] = useState(false);

  const [addOpen, setAddOpen] = useState(false);
  const [addLoading, setAddLoading] = useState(false);
  const [candidates, setCandidates] = useState<WbGroupCandidate[]>([]);
  const [pickSession, setPickSession] = useState<string>("");
  const [pickUid, setPickUid] = useState<string>("");

  const [confirmUnify, setConfirmUnify] = useState<{
    group: GroupDetail;
    member: WbGroupMember;
  } | null>(null);

  const load = useCallback(async (force = false) => {
    if (!force && CACHE && Date.now() - CACHE.at < CACHE_TTL) {
      setData(CACHE.data);
      setError(null);
      setLoading(false);
      return;
    }
    // ⚠️ 强制刷新必须把缓存清掉：写入（同步 / 关联）之后组的 key 会变，
    // 留着旧缓存等于把过期 key 继续发给用户点（点一次报一次「不存在」）。
    if (force) CACHE = null;
    setLoading(true);
    setError(null);
    try {
      const r = await api.workbuddySessionGroups();
      if (!r.ok) throw new Error(r.error || "读取关联会话失败");
      CACHE = { at: Date.now(), data: r };
      setData(r);
    } catch (e) {
      // ⚠️ 读取失败必须与「没有关联会话」长得不一样：撤空态、明写原因，
      // 否则用户会以为关联记录被删了。
      setData(null);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(false);
  }, [load]);

  /**
   * 打开组详情。
   *
   * `retried`：首次失败后重试一次。会话库随时可能被客户端改写，列表与后端可能
   * 差一拍（组被合并 / 拆开），重拉一次列表再试比直接把错误丢给用户更有用。
   */
  const openDetail = useCallback(async (key: string, retried = false) => {
    setOpenKey(key);
    setDetail(null);
    setDetailLoading(true);
    try {
      const r = await api.workbuddySessionGroupDetail(key);
      if (!r.ok) throw new Error(r.error || "读取组详情失败");
      setDetail(r as unknown as GroupDetail);
    } catch (e) {
      if (!retried) {
        setDetailLoading(false);
        await load(true);
        await openDetail(key, true);
        return;
      }
      toast.error(e instanceof Error ? e.message : String(e));
      setOpenKey(null);
    } finally {
      setDetailLoading(false);
    }
  }, [load]);

  const items = useMemo(() => {
    const all = data?.items ?? [];
    switch (filter) {
      case "diverged":
        // 「无法确认」归这里：它同样需要人工看一眼，但卡片上仍写自己的文案。
        return all.filter((g) => g.status === "diverged" || g.status === "unknown");
      case "consistent":
      case "pending":
      case "missing":
        return all.filter((g) => g.status === filter);
      default:
        return all;
    }
  }, [data, filter]);

  const clientRunning = data?.clientRunning ?? false;

  const doUnify = async () => {
    if (!confirmUnify) return;
    const { group, member } = confirmUnify;
    setConfirmUnify(null);
    setBusy(`unify:${group.key}`);
    try {
      // 第三个参数 = 「客户端在跑就由本工具关掉再写」。用户在确认框里已经同意。
      const r = await api.workbuddySessionGroupUnify(
        group.key,
        member.sessionId,
        true,
      );
      if (!r.ok) throw new Error(r.error || "同步失败");
      toast.success(
        `已用「${member.accountName}」的副本统一 ${r.syncedCount ?? 0} 个账号`,
        {
          description: clientNote(
            r,
            r.backupDir ? `原内容已备份：${r.backupDir}` : "原内容已备份，可人工恢复",
          ),
        },
      );
      await load(true);
      // 同步会把同标题簇升级成正式关联组，key 从 `dup:` 换成 `link:`；
      // 用后端回传的新 key 重开，否则拿到的是刚失效的旧 key。
      await openDetail(r.groupKey || group.key);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const doLinkAccount = async (group: WbSessionGroup, uid: string, label: string) => {
    setBusy(`link:${group.key}:${uid}`);
    try {
      const r = await api.workbuddySessionGroupLinkAccount(group.key, uid, true);
      if (r.ok === false) throw new Error(r.error || "关联失败");
      toast.success(`已给「${label}」补一份副本`, {
        description: clientNote(r),
      });
      await load(true);
      await openDetail(r.groupKey || group.key);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const openViewer = async (member: WbGroupMember) => {
    setViewerLoading(true);
    setViewer({ member, turns: [], bodyPath: null });
    try {
      const d = await api.workbuddySessionDetail(member.uid, member.sessionId);
      if (!d.ok) {
        throw new Error(d.error || d.bodyError || "读不到这个副本的正文");
      }
      setViewer({
        member,
        turns: d.turns ?? [],
        bodyPath: d.bodyPath ?? null,
      });
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
      setViewer(null);
    } finally {
      setViewerLoading(false);
    }
  };

  const openAdd = async () => {
    setAddOpen(true);
    setAddLoading(true);
    setPickSession("");
    setPickUid("");
    try {
      const r = await api.workbuddySessionGroupCandidates();
      if (!r.ok) throw new Error(r.error || "读取可关联会话失败");
      setCandidates(r.items ?? []);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
      setAddOpen(false);
    } finally {
      setAddLoading(false);
    }
  };

  const doAdd = async () => {
    if (!pickSession || !pickUid) return;
    setBusy("add");
    try {
      const r = await api.workbuddySessionGroupLinkSession(pickSession, pickUid, true);
      if (r.ok === false) throw new Error(r.error || "关联失败");
      const n = r.copied ?? 1;
      toast.success(`已关联：复制了 ${n} 个副本`, {
        description: clientNote(r, "复制后会自动进入同一个关联组"),
      });
      setAddOpen(false);
      await load(true);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };

  const pickedCandidate = candidates.find((c) => c.sessionId === pickSession);

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader className="flex-row items-start justify-between gap-3 space-y-0">
          <div className="space-y-1">
            <CardTitle className="text-base">关联会话</CardTitle>
            <CardDescription>
              同一段对话在每个账号下各有一份副本。这里按对话归拢，标出谁落后、谁分叉，
              并可以直接补齐账号或统一内容。
            </CardDescription>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => void load(true)}
              disabled={loading}
            >
              {loading ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <RefreshCw className="size-4" />
              )}
              刷新
            </Button>
            <Button size="sm" onClick={() => void openAdd()}>
              <Plus className="size-4" />
              新增关联会话
            </Button>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          {clientRunning && (
            <div className="flex items-start gap-2.5 rounded-xl border border-amber-500/25 bg-amber-500/[0.06] px-3.5 py-2.5">
              <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-400" />
              <div className="min-w-0 flex-1 text-xs leading-relaxed text-amber-700 dark:text-amber-300">
                WorkBuddy 正在运行。「以此为准」会先弹确认框，确认后由本工具退出客户端再写入
                （它开着会把正文写回去）；「关联」和「新增关联会话」同样会先关掉它。
                <span className="font-medium">
                  写完之后会自动把它重新启动
                </span>
                ，不用你自己去开。
              </div>
            </div>
          )}

          {loading && (
            <div className="grid gap-3 md:grid-cols-2">
              <Skeleton className="h-24 w-full" />
              <Skeleton className="h-24 w-full" />
              <Skeleton className="h-24 w-full" />
              <Skeleton className="h-24 w-full" />
            </div>
          )}

          {!loading && error && (
            <div className="flex items-start gap-2.5 rounded-xl border border-destructive/25 bg-destructive/[0.06] px-3.5 py-3">
              <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
              <div className="min-w-0 flex-1">
                <div className="text-sm font-semibold text-destructive">
                  读取失败，不是关联记录被删了
                </div>
                <div className="mt-0.5 break-words text-xs text-destructive/80">
                  {error}
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  className="mt-2"
                  onClick={() => void load(true)}
                >
                  重试
                </Button>
              </div>
            </div>
          )}

          {!loading && !error && data && (
            <>
              <div className="flex flex-wrap items-center gap-1.5">
                {(
                  [
                    ["all", "全部"],
                    ["pending", "待同步"],
                    ["diverged", "有分歧"],
                    ["consistent", "内容一致"],
                    ["missing", "内容缺失"],
                  ] as Array<[Filter, string]>
                ).map(([key, label]) => {
                  const n =
                    key === "all"
                      ? data.counts?.all ?? 0
                      : data.counts?.[key] ?? 0;
                  const on = filter === key;
                  return (
                    <button
                      key={key}
                      type="button"
                      onClick={() => setFilter(key)}
                      className={cn(
                        "h-7 rounded-full border px-3 text-xs transition",
                        on
                          ? "border-foreground/15 bg-foreground/[0.06] font-medium text-foreground"
                          : "border-transparent text-muted-foreground hover:bg-muted/60",
                      )}
                    >
                      {label} {n}
                    </button>
                  );
                })}
                <span className="ml-auto text-xs text-muted-foreground">
                  共 {data.accountTotal ?? 0} 个账号参与比对
                </span>
              </div>

              {items.length === 0 ? (
                <p className="py-10 text-center text-sm text-muted-foreground">
                  {filter === "all"
                    ? "还没有可归拢的会话。用「新增关联会话」把一段对话关联到另一个账号后，就会出现在这里。"
                    : "这个筛选下没有会话。"}
                </p>
              ) : (
                <div className="grid gap-2.5 md:grid-cols-2">
                  {items.map((g) => {
                    const s = statusOf(g.status);
                    // 2026-10-24 用户要求：缺账号不单独提示，内容一样就显示「内容一致」。
                    const pill = s.text;
                    return (
                      <button
                        key={g.key}
                        type="button"
                        className="group text-left"
                        onClick={() => void openDetail(g.key)}
                      >
                        <div className="h-full rounded-xl border bg-card p-3 transition group-hover:border-emerald-500/40 group-hover:bg-emerald-500/[0.03]">
                          <div className="flex items-center gap-1.5 text-[11px] text-muted-foreground">
                            <FolderGit2 className="size-3.5 shrink-0" />
                            <span className="truncate">{g.project || "未分组"}</span>
                            <span className="ml-auto shrink-0">
                              {relTime(g.updatedAt)}
                            </span>
                          </div>
                          <div className="mt-1.5 line-clamp-2 text-[13px] font-medium leading-snug">
                            {g.title}
                          </div>
                          <div className="mt-2.5 flex items-center justify-between gap-2">
                            <span className="inline-flex items-center gap-1 text-[11px] text-muted-foreground">
                              <Users className="size-3.5" />
                              {g.memberCount} 个账号
                            </span>
                            <span
                              className={cn(
                                "shrink-0 rounded-full border px-1.5 py-px text-[10px] font-medium",
                                statusPill(g.status),
                              )}
                            >
                              {pill}
                            </span>
                          </div>
                        </div>
                      </button>
                    );
                  })}
                </div>
              )}

              <p className="text-[11px] leading-relaxed text-muted-foreground">
                {data.note}
              </p>
            </>
          )}
        </CardContent>
      </Card>

      {/* ── 组详情 ─────────────────────────────────────────────── */}
      <Dialog
        open={openKey !== null}
        onOpenChange={(v) => {
          if (!v) {
            setOpenKey(null);
            setDetail(null);
          }
        }}
      >
        <DialogContent className="max-w-3xl">
          {detailLoading || !detail ? (
            <div className="space-y-3 py-4">
              <Skeleton className="h-6 w-2/3" />
              <Skeleton className="h-20 w-full" />
              <Skeleton className="h-40 w-full" />
            </div>
          ) : (
            <>
              <DialogHeader className="gap-2">
                <DialogTitle className="pr-6 text-lg leading-snug">
                  {detail.title}
                </DialogTitle>
                <DialogDescription asChild>
                  <div className="flex flex-wrap items-center gap-2 text-xs">
                    <span className="inline-flex max-w-[16rem] items-center gap-1 rounded-md border bg-muted/50 px-1.5 py-0.5">
                      <FolderGit2 className="size-3 shrink-0" />
                      <span className="truncate">
                        {detail.project || "未分组"}
                      </span>
                    </span>
                    <span className="text-muted-foreground">
                      {detail.memberCount} 个账号 · 共 {detail.accountTotal ?? 0}{" "}
                      个账号参与
                    </span>
                  </div>
                </DialogDescription>
              </DialogHeader>

              <div className="-mr-2 max-h-[68vh] space-y-4 overflow-y-auto pr-2">
                {/* 状态横幅：参考项目那种「浅底 + 深字 + 图标」的窄条 */}
                {detail.status === "diverged" || detail.status === "unknown" ? (
                  <div className="flex items-start gap-2.5 rounded-xl border border-destructive/25 bg-destructive/[0.06] px-3.5 py-3">
                    <AlertTriangle className="mt-0.5 size-4 shrink-0 text-destructive" />
                    <div className="min-w-0 flex-1">
                      <div className="text-[13px] font-semibold text-destructive">
                        {detail.status === "diverged"
                          ? `${detail.memberCount} 个账号的会话内容不一致`
                          : "无法确认这些副本是否一致"}
                      </div>
                      <div className="mt-1 text-xs leading-relaxed text-destructive/80">
                        {detail.status === "diverged"
                          ? "选择要保留的一份；确认后会把它的内容统一到其它可验证的账号。"
                          : "这些副本的标题被改过或体积超限，本工具没有做逐行比对 —— 不能当成一致。"}
                      </div>
                    </div>
                  </div>
                ) : detail.status === "missing" ? (
                  <div className="flex items-start gap-2.5 rounded-xl border border-amber-500/25 bg-amber-500/[0.06] px-3.5 py-3">
                    <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-400" />
                    <div className="min-w-0 flex-1">
                      <div className="text-[13px] font-semibold text-amber-700 dark:text-amber-300">
                        有副本的正文文件不见了
                      </div>
                      <div className="mt-1 text-xs leading-relaxed text-amber-700/80 dark:text-amber-300/80">
                        会话库里还有这行记录，但磁盘上找不到对应的正文。可以选一份正常的副本作为标准补回去。
                      </div>
                    </div>
                  </div>
                ) : detail.status === "pending" ? (
                  <div className="flex items-start gap-2.5 rounded-xl border border-amber-500/25 bg-amber-500/[0.06] px-3.5 py-3">
                    <AlertTriangle className="mt-0.5 size-4 shrink-0 text-amber-600 dark:text-amber-400" />
                    <div className="min-w-0 flex-1">
                      {/* 「待同步」只剩一种原因：真有副本落后（缺账号已不算状态）。 */}
                      <div className="text-[13px] font-semibold text-amber-700 dark:text-amber-300">
                        {`有 ${detail.behindCount ?? detail.memberCount} 个副本落后了，可以安全同步`}
                      </div>
                      <div className="mt-1 text-xs leading-relaxed text-amber-700/80 dark:text-amber-300/80">
                        落后的是「最新副本的前半段」，没有冲突内容。选一份作为标准即可补齐。
                      </div>
                    </div>
                  </div>
                ) : (
                  <div className="flex items-start gap-2.5 rounded-xl border border-emerald-500/25 bg-emerald-500/[0.06] px-3.5 py-3">
                    <Check className="mt-0.5 size-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
                    <div className="min-w-0 flex-1">
                      <div className="text-[13px] font-semibold text-emerald-700 dark:text-emerald-300">
                        {detail.memberCount} 个账号的这份会话内容一致
                      </div>
                      <div className="mt-1 text-xs leading-relaxed text-emerald-700/80 dark:text-emerald-300/80">
                        不需要处理。以后某个账号继续往下聊，这里会重新标成「待同步」或「有分歧」。
                      </div>
                    </div>
                  </div>
                )}

                {/* 关联图 */}
                <div>
                  <div className="mb-2 flex items-center justify-between gap-2">
                    <span className="text-[13px] font-semibold">会话关联图</span>
                    <span className="rounded-full border bg-muted/50 px-2 py-0.5 text-[10px] text-muted-foreground">
                      {detail.memberCount} 个账号
                    </span>
                  </div>

                  <div className="rounded-2xl border border-emerald-500/15 bg-emerald-500/[0.04] p-3.5">
                    <div className="grid items-center gap-4 sm:grid-cols-[1fr_auto_1fr]">
                      <div className="space-y-2.5">
                        {detail.members
                          .filter((_, i) => i % 2 === 0)
                          .map((m) => (
                            <div key={m.uid + m.sessionId} className="relative">
                              <MemberCard
                                m={m}
                                busy={busy}
                                groupKey={detail.key}
                                onUnify={() =>
                                  setConfirmUnify({ group: detail, member: m })
                                }
                                onView={() => void openViewer(m)}
                              />
                              <span className="pointer-events-none absolute -right-4 top-1/2 hidden h-px w-3.5 bg-emerald-500/30 sm:block" />
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

                      <div className="space-y-2.5">
                        {detail.members
                          .filter((_, i) => i % 2 === 1)
                          .map((m) => (
                            <div key={m.uid + m.sessionId} className="relative">
                              <span className="pointer-events-none absolute -left-4 top-1/2 hidden h-px w-3.5 bg-emerald-500/30 sm:block" />
                              <MemberCard
                                m={m}
                                busy={busy}
                                groupKey={detail.key}
                                onUnify={() =>
                                  setConfirmUnify({ group: detail, member: m })
                                }
                                onView={() => void openViewer(m)}
                              />
                            </div>
                          ))}
                      </div>
                    </div>
                  </div>
                </div>

                {detail.missingAccounts.length > 0 && (
                  <div className="flex flex-wrap items-center gap-2 rounded-xl border border-dashed px-3 py-2.5">
                    <span className="text-[11px] text-muted-foreground">补到：</span>
                    {detail.missingAccounts.map((a) => (
                      <button
                        key={a.uid}
                        type="button"
                        disabled={busy === `link:${detail.key}:${a.uid}`}
                        onClick={() => void doLinkAccount(detail, a.uid, a.label)}
                        className="inline-flex items-center gap-1 rounded-full border border-dashed px-2.5 py-1 text-[11px] font-medium text-muted-foreground transition hover:border-solid hover:bg-muted/60 hover:text-foreground disabled:opacity-50"
                      >
                        {busy === `link:${detail.key}:${a.uid}` ? (
                          <Loader2 className="size-3 animate-spin" />
                        ) : (
                          <Plus className="size-3" />
                        )}
                        关联 {a.label}
                      </button>
                    ))}
                  </div>
                )}

                <div className="flex flex-wrap items-center gap-2">
                  <button
                    type="button"
                    onClick={() => {
                      // 先收起详情：两个弹窗叠在一起会抢焦点。
                      setOpenKey(null);
                      void openAdd();
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
                </p>
              </div>
            </>
          )}
        </DialogContent>
      </Dialog>

      {/* ── 覆盖确认 ───────────────────────────────────────────── */}
      <Dialog
        open={confirmUnify !== null}
        onOpenChange={(v) => {
          if (!v) setConfirmUnify(null);
        }}
      >
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>用这份副本覆盖其它账号？</DialogTitle>
            <DialogDescription asChild>
              <div className="space-y-2 text-[13px] leading-relaxed text-muted-foreground">
                <p>
                  将以「{confirmUnify?.member.accountName}」的副本为准，覆盖组内其它{" "}
                  {Math.max((confirmUnify?.group.memberCount ?? 1) - 1, 0)}{" "}
                  个账号的同名副本。被覆盖的原内容会先备份，可人工恢复。
                </p>
                {(confirmUnify?.group.clientRunning ?? clientRunning) && (
                  <p className="flex items-start gap-1.5 rounded-lg border border-amber-500/25 bg-amber-500/[0.06] px-2.5 py-2 text-xs text-amber-700 dark:text-amber-300">
                    <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
                    <span>
                      WorkBuddy 正在运行，点「确认覆盖」会先关闭它（约 1~2
                      秒），写完不会自动启动 —— 需要你自己再打开。
                    </span>
                  </p>
                )}
              </div>
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setConfirmUnify(null)}>
              取消
            </Button>
            <Button onClick={() => void doUnify()}>确认覆盖</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* ── 查看内容（只读） ───────────────────────────────────── */}
      <Dialog
        open={viewer !== null}
        onOpenChange={(v) => {
          if (!v) setViewer(null);
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle className="pr-6">
              {viewer?.member.accountName} 的副本
            </DialogTitle>
            <DialogDescription>
              {viewer?.member.recordCount == null
                ? "条数未知"
                : `${viewer.member.recordCount} 条记录`}
              {" · "}
              只读预览
            </DialogDescription>
          </DialogHeader>
          <div className="max-h-[60vh] space-y-3 overflow-y-auto pr-1">
            {viewerLoading ? (
              <Skeleton className="h-32 w-full" />
            ) : (viewer?.turns.length ?? 0) === 0 ? (
              <p className="py-8 text-center text-sm text-muted-foreground">
                没有可显示的对话内容。
              </p>
            ) : (
              viewer?.turns.map((t, i) => (
                <div key={i} className="space-y-1.5 rounded-lg border p-3">
                  <div className="text-xs text-muted-foreground">
                    第 {i + 1} 回合
                  </div>
                  <div className="whitespace-pre-wrap text-sm">
                    {t.userText || "（空）"}
                  </div>
                  <Separator />
                  <div className="whitespace-pre-wrap text-sm text-muted-foreground">
                    {t.assistantText || "（无回答）"}
                  </div>
                </div>
              ))
            )}
          </div>
          {viewer?.bodyPath && (
            <p className="truncate text-xs text-muted-foreground">
              {viewer.bodyPath}
            </p>
          )}
        </DialogContent>
      </Dialog>

      {/* ── 新增关联会话 ───────────────────────────────────────── */}
      <Dialog
        open={addOpen}
        onOpenChange={(v) => {
          if (!v) setAddOpen(false);
        }}
      >
        <DialogContent className="max-w-xl">
          <DialogHeader>
            <DialogTitle>新增关联会话</DialogTitle>
            <DialogDescription>
              选一个会话和一个目标账号：会给那个账号复制一份副本，并登记成关联组。
              原会话不受影响。
            </DialogDescription>
          </DialogHeader>

          {addLoading ? (
            <Skeleton className="h-24 w-full" />
          ) : candidates.length === 0 ? (
            <p className="py-6 text-center text-sm text-muted-foreground">
              本机会话都已经纳入了关联组，没有可新增的了。
            </p>
          ) : (
            <div className="space-y-3">
              <div className="space-y-1.5">
                <div className="text-xs font-medium">会话</div>
                <Select value={pickSession} onValueChange={setPickSession}>
                  <SelectTrigger className="w-full">
                    <SelectValue placeholder="选一个还没关联的会话" />
                  </SelectTrigger>
                  <SelectContent>
                    {candidates.map((c) => (
                      <SelectItem key={c.sessionId} value={c.sessionId}>
                        {c.title} · {c.accountName} · {relTime(c.updatedAt)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="space-y-1.5">
                <div className="text-xs font-medium">关联到账号</div>
                <Select value={pickUid} onValueChange={setPickUid}>
                  <SelectTrigger className="w-full">
                    <SelectValue placeholder="选目标账号" />
                  </SelectTrigger>
                  <SelectContent>
                    {(data?.accounts ?? [])
                      .filter((a) => a.uid !== pickedCandidate?.uid)
                      .map((a) => (
                        <SelectItem key={a.uid} value={a.uid}>
                          {a.label}
                        </SelectItem>
                      ))}
                  </SelectContent>
                </Select>
              </div>
              {pickedCandidate && (
                <p className="text-xs text-muted-foreground">
                  来源：{pickedCandidate.accountName} · {pickedCandidate.project}
                </p>
              )}
              {(detail?.clientRunning ?? clientRunning) && (
                <p className="text-xs text-amber-700 dark:text-amber-300">
                  检测到 WorkBuddy 正在运行：点「关联」会先关闭它再写入。
                </p>
              )}
            </div>
          )}

          <DialogFooter>
            <Button variant="outline" onClick={() => setAddOpen(false)}>
              取消
            </Button>
            <Button
              disabled={!pickSession || !pickUid || busy === "add"}
              onClick={() => void doAdd()}
            >
              {busy === "add" && <Loader2 className="size-4 animate-spin" />}
              关联
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

/** 状态药丸（列表卡片用）：浅底 + 同色描边，深浅主题都成立。 */
function statusPill(key: string): string {
  switch (key) {
    case "consistent":
      return "border-emerald-500/25 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300";
    case "pending":
      return "border-amber-500/25 bg-amber-500/10 text-amber-700 dark:text-amber-300";
    case "diverged":
      return "border-destructive/25 bg-destructive/10 text-destructive";
    default:
      return "border-border bg-muted/60 text-muted-foreground";
  }
}

/** 成员关系药丸：最新 / 落后 / 分叉 各用自己的颜色，一眼能分。 */
function relPill(relation: string): string {
  switch (relation) {
    case "latest":
      return "border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300";
    case "equal":
      return "border-emerald-500/20 bg-emerald-500/[0.06] text-emerald-700/80 dark:text-emerald-300/80";
    case "behind":
      return "border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300";
    case "ahead":
    case "diverged":
      return "border-destructive/30 bg-destructive/10 text-destructive";
    default:
      return "border-border bg-muted/60 text-muted-foreground";
  }
}

function MemberCard({
  m,
  groupKey,
  busy,
  onUnify,
  onView,
}: {
  m: WbGroupMember;
  groupKey: string;
  busy: string | null;
  onUnify: () => void;
  onView: () => void;
}) {
  const canSync = m.contentState === "ready";
  const isBusy = busy === `unify:${groupKey}`;
  return (
    <div className="rounded-xl border bg-card p-2.5 transition hover:border-emerald-500/30">
      <div className="flex items-start justify-between gap-2">
        <span className="truncate text-[13px] font-semibold leading-tight">
          {m.accountName}
        </span>
        <span
          className={cn(
            "shrink-0 rounded-full border px-1.5 py-px text-[10px] font-medium leading-[15px]",
            relPill(m.relation),
          )}
        >
          {RELATION[m.relation] || "—"}
        </span>
      </div>
      <div className="mt-1 text-[11px] text-muted-foreground">
        {m.recordCount == null ? "条数未知" : `${m.recordCount} 条内容`}
      </div>
      <div className="text-[11px] text-muted-foreground/80">
        {relTime(m.updatedAt)}
      </div>
      {m.reason && (
        <div className="mt-1 line-clamp-2 text-[11px] leading-relaxed text-muted-foreground/80">
          {m.reason}
        </div>
      )}
      <div className="mt-2.5 flex items-center gap-1">
        <button
          type="button"
          disabled={!canSync || isBusy}
          title={canSync ? undefined : "这份副本没有正文文件，不能作为标准"}
          onClick={onUnify}
          className={cn(
            "inline-flex items-center gap-1 rounded-full border px-2.5 py-1 text-[11px] font-medium transition",
            canSync
              ? "border-border bg-card text-foreground hover:border-emerald-500/40 hover:bg-emerald-500/[0.06] hover:text-emerald-700 dark:hover:text-emerald-300"
              : "cursor-not-allowed border-border bg-muted/40 text-muted-foreground/50",
          )}
        >
          {isBusy && <Loader2 className="size-3 animate-spin" />}
          以此为准
        </button>
        <button
          type="button"
          disabled={m.contentState !== "ready"}
          onClick={onView}
          className="inline-flex items-center gap-0.5 rounded-full px-2 py-1 text-[11px] font-medium text-emerald-700 transition hover:bg-emerald-500/10 disabled:opacity-40 dark:text-emerald-300"
        >
          查看内容 <ChevronDown className="size-3" />
        </button>
      </div>
    </div>
  );
}
