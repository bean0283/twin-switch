import { useEffect, useState, type ReactNode } from "react";
import { BrowserRouter, Navigate, NavLink, Outlet, Route, Routes } from "react-router-dom";
import {
  Eraser,
  HardDriveDownload,
  HardDriveUpload,
  Home,
  Repeat2,
  ScrollText,
  Trash2,
  UserRound,
} from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";

import { cn } from "@/lib/utils";
import HomePage from "@/pages/HomePage";
import TraeSwitchPage from "@/pages/TraeSwitchPage";
import TraeRecordsPage from "@/pages/TraeRecordsPage";
import WorkbuddyImportPage from "@/pages/WorkbuddyImportPage";
import WorkbuddyExportPage from "@/pages/WorkbuddyExportPage";
import WorkbuddyCleanupPage from "@/pages/WorkbuddyCleanupPage";
import TraeCleanupPage from "@/pages/TraeCleanupPage";
import WorkbuddySwitchPage from "@/pages/WorkbuddySwitchPage";
import WorkbuddyRecordsPage from "@/pages/WorkbuddyRecordsPage";
import { AppIconMark } from "@/components/product-marks";
import { UpdateEntry } from "@/components/update-entry";
import { Toaster } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";

/** 侧栏导航项：统一激活态样式，避免每个入口重复一份 className 计算。 */
function SidebarLink({
  to,
  end,
  icon,
  label,
}: {
  to: string;
  end?: boolean;
  icon: ReactNode;
  label: string;
}) {
  return (
    <NavLink
      to={to}
      end={end}
      className={({ isActive }) =>
        cn(
          "flex items-center gap-2.5 rounded-lg px-3 py-2.5 text-sm outline-none transition-colors focus-visible:ring-2 focus-visible:ring-sidebar-ring/50",
          isActive
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
        <nav className="flex min-h-0 flex-1 flex-col gap-0.5" aria-label="主导航">
          <SidebarLink to="/" end icon={<Home className="size-4" />} label="首页" />

          {/* Trae 自身的账号 / 会话能力 */}
          <div className="mt-3 flex items-center gap-2 px-3 pb-1" role="presentation">
            <span className="text-[11px] font-medium tracking-wide text-sidebar-foreground/40">
              Trae
            </span>
            <span className="h-px flex-1 bg-sidebar-border" />
          </div>
          <SidebarLink
            to="/trae-switch"
            icon={<Repeat2 className="size-4" />}
            label="Trae 账号管理"
          />
          <SidebarLink
            to="/trae-records"
            icon={<ScrollText className="size-4" />}
            label="Trae 会话记录"
          />

          {/* 独立区域：WorkBuddy 自身的账号能力（与 Trae 区分开） */}
          <div className="mt-3 flex items-center gap-2 px-3 pb-1" role="presentation">
            <span className="text-[11px] font-medium tracking-wide text-sidebar-foreground/40">
              WorkBuddy
            </span>
            <span className="h-px flex-1 bg-sidebar-border" />
          </div>
          <SidebarLink
            to="/workbuddy-switch"
            icon={<UserRound className="size-4" />}
            label="WorkBuddy 账号管理"
          />
          <SidebarLink
            to="/workbuddy-records"
            icon={<ScrollText className="size-4" />}
            label="WorkBuddy 会话记录"
          />

          {/* 独立区域：跨工具的数据迁移，与 Trae 自身的账号 / 会话功能分开 */}
          <div className="mt-3 flex items-center gap-2 px-3 pb-1" role="presentation">
            <span className="text-[11px] font-medium tracking-wide text-sidebar-foreground/40">
              数据迁移
            </span>
            <span className="h-px flex-1 bg-sidebar-border" />
          </div>
          <SidebarLink
            to="/workbuddy-import"
            icon={<HardDriveDownload className="size-4" />}
            label="WorkBuddy → Trae"
          />
          <SidebarLink
            to="/workbuddy-export"
            icon={<HardDriveUpload className="size-4" />}
            label="Trae → WorkBuddy"
          />

          {/* 独立区域：本机数据清理，与「迁移」不是一类事 */}
          <div className="mt-3 flex items-center gap-2 px-3 pb-1" role="presentation">
            <span className="text-[11px] font-medium tracking-wide text-sidebar-foreground/40">
              本机维护
            </span>
            <span className="h-px flex-1 bg-sidebar-border" />
          </div>
          <SidebarLink
            to="/workbuddy-cleanup"
            icon={<Trash2 className="size-4" />}
            label="WorkBuddy 清理"
          />
          <SidebarLink
            to="/trae-cleanup"
            icon={<Eraser className="size-4" />}
            label="Trae 清理"
          />
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
            <Route path="/trae-switch" element={<TraeSwitchPage />} />
            <Route path="/trae-records" element={<TraeRecordsPage />} />
            <Route path="/workbuddy-switch" element={<WorkbuddySwitchPage />} />
            <Route path="/workbuddy-records" element={<WorkbuddyRecordsPage />} />
            <Route path="/workbuddy-import" element={<WorkbuddyImportPage />} />
            <Route path="/workbuddy-export" element={<WorkbuddyExportPage />} />
            <Route path="/workbuddy-cleanup" element={<WorkbuddyCleanupPage />} />
            <Route path="/trae-cleanup" element={<TraeCleanupPage />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Route>
        </Routes>
        <Toaster />
      </BrowserRouter>
    </TooltipProvider>
  );
}
