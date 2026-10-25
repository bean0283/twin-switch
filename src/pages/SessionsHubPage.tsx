import { MessagesSquare } from "lucide-react";

import { HubCard } from "@/components/hub-card";

/**
 * 「会话管理」分组首页。
 *
 * 与「账号管理」分开：这里只放**本地会话**入口（浏览 / 搜索 / 按空间分组 / 编辑 / 删除）。
 * 三个产品的会话页共用同一套交互（搜索、归属工作空间、按空间分类）。
 */
export default function SessionsHubPage() {
  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">会话管理</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          Trae / WorkBuddy / Qoder 的本地会话：搜索标题与空间、看归属工作空间、按空间分类，
          以及编辑与删除。账号切换在「账号管理」里。
        </p>
      </div>

      <section className="space-y-2">
        <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
          <HubCard
            to="/trae-records"
            icon={<MessagesSquare className="size-4" />}
            title="Trae 会话记录"
            desc="解密后浏览正文、按空间分组、导出与真删"
          />
          <HubCard
            to="/workbuddy-records"
            icon={<MessagesSquare className="size-4" />}
            title="WorkBuddy 会话记录"
            desc="浏览会话、按空间分组、编辑标题与正文、删除"
          />
          <HubCard
            to="/qoder-sessions"
            icon={<MessagesSquare className="size-4" />}
            title="Qoder 会话记录"
            desc="按区域查看会话、按空间分组、编辑与真删（写前备份）"
          />
        </div>
      </section>
    </div>
  );
}
