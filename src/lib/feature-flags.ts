/**
 * 前端功能开关（只影响界面入口，后端命令一律保留）。
 *
 * 规则：**只有用户明确要求隐藏入口时才用这里**，而且必须写清「后端还在、怎么恢复」——
 * 否则下一个人看到 `false` 会以为是废弃代码顺手删掉，把功能真删了。
 */

/**
 * 三个账号管理页（Trae / WorkBuddy / Qoder）的「导入本机登录态」入口是否显示。
 *
 * 2026-11-08 用户要求：三个页面都**隐藏**这个入口，后端功能保留。
 * 后端命令仍在（`trae_import_local_login` / `trae_import_all_local_logins` /
 * WorkBuddy 的导入、Qoder 的 `qoder_import_local`），随时可以恢复。
 *
 * 想恢复入口：把这个常量改回 `true`。三个页面共用这一个开关，别在各页面里各写一份。
 */
export const SHOW_LOCAL_LOGIN_IMPORT = false;
