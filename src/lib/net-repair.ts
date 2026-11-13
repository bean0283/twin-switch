import { create } from "zustand";

/**
 * 传输层失败特征检测。
 *
 * 后端 `http_request` 的网络错误分支返回 `code: -1` 且 message 是 reqwest 原文
 * （`error sending request for url (...)`）—— 这是「连接根本没建立」的签名，
 * 与 401 / 5xx 等业务错误不同：前者才可能靠「重建网络客户端」修复。
 */
export function isTransportError(message: string | null | undefined): boolean {
  if (!message) return false;
  return /error sending request|传输失败/i.test(message);
}

interface NetRepairStore {
  open: boolean;
  /** 触发修复的错误原文（错误窗口里展示，保留现场）。 */
  errorMessage: string | null;
  /** 修复成功后要重跑的动作（例如重新刷新刚才失败的账号积分）。 */
  onRepaired: (() => void) | null;
  /** 传输层失败时调用：打开修复窗口（是否亮按钮由窗口内诊断决定）。 */
  openRepair: (errorMessage: string, onRepaired?: () => void) => void;
  close: () => void;
}

export const useNetRepair = create<NetRepairStore>((set) => ({
  open: false,
  errorMessage: null,
  onRepaired: null,
  openRepair: (errorMessage, onRepaired) =>
    set({ open: true, errorMessage, onRepaired: onRepaired ?? null }),
  close: () => set({ open: false, errorMessage: null, onRepaired: null }),
}));
