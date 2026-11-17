/**
 * 客户端生命周期状态（账号页电源按钮的「已启动 / 启动中 / 未启动」）。
 *
 * 三个账号管理页（WorkBuddy / Trae / Qoder）共用：状态永远以**真实进程探测**为准，
 * 界面只是它的投影 ——
 *
 * - `stopped`：客户端没在跑，电源按钮可点（走切号流程，末尾会拉起客户端）。
 * - `starting`：我们刚发起切号/启动（`markStarting()` 乐观置入）。Electron 冷启动
 *   或单实例锁可能让进程晚几秒才出现，所以探测到「不在跑」时不立刻翻回
 *   `stopped`，给 [`STARTING_GRACE_MS`] 宽限；超时还没出现 = 启动失败，自动回落。
 *   宽限期内按钮禁用，防止在客户端半启动状态再点一次（避免重复启动）。
 * - `running`：探测到进程存在 → 按钮显示「已启动」徽标。之后进程消失
 *   （用户正常退出 / 崩溃被杀，不做区分）下一轮轮询立即回到 `stopped`。
 *
 * 切号 / 启动**失败**时页面调 `recheck()`：立即按真实进程状态复核一次，
 * 该是什么就是什么（比如失败发生在关进程之前，客户端其实还在跑 → 仍是「已启动」）。
 */
import { useCallback, useEffect, useRef, useState } from "react";

export type ClientLifecycle = "stopped" | "starting" | "running";

/** `starting` 的宽限期：超过还没探测到进程就认定启动失败，回落「未启动」。 */
const STARTING_GRACE_MS = 15_000;
/** 轮询间隔。探测是一次内存过滤的进程快照（不开子进程），3 秒一轮开销可忽略。 */
const POLL_MS = 3_000;

export function useClientLifecycle(probe: () => Promise<{ running: boolean }>) {
  const [state, setState] = useState<ClientLifecycle>("stopped");
  // probe 会随 clientKey / 区域变化：ref 持有最新版，轮询循环不必重启。
  const probeRef = useRef(probe);
  probeRef.current = probe;
  const stateRef = useRef(state);
  stateRef.current = state;
  /** 进入 `starting` 的时刻（0 = 不在 starting）。宽限期从这里起算。 */
  const startingAtRef = useRef(0);

  useEffect(() => {
    let alive = true;
    async function tick() {
      try {
        const r = await probeRef.current();
        if (!alive) return;
        if (r.running) {
          setState("running");
        } else if (
          stateRef.current === "starting" &&
          startingAtRef.current > 0 &&
          Date.now() - startingAtRef.current < STARTING_GRACE_MS
        ) {
          // 启动命令已发出但进程还没出现：宽限期内保持「启动中」，不抖动。
        } else {
          startingAtRef.current = 0;
          setState("stopped");
        }
      } catch {
        // 单次探测失败（瞬时 IPC 错误）：维持现状，下一轮再试 ——
        // 不能因为一次探测失败就把「运行中」翻成「未启动」。
      }
    }
    void tick();
    const timer = window.setInterval(() => void tick(), POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, []);

  /** 切号 / 启动刚成功发出：乐观置「启动中」，轮询确认进程出现后升为「运行中」。 */
  const markStarting = useCallback(() => {
    startingAtRef.current = Date.now();
    setState("starting");
  }, []);

  /** 立即按真实进程状态复核一次（切号 / 启动失败后的状态回退出口）。 */
  const recheck = useCallback(async () => {
    try {
      const r = await probeRef.current();
      startingAtRef.current = 0;
      setState(r.running ? "running" : "stopped");
    } catch {
      // 探测失败就保持现状，交给下一轮轮询。
    }
  }, []);

  return { state, markStarting, recheck };
}
