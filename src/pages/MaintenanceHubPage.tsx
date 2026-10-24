import { Eraser, HardDriveDownload, HardDriveUpload } from "lucide-react";

import { HubCard } from "@/components/hub-card";

/** 「数据迁移及维护」分组首页：两个迁移方向 + 三家清理。 */
export default function MaintenanceHubPage() {
  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">数据迁移及维护</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          在 Trae 与 WorkBuddy 之间搬会话，以及清理三个产品的本机数据（缓存、备份、回收站）。
        </p>
      </div>

      <section className="space-y-2">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground">数据迁移</h2>
        <div className="grid gap-2 sm:grid-cols-2">
          <HubCard
            to="/workbuddy-import"
            icon={<HardDriveDownload className="size-4" />}
            title="WorkBuddy → Trae"
            desc="把 WorkBuddy 的会话导入 Trae（保留时间与正文）"
          />
          <HubCard
            to="/workbuddy-export"
            icon={<HardDriveUpload className="size-4" />}
            title="Trae → WorkBuddy"
            desc="把 Trae 的会话导出到 WorkBuddy"
          />
        </div>
      </section>

      <section className="space-y-2">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground">本机维护</h2>
        <div className="grid gap-2 sm:grid-cols-2">
          <HubCard
            to="/workbuddy-cleanup"
            icon={<Eraser className="size-4" />}
            title="WorkBuddy 清理"
            desc="缓存、日志、失效会话与备份；默认移入回收站"
          />
          <HubCard
            to="/trae-cleanup"
            icon={<Eraser className="size-4" />}
            title="Trae 清理"
            desc="会话、备份、解密快照与客户端缓存；可清空回收站"
          />
          <HubCard
            to="/qoder-cleanup"
            icon={<Eraser className="size-4" />}
            title="Qoder 清理"
            desc="缓存、崩溃转储、写库备份与回收站（国内/国际分开）"
          />
        </div>
      </section>
    </div>
  );
}
