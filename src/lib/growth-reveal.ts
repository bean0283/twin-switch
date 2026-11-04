//! 成长任务卡片的「隐藏入口」。
//!
//! ## 为什么要有这个东西
//!
//! 这块能力默认**不对外露出**：卡片平时完全不渲染（连请求都不发），
//! 只有知道暗号的人能把它叫出来。重启应用后自动回到隐藏。
//!
//! ## 两条暗号
//!
//! 1. 左下角版本号 `TwinSwitch v…` **3 秒内连点 5 次**（触屏也能用）
//! 2. `Ctrl + Shift + Alt + G`（键盘党）
//!
//! 两条都做，满足条件就**切换**显示/隐藏 —— 能打开就得能关上，
//! 否则叫出来一次就再也收不回去了。
//!
//! ## 为什么不用 localStorage
//!
//! 需求是「重启后还是关闭的」。写进 localStorage 就变成**永久**打开，
//! 下次启动还看得见。所以这里只存进程内变量：
//! 刷新页面会清掉、重启应用一定清掉，跟「本会话有效」完全一致。

const TAP_COUNT = 5;
/** 连点统计的时间窗：超过这个间隔就从第一次重算。 */
const TAP_WINDOW_MS = 3000;

/** 是否已叫出（**只存在内存里**，不落盘）。 */
let revealed = false;
/** 连点时间戳，用来判断「3 秒内 5 次」。 */
let taps: number[] = [];
const listeners = new Set<() => void>();

function emit() {
  for (const fn of listeners) fn();
}

export function isGrowthRevealed(): boolean {
  return revealed;
}

/** 供 `useSyncExternalStore` 订阅；返回取消订阅函数。 */
export function subscribeGrowthReveal(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export function setGrowthRevealed(next: boolean) {
  if (revealed === next) return;
  revealed = next;
  emit();
}

/** 切换显示/隐藏。 */
export function toggleGrowthReveal() {
  setGrowthRevealed(!revealed);
}

/**
 * 版本号被点了一下。
 *
 * @returns `true` 表示这一下**凑满了 5 次**，已经切换过状态（调用方据此弹提示）。
 */
export function tapVersion(): boolean {
  const now = Date.now();
  taps = taps.filter((t) => now - t < TAP_WINDOW_MS);
  taps.push(now);
  if (taps.length < TAP_COUNT) return false;
  taps = [];
  toggleGrowthReveal();
  return true;
}

/** 键盘暗号：`Ctrl + Shift + Alt + G`。 */
export function matchGrowthShortcut(e: KeyboardEvent): boolean {
  return e.ctrlKey && e.shiftKey && e.altKey && e.key.toLowerCase() === "g";
}
