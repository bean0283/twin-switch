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
import { AccountTokenDialog } from "@/components/account-token-dialog";
import { CheckinButton } from "@/components/checkin-button";
import {
  CreditBlock as CreditBlockView,
  CreditsDialog as CreditsDialogView,
  type CreditPack,
  type CreditView,
} from "@/components/credits-ui";
import { Button } from "@/components/ui/button";
import {
  qoderAccountsExport,
  qoderAccountsList,
  qoderAccountRefresh,
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
  /**
   * 积分包明细（上游 `userQuota` / `addOnQuota` 两块，各算一个包）。
   * 空数组 = 还没查过额度 / 旧数据（查询后就会被填上）。
   */
  credit_parts?: { name: string; total: number; used: number; remaining: number }[];
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

/**
 * Qoder 额度 → 统一展示模型（`credits-ui`，与 Trae / WorkBuddy 共用同一个组件）。
 *
 * 差异只有两点：
 * 1. 积分包 = 上游的**两块**（`userQuota` 用户额度 / `addOnQuota` 加购额度），不是一长串资源；
 * 2. **到期时间上游不给** —— `expireAt` 一律 `null`（界面显示「未提供」），绝不编一个出来。
 */
function qoderToView(a: QoderAccount): CreditView {
  if (a.credits == null) {
    // 还没查过：这是「不知道」，不是「0 分」，也不该长得像出错。
    return {
      ok: true,
      unavailable: "还没查到额度：点上面的「刷新积分」",
      remaining: null,
      packCount: 0,
      packs: [],
    };
  }
  // 到期时间来自上游顶层 `expiresAt`（整个套餐周期共用），由后端写进 credits_expire_at。
  // 没有才说「未提供」——**不能一律说未提供**（2026-10-25 之前就是这么错的）。
  const exp = a.credits_expire_at ?? null;
  const parts = a.credit_parts ?? [];
  // 老数据（还没重新查过）没有分块明细：用合计出一行「额度合计」，
  // 而不是显示「暂无可用积分」——额度明明已经查到了，说没有就是骗人。
  const packs: CreditPack[] =
    parts.length > 0
      ? parts.map((p, i) => ({
          key: `${a.id}-${i}`,
          name: p.name || "积分包",
          // 0 总量当成「没有额度信息」，不要显示成 0/0
          total: p.total > 0 ? p.total : null,
          used: p.used,
          remaining: p.remaining,
          expireAt: exp,
          expiryUnavailable: exp == null,
        }))
      : [
          {
            key: `${a.id}-total`,
            name: "额度合计（未分块）",
            total: null,
            used: 0,
            remaining: a.credits,
            expireAt: exp,
            expiryUnavailable: exp == null,
          },
        ];
  const totalSum = parts.reduce((n, p) => n + (p.total || 0), 0);
  return {
    ok: true,
    remaining: a.credits,
    total: totalSum > 0 ? totalSum : null,
    packCount: packs.filter((p) => (p.remaining ?? 0) > 0).length,
    packs,
    updatedAt: a.credits_updated_at ?? null,
    expiryUnavailable: exp == null,
    notes: parts.length === 0 ? ["点「刷新积分」可看到分块明细"] : [],
  };
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

  /** 打开令牌弹窗的账号（弹窗本身是共用的 `AccountTokenDialog`）。 */
  const [dialog, setDialog] = useState<QoderAccount | null>(null);

  const fileRef = useRef<HTMLInputElement | null>(null);
  /** 已自动查过额度的区域（每个区域只自动查一次，避免 reload 后自我循环）。 */
  const autoRefreshedRef = useRef<string>("");

  /** 展开「全部积分包」的账号（与 Trae / WorkBuddy 同一个弹窗）。 */
  const [expand, setExpand] = useState<QoderAccount | null>(null);
  /** 正在单独刷新的账号 id（该卡片积分块里的按钮转圈）。 */
  const [refreshingId, setRefreshingId] = useState<string | null>(null);

  const reload = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const [rs, list] = await Promise.all([qoderRegions(), qoderAccountsList()]);
      setRegions(rs as Region[]);
      setRows(Array.isArray(list) ? (list as QoderAccount[]) : []);
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

  // 进页面 / 切区域时自动查一次额度。
  //
  // 为什么必须有：Trae 与 WorkBuddy 的账号页进页面就会自己查一次积分；Qoder 原来只有
  // 手动「刷新积分」一个入口，于是升级后看到的还是旧数据 —— 卡片会显示
  // 「额度合计（未分块）」，看起来就像「没有积分包」。
  // 节流：同一区域只自动触发一次；10 分钟内查过的不重复打接口。
  useEffect(() => {
    if (busy || refreshingAll || shown.length === 0) return;
    if (autoRefreshedRef.current === tab) return;
    const stale = shown.some(
      (r) =>
        r.credits == null ||
        !r.credits_updated_at ||
        Date.now() - r.credits_updated_at > 10 * 60_000,
    );
    if (!stale) return;
    autoRefreshedRef.current = tab;
    void refreshAll();
    // refreshAll / shown 每次渲染都是新引用，故意只依赖这几个「事件级」的量。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tab, shown, busy, refreshingAll]);

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

  /**
   * 只刷新**一个**账号的额度（卡片积分块里的小刷新按钮）。
   *
   * 与 `refreshAll` 分开：整批会把整个区域的账号串行查一遍（每个账号一次接口，
   * 还要防频控）；点一个号就只该等那一个号。成功后**原地替换**该条，
   * 不整批重拉 —— 其它账号的数字和时间戳不会被无关地改动。
   */
  async function refreshOne(a: QoderAccount) {
    if (refreshingId) return; // 连点保护
    setRefreshingId(a.id);
    setError(null);
    try {
      const up = (await qoderAccountRefresh(a.region, a.id)) as QoderAccount;
      if (!up || up.id !== a.id) {
        toast.error("这个账号的额度没刷到", { description: "接口没有返回该账号的最新额度" });
        return;
      }
      setRows((prev) => prev.map((r) => (r.id === up.id ? { ...r, ...up } : r)));
    } catch (e) {
      // 失败保留旧数字，但必须说出来 —— 静默不动会让用户以为刷新没生效。
      toast.error("额度查询失败", { description: e instanceof Error ? e.message : String(e) });
    } finally {
      setRefreshingId(null);
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
                identityTitle={
                  // 「上次使用」原来单独占卡片底部一行（Qoder 独有）—— 那正是与另两家的
                  // 布局差异。并进悬停提示后信息不丢，卡片主体只剩积分块，三页结构一致。
                  a.last_used_at
                    ? `${a.uid}\n上次使用 ${fmtTime(a.last_used_at)}`
                    : a.uid
                }
                onAvatarClick={() => setDialog(a)}
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
                {/* 与 Trae / WorkBuddy 同一个积分块：大数字 + 包数 + 每包剩余/到期 + 「全部积分包」 */}
                <CreditBlockView
                  item={qoderToView(a)}
                  onExpand={() => setExpand(a)}
                  onRefresh={() => void refreshOne(a)}
                  refreshing={refreshingId === a.id}
                  refreshHint="重新查询这个账号的额度"
                />
              </AccountCard>
            );
          })}
        </div>
      )}

      {/* 令牌弹窗：**与 Trae / WorkBuddy 同一个组件**（原来这里是手写的 fixed 遮罩，
          遮罩、按钮、打码、续期门槛全都是第二套实现 —— 三页各一套就是这次要消灭的）。
          Qoder 独有的「设置 Cosy 设备令牌」通过 `extraFooter` 插槽挂在「关闭」左边，
          不额外造一个弹窗。 */}
      <AccountTokenDialog
        open={dialog !== null}
        onOpenChange={(v) => {
          if (!v) setDialog(null);
        }}
        provider="qoder"
        targetKey={dialog?.region ?? null}
        accountId={dialog?.id ?? null}
        accountName={dialog ? dialog.name || dialog.uid : null}
        extraFooter={
          dialog ? (
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
          ) : null
        }
      />

      {/* 全部积分包（与 Trae / WorkBuddy 同一个弹窗） */}
      <CreditsDialogView
        item={expand ? qoderToView(expand) : null}
        name={expand ? accountName(expand) : ""}
        onClose={() => setExpand(null)}
      />

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
