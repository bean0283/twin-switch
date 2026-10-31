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

/**
 * Token 弹窗样本的时间戳 —— **按运行时刻现算**。
 *
 * ⚠️ 不许写死毫秒常量：写死之后「有效期」显示的年月日会随日子漂，而 `remainingDays`
 *    还是老数字 ⇒ 同一屏两个数字对不上（项目硬规则「同一屏的两个数字必须同源」），
 *    而且探针里那条「年份必须落在 2000~2999」的判据过几年就会自己变红。
 *    线上截图那个账号就是**只剩 5 天**，所以这里刻意造 5 天。
 */
const T5 = Date.now() + 5 * 86_400_000; // access token：还剩 5 天
const T14 = Date.now() + 14 * 86_400_000; // refresh 额度：还剩 14 天

/**
 * T60 的 token 弹窗样本：**老登录的 token 还剩 55 天**。
 *
 * 这正是用户看到「以前将近 2 个月」的那个形态 —— 10 月初登录时服务端签的是 55 天；
 * 而官方 2026-10 起把**新签发**的收紧了，所以这条样本一强制刷新就会「变短」，
 * 弹窗里那条「本次刷新 前 → 后」对照条 + 「刷新 ≠ 延长」的提示才有东西可画。
 *
 * ⚠️⚠️ 收紧后的值**两个产品不一样，样本也必须分开**。T60 初版两边共用了 `T28`，
 *    结果 Trae 弹窗里写着「官方目前签发的就是 28 天」—— 那是 WorkBuddy 的值。共用一个
 *    常量就抓不住这个错，所以这里刻意留两个不同的常量，探针也按产品分别断言。
 * - **Trae = 14 天**（`T14_ISSUED`）：本机 4 份刚续期成功的账号档
 *   （`trae/vault/<客户端>/oauth.json`）`exp - iat` 都是 **1209600 s 整**。
 * - **WorkBuddy = 28 天**（`T28`）：官方客户端自己写的
 *   `backups/workbuddy-desktop.20261007-183235.info` → `auth.expiresIn = 2419200 s`。
 */
const T55 = Date.now() + 55 * 86_400_000; // 老登录拿到的 token：还剩 55 天
const T28 = Date.now() + 28 * 86_400_000; // WorkBuddy 服务端现在签发的：28 天
const T14_ISSUED = Date.now() + 14 * 86_400_000; // Trae 服务端现在签发的：14 天

/** WorkBuddy 账号页的积分样本（两条账号共用一条，够画卡片与「全部积分包」弹窗）。 */
const WB_CREDIT_SAMPLE = {  ok: true,
  account: {
    id: "1736a5f4-f580-48c5-91b7-cd15a4421d6f",
    uid: "6aa2c43b-05f5-489a-840a-298f6cbe81ba",
    name: "弦思",
    tokenState: "plain",
    queryable: true,
    blockedReason: null,
  },
  totalCapacity: 1_500_000,
  totalRemaining: 1_494_260,
  expiringSoonRemaining: 0,
  expiredRemaining: 0,
  soonestExpireAt: 1793966379672,
  expiringSoon: [],
  expired: false,
  packageCount: 14,
  activePackageCount: 14,
  resources: [
    {
      packageCode: "TCACA_code_002_AkiJS3ZHF5",
      packageName: null,
      total: 1_500_000,
      remaining: 1_494_260,
      used: 5_740,
      status: 0,
      expireAt: 1793966379672,
      expired: false,
      expiringSoon: false,
    },
  ],
  activeResources: [
    {
      packageCode: "TCACA_code_002_AkiJS3ZHF5",
      packageName: null,
      total: 1_500_000,
      remaining: 1_494_260,
      used: 5_740,
      status: 0,
      expireAt: 1793966379672,
      expired: false,
      expiringSoon: false,
    },
  ],
  updatedAt: 1760300000000,
  source: "summary",
  refreshed: false,
};

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
      // 2026-10 官方把 SOLO 并进 Trae（国内版 → `trae-cn`，国际版 → `trae-intl`），
      // `solo-cn` 的使用分仍然最高（真机 127 次 / 切号 22 次），但它的主程序已经没了 —— 后端的
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
      // 国际版两条线（2026-10 同一次动作：`solo-intl → trae-intl`）。
      // 本机连数据目录都没有 ⇒ `installed: false`。
      // ⚠️ `trae-intl` 必须一起放进来：否则「两条 SOLO 都不在标签条上」可能是被
      //    「国际版整条线都没有」混过去的 —— 那验不出「只摘 SOLO、不连坐承载方」。
      {
        key: "solo-intl",
        label: "TRAE SOLO（国际版）",
        user_data_dir: "C:/x/TRAE SOLO",
        installed: false,
        exe: null,
        has_login: false,
        merged_into: "trae-intl",
        runnable: false,
      },
      {
        key: "trae-intl",
        label: "Trae（国际版）",
        user_data_dir: "C:/x/Trae",
        installed: false,
        exe: null,
        has_login: false,
        merged_into: null,
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

  // 预演结果（`dry_run = true`）：提示条只在 `adopted` 或 `repaired` 非空时出现。
  //
  // ⚠️ 两个数组都给：`adopted` 走「还有 N 个账号留在别处」，`repaired` 走「补全老版本漏搬的
  //    网页凭证」—— 同一屏两种文案都渲染，一次自查把两条分支都覆盖到。
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
    // T51：老版本 `export_account` 只搬载体 ⇒ 先搬过来的账号缺 `oauth.json` / `profile.json`。
    repaired: [
      {
        from: "solo-cn",
        from_label: "TRAE SOLO CN",
        source_id: "oauth_223728",
        id: "oauth_223728",
        uid: "4052137095223728",
        files: ["oauth.json", "profile.json"],
      },
    ],
    skipped: [],
  },

  // ---- Trae 账号 token 弹窗（T54）----
  // 一段**形如 JWT** 的串（三段、足够长 ⇒ 前端会打码成 `前 18 位…后 10 位`，
  // 探针就靠这个 `…` 断言「默认是打码的，不是把 token 直接糊在屏幕上」）。
  traeAccountToken: {
    ok: true,
    clientKey: "trae-cn",
    id: "oauth_223728",
    uid: "4052137095223728",
    token:
      "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9." +
      "eyJzdWIiOiI0MDUyMTM3MDk1MjIzNzI4IiwidWlkIjoiNDA1MjEzNzA5NTIyMzcyOCIsImV4cCI6MTc5Mjg4NjU3NH0." +
      "7c9f0a3b2e5d8c1f4a6b9d0e3f2a5c8b1d4e7f0a3c6b9d2e5f8a1c4b7d0e3f6a9",
    // ⚠️ `tokenExp` 必须是**毫秒**。旧实现把它再乘 1000 ⇒ 界面显示「58765/4/20 还剩
    //    2000 万天」，探针里的 `saneYear` 就是这条的抓手。
    // ⚠️ T60 起刻意用「老登录还剩 55 天」：官方现在只签发 14 天（Trae 侧），强制刷新后
    //    到期时间反而**提前** —— 对照条（`data-refresh-shift="shortened"`）要靠它才画得出来。
    tokenExp: T55,
    source: "oauth",
    canRefresh: true,
    thresholdDays: 7,
    remainingDays: 55,
  },
  // 默认这条路（`force:false`）：剩余天数够 ⇒ 后端 `refreshed:false` ⇒ 前端弹「强制刷新」确认框。
  traeAccountTokenRefresh: {
    ok: true,
    refreshed: false,
    reason: "fresh",
    force: false,
    source: "oauth",
    token:
      "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9." +
      "eyJzdWIiOiI0MDUyMTM3MDk1MjIzNzI4IiwidWlkIjoiNDA1MjEzNzA5NTIyMzcyOCIsImV4cCI6MTc5Mjg4NjU3NH0." +
      "7c9f0a3b2e5d8c1f4a6b9d0e3f2a5c8b1d4e7f0a3c6b9d2e5f8a1c4b7d0e3f6a9",
    tokenExp: T55,
    tokenExpBefore: T55,
    remainingDays: 55,
    thresholdDays: 7,
    refreshExpiredAt: null,
    updatedFiles: [],
    message: "token 仍然新鲜（剩余 55 天 > 门槛 7 天），本次未刷新",
  },

  // ★ T60 新增（`force:true` 这一趟）：Trae 服务端签发的**就是 14 天**，比原来（55 天）短。
  //   探针点完确认窗里的「强制刷新」后就断言对照条 ⇒ 判据钩在 `data-refresh-shift` 上；
  //   同时按 `data-issued-validity-days` 断言「Trae 档写的是 14，不是 WorkBuddy 的 28」。
  traeAccountTokenRefreshForced: {
    ok: true,
    refreshed: true,
    reason: "refreshed",
    force: true,
    source: "oauth",
    token:
      "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9." +
      "eyJzdWIiOiI0MDUyMTM3MDk1MjIzNzI4IiwidWlkIjoiNDA1MjEzNzA5NTIyMzcyOCIsImV4cCI6MTc5NDMxNDAwMH0." +
      "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f809",
    tokenExp: T14_ISSUED,
    tokenExpBefore: T55,
    remainingDays: 14,
    thresholdDays: 7,
    refreshExpiredAt: null,
    updatedFiles: [],
    message: "已刷新；新有效期由服务端签发（14 天，比原来短）",
  },

  // 刷新**之后**再读一遍账号档案：到期时间要跟着变成 14 天。
  // ⚠️ 不换这一份，同一屏就会「有效期」写 55 天、下面凑着「刷新前 55 天 → 后 14 天」——
  //    同一屏两个数字不同源（项目硬规则）。
  traeAccountTokenAfter: {
    ok: true,
    clientKey: "trae-cn",
    id: "oauth_223728",
    uid: "4052137095223728",
    token:
      "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9." +
      "eyJzdWIiOiI0MDUyMTM3MDk1MjIzNzI4IiwidWlkIjoiNDA1MjEzNzA5NTIyMzcyOCIsImV4cCI6MTc5NDMxNDAwMH0." +
      "1a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f809",
    tokenExp: T14_ISSUED,
    source: "oauth",
    canRefresh: true,
    thresholdDays: 7,
    remainingDays: 14,
  },

  traeCredits: {
    ok: true,
    clientKey: "trae-cn",
    accounts: [],
    summary: { queried: 0, succeeded: 0, failed: 0, totalRemaining: 0 },
    updatedAt: null,
    cached: true,
  },

  // ---- WorkBuddy 账号页（`/workbuddy-switch`）----
  //
  // ⚠️ 这一页以前**从没进过自查**，所以下面这些命令一条桩都没有：不补的话未桩命令返回
  //    `null` ⇒ 页面停在骨架（`skeleton` 常驻），探针会报「没找到账号头像按钮」，
  //    看着像「头像入口没做」，其实是桩缺了（同 §9 硬规则）。
  wbAccountList: {
    accounts: [
      {
        id: "1736a5f4-f580-48c5-91b7-cd15a4421d6f",
        uid: "6aa2c43b-05f5-489a-840a-298f6cbe81ba",
        name: "弦思",
        nickname: "弦思",
        email: null,
        hasToken: true,
        expiresAt: T5,
        lastUsedAt: Date.now() - 86_400_000,
        isCurrent: true,
      },
      {
        id: "b21f0c9e-2a44-4d3e-9f10-77c0a1e5d2b8",
        uid: "3f21ab77-11c2-4e35-8d90-5a2b6c7d8e9f",
        name: "账号 88d9",
        nickname: null,
        email: null,
        hasToken: true,
        expiresAt: T5,
        lastUsedAt: null,
        isCurrent: false,
      },
    ],
    currentUid: "6aa2c43b-05f5-489a-840a-298f6cbe81ba",
    loggedIn: true,
    authFilePath: "C:/Users/11970/.workbuddy/auth.json",
  },

  wbCreditsAccounts: {
    ok: true,
    count: 2,
    queryable: 2,
    accounts: [
      {
        id: "1736a5f4-f580-48c5-91b7-cd15a4421d6f",
        uid: "6aa2c43b-05f5-489a-840a-298f6cbe81ba",
        name: "弦思",
        tokenState: "plain",
        queryable: true,
        blockedReason: null,
      },
      {
        id: "b21f0c9e-2a44-4d3e-9f10-77c0a1e5d2b8",
        uid: "3f21ab77-11c2-4e35-8d90-5a2b6c7d8e9f",
        name: "账号 88d9",
        tokenState: "plain",
        queryable: true,
        blockedReason: null,
      },
    ],
  },

  wbCreditsCached: {
    ok: true,
    accounts: [WB_CREDIT_SAMPLE],
    errors: [],
    updatedAt: 1760300000000,
    cached: true,
    summary: { queried: 2, succeeded: 2, failed: 0, totalRemaining: 1_494_260 },
  },

  wbCreditsQuery: {
    ok: true,
    accounts: [WB_CREDIT_SAMPLE],
    errors: [],
    updatedAt: 1760300000000,
    cached: false,
    summary: { queried: 2, succeeded: 2, failed: 0, totalRemaining: 1_494_260 },
  },

  // ---- WorkBuddy 账号 token 弹窗（T56；与 Trae 侧同一套 UI）----
  //
  // ⚠️ 刻意造成「老登录还剩 55 天」：官方 2026-10 起新签发的只有 28 天，强制刷新后
  // 到期时间反而**提前** —— 弹窗里那条对照条（`data-refresh-shift="shortened"`）与
  // 「刷新 ≠ 延长」的提示都靠这条样本才画得出来；同时年份落在合理区间 ⇒ `saneYear` 有东西可抓。
  wbAccountToken: {
    ok: true,
    id: "1736a5f4-f580-48c5-91b7-cd15a4421d6f",
    uid: "6aa2c43b-05f5-489a-840a-298f6cbe81ba",
    name: "弦思",
    token:
      "eyJhbGciOiJSUzI1NiIsInR5cCIgOiAiSldUIiwia2lkIiA6ICJteWZFenA3ODNLaV9KQ3g4Vm5jM1hfaXg2alpyYjZDZjVPTWtHWk1QSTNzIn0." +
      "eyJleHAiOjE3OTI4ODY1NzQsImlhdCI6MTc5MTY3Njk3NCwidWlkIjoiM2YyMWFiNzctMTFjMi00ZTM1LThkOTAtNWEyYjZjN2Q4ZTlmIn0." +
      "9d2f4a6b8c0e2f4a6b8c0e2f4a6b8c0e2f4a6b8c0e2f4a6b8c0e2f4a6b8c0e",
    tokenExp: T55,
    refreshExpiredAt: T14,
    source: "plain",
    blockedReason: null,
    canRefresh: true,
    thresholdDays: 7,
    remainingDays: 55,
  },

  // 与 Trae 侧同一条路（`force:false`）：后端回 `refreshed:false` ⇒ 前端弹「强制刷新」确认窗。
  wbAccountTokenRefresh: {
    ok: true,
    refreshed: false,
    reason: "fresh",
    force: false,
    token:
      "eyJhbGciOiJSUzI1NiIsInR5cCIgOiAiSldUIiwia2lkIiA6ICJteWZFenA3ODNLaV9KQ3g4Vm5jM1hfaXg2alpyYjZDZjVPTWtHWk1QSTNzIn0." +
      "eyJleHAiOjE3OTI4ODY1NzQsImlhdCI6MTc5MTY3Njk3NCwidWlkIjoiM2YyMWFiNzctMTFjMi00ZTM1LThkOTAtNWEyYjZjN2Q4ZTlmIn0." +
      "9d2f4a6b8c0e2f4a6b8c0e2f4a6b8c0e2f4a6b8c0e2f4a6b8c0e2f4a6b8c0e",
    tokenExp: T55,
    tokenExpBefore: T55,
    remainingDays: 55,
    thresholdDays: 7,
    refreshExpiredAt: T14,
    updatedFiles: [],
    message: "token 仍然新鲜（剩余 55 天 > 门槛 7 天），本次未刷新",
  },

  // ★ T60 新增（`force:true`）：服务端签发 28 天，比原来（55 天）短 ⇒ 对照条标「变短」。
  wbAccountTokenRefreshForced: {
    ok: true,
    refreshed: true,
    reason: "refreshed",
    force: true,
    token:
      "eyJhbGciOiJSUzI1NiIsInR5cCIgOiAiSldUIiwia2lkIiA6ICJteWZFenA3ODNLaV9KQ3g4Vm5jM1hfaXg2alpyYjZDZjVPTWtHWk1QSTNzIn0." +
      "eyJleHAiOjE3OTQzMTQwMDAsImlhdCI6MTc5MTY3Njk3NCwidWlkIjoiM2YyMWFiNzctMTFjMi00ZTM1LThkOTAtNWEyYjZjN2Q4ZTlmIn0." +
      "3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a",
    tokenExp: T28,
    tokenExpBefore: T55,
    remainingDays: 28,
    thresholdDays: 7,
    refreshExpiredAt: T14,
    updatedFiles: [],
    message: "已刷新；新有效期由服务端签发（28 天，比原来短）",
  },

  // 刷新**之后**再读一遍：到期时间跟着变成 28 天（同一屏两个数字必须同源）。
  wbAccountTokenAfter: {
    ok: true,
    id: "1736a5f4-f580-48c5-91b7-cd15a4421d6f",
    uid: "6aa2c43b-05f5-489a-840a-298f6cbe81ba",
    name: "弦思",
    token:
      "eyJhbGciOiJSUzI1NiIsInR5cCIgOiAiSldUIiwia2lkIiA6ICJteWZFenA3ODNLaV9KQ3g4Vm5jM1hfaXg2alpyYjZDZjVPTWtHWk1QSTNzIn0." +
      "eyJleHAiOjE3OTQzMTQwMDAsImlhdCI6MTc5MTY3Njk3NCwidWlkIjoiM2YyMWFiNzctMTFjMi00ZTM1LThkOTAtNWEyYjZjN2Q4ZTlmIn0." +
      "3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f8091a",
    tokenExp: T28,
    refreshExpiredAt: T14,
    source: "plain",
    blockedReason: null,
    canRefresh: true,
    thresholdDays: 7,
    remainingDays: 28,
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
        // 国际版两条线：本机连数据目录都没有（`installed: false`）。
        // ⚠️ 必须把 `trae-intl` 一起放进来 —— 只放 SOLO 会让「标签条上没 SOLO」
        //    这条判据分辨不出「摘对了」和「整条线都没了」。
        {
          key: "solo-intl",
          label: "TRAE SOLO（国际版）",
          installed: false,
          exe: null,
          userDataDir: "C:/x/TRAE SOLO",
          hasLogin: false,
          mergedInto: "trae-intl",
          runnable: false,
          loginLabel: null,
          running: false,
          processCount: 0,
          accounts: 0,
          credits: { accountCount: 0, queryable: 0, withData: 0, totalRemaining: 0, updatedAt: null },
          decrypted: {
            exists: false,
            current: false,
            path: "C:/x/decrypted/solo-intl.db",
            pages: 0,
            tableCount: 0,
            createdMs: 0,
            sessionCount: 0,
            dbBytes: 0,
          },
        },
        {
          key: "trae-intl",
          label: "Trae（国际版）",
          installed: false,
          exe: null,
          userDataDir: "C:/x/Trae",
          hasLogin: false,
          mergedInto: null,
          runnable: false,
          loginLabel: null,
          running: false,
          processCount: 0,
          accounts: 0,
          credits: { accountCount: 0, queryable: 0, withData: 0, totalRemaining: 0, updatedAt: null },
          decrypted: {
            exists: false,
            current: false,
            path: "C:/x/decrypted/trae-intl.db",
            pages: 0,
            tableCount: 0,
            createdMs: 0,
            sessionCount: 0,
            dbBytes: 0,
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
    currentVersion: "0.4.7",
    latestVersion: "0.4.7",
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

  // ---- 设置页（`/settings`，T62）----
  // 与后端 `settings::BackupRetention::default()` 同值（30 天 / 3 份 / 4096 MB）。
  // ⚠️ 这三条数在 `backup-policy.test.ts` 里也钉过 —— 改这里要想想会不会把样本改成
  //    「界面上看着对、实际单位错」的假象。
  settings: {
    ok: true,
    backupRetention: { enabled: true, keepDays: 30, keepCount: 3, maxTotalMb: 4096 },
    summary: "保留策略：≤30 天 · 每客户端 ≤3 份 · 每目录 ≤4.0 GB（任一超限即清理；每客户端最新一批始终保留）",
    path: "C:/Users/x/.twin-switch/settings.json",
  },

  // ---- Trae 清理页（`/trae-cleanup`，T62 起带保留策略与「超出策略」标注）----
  // 刻意造「一个客户端有 4 批、策略只留 3 批」的形态：最新那批必须**不**被标成超出，
  // 更旧的那批必须被标上并带上原因 —— 这正是 T62 之前清不掉的形状。
  traeCleanup: {
    store_root: "C:/Users/x/.twin-switch",
    trash_root: "C:/Users/x/.twin-switch/trash",
    // ⚠️ 必须等于下面各类目之和（同屏两个数字同源）：402653184 + 971243520
    total_bytes: 1_373_896_704,
    total_mb: 1373.9,
    cached: false,
    stale: false,
    scannedAt: Date.now(),
    ageMs: 0,
    notes: [
      // ★ T61：影子客户端的提示必须是「已并入、只剩历史数据」，**不许**是「正在运行」。
      //   桩里放的是修好后的那句；探针按 DOM 断言页面上出现的是这一句。
      "TRAE SOLO CN 已并入Trae CN（国内版）、本机已没有它自己的程序 —— 只剩历史数据；删除它的会话不会结束任何进程。",
      "Trae CN（国内版） 正在运行：删除会话时会自动结束进程，并在完成后重新启动。",
    ],
    categories: [
      {
        id: "tool_residue",
        title: "工具残留（本工具目录）",
        desc:
          "整库备份、解密快照、导入中间产物与日志。删除后不影响已写入 Trae 的数据。" +
          "整库备份按客户端分组、最旧的排在前面 —— 清理顺序就是先从最老的清，" +
          "每个客户端最新的一批永远保留（可回滚）。",
        count: 3,
        recommended_count: 1,
        bytes: 402_653_184,
        bytes_mb: 384,
        backup_policy: {
          enabled: true,
          keepDays: 30,
          keepCount: 3,
          maxTotalMb: 4096,
          summary: "保留策略：≤30 天 · 每客户端 ≤3 份 · 每目录 ≤4.0 GB（任一超限即清理；每客户端最新一批始终保留）",
        },
        // ⚠️ 顺序 = 后端的真实顺序：**每个客户端内部从旧到新**（最旧的排最前）。
        //    样本要是把最新的排前面，探测「先从最老的清」这条规则的判据就白写了。
        items: [
          {
            // ★ 最旧 + 超出策略 ⇒ 必须同时挂「超出策略」和「最旧」两个标记、且默认勾选
            id: "backup:trae-cn:20261010_194737",
            kind: "batch",
            category: "tool_residue",
            title: "删除会话前的整库备份 · trae-cn · 2026/10/10 19:47",
            detail:
              "~/.twin-switch/trae/backup（3 个文件）· 该客户端最旧的一批 · 超出保留策略：超过保留份数（每客户端 3 份）",
            bytes: 396_361_728,
            bytes_mb: 378,
            recommended: true,
            over_policy: true,
            policy_reason: "超过保留份数（每客户端 3 份）",
            oldest: true,
            needs_client_stop: false,
            client_key: "trae-cn",
            client_label: "",
            session_id: "",
            turns: 0,
            updated: "2026/10/10 19:47",
            paths: ["C:/Users/x/.twin-switch/trae/backup/trae-cn_before_delete_20261010_194737.db.bak"],
          },
          {
            id: "backup:trae-cn:20261010_200820",
            kind: "batch",
            category: "tool_residue",
            title: "删除会话前的整库备份 · trae-cn · 2026/10/10 20:08",
            detail: "~/.twin-switch/trae/backup（3 个文件）· 最新一批，保留可回滚",
            bytes: 396_361_728,
            bytes_mb: 378,
            recommended: false,
            over_policy: false,
            policy_reason: "",
            oldest: false,
            needs_client_stop: false,
            client_key: "trae-cn",
            client_label: "",
            session_id: "",
            turns: 0,
            updated: "2026/10/10 20:08",
            paths: ["C:/Users/x/.twin-switch/trae/backup/trae-cn_before_delete_20261010_200820.db.bak"],
          },
          {
            id: "file:edit_tmp",
            kind: "dir",
            category: "tool_residue",
            title: "编辑回写临时目录",
            detail: "改回合中断留下的中间文件",
            bytes: 6_291_456,
            bytes_mb: 6,
            recommended: true,
            oldest: false,
            needs_client_stop: false,
            client_key: "",
            client_label: "",
            session_id: "",
            turns: 0,
            updated: "",
            paths: ["C:/Users/x/.twin-switch/trae/edit_tmp"],
          },
        ],
      },
      {
        id: "client_residue",
        title: "客户端残留（Trae 自身缓存）",
        desc: "Chromium/Electron 缓存、日志与崩溃转储。不碰登录态与偏好设置；清理前请关闭客户端。",
        count: 2,
        recommended_count: 1,
        bytes: 971_243_520,
        bytes_mb: 926,
        items: [
          {
            // ★ T61：影子客户端（已并入、本机已无自己程序）**折成一条**。
            //   桩里必须只有这一条 —— 要是像真机那样拆出 17 行，探针就抓不到「又拆回去了」。
            id: "client_residue:merged-shadow:solo-cn",
            kind: "dir",
            category: "client_residue",
            title: "TRAE SOLO CN（已并入 Trae CN（国内版））· 历史缓存",
            detail:
              "本机已没有它自己的程序，只剩合并前留下的 17 项缓存与日志。删除它们不会影响 Trae CN（国内版），也不会结束任何进程（均可再生，登录态与偏好设置不受影响）。",
            bytes: 39_845_888,
            bytes_mb: 38,
            recommended: true,
            needs_client_stop: false,
            merged_shadow: true,
            merged_paths: 17,
            client_key: "solo-cn",
            client_label: "",
            session_id: "",
            turns: 0,
            updated: "",
            paths: ["C:/Users/x/AppData/Roaming/TRAE SOLO CN/Cache"],
          },
          {
            id: "client_residue:C:/Users/x/AppData/Roaming/Trae CN/Cache",
            kind: "dir",
            category: "client_residue",
            title: "Trae CN（国内版）· Cache",
            detail: "HTTP 磁盘缓存（可再生，不影响登录态）",
            bytes: 931_397_632,
            bytes_mb: 888,
            recommended: false,
            needs_client_stop: true,
            client_key: "trae-cn",
            client_label: "",
            session_id: "",
            turns: 0,
            updated: "",
            paths: ["C:/Users/x/AppData/Roaming/Trae CN/Cache"],
          },
        ],
      },
    ],
  },
};

fs.writeFileSync(OUT, JSON.stringify(data), "utf8");
console.log("data.json written", fs.statSync(OUT).size, "bytes; lines =", lines.lines.length);
