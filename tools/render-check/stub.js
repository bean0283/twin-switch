// 渲染自查用的宿主桩（在页面脚本之前注入）。
//
// ⚠️ 两个全局都要给：`__TAURI_INTERNALS__`（invoke / transformCallback /
// unregisterCallback / convertFileSrc）与 `__TAURI_EVENT_PLUGIN_INTERNALS__`。
// 前端**会直接渲染其字段的命令绝不给 `{}`** —— 空对象会让页面在渲染期就炸，
// 于是「加载失败」和「没有数据」长得一模一样，自查结论全是假的。
(() => {
  const D = window.__RC_DATA__ || {};

  const handlers = {
    "plugin:app|version": () => "0.2.22",
    "plugin:event|listen": () => 1,
    "plugin:event|unlisten": () => null,
    "plugin:event|emit": () => null,

    // ---- 首页 / 通用 ----
    app_overview_cached: () => ({ empty: true, overview: null }),
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
