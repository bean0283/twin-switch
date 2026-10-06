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

  // 组件卸载时停掉轮询
  useEffect(
    () => () => {
      if (pollRef.current !== null) window.clearInterval(pollRef.current);
    },
    [],
  );

  const openSwitch = async (acc: WorkbuddyAccount) => {
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
      toast.success(`已删除 ${acc.name}`);
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

  /** uid → 积分结果。按 uid 匹配：本工具库的 `id` 与账号库一致，参考库的 id 形如 `ref:<uid>`。 */
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

  /** 参考工具库里独有、本工具账号库里没有的账号（只读列出，不能切换）。 */
  const foreignCredits = useMemo(() => {
    const own = new Set(accounts.map((a) => a.uid.toLowerCase()).filter(Boolean));
    return (credits.result?.accounts ?? []).filter(
      (it) => it.account.uid && !own.has(it.account.uid.toLowerCase()),
    );
  }, [accounts, credits.result]);

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
          <AlertTitle>读取失败</AlertTitle>
          <AlertDescription>{error}</AlertDescription>
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
      ) : accounts.length === 0 ? (
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

      {/* 只在参考工具库里、本工具账号库没有的账号：只读展示积分，不能切换 */}
      {!loading && foreignCredits.length > 0 ? (
        <section className="mt-8 space-y-3">
          <div>
            <h2 className="text-base font-semibold">仅存在于参考工具库的账号</h2>
            <p className="mt-0.5 text-xs text-muted-foreground">
              这些账号只在 <code className="rounded bg-muted px-1">~/.wb-switch/accounts.json</code> 里，
              本工具账号库没有 —— 只读展示积分，不能在这里切换。需要的话先「导入本机登录态」或「扫码添加账号」。
            </p>
          </div>
          <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-3">
            {foreignCredits.map((it) => (
              <AccountCard
                key={it.account.id}
                name={it.account.name}
                identity={`uid ${shortUid(it.account.uid)}`}
                identityTitle={it.account.uid}
                identityMono
                chips={<Chip tone="outline">参考工具库 · 只读</Chip>}
              >
                <CreditBlock item={it} loading={creditsLoading} onExpand={() => credits.setExpand(it)} />
              </AccountCard>
            ))}
          </div>
        </section>
      ) : null}

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

      {/* 切换确认 */}
      <Dialog
        open={switchTarget !== null}
        onOpenChange={(o) => {
          if (!o) setSwitchTarget(null);
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
            </div>
          )}
          <DialogFooter>
            <Button variant="ghost" onClick={() => setSwitchTarget(null)}>
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
