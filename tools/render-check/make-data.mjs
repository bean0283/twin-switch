// 把真机探针产出的 doc / lines 包成渲染自查用的桩数据。
import fs from "node:fs";

const DOC = "D:/htw/签到/.workbuddy/t46-wb-doc.json";
const LINES = `${DOC}.lines.json`;
const OUT = "D:/htw/签到/.render-check/data.json";

const doc = JSON.parse(fs.readFileSync(DOC, "utf8"));
const lines = JSON.parse(fs.readFileSync(LINES, "utf8"));
const traeDoc = JSON.parse(fs.readFileSync("D:/htw/签到/.render-check/trae-doc.json", "utf8"));

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
      {
        key: "solo-cn",
        label: "Trae SOLO CN",
        user_data_dir: "C:/x/Trae SOLO CN",
        installed: true,
        exe: "C:/x/Trae SOLO CN/Trae.exe",
        has_login: true,
      },
    ],
    usage: { topPick: "solo-cn", switchWeight: 2, preference: ["solo-cn"], clients: [] },
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
      id: traeDoc.sessionId,
      title: traeDoc.title || "（无标题）",
      created: "2026-10-10 20:00:00",
      updated: "2026-10-10 22:00:00",
      turns: 41,
      messages: 82,
      owner_uid: traeDoc.ownerUid || "",
      owner_label: "测试账号（uid …667777）",
    },
  ],
};

fs.writeFileSync(OUT, JSON.stringify(data), "utf8");
console.log("data.json written", fs.statSync(OUT).size, "bytes; lines =", lines.lines.length);
