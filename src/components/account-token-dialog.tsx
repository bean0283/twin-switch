import { useCallback, useEffect, useState } from "react";
import { Check, Copy, Eye, EyeOff, Loader2, RefreshCw, ShieldAlert, X } from "lucide-react";
import { toast } from "sonner";

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
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import * as api from "@/lib/api";
import type { AccountTokenInfo, TraeAccountToken, WbAccountToken } from "@/lib/trae-types";

/**
 * 账号 token 弹窗：**看 / 复制 / 续期**。Trae 与 WorkBuddy **共用这一个组件**。
 *
 * 入口 = 账号卡片的**头像**（用户要求「点账号头像出现弹窗」）。这里只做三件事：
 *   1. 显示这个账号当前的 jwt 本体（默认打码，可一键复制，也可「显示」原样看）；
 *   2. 显示它的有效期（绝对时间 + 还剩多久）；
 *   3. 「刷新 token 有效期」按钮 —— 续期门槛两边一致：**剩余 > 7 天默认不刷新**
 *      （不消耗 refreshToken 的轮换代次），但**不是拒绝**：弹一个确认窗让用户决定
 *      要不要**强制刷新**。
 *
 * ⚠️ 「跳过」与「失败」长得不一样（项目硬规则）：跳过是后端 `Ok` + `refreshed:false`
 *    → 我们弹「要不要强制刷新」；真失败是 `Err` → 原文照实报错，绝不伪装成「已跳过」。
 *
 * ⚠️ token 是敏感串，**默认打码**：复制按钮一直可用（不必先显示），避免投屏/录屏时被看走。
 *
 * ⚠️ 两个产品的差别只在「数据从哪来 / 怎么刷」，因此在 `loadInfo` / `refreshOnce` 两处
 *    按 `provider` 分流，**其余展示逻辑只有一份**（新加产品只需在这里加一支）。
 */
/**
 * 三个产品的令牌弹窗是**同一个组件**，只按这里分流取值。
 *
 * ⚠️ Qoder 的 access_token 是 `dt-…` 不透明串（**不是 JWT**），到期时间只能读账号库里的
 * `expires_at`；Trae / WorkBuddy 是 JWT，到期时间从 token 的 `exp` 解。
 * 这个差别只在后端取值层，展示层完全共用。
 */
export type TokenProvider = "trae" | "workbuddy" | "qoder";

/**
 * 官方当前**签发**的 access token 有效期（天）。**本工具指定不了这个值。**
 *
 * ⚠️⚠️ 两个产品**不一样**，绝不能共用一个数字 —— 这个弹窗是 Trae 与 WorkBuddy 共用的，
 *   写死一个数就会让另一个产品的界面**撒谎**（T60 的初版就犯过这个错：Trae 弹窗里
 *   写着「官方目前签发的就是 28 天」，而 28 天是 WorkBuddy 的值）。
 *
 * 取证 —— 都取「刚续期成功」那一刻的 `exp - iat`（刷新后的 token 才是全新的，也只有
 * 这一刻 `exp - 刷新时刻` 才等于签发值；拿旧 token 算出来的会偏小）：
 *
 * - **Trae = 14 天**：`~/.twin-switch/trae/vault/trae-cn/oauth_717036/oauth.json`
 *   `iat 2026-10-13T13:26:21Z → exp 2026-10-27T13:26:21Z` = **1209600 s = 14.000 天整**；
 *   同目录另 3 份刚续期的账号档也是 1209600 s。对照那几份「刷新失败、token 没换」的
 *   （12.42 / 12.409 天）正好印证：**算偏小的都不是签发值**。
 * - **WorkBuddy = 28 天**：官方客户端自己写的
 *   `~/.twin-switch/backups/workbuddy-desktop.20261007-183235.info`
 *   → `auth.expiresIn = 2419200 s = 28.000 天整`。
 *
 * ⚠️ 这两个数**只用于文案举例**，不参与任何判定 —— 到期时间一律以 token 里的 `exp` 为准。
 */
export const ISSUED_VALIDITY_DAYS: Record<TokenProvider, number> = {
  trae: 14,
  workbuddy: 28,
  // Qoder = 30 天：账号库里 `expires_at - created_at` 实测 30.035 / 29.999 天
  // （cn 弦思十安 2026-10-20→2026-11-19、global bean0283 同期），是整 30 天。
  // ⚠️ 同样只用于文案举例，判定一律看账号库里的 `expires_at`。
  qoder: 30,
};

/** 刷新结果的**归一化**形状（两个产品的响应共用这几个字段）。 */
interface TokenRefreshOutcome {
  refreshed: boolean;
  reason?: string;
  remainingDays: number;
  thresholdDays?: number;
  message: string;
  /** 刷新后的到期时间（毫秒）。 */
  tokenExp?: number | null;
  /** 刷新**前**的到期时间（毫秒）—— 用来回答「怎么反而变短了」。 */
  tokenExpBefore?: number | null;
}

/** 一次刷新后「到期时间怎么变的」——只在真的刷新过之后才有值。 */
interface RefreshShift {
  before: number;
  after: number;
  shortened: boolean;
}

export function AccountTokenDialog({
  open,
  onOpenChange,
  provider,
  targetKey,
  accountId,
  accountName,
  extraFooter,
}: {
  open: boolean;
  onOpenChange: (v: boolean) => void;
  provider: TokenProvider;
  /**
   * Trae 用：客户端 key（`trae-cn` / `solo-cn`…）；
   * Qoder 用：区域（`cn` / `global`）；
   * WorkBuddy 传 `null`（它不分客户端）。
   */
  targetKey: string | null;
  accountId: string | null;
  /** 只在标题上给人看，不参与取值。 */
  accountName?: string | null;
  /**
   * 产品独有的附加操作（渲染在「关闭」左边）。
   *
   * 目前只有 Qoder 用：它需要一个「设置 Cosy 设备令牌」的入口（签到/活动下发要这个身份）。
   * 做成插槽而不是给 Qoder 再写一个弹窗 —— 那正是这次要消灭的自定义布局。
   */
  extraFooter?: React.ReactNode;
}) {
  const [info, setInfo] = useState<AccountTokenInfo | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reveal, setReveal] = useState(false);
  const [copied, setCopied] = useState(false);
  /** refresh token 是否明文展示（同样默认打码）。 */
  const [revealRt, setRevealRt] = useState(false);
  /** 刚导出过 refresh token（按钮上打个勾）。 */
  const [copiedRt, setCopiedRt] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  /**
   * 续期失败原文（可能很长：后端会把**每一种「凭据 + 设备身份 + 客户端参数」组合**的
   * 服务端答复逐条列出来）。所以它**不进 toast**（会被截断、还会把提示框撑爆），
   * 而是落在弹窗内一块可滚动区域里；toast 只报第一行。
   */
  const [refreshError, setRefreshError] = useState<string | null>(null);
  /** 非空 = 刚刷新成功过，用来把「刷新前 → 刷新后」摆出来。 */
  const [refreshShift, setRefreshShift] = useState<RefreshShift | null>(null);
  /** 非空 = 正在问「要不要强制刷新」。 */
  const [forceAsk, setForceAsk] = useState<{ remainingDays: number; thresholdDays: number } | null>(null);

  const load = useCallback(async () => {
    if (!accountId) return;
    if ((provider === "trae" || provider === "qoder") && !targetKey) return;
    setLoading(true);
    setError(null);
    try {
      setInfo(
        provider === "trae"
          ? normalizeTrae(await api.traeAccountToken(targetKey!, accountId))
          : provider === "qoder"
            // Qoder 后端返回的**就是** `AccountTokenInfo`（`qoder_accounts::token_info` 按
            // 这个形状拼的）⇒ 不需要归一化，直接就是弹窗要的形状。
            ? await api.qoderAccountToken(targetKey!, accountId)
            : normalizeVaultAccount(await api.workbuddyAccountToken(accountId)),
      );
    } catch (cause) {
      setInfo(null);
      setError(api.asError(cause));
    } finally {
      setLoading(false);
    }
  }, [provider, targetKey, accountId]);

  // 每次打开都重新读一遍：token 会被客户端自己改，界面不能拿上次的旧值糊弄人。
  useEffect(() => {
    if (open && accountId && (provider === "workbuddy" || targetKey)) {
      setReveal(false);
      setCopied(false);
      setRevealRt(false);
      setCopiedRt(false);
      setForceAsk(null);
      setRefreshError(null);
      setRefreshShift(null);
      void load();
    } else if (!open) {
      setInfo(null);
      setError(null);
      setForceAsk(null);
      setRefreshError(null);
      setRefreshShift(null);
    }
  }, [open, provider, targetKey, accountId, load]);

  async function copyToken() {
    const token = info?.token;
    if (!token) return;
    try {
      await navigator.clipboard.writeText(token);
      setCopied(true);
      toast.success("已复制 token", { description: "粘贴到需要的地方即可（含 Cloud-IDE-JWT 用法请按目标要求处理）" });
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("复制失败", { description: "可以点「显示」后手动全选复制" });
    }
  }

  /**
   * 复制 refresh token（到剪贴板）。
   *
   * 用途：在**别的工具/机器**上也登记这个账号，之后由那边负责续期 ——
   * 不用把账号密码再登一遍。⚠️ 它等同于登录凭据：拿到就能一直续期，
   * 所以文案里必须写明「只贴到可信工具」。
   */
  async function copyRefreshToken() {
    const rt = info?.refreshToken;
    if (!rt) return;
    try {
      await navigator.clipboard.writeText(rt);
      setCopiedRt(true);
      setTimeout(() => setCopiedRt(false), 1600);
      toast.success("已复制 refresh token", {
        description: "可粘贴到别的工具里登记同一账号。它等同登录凭据，别贴到不可信的地方。",
      });
    } catch {
      toast.error("复制失败", { description: "点「显示」后手动全选复制" });
    }
  }

  /** `force = false` 时后端可能回「本次不刷新」→ 我们据此弹强制刷新确认窗。 */
  async function doRefresh(force: boolean) {
    if (!accountId) return;
    if ((provider === "trae" || provider === "qoder") && !targetKey) return;
    setRefreshing(true);
    setRefreshError(null);
    try {
      const res: TokenRefreshOutcome =
        provider === "trae"
          ? await api.traeAccountTokenRefresh(targetKey!, accountId, force)
          : provider === "qoder"
            ? await api.qoderAccountTokenRefresh(targetKey!, accountId)
            : await api.workbuddyAccountTokenRefresh(accountId, force);
      if (!res.refreshed) {
        setForceAsk({
          remainingDays: res.remainingDays,
          thresholdDays: res.thresholdDays ?? info?.thresholdDays ?? 7,
        });
        return;
      }
      toast.success("token 已刷新", { description: res.message });
      setForceAsk(null);
      // ⚠️ 刷新拿到的有效期**由服务端决定**，不保证比原来长 —— 官方在 2026-10 把这个
      //    值一路收紧（WorkBuddy 55 → 28 天、Trae 现在是 14 天）。不说清楚，用户只会看到
      //    「怎么反而变短了」。
      const before = res.tokenExpBefore ?? null;
      const after = res.tokenExp ?? null;
      setRefreshShift(
        before && after ? { before, after, shortened: after < before } : null,
      );
      await load();
    } catch (cause) {
      const detail = api.asError(cause);
      setRefreshError(detail);
      // toast 只放第一行，完整清单在弹窗里看（见 `refreshError` 的注释）。
      toast.error("刷新 token 失败", { description: detail.split("\n")[0] });
    } finally {
      setRefreshing(false);
    }
  }

  const exp = info?.tokenExp ?? null;
  const token = info?.token ?? "";
  const masked = maskToken(token);
  const what =
    provider === "trae" ? "Trae" : provider === "qoder" ? "Qoder" : "WorkBuddy";
  /** 官方签发的有效期天数 —— **按产品分流**，只用于文案举例（判定一律看 token 的 `exp`）。 */
  const issuedDays = ISSUED_VALIDITY_DAYS[provider];

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle className="min-w-0 truncate pr-6">
              {what} 账号 token{accountName ? `｜${accountName}` : ""}
            </DialogTitle>
            <DialogDescription className="break-all whitespace-pre-wrap">
              这是该账号登录态的 jwt 本体。工具只读取账号档案，不会用它做任何写操作；续期走官方接口，
              成功后新 token 会写回账号档案。
            </DialogDescription>
          </DialogHeader>

          {loading ? (
            <div className="flex items-center gap-2 py-8 text-sm text-muted-foreground">
              <Loader2 className="size-4 animate-spin" />
              正在读账号档案…
            </div>
          ) : error ? (
            <Alert variant="destructive">
              <ShieldAlert className="size-4" />
              <AlertTitle>读不到这个账号的凭证</AlertTitle>
              <AlertDescription className="break-all whitespace-pre-wrap">{error}</AlertDescription>
            </Alert>
          ) : info ? (
            <div className="grid min-w-0 gap-3">
              {/* 有效期 + 来源 */}
              <div className="grid gap-1.5 rounded-lg border bg-muted/30 px-3 py-2.5 text-sm">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-muted-foreground">有效期</span>
                  <span className="font-medium">{expiryText(exp)}</span>
                  <span className={remainToneClass(info.remainingDays)}>{remainText(exp)}</span>
                </div>
                <div className="text-xs text-muted-foreground">来源：{info.sourceLabel}</div>
                {info.blockedReason ? (
                  <div className="text-xs font-medium text-amber-600 break-all whitespace-pre-wrap">
                    {info.blockedReason}
                  </div>
                ) : info.canRefresh ? null : (
                  <div className="text-xs font-medium text-amber-600">
                    缺少续期材料（refreshToken / 设备密钥），这个账号只能看、不能刷。
                  </div>
                )}
                {info.refreshExpiredAt ? (
                  <div className="text-xs text-muted-foreground">
                    刷新额度到期：{new Date(info.refreshExpiredAt).toLocaleString("zh-CN", { hour12: false })}
                  </div>
                ) : null}
              </div>

              {/* token 本体 */}
              <div className="grid min-w-0 gap-2">
                <div className="flex items-center gap-2">
                  <span className="text-xs text-muted-foreground">token</span>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="h-7 px-2 text-xs"
                    onClick={() => setReveal((v) => !v)}
                  >
                    {reveal ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
                    {reveal ? "隐藏" : "显示"}
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    className="ml-auto min-w-0"
                    disabled={!token}
                    onClick={() => void copyToken()}
                  >
                    {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
                    复制 token
                  </Button>
                </div>
                <div className="max-h-32 min-w-0 overflow-auto rounded-lg border bg-muted/40 px-3 py-2 font-mono text-xs leading-5 break-all whitespace-pre-wrap select-all">
                  {reveal ? token || "（空）" : masked || "（空）"}
                </div>

              {/* refresh token：可「导出」到别处续期（仅 WorkBuddy 走这条路径） */}
              {provider === "workbuddy" ? (
                <div className="grid min-w-0 gap-2">
                  <div className="flex items-center gap-2">
                    <span className="text-xs text-muted-foreground">refresh token</span>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-7 px-2 text-xs"
                      disabled={!info.refreshToken}
                      onClick={() => setRevealRt((v) => !v)}
                    >
                      {revealRt ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
                      {revealRt ? "隐藏" : "显示"}
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      className="ml-auto min-w-0 gap-1.5"
                      disabled={!info.refreshToken}
                      onClick={() => void copyRefreshToken()}
                    >
                      {copiedRt ? <Check className="size-4" /> : <Copy className="size-4" />}
                      复制 refresh token
                    </Button>
                  </div>
                  <div className="max-h-32 min-w-0 overflow-auto rounded-lg border bg-muted/40 px-3 py-2 font-mono text-xs leading-5 break-all whitespace-pre-wrap select-all">
                    {info.refreshToken
                      ? revealRt
                        ? info.refreshToken
                        : maskToken(info.refreshToken)
                      : "（这个账号没有可导出的 refresh token）"}
                  </div>
                  <div className="text-[11px] text-muted-foreground">
                    {info.refreshToken ? (
                      <>
                        粘到别的工具里即可登记同一账号、由那边自动续期
                        {info.refreshExpiredAt
                          ? `（有效期到 ${new Date(info.refreshExpiredAt).toLocaleString("zh-CN", { hour12: false })}）`
                          : ""}
                        。
                        <span className="ml-1 font-medium text-amber-600">
                          它等同于登录凭据，只贴到可信工具。
                        </span>
                      </>
                    ) : (
                      "加密信封 / 未采集到凭据的账号解不出明文 refresh token，无法导出。"
                    )}
                  </div>
                </div>
              ) : null}
              </div>

              {/* 续期 */}
              <div className="flex flex-wrap items-center gap-2 rounded-lg border px-3 py-2.5">
                <Button
                  size="sm"
                  variant="outline"
                  disabled={refreshing || !info.canRefresh}
                  onClick={() => void doRefresh(false)}
                >
                  {refreshing ? (
                    <Loader2 className="size-4 animate-spin" />
                  ) : (
                    <RefreshCw className="size-4" />
                  )}
                  刷新 token 有效期
                </Button>
                <span className="min-w-0 text-xs text-muted-foreground">
                  剩余 &gt; {info.thresholdDays} 天时默认不刷新（不消耗 refreshToken 轮换代次）；
                  点下去会先确认，再决定要不要强制刷新。
                </span>
              </div>

              {/* 刷新前后的到期时间对照 —— 「怎么反而变短了」的答案就在这儿。
                  ⚠️ 有效期是**服务端签发**的，本工具指定不了；官方 2026-10 把它一路收紧
                  （WorkBuddy 55 → 28 天、Trae 14 天），所以「刷新」并不等于「延长」。 */}
              {refreshShift ? (
                <div
                  // 自查钩子：探针按这个属性判「对照条画出来了没 / 判没判出变短」。
                  // ⚠️ 不许改成看正文文本 —— `index.html` 的看门狗脚本源码也在 <body> 里，
                  //    全文搜这类判定从第一毫秒起就恒真/恒假（项目硬规则）。
                  data-refresh-shift={refreshShift.shortened ? "shortened" : "extended"}
                  // 自查钩子：探针据此断言「Trae 档写的是 14、WorkBuddy 档写的是 28」——
                  // 共用一个写死数字的老毛病就靠这个属性抓。
                  data-issued-validity-days={issuedDays}
                  className={
                    refreshShift.shortened
                      ? "grid gap-1 rounded-lg border border-amber-500/50 bg-amber-500/10 px-3 py-2 text-xs"
                      : "grid gap-1 rounded-lg border bg-muted/30 px-3 py-2 text-xs"
                  }
                >
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="text-muted-foreground">本次刷新</span>
                    <span className="font-mono">{expiryText(refreshShift.before)}</span>
                    <span className="text-muted-foreground">→</span>
                    <span className="font-mono font-medium">{expiryText(refreshShift.after)}</span>
                  </div>
                  <div className={refreshShift.shortened ? "text-amber-600" : "text-muted-foreground"}>
                    {refreshShift.shortened
                      ? `新的到期时间比原来还早 —— 有效期由服务端签发，本工具无法指定。${what} 官方目前签发的就是 ${issuedDays} 天，所以「刷新」不等于「延长」。`
                      : `到期时间已顺延（有效期由服务端签发，本工具无法指定；${what} 官方目前签发 ${issuedDays} 天）。`}
                  </div>
                </div>
              ) : null}

              {/* 续期失败原文（多行清单）—— 放弹窗内滚动看，不塞 toast */}
              {refreshError ? (
                <Alert variant="destructive" className="min-w-0">
                  <ShieldAlert className="size-4" />
                  <AlertTitle>刷新失败</AlertTitle>
                  <AlertDescription className="max-h-40 min-w-0 overflow-auto font-mono text-xs leading-5 break-all whitespace-pre-wrap">
                    {refreshError}
                  </AlertDescription>
                </Alert>
              ) : null}
            </div>
          ) : null}

          <DialogFooter>
            {extraFooter}
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              <X className="size-4" />
              关闭
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 强制刷新确认：token 还新鲜时才会走到这里 */}
      <AlertDialog open={forceAsk !== null} onOpenChange={(v) => !v && setForceAsk(null)}>
        <AlertDialogContent
          // 自查钩子：这个确认窗是「刷新被跳过」时才出现的，探针在这里就能断言
          // 「写出来的签发天数与本产品一致」（Trae 14 / WorkBuddy 28）。
          data-issued-validity-days={issuedDays}
          className="max-w-[min(44rem,calc(100vw-2rem))] min-w-0"
        >
          <AlertDialogHeader>
            <AlertDialogTitle className="break-all whitespace-pre-wrap">
              还剩 {forceAsk?.remainingDays ?? 0} 天，仍要强制刷新吗？
            </AlertDialogTitle>
            <AlertDialogDescription className="break-all whitespace-pre-wrap">
              token 仍然新鲜（剩余 {forceAsk?.remainingDays ?? 0} 天 &gt; 门槛{" "}
              {forceAsk?.thresholdDays ?? 7} 天），通常不需要刷新。
              强制刷新会消耗一代 refreshToken —— 服务端只保留最近两代，反复刷会把前几代提前作废，
              届时对应代的登录态会失效。
              <br />
              ⚠️ 另外，新 token 的有效期<strong>由服务端签发</strong>（本工具指定不了，{what}{" "}
              官方目前签发的就是 {issuedDays} 天）；如果它比当前剩余还短，刷新反而会让到期时间
              <strong>提前</strong>。
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                setForceAsk(null);
                void doRefresh(true);
              }}
            >
              强制刷新
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

/** Trae 响应 → 归一化形状。 */
function normalizeTrae(t: TraeAccountToken): AccountTokenInfo {
  return {
    token: t.token ?? "",
    refreshToken: t.refreshToken ?? null,
    tokenExp: t.tokenExp ?? null,
    refreshExpiredAt: null,
    sourceLabel:
      t.source === "oauth" ? "网页登录凭证（oauth.json）" : "载体登录态（storage.json）",
    blockedReason: null,
    canRefresh: Boolean(t.canRefresh),
    thresholdDays: t.thresholdDays ?? 7,
    remainingDays: t.remainingDays ?? -1,
  };
}

/**
 * 「账号库型」响应 → 归一化形状。**WorkBuddy 与 Qoder 共用**（两者后端已返回同一形状）。
 *
 * 差别只在 `sourceLabel` 的措辞上：WorkBuddy 说「加密信封 / 明文」，Qoder 说登录来源。
 * 靠 `t.source` 的取值区分：WorkBuddy 只有 `plain` / `envelope`，Qoder 是
 * `oauth` / `pat` / `desktop-app` / `cli`。
 */
function normalizeVaultAccount(t: WbAccountToken): AccountTokenInfo {
  const label =
    t.source === "plain"
      ? "本工具账号库（workbuddy-accounts.json，明文凭据）"
      : t.source === "envelope"
        ? "本工具账号库（WorkBuddy 加密信封，本地解不开）"
        : "本工具账号库（未采集到凭据）";
  return {
    token: t.token ?? "",
    refreshToken: t.refreshToken ?? null,
    tokenExp: t.tokenExp ?? null,
    refreshExpiredAt: t.refreshExpiredAt ?? null,
    sourceLabel: label,
    blockedReason: t.blockedReason ?? null,
    canRefresh: Boolean(t.canRefresh),
    thresholdDays: t.thresholdDays ?? 7,
    remainingDays: t.remainingDays ?? -1,
  };
}

/** 到期时间（绝对）。解不出 exp 时如实说明，不假装有值。 */
/** 敏感串打码：保留头 18 + 尾 10，中间省略（足够分辨是哪一枚，又抄不走）。 */
function maskToken(t: string): string {
  if (!t) return "";
  return t.length > 28 ? `${t.slice(0, 18)}…${t.slice(-10)}` : t;
}

function expiryText(ms: number | null): string {
  if (!ms) return "解不出到期时间（不是 JWT 或缺少 exp）";
  return new Date(ms).toLocaleString("zh-CN", { hour12: false });
}

/** 还剩多久；已过期 / 无 exp 各有独立说法。 */
function remainText(ms: number | null): string {
  if (!ms) return "";
  const left = ms - Date.now();
  if (left <= 0) return "已过期";
  const days = Math.floor(left / 86_400_000);
  const hours = Math.floor((left % 86_400_000) / 3_600_000);
  return `还剩 ${days} 天 ${hours} 小时`;
}

/** 快到期 / 已过期时用醒目色，其余保持安静。 */
function remainToneClass(remainingDays: number): string {
  if (remainingDays < 0) return "text-xs font-medium text-destructive";
  if (remainingDays <= 7) return "text-xs font-medium text-amber-600";
  return "text-xs text-muted-foreground";
}
