import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { ArrowRight } from "lucide-react";

/** 分组首页的入口卡（账号 / 会话、迁移 / 清理）。 */
export function HubCard({
  to,
  icon,
  title,
  desc,
}: {
  to: string;
  icon: ReactNode;
  title: string;
  desc: string;
}) {
  return (
    <Link
      to={to}
      className="group flex items-start gap-3 rounded-xl border bg-card px-4 py-3 transition-colors hover:bg-accent/50"
    >
      <span className="mt-0.5 grid size-9 shrink-0 place-items-center rounded-lg border bg-muted/40 text-muted-foreground">
        {icon}
      </span>
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-1.5 text-sm font-medium">
          {title}
          <ArrowRight className="size-3.5 opacity-0 transition-opacity group-hover:opacity-60" />
        </span>
        <span className="mt-0.5 block text-xs text-muted-foreground">{desc}</span>
      </span>
    </Link>
  );
}
