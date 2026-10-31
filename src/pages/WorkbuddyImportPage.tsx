import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  ArrowRight,
  CheckCircle2,
  Database,
  FolderOpen,
  HardDriveDownload,
  KeyRound,
  Layers,
  LayoutList,
  Loader2,
  Pencil,
  RefreshCw,
  Search,
  Users,
  X,
} from "lucide-react";
import { listen } from "@tauri-apps/api/event";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { Input } from "@/components/ui/input";
import { TraeClientSwitcher, type TraeClientOption } from "@/components/trae-client-switcher";
import { WorkbuddySessionEditor } from "@/components/workbuddy-session-editor";
import { cn } from "@/lib/utils";
import * as api from "@/lib/api";
import { isSelectableClient } from "@/lib/trae-client-list";
import type {
  CrossWriteTarget,
  TraeImportCandidate,
  TraeImportInspect,
  TraeInstalledClient,
  TraeWorkbuddyList,
  TraeWorkbuddyPreviewItem,
  TraeWorkbuddyReport,
  TraeWorkbuddySession,
} from "@/lib/trae-types";
import { crossKeyOf } from "@/lib/trae-types";

/** 「3 分钟前」「昨天 22:03」这类相对时间——重复副本的时间差只有几分钟，绝对时间看不出先后。 */
function relativeTime(ms: number): string {
  if (!ms) return "—";
  const diff = Date.now() - ms;
  if (diff < 0) return formatTime(ms);
  const min = Math.floor(diff / 60000);
  if (min < 1) return "刚刚";
  if (min < 60) return `${min} 分钟前`;
  const hour = Math.floor(min / 60);
  if (hour < 24) return `${hour} 小时前`;
  const day = Math.floor(hour / 24);
  if (day < 30) return `${day} 天前`;
  return formatTime(ms);
}

/** 字节数 → 人类可读。 */
function humanSize(bytes: number): string {
  if (bytes <= 0) return "无正文";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function formatTime(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/**
 * WorkBuddy → Trae 会话导入（`WorkbuddyExportPage` 的反向）。
 *
 * WorkBuddy 的记录是**明文**（`~/.workbuddy/projects/*.jsonl` + `workbuddy.db`），与 Trae 的
 * 加密关系库格式完全不同，因此这里做的是「读取 → 转换 → 加密写入目标账号本地库」，包含提问、
 * 最终回答，以及思考与工具调用过程。
 *
 * ⚠️ 版式必须与 {@link WorkbuddyExportPage} **逐块对应**（T39）：两张卡（目标 / 来源）+
 *    一条操作条 + 进度与结果，确认详情放弹窗里。两个方向是同一件事的两面，用户来回切的时候
 *    不该重新学一遍界面。改动其中一页时请顺手看一眼另一页。
 */
export default function WorkbuddyImportPage() {
  const [list, setList] = useState<TraeWorkbuddyList | null>(null);
  const [clients, setClients] = useState<TraeInstalledClient[]>([]);
  /** 排在最前且**确有使用历史**的客户端（全 0 分时是 null）——只用来打「常用」徽标。 */
  const [usageTopPick, setUsageTopPick] = useState<string | null>(null);
  const [candidates, setCandidates] = useState<TraeImportCandidate[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [dstClient, setDstClient] = useState<string | null>(null);
  const [dstKey, setDstKey] = useState<string | null>(null);
  const [inspect, setInspect] = useState<TraeImportInspect | null>(null);

  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [showDeleted, setShowDeleted] = useState(false);
  const [filter, setFilter] = useState("");
  /**
   * 账号维度（用户要求：与会话管理一致，支持按账号显示）。
   *
   * `""` = 全部账号一起看（跨账号挑选、一次导入），否则只看这一个 uid。
   * 账号下拉的数量与列表同源（都从 `list.sessions` 数出来），不另算一份。
   */
  const [acctView, setAcctView] = useState("");
  /** 工作空间筛选：`""` = 全部。键是后端 `workspace_key`（归一化路径）。 */
  const [wsView, setWsView] = useState("");
  /**
   * 分组视图：按账号 / 按空间 / 平铺。
   *
   * 默认「按账号」—— 这个页面的核心动作是「把哪个号里的哪段对话搬去 Trae」，
   * 账号是第一维度；同一段对话在不同账号下的副本也正好按账号并排看见（谁最新一目了然）。
   */
  const [groupBy, setGroupBy] = useState<"account" | "workspace" | "flat">("account");

  const [busy, setBusy] = useState(false);
  const [busyLabel, setBusyLabel] = useState("");
  const [progress, setProgress] = useState<string[]>([]);
  const [report, setReport] = useState<TraeWorkbuddyReport | null>(null);

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [preview, setPreview] = useState<TraeWorkbuddyPreviewItem[] | null>(null);
  const [previewBusy, setPreviewBusy] = useState(false);
  // T46 · S5：迁移前先改一改。这里列的就是 WorkBuddy 会话，所以直接开 **WorkBuddy 侧**编辑器；
  // 但页脚给的是**跨客户端**写入（目标是 Trae 账号）—— 这个页面要的就是把会话搬到 Trae 去，
  // 摆一份 WorkBuddy 账号清单在这儿等于把用户绕回去（用户报的「怎么还是 workbuddy 到 workbuddy」）。
  const [editTarget, setEditTarget] = useState<{ uid: string; id: string } | null>(null);

  useEffect(() => {
    // `listen()` 是异步的：若在它 resolve 之前组件就卸载了（React StrictMode 在开发模式下
    // 会「挂载 → 卸载 → 再挂载」跑两遍 effect），清理函数拿到的 `un` 仍是 undefined，
    // 第一个监听器就永远注销不掉 —— 于是每条进度事件都被投递两次，日志成对重复。
    // 这里用一个 `active` 标记兜住：已经卸载就立刻注销刚拿到的监听器。
    let active = true;
    let un: (() => void) | undefined;
    void listen<{ line: string }>("trae-workbuddy-progress", (e) => {
      setProgress((prev) => [...prev, e.payload.line]);
    }).then((u) => {
      if (active) un = u;
      else u();
    });
    return () => {
      active = false;
      un?.();
    };
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [src, cand, cl] = await Promise.all([
        api.traeWorkbuddyList(),
        api.traeImportCandidates(""),
        api.traeListClients(),
      ]);
      setList(src);
      setCandidates(cand.candidates);
      setClients(cl.clients);
      setUsageTopPick(cl.usage?.topPick ?? null);
      // ⚠️ 列表刷新后必须摘掉已经不在列表里的勾选（T36）：会话可能被 WorkBuddy 删掉、
      //    正文文件可能被清掉，留着 id 会让「已选 N 个」虚高、确认弹窗里少几行。
      const alive = new Set(src.sessions.map((s) => s.id));
      setSelected((prev) => new Set([...prev].filter((id) => alive.has(id))));
      // 默认目标 = 候选里排最前的那个客户端（后端按「使用记忆」排序，且只产出**已安装**客户端）。
      // 一个候选都没有时退回**可见且已安装**的第一个客户端 —— 好让「该客户端下没有可用账号」
      // 的提示能显出来。⚠️ 不能再用 `clients.find(c => c.installed)` 兜底：合并后的 SOLO
      // 也是 `installed`，但它已不进标签条，选中它会得到一个界面上根本不存在的目标。
      setDstClient(
        (prev) =>
          prev ??
          cand.candidates[0]?.client_key ??
          cl.clients.find((c) => c.installed && isSelectableClient(c))?.key ??
          null,
      );
    } catch (cause) {
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  /** 客户端切换条（顺序 /「常用」徽标 / 未安装置灰都只由后端与共用组件决定）。 */
  const clientOptions = useMemo<TraeClientOption[]>(
    () =>
      clients.map((c) => ({
        key: c.key,
        label: c.label,
        installed: c.installed,
        hasLogin: c.has_login,
        // 这两项是「已并入且本机跑不起来的不进标签条」判据的入参，漏了就会漏出来。
        mergedInto: c.merged_into,
        runnable: c.runnable,
        top: c.key === usageTopPick,
      })),
    [clients, usageTopPick],
  );

  /** uid → `昵称（uid …97eac1）`：多账号时尾部 6 位根本分不清谁是谁。 */
  const accountLabels = useMemo(() => {
    const map = new Map<string, string>();
    for (const a of list?.accounts ?? []) map.set(a.uid, a.label);
    return map;
  }, [list]);

  const ownerOf = useCallback(
    (uid: string) =>
      (uid && accountLabels.get(uid)) || (uid ? `uid …${uid.slice(-6)}` : "未知账号"),
    [accountLabels],
  );

  const dstCandidates = useMemo(
    () => candidates.filter((c) => c.client_key === dstClient),
    [candidates, dstClient],
  );

  const target = useMemo(
    () =>
      dstCandidates.find((c) => `${c.client_key}::${c.account_id}` === dstKey) ?? null,
    [dstCandidates, dstKey],
  );

  // 目标账号默认取该客户端下「有会话库」的第一个：库不存在时导入必然失败，
  // 默认落在它上面等于让用户点一次才发现。切客户端时同步换掉已经不属于该客户端的旧目标。
  useEffect(() => {
    const pick = dstCandidates.find((c) => c.db_exists) ?? dstCandidates[0] ?? null;
    const key = pick ? `${pick.client_key}::${pick.account_id}` : null;
    setDstKey((prev) =>
      prev && dstCandidates.some((c) => `${c.client_key}::${c.account_id}` === prev) ? prev : key,
    );
  }, [dstCandidates]);

  /**
   * T46 · S5：编辑器页脚的**跨客户端**写入目标 = **Trae** 的账号（不是 WorkBuddy 的）。
   *
   * ⚠️ 这里把手上的**全部** Trae 账号都交出去（含国内版与国际版），但**客户端与账号分开给**：
   *    编辑器会按 `clientKey` 分组，先选客户端再选该客户端下的账号。
   *    早先把两者拼成一个字符串（`Trae CN（国内版） · 张三`）塞进一个下拉，
   *    用户报「trae 不应该多选账号，trae 和 trae solo 应当分离」说的就是这个 —— 而且同一个
   *    uid 在两个客户端下各有一份，只按 uid 反查会命中错的那个客户端。
   */
  const crossWrite = useMemo<CrossWriteTarget>(
    () => ({
      toLabel: "Trae",
      accounts: candidates.map((c) => ({
        uid: c.uid,
        clientKey: c.client_key,
        clientLabel: c.client_label,
        label: c.label,
        noDb: !c.db_exists,
      })),
      accountsError: null,
      blocked:
        candidates.length === 0 ? "本机没有可导入的 Trae 账号：先用某个 Trae 账号登录一次" : null,
      defaultKey: target ? crossKeyOf({ clientKey: target.client_key, uid: target.uid }) : null,
      push: async (acc) => {
        if (!editTarget) throw new Error("没有选中要搬走的会话");
        const r = await api.traeWorkbuddyImport(acc.clientKey, acc.uid, [editTarget.id]);
        return `${r.turns} 个回合 / ${r.tool_steps} 个工具步骤 → 账号 ${r.target_label}`;
      },
    }),
    [candidates, target, editTarget],
  );

  // 目标就绪状态（只读探测）：运行中 / 有没有会话库 / 密钥在不在。
  // 「密钥不在且客户端也没跑」这一种情况导入必然失败，必须在点之前就说清楚。
  useEffect(() => {
    if (!target) {
      setInspect(null);
      return;
    }
    let active = true;
    setInspect(null);
    void api
      .traeImportInspect(target.client_key, target.account_id)
      .then((v) => {
        if (active) setInspect(v);
      })
      .catch(() => {
        if (active) setInspect(null);
      });
    return () => {
      active = false;
    };
  }, [target]);

  /**
   * 工作空间键 / 展示名：优先用后端算好的（与会话管理同源），旧后端缺字段时才按 `cwd` 兜底。
   *
   * ⚠️ 兜底只做「显示」级归一化（反斜杠→正斜杠、小写、折叠重复），判据仍在后端；
   *    这样旧数据也不会出现 `D:/x` 与 `d:\x` 两个分组。
   */
  const wsKeyOf = useCallback((s: TraeWorkbuddySession) => {
    if (s.workspace_key !== undefined) return s.workspace_key;
    return s.cwd.replace(/\\/g, "/").replace(/\/{2,}/g, "/").replace(/\/+$/, "").toLowerCase();
  }, []);
  const wsLabelOf = useCallback(
    (s: TraeWorkbuddySession, key: string) => {
      if (s.workspace) return s.workspace;
      if (!key) return "（未归属）";
      const segs = key.split("/").filter(Boolean);
      return segs[segs.length - 1] || key;
    },
    [],
  );

  /** 只看「已删除开关」的全量行：账号下拉永远列出全部账号（否则选了一个号就再也切不到别的号）。 */
  const baseSessions = useMemo(
    () => (list?.sessions ?? []).filter((s) => showDeleted || !s.deleted),
    [list, showDeleted],
  );

  /** 账号筛选 / 已删除开关之后的「当前范围」——空间下拉与分组都从这一份算。 */
  const scopeSessions = useMemo(
    () => baseSessions.filter((s) => !acctView || s.user_id === acctView),
    [baseSessions, acctView],
  );

  /** 账号分布（数量与列表同源）：最近活跃的排前面。 */
  const acctOptions = useMemo(() => {
    const map = new Map<
      string,
      { uid: string; label: string; count: number; importable: number; last: number }
    >();
    for (const s of baseSessions) {
      const cur =
        map.get(s.user_id) ??
        { uid: s.user_id, label: ownerOf(s.user_id), count: 0, importable: 0, last: 0 };
      cur.count += 1;
      if (s.has_body) cur.importable += 1;
      cur.last = Math.max(cur.last, s.updated_at || 0);
      map.set(s.user_id, cur);
    }
    return [...map.values()].sort(
      (a, b) => b.last - a.last || a.label.localeCompare(b.label),
    );
  }, [baseSessions, ownerOf]);

  /** 空间分布（当前范围内）：最近活跃的排前面；同名空间补一级目录区分。 */
  const wsOptions = useMemo(() => {
    const map = new Map<
      string,
      { key: string; label: string; path: string; count: number; last: number; uids: Set<string> }
    >();
    for (const s of scopeSessions) {
      const key = wsKeyOf(s);
      const cur = map.get(key);
      if (cur) {
        cur.count += 1;
        cur.last = Math.max(cur.last, s.updated_at || 0);
        cur.uids.add(s.user_id);
      } else {
        map.set(key, {
          key,
          label: wsLabelOf(s, key),
          path: (s.workspace_path ?? s.cwd).replace(/\\/g, "/"),
          count: 1,
          last: s.updated_at || 0,
          uids: new Set([s.user_id]),
        });
      }
    }
    const list2 = [...map.values()];
    // 同名空间（两个不同目录都叫 `bin`）在下拉里得能区分，否则是两行一模一样的字。
    const dup = new Map<string, number>();
    for (const w of list2) dup.set(w.label, (dup.get(w.label) ?? 0) + 1);
    return list2
      .map((w) => {
        if ((dup.get(w.label) ?? 0) <= 1) return w;
        const segs = (w.path || "").split("/").filter(Boolean);
        return { ...w, label: segs.slice(-2).join("/") || w.label };
      })
      .sort((a, b) => b.last - a.last || a.label.localeCompare(b.label));
  }, [scopeSessions, wsKeyOf, wsLabelOf]);

  /**
   * 搜索 + 账号 + 空间 + 已删除 之后的可见行（勾选与全选只作用于这些行）。
   *
   * 搜索走**元数据**（标题 / 工作目录 / 会话 id / 归属账号 / 空间名 / 模型）——
   * 正文在 `projects/*.jsonl` 里，逐条读会让输入框卡死，占位符已写明。
   */
  const shown = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return scopeSessions
      .filter((s) => {
        if (wsView && wsKeyOf(s) !== wsView) return false;
        if (!q) return true;
        return [
          s.title,
          s.cwd,
          s.id,
          accountLabels.get(s.user_id),
          s.workspace,
          s.model,
        ].some((v) => (v ?? "").toLowerCase().includes(q));
      })
      .slice()
      .sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0));
  }, [scopeSessions, wsView, filter, wsKeyOf, accountLabels]);

  /**
   * 列表要渲染的「块」：按账号（账号顺序 = `acctOptions`）/ 按空间（顺序 = `wsOptions`）/ 平铺。
   *
   * 分组只为**看**：勾选集合始终是全局的会话 id 集合，不随分组变化。
   */
  const groups = useMemo(() => {
    type Block = {
      key: string;
      label: string;
      sub: string;
      count: number;
      items: TraeWorkbuddySession[];
      /** 组头点击后要应用的筛选（切到「只看这组」）。 */
      filter: { kind: "account" | "workspace"; value: string } | null;
    };
    if (groupBy === "flat") {
      return [
        {
          key: "__flat__",
          label: "",
          sub: "",
          count: shown.length,
          items: shown,
          filter: null,
        },
      ] satisfies Block[];
    }
    const map = new Map<string, Block>();
    for (const s of shown) {
      if (groupBy === "account") {
        const uid = s.user_id;
        const cur = map.get(uid);
        if (cur) cur.items.push(s);
        else
          map.set(uid, {
            key: uid,
            label: ownerOf(uid),
            sub: "",
            count: 0,
            items: [s],
            filter: { kind: "account", value: uid },
          });
      } else {
        const key = wsKeyOf(s);
        const cur = map.get(key);
        if (cur) cur.items.push(s);
        else
          map.set(key, {
            key,
            label: wsLabelOf(s, key),
            sub: (s.workspace_path ?? s.cwd).replace(/\\/g, "/"),
            count: 0,
            items: [s],
            filter: { kind: "workspace", value: key },
          });
      }
    }
    const blocks = [...map.values()];
    for (const b of blocks) b.count = b.items.length;
    if (groupBy === "account") {
      const order = new Map(acctOptions.map((a, i) => [a.uid, i]));
      blocks.sort(
        (a, b) =>
          (order.get(a.key) ?? 999) - (order.get(b.key) ?? 999) || a.label.localeCompare(b.label),
      );
    } else {
      const order = new Map(wsOptions.map((w, i) => [w.key, i]));
      blocks.sort(
        (a, b) =>
          (order.get(a.key) ?? 999) - (order.get(b.key) ?? 999) || a.label.localeCompare(b.label),
      );
    }
    return blocks;
  }, [groupBy, shown, acctOptions, wsOptions, ownerOf, wsKeyOf, wsLabelOf]);

  const importable = useMemo(() => shown.filter((s) => s.has_body), [shown]);
  /** 全量可导入数（不受筛选影响）：卡头那句「共 N 个可导入」用它，避免与筛选结果混为一谈。 */
  const totalImportable = useMemo(
    () => (list?.sessions ?? []).filter((s) => s.has_body && (showDeleted || !s.deleted)).length,
    [list, showDeleted],
  );
  const deletedCount = useMemo(
    () => (list?.sessions ?? []).filter((s) => s.deleted).length,
    [list],
  );
  /**
   * 将被导入的会话 = **全部**已勾选且带正文的行，不受当前筛选影响。
   *
   * ⚠️ 早前这里写的是 `importable.filter(...)`（`importable` 已经是筛选后的可见行），
   *    于是「筛掉的行」既不会被导入、又永远进不了下面的 `hiddenSelected` ——
   *    操作条上那句「另有 N 个被藏起来，仍会一起导入」永远显示不出来，
   *    等于把用户勾过的东西静默吃掉（与 MEMORY 里「空与失败不许同形」同一类错误）。
   *    现在勾选集合是全局的：筛掉也照导入，数量由 `hiddenSelected` 明说。
   */
  const selectedSessions = useMemo(
    () => (list?.sessions ?? []).filter((s) => s.has_body && selected.has(s.id)),
    [list, selected],
  );

  /**
   * 已勾选、但当前**看不见**的会话（被筛选或「隐藏已删除」藏起来了）。
   *
   * ⚠️ 勾选与可见行不是一回事（T36）：全选只作用于可见行，但已勾中的行不会因为筛选就被取消，
   *    它们**照样会被导入**。所以数量必须在操作条上显式说出来，不能静默吃掉。
   */
  const hiddenSelected = useMemo(() => {
    const visible = new Set(shown.map((s) => s.id));
    return selectedSessions.filter((s) => !visible.has(s.id));
  }, [selectedSessions, shown]);

  function toggle(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleAll() {
    setSelected((prev) => {
      const allShown = importable.length > 0 && importable.every((s) => prev.has(s.id));
      const next = new Set(prev);
      for (const s of importable) {
        if (allShown) next.delete(s.id);
        else next.add(s.id);
      }
      return next;
    });
  }

  /**
   * 同标题重复副本只留最新的一份（其余账号的旧副本不勾）。
   * 多账号场景下同一段对话往往有几份，全导入会在目标库里留下重复会话。
   */
  function selectNewestOnly() {
    setSelected(new Set(importable.filter((s) => s.is_newest).map((s) => s.id)));
  }

  const dupGroupCount = useMemo(
    () => new Set(importable.filter((s) => s.dup_group).map((s) => s.dup_group)).size,
    [importable],
  );

  /** 会话 id → 标题：预览项只回 `ai_title`，取不到时用来源标题兜底。 */
  const titleOf = useMemo(() => {
    const map = new Map<string, string>();
    for (const s of list?.sessions ?? []) map.set(s.id, s.title);
    return map;
  }, [list]);

  const previewTotal = useMemo(
    () =>
      (preview ?? []).reduce(
        (acc, p) => ({
          turns: acc.turns + p.turns,
          steps: acc.steps + p.tool_steps,
        }),
        { turns: 0, steps: 0 },
      ),
    [preview],
  );

  async function openConfirm() {
    if (!target || selectedSessions.length === 0) return;
    setConfirmOpen(true);
    setPreview(null);
    setPreviewBusy(true);
    setReport(null);
    setProgress([]);
    try {
      const r = await api.traeWorkbuddyPreview(selectedSessions.map((s) => s.id));
      setPreview(r.preview);
    } catch (cause) {
      toast.error("预览失败", { description: api.asError(cause) });
    } finally {
      setPreviewBusy(false);
    }
  }

  async function run() {
    if (!target || selectedSessions.length === 0 || busy) return;
    setBusy(true);
    setBusyLabel("导入到 Trae");
    setProgress([]);
    try {
      const r = await api.traeWorkbuddyImport(
        target.client_key,
        target.uid,
        selectedSessions.map((s) => s.id),
      );
      setReport(r);
      setConfirmOpen(false);
      setSelected(new Set());
      toast.success(`已导入 ${r.written_sessions} 个会话`, {
        description: `${r.turns} 个回合 / ${r.tool_steps} 个工具步骤 → ${r.target_label}`,
      });
      await load();
    } catch (cause) {
      toast.error("导入失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
      setBusyLabel("");
    }
  }

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">WorkBuddy → Trae</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          读取本机 WorkBuddy 的明文会话（JSONL），转换成 Trae 的关系库结构后加密写入指定账号的本地库——
          包含提问、最终回答，以及思考与工具调用过程。
        </p>
      </div>

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>操作失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {!list?.available && !loading ? (
        <Alert>
          <Database className="size-4" />
          <AlertTitle>未检测到 WorkBuddy 数据</AlertTitle>
          <AlertDescription>
            需要本机存在 <code className="font-mono">~/.workbuddy/projects/</code> 与{" "}
            <code className="font-mono">workbuddy.db</code>（即 WorkBuddy 桌面版至少用过一次）。
          </AlertDescription>
        </Alert>
      ) : null}

      {/* 导入目标（Trae 侧） */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="flex items-center gap-2 text-base">
            <HardDriveDownload className="size-4" />
            导入目标：Trae 客户端
            {!dstClient ? (
              <Badge variant="secondary">未检测到</Badge>
            ) : inspect ? (
              inspect.running ? (
                <Badge className="bg-amber-500/15 text-amber-600">运行中（导入时会自动退出）</Badge>
              ) : inspect.db_exists ? (
                <Badge className="bg-emerald-500/15 text-emerald-600">已就绪</Badge>
              ) : (
                <Badge variant="secondary">无会话库（需先登录一次）</Badge>
              )
            ) : null}
          </CardTitle>
          <CardDescription>
            选好客户端与账号后，勾选的 WorkBuddy 会话会被转换成 Trae 的表结构，加密写入该账号的本地库。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="text-xs text-muted-foreground">写入哪个客户端</div>
          <TraeClientSwitcher
            clients={clientOptions}
            value={dstClient}
            onChange={setDstClient}
            disabled={busy}
            label="Trae 客户端（导入目标）"
          />

          <div className="text-xs text-muted-foreground">写入哪个账号</div>
          {dstCandidates.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              该客户端下没有可用账号（需先在本机登录过）。
            </p>
          ) : (
            <div className="flex flex-wrap gap-1.5">
              {dstCandidates.map((c) => {
                const key = `${c.client_key}::${c.account_id}`;
                return (
                  <button
                    key={key}
                    type="button"
                    disabled={busy}
                    onClick={() => setDstKey(key)}
                    className={cn(
                      "rounded-lg border px-2.5 py-1.5 text-left transition-colors",
                      dstKey === key
                        ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                        : "border-border hover:bg-foreground/[0.03]",
                      !c.db_exists && "opacity-60",
                    )}
                  >
                    {/* 主行给「谁」：后端已经拼成 `昵称（uid …尾号）`；副行给「能不能写」 */}
                    <span className="block">{c.label}</span>
                    <span className="block text-xs text-muted-foreground">
                      {c.db_exists ? "有会话库" : "无会话库（需先登录一次）"}
                      {c.is_current ? " · 当前登录" : ""}
                      {c.is_source ? " · 源账号" : ""}
                    </span>
                  </button>
                );
              })}
            </div>
          )}

          <p className="text-xs text-muted-foreground">
            导入会先退出目标客户端（写完自动拉起），目标库会先整份备份；原始 WorkBuddy 记录只读、不会改动。
          </p>

          {inspect && !inspect.key_ready ? (
            <div className="flex flex-wrap items-center gap-2 rounded-lg bg-amber-500/10 px-3 py-2 text-xs leading-5 text-amber-700">
              <KeyRound className="size-3.5 shrink-0" />
              <span className="min-w-0 flex-1">
                {inspect.key_source === "scan_available"
                  ? "该账号没有存盘密钥，导入时会从正在运行的客户端进程内存里扫描——所以开始前别手动关掉客户端。"
                  : "该账号没有存盘密钥，且客户端当前没在运行，导入会失败。请先打开客户端，或到「Trae 账号管理」扫描密钥并解密。"}
              </span>
            </div>
          ) : null}
        </CardContent>
      </Card>

      {/* 来源（WorkBuddy） */}
      <Card>
        <CardHeader className="pb-2">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <CardTitle className="text-base">选择要导入的 WorkBuddy 会话</CardTitle>
              <CardDescription className="flex flex-wrap items-center gap-x-2">
                <span>
                  {list
                    ? `共 ${totalImportable} 个可导入${
                        acctView || wsView || filter ? `，筛选出 ${shown.length} 条` : ""
                      }；已选 ${selectedSessions.length} 个`
                    : "正在读取…"}
                </span>
                <span className="break-all font-mono text-xs text-muted-foreground/80">
                  {list?.data_root ?? ""}
                </span>
              </CardDescription>
            </div>
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => void load()} disabled={busy || loading}>
                {loading ? <Loader2 className="size-4 animate-spin" /> : <RefreshCw className="size-4" />}
                刷新
              </Button>
              <Button
                size="sm"
                variant="outline"
                onClick={toggleAll}
                disabled={importable.length === 0 || busy}
              >
                {importable.length > 0 && importable.every((s) => selected.has(s.id))
                  ? "全部取消"
                  : "全选"}
              </Button>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3">
          {/* 筛选条一：账号 → 空间 → 搜索（与会话管理的口径一致：
              账号数量 / 空间数量 / 列表条数全部从同一份 `list.sessions` 数出来） */}
          <div className="flex flex-wrap items-center gap-2">
            <select
              className="h-8 min-w-[170px] max-w-[240px] rounded-md border border-input bg-background px-2 text-xs"
              value={acctView}
              onChange={(e) => {
                setAcctView(e.target.value);
                // 换账号 = 换一批空间，旧的空间键在这里没有意义。
                setWsView("");
              }}
              title="按账号筛选（跨账号浏览时选「全部账号」）"
            >
              <option value="">
                全部账号（{baseSessions.length}）
              </option>
              {acctOptions.map((a) => (
                <option key={a.uid} value={a.uid}>
                  {a.label} · {a.count} 条（可导入 {a.importable}）
                </option>
              ))}
            </select>

            <select
              className="h-8 min-w-[160px] max-w-[220px] rounded-md border border-input bg-background px-2 text-xs"
              value={wsView}
              onChange={(e) => setWsView(e.target.value)}
              title="按工作空间筛选"
            >
              <option value="">全部工作空间（{wsOptions.length}）</option>
              {wsOptions.map((w) => (
                <option key={w.key} value={w.key}>
                  {w.label} · {w.count}
                </option>
              ))}
            </select>

            <div className="relative min-w-48 flex-1">
              <Search className="pointer-events-none absolute top-1/2 left-2 size-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="搜索标题 / 工作空间 / 账号 / 会话 id"
                className="h-8 pl-7 pr-7 text-xs"
              />
              {filter ? (
                <button
                  type="button"
                  title="清除搜索"
                  className="absolute top-1/2 right-2 -translate-y-1/2 text-muted-foreground"
                  onClick={() => setFilter("")}
                >
                  <X className="size-3.5" />
                </button>
              ) : null}
            </div>

            <label className="flex cursor-pointer items-center gap-1.5 text-xs text-muted-foreground">
              <Checkbox checked={showDeleted} onCheckedChange={(v) => setShowDeleted(Boolean(v))} />
              显示已删除（{deletedCount}）
            </label>
            {dupGroupCount > 0 ? (
              <Button
                size="sm"
                variant="outline"
                disabled={importable.length === 0 || busy}
                onClick={selectNewestOnly}
                title="同标题的多份副本里，只勾选内容最新的那一份"
              >
                每组只留最新（{dupGroupCount} 组）
              </Button>
            ) : null}
          </div>

          {/* 筛选条二：分组视图 + 同源计数 */}
          <div className="flex flex-wrap items-center gap-2">
            <div className="flex items-center gap-0.5 rounded-md border p-0.5">
              {(
                [
                  { key: "account", label: "按账号", icon: Users },
                  { key: "workspace", label: "按空间", icon: Layers },
                  { key: "flat", label: "平铺", icon: LayoutList },
                ] as const
              ).map((v) => (
                <button
                  key={v.key}
                  type="button"
                  className={cn(
                    "flex items-center gap-1 rounded px-2 py-0.5 text-xs",
                    groupBy === v.key
                      ? "bg-muted font-medium text-foreground"
                      : "text-muted-foreground hover:bg-muted/60",
                  )}
                  onClick={() => setGroupBy(v.key)}
                >
                  <v.icon className="size-3" />
                  {v.label}
                </button>
              ))}
            </div>
            {(acctView || wsView || filter) && (
              <Button
                size="sm"
                variant="ghost"
                className="h-7 text-xs"
                onClick={() => {
                  setAcctView("");
                  setWsView("");
                  setFilter("");
                }}
              >
                清除筛选
              </Button>
            )}
            <Badge variant="secondary" className="ml-auto">
              {shown.length === scopeSessions.length
                ? `${shown.length} 条`
                : `${shown.length} / ${scopeSessions.length} 条`}
            </Badge>
            {acctOptions.length > 1 ? (
              <Badge variant="outline">{acctOptions.length} 个账号</Badge>
            ) : null}
            {wsOptions.length > 0 ? (
              <Badge variant="outline">{wsOptions.length} 个空间</Badge>
            ) : null}
          </div>

          {loading && !list ? (
            <Skeleton className="h-40 rounded-xl" />
          ) : shown.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {list?.sessions.length
                ? `没有匹配的会话 —— 筛选后 0 条，本机共 ${list.sessions.length} 条。`
                : "本机暂无可导入的 WorkBuddy 会话。"}
            </p>
          ) : (
            <div className="max-h-[min(62vh,34rem)] overflow-auto rounded-lg border">
              <table className="w-full text-sm">
                <thead className="sticky top-0 z-10 border-b bg-muted/60 text-xs text-muted-foreground backdrop-blur">
                  <tr>
                    <th className="w-10 px-3 py-2">
                      <input
                        type="checkbox"
                        className="size-4 align-middle"
                        title="全选当前可见的可导入会话"
                        checked={
                          importable.length > 0 && importable.every((s) => selected.has(s.id))
                        }
                        onChange={toggleAll}
                      />
                    </th>
                    <th className="px-3 py-2 text-left font-medium">会话</th>
                    <th className="px-3 py-2 text-left font-medium">工作空间</th>
                    <th className="px-3 py-2 text-left font-medium">账号</th>
                    <th className="w-12 px-3 py-2" />
                  </tr>
                </thead>
                <tbody>
                  {groups.map((g) => (
                    <Fragment key={g.key}>
                      {groupBy !== "flat" ? (
                        <tr className="bg-muted/50">
                          <td colSpan={5} className="px-3 py-1.5">
                            <button
                              type="button"
                              className="flex w-full items-center gap-2 text-left"
                              title="只看这一组"
                              onClick={() => {
                                if (!g.filter) return;
                                if (g.filter.kind === "account") {
                                  // 再点一次 = 取消（不然「只看这个账号」进去就出不来）
                                  setAcctView((prev) =>
                                    prev === g.filter!.value ? "" : g.filter!.value,
                                  );
                                  setWsView("");
                                } else {
                                  setWsView((prev) =>
                                    prev === g.filter!.value ? "" : g.filter!.value,
                                  );
                                }
                              }}
                            >
                              {groupBy === "account" ? (
                                <Users className="size-3.5 shrink-0 text-muted-foreground" />
                              ) : (
                                <FolderOpen className="size-3.5 shrink-0 text-muted-foreground" />
                              )}
                              <span className="shrink-0 text-xs font-semibold">{g.label}</span>
                              <Badge variant="secondary" className="shrink-0">
                                {g.count}
                              </Badge>
                              {g.sub ? (
                                <span
                                  className="truncate text-xs text-muted-foreground"
                                  title={g.sub}
                                >
                                  {g.sub}
                                </span>
                              ) : null}
                            </button>
                          </td>
                        </tr>
                      ) : null}
                      {g.items.map((s) => (
                        <SessionRow
                          key={s.id}
                          session={s}
                          owner={ownerOf(s.user_id)}
                          workspace={wsLabelOf(s, wsKeyOf(s))}
                          workspacePath={(s.workspace_path ?? s.cwd).replace(/\\/g, "/")}
                          checked={selected.has(s.id)}
                          disabled={!s.has_body || busy}
                          onToggle={() => toggle(s.id)}
                          onEdit={() => setEditTarget({ uid: s.user_id, id: s.id })}
                        />
                      ))}
                    </Fragment>
                  ))}
                </tbody>
              </table>
            </div>
          )}

          <p className="text-xs text-muted-foreground">
            归属账号显示为「用户名/手机号（uid …尾号）」；同一段对话在不同账号下会有多份，带
            <span className="font-medium text-foreground">「最新」</span>
            标记的那份包含的内容最全（按正文逐行摘要比对，不只看时间）。
            「无正文」的会话无法导入（WorkBuddy 里只留了元数据）。
          </p>
        </CardContent>
      </Card>

      {/* 操作条 */}
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border bg-muted/40 px-4 py-3">
        <div className="text-sm text-muted-foreground">
          已选 <span className="font-medium text-foreground">{selectedSessions.length}</span> 个会话
          {busy ? (
            <span className="ml-3 inline-flex items-center gap-1.5 text-xs">
              <Loader2 className="size-3.5 animate-spin" />
              {busyLabel || "处理中…"}
            </span>
          ) : null}
          {hiddenSelected.length ? (
            <span className="mt-1 block text-xs text-amber-600">
              其中 {hiddenSelected.length} 个被当前筛选或「隐藏已删除」藏起来了，仍会一起导入。
            </span>
          ) : null}
        </div>
        <Button
          disabled={busy || !list?.available || !target || selectedSessions.length === 0}
          onClick={() => void openConfirm()}
        >
          <HardDriveDownload className="size-4" />
          导入到 Trae
        </Button>
      </div>

      {progress.length ? (
        <pre className="max-h-60 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
          {progress.join("\n")}
        </pre>
      ) : null}

      {report ? (
        <Alert>
          <CheckCircle2 className="size-4" />
          <AlertTitle>导入完成</AlertTitle>
          <AlertDescription className="space-y-1">
            <span className="block">
              {report.written_sessions} 个会话 / {report.turns} 个回合 / {report.tool_steps} 个工具步骤 →
              {" "}
              {report.target_label}
            </span>
            {report.changed_pages != null ? (
              <span className="block">
                加密回写：全库 {report.pages} 页，只重写了{" "}
                <span className="font-medium text-foreground">{report.changed_pages}</span> 页（
                {report.changed_mb ?? 0} MB，其中新增 {report.appended_pages ?? 0} 页），其余{" "}
                {report.pages - report.changed_pages} 页直接沿用原密文
                {report.write_ms != null ? ` · ${report.write_ms} ms` : ""}
              </span>
            ) : null}
            <span className="block break-all font-mono text-xs">备份：{report.backup_dir}</span>
            <span className="block text-xs">
              库内现有会话：{report.verified_sessions}
              {report.relaunched ? " · 已自动重启客户端" : " · 请手动启动客户端查看"}
            </span>
          </AlertDescription>
        </Alert>
      ) : null}

      {/* 确认 + 进度 */}
      <Dialog open={confirmOpen} onOpenChange={(v) => !busy && setConfirmOpen(v)}>
        <DialogContent className="max-h-[86vh] max-w-2xl grid-rows-[auto_minmax(0,1fr)_auto] overflow-hidden">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <HardDriveDownload className="size-4" />
              WorkBuddy → Trae
            </DialogTitle>
            <DialogDescription className="break-words">
              导入期间会先退出目标 Trae 客户端，写入完成后自动重启。原始 WorkBuddy 记录只读、不会改动；
              目标库会先整份备份。
            </DialogDescription>
          </DialogHeader>

          <div className="min-h-0 space-y-3 overflow-y-auto pr-1 text-sm">
            <div className="rounded-lg bg-muted/50 p-3">
              <div className="text-xs text-muted-foreground">写入目标</div>
              <div className="mt-0.5">
                {target?.client_label ?? "—"}
                <span className="text-muted-foreground"> · {target?.label ?? "未选择账号"}</span>
              </div>
            </div>

            {previewBusy ? (
              <div className="flex items-center gap-2 text-muted-foreground">
                <Loader2 className="size-4 animate-spin" />
                正在预览转换结果…
              </div>
            ) : preview ? (
              <>
                <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
                  {[
                    ["会话", String(preview.length)],
                    ["回合", String(previewTotal.turns)],
                    ["工具步骤", String(previewTotal.steps)],
                    [
                      "正文体积",
                      humanSize(
                        selectedSessions.reduce((acc, s) => acc + (s.body_bytes || 0), 0),
                      ),
                    ],
                  ].map(([k, v]) => (
                    <div key={k} className="rounded-lg bg-muted/50 p-2.5">
                      <div className="text-xs text-muted-foreground">{k}</div>
                      <div className="font-medium tabular-nums">{v}</div>
                    </div>
                  ))}
                </div>
                <div className="max-h-56 space-y-1 overflow-auto rounded-lg border p-2">
                  {preview.map((p) => (
                    <div key={p.session_id} className="rounded-md px-2 py-1.5">
                      <div className="truncate font-medium">
                        {p.ai_title || titleOf.get(p.session_id) || p.session_id}
                      </div>
                      <div className="truncate text-xs text-muted-foreground">
                        {p.turns} 回合 / {p.tool_steps} 工具步骤
                      </div>
                      <div className="truncate font-mono text-[11px] text-muted-foreground/80">
                        {p.session_id}
                      </div>
                    </div>
                  ))}
                </div>
              </>
            ) : null}

            {progress.length ? (
              <pre className="max-h-40 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground">
                {progress.join("\n")}
              </pre>
            ) : null}

            {hiddenSelected.length ? (
              <div className="rounded-lg bg-amber-500/10 p-2.5 text-xs leading-5 text-amber-700">
                另有 {hiddenSelected.length} 个已选会话被列表的筛选条件藏着，本次同样会写入。
              </div>
            ) : null}

            {inspect?.running ? (
              <Alert>
                <AlertTriangle className="size-4" />
                <AlertDescription className="leading-5">
                  {inspect.client_label} 正在运行，开始导入后它会被关闭（写完自动重新打开）。请先保存好正在编辑的内容。
                </AlertDescription>
              </Alert>
            ) : null}
          </div>

          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={() => setConfirmOpen(false)}>
              取消
            </Button>
            <Button disabled={busy || !preview} onClick={() => void run()}>
              {busy ? <Loader2 className="size-4 animate-spin" /> : <ArrowRight className="size-4" />}
              {busy ? "导入中…" : "确认导入"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* T46 · S5：WorkBuddy → Trae 迁移前，就地改这条 WorkBuddy 会话；页脚直接写入到 **Trae** 账号。 */}
      <WorkbuddySessionEditor
        open={editTarget !== null}
        onOpenChange={(o) => {
          if (!o) {
            setEditTarget(null);
            void load();
          }
        }}
        uid={editTarget?.uid ?? null}
        sessionId={editTarget?.id ?? null}
        cross={crossWrite}
        onDeleted={() => {
          // 会话已被整条删除：清掉编辑种子，列表重读（onOpenChange(false) 那条路已经 load 过）。
          setEditTarget(null);
        }}
      />
    </div>
  );
}

function SessionRow({
  session,
  owner,
  workspace,
  workspacePath,
  checked,
  disabled,
  onToggle,
  onEdit,
}: {
  session: TraeWorkbuddySession;
  /** 归属账号的显示名（`昵称（uid …97eac1）`）。 */
  owner: string;
  /** 归属工作空间的展示名（后端 `workspace_of()` 算出的，与会话管理同源）。 */
  workspace: string;
  /** 归一化后的路径（正斜杠），只用于 `title` 提示。 */
  workspacePath: string;
  checked: boolean;
  disabled: boolean;
  onToggle: () => void;
  /** 打开会话编辑器（迁移前先改一改）。 */
  onEdit: () => void;
}) {
  const dup = Boolean(session.dup_group);
  // 重复组里的旧副本整体压暗一档，让「最新」那份一眼看见
  const dimmed = disabled || (dup && !session.is_newest);
  return (
    <tr
      className={cn(
        "border-b transition-colors last:border-0",
        dimmed ? "opacity-60" : "hover:bg-muted/30",
      )}
    >
      <td className="px-3 py-2 align-top">
        <input
          type="checkbox"
          className="size-4"
          checked={checked}
          disabled={disabled}
          onChange={() => onToggle()}
          aria-label={`选择 ${session.title}`}
        />
      </td>
      <td className="max-w-[420px] px-3 py-2 align-top">
        <div className="flex flex-wrap items-center gap-1.5">
          <span className="truncate font-medium">{session.title}</span>
          {dup && session.is_newest ? (
            <Badge className="bg-emerald-500/15 text-[10px] text-emerald-600">最新</Badge>
          ) : null}
          {dup && !session.is_newest ? (
            <Badge variant="secondary" className="text-[10px]">
              较旧
            </Badge>
          ) : null}
          {session.deleted ? (
            <Badge variant="secondary" className="text-[10px]">
              已删除
            </Badge>
          ) : null}
          {!session.has_body ? (
            <Badge variant="outline" className="text-[10px]">
              无正文
            </Badge>
          ) : null}
        </div>
        <div className="mt-0.5 truncate text-xs text-muted-foreground">
          {/* 相对时间在前：重复副本的绝对时间只差一两分钟，看不出谁靠后 */}
          {relativeTime(session.updated_at)}（{formatTime(session.updated_at)}） ·{" "}
          {humanSize(session.body_bytes)}
          {session.body_lines > 0 ? ` · ${session.body_lines} 条记录` : ""}
          {session.model ? ` · ${session.model}` : ""}
        </div>
        {session.dup_note ? (
          <div
            className={cn(
              "mt-0.5 truncate text-[11px]",
              dup && !session.is_newest ? "text-amber-600" : "text-muted-foreground/80",
            )}
          >
            {session.dup_group}
            {dup ? "｜" : ""}
            {session.dup_note}
          </div>
        ) : null}
      </td>
      <td className="max-w-[180px] px-3 py-2 align-top">
        <div className="truncate text-xs text-muted-foreground" title={workspacePath}>
          {workspace}
        </div>
      </td>
      <td className="max-w-[180px] px-3 py-2 align-top">
        <div className="truncate text-xs text-muted-foreground" title={owner}>
          {owner}
        </div>
      </td>
      <td className="px-3 py-2 align-top">
        <button
          type="button"
          title="编辑这条会话（导入前可先改）"
          aria-label="编辑这条会话"
          className="text-muted-foreground hover:text-foreground shrink-0 rounded-sm p-1 disabled:pointer-events-none disabled:opacity-40"
          disabled={!session.has_body || disabled}
          onClick={onEdit}
        >
          <Pencil className="size-3.5" />
        </button>
      </td>
    </tr>
  );
}
