# TwinSwitch logo 规范

> 版本：v0.1.0（2026-10-06）。设计方向由用户指定：**极简线条 / 单色几何**。

## 1. 概念

**两个等大圆环相交**——两个圆＝**两个客户端**（Trae / WorkBuddy），相交＝**同一个管理台**，
圆环（而非实心圆）＝**切换 / 流转**而非「合并」。整体不依赖颜色表意，
所以深浅主题、单色印刷、托盘小图标都能用同一套形状。

## 2. 几何（可复现的精确参数）

### 2.1 应用内标记 —— `twin-switch-mark.svg`（512 画布）

| 项 | 值 |
| --- | --- |
| 画布 | `viewBox="0 0 512 512"` |
| 圆 1 | `cx=186 cy=256 r=104` |
| 圆 2 | `cx=326 cy=256 r=104` |
| 圆心距 | **140**（≈ 半径的 1.35 倍 ⇒ 交叠明显但不糊成一团） |
| 描边宽 | **42**（≈ 半径的 0.40 倍，视觉上「粗而匀」） |
| 线帽 | `stroke-linecap="round"` |
| 填充 | **`fill="none"`**（纯描边） |
| 颜色 | **`stroke="currentColor"`** —— 不写死颜色，随容器文字色走 |

### 2.2 应用图标 —— `twin-switch-app-1024.svg`（1024 画布）

| 项 | 值 |
| --- | --- |
| 圆角方块 | `x=52 y=52 w=920 h=920 rx=200`，填充 **`#18181B`**（石墨黑） |
| 安全边距 | 四边各留 **52 px** 透明（≈ 5%，兼顾 macOS 图标的呼吸感惯例） |
| 圆 1 / 圆 2 | `cx=405/619 cy=512 r=158`，描边宽 **56** |
| 线条色 | **`#FAFAFA`**（反白米白） |

> 圆角半径 `200 / 920 ≈ 21.7%`，与主流桌面图标（Windows 11 / macOS）的圆角比例同量级。

## 3. 颜色

| 用途 | 值 | 说明 |
| --- | --- | --- |
| 图标底色 | `#18181B` | 石墨黑（Tailwind `zinc-900` 档） |
| 图标线条 | `#FAFAFA` | 米白（Tailwind `zinc-50` 档） |
| 应用内标记 | `currentColor` | 亮色主题下呈深色、暗色主题下呈浅色，**无需维护两份资产** |

**刻意不使用品牌色**：本项目是本地工具，界面本身已是中性灰阶（`sidebar` / `foreground` 语义变量），
一个彩色 logo 会在侧栏里显得突兀；单色也让它在托盘 16 px 下依然干净。

## 4. 用法与落地位置

| 场景 | 资产 | 落点 |
| --- | --- | --- |
| 应用内品牌区（侧栏顶部，36 px） | **内联 SVG**（`AppIconMark`） | `src/components/product-marks.tsx` |
| 窗口 / 任务栏 / 安装包 / 托盘 | 位图全套 | `src-tauri/icons/*`（由 `npx tauri icon` 生成） |
| 网页 favicon / README 展示 | 512 PNG | `public/icon.png` |

**为什么应用内用内联 SVG 而不是位图**：
① 随主题自动变色，不必维护明暗两份；② 任意缩放锐利（高 DPI 屏不糊）；
③ 不占网络 / 磁盘请求；④ 修改只需改一处几何参数。

**托盘图标不加额外资产**：Tauri 托盘走 `app.default_window_icon()`（即 `bundle.icon`），
所以重新生成图标后托盘**自动跟着换**，无需改代码。

## 5. 生成流程（改版时照做）

```bash
# 1) 改 SVG 母版（本目录下两个文件）
# 2) 栅格化：必须在装了 @resvg/resvg-js 的那个 node 工作区目录里跑（ESM 不认 NODE_PATH）
node rasterize.mjs twin-switch-app-1024.svg twin-switch-app-1024.png 1024
#    装包若遇 502，用镜像：npm i @resvg/resvg-js --registry=https://registry.npmmirror.com
# 3) 生成全平台图标（含 .ico / .icns / Windows Square*Logo）
npx tauri icon twin-switch-app-1024.png
# 4) 换 favicon：把 512 PNG 覆盖到 public/icon.png
```

> ⚠️ **母版本目录（`docs/logo/`）是唯一权威来源**。此前工作副本在 `target/logo-work/`，
> 而 `target/` 会被 `cargo clean` 整个删掉 —— 所以母版必须归档在仓库里。

## 6. 候选稿（未采用，留档备查）

`candidates/` 下是当初的五个方向，最终选定 **A2（双环紧交叠）**：

| 文件 | 构想 | 未采用原因 |
| --- | --- | --- |
| `A-twin-rings.svg` | 双环交叠（间距较大） | 交叠不够，读起来像「两个独立圆」 |
| **`A2-twin-rings.svg`** | **双环紧交叠** | ✅ **采用** |
| `B-swap-arrows.svg` | 双箭头循环 | 箭头细节多，16 px 下糊 |
| `C-s-loop.svg` | S 形单线环 | 与字母「S」混淆，且只暗示一个客户端 |
| `D-rings-arrow.svg` | 双环 + 箭头 | 元素过多，不够「极简」 |

## 7. 禁用

- 不要给双环加渐变、投影、描边发光（单色几何的前提是**平**）。
- 不要把两环拉开到不相交（会丢掉「同一管理台」的含义）。
- 不要在 24 px 以下使用 `twin-switch-mark.svg` 的**细描边变体**；小尺寸一律走
  `twin-switch-app-1024` 那套（粗描边 + 实底），否则线条会糊掉。
- 不要把标记旋转或拉伸；它是严格轴对称的。
