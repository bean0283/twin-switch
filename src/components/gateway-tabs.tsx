import { Link, useLocation } from "react-router-dom";

import { cn } from "@/lib/utils";

/**
 * 「积分共享 / 实时监控」的分页切换。
 *
 * ## 为什么不做成两个侧栏一级入口
 *
 * 侧栏的约定是**六项一级导航**（首页 / 账号管理 / 会话管理 / 数据迁移 / 积分共享 / 设置），
 * 多塞一项会把那个结构打乱。这两页本来也是同一件事的两面（配置 vs 观测），
 * 用页内分段切换更贴切。
 *
 * ## 尺寸是硬约束（别改）
 *
 * 这个控件必须落在两页**完全相同**的像素位置，否则来回切的时候鼠标会按空
 * （2026-11-04 用户报「按钮不在一个水平面，切换后老是错位按错」）。所以：
 *
 * - 高度固定 `h-8`（32px），和本页右上的胶囊按钮同高；
 * - `w-fit self-start`：放在 flex-col 的 header 里不被拉成整行宽；
 * - 两个分段的左右内边距固定，切换时**自身宽度不变**（标签字数一样）。
 *
 * 位置本身由两页共用的页头骨架保证：标题行(h-8) → 描述(min-h-8, 最多两行) → 本控件。
 * 任何一页在这三条前面加东西，都会把切换条推歪。
 */
export function GatewayTabs() {
  const { pathname } = useLocation();
  const items = [
    { to: "/gateway", label: "积分共享" },
    { to: "/gateway/live", label: "实时监控" },
  ];
  return (
    <nav
      aria-label="积分共享视图"
      className="inline-flex h-8 w-fit shrink-0 items-center gap-0.5 self-start rounded-lg border border-border bg-muted/30 p-0.5"
    >
      {items.map((it) => {
        const active = pathname === it.to;
        return (
          <Link
            key={it.to}
            to={it.to}
            aria-current={active ? "page" : undefined}
            className={cn(
              "flex h-full items-center rounded-md px-3 text-xs transition-colors",
              active
                ? "bg-background font-medium text-foreground shadow-xs"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {it.label}
          </Link>
        );
      })}
    </nav>
  );
}
