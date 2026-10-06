import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/bricolage-grotesque";
import App from "./App";
import "./index.css";
import { AppErrorBoundary } from "./components/app-error-boundary";
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

const cacheReady = Promise.race([
  primeOverview(),
  new Promise<void>((resolve) => window.setTimeout(resolve, PRIME_TIMEOUT_MS)),
]);

void cacheReady.then(() => {
  mount();
  void refreshOverview();
});
