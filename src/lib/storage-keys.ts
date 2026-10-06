/**
 * `localStorage` 键的统一前缀，以及**改名后的一次性读回退**。
 *
 * ## 为什么需要这个模块
 *
 * v0.0.24 把产品名从 `trae-switch-cn` 改成了 `twin-switch`（TwinSwitch · 双栖），
 * 本机数据目录也跟着改了。`localStorage` 的键前缀同样带旧名字：
 *
 * - `trae-switch-cn.theme`（主题偏好）
 * - `trae-switch-cn.dark`（更早的一版主题开关）
 * - `trae-switch-cn:dialog-size:<名字>`（各弹窗记住的尺寸）
 *
 * 如果只是把常量改成新前缀，用户升级后会**看起来像「设置被重置」**：主题跳回跟随系统、
 * 弹窗尺寸全部复原。所以统一在这里做三件事：
 *
 * 1. **读**：先读新键；没有就读旧键，并把值顺手写到新键下（一次性搬迁，幂等）；
 * 2. **写**：只写新键 —— 任何写路径都不许再落到旧前缀；
 * 3. 旧键**不删**：万一要退回旧版本，那边还能正常读到用户原来的选择。
 *
 * 注意 `readStored` / `writeStored` 内部已各自捕获异常（隐私模式、配额满、存储被禁用），
 * 调用方不需要再包 try/catch。
 */

/** 新前缀。 */
export const STORAGE_PREFIX = "twin-switch";

/** 改名前的旧前缀。**只用于读取回退**，不要用它写入。 */
export const LEGACY_STORAGE_PREFIX = "trae-switch-cn";

/**
 * 新键名 → 对应的旧键名。
 *
 * 前缀可能是 `twin-switch.theme`（点号）也可能是 `twin-switch:dialog-size:x`（冒号），
 * 两种都按「前缀等长替换」处理，所以直接切掉前缀长度再拼旧前缀即可。
 */
function legacyKeyFor(key: string): string | null {
  if (key.startsWith(`${STORAGE_PREFIX}.`) || key.startsWith(`${STORAGE_PREFIX}:`)) {
    return LEGACY_STORAGE_PREFIX + key.slice(STORAGE_PREFIX.length);
  }
  return null;
}

/** 读键：新键优先，读不到则回退读旧键并搬到新键下。取不到值一律返回 `null`。 */
export function readStored(key: string): string | null {
  if (typeof window === "undefined") return null;
  try {
    const fresh = window.localStorage.getItem(key);
    if (fresh !== null) return fresh;

    const legacyKey = legacyKeyFor(key);
    if (!legacyKey) return null;
    const legacy = window.localStorage.getItem(legacyKey);
    if (legacy === null) return null;

    // 一次性搬迁：读到旧值就顺手写到新键下。旧键保留（见文件头说明）。
    try {
      window.localStorage.setItem(key, legacy);
    } catch {
      // 写不进去也不影响本次取值。
    }
    return legacy;
  } catch {
    return null;
  }
}

/** 写键：只写新键。失败时静默（存储不可用不该打断功能，顶多是记不住偏好）。 */
export function writeStored(key: string, value: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(key, value);
  } catch {
    /* 隐私模式 / 配额满：记不住偏好，不影响使用 */
  }
}

/** 删键：新键与旧键一起删，避免下次读时旧值又被搬回来。 */
export function removeStored(key: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(key);
    const legacyKey = legacyKeyFor(key);
    if (legacyKey) window.localStorage.removeItem(legacyKey);
  } catch {
    /* 同上 */
  }
}
