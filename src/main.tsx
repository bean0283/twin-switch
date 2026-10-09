import React from "react";
import ReactDOM from "react-dom/client";
import { toast } from "sonner";
import "@fontsource-variable/bricolage-grotesque";
import App from "./App";
import "./index.css";
import { AppErrorBoundary } from "./components/app-error-boundary";
import * as api from "./lib/api";
import { installGlobalErrorHandlers } from "./lib/error-report";
import { primeOverview, refreshOverview } from "./lib/overview-store";
import { applyTheme, getThemePreference, watchSystemTheme } from "./lib/theme";

applyTheme(getThemePreference());
const stopWatchingSystemTheme = watchSystemTheme();
if (import.meta.hot) import.meta.hot.dispose(stopWatchingSystemTheme);

// 全局错误捕获要在挂载前装好：事件回调与异步链路的异常归它兜底（渲染期另有 ErrorBoundary）。
installGlobalErrorHandlers();

/** 缓存读取的最长等待：超时就照常挂载，不能把窗口停在启动占位上。 */
const PRIME_TIMEOUT_MS = 800;

/**
 * 启动顺序：**先把磁盘缓存捞回来，再挂载界面，最后后台重算**。
 *
 * 为什么不等组件挂载后再读缓存：那样第一帧必然是「骨架屏」，缓存回来的那一下
 * 只是把骨架替换掉 —— 用户看到的就是「先空一屏再跳出内容」。改成先读后挂载，
 * **首屏第一帧就是真实数据**（标题旁挂「缓存 · N 分钟前」徽标）。
 *
 * 重算放在挂载之后、且在前端只是发一条 IPC：真正的重活在 Rust 的后台线程池里跑，
 * 界面全程可操作。
 *
 * ⚠️ 必须**先 prime 再 refresh**（不是同时发）：`refreshOverview` 一跑就写回缓存，
 * 若与 prime 并行，可能把「刚算完的新结果」当成读到的缓存，反而丢掉实时值。
 */
function mount() {
  ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
    <React.StrictMode>
      <AppErrorBoundary>
        <App />
      </AppErrorBoundary>
    </React.StrictMode>,
  );

  // 入口 JS 已接管首屏：index.html 的静态兜底脚本据此不再替换占位（否则 10 秒后会把
  // 可用的界面或错误页换成「启动失败」提示）。
  window.__WB_MOUNTED__ = true;
}

/** 自动搬家的最长等待：这是个可选动作，**不许**把启动路径拖住。 */
const IMPORT_TIMEOUT_MS = 3000;

/**
 * 启动时的**静默搬家**：把参考工具账号库里的明文凭据并进本工具账号库。
 *
 * 为什么需要：本工具自己的账号多是「导入本机登录态」收进来的，凭据是 WorkBuddy 加密
 * 信封，本地解不出明文 ⇒ 查不了积分。参考工具那边的同一批账号是它自己扫码得到的明文。
 * 这件事以前要用户点一个按钮，但按钮存在的全部意义只是「用户不知道该点」——
 * 2026-10-09 起改成启动自动做：**幂等**（本地已是明文就落入 `kept`，不重写凭据）、
 * 只读对方文件、一个字节都不写它。
 *
 * ⚠️ 必须排在 `refreshOverview()` **之前**：总览的 `queryableCount` 与积分缓存都按
 * 账号库算，先搬完再算，才不会出现「这一轮显示查不了、下一轮才有数」。
 * ⚠️ 任何失败都咽掉：对方没装过那个工具、或文件损坏都是**常态**，不是本工具的错误，
 * 启动时弹红字只会吓人。只有**真的搬动了东西**才提示一句。
 */
async function autoImportReferenceAccounts(): Promise<void> {
  try {
    const r = await Promise.race([
      api.workbuddyImportReferenceAccounts(),
      new Promise<null>((resolve) => window.setTimeout(() => resolve(null), IMPORT_TIMEOUT_MS)),
    ]);
    if (r && r.imported > 0) toast.success(`已自动导入 ${r.imported} 个账号的明文凭据，积分可以查询了`);
  } catch {
    // 静默：可选增强，失败不影响任何功能
  }
}

const cacheReady = Promise.race([
  primeOverview(),
  new Promise<void>((resolve) => window.setTimeout(resolve, PRIME_TIMEOUT_MS)),
]);

void cacheReady.then(async () => {
  mount();
  // 先搬明文再重算：顺序反了会算出一份「账号查不了」的快照，用户看到的第一个数字就是错的。
  await autoImportReferenceAccounts();
  void refreshOverview();
});
