// 渲染自查用的宿主桩（在页面脚本之前注入）。
//
// ⚠️ 两个全局都要给：`__TAURI_INTERNALS__`（invoke / transformCallback /
// unregisterCallback / convertFileSrc）与 `__TAURI_EVENT_PLUGIN_INTERNALS__`。
// 前端**会直接渲染其字段的命令绝不给 `{}`** —— 空对象会让页面在渲染期就炸，
// 于是「加载失败」和「没有数据」长得一模一样，自查结论全是假的。
(() => {
  const D = window.__RC_DATA__ || {};

  const handlers = {
    // ⚠️ 每次发版都要跟着改 —— 截图页脚会照实显示桩里的版本，忘了改就会出现
    //    「代码是 vX、截图是 vY」的假证据。
    "plugin:app|version": () => "0.2.26",
    "plugin:event|listen": () => 1,
    "plugin:event|unlisten": () => null,
    "plugin:event|emit": () => null,

    // ---- 首页 / 通用 ----
    app_overview_cached: () => ({ empty: true, overview: null }),
    // ⚠️ 这两个以前**没桩**，兜底返回 `null` ⇒ 启动编排里的 `refreshOverview()` 永远拿不到
    //    数据，页面骨架常驻（实测 `skeleton=3` 25 秒不动）。补齐后页面才真的画完。
    app_overview_snapshot: () => D.appOverview,
    update_state: () => D.updateState,
    workbuddy_import_reference_accounts: () => D.refAccounts,
    trae_credits_query: () => D.traeCredits,
    trae_list_clients: () => D.clients,
    get_error_log_path: () => "C:/tmp/error.log",

    // ---- WorkBuddy 会话记录页 ----
    workbuddy_session_list_by_account: () => D.wbListByAccount,
    workbuddy_session_edge_sync_dbs: () => ({
      items: [{ name: "edge-sync-mapping-v4.db", path: "C:/x/edge-sync-mapping-v4.db", size: 4096 }],
      note: "云端映射库仅列出，本工具从不写入",
    }),
    workbuddy_session_links_preview: () => ({ ok: true, groups: [] }),
    workbuddy_session_copy_preview: () => ({ ok: true, items: [] }),

    // ---- WorkBuddy 会话编辑器 ----
    workbuddy_session_edit_doc: () => D.wbDoc,
    workbuddy_session_edit_lines: () => D.wbLines,
    workbuddy_session_edit_targets: () => D.wbTargets,
    workbuddy_session_edit_validate: () => ({
      errors: [],
      warnings: ["共 913 行（其中 0 行解析失败、10 个用户提问）"],
      errorCount: 0,
      warnCount: 1,
      stats: {},
    }),

    // ---- 数据迁移：WorkBuddy → Trae ----
    trae_workbuddy_list: () => D.wbList,
    // ⚠️ 这里必须给**真候选**：跨客户端页脚的目标账号就来自它，给空数组等于把
    //    「没有账号可写」这条禁用态当成通过。
    trae_import_candidates: () => D.traeImportCandidates,
    trae_import_inspect: () => ({
      client_key: "solo-cn",
      client_label: "Trae SOLO CN",
      account_id: "live",
      uid: "u-1",
      db_exists: true,
      running: false,
      key_ready: true,
      session_count: 0,
      will_create: 0,
      will_update: 0,
    }),

    // ---- 数据迁移：Trae → WorkBuddy ----
    trae_wb_export_target: () => D.wbTarget,
    trae_decrypted_status: () => ({
      clientKey: "solo-cn",
      exists: true,
      path: "C:/x/decrypted/solo-cn.db",
      pages: 1234,
      current: true,
      tables: [],
    }),
    trae_ensure_decrypted: () => ({
      reused: true,
      path: "C:/x/decrypted/solo-cn.db",
      pages: 1234,
      elapsed_ms: 1,
      tables: [],
    }),
    trae_wb_export_sessions: () => D.wbExportSessions,

    // ---- Trae 记录页 / Trae 会话编辑器（本轮唯一改动：页脚去掉「关闭编辑」按钮）----
    trae_list_sessions: () => ({ sessions: D.traeSessions }),
    trae_session_edit_doc: () => D.traeDoc,
    trae_session_edit_targets: () => D.traeTargets,
    trae_session_edit_validate: () => ({
      ok: true,
      errors: [],
      warnings: ["fts_* 是检索镜像，改动只影响搜索命中，不影响会话内容"],
    }),
    trae_session_edit_new_ids: () => [],
    trae_session_edit_cell: () => ({ ok: false, bytes: 0, value: null }),

    // ---- Trae 账号页（`/trae-switch`）----
    // ★ 本轮新增：账号页顶部的「已并入的客户端还有账号没带过来」提示条。
    //   这里必须给**非空的 adopted** —— 给空数组等于把「提示条没渲染」当成通过。
    trae_account_overview: () => D.traeOverview,
    trae_credits_cached: () => D.traeCredits,
    // ⚠️ 必须**按客户端区分**，不能无条件返回同一份：
    //    后端是「按 `merged_into == clientKey` 找源」，所以在 `solo-cn` 页面上（它不是
    //    承载方）结果**必然为空**。桩若无条件返回非空，页面就会出现真机上不会有的提示条，
    //    而且文案会把承载方写成源自己（「TRAE SOLO CN 已经并入 TRAE SOLO CN」）。
    trae_vault_adopt_merged: (args) =>
      args?.clientKey === "trae-cn"
        ? D.traeVaultAdoptMerged
        : {
            target: args?.clientKey ?? "",
            target_label: "",
            dry_run: Boolean(args?.dryRun),
            adopted: [],
            skipped: [],
          },
    trae_session_links_diverged: () => ({ ok: true, count: 0, groups: [] }),

    // ---- 首页（`/`）----
    // ⚠️ 这两条不补就是「未桩命令 ⇒ 返回 null」，首页 `trae.value.categories` 会直接抛
    //    `Cannot read properties of null (reading 'categories')` —— 自查里那条
    //    unhandledrejection 就是它，**不是页面 bug**（后端真会返回对象）。
    //    教训同「缺桩导致骨架常驻」：跑了新页面就得把该页要的命令补齐。
    trae_cleanup_scan: () => ({ categories: [] }),
    trae_wb_cleanup_scan: () => ({ totals: { recommended_bytes: 0 } }),
  };

  window.__TAURI_INTERNALS__ = {
    invoke: (cmd, args) => {
      const h = handlers[cmd];
      if (!h) {
        // 明确记一笔，便于把「桩没覆盖」和「页面真报错」区分开。
        console.warn("[rc] unstubbed command:", cmd, JSON.stringify(args || {}).slice(0, 200));
        return Promise.resolve(null);
      }
      try {
        return Promise.resolve(h(args || {}));
      } catch (e) {
        return Promise.reject(String(e));
      }
    },
    transformCallback: (cb, once) => {
      const id = Math.floor(Math.random() * 1e9);
      window[`_${id}`] = cb;
      if (once) setTimeout(() => delete window[`_${id}`], 0);
      return id;
    },
    unregisterCallback: () => {},
    convertFileSrc: (p) => p,
  };

  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: () => {},
  };
})();
