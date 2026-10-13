/**
 * 整库备份保留策略的**纯函数**部分（T62）。
 *
 * 为什么单独立一个文件：这些换算要单测，而页面组件里有 React / Radix，单测成本高。
 * 判定本身在后端（`wb_switch_core::modules::settings::excess_batches`），
 * 前端只负责「把界面上填的数换算成后端要的单位」—— 换算错了会静默地把 4 GB 存成 4 MB。
 */

/** 一个目录最多留多少 MB 的上限（与后端 `sanitized` 的夹取上限一致：1 TB）。 */
export const MAX_TOTAL_MB_CAP = 1_048_576;

/** 保留天数的上限（与后端一致：10 年）。 */
export const KEEP_DAYS_CAP = 3650;

/** 保留份数的上限（与后端一致）。 */
export const KEEP_COUNT_CAP = 500;

/**
 * 解析输入框里的整数：空串 / 非法值按 `min` 算，超出区间夹回去。
 *
 * ⚠️ 后端也会再夹一次（`sanitized`）—— 这里先夹是为了让**输入框里显示的就是真正生效的值**，
 * 不要等保存后才在摘要里看到「被改成了 3650」。
 */
export function clampInt(raw: string, min: number, max: number): number {
  const n = Number.parseInt(raw.trim(), 10);
  if (!Number.isFinite(n)) return min;
  return Math.min(max, Math.max(min, n));
}

/**
 * MB → 输入框里显示的 GB（保留一位小数；整数就不带小数点）。
 *
 * ⚠️ 反函数必须自洽：`gbInputToMb(mbToGbInput(x)) === x` 对 1024 的整数倍成立，
 * 对 512 MB 这类会变成 0.5 GB 再回到 512（也在单测里钉住）。
 */
export function mbToGbInput(mb: number): number {
  if (!Number.isFinite(mb) || mb <= 0) return 0;
  return Math.round((mb / 1024) * 10) / 10;
}

/**
 * 输入框里的 GB → MB。
 *
 * 空 / 非正数一律 **0**（= 不按容量判，不是「留 0 MB 全清掉」—— 用户清空输入框
 * 的本意是「不限制」）。上界与后端一致。
 */
export function gbInputToMb(raw: string): number {
  const gb = Number.parseFloat(raw.trim());
  if (!Number.isFinite(gb) || gb <= 0) return 0;
  return Math.min(MAX_TOTAL_MB_CAP, Math.round(gb * 1024));
}

/** 人类可读的容量（界面上的摘要一律用后端的，这个只给表单旁的说明用）。 */
export function humanMb(mb: number): string {
  if (mb >= 1024) return `${Math.round((mb / 1024) * 10) / 10} GB`;
  return `${mb} MB`;
}
