import { cn } from "@/lib/utils";

/**
 * 客户端切换条的一项。只保留两个页面都能提供的字段，
 * 免得组件被某一端的线上数据结构绑住。
 */
export interface TraeClientOption {
  key: string;
  label: string;
  /** 本机未安装时置灰不可点。 */
  installed: boolean;
  /** 该客户端当前已登录 —— 戴上绿点。 */
  hasLogin: boolean;
  /** 是否戴「常用」徽标；只能有一个，且必须来自后端的使用记忆排序。 */
  top?: boolean;
  /**
   * 已并入哪个客户端（key）。`null`/缺省 = 仍是独立客户端。
   *
   * 2026-10 官方把 TRAE SOLO 并进 Trae CN：只剩一个程序，SOLO 的数据目录留在原处。
   * 这类客户端**仍然可以选**（旧会话要能看、能迁），但要明确告诉用户
   * 「切号请到承载方那边去」—— 否则用户会以为切了 SOLO，其实动的是另一个库。
   */
  mergedInto?: string | null;
  /**
   * 本机能不能真把它跑起来（数据目录在 **且** 找得到主程序）。
   *
   * ⚠️ 与 `installed` 不是一回事：合并后的 SOLO `installed=true`（旧数据还在）
   * 但 `runnable=false`（程序已卸载）。别拿一个当另一个用。
   */
  runnable?: boolean;
}

/**
 * Trae 客户端切换条：**本机有几个客户端就画几个标签**。
 *
 * ⚠️ 首页（`HomePage`）与「Trae 会话记录」（`TraeRecordsPage`）必须共用这一个组件。
 *    客户端顺序、「常用」徽标、未安装置灰这三件事若在两端各写一份，必然有一端先跑偏；
 *    而现在两处看到的顺序与徽标本身就是同一份后端数据（`client_usage::order_keys()` /
 *    `snapshot_for()` 的 `topPick`）。
 *
 * ⚠️ 切换只决定「看哪一个客户端」，**绝不合并任何数字**：每个客户端的账号库、会话库、
 *    积分都只从它自己的 key 算。
 */
export function TraeClientSwitcher({
  clients,
  value,
  onChange,
  disabled,
  label = "Trae 客户端",
}: {
  clients: TraeClientOption[];
  value: string | null;
  onChange: (key: string) => void;
  /** 有耗时操作在跑时整条禁用，避免切到一半换客户端。 */
  disabled?: boolean;
  label?: string;
}) {
  if (clients.length === 0) return null;

  return (
    <div className="flex flex-wrap items-center gap-2" role="group" aria-label={label}>
      {clients.map((c) => {
        const active = c.key === value;
        // 承载方的显示名（"已并入 X" 里的 X）。
        const hostLabel = c.mergedInto
          ? (clients.find((x) => x.key === c.mergedInto)?.label ?? c.mergedInto)
          : "";
        return (
          <button
            key={c.key}
            type="button"
            disabled={disabled || !c.installed}
            aria-pressed={active}
            title={
              c.mergedInto
                ? `已并入 ${hostLabel}：本机只剩一个程序，数据目录仍各自保留。切换账号请到「${hostLabel}」下操作。`
                : c.top
                  ? "按使用记忆自动排在最前（切换账号次数 ×3 + 打开页面次数）"
                  : c.installed
                    ? undefined
                    : "本机未安装这个客户端"
            }
            onClick={() => onChange(c.key)}
            className={cn(
              "flex items-center gap-2 rounded-lg border px-3 py-2 text-sm transition-colors",
              active
                ? "border-foreground/20 bg-foreground/[0.06] font-medium"
                : "border-border bg-background hover:bg-foreground/[0.03]",
              (!c.installed || disabled) && "cursor-not-allowed opacity-40",
            )}
          >
            {c.label}
            {c.mergedInto ? (
              <span className="rounded bg-amber-500/[0.15] px-1.5 py-0.5 text-[10px] font-normal text-amber-700">
                已并入
              </span>
            ) : null}
            {c.top ? (
              <span className="rounded bg-foreground/[0.08] px-1.5 py-0.5 text-[10px] font-normal text-muted-foreground">
                常用
              </span>
            ) : null}
            {c.installed && c.hasLogin ? (
              <span className="size-2 rounded-full bg-emerald-500" aria-hidden="true" />
            ) : null}
          </button>
        );
      })}
    </div>
  );
}
