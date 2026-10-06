import { CheckCircle2, Loader2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { avatarTone } from "@/lib/avatar-tone";
import { cn } from "@/lib/utils";

/**
 * 账号卡的统一外观。
 *
 * WorkBuddy 与 Trae 两个账号管理页共用这一套 —— 布局（圆形头像 + 名称 + 身份 +
 * 状态徽标，右上角一排图标按钮，下面整块放积分）在这里**只实现一次**，
 * 两侧的差别只在于传进来的数据。
 *
 * 观感对齐参考项目 `workbuddy-switch` 的 `account-card.tsx`：
 * 同一个账号不需要上下对照两处，就能看完「是谁、还剩多少、能做什么」。
 */

/** 卡片顶部的圆形头像：有头像图就用图，否则按名字哈希取固定色调。 */
export function Avatar({
  name,
  url,
  size = "md",
}: {
  name: string;
  /** 真实头像 URL（Trae 侧有）；取不到时回落到首字母色块。 */
  url?: string | null;
  size?: "md" | "sm";
}) {
  const box = size === "md" ? "size-11 text-base" : "size-9 text-sm";
  if (url) {
    return (
      <img
        src={url}
        alt=""
        className={cn("shrink-0 rounded-full object-cover", box)}
        referrerPolicy="no-referrer"
      />
    );
  }
  const initial = (name || "?").trim().charAt(0).toUpperCase();
  return (
    <div
      className={cn(
        "flex shrink-0 items-center justify-center rounded-full font-semibold",
        box,
        avatarTone(name),
      )}
      aria-hidden="true"
    >
      {initial}
    </div>
  );
}

/** 一个状态小徽标：比默认 Badge 更矮更紧凑，避免把卡片撑高。 */
export function Chip({
  children,
  tone = "muted",
}: {
  children: React.ReactNode;
  tone?: "muted" | "ok" | "warn" | "err" | "outline";
}) {
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-md px-1.5 py-0 text-[11px] leading-5 font-medium",
        tone === "ok" && "bg-emerald-500/15 text-emerald-700",
        tone === "warn" && "bg-amber-500/15 text-amber-700",
        tone === "err" && "bg-destructive/10 text-destructive",
        tone === "muted" && "bg-muted text-muted-foreground",
        tone === "outline" && "border border-border text-muted-foreground",
      )}
    >
      {children}
    </span>
  );
}

/** 卡片右上角的小图标按钮（size-8 圆角方按钮，与参考项目一致）。 */
export function IconAction({
  icon,
  label,
  onClick,
  disabled,
  destructive,
  active,
  spinning,
}: {
  icon: React.ReactNode;
  label: string;
  onClick?: () => void;
  disabled?: boolean;
  destructive?: boolean;
  /** 已生效态（如「当前登录」的切换按钮）：不再是可点的按钮，而是高亮的徽标。 */
  active?: boolean;
  spinning?: boolean;
}) {
  if (active) {
    return (
      <span
        className="relative inline-flex size-8 items-center justify-center rounded-lg border border-primary/25 bg-primary/10 text-primary"
        title={label}
        aria-label={label}
      >
        {icon}
        <span className="absolute -top-1 -right-1 flex size-3.5 items-center justify-center rounded-full bg-primary text-primary-foreground">
          <CheckCircle2 className="size-2.5" strokeWidth={3} />
        </span>
      </span>
    );
  }
  return (
    <Button
      type="button"
      variant="outline"
      size="icon"
      className={cn("size-8 rounded-lg", destructive && "text-muted-foreground hover:text-destructive")}
      onClick={onClick}
      disabled={disabled}
      title={label}
      aria-label={label}
    >
      {spinning ? <Loader2 className="size-4 animate-spin" /> : icon}
    </Button>
  );
}

/**
 * 账号卡外壳。
 *
 * @param name     显示名（同时决定头像文字与色调）
 * @param identity 身份行（邮箱 / uid），由调用方决定怎么脱敏
 * @param chips    状态徽标（当前登录 / 无凭据 / 即将过期…）
 * @param actions  右上角图标按钮
 * @param children 卡片主体（积分块）
 */
export function AccountCard({
  name,
  avatarUrl,
  identity,
  identityTitle,
  identityMono,
  chips,
  actions,
  isCurrent,
  children,
}: {
  name: string;
  avatarUrl?: string | null;
  identity: React.ReactNode;
  /** 身份行的 `title`（截断时鼠标悬停看全文）。 */
  identityTitle?: string;
  /** 身份行是否用等宽字体（uid 用，邮箱不用）。 */
  identityMono?: boolean;
  chips?: React.ReactNode;
  actions?: React.ReactNode;
  isCurrent?: boolean;
  children?: React.ReactNode;
}) {
  return (
    <article
      className={cn(
        "flex min-w-0 flex-col overflow-hidden rounded-2xl border bg-card transition-shadow hover:shadow-md",
        isCurrent && "border-primary/40",
      )}
    >
      <header
        className={cn(
          // `flex-wrap` + 身份列的 `basis-28`（7rem 下限）是**必需的**，不是装饰：
          // 右上角操作栏（切换 / 改名 / 删除，两侧同一套 3 个图标）在窄卡片里若放不下，
          // 会整体换到第二行；没有 `flex-wrap` 时 flex 会先把身份列压到 0 ——
          // 表现就是「账号名与 uid 整行消失、状态徽标被挤成一字一行、手机号溢出盖住按钮」。
          "relative flex min-w-0 flex-wrap items-center gap-x-3 gap-y-2 border-b px-4 py-3",
          isCurrent ? "bg-primary/5" : "bg-muted/30",
        )}
      >
        <Avatar name={name} url={avatarUrl} />
        {/* 用 `grow` 而不是 `flex-1`：`flex-1` 会写 `flex:1 1 0%` 这个**简写**，
            与 `basis-*` 争同一个 `flex-basis`，谁生效取决于样式表顺序（目前 basis 在后，
            但没必要押注这个）。`grow` 只写 `flex-grow`，与 basis 不冲突。 */}
        <div className="min-w-0 grow basis-28">
          <h3 className="truncate text-sm font-semibold" title={name}>
            {name}
          </h3>
          <p
            className={cn("mt-0.5 truncate text-xs text-muted-foreground", identityMono && "font-mono")}
            title={identityTitle}
          >
            {identity}
          </p>
          {chips ? <div className="mt-1.5 flex min-w-0 flex-wrap items-center gap-1.5">{chips}</div> : null}
        </div>
        {actions ? (
          // `min-w-0` + `flex-wrap`：极端窄卡片下按钮在**自己的行内**换行，不会撑破卡片。
          <div className="ml-auto flex min-w-0 flex-wrap items-center justify-end gap-1.5">
            {actions}
          </div>
        ) : null}
      </header>

      <section className="flex min-w-0 flex-1 flex-col px-4 pt-3.5 pb-4">{children}</section>
    </article>
  );
}
