import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  isGrowthRevealed,
  matchGrowthShortcut,
  setGrowthRevealed,
  subscribeGrowthReveal,
  tapVersion,
  toggleGrowthReveal,
} from "./growth-reveal";

/**
 * 这块是「隐藏入口」的判定逻辑，出错只有两种症状：
 * ① 误触（随便点点就冒出来）② 点不出来（暗号失灵）。
 * 两种都很难在界面上发现，所以把边界钉死。
 */
describe("growth-reveal", () => {
  beforeEach(() => {
    setGrowthRevealed(false);
    vi.useRealTimers();
  });

  it("默认是隐藏的（重启后的初始状态）", () => {
    expect(isGrowthRevealed()).toBe(false);
  });

  it("连点 5 次才叫出来，前 4 次不动", () => {
    vi.useFakeTimers();
    for (let i = 0; i < 4; i += 1) {
      expect(tapVersion()).toBe(false);
      vi.advanceTimersByTime(100);
    }
    expect(isGrowthRevealed()).toBe(false);
    expect(tapVersion()).toBe(true);
    expect(isGrowthRevealed()).toBe(true);
  });

  it("再连点 5 次能收回（叫得出来也要收得回去）", () => {
    vi.useFakeTimers();
    for (let i = 0; i < 5; i += 1) tapVersion();
    expect(isGrowthRevealed()).toBe(true);
    for (let i = 0; i < 5; i += 1) tapVersion();
    expect(isGrowthRevealed()).toBe(false);
  });

  it("超出 3 秒窗口的点击不累计（散着点不会误触）", () => {
    vi.useFakeTimers();
    // 每次间隔 2 秒：5 次一共 8 秒，永远凑不满窗口内 5 次
    for (let i = 0; i < 6; i += 1) {
      tapVersion();
      vi.advanceTimersByTime(2000);
    }
    expect(isGrowthRevealed()).toBe(false);
  });

  it("订阅者能收到变化（界面据此重渲染）", () => {
    const seen: boolean[] = [];
    const off = subscribeGrowthReveal(() => seen.push(isGrowthRevealed()));
    toggleGrowthReveal();
    toggleGrowthReveal();
    off();
    toggleGrowthReveal();
    expect(seen).toEqual([true, false]);
  });

  it("键盘暗号只认 Ctrl+Shift+Alt+G", () => {
    const mk = (o: Partial<KeyboardEvent>) =>
      ({ ctrlKey: true, shiftKey: true, altKey: true, key: "g", ...o }) as KeyboardEvent;
    expect(matchGrowthShortcut(mk({}))).toBe(true);
    expect(matchGrowthShortcut(mk({ key: "G" }))).toBe(true);
    // 少一个修饰键都不算
    expect(matchGrowthShortcut(mk({ ctrlKey: false }))).toBe(false);
    expect(matchGrowthShortcut(mk({ shiftKey: false }))).toBe(false);
    expect(matchGrowthShortcut(mk({ altKey: false }))).toBe(false);
    expect(matchGrowthShortcut(mk({ key: "h" }))).toBe(false);
  });
});
