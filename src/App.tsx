import { useEffect, useState, type ReactNode } from "react";
import {
  BrowserRouter,
  Navigate,
  NavLink,
  Outlet,
  Route,
  Routes,
  useLocation,
} from "react-router-dom";
import { ArrowLeft, Home, MessagesSquare, Settings, Share2, Users, Wrench } from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";

import { cn } from "@/lib/utils";
import HomePage from "@/pages/HomePage";
import AccountsHubPage from "@/pages/AccountsHubPage";
import SessionsHubPage from "@/pages/SessionsHubPage";
import MaintenanceHubPage from "@/pages/MaintenanceHubPage";
import TraeSwitchPage from "@/pages/TraeSwitchPage";
import TraeRecordsPage from "@/pages/TraeRecordsPage";
import WorkbuddyImportPage from "@/pages/WorkbuddyImportPage";
import WorkbuddyExportPage from "@/pages/WorkbuddyExportPage";
import WorkbuddyCleanupPage from "@/pages/WorkbuddyCleanupPage";
import QoderCleanupPage from "@/pages/QoderCleanupPage";
import TraeCleanupPage from "@/pages/TraeCleanupPage";
import SettingsPage from "@/pages/SettingsPage";
import GatewayPage from "@/pages/GatewayPage";
import GatewayGuidePage from "@/pages/GatewayGuidePage";
import QoderAccountsPage from "@/pages/QoderAccountsPage";
import QoderSessionsPage from "@/pages/QoderSessionsPage";
import WorkbuddySwitchPage from "@/pages/WorkbuddySwitchPage";
import WorkbuddyRecordsPage from "@/pages/WorkbuddyRecordsPage";
import { AppIconMark } from "@/components/product-marks";
import { UpdateEntry } from "@/components/update-entry";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";

/**
 * 侧栏**两级导航**。
 *
 * 一级只有 6 项（首页 / 账号管理 / 会话管理 / 数据迁移及维护 / 积分共享 / 设置）；
 * 三个产品的账号与会话、迁移与清理都收进分组，避免侧栏被 20 多个入口拉成一长条
 * （用户反馈的原问题）。进入某组后，该组自动展开二级项，其余组保持收起。
 */
type NavChild = { to: string; label: string };
type NavGroup = {
  key: string;
  label: string;
  icon: ReactNode;
  root: string;
  children: NavChild[];
};

const NAV_GROUPS: NavGroup[] = [
  {
    // 账号与会话**拆成两组**（用户要求）：一个分组里 6 个入口、账号和会话混排，点错率高。
    key: "accounts",
    label: "账号管理",
    icon: <Users className="size-4" />,
    root: "/accounts",
    children: [
      { to: "/trae-switch", label: "Trae 账号" },
      { to: "/workbuddy-switch", label: "WorkBuddy 账号" },
      { to: "/qoder-accounts", label: "Qoder 账号" },
    ],
  },
  {
    key: "sessions",
    label: "会话管理",
    icon: <MessagesSquare className="size-4" />,
    root: "/sessions",
    children: [
      { to: "/trae-records", label: "Trae 会话" },
      { to: "/workbuddy-records", label: "WorkBuddy 会话" },
      { to: "/qoder-sessions", label: "Qoder 会话" },
    ],
  },
  {
    key: "maintenance",
    label: "数据迁移及维护",
    icon: <Wrench className="size-4" />,
    root: "/migrate",
    children: [
      { to: "/workbuddy-import", label: "WorkBuddy → Trae" },
      { to: "/workbuddy-export", label: "Trae → WorkBuddy" },
      { to: "/workbuddy-cleanup", label: "WorkBuddy 清理" },
      { to: "/trae-cleanup", label: "Trae 清理" },
      { to: "/qoder-cleanup", label: "Qoder 清理" },
    ],
  },
];

/** 侧栏类别之间的小横杠（比分组标题时代的粗线更细，只做视觉分组）。 */
function NavDivider() {
  return <div className="my-1.5 h-px shrink-0 bg-sidebar-border" role="presentation" />;
}

/** 二级项（含分组首页本身）的链接样式：比一级小一号、带左侧竖线。 */
function SidebarSubLink({ to, label }: NavChild) {
  return (
    <NavLink
      to={to}
      className={({ isActive }) =>
        cn(
          "block truncate rounded-md px-2 py-1.5 text-[13px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring/50",
          isActive
            ? "bg-foreground/[0.06] font-medium text-foreground"
            : "text-muted-foreground hover:bg-foreground/[0.04] hover:text-foreground",
        )
      }
    >
      {label}
    </NavLink>
  );
}

/** 侧栏导航项：统一激活态样式，避免每个入口重复一份 className 计算。 */
function SidebarLink({
  to,
  end,
  icon,
  label,
  active,
}: {
  to: string;
  end?: boolean;
  icon: ReactNode;
  label: string;
  /** 外部强制指定激活态（分组头在子页里也要高亮，而不是只看精确路径）。 */
  active?: boolean;
}) {
  return (
    <NavLink
      to={to}
      end={end}
      className={({ isActive }) =>
        cn(
          "flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring/50",
          (active ?? isActive)
            ? "bg-foreground/[0.06] font-medium text-foreground"
            : "text-muted-foreground hover:bg-foreground/[0.04] hover:text-foreground",
        )
      }
    >
      {icon}
      {label}
    </NavLink>
  );
}

function Layout() {
  const [version, setVersion] = useState("");
  const { pathname } = useLocation();
  /** 当前是否在某个分组里（含分组首页）—— 决定「首页」是否变成「返回」。 */
  const activeGroup = NAV_GROUPS.find(
    (g) => pathname === g.root || g.children.some((c) => c.to === pathname),
  );
  const inGroup = Boolean(activeGroup);
  const hasUnifiedTitleBar =
    typeof navigator !== "undefined" && navigator.userAgent.includes("Macintosh");

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

  return (
    <div className="flex h-screen min-h-0 overflow-hidden bg-background">
      {hasUnifiedTitleBar ? (
        <div
          data-tauri-drag-region
          className="fixed inset-x-0 top-0 z-50 h-8"
          aria-hidden="true"
        />
      ) : null}
      <aside
        className={cn(
          "flex min-h-0 w-[220px] shrink-0 flex-col border-r border-sidebar-border bg-sidebar px-3 pb-4",
          hasUnifiedTitleBar ? "pt-20" : "pt-4",
        )}
      >
        <div className="flex items-center gap-2.5 px-1 pb-5">
          <AppIconMark size={36} className="drop-shadow-sm" />
          <div className="min-w-0">
            <div
              className="truncate text-[15px] leading-5 tracking-[-0.02em] text-sidebar-foreground/90"
              style={{
                fontFamily: '"Bricolage Grotesque Variable", "SF Pro Display", ui-sans-serif, sans-serif',
                fontWeight: 640,
              }}
            >
              TwinSwitch
            </div>
            {/* 侧栏只有 220px（内容 196px），副标题必须能截断：中文标签一长就会顶破这一行。 */}
            <div
              className="truncate text-[11px] leading-4 text-sidebar-foreground/45"
              title="双栖 · 账号与会话管理"
            >
              双栖 · 账号与会话管理
            </div>
          </div>
        </div>
        <nav
          className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto"
          aria-label="主导航"
        >
          {/* 在分组内时，「首页」变成「返回」—— 一眼就知道能退回上一层 */}
          <SidebarLink
            to="/"
            end
            icon={inGroup ? <ArrowLeft className="size-4" /> : <Home className="size-4" />}
            label={inGroup ? "返回" : "首页"}
          />

          {/* 类别之间用小横杠分开：首页/返回 | 账号管理 | 会话管理 | 数据迁移及维护 | 积分共享·设置 */}
          <NavDivider />

          {NAV_GROUPS.map((g, i) => {
            const open = activeGroup?.key === g.key;
            return (
              <div key={g.key} className="flex flex-col gap-0.5">
                {i > 0 ? <NavDivider /> : null}
                <SidebarLink to={g.root} icon={g.icon} label={g.label} active={open} />
                {open ? (
                  // 二级：左侧竖线 + 缩进，数量固定（账号组 6 个、维护组 5 个）
                  <div className="mt-0.5 ml-4 flex flex-col gap-0.5 border-l border-sidebar-border pl-2">
                    {g.children.map((c) => (
                      <SidebarSubLink key={c.to} to={c.to} label={c.label} />
                    ))}
                  </div>
                ) : null}
              </div>
            );
          })}

          <NavDivider />
          <SidebarLink to="/gateway" icon={<Share2 className="size-4" />} label="积分共享" />
          <SidebarLink to="/settings" icon={<Settings className="size-4" />} label="设置" />
        </nav>
        <UpdateEntry fallbackVersion={version} />
      </aside>
      <main
        className={cn(
          "min-w-0 flex-1 overflow-y-auto bg-background overscroll-contain",
          hasUnifiedTitleBar && "pt-16 [&>div]:pt-4",
        )}
      >
        <Outlet />
      </main>
    </div>
  );
}

export default function App() {
  /**
   * 启动预热**不在这里**：已上移到 `main.tsx` 的挂载之前。
   *
   * 原先放在本组件的 `useEffect` 里有两个问题：
   * ① `useEffect` 是**子组件先于父组件**执行，所以首页自己的 effect 会先跑，
   *    它触发的两条重活把 IPC 队列（以及当时的主线程）占住，缓存回包被压到后面；
   * ② 挂载后才读缓存，首屏必然先闪一帧骨架屏。
   * 现在由入口文件「先读缓存 → 挂载 → 后台重算」，两个问题都消掉了。
   */
  return (
    <TooltipProvider delayDuration={250}>
      <BrowserRouter>
        <Routes>
          <Route element={<Layout />}>
            <Route path="/" element={<HomePage />} />
            {/* 首页占用了 `/`，Trae 账号管理迁到显式路径；旧书签靠 /home 别名兜底 */}
            <Route path="/home" element={<HomePage />} />
            <Route path="/accounts" element={<AccountsHubPage />} />
            <Route path="/migrate" element={<MaintenanceHubPage />} />
            <Route path="/trae-switch" element={<TraeSwitchPage />} />
            <Route path="/trae-records" element={<TraeRecordsPage />} />
            <Route path="/workbuddy-switch" element={<WorkbuddySwitchPage />} />
            <Route path="/workbuddy-records" element={<WorkbuddyRecordsPage />} />
            <Route path="/workbuddy-import" element={<WorkbuddyImportPage />} />
            <Route path="/workbuddy-export" element={<WorkbuddyExportPage />} />
            <Route path="/workbuddy-cleanup" element={<WorkbuddyCleanupPage />} />
            <Route path="/trae-cleanup" element={<TraeCleanupPage />} />
            <Route path="/qoder-cleanup" element={<QoderCleanupPage />} />
            <Route path="/qoder-accounts" element={<QoderAccountsPage />} />
            <Route path="/sessions" element={<SessionsHubPage />} />
            <Route path="/qoder-sessions" element={<QoderSessionsPage />} />
            <Route path="/gateway" element={<GatewayPage />} />
            <Route path="/gateway-guide" element={<GatewayGuidePage />} />
            <Route path="/settings" element={<SettingsPage />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
        <Toaster />
      </BrowserRouter>
    </TooltipProvider>
  );
}
