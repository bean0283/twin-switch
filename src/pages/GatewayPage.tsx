import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BookOpen, Copy, KeyRound, Loader2, RefreshCw, Server, Trash2, XCircle } from "lucide-react";

import { Link } from "react-router-dom";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";

import {
  gatewayKeyCreate,
  gatewayKeyDelete,
  gatewayKeyRestore,
  gatewayKeyRevoke,
  gatewayKeysList,
  gatewayModelsDiscover,
  gatewayPool,
  gatewayPreview,
  gatewayModelsGet,
  gatewaySetEnabled,
  gatewayStatus,
  gatewayUpstreamsGet,
  gatewayUpstreamsSet,
} from "@/lib/api";
import { cn } from "@/lib/utils";
import { copyText } from "@/lib/clipboard";

/** 池子里的一个账号（与后端 `PoolEntry` 同形）。 */
type PoolEntry = {
  key: string;
  product: string;
  scope: string;
  label: string;
  remaining: number | null;
  expires_at: number | null;
  usable: boolean;
  reason: string | null;
  cooldown_until: number | null;
  /// 按模型记的限额（后端 `gateway_pool::model_limits`）——只锁这几个模型，号还能用。
  limited_models: { model: string; reset_at: number; kind: string }[];
};

type KeyRow = {
  id: string;
  name: string;
  prefix: string;
  suffix: string;
  scopes: string[];
  revoked_at: number | null;
};

type Status = {
  running: boolean;
  port: number;
  requests: number;
  rejected: number;
  addr: string;
};

type Discovered = {
  scope: string;
  product: string;
  model: string;
  display: string;
  source: string;
};

type SourceReport = { product: string; scope: string; found: number; note: string };

type Preview = {
  model: string;
  candidates: string[];
  picked: PoolEntry | null;
  reason: string | null;
};

const PRODUCTS: Record<string, string> = { trae: "Trae", workbuddy: "WorkBuddy", qoder: "Qoder" };


function fmtExpiry(ms: number | null): string {
  if (!ms) return "—";
  const diff = ms - Date.now();
  if (diff <= 0) return "已过期";
  const h = Math.round(diff / 3_600_000);
  if (h >= 48) return `${Math.round(h / 24)} 天后`;
  if (h >= 1) return `${h} 小时后`;
  return `${Math.max(1, Math.round(diff / 60_000))} 分钟后`;
}

/// 限额恢复倒计时的人话：`5h39m 后恢复（22:04）`。
///
/// 口径照 changexbc/workbuddy-switch 的限额 chip：**剩余时长 + 几点恢复**都要给 ——
/// 只有「还剩 5 小时」看不出要不要等，只有「22:04」看不出还要等多久。
function fmtRecover(ms: number): string {
  const diff = ms - Date.now();
  if (diff <= 0) return "已恢复";
  const h = Math.floor(diff / 3_600_000);
  const m = Math.round((diff % 3_600_000) / 60_000);
  const left = h > 0 ? `${h}h${m}m` : `${Math.max(1, m)}m`;
  const at = new Date(ms).toLocaleTimeString("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
  return `${left} 后恢复（${at}）`;
}

/// 作用域 → 中文名 / 短标签 / 前缀别名（与后端 `gateway_pool::scope_alias` 一致）。
const SCOPE_LABEL: Record<string, string> = {
  "trae-cn": "Trae 国内",
  "trae-global": "Trae 国际",
  "wb-cn": "WorkBuddy 国内",
  "wb-global": "WorkBuddy 国际",
  "qoder-cn": "Qoder 国内",
  "qoder-global": "Qoder 国际",
};
const SCOPE_SHORT: Record<string, string> = {
  "trae-cn": "Trae",
  "trae-global": "Trae 国际",
  "wb-cn": "WB",
  "wb-global": "WB 国际",
  "qoder-cn": "Qoder",
  "qoder-global": "Qoder 国际",
};
const SCOPE_ALIAS: Record<string, string> = {
  "trae-cn": "trae",
  "trae-global": "trae-g",
  "wb-cn": "wb",
  "wb-global": "wb-g",
  "qoder-cn": "qoder",
  "qoder-global": "qoder-g",
};

function Card({
  title,
  desc,
  right,
  children,
}: {
  title: string;
  desc?: string;
  right?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className="rounded-xl border border-border bg-card p-5">
      <div className="mb-3 flex items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold">{title}</h2>
          {desc && <p className="mt-0.5 text-xs text-muted-foreground">{desc}</p>}
        </div>
        {right}
      </div>
      {children}
    </section>
  );
}

export default function GatewayPage() {
  const [status, setStatus] = useState<Status | null>(null);
  const [pool, setPool] = useState<PoolEntry[]>([]);
  /** 账号池按产品折叠（与清理页的分类块一致）：默认收起，点「展开」看明细。 */
  const [poolCollapsed, setPoolCollapsed] = useState<Set<string>>(new Set());
  const [keys, setKeys] = useState<KeyRow[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [keyName, setKeyName] = useState("");
  const [plaintext, setPlaintext] = useState<string | null>(null);

  const [model, setModel] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null);

  const [discovered, setDiscovered] = useState<Discovered[]>([]);
  const [reports, setReports] = useState<SourceReport[]>([]);
  /// 「复制时固定来源」：全局一个下拉（空 = 自动选号，复制裸名）。
  /// 之前是每个模型一个下拉，25 行下拉把页面撑得又高又乱。
  const [copyScope, setCopyScope] = useState<string>("");
  const [scanning, setScanning] = useState(false);
  /// 积分共享总开关（关闭会真的停网关）。
  const [gwOn, setGwOn] = useState(true);

  const [upstreamsText, setUpstreamsText] = useState("");
  const [upstreamsOpen, setUpstreamsOpen] = useState(false);

  /// 唯一的复制出口：真的写进剪贴板才弹成功，失败必须报错（不许「看着像成功」）。
  const doCopy = useCallback(async (text: string, hint: string) => {
    const ok = await copyText(text);
    if (ok) {
      toast.success(`已复制 ${text}`, { description: hint });
    } else {
      toast.error("复制失败", { description: "剪贴板被占用或被系统拦下，请手动复制：" + text });
    }
  }, []);

  const reload = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const [s, p, k] = await Promise.all([gatewayStatus(), gatewayPool(), gatewayKeysList()]);
      setStatus(s as Status);
      setGwOn(Boolean((s as Status).running));
      const rows = Array.isArray(p) ? (p as PoolEntry[]) : [];
      setPool(rows);
      setPoolCollapsed((prev) => (prev.size > 0 ? prev : new Set(rows.map((r) => r.product))));
      setKeys(Array.isArray(k) ? (k as KeyRow[]) : []);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const grouped = useMemo(() => {
    const map = new Map<string, PoolEntry[]>();
    for (const e of pool) {
      const arr = map.get(e.product) ?? [];
      arr.push(e);
      map.set(e.product, arr);
    }
    return Array.from(map.entries());
  }, [pool]);

  // 分组口径：**跨产品**才算通用（Trae ↔ WorkBuddy ↔ Qoder）。
  // 同一产品的国内/国际两个区域同名（如 qoder 的 qmodel）**各归各的方框** ——
  // 它们其实是不同区域的不同服务，混在一起会让人以为「国内外并起来了」。
  const { sharedModels, ownModels, ownScopes } = useMemo(() => {
    const productOf = (scope: string) => scope.split("-")[0];
    const byModel = new Map<string, string[]>();
    discovered.forEach((d) => {
      const arr = byModel.get(d.model) ?? [];
      if (!arr.includes(d.scope)) arr.push(d.scope);
      byModel.set(d.model, arr);
    });
    const shared: { model: string; scopes: string[] }[] = [];
    const own: Record<string, string[]> = {};
    byModel.forEach((scopes, model) => {
      const products = new Set(scopes.map(productOf));
      if (products.size >= 2) {
        shared.push({ model, scopes: scopes.slice().sort() });
      } else {
        // 同一产品：每个区域各自列出来（国内外分开）。
        scopes.forEach((sc) => {
          (own[sc] ??= []).push(model);
        });
      }
    });
    shared.sort((a, b) => a.model.localeCompare(b.model));
    Object.keys(own).forEach((k) => own[k].sort());
    return { sharedModels: shared, ownModels: own, ownScopes: Object.keys(own).sort() };
  }, [discovered]);


  async function createKey() {
    setBusy(true);
    setError(null);
    try {
      const r = await gatewayKeyCreate(keyName.trim() || "默认", []);
      setPlaintext(r.plaintext);
      setKeyName("");
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function doPreview() {
    if (!model.trim()) return;
    setError(null);
    try {
      setPreview((await gatewayPreview(model.trim())) as Preview);
    } catch (e) {
      setPreview(null);
      setError(String(e));
    }
  }

  // 打开本页就自动识别一次：各平台的模型更新后，这里点开即是最新（也可再手动点）。
  const autoScanned = useRef(false);
  useEffect(() => {
    if (autoScanned.current) return;
    autoScanned.current = true;
    // ① 先读**本地** models.json 立即渲染（毫秒级，不联网、不转圈）。
    void (async () => {
      try {
        const cached = (await gatewayModelsGet()) as Record<string, string[]>;
        const rows: Discovered[] = Object.entries(cached ?? {}).flatMap(([scope, list]) =>
          (list ?? []).map((m) => ({
            scope,
            product: scope.split("-")[0],
            model: m,
            display: "",
            source: "本地缓存",
          })),
        );
        if (rows.length > 0) setDiscovered(rows);
      } catch {
        /* 本地没有就等下面那次联网识别 */
      }
    })();
    // ② 页面空闲 2.5 秒后再联网刷新（各平台模型更新时自然同步）。
    const timer = setTimeout(() => void scanModels(), 2500);
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function scanModels() {
    setScanning(true);
    setError(null);
    try {
      const r = await gatewayModelsDiscover();
      setDiscovered(r.models ?? []);
      setReports(r.reports ?? []);
    } catch (e) {
      setError(String(e));
    } finally {
      setScanning(false);
    }
  }




  async function openUpstreams() {
    setError(null);
    try {
      const v = await gatewayUpstreamsGet();
      setUpstreamsText(JSON.stringify(v ?? {}, null, 2));
      setUpstreamsOpen(true);
    } catch (e) {
      setError(String(e));
    }
  }

  async function saveUpstreams() {
    setBusy(true);
    setError(null);
    try {
      await gatewayUpstreamsSet(JSON.parse(upstreamsText));
      setUpstreamsOpen(false);
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 px-6 py-6">
      <header className="flex items-start justify-between gap-3">
        <div>
          <h1 className="text-lg font-semibold">积分共享</h1>
          <p className="mt-1 text-xs text-muted-foreground">
            把 Trae / WorkBuddy / Qoder 的账号额度合成一个池子，按「谁先过期就用谁」供给本机其它 AI
            工具。数据只在本机。
          </p>
        </div>
        <div className="flex items-center gap-2">
          <label className="flex items-center gap-1.5 rounded-lg border border-border px-2.5 py-1.5 text-xs text-muted-foreground">
            <input
              type="checkbox"
              checked={gwOn}
              onChange={async (e) => {
                const on = e.target.checked;
                setGwOn(on);
                try {
                  await gatewaySetEnabled(on);
                  await reload();
                } catch (err) {
                  setError(String(err));
                  setGwOn(!on);
                }
              }}
            />
            {gwOn ? "积分共享开启" : "已关闭（网关不监听端口）"}
          </label>
          <Button variant="outline" size="sm" asChild className="gap-1.5">
            <Link to="/gateway-guide">
              <BookOpen className="size-4" />
              使用说明
            </Link>
          </Button>
          <button
            type="button"
            onClick={() => void reload()}
            className="inline-flex items-center gap-1.5 rounded-lg border border-border px-3 py-1.5 text-xs hover:bg-accent"
          >
            {busy ? <Loader2 className="size-3.5 animate-spin" /> : <RefreshCw className="size-3.5" />}
            刷新
          </button>
        </div>
      </header>

      {error && (
        <div className="flex items-start gap-2 rounded-xl border border-destructive/30 bg-destructive/5 px-4 py-3 text-xs text-destructive">
          <XCircle className="mt-0.5 size-3.5 shrink-0" />
          <span className="break-all">{error}</span>
        </div>
      )}

      <Card
        title="网关状态"
        desc="随应用启动，只绑定 127.0.0.1"
        right={
          <span
            className={cn(
              "rounded-full border px-2.5 py-0.5 text-[11px]",
              status?.running
                ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-600"
                : "border-border text-muted-foreground",
            )}
          >
            {status?.running ? "运行中" : "未启动"}
          </span>
        }
      >
        <div className="grid grid-cols-2 gap-3 text-xs sm:grid-cols-4">
          <div>
            <div className="text-muted-foreground">地址</div>
            <div className="mt-0.5 font-mono">{status?.addr ?? "—"}</div>
          </div>
          <div>
            <div className="text-muted-foreground">端口</div>
            <div className="mt-0.5 font-mono">{status?.port ?? "—"}</div>
          </div>
          <div>
            <div className="text-muted-foreground">成功请求</div>
            <div className="mt-0.5 font-mono">{status?.requests ?? 0}</div>
          </div>
          <div>
            <div className="text-muted-foreground">被拒</div>
            <div className="mt-0.5 font-mono">{status?.rejected ?? 0}</div>
          </div>
        </div>
        <p className="mt-3 text-xs text-muted-foreground">
          客户端填 <span className="font-mono">http://127.0.0.1:57892/v1</span>，Key 用下面新建的{" "}
          <span className="font-mono">sk-ts-…</span>。
        </p>
      </Card>

      <Card
        title="接入自定义模型（以 WorkBuddy 为例）"
        desc="把网关当 OpenAI 兼容服务用；下面两条地址按客户端的要求二选一，点右侧复制"
      >
        <div className="space-y-2 text-xs">
          <div className="flex items-center gap-2 rounded-lg border border-border px-3 py-2">
            <div className="min-w-0 flex-1">
              <div className="font-medium">WorkBuddy 的「接口地址」要填完整端点</div>
              <div className="truncate font-mono text-[11px] text-muted-foreground">
                http://127.0.0.1:57892/v1/chat/completions
              </div>
              <div className="text-[11px] text-muted-foreground">
                它会把填的地址**原样当作对话端点直接 POST**，必须填到 /chat/completions 这一层。
              </div>
            </div>
            <Button
              variant="outline"
              size="sm"
              className="gap-1.5"
              onClick={() => void doCopy("http://127.0.0.1:57892/v1/chat/completions", "填进客户端的接口地址")}
            >
              <Copy className="size-3.5" /> 复制
            </Button>
          </div>
          <div className="flex items-center gap-2 rounded-lg border border-border px-3 py-2">
            <div className="min-w-0 flex-1">
              <div className="font-medium">其他 OpenAI 兼容客户端（Trae 等）填 Base URL</div>
              <div className="truncate font-mono text-[11px] text-muted-foreground">
                http://127.0.0.1:57892/v1
              </div>
              <div className="text-[11px] text-muted-foreground">
                标准 OpenAI 惯例：客户端自己拼 /chat/completions。
              </div>
            </div>
            <Button
              variant="outline"
              size="sm"
              className="gap-1.5"
              onClick={() => void doCopy("http://127.0.0.1:57892/v1", "只填到 base_url 的客户端用这个")}
            >
              <Copy className="size-3.5" /> 复制
            </Button>
          </div>
          <p className="text-[11px] text-muted-foreground">
            API Key 在上方创建；模型名在下方「模型」卡片里勾选后点复制，填进客户端。
          </p>
        </div>
      </Card>

      <Card title="账号池" desc="最紧迫（最早过期）的排在最前面">
        {pool.length === 0 ? (
          <p className="py-6 text-center text-xs text-muted-foreground">
            还没有账号。先去各产品的账号管理里添加。
          </p>
        ) : (
          <div className="flex flex-col gap-4">
            {grouped.map(([product, rows]) => {
              const on = rows.filter((r) => r.usable).length;
              const collapsed = poolCollapsed.has(product);
              return (
                <div key={product} className="rounded-lg border border-border">
                  {/* 块头：产品 + 可用数 + 展开/收起（与清理页同一套交互） */}
                  <div className="flex flex-wrap items-center justify-between gap-2 border-b bg-muted/20 px-3 py-2">
                    <div className="flex min-w-0 items-center gap-2">
                      <span className="text-[11px] font-medium tracking-wide text-muted-foreground">
                        {PRODUCTS[product] ?? product}
                      </span>
                      <span
                        className={cn(
                          "rounded px-1 text-[10px] tabular-nums",
                          on === rows.length
                            ? "bg-emerald-500/10 text-emerald-600"
                            : on === 0
                              ? "bg-destructive/10 text-destructive"
                              : "bg-amber-500/10 text-amber-600",
                        )}
                      >
                        {on}/{rows.length} 可用
                      </span>
                    </div>
                    <button
                      type="button"
                      onClick={() =>
                        setPoolCollapsed((prev) => {
                          const next = new Set(prev);
                          if (next.has(product)) next.delete(product);
                          else next.add(product);
                          return next;
                        })
                      }
                      className="rounded px-1.5 py-0.5 text-[11px] text-muted-foreground hover:bg-accent hover:text-foreground"
                    >
                      {collapsed ? "展开" : "收起"}
                    </button>
                  </div>

                  {collapsed ? null : (
                    <div className="overflow-hidden">
                      <table className="w-full text-xs">
                        <thead className="bg-muted/40 text-muted-foreground">
                          <tr>
                            <th className="px-3 py-2 text-left font-medium">账号</th>
                            <th className="px-3 py-2 text-left font-medium">作用域</th>
                            <th className="px-3 py-2 text-right font-medium">剩余</th>
                            <th className="px-3 py-2 text-left font-medium">到期</th>
                            <th className="px-3 py-2 text-left font-medium">状态</th>
                          </tr>
                        </thead>
                        <tbody>
                          {rows.map((r) => (
                            <tr
                              key={r.key}
                              className={cn(
                                "border-t border-border/60",
                                !r.usable && "bg-destructive/[0.03]",
                              )}
                            >
                              <td className="px-3 py-2">{r.label}</td>
                              <td className="px-3 py-2 font-mono">{r.scope}</td>
                              <td className="px-3 py-2 text-right font-mono">
                                {r.remaining == null ? (
                                  <span className="text-muted-foreground">未知</span>
                                ) : (
                                  Math.round(r.remaining)
                                )}
                              </td>
                              <td className="px-3 py-2">{fmtExpiry(r.expires_at)}</td>
                              <td className="px-3 py-2">
                                {r.cooldown_until && r.cooldown_until > Date.now() ? (
                                  /* 整号冷却：上游明确拒绝了这个账号（凭据死 / 传输炸） */
                                  <span
                                    className="cursor-help text-amber-600"
                                    title={`上游报错后暂停到 ${new Date(r.cooldown_until).toLocaleString()}，期间不会选它`}
                                  >
                                    {fmtRecover(r.cooldown_until)}
                                  </span>
                                ) : r.limited_models.length > 0 ? (
                                  /* 模型级冷却：只有这几个模型用完，别的模型照常能用 */
                                  <span
                                    className="cursor-help text-amber-600"
                                    title={r.limited_models
                                      .map((m) => `${m.model} 至 ${new Date(m.reset_at).toLocaleString()}`)
                                      .join("\n")}
                                  >
                                    {r.limited_models.length} 个模型受限 ·{" "}
                                    {fmtRecover(r.limited_models[0].reset_at)}
                                  </span>
                                ) : r.usable ? (
                                  <span className="text-emerald-600">可用</span>
                                ) : (
                                  /* 原因可能很长（401 / 过期 / 无凭据），放 title，表格不被撑宽 */
                                  <span
                                    className="cursor-help text-destructive"
                                    title={r.reason ?? "不可用"}
                                  >
                                    不可用
                                  </span>
                                )}
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </Card>

      <Card
        title="网关 Key"
        desc="明文只在创建时显示一次；库里只存哈希与前后 4 位"
        right={<KeyRound className="size-4 text-muted-foreground" />}
      >
        <div className="mb-3 flex flex-wrap items-center gap-2">
          <input
            value={keyName}
            onChange={(e) => setKeyName(e.target.value)}
            placeholder="用途，例如 Cursor"
            className="h-8 w-56 rounded-lg border border-border bg-background px-2.5 text-xs outline-none focus:border-primary"
          />
          <button
            type="button"
            onClick={() => void createKey()}
            disabled={busy}
            className="inline-flex h-8 items-center gap-1.5 rounded-lg bg-primary px-3 text-xs font-medium text-primary-foreground hover:opacity-90 disabled:opacity-50"
          >
            新建 Key
          </button>
        </div>

        {plaintext && (
          <div className="mb-3 rounded-lg border border-dashed border-primary/40 bg-primary/5 px-3 py-2.5">
            <div className="text-[11px] font-medium">复制保存，只显示这一次：</div>
            <div className="mt-1 flex items-center gap-2">
              <code className="select-all break-all font-mono text-xs">{plaintext}</code>
              <button
                type="button"
                onClick={() => void doCopy(plaintext, "只显示这一次，丢了要重建 Key")}
                className="inline-flex shrink-0 items-center gap-1 rounded-md border border-border px-2 py-1 text-[11px] hover:bg-accent"
              >
                <Copy className="size-3" />
                复制
              </button>
            </div>
          </div>
        )}

        {keys.length === 0 ? (
          <p className="py-4 text-center text-xs text-muted-foreground">
            还没有 Key。新建一把后填进客户端才能用。
          </p>
        ) : (
          <div className="overflow-hidden rounded-lg border border-border">
            <table className="w-full text-xs">
              <thead className="bg-muted/40 text-muted-foreground">
                <tr>
                  <th className="px-3 py-2 text-left font-medium">用途</th>
                  <th className="px-3 py-2 text-left font-medium">Key</th>
                  <th className="px-3 py-2 text-left font-medium">状态</th>
                  <th className="px-3 py-2 text-right font-medium">操作</th>
                </tr>
              </thead>
              <tbody>
                {keys.map((k) => (
                  <tr key={k.id} className="border-t border-border/60">
                    <td className="px-3 py-2">{k.name}</td>
                    <td className="px-3 py-2 font-mono">
                      {k.prefix}…{k.suffix}
                    </td>
                    <td className="px-3 py-2">
                      {k.revoked_at ? (
                        <span className="text-muted-foreground">已吊销</span>
                      ) : (
                        <span className="text-emerald-600">有效</span>
                      )}
                    </td>
                    <td className="px-3 py-2 text-right">
                      {k.revoked_at ? (
                        <div className="inline-flex gap-1.5">
                          <button
                            type="button"
                            onClick={() => void gatewayKeyRestore(k.id).then(reload)}
                            className="rounded-md border border-border px-2 py-1 hover:bg-accent"
                          >
                            恢复
                          </button>
                          <button
                            type="button"
                            onClick={() => void gatewayKeyDelete(k.id).then(reload)}
                            className="inline-flex items-center gap-1 rounded-md border border-destructive/30 px-2 py-1 text-destructive hover:bg-destructive/5"
                          >
                            <Trash2 className="size-3" />
                            删除
                          </button>
                        </div>
                      ) : (
                        <button
                          type="button"
                          onClick={() => void gatewayKeyRevoke(k.id).then(reload)}
                          className="rounded-md border border-border px-2 py-1 hover:bg-accent"
                        >
                          吊销
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      <Card
        title="模型管理"
        desc="通用模型默认由后端自动选号（积分到期最早的先用）；专有模型选中即走对应产品的积分"
        right={
          <button
            type="button"
            onClick={() => void scanModels()}
            disabled={scanning}
            className="inline-flex items-center gap-1.5 rounded-lg border border-border px-3 py-1.5 text-xs hover:bg-accent disabled:opacity-50"
          >
            {scanning ? <Loader2 className="size-3.5 animate-spin" /> : <RefreshCw className="size-3.5" />}
            重新识别
          </button>
        }
      >
        {reports.length > 0 && (
          <div className="mb-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-[10px] text-muted-foreground">
            {reports.map((r) => (
              <span key={`${r.product}-${r.scope}`} className="flex items-center gap-1" title={r.note}>
                <span
                  className={cn(
                    "size-1.5 shrink-0 rounded-full",
                    r.found > 0 ? "bg-emerald-500" : "bg-amber-500",
                  )}
                />
                <span>{r.scope}</span>
                <span className="text-foreground">{r.found > 0 ? r.found : "未识别"}</span>
              </span>
            ))}
          </div>
        )}

        {discovered.length === 0 ? (
          <p className="text-xs text-muted-foreground">
            点右上「重新识别」，从三个产品里读出可用模型。
          </p>
        ) : (
          // 每个模块一个小方框（通用 / 各作用域），框内竖排 + 框内滚动
          <div className="grid grid-cols-1 gap-3 md:grid-cols-2 2xl:grid-cols-3">
            {/* 通用模型 */}
            <div className="flex min-w-0 flex-col overflow-hidden rounded-lg border border-border">
              <div className="flex items-center gap-1.5 border-b border-border/60 bg-muted/40 px-2.5 py-1.5">
                <span className="text-[11px] font-medium">通用模型</span>
                <span className="rounded bg-background px-1 text-[10px] text-muted-foreground">
                  {sharedModels.length}
                </span>
                <select
                  value={copyScope}
                  onChange={(e) => setCopyScope(e.target.value)}
                  className="ml-auto h-5 max-w-[92px] rounded border border-border bg-background px-1 text-[10px] outline-none focus:border-primary"
                  title="点模型名时复制成什么：自动 = 裸名（后端选号）；选某一家 = 带前缀锁定"
                >
                  <option value="">自动</option>
                  {Object.keys(SCOPE_LABEL).map((sc) => (
                    <option key={sc} value={sc}>
                      {SCOPE_SHORT[sc]}
                    </option>
                  ))}
                </select>
              </div>
              <div className="max-h-56 overflow-y-auto p-1">
                {sharedModels.map(({ model, scopes }) => {
                  const useScope = copyScope && scopes.includes(copyScope) ? copyScope : "";
                  const toCopy = useScope ? `${SCOPE_ALIAS[useScope]}:${model}` : model;
                  return (
                    // ⚠️ 这里**不能**用整行 `<button>` 包住来源徽章：徽章本身要是按钮
                    //    （点它就锁定那一家），button 套 button 是非法 HTML，点了会串行为。
                    // ⚠️ 卡片窄（窗口小 / 两列）时徽章会顶出边界被 `overflow-hidden` 裁掉，
                    //    右边的复制图标就点不到了 ⇒ 整行 `flex-wrap`，放不下自动落到第二行。
                    <div
                      key={model}
                      className="flex w-full flex-wrap items-center gap-x-1 gap-y-0.5 rounded-md px-1.5 py-1 text-[11px] hover:bg-accent"
                    >
                      <button
                        type="button"
                        onClick={() =>
                          void doCopy(
                            toCopy,
                            useScope
                              ? `锁定 ${SCOPE_LABEL[useScope] ?? useScope} 的积分`
                              : "不写前缀 = 后端在所有提供它的产品间自动选号",
                          )
                        }
                        title={`复制：${toCopy}（按块头下拉的默认来源）`}
                        className="min-w-0 flex-1 basis-[72px] truncate text-left font-mono"
                      >
                        {model}
                      </button>
                      <span className="flex shrink-0 flex-wrap items-center justify-end gap-0.5">
                        {scopes.map((sc) => (
                          <button
                            key={sc}
                            type="button"
                            onClick={() =>
                              void doCopy(
                                `${SCOPE_ALIAS[sc] ?? sc}:${model}`,
                                `锁定 ${SCOPE_LABEL[sc] ?? sc} —— 只走这一家的积分`,
                              )
                            }
                            title={`点一下复制 ${SCOPE_ALIAS[sc] ?? sc}:${model}（锁定 ${SCOPE_LABEL[sc] ?? sc}）`}
                            className="rounded border border-border px-1 text-[9px] text-muted-foreground transition-colors hover:border-primary hover:bg-primary/5 hover:text-foreground"
                          >
                            {SCOPE_SHORT[sc] ?? sc}
                          </button>
                        ))}
                        {/* 复制按钮必须是真的按钮（之前只是个图标，点了没反应） */}
                        <button
                          type="button"
                          onClick={() =>
                            void doCopy(
                              toCopy,
                              useScope
                                ? `锁定 ${SCOPE_LABEL[useScope] ?? useScope} 的积分`
                                : "不写前缀 = 后端在所有提供它的产品间自动选号",
                            )
                          }
                          title={`复制 ${toCopy}`}
                          className="grid size-4 shrink-0 place-items-center rounded text-muted-foreground/60 transition-colors hover:bg-primary/10 hover:text-primary"
                        >
                          <Copy className="size-3" />
                        </button>
                      </span>
                    </div>
                  );
                })}
                </div>
                <div className="border-t border-border/60 px-2 py-1 text-[10px] leading-4 text-muted-foreground">
                  要「自动切换」就点<span className="text-foreground">模型名</span>或最右边的复制图标（下拉保持
                  <span className="text-foreground">自动</span>＝复制裸名）；想让某个模型只花某一家的积分，点
                  <span className="text-foreground"> Trae / WB / Qoder </span>徽章＝锁死那一家（自动带前缀）
                </div>
              </div>

            {/* 各作用域一个小方框 */}
            {ownScopes.map((scope) => (
              <div key={scope} className="flex min-w-0 flex-col overflow-hidden rounded-lg border border-border">
                <div className="flex items-center gap-1.5 border-b border-border/60 bg-muted/40 px-2.5 py-1.5">
                  <span className="text-[11px] font-medium">{SCOPE_LABEL[scope] ?? scope}</span>
                  <span className="rounded bg-background px-1 text-[10px] text-muted-foreground">
                    {ownModels[scope].length}
                  </span>
                  <span className="ml-auto text-[10px] text-muted-foreground">专有</span>
                </div>
                <div className="max-h-56 overflow-y-auto p-1">
                  {ownModels[scope].map((model) => (
                    <button
                      key={model}
                      type="button"
                      onClick={() => void doCopy(model, "专有模型：只有这一家提供，直接裸名")}
                      title={`复制：${model}`}
                      className="flex w-full items-center gap-1.5 rounded-md px-1.5 py-1 text-left text-[11px] hover:bg-accent"
                    >
                      <span className="min-w-0 flex-1 truncate font-mono">{model}</span>
                      <Copy className="size-3 shrink-0 text-muted-foreground/50" />
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>
        )}
      </Card>

      <Card title="选号预览" desc="只算不发，不消耗额度">
        <div className="flex flex-wrap items-center gap-2">
          <input
            value={model}
            onChange={(e) => setModel(e.target.value)}
            placeholder="glm-5 或 qoder-cn:glm-5"
            className="h-8 w-64 rounded-lg border border-border bg-background px-2.5 text-xs outline-none focus:border-primary"
          />
          <button
            type="button"
            onClick={() => void doPreview()}
            className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-border px-3 text-xs hover:bg-accent"
          >
            <Server className="size-3.5" />
            预览
          </button>
        </div>
        {preview && (
          <div className="mt-3 rounded-lg border border-border bg-muted/30 px-3 py-2 text-xs">
            <div>
              候选 <span className="font-mono">{preview.candidates.join(" / ") || "—"}</span>
            </div>
            <div className="mt-1">
              {preview.picked ? (
                <>
                  选中{" "}
                  <span className="font-medium">
                    {preview.picked.scope} / {preview.picked.label}
                  </span>
                  {preview.picked.remaining != null && (
                    <span className="text-muted-foreground">
                      {" "}
                      · 剩余 {Math.round(preview.picked.remaining)}
                    </span>
                  )}
                  <span className="text-muted-foreground"> · {fmtExpiry(preview.picked.expires_at)}</span>
                  {/* 这个号在当前模型上的限额：有就说明为什么它不是「完全没问题」 */}
                  {(preview.picked.limited_models?.length ?? 0) > 0 && (
                    <span
                      className="ml-2 rounded bg-amber-500/10 px-1.5 py-0.5 text-[11px] text-amber-600"
                      title={preview.picked.limited_models
                        .map((m) => `${m.model} 至 ${new Date(m.reset_at).toLocaleString()}`)
                        .join("\n")}
                    >
                      {preview.picked.limited_models[0].model} ·{" "}
                      {fmtRecover(preview.picked.limited_models[0].reset_at)}
                    </span>
                  )}
                </>
              ) : (
                <span className="text-muted-foreground">{preview.reason ?? "没有可用账号"}</span>
              )}
            </div>
          </div>
        )}
      </Card>

      <Card title="上游地址" desc="决定请求真正发到哪里；没登记的产品会返回 501 并说明原因">
        <div className="flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => void openUpstreams()}
            className="rounded-lg border border-border px-3 py-1.5 text-xs hover:bg-accent"
          >
            编辑 upstreams.json
          </button>
        </div>
        <p className="mt-2 text-[11px] leading-relaxed text-muted-foreground">
          例：
          <span className="font-mono">
            {'{"qoder-cn":{"base_url":"https://gateway.qoder.com.cn","chat_path":"/v1/chat/completions"}}'}
          </span>
        </p>

        {upstreamsOpen && (
          <div className="mt-3">
            <div className="mb-1 text-[11px] text-muted-foreground">upstreams.json</div>
            <textarea
              value={upstreamsText}
              onChange={(e) => setUpstreamsText(e.target.value)}
              spellCheck={false}
              className="h-40 w-full rounded-lg border border-border bg-background p-2.5 font-mono text-xs outline-none focus:border-primary"
            />
            <div className="mt-2 flex gap-2">
              <button
                type="button"
                onClick={() => void saveUpstreams()}
                disabled={busy}
                className="rounded-lg bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground hover:opacity-90 disabled:opacity-50"
              >
                保存
              </button>
              <button
                type="button"
                onClick={() => setUpstreamsOpen(false)}
                className="rounded-lg border border-border px-3 py-1.5 text-xs hover:bg-accent"
              >
                取消
              </button>
            </div>
          </div>
        )}
      </Card>
    </div>
  );
}
