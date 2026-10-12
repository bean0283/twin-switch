import type { TraeInstalledClient } from "./trae-types";

/**
 * 导入弹窗打开时默认选中的**目标客户端**。
 *
 * ⚠️ 不能直接拿「正在浏览的客户端」当默认目标。
 *
 * 2026-10 官方把 TRAE SOLO 并进 Trae CN 之后，SOLO 的数据目录与登录态**都还在**
 * （客户端列表里照常显示、也照常有账号候选、`database_path` 也检查得到库），
 * 但它**已经没有自己的程序了** —— 把默认目标设成 SOLO 自己，用户点下去就是把会话
 * 写进一个永远不会被读到的库：**不报错、也没效果**，比直接失败更难发现。
 *
 * 取值顺序：
 * ① 源客户端自己还能用 ⇒ 仍选它（同客户端跨账号复制是最常用用法，保持原行为）；
 * ② 源已并入别人 ⇒ 落到承载方（`merged_into`）；
 * ③ 兜底：第一个能用的客户端；一个能用的都没有 ⇒ `null`（界面按「没有目标」处理）。
 *
 * 判据只用 `installed && runnable !== false`：`runnable` 缺省（老版本后端没这个字段）
 * 时按「能用」处理，避免把整套客户端误判成不可用。
 */
export function defaultTargetClient(
  srcKey: string | null,
  clients: TraeInstalledClient[],
): string | null {
  const usable = clients.filter((c) => c.installed && c.runnable !== false);
  if (!srcKey) return usable[0]?.key ?? null;
  if (usable.some((c) => c.key === srcKey)) return srcKey;
  const host = clients.find((c) => c.key === srcKey)?.merged_into;
  if (host && usable.some((c) => c.key === host)) return host;
  return usable[0]?.key ?? null;
}

/**
 * 目标客户端下拉里**能列出来**的客户端。
 *
 * 与 [`defaultTargetClient`] 用同一条判据 —— 两处必须同源，否则会出现
 * 「默认选中的那个不在列表里」这种自相矛盾的界面。
 */
export function targetClientOptions(
  clients: TraeInstalledClient[],
): TraeInstalledClient[] {
  return clients.filter((c) => c.installed && c.runnable !== false);
}
