import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import {
  AlertTriangle,
  CheckCircle2,
  ExternalLink,
  FileDown,
  FileUp,
  Loader2,
  LogIn,
  PencilLine,
  Power,
  RotateCcw,
  Trash2,
  UserPlus,
} from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
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
import { Skeleton } from "@/components/ui/skeleton";
import { AccountCard, Chip, IconAction } from "@/components/account-card";
import {
  CreditBlock,
  CreditsDialog,
  CreditsHeader,
  useWorkbuddyCredits,
} from "@/components/workbuddy-credits";
import * as api from "@/lib/api";
import type {
  WbCreditItem,
  WorkbuddyAccount,
  WorkbuddyAccountList,
  WorkbuddyOAuthPoll,
  WorkbuddyOAuthStart,
  WorkbuddySwitchPrecheck,
  WorkbuddySwitchResult,
} from "@/lib/trae-types";

/**
 * 切换成功后确认窗自动关闭的延时（ms）。
 *
 * 这个窗在切换成功后已经没有可做的事（结果同时以 toast 留在屏幕上、下面的账号卡也已经
 * 刷成「当前登录」），但它会盖住整片账号区 —— 用户切完往往还要接着看积分 / 再切下一个。
 * 留 2.5 s 让他看清「结束 N 个进程 / 是否重启」，然后把位置让出来。
 */
const AUTO_CLOSE_MS = 2500;

/** 过期时间的人话描述。 */
function expiryLabel(expiresAt: number | null): string | null {
  if (!expiresAt) return null;
  const days = Math.floor((expiresAt - Date.now()) / 86400_000);
  if (days < 0) return "凭据已过期";
  if (days === 0) return "今天过期";
  if (days <= 7) return `${days} 天后过期`;
  return null;
}

/** uid 只显示首尾，中间省略（完整值挂在 title 上）。 */
function shortUid(uid: string): string {
  if (!uid) return "—";
  if (uid.length <= 18) return uid;
  return `${uid.slice(0, 8)}…${uid.slice(-6)}`;
}

/** 邮箱脱敏：本地段只留首字符（卡片上不做展示型泄漏）。 */
function maskEmail(email: string): string {
  const [local, domain] = email.split("@");
  if (!domain) return email;
  return `${local.slice(0, 1)}${"*".repeat(Math.max(3, local.length - 1))}@${domain}`;
}

/**
 * 单个账号卡：**账号身份 + 积分 + 操作按钮**三合一。
 *
 * 卡片外壳（头像 / 名称 / 身份 / 状态徽标 / 右上角操作栏）由
 * [`@/components/account-card`] 提供，与 Trae 侧的账号卡是同一套实现。
 */
function WorkbuddyAccountCard({
  acc,
  credit,
  creditError,
  creditsLoading,
  busy,
  onSwitch,
  onRename,
  onDelete,
  onExpand,
}: {
  acc: WorkbuddyAccount;
  credit: WbCreditItem | null;
  creditError: string | null;
  creditsLoading: boolean;
  busy: boolean;
  onSwitch: () => void;
  onRename: () => void;
  onDelete: () => void;
  onExpand: () => void;
}) {
  const exp = expiryLabel(acc.expiresAt);
  const identity = acc.email ? maskEmail(acc.email) : `uid ${shortUid(acc.uid)}`;
  return (
    <AccountCard
      name={acc.name}
      identity={identity}
      identityTitle={acc.email || acc.uid}
      identityMono={!acc.email}
      isCurrent={acc.isCurrent}
      chips={
        <>
          {acc.isCurrent ? <Chip tone="ok">当前登录</Chip> : null}
          {!acc.hasToken ? <Chip tone="warn">无凭据</Chip> : null}
          {exp ? <Chip tone="err">{exp}</Chip> : null}
        </>
      }
      actions={
        <>
          <IconAction
            icon={<Power className="size-4" />}
            label={acc.isCurrent ? "当前登录账号" : "切换到这个账号"}
            active={acc.isCurrent}
            spinning={busy}
            disabled={!acc.hasToken}
            onClick={onSwitch}
          />
          <IconAction icon={<PencilLine className="size-4" />} label="改显示名" onClick={onRename} />
          <IconAction
            icon={<Trash2 className="size-4" />}
            label="从本工具账号库删除"
            destructive
            onClick={onDelete}
          />
        </>
      }
    >
      <CreditBlock item={credit} error={creditError} loading={creditsLoading} onExpand={onExpand} />
    </AccountCard>
  );
}

export default function WorkbuddySwitchPage() {
  const [list, setList] = useState<WorkbuddyAccountList | null>(null);
  const [loading, setLoading] = useState(true);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // 切换确认
  const [switchTarget, setSwitchTarget] = useState<WorkbuddyAccount | null>(null);
  const [precheck, setPrecheck] = useState<WorkbuddySwitchPrecheck | null>(null);
  const [switchResult, setSwitchResult] = useState<WorkbuddySwitchResult | null>(null);

  /**
   * 切换成功后的自动关窗定时器。
   *
   * ⚠️ 必须能被「手动关闭 / 重新打开」打断：否则上一次残留的定时器会把**下一个**弹窗
   *    一起关掉（切完 A 又马上点 B，B 的窗会在 2.5 s 后自己消失）。
   */
  const autoCloseRef = useRef<number | null>(null);
  const clearAutoClose = useCallback(() => {
    if (autoCloseRef.current !== null) {
      window.clearTimeout(autoCloseRef.current);
      autoCloseRef.current = null;
    }
  }, []);
  const closeSwitch = useCallback(() => {
    clearAutoClose();
    setSwitchTarget(null);
  }, [clearAutoClose]);

  // 改名
  const [renameTarget, setRenameTarget] = useState<WorkbuddyAccount | null>(null);
  const [renameValue, setRenameValue] = useState("");

  // 删除
  const [deleteTarget, setDeleteTarget] = useState<WorkbuddyAccount | null>(null);

  // OAuth 扫码
  const [oauth, setOauth] = useState<WorkbuddyOAuthStart | null>(null);
  const [oauthNote, setOauthNote] = useState<string>("等待浏览器完成授权…");
  const pollRef = useRef<number | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await api.workbuddyAccountList();
      setList(data);
    } catch (e) {
      setError(api.asError(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 组件卸载时停掉轮询与自动关窗定时器
  useEffect(
    () => () => {
      if (pollRef.current !== null) window.clearInterval(pollRef.current);
      if (autoCloseRef.current !== null) window.clearTimeout(autoCloseRef.current);
    },
    [],
  );

  const openSwitch = async (acc: WorkbuddyAccount) => {
    clearAutoClose();
    setSwitchTarget(acc);
    setSwitchResult(null);
    setPrecheck(null);
    try {
      setPrecheck(await api.workbuddySwitchPrecheck(acc.id));
    } catch (e) {
      toast.error(api.asError(e));
    }
  };

  const runSwitch = async () => {
    const acc = switchTarget;
    if (!acc) return;
    setBusyId(acc.id);
    try {
      const r = await api.workbuddySwitchTo(acc.id, true);
      setSwitchResult(r);
      toast.success(`已切换到 ${acc.name}`);
      await load();
      // 切换已经结束，这扇窗没有剩余职责：留着只会挡住下面已经变成「当前登录」的账号卡。
      // 成功结果同时以 toast 形式留在屏幕上，关掉窗不会丢信息。
      autoCloseRef.current = window.setTimeout(() => {
        autoCloseRef.current = null;
        setSwitchTarget(null);
      }, AUTO_CLOSE_MS);
    } catch (e) {
      toast.error(api.asError(e));
    } finally {
      setBusyId(null);
    }
  };

  const runRollback = async () => {
    setBusyId("rollback");
    try {
      await api.workbuddyRollback();
      toast.success("已回滚到上一次切换前的登录态");
      await load();
    } catch (e) {
      toast.error(api.asError(e));
    } finally {
      setBusyId(null);
    }
  };

  const runImportLocal = async () => {
    setBusyId("import");
    try {
      await api.workbuddyImportLocal();
      toast.success("已导入本机 WorkBuddy 登录态");
      await load();
    } catch (e) {
      toast.error(api.asError(e));
    } finally {
      setBusyId(null);
    }
  };

  const runRemove = async () => {
    const acc = deleteTarget;
    if (!acc) return;
    try {
      await api.workbuddyRemoveAccount(acc.id);
      // ⚠️ 这句是给用户的定心丸：后端删完会**立墓碑**（`workbuddy-import-blocklist.json`），
      // 启动时的自动导入不会再把这个身份搬回来。2026-10-09 之前没有这条护栏，用户删掉一个
      // 账号、重启后它自己从参考库回来了 —— **删除等于没删**。
      //（之后用户主动扫码 / 导入账号包把它加回来时，墓碑会自动撤掉。）
      toast.success(`已删除 ${acc.name}`, {
        description: "以后启动时不会再自动导入这个账号",
      });
      setDeleteTarget(null);
      await load();
    } catch (e) {
      toast.error(api.asError(e));
    }
  };

  const runRename = async () => {
    const acc = renameTarget;
    if (!acc) return;
    try {
      await api.workbuddyRenameAccount(acc.id, renameValue);
      toast.success("已保存显示名");
      setRenameTarget(null);
      await load();
    } catch (e) {
      toast.error(api.asError(e));
    }
  };

  const runExport = async () => {
    setBusyId("export");
    try {
      const payload = await api.workbuddyExportAccounts();
      const text = JSON.stringify(payload, null, 2);
      const blob = new Blob([text], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `workbuddy-accounts-${Date.now()}.json`;
      a.click();
      URL.revokeObjectURL(url);
      toast.success("账号包已导出");
    } catch (e) {
      toast.error(api.asError(e));
    } finally {
      setBusyId(null);
    }
  };

  const runImportFile = async (file: File) => {
    setBusyId("import-file");
    try {
      const payload = JSON.parse(await file.text());
      const preview = await api.workbuddyPreviewImport(payload);
      const r = await api.workbuddyImportAccounts(payload, "merge");
      toast.success(
        `已导入：新增 ${r.created ?? 0} 个，更新 ${r.updated ?? 0} 个（预览 ${preview.count} 项）`,
      );
      await load();
    } catch (e) {
      toast.error(api.asError(e));
    } finally {
      setBusyId(null);
    }
  };

  // 注：原来这里有个 `runImportReference()`（手动搬家）。2026-10-09 起搬家里程移到
  // `main.tsx` 的启动编排里自动跑，界面上不再有入口，所以这个函数整段删掉了。

  // ---- OAuth 扫码 ----
  const startOauth = async () => {
    setBusyId("oauth");
    try {
      const r = await api.workbuddyOauthStart();
      setOauth(r);
      setOauthNote("等待浏览器完成授权…");
      await api.workbuddyOpenUrl(r.verificationUri);
      if (pollRef.current !== null) window.clearInterval(pollRef.current);
      pollRef.current = window.setInterval(() => void pollOauth(r.loginId), 2000);
    } catch (e) {
      toast.error(api.asError(e));
    } finally {
      setBusyId(null);
    }
  };

  const pollOauth = async (loginId: string) => {
    try {
      const r: WorkbuddyOAuthPoll = await api.workbuddyOauthPoll(loginId);
      if (!r.done) return;
      if (pollRef.current !== null) {
        window.clearInterval(pollRef.current);
        pollRef.current = null;
      }
      if (r.error) {
        setOauthNote(`登录失败：${r.error}`);
        toast.error(r.error);
      } else {
        setOauthNote(`登录完成：${r.result?.name ?? "新账号"}`);
        toast.success("账号已加入账号库");
        await load();
      }
    } catch (e) {
      setOauthNote(api.asError(e));
    }
  };

  const stopOauth = async () => {
    if (pollRef.current !== null) {
      window.clearInterval(pollRef.current);
      pollRef.current = null;
    }
    if (oauth) await api.workbuddyOauthStop(oauth.loginId);
    setOauth(null);
  };

  const accounts = list?.accounts ?? [];

  // ---- 积分：与账号卡片融合显示 ----
  const credits = useWorkbuddyCredits();

  /** uid → 积分查询失败原因。按 uid 匹配：积分账号本来就是本工具账号库里的账号。 */
  const creditByUid = useMemo(() => {
    const m = new Map<string, WbCreditItem>();
    for (const it of credits.result?.accounts ?? []) {
      m.set(it.account.uid.toLowerCase(), it);
    }
    return m;
  }, [credits.result]);

  /** uid → 查询失败原因。 */
  const creditErrorByUid = useMemo(() => {
    const m = new Map<string, string>();
    for (const e of credits.result?.errors ?? []) {
      if (e.uid) m.set(e.uid.toLowerCase(), e.error);
    }
    return m;
  }, [credits.result]);

  const creditsLoading = credits.result === null && credits.error === null;

  return (
    <div className="mx-auto w-full max-w-6xl px-4 py-6">
      <div className="mb-5 flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold">WorkBuddy 账号管理</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            国内版 · 登录态文件{" "}
            <code className="rounded bg-muted px-1 text-xs">
              {list?.authFilePath ?? "workbuddy-desktop.info"}
            </code>
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button variant="outline" size="sm" onClick={() => void runImportLocal()} disabled={busyId !== null}>
            {busyId === "import" ? <Loader2 className="size-4 animate-spin" /> : <LogIn className="size-4" />}
            导入本机登录态
          </Button>
          <Button variant="outline" size="sm" onClick={() => void startOauth()} disabled={busyId !== null}>
            {busyId === "oauth" ? <Loader2 className="size-4 animate-spin" /> : <UserPlus className="size-4" />}
            扫码添加账号
          </Button>
          {/* ⚠️ 这里曾经有个「导入参考工具账号」按钮，2026-10-09 起**删掉了** ——
              改为 `main.tsx` 启动时自动导入（幂等，只搬明文，本地已可用的不覆盖）。
              删按钮时**必须**同步改掉指着它的文案（积分卡片红字 + 首页注意事项），
              否则就是 T42 那个坑的反面：入口没了、提示还在叫用户去点。 */}
          <Button variant="outline" size="sm" onClick={() => void runExport()} disabled={busyId !== null}>
            <FileDown className="size-4" />
            导出账号包
          </Button>
          <Button variant="outline" size="sm" asChild>
            <label className="cursor-pointer">
              <FileUp className="size-4" />
              导入账号包
              <input
                type="file"
                accept="application/json"
                className="hidden"
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) void runImportFile(f);
                  e.target.value = "";
                }}
              />
            </label>
          </Button>
        </div>
      </div>

      {error && (
        <Alert variant="destructive" className="mb-4">
          <AlertTriangle className="size-4" />
          <AlertTitle>读取账号库失败</AlertTitle>
          <AlertDescription>
            <div className="break-all">{error}</div>
            {/* ⚠️ 这一句很关键：读取失败长得和「账号库是空的」一模一样，
                不说清楚会被当成账号丢了（2026-10-08 真的这么误会过一次）。 */}
            <div className="mt-1 text-xs opacity-80">
              这是<span className="font-medium">读取</span>失败，不是账号被删。账号库文件{" "}
              <code className="rounded bg-background/40 px-1">~/.twin-switch/workbuddy-accounts.json</code>{" "}
              不会被本操作改动，修好读取问题后账号就会回来。
            </div>
          </AlertDescription>
        </Alert>
      )}

      {oauth && (
        <Alert className="mb-4">
          <LogIn className="size-4" />
          <AlertTitle>扫码登录</AlertTitle>
          <AlertDescription className="space-y-2">
            <div>{oauthNote}</div>
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => void api.workbuddyOpenUrl(oauth.verificationUri)}>
                <ExternalLink className="size-4" />
                重新打开授权页
              </Button>
              <Button variant="ghost" size="sm" onClick={() => void stopOauth()}>
                取消
              </Button>
            </div>
            <div className="break-all font-mono text-xs text-muted-foreground">{oauth.verificationUri}</div>
          </AlertDescription>
        </Alert>
      )}

      {/* 顶部工具条：账号数 + 合计积分 + 刷新积分 */}
      <div className="mb-4">
        <CreditsHeader credits={credits} accountCount={accounts.length} />
      </div>

      {loading ? (
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          <Skeleton className="h-56 rounded-2xl" />
          <Skeleton className="h-56 rounded-2xl" />
        </div>
      ) : error ? null : accounts.length === 0 ? (
        <Card>
          <CardContent className="py-10 text-center text-sm text-muted-foreground">
            账号库还是空的。点「导入本机登录态」把当前登录的 WorkBuddy 账号收进来，或用「扫码添加账号」新增。
          </CardContent>
        </Card>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {accounts.map((acc) => {
            const key = acc.uid.toLowerCase();
            return (
              <WorkbuddyAccountCard
                key={acc.id}
                acc={acc}
                credit={creditByUid.get(key) ?? null}
                creditError={creditErrorByUid.get(key) ?? null}
                creditsLoading={creditsLoading}
                busy={busyId === acc.id}
                onSwitch={() => void openSwitch(acc)}
                onRename={() => {
                  setRenameTarget(acc);
                  setRenameValue(acc.name);
                }}
                onDelete={() => setDeleteTarget(acc)}
                onExpand={() => {
                  const it = creditByUid.get(key);
                  if (it) credits.setExpand(it);
                }}
              />
            );
          })}
        </div>
      )}

      <div className="mt-8">
        <Card>
          <CardHeader className="pb-3">
            <CardTitle className="text-base">回滚</CardTitle>
            <CardDescription>
              把登录态恢复成上一次切换前的那份（用切换前自动备份的 <code>workbuddy-desktop.*.info</code>）。
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Button variant="outline" onClick={() => void runRollback()} disabled={busyId !== null}>
              {busyId === "rollback" ? <Loader2 className="size-4 animate-spin" /> : <RotateCcw className="size-4" />}
              回滚到上一次切换前
            </Button>
          </CardContent>
        </Card>
      </div>

      {/* 切换确认。成功后由 `runSwitch` 里那个定时器自动关掉（见 `AUTO_CLOSE_MS`）。 */}
      <Dialog
        open={switchTarget !== null}
        onOpenChange={(o) => {
          if (!o) closeSwitch();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>切换到「{switchTarget?.name}」</DialogTitle>
            <DialogDescription>
              切换会结束 WorkBuddy 的全部进程，未保存的会话会中断；写登录态失败会自动还原备份。
            </DialogDescription>
          </DialogHeader>
          {precheck && (
            <div className="space-y-1 rounded-md border p-3 text-sm">
              <div className="flex justify-between">
                <span className="text-muted-foreground">客户端运行状态</span>
                <span>{precheck.running ? "运行中（将被结束）" : "未运行"}</span>
              </div>
              <div className="flex justify-between">
                <span className="text-muted-foreground">凭据</span>
                <span>{precheck.hasToken ? "可用" : "缺失，无法切换"}</span>
              </div>
              {precheck.alreadyCurrent && (
                <div className="flex items-center gap-1.5 text-amber-600">
                  <CheckCircle2 className="size-4" />
                  该账号已经是当前登录账号
                </div>
              )}
            </div>
          )}
          {switchResult && (
            <div className="rounded-md border border-emerald-500/40 bg-emerald-500/10 p-3 text-sm">
              切换完成：结束 {switchResult.killed.length} 个进程
              {switchResult.relaunched ? "，已重启客户端" : "，未重启客户端"}
              <span className="mt-1 block text-xs text-emerald-700/80">
                此窗口即将自动关闭（结果也会留在右下角提示里）。
              </span>
            </div>
          )}
          <DialogFooter>
            <Button variant="ghost" onClick={closeSwitch}>
              关闭
            </Button>
            <Button
              onClick={() => void runSwitch()}
              disabled={busyId !== null || !precheck?.hasToken || precheck?.alreadyCurrent}
            >
              确认切换
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 改名 */}
      <Dialog
        open={renameTarget !== null}
        onOpenChange={(o) => {
          if (!o) setRenameTarget(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>显示名</DialogTitle>
            <DialogDescription>只改本工具里的显示名，不会改动 WorkBuddy 账号本身的昵称。</DialogDescription>
          </DialogHeader>
          <Input value={renameValue} onChange={(e) => setRenameValue(e.target.value)} placeholder="留空则回落到昵称" />
          <DialogFooter>
            <Button variant="ghost" onClick={() => setRenameTarget(null)}>
              取消
            </Button>
            <Button onClick={() => void runRename()}>保存</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 删除 */}
      <Dialog
        open={deleteTarget !== null}
        onOpenChange={(o) => {
          if (!o) setDeleteTarget(null);
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>删除「{deleteTarget?.name}」？</DialogTitle>
            <DialogDescription>
              只从本工具的账号库里删除这条记录，<b>不会</b>动 WorkBuddy 的登录态与会话数据。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setDeleteTarget(null)}>
              取消
            </Button>
            <Button variant="destructive" onClick={() => void runRemove()}>
              删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 全部积分包 */}
      <CreditsDialog item={credits.expand} onClose={() => credits.setExpand(null)} />
    </div>
  );
}
