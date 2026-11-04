import { cn } from "@/lib/utils";

/**
 * 药丸分段控件（页内切换用）。
 *
 * 与 [`@/components/gateway-tabs`] 共用同一套外观常量，所以「一级页签」和「二级页签」
 * 看起来是一家人；两者的尺寸都是硬约束：`h-8` + `p-0.5`，和页面右上角的胶囊按钮同高。
 *
 * 为什么不用 shadcn 的 Tabs：默认样式是下划线那一套，放进这套界面会显得像另一个产品；
 * 而且它的高度随字号浮动，两个页面之间会对不齐（用户 2026-11-04 报过切换条错位）。
 */
export const PILL_GROUP =
  "inline-flex h-8 w-fit shrink-0 items-center gap-0.5 self-start rounded-lg border border-border bg-muted/30 p-0.5";

/** 单个药丸的类名。选中态用「白底 + 细投影」从灰底里浮出来。 */
export function pillItemClass(active: boolean): string {
  return cn(
    "flex h-full items-center rounded-md px-3 text-xs transition-colors",
    active
      ? "bg-background font-medium text-foreground shadow-xs"
      : "text-muted-foreground hover:text-foreground",
  );
}

export interface PillTabItem<T extends string> {
  id: T;
  label: string;
}

/**
 * 受控的页内分段。
 *
 * ⚠️ 切换时**自身宽度必须不变**（各段文字长度尽量接近），否则鼠标会跟着标签一起跑。
 */
export function PillTabs<T extends string>({
  items,
  value,
  onChange,
  ariaLabel,
}: {
  items: readonly PillTabItem<T>[];
  value: T;
  onChange: (id: T) => void;
  ariaLabel?: string;
}) {
  return (
    <div role="tablist" aria-label={ariaLabel} className={PILL_GROUP}>
      {items.map((it) => {
        const active = it.id === value;
        return (
          <button
            key={it.id}
            type="button"
            role="tab"
            aria-selected={active}
            onClick={() => onChange(it.id)}
            className={pillItemClass(active)}
          >
            {it.label}
          </button>
        );
      })}
    </div>
  );
}
