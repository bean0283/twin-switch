/**
 * 统一剪贴板出口。
 *
 * 为什么不用 `navigator.clipboard.writeText` 直接调：
 * Tauri 的 WebView2 在窗口未聚焦 / 页面刚切路由 / 某些系统策略下，
 * 异步 clipboard API 会 reject —— 而 `void` 掉 Promise 就等于「静默失败」，
 * 用户点了「复制」，粘贴出来还是旧内容（或什么都没有），界面上却弹了成功。
 *
 * 所以：先走异步 API；失败降级到 `execCommand("copy")`（同步、老通道，WebView 里通常可用）；
 * 两条都失败 return false，让调用方如实报错，绝不假装成功。
 */

export async function copyText(text: string): Promise<boolean> {
  if (!text) return false;

  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // 落到下面的同步通道
  }

  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    // 必须在视口内且不被看见，否则部分 WebView 里 select() 选不中
    ta.style.position = "fixed";
    ta.style.top = "0";
    ta.style.left = "0";
    ta.style.width = "1px";
    ta.style.height = "1px";
    ta.style.padding = "0";
    ta.style.border = "none";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    const prevFocus = document.activeElement as HTMLElement | null;
    ta.focus();
    ta.select();
    ta.setSelectionRange(0, text.length);
    const ok = document.execCommand("copy");
    document.body.removeChild(ta);
    prevFocus?.focus?.();
    return ok;
  } catch {
    return false;
  }
}
