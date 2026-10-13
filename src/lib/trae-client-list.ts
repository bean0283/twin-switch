/**
 * 「界面上该出现哪些 Trae 客户端」的**唯一判据**。
 *
 * 2026-10 官方把 TRAE SOLO 并进 Trae：**国内版 `solo-cn → trae-cn`、国际版 `solo-intl → trae-intl`**
 * （两条线同一次动作）。只剩一个程序，SOLO 的数据目录留在原处。
 * 本机（已合并）的 SOLO 因此是 `installed = true`（旧数据还在）+ `runnable = false`
 * （程序已卸载）——它仍然**能读**（旧会话要看得见、账号要能带过来），
 * 但**不能再作为一个可切换的客户端出现在界面上**：选中它只会让人以为切了号，
 * 实际写进一个永远不会被读的目录（症状就是「点了没反应 / 无法解密 / 不存在」）。
 *
 * ⚠️ 判据是「**已并入 且 本机跑不起来**」，不是「已并入」：老机器上 SOLO 还独立装着
 *    （`runnable = true`），那时它是个正常的可用客户端，摘掉反而是砍功能。
 * ⚠️ 判据与后端完全同源：`runnable` 就是 `trae_discover::has_own_exe` + 数据目录在，
 *    切号护栏（`ensure_switchable`）和导入目标护栏（`import_sessions`）也走这一条。
 *    两端各写一份规则，早晚会出现「界面显示能选、后端报错」这类自相矛盾。
 * ⚠️ **未安装**的客户端照旧出现（置灰）—— 那是「本机存在这个概念」的提示，
 *    与「已并入」是两件事（见 `TraeClientSwitcher` 里 `installed` 与 `runnable` 的注释）。
 * ⚠️ 后端**不动** `trae_list_clients` 的返回：`solo-cn` / `solo-intl` 这两个 key 还得能查到
 *    （`merged_into` 反查承载方、已有连线数据里存的 `client_key`、账号并入的源扫描都靠它）。
 *    这里只管「画不画出来」。
 *
 * ⚠️ 另外还兼一个**临时**开关：国际版整体不开放（更新落后于国内版），见 [`INTL_GATE`]。
 *    同样是「只管画不画」，后端返回与数据一个字不动。
 */
export interface SelectableClientLike {
  key: string;
  installed: boolean;
  /** 本机能不能真把它跑起来（数据目录在 **且** 找得到**它自己的**主程序）。 */
  runnable?: boolean;
  /** `trae_list_clients` 的线上结构用 snake_case。 */
  merged_into?: string | null;
  /** `app_overview` 的结构用 camelCase。 */
  mergedInto?: string | null;
}

/**
 * 「国际版整体不开放」的**临时**前端开关（唯一一处）。
 *
 * 背景（2026-10）：国际版的更新落后于国内版，本轮功能国际版还没跟上 ⇒ 先把它的
 * **内容与功能都藏起来**，等后续跟上再解开。这是**前端**的开关 ——
 * 后端照旧返回国际版客户端（`trae_list_clients` 一个字不改），数据、连线、
 * 账号档案一个字节都不动，只是界面上不画、也选不到。
 *
 * ⚠️ 要解开：把 `enabled` 改回 `true` 即可。判据只在 `isSelectableClient` 一处，
 *    别在各页面自己加过滤 —— 5 处选择器 / 4 处默认值都走这一个出口，漏一处就是
 *    「标签条上没有了、但某个下拉里还选得到」。
 * ⚠️ 用可变对象而不是 `const INTL_ENABLED = false`：单测要**两种状态都跑一遍**
 *    （尤其「解开后国际版 SOLO 必须落到 `trae-intl`、不许落回国内版」这条护栏）。
 *    生产代码只读它，不会写。
 */
export const INTL_GATE = { enabled: false };

/** 该客户端是不是国际版（key 一律以 `-intl` 结尾，见后端的 `CLIENTS` 表）。 */
export function isIntlClient(key: string): boolean {
  return key.endsWith("-intl");
}

/** 该项该不该出现在客户端选择器里。 */
export function isSelectableClient(c: SelectableClientLike): boolean {
  // 国际版整体不开放（临时开关，见 `INTL_GATE`）。
  if (!INTL_GATE.enabled && isIntlClient(c.key)) return false;
  // 用 `||` 而不是 `??`：`""` 这种空值也算「没并入」，不然会被误判成已并入。
  const host = c.merged_into || c.mergedInto || null;
  return !(host !== null && c.runnable === false);
}

/** 过滤后的选择器入参；顺序原样保留（顺序只由后端的 `client_usage::order_keys()` 决定）。 */
export function selectableClients<T extends SelectableClientLike>(clients: T[]): T[] {
  return clients.filter(isSelectableClient);
}

/** 默认要看的那个：可见列表的第一项（顺序后端已排好，前端不另定规则）。 */
export function firstSelectableKey(clients: SelectableClientLike[]): string | null {
  return selectableClients(clients)[0]?.key ?? null;
}

/**
 * 定位选中项：原先选的那个若已从界面撤掉（例如 SOLO 并入后本机不再有它的程序，
 * 而 `localStorage` 里还留着上次选中的 `solo-cn`），回落到可见列表首项。
 *
 * ⚠️ 不能写成 `prev ?? first` —— 那样记忆里的死 key 会被一直保留，
 *    页面会去加载一个界面上已经没有的客户端，选中态与标签条对不上。
 */
export function resolveSelectableKey(
  prev: string | null,
  clients: SelectableClientLike[],
): string | null {
  const visible = selectableClients(clients);
  if (prev && visible.some((c) => c.key === prev)) return prev;
  return visible[0]?.key ?? null;
}
