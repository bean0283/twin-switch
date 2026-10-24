import { MessagesSquare, UserRound } from "lucide-react";

import { HubCard } from "@/components/hub-card";

/**
 * 「账号及会话管理」分组首页。
 *
 * 侧栏一级项只有 5 个，三个产品的账号/会话都在这个分组里 ——
 * 避免侧栏被 20 多个入口撑成一长条（用户反馈的就是这个）。
 */
export default function AccountsHubPage() {
  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">账号及会话管理</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          Trae / WorkBuddy / Qoder 三个产品的账号与本地会话都收在这里：切号、导入登录态、
          浏览会话、编辑与删除。
        </p>
      </div>

      <section className="space-y-2">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground">Trae</h2>
        <div className="grid gap-2 sm:grid-cols-2">
          <HubCard
            to="/trae-switch"
            icon={<UserRound className="size-4" />}
            title="Trae 账号管理"
            desc="切换账号、导入本机登录态、看积分与到期时间"
          />
          <HubCard
            to="/trae-records"
            icon={<MessagesSquare className="size-4" />}
            title="Trae 会话记录"
            desc="浏览本机 Trae 会话；解密后可看正文、导出"
          />
        </div>
      </section>

      <section className="space-y-2">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground">WorkBuddy</h2>
        <div className="grid gap-2 sm:grid-cols-2">
          <HubCard
            to="/workbuddy-switch"
            icon={<UserRound className="size-4" />}
            title="WorkBuddy 账号管理"
            desc="扫码添加、导入登录态、切换与积分"
          />
          <HubCard
            to="/workbuddy-records"
            icon={<MessagesSquare className="size-4" />}
            title="WorkBuddy 会话记录"
            desc="浏览会话、编辑标题与正文、删除"
          />
        </div>
      </section>

      <section className="space-y-2">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground">Qoder</h2>
        <div className="grid gap-2 sm:grid-cols-2">
          <HubCard
            to="/qoder-accounts"
            icon={<UserRound className="size-4" />}
            title="Qoder 账号管理"
            desc="国内版 / 国际版分开管理，积分与签到状态"
          />
          <HubCard
            to="/qoder-sessions"
            icon={<MessagesSquare className="size-4" />}
            title="Qoder 会话记录"
            desc="按区域查看会话，编辑与真删（写前备份）"
          />
        </div>
      </section>
    </div>
  );
}
