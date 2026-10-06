import { cn } from "@/lib/utils";

interface MarkProps {
  size?: number;
  className?: string;
}

/**
 * 应用标记（TwinSwitch · 双栖）：**两个相扣的圆环**。
 *
 * 语义：两个 AI 客户端（Trae / WorkBuddy）被同一套工具串在一起，
 * 同时「环」也承接了原来那个双向箭头「循环 / 切换」的意思。
 *
 * ## 为什么是内联 SVG，而不是原来的 PNG
 *
 * 之前这里是 `<img src="icon-transparent.png">`（48 KB 位图）。改成内联 SVG 有三个实打实的好处：
 *
 * 1. **能继承颜色**：`stroke="currentColor"` ⇒ 浅色主题下是深墨色、深色主题下自动变浅，
 *    不用再为两套主题各准备一张图（原来那一张 PNG 只能在一种主题下好看）；
 * 2. **任意尺寸都锐利**：侧栏 36px、将来若放到 20px 或 96px 都不会糊；
 * 3. **少一次资源请求**，也不再需要 `public/icon-transparent.png`。
 *
 * ⚠️ 桌面安装图标（任务栏 / 资源管理器）与 favicon 仍然是位图 —— 那是
 * `src-tauri/icons/*`（由 `npx tauri icon` 从同一份母版生成）与 `public/icon.png`，
 * 它们有底色，与本组件「无底色、跟文字色」的定位不同，别混用。
 */
export function AppIconMark({ size = 32, className }: MarkProps) {
  return (
    <span
      aria-hidden
      className={cn("inline-flex shrink-0", className)}
      style={{ width: size, height: size }}
    >
      <svg
        viewBox="0 0 512 512"
        width={size}
        height={size}
        fill="none"
        className="size-full"
        // 描边宽度按画布比例写死（512 画布下 42），缩放时自动等比 —— 不要写 px。
        strokeWidth={42}
      >
        <g stroke="currentColor" strokeLinecap="round">
          <circle cx="186" cy="256" r="104" />
          <circle cx="326" cy="256" r="104" />
        </g>
      </svg>
    </span>
  );
}
