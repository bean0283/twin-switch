// 本机总览的**进程内单例 store**：解决「启动慢、加载期间无法操作」。
//
// 原来的做法是首页组件挂载时 await `app_overview_snapshot()`，
// 而那个调用要枚举进程（WMI）+ 统计 SQLite，冷启动可能几百毫秒到数秒，
// 期间整页是骨架屏，点不了任何东西。
//
// 现在的节奏：
//   1. **应用启动**（App 挂载，早于用户点进首页）→ `primeOverview()` 读磁盘缓存，毫秒级；
//   2. 紧接着 `refreshOverview()` 在**后台**重算，算完把结果推给所有订阅者；
//   3. 后端在算完时顺手落盘 → 下次启动第 1 步就是热的。
//
// 因为 store 在模块作用域，用户从别的页面切回首页时数据已经在了，**永远不会看到骨架屏**。
// `useSyncExternalStore` 保证任何组件的订阅都能收到更新。

import { useSyncExternalStore } from "react";

import * as api from "./api";
import type { Overview, OverviewReclaimCache } from "./trae-types";

export interface OverviewState {
  /** 总览快照；null = 从来没拿到过（首启且无缓存）。 */
  overview: Overview | null;
  /** 可回收空间（可能比 overview 晚到：它由首页另起一条腿扫描）。 */
  reclaim: OverviewReclaimCache | null;
  /** 正在后台重算。 */
  refreshing: boolean;
  /** 当前展示的 `overview` 是磁盘缓存（还没被本次刷新的结果覆盖）。 */
  fromCache: boolean;
  /** 缓存/快照的生成时间（ms）。 */
  generatedAt: number | null;
  /** 上一次刷新失败的提示（有缓存时不该整页报错，只做角标）。 */
  error: string | null;
  /** 本次运行是否已经跑完过一次实时刷新（无论成败）。 */
  settled: boolean;
}

let state: OverviewState = {
  overview: null,
  reclaim: null,
  refreshing: false,
  fromCache: false,
  generatedAt: null,
  error: null,
  settled: false,
};

const listeners = new Set<() => void>();

function emit(next: OverviewState) {
  state = next;
  for (const fn of listeners) fn();
}

/** 订阅（配合 `useSyncExternalStore`）。 */
export function subscribeOverview(fn: () => void): () => void {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** 取当前状态（引用稳定，只在真正变化时替换对象）。 */
export function getOverviewState(): OverviewState {
  return state;
}

/** React 订阅入口（任何页面都可以用，数据是共享的）。 */
export function useOverviewState(): OverviewState {
  return useSyncExternalStore(subscribeOverview, getOverviewState, getOverviewState);
}

let primed: Promise<void> | null = null;
let inflight: Promise<void> | null = null;

/**
 * 读磁盘缓存（**只读文件，毫秒级**）。只跑一次；读不到就静默跳过，等实时刷新。
 *
 * 注意：缓存绝不覆盖已经拿到的实时结果（`??` 而不是赋值），
 * 否则用户点了「刷新」之后再切回首页会被旧数字盖回去。
 */
export function primeOverview(): Promise<void> {
  if (primed) return primed;
  primed = (async () => {
    try {
      const c = await api.appOverviewCached();
      if (c.empty || !c.snapshot) return;
      emit({
        ...state,
        overview: state.overview ?? c.snapshot,
        reclaim: state.reclaim ?? c.reclaim,
        fromCache: state.settled ? state.fromCache : true,
        generatedAt: state.generatedAt ?? c.generatedAt,
      });
    } catch {
      // 缓存是纯加速手段，读不到不影响功能
    }
  })();
  return primed;
}

/**
 * 后台重算总览。同一次运行里并发调用会复用同一个 Promise（不会重复打 WMI）。
 */
export function refreshOverview(): Promise<void> {
  if (inflight) return inflight;
  emit({ ...state, refreshing: true, error: null });
  inflight = (async () => {
    try {
      const snap = await api.appOverviewSnapshot();
      emit({
        ...state,
        overview: snap,
        fromCache: false,
        generatedAt: snap.generatedAt,
        refreshing: false,
        settled: true,
        error: null,
      });
    } catch (e) {
      emit({
        ...state,
        refreshing: false,
        settled: true,
        error: e instanceof Error ? e.message : String(e),
      });
    } finally {
      inflight = null;
    }
  })();
  return inflight;
}

/** 记住首页扫出来的可回收空间：先更新内存（立刻可见），再写回后端缓存。 */
export function rememberReclaim(traeBytes: number, wbBytes: number): void {
  const reclaim: OverviewReclaimCache = {
    traeBytes,
    wbBytes,
    totalBytes: traeBytes + wbBytes,
    at: Date.now(),
  };
  emit({ ...state, reclaim });
  void api.appOverviewSaveReclaim(traeBytes, wbBytes).catch(() => {
    // 写缓存失败只影响下次启动速度，不影响本次展示
  });
}

// 启动预热由 `main.tsx` 显式编排（先 `primeOverview()` → 挂载 → 再 `refreshOverview()`），
// 不在这里包一个「先读后刷」的合成函数：
//   - 读缓存必须早于挂载，才能让首屏第一帧就有数据；
//   - 重算必须晚于挂载，否则会跟首屏渲染抢时间；
//   - 合成函数容易被误调两次，导致重复跑一遍 WMI。
// 两个函数各自幂等（`in-flight` 复用），单独调用是安全的。
