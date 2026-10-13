import type { TraeInstalledClient } from "./trae-types";
import { isIntlClient, selectableClients } from "./trae-client-list";

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
 * ③ 兜底：同一个「线」上第一个能用的客户端（国内版 / 国际版各算一条线）；
 *    同一条线上一个能用的都没有 ⇒ `null`（界面按「没有目标」处理）。
 *
 * ⚠️ ③ 必须**同线兜底**：国际版会话写进国内版库 = 跨了产品线，客户端根本读不到，
 *    而且两个库的项目/账号表是两套。国际版被临时开关藏起来时（见 `INTL_GATE`），
 *    `solo-intl` 的默认目标因此是 `null` —— 这是**故意**的，不是漏了。
 *
 * 判据**借 `@/lib/trae-client-list` 那一条**（`isSelectableClient`）：除了这里原来的
 * 「已安装且本机跑得起来」，还一并带上「已并入且本机跑不起来的不选」与
 * 「国际版整体不开放（临时开关）」—— 三件事都只在一处判定，各页面别自己再 filter 一遍。
 * `runnable` 缺省（老版本后端没这个字段）时按「能用」处理，避免把整套客户端误判成不可用。
 */
export function defaultTargetClient(
  srcKey: string | null,
  clients: TraeInstalledClient[],
): string | null {
  const usable = usableTargets(clients);
  if (!srcKey) return usable[0]?.key ?? null;
  if (usable.some((c) => c.key === srcKey)) return srcKey;
  const host = clients.find((c) => c.key === srcKey)?.merged_into;
  if (host && usable.some((c) => c.key === host)) return host;
  // 兜底只在本条线上找；跨线宁可返回 null，也不把会话写进另一个产品线的库。
  return usable.find((c) => isIntlClient(c.key) === isIntlClient(srcKey))?.key ?? null;
}

/**
 * 能作为导入目标的客户端。
 *
 * 判据 = 「已安装 + 本机跑得起来」**再叠上**共用的隐藏判据（`isSelectableClient`：
 * 已并入且跑不起来的不选、国际版临时不开放）。
 *
 * ⚠️ 两个条件都要 `runnable` 这一项，但含义不同：界面选择器只摘「已并入且跑不起来」的
 *    （老机器上 SOLO 还独立装着就该留着）；**导入目标更严** —— 本机跑不起来的客户端
 *    一律不能当目标，写进去的会话没人会读。所以这里比界面多一道 `runnable !== false`。
 */
function usableTargets(clients: TraeInstalledClient[]): TraeInstalledClient[] {
  return selectableClients(clients.filter((c) => c.installed && c.runnable !== false));
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
  return usableTargets(clients);
}
