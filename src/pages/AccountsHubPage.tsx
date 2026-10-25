import { UserRound } from "lucide-react";

import { HubCard } from "@/components/hub-card";

/**
 * 「账号管理」分组首页。
 *
 * 侧栏一级项从原来的「账号及会话管理」拆成了两个分组：账号归账号、会话归会话
 * （用户反馈：一个分组里 6 个入口、账号和会话混在一起，点错率高）。
 * 这里只放**账号**相关入口；会话在 `/sessions`（`SessionsHubPage`）。
 */
export default function AccountsHubPage() {
  return (
    <div className="mx-auto flex min-h-full w-full max-w-6xl flex-col gap-5 px-4 py-6">
      <div>
        <h1 className="text-xl font-semibold tracking-tight">账号管理</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          Trae / WorkBuddy / Qoder 三个产品的账号：切换、导入本机登录态、看积分与到期时间。
          会话不在这一组，在「会话管理」里。
        </p>
      </div>

      <section className="space-y-2">
        <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
          <HubCard
            to="/trae-switch"
            icon={<UserRound className="size-4" />}
            title="Trae 账号管理"
            desc="切换账号、导入本机登录态、看积分与到期时间"
          />
          <HubCard
            to="/workbuddy-switch"
            icon={<UserRound className="size-4" />}
            title="WorkBuddy 账号管理"
            desc="扫码添加、导入登录态、切换与积分"
          />
          <HubCard
            to="/qoder-accounts"
            icon={<UserRound className="size-4" />}
            title="Qoder 账号管理"
            desc="国内版 / 国际版分开管理，积分与签到状态"
          />
        </div>
      </section>
    </div>
  );
}
