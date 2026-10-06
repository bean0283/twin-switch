/** 更新源常量与「打开发布页」。
 *
 * 这三个常量与 Rust 侧 `wb_switch_core::modules::update` 的 `GITHUB_OWNER` /
 * `GITHUB_REPO` **必须一致**（前端只用它们拼展示用的链接，真正的检查与下载都在 Rust）。
 */
export const GITHUB_OWNER = "bean0283";
export const GITHUB_REPO = "twin-switch";
export const GITHUB_REPOSITORY_URL = `https://github.com/${GITHUB_OWNER}/${GITHUB_REPO}`;
export const GITHUB_RELEASE_URL = `${GITHUB_REPOSITORY_URL}/releases/latest`;

/** 在桌面端走 Tauri opener 打开，在浏览器（web 预览）里开新标签页。 */
export async function openReleaseUrl(url: string = GITHUB_RELEASE_URL): Promise<void> {
  const isWebPreview = typeof window !== "undefined" && !("__TAURI_INTERNALS__" in window);
  if (isWebPreview) {
    window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  const { openUrl } = await import("@tauri-apps/plugin-opener");
  await openUrl(url);
}
