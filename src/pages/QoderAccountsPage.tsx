import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowLeftRight,
  CheckCircle2,
  Download,
  ExternalLink,
  FileInput,
  Loader2,
  LogIn,
  Pencil,
  RefreshCw,
  Trash2,
  Upload,
  XCircle,
} from "lucide-react";
import { openUrl as openExternal } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";

import { AccountCard, Chip, IconAction } from "@/components/account-card";
import { CheckinButton } from "@/components/checkin-button";
import { Button } from "@/components/ui/button";
import {
  qoderAccountsExport,
  qoderAccountsList,
  qoderAccountsRefreshAll,
  qoderAccountRemove,
  qoderAccountRename,
  qoderAccountSwitch,
  qoderImportApply,
  qoderImportLocal,
  qoderImportPreview,
  qoderLoginCancel,
  qoderLoginPoll,
  qoderLoginStart,
  qoderRegions,
  qoderTokenRefresh,
  qoderCreditHistory,
  traeRevealPath,
  qoderAccountSetCosy,
} from "@/lib/api";
import { cn } from "@/lib/utils";
import { copyText } from "@/lib/clipboard";

type Region = { region: string; label: string; domain: string; cli_dir: string };

type QoderAccount = {
  id: string;
  uid: string;
  name: string;
  email: string | null;
  region: "cn" | "global";
  source: string;
  access_token: string | null;
  refresh_token: string | null;
  expires_at: number | null;
  plan: string | null;
  credits: number | null;
  credits_expire_at: number | null;
  credits_updated_at: number | null;
  last_used_at: number | null;
  disabled: boolean;
  /** 桌面端登录态里的 user.id 与本条 uid 相同（后端解 auth.v1.dat 得出，解不开就 false）。 */
  isCurrent?: boolean;
  /** 客户端自己写下的明文回显（`~/.qoder-cn/.qoder-app-status.json`）。 */
  appStatus?: {
    loggedIn: boolean;
    name: string | null;
    avatarUrl: string | null;
    version: string | null;
    snapshotAt: string | null;
  } | null;
};

/**
 * 卡片上显示的名字。
 *
 * 兜底顺序：账号库里的名字 → **只有「当前登录」那条**才用客户端回显里的名字 → 邮箱 → uid 尾号。
 * ⚠️ 回显里的名字属于**当前登录的那个号**，绝不能给其它账号当名字用（会张冠李戴）。
 */
function accountName(a: QoderAccount): string {
  if (a.name?.trim()) return a.name.trim();
  if (a.isCurrent && a.appStatus?.name?.trim()) return a.appStatus.name.trim();
  if (a.email?.trim()) return a.email.trim();
  return `uid …${a.uid.slice(-6)}`;
}

/** 只留前 6 / 后 4，中间打码 —— 截图发出去也不会泄露。 */
function mask(v: string | null): string {
  if (!v) return "—";
  if (v.length <= 12) return "•".repeat(v.length);
  return `${v.slice(0, 6)}…${v.slice(-4)}`;
}

const money = (v: number | null) =>
  v == null
    ? "—"
    : v.toLocaleString("zh-CN", { minimumFractionDigits: 2, maximumFractionDigits: 2 });

const pad = (n: number) => String(n).padStart(2, "0");

function fmtTime(ms: number | null): string {
  if (!ms) return "—";
  const d = new Date(ms);
  return `${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fmtAgo(ms: number | null): string {
  if (!ms) return "还没查过";
  const diff = Date.now() - ms;
  if (diff < 60_000) return "刚刚更新";
  if (diff < 3_600_000) return `${Math.round(diff / 60_000)} 分钟前更新`;
  if (diff < 86_400_000) return `${Math.round(diff / 3_600_000)} 小时前更新`;
  return `${Math.round(diff / 86_400_000)} 天前更新`;
}

export default function QoderAccountsPage() {
  const [regions, setRegions] = useState<Region[]>([]);
  const [rows, setRows] = useState<QoderAccount[]>([]);
  const [tab, setTab] = useState<"cn" | "global">("cn");
  const [busy, setBusy] = useState(false);
  const [refreshingAll, setRefreshingAll] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [errorOpen, setErrorOpen] = useState(false);

  const [login, setLogin] = useState<{ loginId: string; url: string } | null>(null);
  const [loginState, setLoginState] = useState<"idle" | "pending" | "ok" | "error">("idle");
  const [loginError, setLoginError] = useState<string | null>(null);

  const [dialog, setDialog] = useState<QoderAccount | null>(null);
  const [showSecret, setShowSecret] = useState(false);
  const [refreshingToken, setRefreshingToken] = useState(false);

  const fileRef = useRef<HTMLInputElement | null>(null);

  /** 近 7 天消耗（本机快照算的，不是官方用量）。 */
  type Hist = Awaited<ReturnType<typeof qoderCreditHistory>>;
  const [history, setHistory] = useState<Hist | null>(null);

  const reload = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const [rs, list] = await Promise.all([qoderRegions(), qoderAccountsList()]);
      setRegions(rs as Region[]);
      setRows(Array.isArray(list) ? (list as QoderAccount[]) : []);
      // 历史是**附加信息**：拉不到不影响账号列表，也不弹错（空态由卡片自己说）。
      void qoderCreditHistory(7)
        .then(setHistory)
        .catch(() => setHistory(null));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const shown = useMemo(() => rows.filter((r) => r.region === tab), [rows, tab]);
  const currentRegion = regions.find((r) => r.region === tab);

  const summary = useMemo(() => {
    const queried = shown.filter((r) => r.credits != null);
    const sum = queried.reduce((n, r) => n + (r.credits ?? 0), 0);
    const latest = shown.reduce<number | null>(
      (m, r) =>
        r.credits_updated_at && (!m || r.credits_updated_at > m) ? r.credits_updated_at : m,
      null,
    );
    return { queried: queried.length, sum, latest };
  }, [shown]);

  // ---------------------------------------------------------------- 动作

  /** 从本机 Qoder 客户端导入登录态 —— 自动识别国内版 / 国际版。 */
  async function doImportLocal() {
    setError(null);
    try {
      const r = await qoderImportLocal();
      await reload();

      const lines: string[] = [];
      const failures: string[] = [];
      const picked: [("cn" | "global"), string][] = [
        ["cn", "国内版"],
        ["global", "国际版"],
      ];
      for (const [key, label] of picked) {
        const x = r[key];
        if (!x) continue;
        if (x.imported || x.updated) {
          const who = x.accounts.map((a) => a.name || a.uid).join("、");
          lines.push(
            `${label}：新增 ${x.imported}、更新 ${x.updated}${who ? `（${who}）` : ""}${
              x.failed.length ? `，${x.failed.length} 个令牌失效` : ""
            }`,
          );
          for (const f of x.failed) failures.push(`${label} ${f.error}`);
        } else {
          lines.push(`${label}：${x.reason ?? "没有可导入的账号"}`);
        }
      }

      if (failures.length) {
        // 导入成功但令牌失效：必须说清楚，不能只报「导入成功」。
        setError(`${lines.join("；")}。失效原因：${failures.join("；")}`);
      } else {
        toast.success("已导入本机账号", { description: lines.join("；") });
      }
    } catch (e) {
      setError(String(e));
    }
  }

  /** 发起浏览器登录：拿授权页地址 → 打开 → 每 2 秒轮询一次。 */
  async function startLogin() {
    setError(null);
    setLoginError(null);
    setLoginState("pending");
    try {
      const r = await qoderLoginStart(tab);
      setLogin({ loginId: r.loginId, url: r.verificationUri });
      void openExternal(r.verificationUri).catch(() => {});
    } catch (e) {
      setLoginState("error");
      setLoginError(String(e));
    }
  }

  async function cancelLogin() {
    if (login) await qoderLoginCancel(login.loginId).catch(() => false);
    setLogin(null);
    setLoginState("idle");
    setLoginError(null);
  }

  useEffect(() => {
    if (!login) return;
    let stopped = false;
    const timer = window.setInterval(async () => {
      if (stopped) return;
      try {
        const r = await qoderLoginPoll(tab, login.loginId);
        if (stopped) return;
        if (r.done) {
          stopped = true;
          window.clearInterval(timer);
          if (r.error) {
            setLoginState("error");
            setLoginError(r.error);
          } else {
            setLoginState("ok");
            setLogin(null);
            await reload();
          }
        }
      } catch (e) {
        if (!stopped) {
          setLoginState("error");
          setLoginError(String(e));
        }
      }
    }, 2000);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [login, tab, reload]);

  /** 切换：把凭据写回 Qoder 桌面端登录态（auth.v1.dat）。 */
  async function doSwitch(a: QoderAccount) {
    setError(null);
    const ver = a.region === "cn" ? "国内版" : "国际版";
    if (
      !confirm(
        `把「${a.name || a.uid}」切换为当前 ${ver} Qoder 登录账号？\n\n` +
          `会自动关闭 ${ver} Qoder（未保存的对话会丢），写好登录态后自动重新打开；` +
          `原登录态会先备份。另一个版本不受影响。\n\n` +
          `注意：Qoder 的会话历史不按账号隔离（main.sqlite 里没有账号字段），` +
          `切完之后两个账号看到的是同一批历史 —— 这是客户端本身的行为，不是本工具改的。`,
      )
    ) {
      return;
    }
    try {
      const r = await qoderAccountSwitch(a.region, a.id);
      toast.success("已切换账号", { description: r.note });
      await reload();
    } catch (e) {
      setError(String(e));
    }
  }

  /** 修改：只改显示名。 */
  async function doRename(a: QoderAccount) {
    const next = window.prompt("新的显示名", a.name || "");
    if (next == null) return;
    const name = next.trim();
    if (!name) return;
    setError(null);
    try {
      await qoderAccountRename(a.region, a.id, name);
      toast.success("名字已更新");
      await reload();
    } catch (e) {
      setError(String(e));
    }
  }

  /** 刷新 token 有效期（用 refresh_token 换新 token）。 */
  async function doRefreshToken(a: QoderAccount) {
    setRefreshingToken(true);
    setError(null);
    try {
      const updated = (await qoderTokenRefresh(a.region, a.id)) as QoderAccount;
      setDialog(updated);
      toast.success("令牌已刷新");
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setRefreshingToken(false);
    }
  }

  async function removeOne(a: QoderAccount) {
    if (!confirm(`把 ${a.name || a.uid} 移到回收站？之后可以恢复。`)) return;
    setBusy(true);
    try {
      await qoderAccountRemove(a.region, a.id);
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function refreshAll() {
    setRefreshingAll(true);
    setError(null);
    try {
      const r = (await qoderAccountsRefreshAll(tab)) as {
        ok: number;
        failed: { uid: string; error: string }[];
        total: number;
      };
      await reload();
      if (r.failed?.length) {
        setError(
          `${r.ok}/${r.total} 个账号查到额度；失败：${r.failed
            .slice(0, 2)
            .map((f) => `${f.uid}（${f.error}）`)
            .join("、")}`,
        );
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setRefreshingAll(false);
    }
  }

  /** 导出账号包：落到 ~/.twin-switch/qoder/exports/，并把 JSON 放进剪贴板。 */
  async function exportPack() {
    setError(null);
    try {
      const r = await qoderAccountsExport(tab);
      // 剪贴板是「附带方便」，写不进去不该影响导出结论（也不能弹成功地说复制了）
      const copied = await copyText(r.json);
      toast.success(`已导出 ${r.count} 个账号`, {
        description: `${r.path}${copied ? "（JSON 已复制到剪贴板）" : ""}`,
      });
      // 导出完直接定位到文件 —— 用户下一步就是把它拖走 / 发出去，
      // 不该再让他自己去 `~/.twin-switch/qoder/exports/` 里翻。
      void traeRevealPath(r.path).catch(() => {});
    } catch (e) {
      toast.error("导出失败", { description: String(e) });
    }
  }

  /** 选文件 → 预览（脱敏）→ 确认 → 全量导入。 */
  async function importPack(file: File) {
    setError(null);
    try {
      const text = await file.text();
      const preview = await qoderImportPreview(tab, text);
      const head = preview.items
        .slice(0, 3)
        .map((i) => `${i.name || i.uid}${i.hasToken ? "" : "（无凭据）"}`)
        .join("、");
      const rejectedNote = preview.rejected.length
        ? `\n另有 ${preview.rejected.length} 条跨区/无效条目会被拒绝。`
        : "";
      if (
        !confirm(
          `这份文件共 ${preview.total} 条，可导入 ${preview.items.length} 条：${head}${
            preview.items.length > 3 ? "…" : ""
          }${rejectedNote}\n\n继续导入？`,
        )
      ) {
        return;
      }
      const r = await qoderImportApply(tab, text, null);
      toast.success(`导入完成：共 ${r.imported + r.updated} 条`, {
        description: `新增 ${r.imported}、更新 ${r.updated}、跳过 ${r.skipped}、拒绝 ${r.rejected.length}`,
      });
      await reload();
    } catch (e) {
      setError(String(e));
    }
  }

  // ---------------------------------------------------------------- 渲染

  return (
    <div className="mx-auto flex max-w-6xl flex-col gap-5 px-6 py-6">
      <header>
        <h1 className="text-xl font-semibold">Qoder 账号管理</h1>
        <p className="mt-1 text-xs text-muted-foreground">
          {currentRegion
            ? `${currentRegion.label} · 账号库 ~/.twin-switch/qoder/accounts.${currentRegion.region}.json · CLI 目录 ~/${currentRegion.cli_dir}`
            : "国内版与国际版是两套账号体系，分开存"}
        </p>
      </header>

      <div className="flex flex-wrap items-center gap-2">
        <Button variant="outline" size="sm" onClick={() => void doImportLocal()} className="gap-1.5">
          <FileInput className="size-4" />
          导入本机账号
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={() => void startLogin()}
          disabled={loginState === "pending"}
          className="gap-1.5"
        >
          {loginState === "pending" ? (
            <Loader2 className="size-4 animate-spin" />
          ) : (
            <LogIn className="size-4" />
          )}
          浏览器登录
        </Button>
        <Button variant="outline" size="sm" onClick={() => void exportPack()} className="gap-1.5">
          <Download className="size-4" />
          导出账号包
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={() => fileRef.current?.click()}
          className="gap-1.5"
        >
          <Upload className="size-4" />
          导入账号包
        </Button>
        <input
          ref={fileRef}
          type="file"
          accept=".json,application/json"
          className="hidden"
          onChange={(e) => {
            const f = e.target.files?.[0];
            e.target.value = "";
            if (f) void importPack(f);
          }}
        />
        {/* 与 Trae / WorkBuddy 同一个组件、同一个命令，弹窗天然一致 */}
        <CheckinButton scope="qoder" onSuccess={() => void reload()} />
      </div>

      {(login || loginState === "error" || loginState === "ok") && (
        <section className="rounded-2xl border border-primary/30 bg-primary/5 px-5 py-4">
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <h2 className="text-sm font-semibold">
                {loginState === "ok"
                  ? "登录成功"
                  : loginState === "error"
                    ? "登录未完成"
                    : "在浏览器里完成登录"}
              </h2>
              <p className="mt-0.5 text-xs text-muted-foreground">
                {loginState === "ok"
                  ? "账号已写入当前区域，卡片马上出现。"
                  : loginState === "error"
                    ? "失败原因见下；可以重新点「浏览器登录」再试一次。"
                    : "浏览器打开后登录并选择账号；这个面板会自动检测，不用手点确认。"}
              </p>
            </div>
            <button
              type="button"
              onClick={() => void cancelLogin()}
              className="shrink-0 rounded-lg border px-2.5 py-1 text-xs hover:bg-accent"
            >
              关闭
            </button>
          </div>

          {loginState === "error" && loginError && (
            <p className="mt-2 break-all text-xs text-destructive">{loginError}</p>
          )}
          {loginState === "ok" && (
            <p className="mt-2 inline-flex items-center gap-1.5 text-xs text-emerald-600">
              <CheckCircle2 className="size-3.5" />
              已添加
            </p>
          )}

          {login && (
            <div className="mt-3 flex flex-wrap items-center gap-2">
              <code className="min-w-0 flex-1 truncate rounded-lg border bg-background px-2.5 py-1.5 font-mono text-[11px]">
                {login.url}
              </code>
              <Button
                size="sm"
                variant="outline"
                className="gap-1.5"
                onClick={() => void openExternal(login.url).catch(() => {})}
              >
                <ExternalLink className="size-4" />
                打开浏览器
              </Button>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => void navigator.clipboard.writeText(login.url)}
              >
                复制链接
              </Button>
              <span className="inline-flex items-center gap-1.5 text-[11px] text-muted-foreground">
                {loginState === "pending" && <Loader2 className="size-3 animate-spin" />}
                等待授权…
              </span>
            </div>
          )}
        </section>
      )}

      {error && (
        <div className="rounded-xl border border-destructive/30 bg-destructive/5 px-4 py-3 text-xs text-destructive">
          <div className="flex items-start gap-2">
            <XCircle className="mt-0.5 size-3.5 shrink-0" />
            <span className={cn("min-w-0 break-all", !errorOpen && "line-clamp-2")}>{error}</span>
          </div>
          {error.length > 90 && (
            <button
              type="button"
              onClick={() => setErrorOpen((v) => !v)}
              className="mt-1.5 ml-5 rounded-md border border-destructive/30 px-2 py-0.5 text-[11px] hover:bg-destructive/10"
            >
              {errorOpen ? "收起" : "展开完整报错"}
            </button>
          )}
        </div>
      )}

      {/* 概览行：区域切换 + 汇总 */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <div className="flex gap-1">
          {(
            regions.length
              ? regions
              : [{ region: "cn", label: "Qoder CN", domain: "", cli_dir: "" }]
          ).map((r) => (
            <button
              key={r.region}
              type="button"
              onClick={() => setTab(r.region as "cn" | "global")}
              className={cn(
                "rounded-lg px-2.5 py-1 text-xs",
                tab === r.region
                  ? "bg-primary/10 font-medium text-foreground"
                  : "text-muted-foreground hover:bg-accent",
              )}
            >
              {r.label}
              <span className="ml-1.5 text-[11px] text-muted-foreground">
                {rows.filter((x) => x.region === r.region).length}
              </span>
            </button>
          ))}
        </div>

        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
          <span>
            合计剩余 <b className="text-foreground">{money(summary.sum)}</b>
          </span>
          <span>·</span>
          <span>
            {summary.queried}/{shown.length} 个账号可查
          </span>
          <span>·</span>
          <span>{fmtAgo(summary.latest)}</span>
        </div>

        <button
          type="button"
          onClick={() => void refreshAll()}
          disabled={refreshingAll || busy || shown.length === 0}
          className="ml-auto inline-flex items-center gap-1.5 rounded-lg border px-2.5 py-1.5 text-xs hover:bg-accent disabled:opacity-50"
        >
          {refreshingAll || busy ? (
            <Loader2 className="size-3.5 animate-spin" />
          ) : (
            <RefreshCw className="size-3.5" />
          )}
          刷新积分
        </button>
      </div>

      {/* 近 7 天消耗：数据只来自本机快照（Qoder 没有用量接口），样本不足就明说 */}
      {history && (
        <section className="rounded-2xl border bg-card px-5 py-4">
          <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
            <h2 className="text-sm font-semibold">近 {history.windowDays} 天消耗</h2>
            <span className="text-xs text-muted-foreground">
              本机快照算出来的（Qoder 没有用量接口）
            </span>
            {history.enoughData && history.days.length > 0 ? (
              <span className="ml-auto font-mono text-sm">{money(history.totalUsed)}</span>
            ) : (
              <span className="ml-auto text-xs text-muted-foreground">样本不足</span>
            )}
          </div>

          {history.enoughData && history.days.length > 0 ? (
            <>
              <div className="mt-3 flex items-end gap-1.5">
                {history.days.map((d) => {
                  const max = Math.max(...history.days.map((x) => x.used), 1);
                  return (
                    <div key={d.day} className="flex min-w-0 flex-1 flex-col items-center gap-1">
                      <div className="flex h-16 w-full items-end">
                        <div
                          className="w-full rounded-t bg-primary/60"
                          style={{ height: `${Math.max(2, (d.used / max) * 100)}%` }}
                          title={`${d.day}：${money(d.used)}`}
                        />
                      </div>
                      <span className="text-[10px] text-muted-foreground">{d.day}</span>
                    </div>
                  );
                })}
              </div>
              {history.accounts.length > 0 && (
                <div className="mt-3 space-y-1 border-t border-border/60 pt-2">
                  {history.accounts.map((x) => (
                    <div key={x.uid} className="flex items-center gap-2 text-xs">
                      <span className="min-w-0 flex-1 truncate">{x.name}</span>
                      <span className="font-mono text-muted-foreground">{money(x.used)}</span>
                    </div>
                  ))}
                </div>
              )}
              {history.resetEvents > 0 && (
                <p className="mt-2 text-[11px] text-muted-foreground">
                  有 {history.resetEvents} 次额度上升（充值 / 签到 / 周期重置），这些不计入消耗。
                </p>
              )}
            </>
          ) : (
            <p className="mt-2 text-xs text-muted-foreground">
              {history.points > 0
                ? "采样点还不够两笔（算不出差值），多点几次「刷新积分」就会出曲线。"
                : "还没有采样点。点一次「刷新积分」开始记录（同一账号 10 分钟记一笔）。"}
            </p>
          )}
        </section>
      )}

      {shown.length === 0 ? (
        <section className="rounded-2xl border bg-card px-5 py-10 text-center text-sm text-muted-foreground">
          这个区域还没有账号。点上面「导入本机账号」，或用「浏览器登录」。
        </section>
      ) : (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3">
          {shown.map((a) => {
            const hasToken = (a.access_token ?? "").trim().length > 0;
            return (
              <AccountCard
                key={a.id}
                name={accountName(a)}
                identity={`uid ${a.uid}`}
                identityMono
                identityTitle={a.uid}
                onAvatarClick={() => {
                  setDialog(a);
                  setShowSecret(false);
                }}
                avatarLabel={`查看 ${a.name || a.uid} 的令牌`}
                chips={
                  <>
                    {a.isCurrent ? <Chip tone="ok">当前登录</Chip> : null}
                    <Chip tone="outline">{a.source}</Chip>
                    {a.plan ? <Chip tone="ok">{a.plan}</Chip> : null}
                    {!hasToken ? <Chip tone="warn">没有凭据</Chip> : null}
                  </>
                }
                actions={
                  <>
                    <IconAction
                      icon={<ArrowLeftRight className="size-4" />}
                      label="切换（写回 Qoder 客户端登录态）"
                      onClick={() => void doSwitch(a)}
                      disabled={!hasToken}
                    />
                    <IconAction
                      icon={<Pencil className="size-4" />}
                      label="修改显示名"
                      onClick={() => void doRename(a)}
                    />
                    <IconAction
                      icon={<Trash2 className="size-4" />}
                      label="删除"
                      destructive
                      onClick={() => void removeOne(a)}
                    />
                  </>
                }
              >
                <div className="flex items-baseline justify-between gap-2">
                  <span className="text-[22px] leading-none font-semibold tracking-tight">
                    {money(a.credits)}
                  </span>
                  <span className="text-[11px] text-muted-foreground">
                    {fmtAgo(a.credits_updated_at)}
                  </span>
                </div>

                <div className="mt-1 flex items-center justify-between text-[11px] text-muted-foreground">
                  <span>到期 {fmtTime(a.credits_expire_at)}</span>
                  <span>{a.credits == null ? "点「刷新积分」获取" : "Qoder 额度"}</span>
                </div>

                <p className="mt-2 text-[11px] text-muted-foreground">
                  上次使用 {fmtTime(a.last_used_at)}
                </p>
              </AccountCard>
            );
          })}
        </div>
      )}

      {dialog && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
          onClick={() => setDialog(null)}
        >
          <div
            className="w-full max-w-lg rounded-2xl border bg-card p-5 shadow-xl"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-start justify-between gap-3">
              <div>
                <h2 className="text-sm font-semibold">令牌与有效期</h2>
                <p className="mt-0.5 text-xs text-muted-foreground">{dialog.name || dialog.uid}</p>
              </div>
              <button
                type="button"
                onClick={() => setDialog(null)}
                className="rounded-lg border px-2.5 py-1 text-xs hover:bg-accent"
              >
                关闭
              </button>
            </div>

            <div className="mt-3 space-y-2 text-[11px]">
              <div>
                <div className="text-muted-foreground">Access token</div>
                <code className="mt-0.5 block break-all rounded-lg border bg-muted/30 px-2 py-1.5 font-mono">
                  {showSecret ? dialog.access_token || "—" : mask(dialog.access_token)}
                </code>
              </div>
              <div>
                <div className="text-muted-foreground">Refresh token</div>
                <code className="mt-0.5 block break-all rounded-lg border bg-muted/30 px-2 py-1.5 font-mono">
                  {showSecret ? dialog.refresh_token || "—" : mask(dialog.refresh_token)}
                </code>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">令牌来源</span>
                <span className="font-mono">{dialog.source}</span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">令牌到期</span>
                <span>{fmtTime(dialog.expires_at)}</span>
              </div>
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground">上次刷新</span>
                <span>{fmtTime(dialog.credits_updated_at)}</span>
              </div>
            </div>

            <div className="mt-4 flex flex-wrap items-center gap-2">
              <Button size="sm" variant="outline" onClick={() => setShowSecret((v) => !v)}>
                {showSecret ? "隐藏明文" : "显示明文"}
              </Button>
              <Button
                size="sm"
                onClick={() => void doRefreshToken(dialog)}
                disabled={refreshingToken}
                className="gap-1.5"
              >
                {refreshingToken ? (
                  <Loader2 className="size-4 animate-spin" />
                ) : (
                  <RefreshCw className="size-4" />
                )}
                刷新有效期
              </Button>
              {dialog.access_token && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => void navigator.clipboard.writeText(dialog.access_token ?? "")}
                >
                  复制 access token
                </Button>
              )}
              <Button
                size="sm"
                variant="ghost"
                title="抓包请求头里复制 cosy-machinetoken / cosy-machinetype 各一份；活动下发绑定这个身份"
                onClick={async () => {
                  const tk = window.prompt("粘贴 cosy-machinetoken（抓包请求头里整段复制）");
                  if (!tk?.trim()) return;
                  const ty = window.prompt("粘贴 cosy-machinetype（18 位）");
                  if (!ty?.trim()) return;
                  try {
                    await qoderAccountSetCosy(dialog.region, dialog.id, tk.trim(), ty.trim());
                    toast.success("Cosy 设备身份已保存", {
                      description: "之后签到 / 查活动会用它",
                    });
                    await reload();
                  } catch (e) {
                    setError(String(e));
                  }
                }}
              >
                设置 Cosy 设备令牌…
              </Button>
            </div>
          </div>
        </div>
      )}

      <section className="rounded-2xl border bg-card px-5 py-4">
        <h2 className="text-sm font-semibold">这个账号库是怎么存的</h2>
        <p className="mt-1 text-xs leading-relaxed text-muted-foreground">
          国内版与国际版分开落盘（<span className="font-mono">accounts.cn.json</span> /{" "}
          <span className="font-mono">accounts.global.json</span>），删除只是挪进回收站。token 只存在本机账号库里，
          不往任何第三方发送；「积分共享」按「谁先过期用谁」排序时会用到这里的额度。
        </p>
      </section>
    </div>
  );
}
