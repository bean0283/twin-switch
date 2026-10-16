import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import {
  AlertTriangle,
  ArchiveRestore,
  CheckCircle2,
  ExternalLink,
  FileDown,
  FileUp,
  GitCompare,
  Layers,
  Loader2,
  LogIn,
  PencilLine,
  Power,
  RotateCcw,
  Save,
  Trash2,
  UserPlus,
  X,
} from "lucide-react";

import { AccountCard, Chip, IconAction } from "@/components/account-card";
import { CheckinButton } from "@/components/checkin-button";
import {
  TraeCreditBlock,
  TraeCreditsDialog,
  TraeCreditsHeader,
  useTraeCredits,
} from "@/components/trae-credits";
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
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { TraeClientSwitcher, type TraeClientOption } from "@/components/trae-client-switcher";
import { AccountTokenDialog } from "@/components/account-token-dialog";
import * as api from "@/lib/api";
import { resolveSelectableKey, selectableClients } from "@/lib/trae-client-list";
import type {
  TraeAccountOverview,
  TraeAdoptMergedResult,
  TraeBrowserInfo,
  TraeCreditEntry,
  TraeDivergedGroup,
  TraeDivergedLinks,
  TraeInstalledClient,
  TraeOAuthSessionStatus,
  TraeOAuthStartResult,
  TraeSwitchResult,
  TraeSyncDirection,
  TraeVaultEntry,
} from "@/lib/trae-types";

function formatBytes(bytes: number): string {
  if (!bytes) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function outcomeBadge(outcome: TraeSwitchResult["outcome"]): { label: string; tone: string } {
  switch (outcome) {
    case "active":
      return { label: "切换成功", tone: "bg-emerald-500/15 text-emerald-600" };
    case "rolled_back":
      return { label: "已回滚", tone: "bg-amber-500/15 text-amber-600" };
    default:
      return { label: "需人工确认", tone: "bg-orange-500/15 text-orange-600" };
  }
}

function oauthStateLabel(state: string): string {
  switch (state) {
    case "waiting":
      return "等待浏览器完成授权…";
    case "callback_received":
      return "已收到授权回调，正在完成登录…";
    case "processing":
      return "正在完成登录…";
    case "done":
      return "登录完成";
    case "error":
      return "登录失败";
    case "stopped":
      return "已停止监听";
    default:
      return "等待浏览器完成授权…";
  }
}

/** uid 只显示首尾，中间省略（完整值挂在 title 上）。 */
function shortUid(uid: string | null | undefined): string {
  if (!uid) return "—";
  if (uid.length <= 18) return uid;
  return `${uid.slice(0, 8)}…${uid.slice(-6)}`;
}

/** 令牌到期的人话描述；只在 7 天内到期时提示，减少噪声。 */
function tokenExpiryLabel(ms: number | null | undefined): string | null {
  if (!ms) return null;
  const days = Math.floor((ms - Date.now()) / 86400_000);
  if (days < 0) return "令牌已过期";
  if (days <= 7) return `${days} 天后过期`;
  return null;
}

/**
 * 弹窗里的一行：一个**需要处理**的关联组（两端分叉，或副本数据写坏了）。
 *
 * 方向一律翻译成账号名，**不再出现「当前账号」**：
 * 这个弹窗从 T35 起是在**切换之前**弹的，那时「当前账号」还是旧的那个 ——
 * 写成「当前账号」会把方向说反，而这个动作是要**覆盖数据**的。
 * 两端名字都来自后端（`selfLabel` / `partnerLabel`），与「我现在登的是谁」无关。
 *
 * ⚠️ 导出仅为**本地渲染自查 / 预览**（`preview/`，不进生产入口）：本块的「自检警示」
 *    是多行文本 + 按钮组，属于最容易在小宽度下压坏的那类布局，必须能单独渲染核对。
 */
export function DivergedRow({
  group,
  checked,
  direction,
  disabled,
  onChecked,
  onDirection,
}: {
  group: TraeDivergedGroup;
  checked: boolean;
  /** `null` = 还没选方向（后端推不出谁新时不给建议，用户必须自己点）。 */
  direction: TraeSyncDirection | null;
  disabled: boolean;
  onChecked: (v: boolean) => void;
  onDirection: (v: TraeSyncDirection) => void;
}) {
  const title = (group.title as string) || "(无标题)";
  const selfIsSource = group.selfRole === "source";
  const syncInDir: TraeSyncDirection = selfIsSource ? "targetToSource" : "sourceToTarget";
  const syncOutDir: TraeSyncDirection = selfIsSource ? "sourceToTarget" : "targetToSource";
  // `selfLabel` 是「被探测的那一端」的账号名（切换前 = 要切过去的那个账号）。
  const self = group.selfLabel || "本端账号";
  const partner = group.partnerLabel || "对端账号";
  // 自检问题：**哪一份坏了必须写清楚** —— 用户据此决定往哪边同步（方向不能猜）。
  const selfIssues = group.selfIssues ?? [];
  const partnerIssues = group.partnerIssues ?? [];
  const broken = selfIssues.length > 0 || partnerIssues.length > 0;

  return (
    <div className="flex items-start gap-3 rounded-md border p-3">
      <input
        type="checkbox"
        className="mt-1 size-4 shrink-0 accent-primary"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChecked(e.target.checked)}
      />
      <div className="min-w-0 flex-1">
        <div className="truncate font-medium" title={title}>
          {title}
        </div>
        <div className="mt-0.5 text-xs text-muted-foreground">
          {self} {group.selfMessages ?? "?"} 条 · {partner} {group.partnerMessages ?? "?"} 条
        </div>
        {broken && (
          <div className="mt-1.5 space-y-0.5 rounded border border-destructive/30 bg-destructive/5 px-2 py-1 text-xs text-destructive">
            <div className="font-medium">⚠️ 这条副本的数据引用已错位，客户端里会显示不全</div>
            {selfIssues.length > 0 && <div>· {self}：{selfIssues.join("；")}</div>}
            {partnerIssues.length > 0 && (
              <div>
                · {partner}：{partnerIssues.join("；")}
              </div>
            )}
            <div className="text-destructive/90">
              {selfIssues.length > 0 && partnerIssues.length === 0
                ? `点「同步到「${self}」」，用「${partner}」的内容重建这一份。`
                : partnerIssues.length > 0 && selfIssues.length === 0
                  ? `点「同步到「${partner}」」，用「${self}」的内容重建对面那份。`
                  : "两份都有问题，请人工判断该保留哪一份。"}
            </div>
          </div>
        )}
        <div className="mt-2 flex flex-wrap gap-1.5">
          <Button
            size="sm"
            variant={direction === syncInDir ? "default" : "outline"}
            disabled={disabled || !checked}
            onClick={() => onDirection(syncInDir)}
          >
            同步到「{self}」
            {/* 标「推荐」而非「较新」：推荐的是**保留较新一端**这个方向，
                写成「较新」容易被读成「对面那个更新」，正好选反。
                副本写坏时后端不给建议（`null`），两个按钮都不标。 */}
            {group.suggestedDirection === syncInDir ? "（推荐）" : ""}
          </Button>
          <Button
            size="sm"
            variant={direction === syncOutDir ? "default" : "outline"}
            disabled={disabled || !checked}
            onClick={() => onDirection(syncOutDir)}
          >
            同步到「{partner}」
            {group.suggestedDirection === syncOutDir ? "（推荐）" : ""}
          </Button>
        </div>
      </div>
    </div>
  );
}

/**
 * 一次「用户已经点了、但**还没有执行**」的切换（T35）。
 *
 * 以前是**切完了**才探测并弹窗：用户看到提示时账号已经换过去了，想反悔都来不及。
 * 现在探测与确认都挪到切换**之前**，这个对象就是「弹窗背后待执行的那次动作」。
 */
interface PendingSwitch {
  entry: TraeVaultEntry;
  kind: "switch" | "rollback";
  /** 目标账号的 uid —— 探测分叉必须有一个「自己」，拿不到就无从探测。 */
  targetUid: string;
}

/**
 * 从账号库条目里取目标账号的 uid。
 *
 * 载体（`carrier`）看 `meta.verified_uid`；网页凭证（`oauth`）看 `oauth.uid`。
 * 两个都兜一遍：备份时可能还没验出 uid，而 oauth 条目也带 verified_uid。
 */
function targetUidOf(entry: TraeVaultEntry): string | null {
  return entry.meta?.verified_uid ?? entry.oauth?.uid ?? null;
}

export default function TraeSwitchPage() {
  const [clients, setClients] = useState<TraeInstalledClient[]>([]);
  /** 排在最前且**确有使用历史**的客户端（全 0 分时是 null）——只用来打「常用」徽标。 */
  const [usageTopPick, setUsageTopPick] = useState<string | null>(null);
  const [clientKey, setClientKey] = useState<string | null>(null);
  const [overview, setOverview] = useState<TraeAccountOverview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<string[]>([]);

  /**
   * 「已并入的客户端还有账号没带过来」的预演结果（`null` = 没探到 / 不用提示）。
   *
   * 2026-10 官方把 TRAE SOLO 并进 Trae CN 时**只搬了程序与会话**（会话实测已原样在
   * Trae CN 库里）—— 本工具自己的账号库官方不会碰 ⇒ SOLO 下备份过的账号在 Trae CN 下
   * 看不见、也切不过去。这里只预演，用户点了才动账号库。
   */
  const [mergePending, setMergePending] = useState<TraeAdoptMergedResult | null>(null);
  const [mergeBusy, setMergeBusy] = useState(false);
  /** 正在看 token 的那个账号（点头像打开）。 */
  const [tokenTarget, setTokenTarget] = useState<TraeVaultEntry | null>(null);

  // 网页（OAuth）登录
  const [loginOpen, setLoginOpen] = useState(false);
  const [oauthStart, setOauthStart] = useState<TraeOAuthStartResult | null>(null);
  const [oauthStatus, setOauthStatus] = useState<TraeOAuthSessionStatus | null>(null);
  const [oauthBrowsers, setOauthBrowsers] = useState<TraeBrowserInfo[]>([]);
  const [oauthBrowserKey, setOauthBrowserKey] = useState("");
  const [manualOpen, setManualOpen] = useState(false);
  const [manualUrl, setManualUrl] = useState("");

  // 重命名 / 交接记忆 对话框
  //
  // ⚠️ 2026-10-14（T65 二改）：原来这里还有 `backupOpen/backupName`（备份当前登录态）与
  // `importOpen/importText/importName`（粘贴 JSON 导入账号备份）两组状态。按用户要求
  // 「严格按 workbuddy 的界面进行设计」，Trae 页头部只留 WB 那五个入口，这两个**弹窗型**
  // 入口被替换成 WB 那套**文件型**的「导出账号包 / 导入账号包」（见 `onExportPack` /
  // `onImportPack`）—— 连同弹窗、连同一整组 state 一起删，不留死代码。
  const [renameTarget, setRenameTarget] = useState<TraeVaultEntry | null>(null);
  const [renameName, setRenameName] = useState("");
  const [handoffOpen, setHandoffOpen] = useState(false);

  /** 底部「回滚」卡里选中的账号 id（回滚 = 切回它）。 */
  const [rollbackTarget, setRollbackTarget] = useState<string | null>(null);

  // **切换前**的「两端不一致」确认框（T35）：探测到分叉/写坏才弹，纯粹是提醒，
  // 失败不影响主流程 —— 提醒不成立就照常切，不打扰用户。
  const [diverged, setDiverged] = useState<TraeDivergedLinks | null>(null);
  /** 每组是否参与本次同步（默认全选）。 */
  const [divergedOn, setDivergedOn] = useState<Record<string, boolean>>({});
  /** 每组选的方向（默认取后端建议值）。 */
  const [divergedDir, setDivergedDir] = useState<Record<string, TraeSyncDirection>>({});
  /**
   * 弹窗背后**还没执行**的那次切换（T35 的关键：先问、再切）。
   *
   * `null` = 这次弹窗不是由切换触发的（纯提醒），此时按钮只做「同步」。
   * 有值 = 用户在账号卡上点了切换/回滚，等他答完再决定要不要真的切。
   */
  const [pendingSwitch, setPendingSwitch] = useState<PendingSwitch | null>(null);
  const [syncBusy, setSyncBusy] = useState(false);
  const [syncLog, setSyncLog] = useState<string[]>([]);

  // 交接记忆表单
  const [hoProject, setHoProject] = useState("");
  const [hoSessions, setHoSessions] = useState("");
  const [hoSteps, setHoSteps] = useState("");
  const [hoKeyFiles, setHoKeyFiles] = useState("");
  const [hoNote, setHoNote] = useState("");
  const [hoResult, setHoResult] = useState<{ files: string[]; skipped: string[] } | null>(null);

  const progressRef = useRef<HTMLPreElement>(null);
  useEffect(() => {
    if (progressRef.current) {
      progressRef.current.scrollTop = progressRef.current.scrollHeight;
    }
  }, [progress]);

  async function loadClients() {
    try {
      const { clients, usage } = await api.traeListClients();
      setClients(clients);
      setUsageTopPick(usage?.topPick ?? null);
      // ⚠️ 顺序**已经由后端排好**（使用记忆 + 能不能跑起来，降序）⇒ 取可见列表第一个。
      //    前端**不要**再自己写一套「trae-cn 优先」的规则：同一件事被两处独立推导，
      //    早晚会不一致（页面之间顺序不同就是这么来的）。
      //    ⚠️ 必须走 `resolveSelectableKey`：记忆里可能还留着已被摘掉的客户端（合并后的
      //    `solo-cn`），写 `prev ?? first` 会把它一直用下去，选中态与标签条对不上。
      setClientKey((prev) => resolveSelectableKey(prev, clients));
    } catch (cause) {
      setError(api.asError(cause));
    }
  }

  useEffect(() => {
    void loadClients();
  }, []);

  /**
   * ⚠️ 2026-10-14（T65 二改）：「重置排序」按钮**已按用户要求撤掉**
   * （原话：「其他的重置排序、导入全部客户端等不需要」），所以这里那份
   * `traeClientUsageReset` 的调用一并删除 —— 入口没了、函数还留着就是死代码。
   *
   * 后端能力（`trae_client_usage_reset`）仍在，需要时把入口加回客户端标签条右侧即可。
   * 内置默认里 `solo-cn` 仍排第一，但它已被摘掉（并入 Trae CN、本机没有它的程序）
   * ⇒ 界面上看到的第一项就是 `trae-cn`。别在任何地方写死 `solo-cn`，用户根本看不到它。
   */

  async function loadOverview(key: string) {
    setLoading(true);
    setError(null);
    try {
      const data = await api.traeAccountOverview(key);
      setOverview(data);
    } catch (cause) {
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (!clientKey) return;
    void loadOverview(clientKey);
    void loadMergePending(clientKey);
  }, [clientKey]);

  /**
   * 预演「已并入的客户端有哪些账号可以带过来 / 有哪些残档可以补全」。
   *
   * ⚠️ 只对**承载方**有意义：后端按 `merged_into == clientKey` 找源，所以切换条选中
   * `solo-cn` 时结果必然为空 —— 那正是对的（它自己就是被并进去的那个）。
   */
  async function loadMergePending(key: string) {
    try {
      const res = await api.traeVaultAdoptMerged(key, true);
      // ⚠️ 两个数组都在「后端新加的字段」上：`repaired` 是 T51 才有的。
      //    这里归一化，避免老桩 / 老后端少字段时 `undefined.length` 直接抛异常
      //    ⇒ 提示条静默消失，看起来像「没有待并入账号」，把真问题藏了。
      const adopted = res.adopted ?? [];
      const repaired = res.repaired ?? [];
      setMergePending(adopted.length > 0 || repaired.length > 0 ? { ...res, adopted, repaired } : null);
    } catch {
      // 预演失败不该打扰用户：这只是顺带提醒，不是本页主功能。
      setMergePending(null);
    }
  }

  /** 用户确认后真正搬入（后端按 uid 去重、不覆盖、不动源），顺带补全老版本漏搬的侧车文件。 */
  async function onAdoptMerged() {
    if (!clientKey || mergeBusy || !mergePending) return;
    setMergeBusy(true);
    try {
      const res = await api.traeVaultAdoptMerged(clientKey, false);
      const adopted = res.adopted ?? [];
      const repaired = res.repaired ?? [];
      const sources = [...new Set([...adopted, ...repaired].map((a) => a.from_label))].join(" / ");
      const parts: string[] = [];
      if (adopted.length) parts.push(`已并入 ${adopted.length} 个账号`);
      if (repaired.length) parts.push(`补全 ${repaired.length} 个账号的网页凭证`);
      toast.success(parts.join("，") || "没有需要处理的账号", { description: `来源：${sources}` });
      setMergePending(null);
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("并入账号失败", { description: api.asError(cause) });
    } finally {
      setMergeBusy(false);
    }
  }

  // 网页登录：发起时拉取本机浏览器列表（私密窗口用）
  useEffect(() => {
    if (!loginOpen) return;
    api
      .traeOauthBrowsers()
      .then(({ browsers }) => {
        setOauthBrowsers(browsers);
        setOauthBrowserKey((prev) => prev || browsers[0]?.key || "");
      })
      .catch(() => setOauthBrowsers([]));
  }, [loginOpen]);

  // 网页登录：1.5s 轮询状态；收到回调时后端自动完成 token 交换并落库
  useEffect(() => {
    if (!loginOpen || !clientKey) return;
    let cancelled = false;
    const t = setInterval(async () => {
      try {
        const s = await api.traeOauthStatus();
        if (cancelled) return;
        setOauthStatus(s);
        if (s.state === "done" || s.state === "error" || s.state === "stopped") {
          clearInterval(t);
          if (s.state === "done") {
            toast.success(s.message);
            void loadOverview(clientKey);
          }
        }
      } catch {
        // 单次轮询失败忽略，下一轮继续
      }
    }, 1500);
    return () => {
      cancelled = true;
      clearInterval(t);
    };
  }, [loginOpen, clientKey]);

  const oauthLoginUrl = oauthStart?.loginUrl ?? oauthStatus?.loginUrl ?? "";

  async function onOauthStart() {
    if (!clientKey || busy) return;
    setBusy(true);
    try {
      // 不再传「账号备注」：头部那个输入框已按 WB 的版式去掉，
      // 账号名导入后在卡片上用「重命名」随时能改（Trae 侧本来就有这个入口）。
      const res = await api.traeOauthStart(clientKey, undefined);
      setOauthStart(res);
      setOauthStatus(null);
      setLoginOpen(true);
      await api.traeOauthOpenUrl(res.loginUrl);
    } catch (cause) {
      toast.error("发起登录失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  async function onImportLocal() {
    if (!clientKey || busy) return;
    setBusy(true);
    try {
      const r = await api.traeImportLocalLogin(clientKey);
      if (r.ok) {
        toast.success(
          `已导入本地登录态${r.duplicate ? "（账号已存在，已更新）" : ""}`,
          { description: `${r.displayName || r.uid || r.id} · uid ${r.uid ?? "-"}` },
        );
        void loadOverview(clientKey);
      } else {
        toast.error("导入本地登录态失败", { description: r.error });
      }
    } catch (cause) {
      toast.error("导入本地登录态失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  /**
   * 导出**账号包**：本客户端下账号库里所有账号，逐个导出成自包含 JSON，
   * 再打包成一个文件让用户下载。
   *
   * ⚠️ 与 WorkBuddy 页的「导出账号包」**同一套交互**（下载一个 `.json` 文件），
   * 而不是原来那个「复制到剪贴板」—— 用户要求严格按 WB 的界面来。
   * ⚠️ 后端 `trae_vault::export_account` 是**逐账号**的，这里在前端聚合：
   * 一个个成功才收进来，**失败的账号要单独列出来**，绝不悄悄少几个
   * （「少导出了 3 个」和「导出了 3 个」在界面上长得一样，是最典型的坑）。
   */
  async function onExportPack() {
    if (!clientKey || busy) return;
    const entries = overview?.vault ?? [];
    if (!entries.length) {
      toast.error("账号库是空的", { description: "先在客户端登录一次，然后点「导入本机登录态」" });
      return;
    }
    setBusy(true);
    try {
      const accounts: Record<string, unknown>[] = [];
      const failed: string[] = [];
      for (const e of entries) {
        try {
          const r = await api.traeExportAccount(clientKey, e.id);
          if (r.payload) accounts.push(r.payload);
          else failed.push(e.id);
        } catch {
          failed.push(e.id);
        }
      }
      if (!accounts.length) {
        toast.error("导出失败", { description: `所有账号都没导出成功：${failed.join("、")}` });
        return;
      }
      const pack = {
        format: "trae-account-pack",
        version: 1,
        client: clientKey,
        exported_at: new Date().toISOString(),
        count: accounts.length,
        accounts,
      };
      const blob = new Blob([JSON.stringify(pack, null, 2)], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `trae-accounts-${clientKey}-${Date.now()}.json`;
      a.click();
      URL.revokeObjectURL(url);
      toast.success(`账号包已导出（${accounts.length} 个账号）`, {
        description: failed.length ? `${failed.length} 个失败未包含：${failed.join("、")}` : undefined,
      });
    } catch (cause) {
      toast.error("导出账号包失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  /**
   * 导入**账号包**：接受两种文件 ——
   * ① 本工具导出的账号包（`{ accounts: [...] }`，一次多个）；
   * ② 单个账号的自包含 JSON（旧版逐账号导出的那种），也能直接导入。
   *
   * ⚠️ 逐个导入、逐个报结果：**成功数和失败数分开说**，失败的要把账号名列出来。
   * 冲突时后端自动改 id（不改用户已有账号），所以「导入 N 个」与「新增 N 个」可能不等，
   * 这里如实按后端返回的 id 报。
   */
  async function onImportPack(file: File) {
    if (!clientKey || busy) return;
    setBusy(true);
    try {
      const raw: unknown = JSON.parse(await file.text());
      const list: unknown[] = Array.isArray((raw as { accounts?: unknown[] })?.accounts)
        ? ((raw as { accounts: unknown[] }).accounts as unknown[])
        : [raw];
      if (!list.length) {
        toast.error("账号包里没有账号", { description: "文件内容为空" });
        return;
      }
      const imported: string[] = [];
      const failed: string[] = [];
      for (const item of list) {
        try {
          const r = await api.traeImportAccount(clientKey, item as Record<string, unknown>);
          imported.push(r.id);
        } catch (cause) {
          const name =
            (item as { id?: string })?.id ?? (item as { name?: string })?.name ?? "未命名";
          failed.push(`${name}（${api.asError(cause)}）`);
        }
      }
      if (imported.length) {
        toast.success(`已导入 ${imported.length} 个账号`, {
          description: imported.join("、"),
        });
        void loadOverview(clientKey);
      }
      if (failed.length) {
        toast.error(`${failed.length} 个账号没导入成功`, { description: failed.join("；") });
      }
    } catch (cause) {
      toast.error("导入账号包失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  async function onOauthOpenUrl(privateMode: boolean) {
    if (!oauthLoginUrl) return;
    try {
      await api.traeOauthOpenUrl(oauthLoginUrl, privateMode, privateMode ? oauthBrowserKey || undefined : undefined);
    } catch (cause) {
      toast.error("打开浏览器失败", { description: api.asError(cause) });
    }
  }

  async function onOauthStop() {
    try {
      await api.traeOauthStop();
      setOauthStatus((prev) =>
        prev
          ? { ...prev, state: "stopped", message: "已停止监听" }
          : { state: "stopped", message: "已停止监听" },
      );
    } catch (cause) {
      toast.error("停止监听失败", { description: api.asError(cause) });
    }
  }

  async function onOauthManual() {
    if (!clientKey || !manualUrl.trim()) return;
    setBusy(true);
    try {
      const s = await api.traeOauthManual(clientKey, manualUrl.trim(), undefined);
      setOauthStatus(s);
      if (s.state === "done") {
        toast.success(s.message);
        void loadOverview(clientKey);
      } else if (s.state === "error") {
        toast.error(s.message);
      }
    } catch (cause) {
      toast.error("手动完成登录失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  function onOauthClose() {
    setLoginOpen(false);
    setOauthStart(null);
    setOauthStatus(null);
    setManualOpen(false);
    setManualUrl("");
    void api.traeOauthStop();
  }

  function pushProgress(lines: string[]) {
    setProgress((prev) => [...prev, ...lines]);
  }

  // 「同步差异」的进度行走 trae-import-progress；这里单独收一列，给弹窗自检用。
  useEffect(() => {
    // 同 TraeRecordsPage：`listen()` 是异步的，StrictMode 下首轮清理拿不到 un，
    // 会残留一个监听把每条进度事件投递两遍。
    let active = true;
    let un: (() => void) | undefined;
    void listen<{ line: string }>("trae-import-progress", (e) => {
      setSyncLog((prev) => [...prev, e.payload.line]);
    }).then((u) => {
      if (active) un = u;
      else u();
    });
    return () => {
      active = false;
      un?.();
    };
  }, []);

  const syncSelected = diverged
    ? diverged.groups.filter(
        (g) => divergedOn[g.groupId] && (divergedDir[g.groupId] ?? g.suggestedDirection),
      ).length
    : 0;

  /**
   * 探测**指定账号**有没有「与其它账号还留着关联、但两端已经不一致」的会话副本。
   * 返回 `null` = 没什么要提醒的（或探测失败）。
   *
   * ⚠️ 这个探测**只读本地数据**（关联登记 + 实时库里的会话），与「客户端当前登的是谁」
   *    完全无关 —— 所以可以在切换**之前**拿目标账号的 uid 去探。
   *    T35 之前是切完再探，用户只能在账号已经换过去之后被动接受，想放弃都来不及。
   * ⚠️ 探测失败**绝不挡主流程**：切换是用户明确要的动作，提醒只是锦上添花，
   *    把一个正常的切换改成失败反而误导。
   */
  async function probeTargetDivergence(uid: string): Promise<TraeDivergedLinks | null> {
    if (!clientKey) return null;
    try {
      const v = await api.traeSessionLinksDiverged(clientKey, uid);
      return v.ok && v.count > 0 ? v : null;
    } catch {
      return null;
    }
  }

  /** 把探测结果装进弹窗（默认全选、方向取后端建议），并记下背后待执行的那次切换。 */
  function openDivergencePrompt(v: TraeDivergedLinks, pending: PendingSwitch | null) {
    setDiverged(v);
    setDivergedOn(Object.fromEntries(v.groups.map((g) => [g.groupId, true])));
    // ⚠️ 后端推不出方向时给 `null`（副本写坏了 ⇒ 条数相等，猜谁新必错）。
    //    这种组**不预置方向**，用户不点就不下发 —— 缺省值必须能表达「没选」。
    setDivergedDir(
      Object.fromEntries(
        v.groups
          .filter((g) => g.suggestedDirection !== null)
          .map((g) => [g.groupId, g.suggestedDirection as TraeSyncDirection]),
      ),
    );
    setPendingSwitch(pending);
  }

  /** 关掉弹窗（跳过 / 取消），并清掉背后待执行的那次切换。 */
  function closeDivergencePrompt() {
    setDiverged(null);
    setPendingSwitch(null);
  }

  /** 把待执行的那次动作跑完（切 / 回滚走同一套确认，只有最后一步不同）。 */
  async function runPending(p: PendingSwitch) {
    if (p.kind === "rollback") await doRollback(p.entry.id);
    else await doSwitch(p.entry);
  }

  /**
   * 执行弹窗里勾选的同步：一次写库周期搞定全部组，客户端只重启一次。
   *
   * 同步成功后**接着把待执行的那次切换做掉** —— 用户最初点的是「切换」，
   * 同步只是他同意加上的前置步骤，不该让他再点一次。
   */
  async function runDivergedSync() {
    if (!clientKey || !diverged || syncBusy) return;
    const pending = pendingSwitch;
    const items: { groupId: string; direction: TraeSyncDirection }[] = [];
    for (const g of diverged.groups) {
      if (!divergedOn[g.groupId]) continue;
      const direction = divergedDir[g.groupId] ?? g.suggestedDirection;
      // ⚠️ 没有方向就**绝不下发**：这是覆盖数据的操作，猜错方向 = 拿旧内容盖掉新内容。
      if (!direction) continue;
      items.push({ groupId: g.groupId, direction });
    }
    if (items.length === 0) {
      // 一项都没勾 ≈ 用户实际想跳过同步。此时主按钮已被禁用，这段只是兜底：
      // 别让「点了主按钮却什么都不发生」的情况出现。
      if (pending) {
        closeDivergencePrompt();
        await runPending(pending);
      }
      return;
    }
    setSyncBusy(true);
    setSyncLog([]);
    try {
      const r = await api.traeSessionSyncGroups(clientKey, items);
      toast.success(`已同步 ${r.count} 个关联会话`, {
        description: r.relaunched ? "已自动重启客户端" : "同步完成，但自动重启客户端失败",
      });
      setDiverged(null);
      await loadOverview(clientKey);
      setPendingSwitch(null);
      if (pending) await runPending(pending);
    } catch (cause) {
      // 同步失败就**停在原地**：待执行的切换留着，用户还能改选「直接切换」或重试。
      // 不能默默把切换做掉 —— 那等于绕过了他刚提的那个要求。
      toast.error("同步失败", { description: api.asError(cause) });
    } finally {
      setSyncBusy(false);
    }
  }

  /** 真正执行一次切换。先经过 [`onSwitch`] 的分叉探测与确认。 */
  async function doSwitch(entry: TraeVaultEntry) {
    if (!clientKey) return;
    setBusy(true);
    setProgress([]);
    try {
      const res = await api.traeSwitchTo(clientKey, entry.id);
      pushProgress(res.progress ?? []);
      const badge = outcomeBadge(res.outcome);
      toast.success(`${badge.label}：${entry.id}`, {
        description: res.message || undefined,
      });
      await loadOverview(clientKey);
      // ⚠️ 切完**不再**弹同步确认：T35 起已经在切换之前问过了。
      //    再弹一次等于把同一个问题问两遍，而且是真的「切完才问」。
    } catch (cause) {
      toast.error("切换失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  /**
   * 切换到某个账号。
   *
   * **先问、再切**（T35）：目标账号若有「关联副本已分叉 / 写坏」，先弹确认框，
   * 由用户决定「先同步再切」「直接切」还是「取消」。没有要提醒的就照常静默切换。
   */
  async function onSwitch(entry: TraeVaultEntry) {
    if (!clientKey || busy) return;
    const uid = targetUidOf(entry);
    // 拿不到 uid 就无从探测（探测必须有一个 uid 当「自己」）⇒ 不打扰，直接切。
    if (uid) {
      setBusy(true);
      const v = await probeTargetDivergence(uid);
      setBusy(false);
      if (v) {
        openDivergencePrompt(v, { entry, kind: "switch", targetUid: uid });
        return;
      }
    }
    await doSwitch(entry);
  }

  /**
   * 回滚到指定账号。后端语义 `trae_switch::rollback_to` ≡ `switch_to`，
   * 即「结束客户端 → 写回该账号的登录态 → 重启」，用于切换异常后恢复。
   * 入口在页面底部的独立「回滚」卡里（与 WorkBuddy 账号页同款位置与外观）。
   *
   * 与切换走同一套「先问、再执行」—— 回滚同样是切换，没有理由少问一次。
   */
  async function onRollback(accountId: string) {
    if (!clientKey || busy) return;
    const entry = overview?.vault.find((e) => e.id === accountId);
    const uid = entry ? targetUidOf(entry) : null;
    if (entry && uid) {
      setBusy(true);
      const v = await probeTargetDivergence(uid);
      setBusy(false);
      if (v) {
        openDivergencePrompt(v, { entry, kind: "rollback", targetUid: uid });
        return;
      }
    }
    await doRollback(accountId);
  }

  /** 真正执行一次回滚。先经过 [`onRollback`] 的分叉探测与确认。 */
  async function doRollback(accountId: string) {
    if (!clientKey) return;
    setBusy(true);
    setProgress([]);
    try {
      const res = await api.traeRollbackTo(clientKey, accountId);
      pushProgress(res.progress ?? []);
      const badge = outcomeBadge(res.outcome);
      toast.success(`${badge.label}：${accountId}`, {
        description: res.message || undefined,
      });
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("回滚失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  // ⚠️ 2026-10-14（T65 二改）：原来这里还有 `onBackup()`（备份当前登录态）与
  // `onImport()`（粘贴 JSON 导入账号备份）。用户要求 Trae 页头部严格按 WorkBuddy 的
  // 五个入口来（导入本机登录态 / 扫码添加账号 / 导出账号包 / 导入账号包 / 签到），
  // 这两个入口连同它们的弹窗一并删除。账号入库走「导入本机登录态」，
  // 整库搬家走「导出账号包 / 导入账号包」（文件型，与 WB 一致）。

  async function onRename() {
    if (!clientKey || !renameTarget || !renameName.trim()) return;
    try {
      const res = await api.traeRenameAccount(clientKey, renameTarget.id, renameName.trim());
      setRenameTarget(null);
      toast.success("已重命名", { description: `新名称：${res.id}` });
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("重命名失败", { description: api.asError(cause) });
    }
  }

  async function onExportAccount(accountId: string) {
    try {
      const res = await api.traeExportAccount(clientKey!, accountId);
      const text = JSON.stringify(res.payload ?? {}, null, 2);
      await navigator.clipboard.writeText(text);
      toast.success("已导出账号备份到剪贴板", {
        description: `粘贴到文本文件保存即可（含 base64 载体，可再导入）`,
      });
    } catch (cause) {
      toast.error("导出失败", { description: api.asError(cause) });
    }
  }

  async function onRemove(entry: TraeVaultEntry) {
    if (!clientKey) return;
    if (!window.confirm(`确定从账号库移除「${entry.id}」？\n只删本地档案，不影响客户端当前登录态。`)) return;
    try {
      await api.traeRemoveAccount(clientKey, entry.id);
      toast.success("已从账号库移除", { description: entry.id });
      await loadOverview(clientKey);
    } catch (cause) {
      toast.error("移除失败", { description: api.asError(cause) });
    }
  }

  async function onHandoffPreview() {
    if (!clientKey) return;
    try {
      const res = await api.traeHandoffPreview({
        clientKey,
        projectPath: hoProject.trim() || undefined,
        sessionIds: hoSessions
          .split(/[\s,，]+/)
          .map((s) => s.trim())
          .filter(Boolean),
        nextSteps: hoSteps
          .split("\n")
          .map((s) => s.trim())
          .filter(Boolean),
        keyFiles: hoKeyFiles
          .split(/[\s,，]+/)
          .map((s) => s.trim())
          .filter(Boolean),
        note: hoNote.trim() || undefined,
      });
      setHoResult({ files: res.files ?? [], skipped: res.skipped ?? [] });
    } catch (cause) {
      toast.error("交接记忆预览失败", { description: api.asError(cause) });
    }
  }

  async function onHandoffWrite() {
    if (!clientKey || busy) return;
    setBusy(true);
    try {
      const res = await api.traeHandoffWrite({
        clientKey,
        projectPath: hoProject.trim() || undefined,
        sessionIds: hoSessions
          .split(/[\s,，]+/)
          .map((s) => s.trim())
          .filter(Boolean),
        nextSteps: hoSteps
          .split("\n")
          .map((s) => s.trim())
          .filter(Boolean),
        keyFiles: hoKeyFiles
          .split(/[\s,，]+/)
          .map((s) => s.trim())
          .filter(Boolean),
        note: hoNote.trim() || undefined,
      });
      setHoResult({ files: res.files ?? [], skipped: res.skipped ?? [] });
      toast.success("交接记忆已写入", {
        description: `落盘 ${res.files?.length ?? 0} 个文件${res.skipped?.length ? `，跳过 ${res.skipped.length} 项` : ""}`,
      });
    } catch (cause) {
      toast.error("交接记忆写入失败", { description: api.asError(cause) });
    } finally {
      setBusy(false);
    }
  }

  /**
   * 界面上可见的客户端：**已被摘掉的不在其中**（2026-10 官方把 SOLO 并进 Trae CN 后，
   * 本机不再有它自己的程序 ⇒ 不再作为一个可切换的客户端出现）。判据只有
   * `@/lib/trae-client-list` 一处。
   */
  const visibleClients = useMemo(() => selectableClients(clients), [clients]);

  /**
   * 切换条的入参：只做一次「线上结构 → 组件结构」的字段翻译。
   * 顺序 /「常用」徽标 /「已并入且跑不起来的不显示」全在共用组件与那个判据里，
   * 本页不另写一套（页面之间顺序不同、徽标不同就是这么来的）。
   */
  const clientOptions = useMemo<TraeClientOption[]>(
    () =>
      clients.map((c) => ({
        key: c.key,
        label: c.label,
        installed: c.installed,
        hasLogin: c.has_login,
        mergedInto: c.merged_into,
        runnable: c.runnable,
        top: c.key === usageTopPick,
      })),
    [clients, usageTopPick],
  );

  /** 当前客户端：选中项若已从界面撤掉就落到可见列表首项，免得选中态悬空。 */
  const selected = visibleClients.find((c) => c.key === clientKey) ?? visibleClients[0] ?? null;
  const live = overview?.live;

  /**
   * 头部副标题里显示的登录态文件路径（与 WB 页那片等宽小字同构）。
   * 分隔符跟着数据目录走：Windows 上 `user_data_dir` 是反斜杠，macOS / Linux 是斜杠。
   */
  const loginStateFile = useMemo(() => {
    const dir = selected?.user_data_dir;
    if (!dir) return "—";
    const sep = dir.includes("\\") ? "\\" : "/";
    return [dir.replace(/[\\/]+$/, ""), "User", "globalStorage", "storage.json"].join(sep);
  }, [selected]);

  // ---- 积分：与账号卡片融合显示（展示层与 WorkBuddy 侧同一套）----
  const credits = useTraeCredits(clientKey);

  /** 账号 id → 积分条目（后端就是按账号目录扫描的，id 与账号库一致）。 */
  const creditById = useMemo(() => {
    const m = new Map<string, TraeCreditEntry>();
    for (const a of credits.result?.accounts ?? []) m.set(a.id, a);
    return m;
  }, [credits.result]);

  const creditsLoading = credits.result === null && credits.error === null;

  /**
   * 当前登录的账号是否已经在账号库里（不在就提示先「导入本机登录态」）。
   *
   * ⚠️ 判据 = 后端算好的 `isCurrent`（唯一出口），**不再在这里自己拼 uid 比较**。
   * 这里原来写的是 `v.meta?.verified_uid === live.uid || v.oauth?.uid === live.uid`，
   * 与卡片上那枚「当前登录」徽标（另一处自己拼、且漏了 oauth 分支）**不是同一份判据** ——
   * 于是出现同屏自相矛盾：这条 Alert 认为「账号已在库里」（不报），卡片却一个徽标都不亮。
   * 现在两处都读 `isCurrent`，要么一起亮、要么一起不亮。
   */
  const liveInVault = useMemo(() => {
    if (!overview?.loggedIn || !live?.uid) return true;
    return overview.vault.some((v) => v.isCurrent);
  }, [overview, live]);

  /** 底部「回滚」卡的下拉候选：整个账号库（名字优先取积分条目里的真实昵称）。 */
  const rollbackOptions = useMemo(() => {
    return (overview?.vault ?? []).map((v) => ({
      id: v.id,
      label: creditById.get(v.id)?.name || v.displayName || v.id,
    }));
  }, [overview, creditById]);

  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      {/* ⚠️ 顶部版式**逐字照「WorkBuddy 账号管理」页**（2026-10-14 用户第二次提出，原话：
          「trae 的界面要求是导入本机登录态、扫码添加账号、导出账号包、导入账号包、签到，
          其他的重置排序、导入全部客户端等不需要。你要严格按 workbuddy 的界面进行设计」）。
          基准就是 WB 页那一行：左边「标题 + 一行说明」，右边一排 `outline` 小按钮，
          外层 `flex flex-wrap items-end justify-between gap-3`；
          按钮的**集合、顺序、图标、文案**全部对齐 WB（`LogIn` / `UserPlus` / `FileDown` /
          `FileUp` / 签到）。
          撤掉的四个入口：导入全部客户端、重置排序、导入备份、备份当前登录态
          （弹窗 / state / handler 一并删除，不留死代码 —— 见文件里几处「T65 二改」注释）。
          账号入库走「导入本机登录态」，整库搬家走「导出 / 导入账号包」。
          外层间距仍由页面容器的 `flex-col gap-5`（20px）给，与 WB 的 `mb-5` 同值。 */}
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold">Trae 账号管理</h1>
          {/* 副标题**与 WB 页同构**：WB 是「国内版 · 登录态文件 <路径>」，
              Trae 是「<客户端名> · 登录态文件 <路径>」—— 同样「一个标签 + 等宽小字路径」。
              ⚠️ 路径不是编的：`Trae/<数据目录>/User/globalStorage/storage.json` 就是这个
              客户端的登录态载体（`trae_import_local_login` 解密的就是它，账号卡上判
              「有载体」判的也是它），这里只是把账号库一直在用的那个相对路径拼到数据目录后面。
              数据目录本身来自后端（`selected.user_data_dir`，与「未识别到登录态」提示同源）。 */}
          <p className="mt-1 text-sm text-muted-foreground">
            {selected ? selected.label : "未检测到客户端"} · 登录态文件{" "}
            <code className="rounded bg-muted px-1 text-xs">{loginStateFile}</code>
          </p>
        </div>

        {/* 五个入口，与 WB 页**一一对应**（顺序也照抄）。按钮说明一律挂 `title`：
            版式对齐 WB 后没有多余的行内空间，但信息一个字都不能少。
            ⚠️ 不再用 `{selected && ...}` 整组隐藏 —— WB 页的按钮是常驻的，
            没有客户端时只置灰，不该「消失」（消失掉的按钮等于告诉用户这个功能不存在）。 */}
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => void onImportLocal()}
            disabled={!selected || busy}
            title="直接解密本机客户端已登录的账号，无需浏览器授权"
          >
            {busy ? <Loader2 className="size-4 animate-spin" /> : <LogIn className="size-4" />}
            导入本机登录态
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void onOauthStart()}
            disabled={!selected || busy}
            title="网页授权登录：立即拿账号凭证，可查额度 / 签到 / 用量；不写客户端登录态，不产生可切换载体"
          >
            <UserPlus className="size-4" />
            扫码添加账号
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => void onExportPack()}
            disabled={!selected || busy || !(overview?.vault.length ?? 0)}
            title="把本客户端账号库里的所有账号导出成一个 JSON 文件（自包含载体，可再导入）"
          >
            <FileDown className="size-4" />
            导出账号包
          </Button>
          {/* 与 WB 页同构：文件选择器藏在 `label` 里，外观就是普通 `outline` 小按钮。
              `asChild` 下 `disabled` 不生效（透传到 label 上无意义），所以用
              `pointer-events-none opacity-50` 表达置灰，`onChange` 里再兜一层判空。 */}
          <Button
            variant="outline"
            size="sm"
            asChild
            className={!selected || busy ? "pointer-events-none opacity-50" : undefined}
          >
            <label className="cursor-pointer" title="从 JSON 文件导入账号（支持整包，也支持单个账号）">
              <FileUp className="size-4" />
              导入账号包
              <input
                type="file"
                accept="application/json"
                className="hidden"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) void onImportPack(f);
                  e.target.value = "";
                }}
              />
            </label>
          </Button>
          {/* 签到：只签**当前选中客户端**下的账号（首页那个「一键签到」签的是全部）。
              两者是同一套实现（`CheckinButton`），只是 scope / clientKey 不同。 */}
          <CheckinButton
            scope="trae"
            clientKey={clientKey}
            onSuccess={() => void credits.refresh()}
          />
        </div>
      </div>

      {/* 客户端选择：顺序只由后端排序决定（使用记忆 + 能不能跑起来）。
          ⚠️ v0.2.25 起改用与首页 /「Trae 会话记录」**共用的** `<TraeClientSwitcher>`：
          本页原先自己内联了一套，于是「已并入」徽标、置灰规则都没跟上（合并后的 SOLO
          还留在标签条上），正是「同一件事两处实现必然有一处跑偏」。
          「已并入且本机跑不起来的客户端不进标签条」的判据也只有那一处。
          ⚠️ 右侧原来还有「重置排序 / 导入备份 / 备份当前登录态」三个按钮，
          2026-10-14（T65 二改）按用户要求撤掉，这一行只剩标签条 + 运行状态。 */}
      {visibleClients.length > 0 && (
        <div className="flex flex-wrap items-center gap-2">
          <TraeClientSwitcher
            clients={clientOptions}
            value={clientKey}
            onChange={setClientKey}
            disabled={busy}
            label="Trae 客户端（账号管理）"
          />
          <Badge variant="secondary" className="ml-auto">
            {overview?.running ? "客户端运行中" : "客户端未运行"}
          </Badge>
        </div>
      )}

      {/* 网页（OAuth）登录**进行中**的状态：与「WorkBuddy 账号管理」页的「扫码登录」`Alert`
          同一套版式（2026-10-14 用户要求：以 WorkBuddy 为准）。入口按钮在上一行标题的右侧。
          ⚠️ 别再把它套回 `Card` —— 两个页面顶部一模一样才是这次改动的目的。 */}
      {selected && loginOpen ? (
        <Alert>
          <LogIn className="size-4" />
          <AlertTitle>网页授权登录</AlertTitle>
          {/* ⚠️ `AlertDescription` 默认是 `grid justify-items-start`：行内要「撑满」的元素
              （回调 URL 那行的输入框）必须显式 `justify-items-stretch` + `w-full`，
              否则整行会缩成内容宽，输入框就没宽度了。 */}
          <AlertDescription className="min-w-0 justify-items-stretch gap-3">
            <div className="flex w-full min-w-0 flex-wrap items-center gap-2 text-sm">
              {["waiting", "callback_received", "processing"].includes(oauthStatus?.state ?? "") ? (
                <Loader2 className="size-4 animate-spin text-muted-foreground" />
              ) : oauthStatus?.state === "done" ? (
                <CheckCircle2 className="size-4 text-emerald-500" />
              ) : oauthStatus?.state === "error" || oauthStatus?.state === "stopped" ? (
                <AlertTriangle className="size-4 text-amber-500" />
              ) : null}
              <span className="min-w-0 flex-1 text-muted-foreground">
                {oauthStatus?.message || oauthStateLabel(oauthStatus?.state ?? "waiting")}
              </span>
              <Button size="sm" variant="ghost" onClick={onOauthClose} title="关闭并停止监听">
                <X className="size-4" />关闭
              </Button>
            </div>

            {oauthLoginUrl ? (
              <div className="flex w-full min-w-0 flex-wrap items-center gap-2">
                <Input readOnly value={oauthLoginUrl} className="min-w-0 flex-1 font-mono text-xs" />
                <Button size="sm" variant="outline" onClick={() => void onOauthOpenUrl(false)}>
                  <ExternalLink className="size-4" />打开
                </Button>
                {oauthBrowsers.length > 0 ? (
                  <>
                    <select
                      value={oauthBrowserKey}
                      onChange={(e) => setOauthBrowserKey(e.target.value)}
                      className="h-8 rounded-md border border-input bg-background px-2 text-xs outline-none"
                    >
                      {oauthBrowsers.map((b) => (
                        <option key={b.key} value={b.key}>
                          {b.label}
                        </option>
                      ))}
                    </select>
                    <Button size="sm" variant="outline" onClick={() => void onOauthOpenUrl(true)}>
                      私密窗口
                    </Button>
                  </>
                ) : null}
              </div>
            ) : null}

            {["waiting", "callback_received", "processing"].includes(oauthStatus?.state ?? "") ? (
              <div className="flex w-full flex-wrap items-center gap-2">
                <Button size="sm" variant="ghost" onClick={() => void onOauthStop()}>
                  停止监听
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setManualOpen((v) => !v)}>
                  授权页没回跳？手动粘贴回调 URL
                </Button>
              </div>
            ) : null}

            {manualOpen ? (
              <div className="flex w-full min-w-0 flex-wrap items-center gap-2">
                <Input
                  value={manualUrl}
                  onChange={(e) => setManualUrl(e.target.value)}
                  placeholder="http://127.0.0.1:17388/authorize?authCodeInfo=…"
                  className="min-w-0 flex-1 font-mono text-xs"
                />
                <Button size="sm" onClick={() => void onOauthManual()} disabled={!manualUrl.trim() || busy}>
                  {busy ? <Loader2 className="size-4 animate-spin" /> : null}完成登录
                </Button>
              </div>
            ) : null}

            {oauthStatus?.note ? (
              <Alert>
                <AlertTriangle className="size-4" />
                <AlertTitle>注意</AlertTitle>
                <AlertDescription>{oauthStatus.note}</AlertDescription>
              </Alert>
            ) : null}
          </AlertDescription>
        </Alert>
      ) : null}

      {error ? (
        <Alert variant="destructive">
          <AlertTriangle className="size-4" />
          <AlertTitle>加载失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      ) : null}

      {loading ? (
        <div className="grid gap-4 md:grid-cols-2">
          <Skeleton className="h-40 rounded-xl" />
          <Skeleton className="h-40 rounded-xl" />
        </div>
      ) : selected ? (
        <>
          {/* 当前登录态不再单列一张卡：它就是账号库里带「当前」徽标的那张卡
              （与 WorkBuddy 账号管理页一致）。只有「当前账号还没入库」才需要提示。 */}
          {!overview?.loggedIn ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertTitle>未识别到登录态</AlertTitle>
              <AlertDescription>
                请先在 {selected.label} 中登录，再回来导入 / 切换账号。
                <span className="mt-1 block font-mono text-[11px] text-muted-foreground/70">
                  数据目录：{selected.user_data_dir}
                </span>
              </AlertDescription>
            </Alert>
          ) : !liveInVault ? (
            <Alert>
              <AlertTriangle className="size-4" />
              <AlertTitle>当前登录的账号还没加入账号库</AlertTitle>
              <AlertDescription>
                {live?.username ?? live?.email ?? "当前账号"}
                {live?.uid ? `（uid ${shortUid(live.uid)}）` : ""} 还没进账号库。
                点右上角「导入本机登录态」把它收进来，之后就能像 WorkBuddy 那样一卡一账号地看积分与切换。
              </AlertDescription>
            </Alert>
          ) : null}

          {/* 「已并入的客户端还有账号没带过来」/「带过来的账号缺网页凭证」：
              只提示，用户点一下才动账号库。 */}
          {mergePending ? (
            <Alert className="border-amber-500/40 bg-amber-500/[0.06]">
              <Layers className="size-4" />
              <AlertTitle>
                {mergePending.adopted.length
                  ? `还有 ${mergePending.adopted.length} 个账号留在别处`
                  : `有 ${mergePending.repaired.length} 个账号缺网页凭证`}
              </AlertTitle>
              <AlertDescription className="min-w-0 space-y-2">
                {mergePending.adopted.length ? (
                  <span className="block">
                    {[...new Set(mergePending.adopted.map((a) => a.from_label))].join(" / ")}
                    {" "}已经并入 {selected.label}，但<strong>账号库是分开存的</strong> ——
                    不搬过来的话，在 {selected.label} 下看不到这几个账号、也切不过去。
                    搬入按 uid 去重、不覆盖已有账号，源客户端的账号照样保留。
                  </span>
                ) : null}
                {mergePending.repaired.length ? (
                  <span className="block">
                    这些账号早先搬过来时<strong>只带了载体文件</strong>，网页凭证（oauth.json）没跟过来 ——
                    所以卡片上显示的是原始目录名与 <span className="font-mono">0 B</span>、也查不了积分。
                    点下面的按钮会<strong>只补缺失的文件</strong>（绝不覆盖已有内容、不动载体登录态）。
                  </span>
                ) : null}
                <span className="block min-w-0 font-mono text-[11px] break-all text-muted-foreground/80">
                  {mergePending.adopted
                    .map((a) => `${a.source_id}${a.uid ? `（uid …${a.uid.slice(-6)}）` : ""}`)
                    .join(" · ")}
                  {mergePending.adopted.length && mergePending.repaired.length ? " · " : ""}
                  {mergePending.repaired
                    .map((a) => `${a.id}（补 ${a.files.join(" + ")}）`)
                    .join(" · ")}
                </span>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={mergeBusy}
                  onClick={() => void onAdoptMerged()}
                >
                  {mergeBusy ? (
                    <Loader2 className="mr-2 size-4 animate-spin" />
                  ) : (
                    <Layers className="mr-2 size-4" />
                  )}
                  {mergePending.adopted.length
                    ? `一并带入 ${selected.label}`
                    : "补全这些账号的网页凭证"}
                </Button>
              </AlertDescription>
            </Alert>
          ) : null}

          {/* 账号库：一卡一账号（与 WorkBuddy 账号管理页同一套卡片） */}
          <section className="space-y-4">
            <TraeCreditsHeader
              credits={credits}
              accountCount={overview?.vault.length ?? 0}
              clientLabel={selected.label}
            />

            {overview?.vault.length ? (
              <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
                {overview.vault.map((entry) => {
                  const meta = entry.meta;
                  const oa = entry.oauth;
                  const isOauth = entry.kind === "oauth";
                  const hasCarrier = (meta?.files ?? []).some(
                    (f) => f.rel.replace(/\\/g, "/") === "User/globalStorage/storage.json",
                  );
                  // ⚠️ 「当前登录」判据 = 后端 `isCurrent`（唯一出口，与 WorkBuddy 同源同形）。
                  // 这里原来自己拼 `overview.loggedIn && meta?.verified_uid === live?.uid`：
                  // `meta.verified_uid` 只在**切换成功**时写、oauth 账号连 `meta` 都没有
                  // ⇒ 三个账号一个「当前登录」徽标都不亮（T65 四改，用户截图）。
                  const isLive = entry.isCurrent === true;
                  const name =
                    entry.displayName || (isOauth && oa?.displayName ? oa.displayName : null) || entry.id;
                  // uid **优先用后端那一份**（与 `isCurrent` 同源，见 `TraeVaultEntry.uid`）；
                  // 后端读不出时才回退到自己解析的旧路径。
                  const uid =
                    entry.uid ?? (isOauth ? (oa?.uid ?? null) : (meta?.verified_uid ?? null));
                  const exp = tokenExpiryLabel(oa?.tokenExp);
                  const credit = creditById.get(entry.id) ?? null;
                  return (
                    <AccountCard
                      key={entry.id}
                      name={name}
                      avatarUrl={oa?.avatar ?? credit?.avatar ?? null}
                      identity={uid ? `uid ${shortUid(uid)}` : entry.id}
                      identityTitle={uid ?? entry.id}
                      identityMono
                      isCurrent={isLive}
                      onAvatarClick={() => setTokenTarget(entry)}
                      avatarLabel="查看 token（可复制 / 续期）"
                      chips={
                        <>
                          {isLive ? <Chip tone="ok">当前登录</Chip> : null}
                          {isOauth ? <Chip tone="outline">网页凭证</Chip> : null}
                          {credit?.mobile ? <Chip tone="muted">{credit.mobile}</Chip> : null}
                          {!isOauth && meta ? (
                            <Chip tone="muted">
                              {meta.file_count} 文件 · {formatBytes(meta.total_bytes)}
                            </Chip>
                          ) : null}
                          {isOauth && !hasCarrier ? <Chip tone="warn">首次切换自动合成载体</Chip> : null}
                          {exp ? <Chip tone="err">{exp}</Chip> : null}
                        </>
                      }
                      actions={
                        <>
                          <IconAction
                            icon={<Power className="size-4" />}
                            label={
                              isLive
                                ? "当前登录账号"
                                : isOauth && !hasCarrier
                                  ? "切换（首次会以当前登录态为骨架自动合成载体）"
                                  : "切换到这个账号"
                            }
                            active={isLive}
                            disabled={busy || isLive}
                            spinning={busy}
                            onClick={() => void onSwitch(entry)}
                          />
                          <IconAction
                            icon={<PencilLine className="size-4" />}
                            label="重命名"
                            onClick={() => {
                              setRenameTarget(entry);
                              setRenameName(entry.id);
                            }}
                          />
                          <IconAction
                            icon={<Trash2 className="size-4" />}
                            label="从账号库移除"
                            destructive
                            onClick={() => void onRemove(entry)}
                          />
                        </>
                      }
                    >
                      <TraeCreditBlock
                        entry={credit}
                        loading={creditsLoading}
                        onExpand={() => {
                          if (credit) credits.setExpand(credit);
                        }}
                      />
                    </AccountCard>
                  );
                })}
              </div>
            ) : (
              <Card>
                <CardContent className="py-10 text-center text-sm text-muted-foreground">
                  账号库为空。点「扫码添加账号」授权添加，或「导入本机登录态」把客户端当前登录的账号收进来。
                </CardContent>
              </Card>
            )}
          </section>

          {/* 回滚 / 导出：单独一张卡（与 WorkBuddy 账号页同位置、同外观），不再塞进账号卡的图标栏 */}
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-base">回滚</CardTitle>
              <CardDescription>
                结束客户端并把登录态重新写回指定账号，用于切换异常、客户端起不来，或想强制回到
                某个账号时。回滚前会先把当前登录态备份一份，写回失败会自动还原；
                点「导出该账号备份」可把选中账号的登录态复制到剪贴板，便于在别的机器导入。
              </CardDescription>
            </CardHeader>
            <CardContent className="flex flex-wrap items-center gap-2">
              <Select
                value={rollbackTarget ?? ""}
                onValueChange={setRollbackTarget}
                disabled={busy || !rollbackOptions.length}
              >
                <SelectTrigger className="w-full sm:w-72">
                  <SelectValue placeholder="选择账号…" />
                </SelectTrigger>
                <SelectContent>
                  {rollbackOptions.map((o) => (
                    <SelectItem key={o.id} value={o.id}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Button
                variant="outline"
                disabled={busy || !rollbackTarget}
                onClick={() => rollbackTarget && void onRollback(rollbackTarget)}
              >
                {busy ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <RotateCcw className="size-4" />
                )}
                回滚到该账号
              </Button>
              <Button
                variant="outline"
                disabled={busy || !rollbackTarget}
                onClick={() => rollbackTarget && void onExportAccount(rollbackTarget)}
              >
                <FileDown className="size-4" />
                导出该账号备份
              </Button>
            </CardContent>
          </Card>

          {/* 切换进度 */}
          {progress.length > 0 ? (
            <Card>
              <CardHeader className="pb-2">
                <CardTitle className="flex items-center gap-2 text-sm">
                  {busy ? <Loader2 className="size-4 animate-spin" /> : <CheckCircle2 className="size-4 text-emerald-500" />}
                  切换日志
                </CardTitle>
              </CardHeader>
              <CardContent>
                <pre
                  ref={progressRef}
                  className="max-h-52 overflow-auto rounded-lg bg-muted/60 p-3 text-xs leading-5 text-muted-foreground"
                >
                  {progress.join("\n") || "（无输出）"}
                </pre>
              </CardContent>
            </Card>
          ) : null}

          {/* 交接记忆 */}
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="flex items-center gap-2 text-base">
                <ArchiveRestore className="size-4" />
                交接记忆
                <Button size="sm" variant="ghost" className="ml-auto" onClick={() => setHandoffOpen((v) => !v)}>
                  {handoffOpen ? "收起" : "展开"}
                </Button>
              </CardTitle>
              <CardDescription>
                从解密库自动提取会话要点 → 写入项目工作目录、项目规则与 Trae 记忆库，方便下一个账号接续任务。
              </CardDescription>
            </CardHeader>
            {handoffOpen ? (
              <CardContent className="grid gap-3">
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">项目工作目录（绝对路径，留空则跳过记忆库写入）</label>
                  <Input
                    value={hoProject}
                    onChange={(e) => setHoProject(e.target.value)}
                    placeholder="D:\projects\my-app"
                  />
                </div>
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">会话 ID（可选，逗号分隔；留空自动取解密库全部会话）</label>
                  <Input
                    value={hoSessions}
                    onChange={(e) => setHoSessions(e.target.value)}
                    placeholder="32 位十六进制会话 ID，多个用逗号分隔"
                  />
                </div>
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">下一步计划（每行一条）</label>
                  <textarea
                    value={hoSteps}
                    onChange={(e) => setHoSteps(e.target.value)}
                    rows={3}
                    className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring"
                    placeholder={"1. 修复登录页白屏\n2. 补单元测试"}
                  />
                </div>
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">关键文件（可选，逗号分隔）</label>
                  <Input
                    value={hoKeyFiles}
                    onChange={(e) => setHoKeyFiles(e.target.value)}
                    placeholder="src/lib/api.ts, src/pages/LoginPage.tsx"
                  />
                </div>
                <div className="grid gap-2">
                  <label className="text-xs text-muted-foreground">补充说明（可选）</label>
                  <textarea
                    value={hoNote}
                    onChange={(e) => setHoNote(e.target.value)}
                    rows={2}
                    className="w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring"
                  />
                </div>
                <div className="flex items-center gap-2">
                  <Button variant="outline" onClick={() => void onHandoffPreview()} disabled={busy}>
                    预览落点
                  </Button>
                  <Button onClick={() => void onHandoffWrite()} disabled={busy}>
                    <Save className="size-4" />写入交接记忆
                  </Button>
                  {busy ? <Loader2 className="size-4 animate-spin text-muted-foreground" /> : null}
                </div>
                {hoResult ? (
                  <div className="rounded-lg border border-border bg-muted/40 p-3 text-xs">
                    {hoResult.files.length ? (
                      <>
                        <div className="mb-1 font-medium">将写入 / 已写入：</div>
                        <ul className="space-y-0.5 font-mono text-muted-foreground">
                          {hoResult.files.map((f) => (
                            <li key={f} className="truncate">{f}</li>
                          ))}
                        </ul>
                      </>
                    ) : null}
                    {hoResult.skipped.length ? (
                      <>
                        <div className="mb-1 mt-2 font-medium text-amber-600">跳过：</div>
                        <ul className="space-y-0.5 text-amber-600/80">
                          {hoResult.skipped.map((s) => (
                            <li key={s}>{s}</li>
                          ))}
                        </ul>
                      </>
                    ) : null}
                  </div>
                ) : null}
              </CardContent>
            ) : null}
          </Card>
        </>
      ) : (
        <Alert>
          <AlertTriangle className="size-4" />
          <AlertTitle>未安装 Trae 客户端</AlertTitle>
          <AlertDescription>
            未检测到 Trae 客户端的数据目录。请先安装并登录 Trae CN（国内版）再回来。
          </AlertDescription>
        </Alert>
      )}

      <Separator className="my-1" />

      {/* 账号 token 弹窗（点头像打开）：看 / 复制 / 续期。与 WorkBuddy 侧共用同一组件。 */}
      <AccountTokenDialog
        open={tokenTarget !== null}
        onOpenChange={(v) => !v && setTokenTarget(null)}
        provider="trae"
        targetKey={clientKey}
        accountId={tokenTarget?.id ?? null}
        accountName={tokenTarget ? tokenTarget.displayName || tokenTarget.id : null}
      />

      {/* ⚠️ 2026-10-14（T65 二改）：原来这里还有两个 `Dialog`
          ——「备份当前登录态」与「导入账号备份（粘贴 JSON）」。
          Trae 页头部改成与 WorkBuddy 一致的五个入口后，这两个入口没有了，
          弹窗一并删除（留着就是永远打不开的死 UI）。
          现在整库搬家走文件：「导出账号包」下载 JSON、「导入账号包」选文件导入。 */}

      {/* 重命名对话框 */}
      <Dialog open={renameTarget !== null} onOpenChange={(v) => !v && setRenameTarget(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>重命名账号</DialogTitle>
            <DialogDescription>改名只影响账号库条目，不影响客户端登录态。</DialogDescription>
          </DialogHeader>
          <Input value={renameName} onChange={(e) => setRenameName(e.target.value)} autoFocus />
          <DialogFooter>
            <Button variant="outline" onClick={() => setRenameTarget(null)}>取消</Button>
            <Button onClick={() => void onRename()} disabled={!renameName.trim() || renameName.trim() === renameTarget?.id}>
              确定
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 全部积分包 */}
      <TraeCreditsDialog entry={credits.expand} onClose={() => credits.setExpand(null)} />

      {/* 切换**之前**的「两端不一致」确认框（T35）：非强迫、可跳过，失败不打断主流程。
          以前是切完再弹 —— 那时账号已经换过去了，用户想反悔都来不及。 */}
      <Dialog
        open={diverged !== null}
        onOpenChange={(o) => {
          if (!o && !syncBusy) closeDivergencePrompt();
        }}
      >
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle className="flex items-center gap-2">
              <GitCompare className="size-4 text-amber-500" />
              {pendingSwitch
                ? `${pendingSwitch.kind === "rollback" ? "回滚" : "切换"}到「${pendingSwitch.entry.id}」前，先处理 ${diverged?.count ?? 0} 个会话？`
                : `检测到 ${diverged?.count ?? 0} 个会话需要处理`}
            </DialogTitle>
            <DialogDescription>
              这些会话此前在账号之间复制过并建立过关联，现在**两端内容不一致**，
              或**副本的数据引用已经错位**（后者在客户端里会显示不全）。
              同步会把**被覆盖那一端**的内容换成另一端，会话 id 与账号归属都保持不变。
              {pendingSwitch ? (
                <span className="mt-1 block">
                  现在还没切换，可以先处理干净再切；也可以直接切换，
                  之后仍能在「会话记录 → 跨账号关联」里同步。
                </span>
              ) : null}
            </DialogDescription>
          </DialogHeader>
          <div className="max-h-[46vh] space-y-2 overflow-y-auto pr-1">
            {(diverged?.groups ?? []).map((g) => (
              <DivergedRow
                key={g.groupId}
                group={g}
                checked={divergedOn[g.groupId] ?? true}
                direction={divergedDir[g.groupId] ?? g.suggestedDirection}
                disabled={syncBusy}
                onChecked={(v) => setDivergedOn((p) => ({ ...p, [g.groupId]: v }))}
                onDirection={(v) => setDivergedDir((p) => ({ ...p, [g.groupId]: v }))}
              />
            ))}
          </div>
          {syncLog.length > 0 && (
            <pre className="max-h-28 overflow-y-auto rounded-md border bg-muted/40 p-2 text-[11px] leading-relaxed">
              {syncLog.join("\n")}
            </pre>
          )}
          {/* ⚠️ 这行提示**不能**塞进 `DialogFooter`：那个容器在 ≥640px 是 `flex-row`，
              三个按钮已经把宽度吃满，文字列会被压到几十像素 ⇒ **一个字一行**
              （730px 实测折成 8 行）。单独占一行，任何宽度都正常。 */}
          <p className="text-xs text-muted-foreground">
            {pendingSwitch
              ? `会先整份备份再回写；同步完成后自动${pendingSwitch.kind === "rollback" ? "回滚" : "切换"}到「${pendingSwitch.entry.id}」`
              : "会先整份备份再回写，并自动重启客户端"}
          </p>
          <DialogFooter>
            {pendingSwitch ? (
              <>
                {/* 「取消」= 连切换一起放弃（用户本来只想切号，看到有分叉决定先不动）。 */}
                <Button variant="outline" onClick={closeDivergencePrompt} disabled={syncBusy}>
                  取消{pendingSwitch.kind === "rollback" ? "回滚" : "切换"}
                </Button>
                <Button
                  variant="outline"
                  disabled={syncBusy}
                  onClick={() => {
                    const p = pendingSwitch;
                    closeDivergencePrompt();
                    void runPending(p);
                  }}
                >
                  直接{pendingSwitch.kind === "rollback" ? "回滚" : "切换"}（不同步）
                </Button>
                <Button
                  onClick={() => void runDivergedSync()}
                  disabled={syncBusy || syncSelected === 0}
                >
                  {syncBusy && <Loader2 className="mr-2 size-4 animate-spin" />}
                  {syncSelected > 0
                    ? `同步 ${syncSelected} 项并${pendingSwitch.kind === "rollback" ? "回滚" : "切换"}`
                    : "选定方向后可继续"}
                </Button>
              </>
            ) : (
              <>
                <Button variant="outline" onClick={closeDivergencePrompt} disabled={syncBusy}>
                  跳过
                </Button>
                <Button
                  onClick={() => void runDivergedSync()}
                  disabled={syncBusy || syncSelected === 0}
                >
                  {syncBusy && <Loader2 className="mr-2 size-4 animate-spin" />}
                  {syncSelected > 0 ? `同步选中的 ${syncSelected} 项` : "选定方向后可同步"}
                </Button>
              </>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
