// 把真机探针产出的 doc / lines 包成渲染自查用的桩数据。
import fs from "node:fs";
import path from "node:path";

const HERE = path.dirname(
  decodeURIComponent(new URL(import.meta.url).pathname.replace(/^\//, "")),
);
const DOC = "D:/htw/签到/.workbuddy/t46-wb-doc.json";
const LINES = `${DOC}.lines.json`;
// ⚠️ 直接写到 driver 读取的位置（`tools/render-check/data.json`）。
//    以前这里输出到别处、再手工拷过去，链路一断没人发现 —— 本轮自查时就发现拷丢了，
//    于是整条渲染自查静默失效（driver 直接读不到文件）。
const OUT = path.join(HERE, "data.json");

const doc = JSON.parse(fs.readFileSync(DOC, "utf8"));
const lines = JSON.parse(fs.readFileSync(LINES, "utf8"));
// Trae 编辑器的留档可能缺失 —— 缺了就给空对象：只有「点开编辑器」的那几档会用到它，
// 普通列表档不受影响。⚠️ 不给 `null`：`stub.js` 直接把它当 doc 渲染，`null` 会让页面在渲染期就炸。
const traeDocPath = path.join(HERE, "trae-doc.json");
const traeDoc = fs.existsSync(traeDocPath)
  ? JSON.parse(fs.readFileSync(traeDocPath, "utf8"))
  : {};

const uid = doc.ownerUid;
const cid = doc.sessionId;
const uid2 = "11111111-2222-4333-8444-55555555aaaa";
const labelOf = (u) => (u === uid ? "测试账号（uid …5d5e70）" : `另一个账号（uid …${u.slice(-6)}）`);

const sessionItem = {
  id: cid,
  title: doc.title,
  cwd: String(doc.row.cwd || "D:\\htw\\test"),
  model: String(doc.row.model || "deepseek-v4.1-flash"),
  createdAt: Number(doc.row.created_at || 0),
  updatedAt: Number(doc.row.updated_at || 0),
  hasBody: true,
  bodyBytes: doc.body.bytes,
  dupGroup: "",
  isNewest: true,
  dupNote: "",
};

const data = {
  clients: {
    clients: [
      // ⚠️ 顺序必须复刻**后端排序之后**的结果，不是 `CLIENTS` 的声明顺序。
      //
      // 2026-10 官方把 SOLO 并进 Trae CN 之后，`solo-cn` 的使用分仍然最高
      // （真机 127 次 / 切号 22 次），但它的主程序已经没了 —— 后端的
      // `client_usage::sort_with_order` 会把「跑不起来」的客户端整体沉到末尾，
      // 所以界面取 `installed[0]` 拿到的是 `trae-cn`。
      //
      // 桩数据如果照 `CLIENTS` 的声明顺序写（`solo-cn` 在前），页面就会停在
      // `solo-cn` 上 —— 整场自查验的是**一个真机上不会出现的界面**。
      {
        key: "trae-cn",
        label: "Trae CN",
        user_data_dir: "C:/x/Trae CN",
        installed: true,
        exe: "C:/x/Trae CN/Trae CN.exe",
        has_login: true,
        merged_into: null,
        runnable: true,
      },
      {
        key: "solo-cn",
        label: "TRAE SOLO CN",
        user_data_dir: "C:/x/Trae SOLO CN",
        installed: true,
        exe: null,
        has_login: true,
        merged_into: "trae-cn",
        runnable: false,
      },
    ],
    usage: {
      // 「排在最前且确有使用历史」——排序后第一位是 trae-cn，它有 13 次使用 ⇒ 就是它。
      topPick: "trae-cn",
      switchWeight: 2,
      preference: ["trae-cn", "solo-cn"],
      clients: [],
    },
  },
  wbListByAccount: {
    ok: true,
    accounts: [
      { uid, label: labelOf(uid), count: 2, sessions: [sessionItem, { ...sessionItem, id: `${cid}-b`, title: "第二条会话（用于看窄档布局下标题会不会把操作按钮挤走）" }] },
      { uid: uid2, label: labelOf(uid2), count: 1, sessions: [{ ...sessionItem, id: `${cid}-c`, title: "另一个账号下的一条会话" }] },
    ],
  },
  wbDoc: doc,
  wbLines: lines,
  wbTargets: {
    ok: true,
    accounts: [
      { uid, label: labelOf(uid), sessions: 12 },
      { uid: uid2, label: labelOf(uid2), sessions: 3 },
    ],
  },
  wbList: {
    available: true,
    data_root: "C:/Users/x/.workbuddy",
    accounts: [
      { uid, label: labelOf(uid), name: "测试账号", meta: "", kind: "personal" },
      { uid: uid2, label: labelOf(uid2), name: "另一个账号", meta: "", kind: "personal" },
    ],
    sessions: [
      {
        id: cid,
        title: doc.title,
        cwd: String(doc.row.cwd || "D:\\htw\\test"),
        user_id: uid,
        created_at: Number(doc.row.created_at || 0),
        updated_at: Number(doc.row.updated_at || 0),
        model: "deepseek-v4.1-flash",
        deleted: false,
        has_body: true,
        body_bytes: doc.body.bytes,
        body_lines: doc.body.lineCount,
        content_digest: "0123456789abcdef",
        dup_group: "",
        is_newest: true,
        dup_note: "",
      },
    ],
  },
  wbTarget: {
    available: true,
    data_root: "C:/Users/x/.workbuddy",
    db_path: "C:/Users/x/.workbuddy/workbuddy.db",
    projects_dir: "C:/Users/x/.workbuddy/projects",
    running: false,
    exe: null,
    accounts: [
      { uid, label: labelOf(uid), name: "测试账号", meta: "", kind: "personal" },
      { uid: uid2, label: labelOf(uid2), name: "另一个账号", meta: "", kind: "personal" },
    ],
    default_uid: uid,
  },
  // 「WorkBuddy → Trae」页脚跨客户端写入的目标账号：故意给三家（含一个「无会话库」、一个别的客户端），
  // 用来验证 label 里的客户端名与「无会话库」标注都真的渲染出来。
  traeImportCandidates: {
    candidates: [
      {
        client_key: "solo-cn",
        client_label: "Trae SOLO CN",
        account_id: "live",
        label: labelOf(uid),
        display: null,
        uid: "aaaa1111-2222-4333-8444-555566667777",
        kind: "live",
        db_exists: true,
        is_current: true,
      },
      {
        client_key: "solo-cn",
        client_label: "Trae SOLO CN",
        account_id: "oauth:2",
        label: "另一个 Trae 账号（uid …445566）",
        display: null,
        uid: "bbbb1111-2222-4333-8444-555566664455",
        kind: "oauth",
        db_exists: false,
      },
      {
        client_key: "trae-cn",
        client_label: "TRAE CN",
        account_id: "live",
        label: "国际版账号（uid …778899）",
        display: null,
        uid: "cccc1111-2222-4333-8444-555566667788",
        kind: "live",
        db_exists: true,
      },
    ],
  },
  wbExportSessions: {
    client_key: "solo-cn",
    client_label: "Trae SOLO CN",
    data_root: "C:/x/decrypted",
    count: 1,
    sessions: [
      {
        id: "68f0a1b2c3d4e5f6a7b8c9d0",
        title: "Trae 侧的一条会话（导出前先改一改）",
        cwd: "D:\\htw\\签到",
        workspace_key: "d-htw-签到",
        created_ms: Number(doc.row.created_at || 0),
        updated_ms: Number(doc.row.updated_at || 0),
        turns: 10,
        owner_uid: uid,
        owner_label: labelOf(uid),
      },
    ],
  },
  traeDoc,
  traeTargets: {
    clientKey: "solo-cn",
    accounts: [
      {
        uid: "aaaa1111-2222-4333-8444-555566667777",
        label: "测试账号（uid …667777）",
        projects: 3,
        sessions: 41,
      },
    ],
  },
  traeSessions: [
    {
      id: traeDoc.sessionId || "6ac22cadf92b0001",
      title: traeDoc.title || "（无标题）",
      created: "2026-10-10 20:00:00",
      updated: "2026-10-10 22:00:00",
      turns: 41,
      messages: 82,
      owner_uid: traeDoc.ownerUid || "",
      owner_label: "测试账号（uid …667777）",
    },
  ],

  // ---- Trae 账号页（`/trae-switch`）----
  //
  // ★ 本轮新增：账号页顶部要出现「已并入的客户端还有账号没带过来」提示条。
  //   复刻真机数字：Trae CN 下 1 个账号、SOLO 下 3 个（其中 223728 两边都有，
  //   所以「待并入」是 717036 / 882023 两个）。
  traeOverview: {
    clientKey: "trae-cn",
    loggedIn: true,
    running: true,
    live: {
      clientKey: "trae-cn",
      label: "Trae CN",
      hasStorage: true,
      loggedIn: true,
      uid: "4052137095223728",
      username: "测试账号",
      avatarUrl: null,
      email: null,
      region: "CN",
      host: "https://api.trae.cn",
      deviceId: null,
      machineId: null,
      devDeviceId: null,
      tokenExp: 1893456000,
      refreshExp: 1893456000,
      tokenExpText: "2030-01-01 00:00",
      refreshExpText: "2030-01-01 00:00",
      clientVersion: "1.107.1",
      knownUids: ["4052137095223728", "842681600717036", "925158341882023"],
    },
    vault: [
      {
        id: "oauth_223728",
        kind: "oauth",
        displayName: "测试账号",
        meta: {
          id: "oauth_223728",
          client: "trae-cn",
          kind: "oauth",
          root_dir: "C:/x/Trae CN",
          entries: [],
          files: [],
          file_count: 0,
          total_bytes: 0,
          created_at: "2026-10-04T09:23:00+08:00",
          last_used_at: "2026-10-12T18:13:00+08:00",
          verified_uid: "4052137095223728",
        },
        oauth: {
          kind: "oauth",
          id: "oauth_223728",
          client: "trae-cn",
          displayName: "测试账号",
          uid: "4052137095223728",
          host: "https://api.trae.cn",
          tokenExp: 1893456000,
        },
      },
    ],
  },

  // 预演结果（`dry_run = true`）：提示条只在 `adopted` 非空时出现。
  traeVaultAdoptMerged: {
    target: "trae-cn",
    target_label: "Trae CN",
    dry_run: true,
    adopted: [
      {
        from: "solo-cn",
        from_label: "TRAE SOLO CN",
        source_id: "oauth_717036",
        id: "oauth_717036",
        uid: "842681600717036",
      },
      {
        from: "solo-cn",
        from_label: "TRAE SOLO CN",
        source_id: "oauth_882023",
        id: "oauth_882023",
        uid: "925158341882023",
      },
    ],
    skipped: [],
  },

  traeCredits: {
    ok: true,
    clientKey: "trae-cn",
    accounts: [],
    summary: { queried: 0, succeeded: 0, failed: 0, totalRemaining: 0 },
    updatedAt: null,
    cached: true,
  },

  // ---- 全局总览（`app_overview_snapshot`）----
  //
  // ⚠️ 以前这里**没有**这份桩，`stub.js` 的兜底就返回 `null` ⇒ `refreshOverview()` 拿不到
  //    数据，首页/账号页的骨架一直转（实测：`skeleton=3` 常驻 25 秒不动）。**挂载 ≠ 画完了**，
  //    自查必须让页面真的走完这条链，否则「版式干净」是在半成品上量的。
  appOverview: {
    ok: true,
    generatedAt: 1760300000000,
    trae: {
      clients: [
        {
          key: "trae-cn",
          label: "Trae CN",
          installed: true,
          exe: "C:/x/Trae CN/Trae CN.exe",
          userDataDir: "C:/x/Trae CN",
          hasLogin: true,
          mergedInto: null,
          runnable: true,
          loginLabel: "测试账号（…3728）",
          running: true,
          processCount: 1,
          accounts: 1,
          credits: { accountCount: 1, queryable: 1, withData: 1, totalRemaining: 12345, updatedAt: 1760300000000 },
          decrypted: {
            exists: true,
            current: true,
            path: "C:/x/decrypted/trae-cn.db",
            pages: 1234,
            tableCount: 9,
            createdMs: 1760300000000,
            sessionCount: 23,
            dbBytes: 622700544,
          },
        },
        {
          key: "solo-cn",
          label: "TRAE SOLO CN",
          installed: true,
          exe: null,
          userDataDir: "C:/x/Trae SOLO CN",
          hasLogin: true,
          mergedInto: "trae-cn",
          runnable: false,
          loginLabel: "测试账号（…2023）",
          running: false,
          processCount: 0,
          accounts: 3,
          credits: { accountCount: 3, queryable: 3, withData: 3, totalRemaining: 4567, updatedAt: 1760300000000 },
          decrypted: {
            exists: false,
            current: false,
            path: "C:/x/decrypted/solo-cn.db",
            pages: 0,
            tableCount: 0,
            createdMs: 0,
            sessionCount: 7,
            dbBytes: 327155712,
          },
        },
      ],
      installedClients: 2,
      runningClients: 1,
      accountTotal: 4,
      sessionTotal: 30,
      anyDecrypted: true,
      topPick: "trae-cn",
    },
    workbuddy: {
      running: true,
      processCount: 1,
      processes: [{ name: "WorkBuddy.exe", pid: 4321 }],
      exe: "C:/x/WorkBuddy/WorkBuddy.exe",
      loggedIn: true,
      currentUid: uid,
      currentLabel: labelOf(uid),
      authFilePath: "C:/x/.wb-switch/accounts.json",
      authFileExists: true,
      authFileBytes: 2048,
      accounts: [
        { id: uid, uid, name: "测试账号", tokenState: "plain", queryable: true, isCurrent: true, lastUsedAt: 1760300000000 },
      ],
      accountCount: 1,
      queryableCount: 1,
      sessionCount: 2,
      deletedSessionCount: 0,
      bodyFileCount: 2,
      dbPath: "C:/x/.workbuddy/workbuddy.db",
      dbBytes: 1048576,
      dbExists: true,
      lastSwitch: null,
    },
    credits: { ok: true, accounts: [], errors: [], updatedAt: null, cached: true, summary: { queried: 0, succeeded: 0, failed: 0 } },
    notes: [],
  },

  // 更新状态：给一份「已是最新」，免得更新入口卡在检查中。
  updateState: {
    currentVersion: "0.2.24",
    latestVersion: "0.2.24",
    hasUpdate: false,
    checking: false,
    downloading: false,
    downloaded: false,
    error: null,
    notes: null,
    publishedAt: null,
  },

  // 启动时那次自动导入（T43 起由 `main.tsx` 编排，界面无按钮）。
  refAccounts: { created: 0, updated: 0, imported: 0, total: 1, available: true, note: null, kept: 0, blocked: 0 },
};

fs.writeFileSync(OUT, JSON.stringify(data), "utf8");
console.log("data.json written", fs.statSync(OUT).size, "bytes; lines =", lines.lines.length);
