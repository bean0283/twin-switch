import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { BookOpen, Copy, KeyRound, Loader2, RefreshCw, Server, Trash2, XCircle } from "lucide-react";

import { Link } from "react-router-dom";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";

import {
  gatewayKeyCreate,
  gatewayKeyDelete,
  gatewayKeyResetUsage,
  gatewayKeyRestore,
  gatewayKeyRevoke,
  gatewayKeySetQuota,
  gatewayKeysList,
  gatewayPricing,
  gatewayModelsDiscover,
  gatewayPool,
  gatewayPreview,
  gatewayModelsGet,
  gatewayModelLabels,
  gatewaySetEnabled,
  gatewaySetLan,
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
  last_used_at?: number | null;
  /** 额度（两个上限，`null`/缺省 = 该项不限）。 */
  quota?: {
    max_total_tokens?: number | null;
    max_daily_tokens?: number | null;
  };
  /** 累计用量（token）。 */
  usage?: {
    total_tokens?: number;
    prompt_tokens?: number;
    completion_tokens?: number;
    requests?: number;
    day?: string;
    day_tokens?: number;
    last_at?: number | null;
  };
  /** `quota` = 额度用尽被自动吊销；`null` = 手工吊销。 */
  revoked_reason?: string | null;
};

/** token 数量 × 单价（元/百万）＝ 金额（元）。 */
function yuanOfTokens(tokens: number, perMtok: number): number {
  return (tokens / 1_000_000) * perMtok;
}

/** 金额显示：小额保留 4 位，否则 2 位 —— ¥0.16 和 ¥12.34 都要看得清。 */
function fmtYuan(v: number): string {
  return `¥${v.toFixed(v >= 0.01 || v === 0 ? 2 : 4)}`;
}

/**
 * 一把 Key 的金额视图。与网关 `/v1/user/balance` **同一套算法**：
 *   已花费 = 输入token×输入价 + 输出token×输出价
 *   预算   = 额度token × (已花费/已用)   ← 同一 p/c 比例折算
 *   剩余   = 预算 − 已花费（不为负）
 * `perMtok` 是这把 Key 实际混合下来的每百万 token 单价。
 */
function keyMoney(
  k: KeyRow,
  pricing: { inputPerMtok: number; outputPerMtok: number } | null,
): { spent: number; budget: number; left: number; perMtok: number } | null {
  if (!pricing) return null;
  const budgetTokens = k.quota?.max_total_tokens ?? null;
  if (budgetTokens == null) return null;
  const p = k.usage?.prompt_tokens ?? 0;
  const c = k.usage?.completion_tokens ?? 0;
  const spent = yuanOfTokens(p, pricing.inputPerMtok) + yuanOfTokens(c, pricing.outputPerMtok);
  const used = k.usage?.total_tokens ?? 0;
  const perMtok = used > 0 ? (spent / used) * 1_000_000 : pricing.inputPerMtok;
  const budget = yuanOfTokens(budgetTokens, perMtok);
  return { spent, budget, left: Math.max(0, budget - spent), perMtok };
}

/** token 数量按人眼习惯缩写：1234 → 1.2k，1_234_567 → 1.23M。 */
function fmtTok(n: number | undefined | null): string {
  const v = n ?? 0;
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(v >= 10_000_000 ? 1 : 2)}M`;
  if (v >= 1000) return `${(v / 1000).toFixed(v >= 10_000 ? 0 : 1)}k`;
  return String(v);
}

type Status = {
  running: boolean;
  port: number;
  requests: number;
  rejected: number;
  addr: string;
  /** 本机回环地址（始终可用）。 */
  localAddr?: string;
  /** 是否真的绑了 `0.0.0.0`（不是配置值，是实际生效值）。 */
  lan?: boolean;
  /** 配置里的意图值（网关没在跑时 `lan` 恒 false，但勾选已经落盘）。 */
  lanPref?: boolean;
  /** 局域网地址；探测不到时为 null —— 不许拿回环地址冒充。 */
  lanAddr?: string | null;
  lanAddrError?: string | null;
  /** 绑定失败原因（空 = 没失败过）。用来区分「关了」和「起不来」。 */
  bindError?: string | null;
};

type Discovered = {
  scope: string;
  product: string;
  model: string;
  display: string;
  /// 对外公开名（Qoder 是客户端里显示的那个名字，其余家等于 `model`）。
  public?: string;
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

/// 专有板块的标题（板块 key 是**产品级**：Qoder 两个区域并成一框）。
const OWN_LABEL: Record<string, string> = { qoder: "Qoder" };

/// 作用域 → 产品（`qoder-global` → `qoder`）。
const productOfScope = (scope: string) => scope.split("-")[0];

/// 「通用模型」框头的筛选/默认来源选项：按**产品**筛，不按区域 ——
/// 用户眼里「Qoder 有没有这个模型」比「是 Qoder 国内还是国际」重要得多。
const FILTER_PRODUCTS: { key: string; label: string }[] = [
  { key: "trae", label: "Trae" },
  { key: "wb", label: "WB" },
  { key: "qoder", label: "Qoder" },
];

/// 模型名归一化指纹（与后端 `gateway_pool::normalize_model_key` **同一条规则**）。
///
/// 「通用模型」靠它把 `GLM-5.3`（Qoder）和 `glm-5.3`（Trae）认成同一个模型 ——
/// 两边规则一旦不一致，通用列表里就会冒出两个只差大小写的同名条目。
function normModel(s: string): string {
  return s.toLowerCase().replace(/[^\p{L}\p{N}]/gu, "");
}

/// 对外公开名（与后端 `gateway_models::public_from` 同一条规则）。
///
/// Qoder 上游认 `gmodel` 这类代号，客户端里显示的是 `GLM-5.3` —— 界面上只给后者
/// （2026-10-24 用户要求「只显示正常的模型，复制也只复制正常的模型名」）。
function publicOf(scope: string, model: string, display: string): string {
  if (!scope.startsWith("qoder")) return model;
  const d = (display ?? "").trim();
  if (!d || d.toLowerCase() === model.toLowerCase()) return model;
  return d;
}

/// 模型名渲染：有比公开名更好看的显示名时并排给（`DeepSeek-V4-Flash 正式版 deepseek-v4.1-flash`），
/// 否则只显示公开名。**代号永远不进这个组件** —— 用户认不出 `gmodel`。
function ModelName({ display, name }: { display: string; name: string }) {
  if (display && display !== name) {
    return (
      <>
        {display}
        <span className="ml-1.5 font-mono text-[10px] text-muted-foreground">{name}</span>
      </>
    );
  }
  return <span className="font-mono">{name}</span>;
}

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
  /** 当前 token 单价（把额度换算成金额用；后端 `gateway_pricing` 给）。 */
  const [pricing, setPricing] = useState<{
    inputPerMtok: number;
    outputPerMtok: number;
  } | null>(null);
  /** 正在编辑额度的那把 Key（null = 没有展开的编辑器）。 */
  const [quotaEdit, setQuotaEdit] = useState<{
    id: string;
    total: string;
    daily: string;
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [keyName, setKeyName] = useState("");
  const [plaintext, setPlaintext] = useState<string | null>(null);

  const [model, setModel] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null);

  const [discovered, setDiscovered] = useState<Discovered[]>([]);
  const [reports, setReports] = useState<SourceReport[]>([]);
  /// 「复制时固定来源」：全局一个下拉（空 = 全部，复制裸名）。
  /// 之前是每个模型一个下拉，25 行下拉把页面撑得又高又乱。
  const [copyScope, setCopyScope] = useState<string>("");
  /// 通用框的搜索词。模型多起来（20 组、名字还长短不一）光靠滚动找不到 ——
  /// 用户报「没看到 DeepSeek-V4.1-Flash」，其实它在通用框里、只是要往下滚。
  const [modelQuery, setModelQuery] = useState("");
  const [scanning, setScanning] = useState(false);
  /// 积分共享总开关（关闭会真的停网关）。
  const [gwOn, setGwOn] = useState(true);
  /// 局域网共享（真的绑 0.0.0.0 才为 true）。切换会重启网关。
  const [lanOn, setLanOn] = useState(false);
  const [lanBusy, setLanBusy] = useState(false);

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
      const [s, p, k, pr] = await Promise.all([
        gatewayStatus(),
        gatewayPool(),
        gatewayKeysList(),
        gatewayPricing().catch(() => null),
      ]);
      setPricing(pr);
      setStatus(s as Status);
      setGwOn(Boolean((s as Status).running));
      // 网关没在跑时 `lan` 恒为 false，用配置值 `lanPref` 还原用户的勾选
      setLanOn(
        (s as Status).running ? Boolean((s as Status).lan) : Boolean((s as Status).lanPref),
      );
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

  /// 切换局域网共享。改绑定地址 = 重启网关，所以必须把最新状态读回来再说话，
  /// 不许「勾上了就当成功」—— 绑不上时要报错（`bindError`）。
  const toggleLan = useCallback(
    async (on: boolean) => {
      setLanBusy(true);
      setError(null);
      setLanOn(on); // 乐观更新，失败回滚
      try {
        const r = await gatewaySetLan(on);
        const s = r.status as Status;
        setStatus(s);
        // 网关没在跑时 `lan` 不会生效（配置已落盘），此时以用户选择为准，别把勾弹回去。
        setLanOn(s.running ? Boolean(s.lan) : on);
        if (on && s.running && !s.lan) {
          toast.error("没能切到局域网模式", {
            description: s.bindError ?? "网关没有重新监听，先点一次「刷新」看状态",
          });
        } else if (on && !s.running) {
          // 总开关关着时只落配置、不重绑 —— 这不是失败，别报成失败
          toast.success("已记录局域网设置", {
            description: "积分共享当前是关闭的，打开总开关后会按局域网模式启动",
          });
        } else if (on) {
          toast.success("已开启局域网共享", {
            description: s.lanAddr
              ? `同网段设备填 ${s.lanAddr}`
              : (s.lanAddrError ?? "未识别到局域网地址，请手动查本机 IP"),
          });
        } else {
          toast.success("已关闭局域网共享", { description: "网关回到只监听 127.0.0.1" });
        }
      } catch (err) {
        setError(String(err));
        setLanOn(!on);
      } finally {
        setLanBusy(false);
        void reload();
      }
    },
    [reload],
  );

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
  //
  // 板块归属按**产品**归并：Qoder 国内/国际各自只有 10 / 14 个模型，拆成两个框
  // 用户得两头找（2026-10-24 要求「不再分国内国际，数量太少，合并显示」）；
  // Trae / WB 各有 40+ 个，仍按国内外分开列。
  //
  // ⚠️ 分组的键是**公开名**的归一化指纹：Qoder 的 `GLM-5.3`(代号 `gmodel`) 与
  //    Trae 的 `glm-5.3` 是同一个模型 ⇒ 合并成一行、各挂一个来源徽章，
  //    点徽章用 `qoder:` / `trae:` 前缀锁定那一家（与原有的裸名自动选号并存）。
  const { sharedModels, ownModels, ownScopes, sharedCount } = useMemo(() => {
    const productOf = (scope: string) => scope.split("-")[0];
    /// 板块 key：Qoder 两个区域并到同一个 `qoder` 框，其余按作用域各自成框。
    const boardOf = (scope: string) => (scope.startsWith("qoder") ? "qoder" : scope);
    type Group = { name: string; display: string; rank: number; scopes: string[] };
    const groups = new Map<string, Group>();
    discovered.forEach((d) => {
      const pub = d.public || publicOf(d.scope, d.model, d.display);
      const key = normModel(pub);
      // 代表名怎么挑：① 公开名已经是人话（Qoder 的 `GLM-5.3`）优先；
      //                ② 有显示名的次之；③ 只剩代号的最后。
      const display = d.display && d.display !== pub ? d.display : pub;
      const rank = pub !== d.model ? 2 : display !== pub ? 1 : 0;
      const g = groups.get(key);
      if (!g) {
        groups.set(key, { name: pub, display, rank, scopes: [d.scope] });
        return;
      }
      if (!g.scopes.includes(d.scope)) g.scopes.push(d.scope);
      if (rank > g.rank) {
        g.name = pub;
        g.display = display;
        g.rank = rank;
      }
    });

    type Row = { key: string; name: string; display: string; scopes: string[] };
    const shared: Row[] = [];
    const own: Record<string, Row[]> = {};
    groups.forEach((g, key) => {
      const scopes = g.scopes.slice().sort();
      const row: Row = { key, name: g.name, display: g.display, scopes };
      if (new Set(scopes.map(productOf)).size >= 2) {
        shared.push(row);
      } else {
        // 同一产品：按板块归并（Qoder 国内+国际并成一框，同名模型只出现一行）。
        // `groups` 已经按公开名指纹合并过，所以这里 push 一次就够了，不会重复。
        (own[boardOf(scopes[0])] ??= []).push(row);
      }
    });
    shared.sort((a, b) => a.name.localeCompare(b.name));
    Object.keys(own).forEach((k) => own[k].sort((a, b) => a.name.localeCompare(b.name)));
    // 板块顺序固定成 Trae → WorkBuddy → Qoder（字母序会让 Qoder 跑最前面，
    // 但它模型最少，摆在头一个反而像主入口）。
    const ORDER = ["trae-cn", "trae-global", "wb-cn", "wb-global", "qoder"];
    const rank = (s: string) => (ORDER.includes(s) ? ORDER.indexOf(s) : 99);
    const ownScopes = Object.keys(own).sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
    // 每个产品有多少模型被**并进「通用模型」**了。
    // 用户报「WB 少了 DeepSeek-V4.1-Flash」就是因为它与 Trae 同名、跑去了通用框，
    // 专有框里只数剩下的 43 个 —— 不给这句提示，看着就像识别漏了。
    const sharedCount: Record<string, number> = {};
    shared.forEach((r) => {
      new Set(r.scopes.map(productOf)).forEach((p) => {
        sharedCount[p] = (sharedCount[p] ?? 0) + 1;
      });
    });
    return { sharedModels: shared, ownModels: own, ownScopes, sharedCount };
  }, [discovered]);

  /// 框头下拉选了一家 ⇒ 通用列表只留这家真正提供的（之前那个下拉只管「复制时加
  /// 什么前缀」，列表照样全显示，用户以为筛选坏了 —— 2026-10-24 反馈）。
  /// 搜索框与下拉是**与**关系：先按产品筛，再按关键字筛。
  const visibleShared = sharedModels.filter((r) => {
    if (copyScope && !r.scopes.some((sc) => productOfScope(sc) === copyScope)) return false;
    const q = modelQuery.trim().toLowerCase();
    if (!q) return true;
    return (
      r.name.toLowerCase().includes(q) ||
      (r.display ?? "").toLowerCase().includes(q) ||
      r.scopes.some((sc) => sc.includes(q))
    );
  });


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
        // 显示名表跟清单一起读：本地缓存里只有代号，不补显示名的话首屏又会
        // 退回 `gmodel` 这种看不懂的名字（Qoder 的公开名就是从这张表来的）。
        const [cached, table] = await Promise.all([
          gatewayModelsGet() as Promise<Record<string, string[]>>,
          gatewayModelLabels().catch(() => ({}) as Record<string, Record<string, string>>),
        ]);
        const rows: Discovered[] = Object.entries(cached ?? {}).flatMap(([scope, list]) =>
          (list ?? []).map((m) => {
            const display = table?.[scope]?.[m] ?? "";
            return {
              scope,
              product: scope.split("-")[0],
              model: m,
              display,
              public: publicOf(scope, m, display),
              source: "本地缓存",
            };
          }),
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

  /// 展示用的主机前缀：开了局域网且**真的探测到地址**才用它（方便复制给其它设备），
  /// 否则用本机回环。绝不把 127.0.0.1 冒充成「局域网地址」。
  const hostBase = useMemo(() => {
    const strip = (u: string) => u.replace(/\/v1\/?$/, "");
    if (status?.lan && status.lanAddr) return strip(status.lanAddr);
    return strip(status?.localAddr ?? `http://127.0.0.1:${status?.port ?? 57892}/v1`);
  }, [status]);

  return (
    <div className="mx-auto flex max-w-5xl flex-col gap-4 px-6 py-6">
      <header className="flex items-start justify-between gap-3">
        <div>
          <h1 className="text-lg font-semibold">积分共享</h1>
          <p className="mt-1 text-xs text-muted-foreground">
            把 Trae / WorkBuddy / Qoder 的账号额度合成一个池子，按「谁先过期就用谁」供给本机其它 AI
            工具。默认只监听本机；打开下面「局域网共享」后同网段设备（NAS、另一台电脑）也能用。
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
        desc={
          status?.lan
            ? "随应用启动，已开给同网段的设备（非本机来源一律强制 Key）"
            : "随应用启动，只绑定 127.0.0.1"
        }
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
            <div className="text-muted-foreground">本机地址</div>
            <div className="mt-0.5 break-all font-mono">{status?.localAddr ?? status?.addr ?? "—"}</div>
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

        {/* 绑定失败要单独说：否则「未启动」和「用户自己关了」长得一模一样 */}
        {status?.bindError && (
          <p className="mt-3 rounded-lg border border-destructive/30 bg-destructive/5 px-3 py-2 text-[11px] text-destructive">
            上次绑定失败：{status.bindError}
          </p>
        )}

        <div className="mt-4 rounded-lg border border-border">
          <label className="flex cursor-pointer items-start gap-2 px-3 py-2.5">
            <input
              type="checkbox"
              className="mt-0.5"
              checked={lanOn}
              disabled={lanBusy}
              onChange={(e) => void toggleLan(e.target.checked)}
            />
            <span className="min-w-0">
              <span className="text-xs font-medium">允许局域网内的其它设备使用（NAS、另一台电脑）</span>
              <span className="mt-0.5 block text-[11px] text-muted-foreground">
                勾上后网关从 127.0.0.1 改绑 0.0.0.0，同网段设备用自己的 Key 调用。
                切换会重启网关，正在生成的对话会中断。
              </span>
            </span>
          </label>

          {lanOn && (
            <div className="space-y-2 border-t border-border px-3 py-2.5">
              {!gwOn ? (
                <p className="text-[11px] text-muted-foreground">
                  积分共享当前是关闭的，设置已保存：打开上面那个总开关后，网关会直接按局域网模式（0.0.0.0）启动。
                </p>
              ) : status?.lanAddr ? (
                <div className="flex items-center gap-2">
                  <div className="min-w-0 flex-1">
                    <div className="text-[11px] text-muted-foreground">局域网地址（给 NAS / 其它设备填）</div>
                    <div className="mt-0.5 break-all font-mono text-xs">{status.lanAddr}</div>
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    className="gap-1.5"
                    onClick={() => void doCopy(status.lanAddr ?? "", "局域网设备填这个 base_url")}
                  >
                    <Copy className="size-3.5" /> 复制
                  </Button>
                </div>
              ) : (
                <p className="text-[11px] text-amber-600">
                  {status?.lanAddrError ?? "没有识别到局域网地址（开关可能还没生效，点右上角刷新）"}
                </p>
              )}
              <p className="text-[11px] text-muted-foreground">
                首次开启时 Windows 可能弹防火墙授权框，<strong>点「允许」</strong>；如果没弹或点了取消，
                局域网设备会连不上（网关自己看不出来），需要到防火墙里放行入站 TCP {status?.port ?? 57892}。
                另外本应用要一直开着（关闭窗口到托盘即可），网关随它起停。
              </p>
              <p className="text-[11px] text-muted-foreground">
                上面地址是按默认路由探测的。如果它显示的<strong>不是你以为的那张网卡</strong>（例如是 VPN 的虚拟网卡），
                在 cmd 里跑 <span className="font-mono">ipconfig</span> 找「以太网/WLAN」的 IPv4 地址，
                端口不变，直接替换 IP 即可。
              </p>
              <p className="text-[11px] text-amber-600">
                风险提示：同网段任何拿到 Key 的软件都能消耗你的积分。建议按设备分开发 Key，不用了立刻吊销；
                只在自己的内网开，别把路由器端口转发到公网。
              </p>
            </div>
          )}
        </div>

        <p className="mt-3 text-xs text-muted-foreground">
          本机客户端填 <span className="font-mono">{status?.localAddr ?? `http://127.0.0.1:${status?.port ?? 57892}/v1`}</span>，
          Key 用下面新建的 <span className="font-mono">sk-ts-…</span>。
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
                {hostBase}/v1/chat/completions
              </div>
              <div className="text-[11px] text-muted-foreground">
                它会把填的地址**原样当作对话端点直接 POST**，必须填到 /chat/completions 这一层。
              </div>
            </div>
            <Button
              variant="outline"
              size="sm"
              className="gap-1.5"
              onClick={() => void doCopy(`${hostBase}/v1/chat/completions`, "填进客户端的接口地址")}
            >
              <Copy className="size-3.5" /> 复制
            </Button>
          </div>
          <div className="flex items-center gap-2 rounded-lg border border-border px-3 py-2">
            <div className="min-w-0 flex-1">
              <div className="font-medium">其他 OpenAI 兼容客户端（Trae 等）填 Base URL</div>
              <div className="truncate font-mono text-[11px] text-muted-foreground">
                {hostBase}/v1
              </div>
              <div className="text-[11px] text-muted-foreground">
                标准 OpenAI 惯例：客户端自己拼 /chat/completions。
              </div>
            </div>
            <Button
              variant="outline"
              size="sm"
              className="gap-1.5"
              onClick={() => void doCopy(`${hostBase}/v1`, "只填到 base_url 的客户端用这个")}
            >
              <Copy className="size-3.5" /> 复制
            </Button>
          </div>
          <p className="text-[11px] text-muted-foreground">
            API Key 在上方创建；模型名在下方「模型」卡片里勾选后点复制，填进客户端。
            {status?.lan && status.lanAddr && (
              <>
                {" "}
                上面是**局域网地址**（给 NAS / 其它设备）；本机自己用{" "}
                <span className="font-mono">{status.localAddr}</span> 也行。
              </>
            )}
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
                  <th className="px-3 py-2 text-left font-medium">用量 / 额度（token ≈ 金额）</th>
                  <th className="px-3 py-2 text-left font-medium">状态</th>
                  <th className="px-3 py-2 text-right font-medium">操作</th>
                </tr>
              </thead>
              <tbody>
                {keys.map((k) => {
                  const used = k.usage?.total_tokens ?? 0;
                  const today = k.usage?.day_tokens ?? 0;
                  const maxTotal = k.quota?.max_total_tokens ?? null;
                  const maxDaily = k.quota?.max_daily_tokens ?? null;
                  const pct = maxTotal ? Math.min(100, Math.round((used / maxTotal) * 100)) : 0;
                  const editing = quotaEdit?.id === k.id;
                  const money = keyMoney(k, pricing);
                  return (
                    <Fragment key={k.id}>
                      <tr className="border-t border-border/60">
                        <td className="px-3 py-2">{k.name}</td>
                        <td className="px-3 py-2 font-mono">
                          {k.prefix}…{k.suffix}
                        </td>
                        <td className="px-3 py-2">
                          <div className="flex items-center gap-1.5">
                            <span className="font-mono">{fmtTok(used)}</span>
                            <span className="text-muted-foreground">
                              / {maxTotal ? fmtTok(maxTotal) : "不限"}
                            </span>
                            {maxTotal ? (
                              <span
                                className={cn(
                                  "text-[10px]",
                                  pct >= 100
                                    ? "text-destructive"
                                    : pct >= 80
                                      ? "text-amber-600"
                                      : "text-muted-foreground",
                                )}
                              >
                                {pct}%
                              </span>
                            ) : null}
                          </div>
                          <div className="text-[10px] text-muted-foreground">
                            今日 {fmtTok(today)}
                            {maxDaily ? ` / ${fmtTok(maxDaily)}` : ""} · {k.usage?.requests ?? 0} 次
                          </div>
                          {money ? (
                            <div className="text-[10px]">
                              <span
                                className={cn(
                                  "font-mono",
                                  money.left <= 0 ? "text-destructive" : "text-foreground/80",
                                )}
                                title={`已花 ${fmtYuan(money.spent)} / 预算 ${fmtYuan(
                                  money.budget,
                                )}（混合单价 ${money.perMtok.toFixed(2)} 元/百万 token）`}
                              >
                                ≈ {fmtYuan(money.left)} 剩余
                              </span>
                              <span className="text-muted-foreground">
                                {" "}
                                · 预算 {fmtYuan(money.budget)}
                              </span>
                            </div>
                          ) : null}
                          {maxTotal ? (
                            <div className="mt-1 h-1 w-24 overflow-hidden rounded bg-muted">
                              <div
                                className={cn(
                                  "h-full",
                                  pct >= 100
                                    ? "bg-destructive"
                                    : pct >= 80
                                      ? "bg-amber-500"
                                      : "bg-emerald-500",
                                )}
                                style={{ width: `${pct}%` }}
                              />
                            </div>
                          ) : null}
                        </td>
                        <td className="px-3 py-2">
                          {k.revoked_at ? (
                            <span className="text-muted-foreground">
                              {k.revoked_reason === "quota" ? "已吊销（额度用尽）" : "已吊销"}
                            </span>
                          ) : (
                            <span className="text-emerald-600">有效</span>
                          )}
                        </td>
                        <td className="px-3 py-2 text-right">
                          <div className="inline-flex flex-wrap justify-end gap-1.5">
                            <button
                              type="button"
                              onClick={() =>
                                setQuotaEdit(
                                  editing
                                    ? null
                                    : {
                                        id: k.id,
                                        total: maxTotal ? String(maxTotal) : "",
                                        daily: maxDaily ? String(maxDaily) : "",
                                      },
                                )
                              }
                              className="rounded-md border border-border px-2 py-1 hover:bg-accent"
                            >
                              额度
                            </button>
                            {k.revoked_at ? (
                              <>
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
                              </>
                            ) : (
                              <button
                                type="button"
                                onClick={() => void gatewayKeyRevoke(k.id).then(reload)}
                                className="rounded-md border border-border px-2 py-1 hover:bg-accent"
                              >
                                吊销
                              </button>
                            )}
                          </div>
                        </td>
                      </tr>
                      {editing && quotaEdit ? (
                        <tr className="border-t border-border/40 bg-muted/20">
                          <td colSpan={5} className="px-3 py-2">
                            <div className="flex flex-wrap items-center gap-2">
                              <label className="text-[11px]">
                                总量上限
                                <input
                                  value={quotaEdit.total}
                                  onChange={(e) =>
                                    setQuotaEdit({ ...quotaEdit, total: e.target.value })
                                  }
                                  inputMode="numeric"
                                  placeholder="不限"
                                  className="ml-1 w-24 rounded border border-border bg-background px-1.5 py-0.5 font-mono text-[11px]"
                                />
                              </label>
                              <label className="text-[11px]">
                                每日上限
                                <input
                                  value={quotaEdit.daily}
                                  onChange={(e) =>
                                    setQuotaEdit({ ...quotaEdit, daily: e.target.value })
                                  }
                                  inputMode="numeric"
                                  placeholder="不限"
                                  className="ml-1 w-24 rounded border border-border bg-background px-1.5 py-0.5 font-mono text-[11px]"
                                />
                              </label>
                              <button
                                type="button"
                                onClick={() => {
                                  const t = Number(quotaEdit.total.trim() || 0);
                                  const d = Number(quotaEdit.daily.trim() || 0);
                                  void gatewayKeySetQuota(
                                    k.id,
                                    t > 0 ? Math.floor(t) : null,
                                    d > 0 ? Math.floor(d) : null,
                                  )
                                    .then(() => {
                                      toast.success("额度已保存");
                                      setQuotaEdit(null);
                                      return reload();
                                    })
                                    .catch((e) =>
                                      toast.error("保存额度失败", { description: String(e) }),
                                    );
                                }}
                                className="rounded-md border border-primary/40 bg-primary/10 px-2 py-1 text-[11px] hover:bg-primary/15"
                              >
                                保存
                              </button>
                              <button
                                type="button"
                                onClick={() =>
                                  void gatewayKeyResetUsage(k.id)
                                    .then(() => {
                                      toast.success("用量已清零");
                                      return reload();
                                    })
                                    .catch((e) =>
                                      toast.error("清零失败", { description: String(e) }),
                                    )
                                }
                                className="rounded-md border border-border px-2 py-1 text-[11px] hover:bg-accent"
                              >
                                用量清零
                              </button>
                              <button
                                type="button"
                                onClick={() => setQuotaEdit(null)}
                                className="rounded-md border border-border px-2 py-1 text-[11px] hover:bg-accent"
                              >
                                取消
                              </button>
                              {pricing && quotaEdit.total.trim() ? (
                                <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] text-foreground/80">
                                  换算 ≈{" "}
                                  {fmtYuan(
                                    yuanOfTokens(
                                      Number(quotaEdit.total) || 0,
                                      pricing.inputPerMtok,
                                    ),
                                  )}{" "}
                                  ~{" "}
                                  {fmtYuan(
                                    yuanOfTokens(
                                      Number(quotaEdit.total) || 0,
                                      pricing.outputPerMtok,
                                    ),
                                  )}
                                  <span className="text-muted-foreground">
                                    （固定高峰价：输入 {pricing.inputPerMtok} / 输出{" "}
                                    {pricing.outputPerMtok} 元每百万 token）
                                  </span>
                                </span>
                              ) : null}
                              <span className="text-[10px] text-muted-foreground">
                                留空 = 不限；到上限会自动吊销这把 Key（提高额度后点「恢复」即可）
                              </span>
                            </div>
                          </td>
                        </tr>
                      ) : null}
                    </Fragment>
                  );
                })}
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
                  {copyScope ? `${visibleShared.length}/${sharedModels.length}` : sharedModels.length}
                </span>
                <select
                  value={copyScope}
                  onChange={(e) => setCopyScope(e.target.value)}
                  className="ml-auto h-5 max-w-[92px] rounded border border-border bg-background px-1 text-[10px] outline-none focus:border-primary"
                  title="按产品筛选，同时决定复制时带哪个前缀：全部 = 裸名（后端自动选号）；选某一家 = 只显示这家有的模型并带前缀锁定"
                >
                  <option value="">全部</option>
                  {FILTER_PRODUCTS.map((p) => (
                    <option key={p.key} value={p.key}>
                      {p.label}
                    </option>
                  ))}
                </select>
              </div>
              {/* 搜索框放在滚动区外面：滚到下面时也能改关键字 */}
              <div className="border-b border-border/60 px-1.5 py-1">
                <input
                  value={modelQuery}
                  onChange={(e) => setModelQuery(e.target.value)}
                  placeholder="搜模型…（如 v4.1 / flash / qwen）"
                  className="h-6 w-full rounded border border-border bg-background px-1.5 text-[10px] outline-none placeholder:text-muted-foreground/60 focus:border-primary"
                />
              </div>
              <div className="max-h-72 overflow-y-auto p-1">
                {visibleShared.length === 0 && (
                  <p className="px-1.5 py-1 text-[10px] text-muted-foreground">
                    {modelQuery.trim()
                      ? `没有匹配「${modelQuery.trim()}」的模型 —— 也可能它只属于某一家，在下面的专有框里。`
                      : "这家没有与其他产品同名的模型 —— 它自己的模型都在下面的专有框里。"}
                  </p>
                )}
                {visibleShared.map(({ key, name, display, scopes }) => {
                  // 筛选的是**产品**，前缀要落到真正提供它的具体作用域（`qoder` 可能
                  // 只有国际版有），否则前缀会把请求指到一个没有这个模型的作用域。
                  const useScope =
                    copyScope && scopes.find((sc) => productOfScope(sc) === copyScope)
                      ? scopes.find((sc) => productOfScope(sc) === copyScope)!
                      : "";
                  // 复制的永远是**公开名**（`GLM-5.3` 而不是 `gmodel`）：发哪一家的哪个代号
                  // 由后端按落到的账号决定，用户只需要认得出这是哪个模型。
                  const toCopy = useScope ? `${SCOPE_ALIAS[useScope]}:${name}` : name;
                  return (
                    // ⚠️ 这里**不能**用整行 `<button>` 包住来源徽章：徽章本身要是按钮
                    //    （点它就锁定那一家），button 套 button 是非法 HTML，点了会串行为。
                    // ⚠️ 卡片窄（窗口小 / 两列）时徽章会顶出边界被 `overflow-hidden` 裁掉，
                    //    右边的复制图标就点不到了 ⇒ 整行 `flex-wrap`，放不下自动落到第二行。
                    <div
                      key={key}
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
                        className="min-w-0 flex-1 basis-[72px] truncate text-left"
                      >
                        <ModelName display={display} name={name} />
                      </button>
                      <span className="flex shrink-0 flex-wrap items-center justify-end gap-0.5">
                        {scopes.map((sc) => (
                          <button
                            key={sc}
                            type="button"
                            onClick={() =>
                              void doCopy(
                                `${SCOPE_ALIAS[sc] ?? sc}:${name}`,
                                `锁定 ${SCOPE_LABEL[sc] ?? sc} —— 只走这一家的积分`,
                              )
                            }
                            title={`点一下复制 ${SCOPE_ALIAS[sc] ?? sc}:${name}（锁定 ${SCOPE_LABEL[sc] ?? sc}）`}
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
                  下拉选<span className="text-foreground">全部</span>＝只列所有模型、复制裸名（后端自动选号）；
                  选某一家＝
                  <span className="text-foreground">只列这家有的模型</span>并带前缀锁定（如
                  <span className="font-mono">qoder:GLM-5.3</span>）。点
                  <span className="text-foreground"> Trae / WB / Qoder </span>徽章也能单独锁那一家
                </div>
              </div>

            {/* 专有板块（Qoder 国内+国际已并成一框） */}
            {ownScopes.map((board) => (
              <div key={board} className="flex min-w-0 flex-col overflow-hidden rounded-lg border border-border">
                <div className="flex items-center gap-1.5 border-b border-border/60 bg-muted/40 px-2.5 py-1.5">
                  <span className="text-[11px] font-medium">
                    {OWN_LABEL[board] ?? SCOPE_LABEL[board] ?? board}
                  </span>
                  <span className="rounded bg-background px-1 text-[10px] text-muted-foreground">
                    {ownModels[board].length}
                  </span>
                  <span className="ml-auto text-[10px] text-muted-foreground">专有</span>
                </div>
                <div className="max-h-72 overflow-y-auto p-1">
                  {ownModels[board].map(({ key, name, display }) => (
                    // 专有块只有一个动作：点右边的复制按钮复制裸名。
                    // ⚠️ 以前名字本身是按钮、右边那个 `<Copy>` 只是装饰（没有 onClick）——
                    //    用户照着形状点图标，一点反应都没有（2026-10-24 反馈）。
                    //    现在名字是**纯文本**，唯一的交互就是那个真按钮。
                    <div
                      key={key}
                      className="flex w-full items-center gap-1 rounded-md px-1.5 py-1 text-[11px] hover:bg-accent"
                    >
                      <span className="min-w-0 flex-1 truncate">
                        <ModelName display={display} name={name} />
                      </span>
                      <button
                        type="button"
                        onClick={() =>
                          void doCopy(
                            name,
                            `${OWN_LABEL[board] ?? board} 专有模型：只有这一家提供，直接裸名`,
                          )
                        }
                        title={`复制 ${name}`}
                        className="grid size-5 shrink-0 place-items-center rounded text-muted-foreground/60 transition-colors hover:bg-primary/10 hover:text-primary"
                      >
                        <Copy className="size-3" />
                      </button>
                    </div>
                  ))}
                </div>
                {/* 「WB 少了某个模型」的真相：它与别的产品同名，被并进上面的通用框了。
                    不写这一句，用户只会以为识别漏读。 */}
                {(sharedCount[productOfScope(board)] ?? 0) > 0 && (
                  <div className="border-t border-border/60 px-2 py-1 text-[10px] leading-4 text-muted-foreground">
                    另有 {sharedCount[productOfScope(board)]} 个与其它产品同名的模型已并入
                    <span className="text-foreground">「通用模型」</span>（带本产品徽章），
                    这里只列它独有的。
                  </div>
                )}
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
            placeholder="GLM-5.3 或 qoder:GLM-5.3"
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
