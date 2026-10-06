// 头像底色：按名字哈希取固定色调，同一账号在卡片、弹窗、列表里颜色始终一致。
//
// 名字 → 下标用「乘 31 累加」的经典字符串哈希（与参考实现一致），
// 好处是纯函数、不需要额外状态，且同一名字在任何页面/任何进程里都得到同一个颜色。
//
// 注意：这里的色板是**浅底深字**，只在浅色主题下有对比度。
// 深色主题下如果要复用，需要另配一套（当前工具全站是浅色，暂不需要）。

const AVATAR_TONES = [
  "bg-emerald-100 text-emerald-800",
  "bg-violet-100 text-violet-800",
  "bg-sky-100 text-sky-800",
  "bg-amber-100 text-amber-800",
  "bg-rose-100 text-rose-800",
  "bg-teal-100 text-teal-800",
] as const;

/** 按名字哈希取固定色调。 */
export function avatarTone(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i += 1) hash = (hash * 31 + name.charCodeAt(i)) >>> 0;
  return AVATAR_TONES[hash % AVATAR_TONES.length];
}
