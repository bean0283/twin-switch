// 渲染自查驱动：headless Edge + CDP。
//
// 每个尺寸**单独开一个 Edge**（旧进程没退干净会静默绑定失败），端口每次随机。
// 判据见 measure()：横向溢出 / 竖排文字 / 页脚与拖拽手柄重叠 / console 报错。
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";

const EDGE = "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe";
const BASE = process.env.RC_BASE || "http://127.0.0.1:1420";
const HERE = path.dirname(decodeURIComponent(new URL(import.meta.url).pathname.replace(/^\//, "")));
const OUTDIR = path.join(HERE, "out");

const stub = fs.readFileSync(path.join(HERE, "stub.js"), "utf8");
const data = fs.readFileSync(path.join(HERE, "data.json"), "utf8");

const CLICK_EDIT = `(() => {
  const btns = [...document.querySelectorAll('button')];
  const b = btns.find(x => (x.getAttribute('title') || '').startsWith('编辑这条会话'))
         || btns.find(x => (x.getAttribute('title') || '').startsWith('编辑这条 Trae 会话'));
  if (!b) return 'no-edit-button';
  b.click();
  return 'clicked';
})()`;

const MEASURE = `(() => {
  const de = document.documentElement;
  const overflowX = de.scrollWidth - window.innerWidth;
  const vertical = [];
  for (const el of document.querySelectorAll('body *')) {
    const cs = getComputedStyle(el);
    if (cs.display === 'none' || cs.visibility === 'hidden' || cs.opacity === '0') continue;
    if (el.children.length !== 0) continue;
    const txt = (el.textContent || '').trim();
    if (txt.length < 4) continue;
    const r = el.getBoundingClientRect();
    if (r.width <= 0 || r.height <= 0) continue;
    if (r.width <= 22 && r.height >= 44) {
      vertical.push({ text: txt.slice(0, 18), w: Math.round(r.width), h: Math.round(r.height) });
    }
  }
  // 页脚 / 手柄重叠
  const handle = document.querySelector('[data-slot="dialog-resize-handle"]');
  const dialog = document.querySelector('[data-slot="dialog-content"]');
  let footerHit = false;
  let dialogRect = null;
  if (handle && dialog) {
    const h = handle.getBoundingClientRect();
    dialogRect = { w: Math.round(dialog.getBoundingClientRect().width), h: Math.round(dialog.getBoundingClientRect().height) };
    const last = dialog.lastElementChild;
    if (last) {
      const l = last.getBoundingClientRect();
      footerHit = l.right > h.left && l.bottom > h.top;
    }
  }
  const dialogTexts = dialog ? dialog.querySelectorAll('*').length : 0;
  return {
    url: location.pathname,
    innerW: window.innerWidth,
    overflowX,
    vertical: vertical.slice(0, 6),
    verticalCount: vertical.length,
    dialog: dialogRect,
    footerHitHandle: footerHit,
    dialogNodes: dialogTexts,
    hasDialog: !!dialog,
    title: (document.querySelector('[data-slot="dialog-title"]') || {}).textContent || null,
    bodyText: (document.body.innerText || '').slice(0, 400),
    // 页脚按钮集合（判「关闭编辑」有没有真的消失）
    footerButtons: [...document.querySelectorAll('[data-slot="dialog-content"] button')]
      .map((b) => (b.textContent || '').trim())
      .filter(Boolean)
      .slice(0, 20),
    hasCloseEditText: (document.body.innerText || '').includes('关闭编辑'),
    pencilButtons: [...document.querySelectorAll('button')]
      .filter((b) => (b.getAttribute('title') || '').startsWith('编辑这条'))
      .map((b) => b.getAttribute('title')),
    // 弹窗页脚：文字 + 全部 combobox 的可见值。
    // ⚠️ 跨客户端改造的判据就在这里 —— 「写入到账号」那个下拉到底列的是哪一端的账号。
    footerText: (() => {
      const f = document.querySelector('[data-slot="dialog-content"]');
      if (!f) return null;
      const bars = [...f.querySelectorAll('div.border-t')];
      const last = bars[bars.length - 1];
      return last ? (last.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 220) : null;
    })(),
    comboValues: [...document.querySelectorAll('[data-slot="dialog-content"] button[role="combobox"]')]
      .map((b) => (b.textContent || '').trim()),
    // 展开中的下拉选项（Radix 只在打开时渲染 SelectContent ⇒ 必须先真点开）
    options: [...document.querySelectorAll('[role="option"]')].map((o) =>
      (o.textContent || '').replace(/\\s+/g, ' ').trim(),
    ),
    // 回合级删除：左栏每个回合行各一个（**常显**，不再 hover 才出现）
    turnDelBtns: [...document.querySelectorAll('button')].filter(
      (b) => (b.getAttribute('title') || '') === '删除这个回合（写回前可撤销）',
    ).length,
    // 判「一直显示」：无 hover 时第一个删除按钮的 computed opacity 必须是 1（旧实现是 0）
    turnDelOpacity: (() => {
      const b = [...document.querySelectorAll('button')].find(
        (x) => (x.getAttribute('title') || '') === '删除这个回合（写回前可撤销）',
      );
      return b ? getComputedStyle(b).opacity : null;
    })(),
    // 多选删除的入口 + 勾选中的个数（勾选指示是回合按钮内部的纯展示 span）
    hasMultiToggle: [...document.querySelectorAll('button')].some(
      (b) => (b.textContent || '').trim() === '多选删除',
    ),
    checkedTurns: document.querySelectorAll('[data-turn-check="1"]').length,
    turnCheckBoxes: document.querySelectorAll('[data-turn-check]').length,
    multiBarText: (() => {
      const b = [...document.querySelectorAll('button')].find((x) =>
        (x.textContent || '').trim().startsWith('删除选中（'),
      );
      return b ? (b.textContent || '').trim() : null;
    })(),
    hasDeleteTurnText: (document.body.innerText || '').includes('删除本回合'),
    // 删除确认框（Radix AlertDialog 的 role 是 alertdialog）
    alertText: (() => {
      const a = document.querySelector('[role="alertdialog"]');
      return a ? (a.innerText || '').replace(/\\s+/g, ' ').trim().slice(0, 260) : null;
    })(),
    // ★ 本轮判据：确认框里有没有**内容超出卡片右边界**。
    //     老实现：回合提问用 truncate（nowrap）⇒ grid 的 auto 列被撑到卡片外。
    alertOverflow: (() => {
      const a = document.querySelector('[role="alertdialog"]');
      if (!a) return null;
      const ar = a.getBoundingClientRect();
      let worst = 0;
      let sample = null;
      for (const el of a.querySelectorAll('*')) {
        const r = el.getBoundingClientRect();
        if (r.width <= 0 || r.height <= 0) continue;
        const over = Math.round(r.right - ar.right);
        if (over > worst) {
          worst = over;
          sample = (el.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 40);
        }
      }
      return { dialogRight: Math.round(ar.right), worstOverflow: worst, sample };
    })(),
    // ★ 本轮判据：跨客户端页脚的「客户端」pill（只有存在多个客户端时才会渲染）
    crossPills: [...document.querySelectorAll('[data-cross-client]')].map((b) => ({
      key: b.dataset.crossClient,
      active: b.getAttribute('data-cross-client-active') === '1',
      text: (b.textContent || '').replace(/\\s+/g, ' ').trim(),
    })),
    crossAccountValue: (() => {
      const b = document.querySelector('[data-cross-account]');
      return b ? (b.textContent || '').replace(/\\s+/g, ' ').trim() : null;
    })(),
  };
})()`;

/** 点左栏第 n 个回合的删除按钮（打开确认框）。不传 = 第一个。 */
const clickTurnDel = (n = 0) => `(() => {
  const bs = [...document.querySelectorAll('button')].filter(
    (x) => (x.getAttribute('title') || '') === '删除这个回合（写回前可撤销）',
  );
  const b = bs[${n}];
  if (!b) return 'no-turn-del-button';
  b.click();
  return 'clicked';
})()`;

/** 真指针点击一个矩形中心（Radix 的 Select / pill 只认真实指针事件）。 */
async function pointerClick(cdp, p) {
  if (!p) return false;
  await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: p.x, y: p.y, button: "none" });
  await cdp.send("Input.dispatchMouseEvent", { type: "mousePressed", x: p.x, y: p.y, button: "left", clickCount: 1 });
  await cdp.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: p.x, y: p.y, button: "left", clickCount: 1 });
  return true;
}

/** 取第 n 个匹配元素的中心点。 */
const RECT_OF = (sel, n = 0) => `(() => {
  const el = document.querySelectorAll('${sel}')[${n}];
  if (!el) return null;
  const r = el.getBoundingClientRect();
  return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
})()`;

/** 真指针事件点开第一个下拉（Radix 只认 pointerdown，`.click()` 打不开）。 */
const CLICK_SELECT = `(() => {
  const b = document.querySelector('[data-slot="dialog-content"] button[role="combobox"]');
  if (!b) return null;
  const r = b.getBoundingClientRect();
  return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
})()`;

/** 进入多选态。 */
const CLICK_MULTI_TOGGLE = `(() => {
  const b = [...document.querySelectorAll('button')].find(
    (x) => (x.textContent || '').trim() === '多选删除',
  );
  if (!b) return 'no-multi-button';
  b.click();
  return 'on';
})()`;

/**
 * 点左栏第 n 个回合行（多选态下点整行 = 切换勾选）。
 * ⚠️ 不能在同一个 evaluate 里连点两次：React 的批处理不会在同步代码里 flush，
 * 第二次点击的 handler 还是上一次渲染的闭包 ⇒ 必须拆成多次 evaluate + sleep。
 */
const clickTurnRow = (n) => `(() => {
  const rows = [...document.querySelectorAll('[data-slot="dialog-content"] button')].filter(
    (b) => /^#\\d+/.test((b.textContent || '').trim()),
  );
  if (rows.length <= ${n}) return 'only-' + rows.length;
  rows[${n}].click();
  return 'picked-' + (rows.length);
})()`;

/** 点「删除选中（N）」。 */
const CLICK_MULTI_DEL = `(() => {
  const b = [...document.querySelectorAll('button')].find((x) =>
    (x.textContent || '').trim().startsWith('删除选中（'),
  );
  if (!b) return 'no-delete-selected';
  b.click();
  return 'clicked';
})()`;

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

async function http(url) {
  const res = await fetch(url, { proxy: undefined });
  return res.json();
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.events = [];
    ws.addEventListener("message", (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) {
        const { resolve, reject } = this.pending.get(m.id);
        this.pending.delete(m.id);
        if (m.error) reject(new Error(JSON.stringify(m.error)));
        else resolve(m.result);
      } else if (m.method) {
        this.events.push(m);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`CDP timeout: ${method}`));
        }
      }, 30000);
    });
  }
  errors() {
    const out = [];
    for (const e of this.events) {
      if (e.method === "Runtime.exceptionThrown") {
        out.push(`exception: ${e.params?.exceptionDetails?.exception?.description || e.params?.exceptionDetails?.text}`);
      }
      if (e.method === "Runtime.consoleAPICalled" && e.params?.type === "error") {
        out.push(`console.error: ${(e.params.args || []).map((a) => a.value ?? a.description ?? a.type).join(" ")}`);
      }
      if (e.method === "Log.entryAdded" && e.params?.entry?.level === "error") {
        out.push(`log: ${e.params.entry.text}`);
      }
    }
    return out;
  }
}

async function runCase(c) {
  const port = 9000 + Math.floor(Math.random() * 1500);
  const profile = path.join(os.tmpdir(), `rc-edge-${Date.now()}-${port}`);
  const args = [
    "--headless=new",
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profile}`,
    "--no-proxy-server",
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-gpu",
    "--disable-extensions",
    "--force-device-scale-factor=1",
    `--window-size=${c.width},${c.height}`,
    "about:blank",
  ];
  const child = spawn(EDGE, args, { stdio: "ignore" });
  let cdp = null;
  const result = { case: c.name, width: c.width, height: c.height, route: c.route };
  try {
    let targets = null;
    for (let i = 0; i < 60; i += 1) {
      try {
        const list = await http(`http://127.0.0.1:${port}/json/list`);
        const page = list.find((t) => t.type === "page");
        if (page && page.webSocketDebuggerUrl) {
          targets = page;
          break;
        }
      } catch {
        /* 还没起来 */
      }
      await sleep(250);
    }
    if (!targets) throw new Error("CDP 页面目标没起来");

    const ws = new WebSocket(targets.webSocketDebuggerUrl);
    await new Promise((res, rej) => {
      ws.addEventListener("open", res, { once: true });
      ws.addEventListener("error", rej, { once: true });
      setTimeout(() => rej(new Error("WS 连接超时")), 15000);
    });
    cdp = new Cdp(ws);
    await cdp.send("Page.enable");
    await cdp.send("Runtime.enable");
    await cdp.send("Log.enable");
    await cdp.send("Emulation.setDeviceMetricsOverride", {
      width: c.width,
      height: c.height,
      deviceScaleFactor: 1,
      mobile: false,
    });
    const source = `window.__RC_DATA__ = ${data};\n${stub}`;
    await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source });
    await cdp.send("Page.navigate", { url: `${BASE}${c.route}` });
    await sleep(c.wait ?? 3000);

    const first = await cdp.send("Runtime.evaluate", { expression: MEASURE, returnByValue: true });
    result.before = first.result.value;

    if (c.click) {
      const cl = await cdp.send("Runtime.evaluate", { expression: CLICK_EDIT, returnByValue: true });
      result.click = cl.result.value;
      await sleep(c.waitAfterClick ?? 2500);
      const second = await cdp.send("Runtime.evaluate", { expression: MEASURE, returnByValue: true });
      result.after = second.result.value;
    }

    if (c.delTurn) {
      // `delTurn: 1` = 点第 2 个回合的 🗑（自查里那一条带超长附件名的提问）。
      const n = typeof c.delTurn === "number" ? c.delTurn : 0;
      const d = await cdp.send("Runtime.evaluate", { expression: clickTurnDel(n), returnByValue: true });
      result.turnDel = d.result.value;
      await sleep(900);
      const third = await cdp.send("Runtime.evaluate", { expression: MEASURE, returnByValue: true });
      result.afterTurnDel = third.result.value;
    }

    if (c.openSelect) {
      const box = await cdp.send("Runtime.evaluate", { expression: CLICK_SELECT, returnByValue: true });
      const p = box.result.value;
      if (p) {
        await cdp.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: p.x, y: p.y, button: "none" });
        await cdp.send("Input.dispatchMouseEvent", {
          type: "mousePressed", x: p.x, y: p.y, button: "left", clickCount: 1,
        });
        await cdp.send("Input.dispatchMouseEvent", {
          type: "mouseReleased", x: p.x, y: p.y, button: "left", clickCount: 1,
        });
        await sleep(700);
      }
      const opened = await cdp.send("Runtime.evaluate", { expression: MEASURE, returnByValue: true });
      result.opened = opened.result.value;
    }

    if (c.multi) {
      // 进入多选 → 勾两个回合 → 点「删除选中」（应弹确认框）。
      const steps = [];
      const ev = async (expr) => {
        const r = await cdp.send("Runtime.evaluate", { expression: expr, returnByValue: true });
        return r.result.value;
      };
      steps.push(["toggle", await ev(CLICK_MULTI_TOGGLE)]);
      await sleep(700);
      steps.push(["row0", await ev(clickTurnRow(0))]);
      await sleep(350);
      steps.push(["row1", await ev(clickTurnRow(1))]);
      await sleep(350);
      const mid = await cdp.send("Runtime.evaluate", { expression: MEASURE, returnByValue: true });
      result.afterMulti = mid.result.value;
      steps.push(["delSel", await ev(CLICK_MULTI_DEL)]);
      await sleep(900);
      const fin = await cdp.send("Runtime.evaluate", { expression: MEASURE, returnByValue: true });
      result.afterMultiDel = fin.result.value;
      result.multiSteps = steps;
    }

    if (c.cross) {
      // ★ 本轮重点：Trae CN 与 TRAE SOLO CN 必须**分开选**，账号下拉里只列当前客户端的账号。
      const ev = async (expr) => {
        const r = await cdp.send("Runtime.evaluate", { expression: expr, returnByValue: true });
        return r.result.value;
      };
      result.crossPills = await ev(MEASURE + ".crossPills");
      result.crossDefaultValue = await ev(MEASURE + ".crossAccountValue");
      // 切到第二个客户端，再看账号下拉里到底列了谁的账号
      const pill2 = await ev(RECT_OF("[data-cross-client]", 1));
      if (await pointerClick(cdp, pill2)) {
        await sleep(600);
        result.crossAfterSwitchValue = await ev(MEASURE + ".crossAccountValue");
        result.crossAfterSwitchPills = await ev(MEASURE + ".crossPills");
        const box = await ev(RECT_OF("[data-cross-account]"));
        await pointerClick(cdp, box);
        await sleep(800);
        result.crossOptions = await ev(
          `[...document.querySelectorAll('[role="option"]')].map((o) => (o.textContent || '').replace(/\\s+/g, ' ').trim())`,
        );
        await cdp.send("Input.dispatchKeyEvent", {
          type: "keyDown",
          key: "Escape",
          code: "Escape",
          windowsVirtualKeyCode: 27,
        });
        await sleep(300);
        const m = await cdp.send("Runtime.evaluate", { expression: MEASURE, returnByValue: true });
        result.afterCross = m.result.value;
      }
    }

    const shot = await cdp.send("Page.captureScreenshot", { format: "png" });
    fs.mkdirSync(OUTDIR, { recursive: true });
    const file = path.join(OUTDIR, `${c.name}.png`);
    fs.writeFileSync(file, Buffer.from(shot.data, "base64"));
    result.screenshot = file;
    result.errors = cdp.errors();
  } catch (e) {
    result.fatal = String(e && e.message ? e.message : e);
  } finally {
    try {
      cdp?.ws.close();
    } catch {
      /* ignore */
    }
    child.kill();
    await sleep(400);
    try {
      fs.rmSync(profile, { recursive: true, force: true });
    } catch {
      /* ignore */
    }
  }
  return result;
}

const CASES = [
  { name: "wb-records-1600", route: "/workbuddy-records", width: 1600, height: 900 },
  { name: "wb-records-1360", route: "/workbuddy-records", width: 1360, height: 800 },
  { name: "wb-records-730", route: "/workbuddy-records", width: 730, height: 800 },
  { name: "wb-editor-1600", route: "/workbuddy-records", width: 1600, height: 900, click: true },
  { name: "wb-editor-1360", route: "/workbuddy-records", width: 1360, height: 800, click: true },
  { name: "wb-editor-730", route: "/workbuddy-records", width: 730, height: 800, click: true },
  // ★ 本轮新增：左栏回合行上的「删除这个回合」按钮 + 点开后要弹确认框。
  { name: "wb-editor-turndel-1600", route: "/workbuddy-records", width: 1600, height: 900, click: true, delTurn: true },
  { name: "wb-editor-turndel-1360", route: "/workbuddy-records", width: 1360, height: 800, click: true, delTurn: true },
  { name: "wb-editor-turndel-730", route: "/workbuddy-records", width: 730, height: 800, click: true, delTurn: true },
  // ★ 本轮新增：第 2 个回合的提问是**一长串附件名**（没有任何空格/换行）——
  //    确认框里它就是那条会把卡片横向撑破的行，专门留一档回归。
  { name: "wb-editor-turndel2-1600", route: "/workbuddy-records", width: 1600, height: 900, click: true, delTurn: 1 },
  { name: "wb-editor-turndel2-1360", route: "/workbuddy-records", width: 1360, height: 800, click: true, delTurn: 1 },
  { name: "wb-editor-turndel2-730", route: "/workbuddy-records", width: 730, height: 800, click: true, delTurn: 1 },
  // ★ 本轮新增：批量删除回合（多选 → 删除选中）。
  { name: "wb-editor-multi-1600", route: "/workbuddy-records", width: 1600, height: 900, click: true, multi: true },
  { name: "wb-editor-multi-1360", route: "/workbuddy-records", width: 1360, height: 800, click: true, multi: true },
  { name: "wb-editor-multi-730", route: "/workbuddy-records", width: 730, height: 800, click: true, multi: true },
  { name: "wb-import-1360", route: "/workbuddy-import", width: 1360, height: 800 },
  { name: "wb-import-730", route: "/workbuddy-import", width: 730, height: 800 },
  { name: "wb-export-1360", route: "/workbuddy-export", width: 1360, height: 800 },
  { name: "wb-export-730", route: "/workbuddy-export", width: 730, height: 800 },
  // ★ 本轮重点：两个「数据迁移」页里的编辑器，页脚必须是**跨客户端**写入。
  { name: "wb-import-editor-1600", route: "/workbuddy-import", width: 1600, height: 900, click: true, cross: true },
  { name: "wb-import-editor-1360", route: "/workbuddy-import", width: 1360, height: 800, click: true },
  { name: "wb-import-editor-730", route: "/workbuddy-import", width: 730, height: 800, click: true },
  { name: "wb-export-editor-1600", route: "/workbuddy-export", width: 1600, height: 900, click: true, cross: true },
  { name: "wb-export-editor-1360", route: "/workbuddy-export", width: 1360, height: 800, click: true },
  { name: "wb-export-editor-730", route: "/workbuddy-export", width: 730, height: 800, click: true },
  // Trae 侧回归：编辑器的页脚少了「关闭编辑」按钮，确认整条页脚还站得住。
  { name: "trae-records-1600", route: "/trae-records", width: 1600, height: 900 },
  { name: "trae-records-730", route: "/trae-records", width: 730, height: 800 },
  { name: "trae-editor-1360", route: "/trae-records", width: 1360, height: 800, click: true },
  { name: "trae-editor-730", route: "/trae-records", width: 730, height: 800, click: true },
  // ★ 本轮新增：Trae 侧的批量删回合（多选 → 勾两个回合 → 删除选中 → 确认框）。
  { name: "trae-editor-multi-1600", route: "/trae-records", width: 1600, height: 900, click: true, multi: true },
  { name: "trae-editor-multi-1360", route: "/trae-records", width: 1360, height: 800, click: true, multi: true },
  { name: "trae-editor-multi-730", route: "/trae-records", width: 730, height: 800, click: true, multi: true },
  // Trae 侧单回合 🗑（同一条第 2 个回合）—— 与批量走同一条路，顺带验确认框不撑破卡片。
  { name: "trae-editor-turndel-1360", route: "/trae-records", width: 1360, height: 800, click: true, delTurn: 1 },
];

const only = process.argv[2];
const list = only ? CASES.filter((c) => c.name.includes(only)) : CASES;
const out = [];
for (const c of list) {
  const r = await runCase(c);
  out.push(r);
  const b = r.before || {};
  const a = r.after || {};
  console.log(
    `\n[${r.case}] ${r.width}x${r.height} ${r.route}` +
      (r.fatal ? `\n  ❌ FATAL ${r.fatal}` : "") +
      `\n  before: overflowX=${b.overflowX} 竖排=${b.verticalCount} dialog=${b.hasDialog}` +
      (r.click ? `\n  click: ${r.click}` : "") +
      (r.after
        ? `\n  after : overflowX=${a.overflowX} 竖排=${a.verticalCount} 手柄重叠=${a.footerHitHandle} 标题=${JSON.stringify(a.title)} 节点=${a.dialogNodes}` +
          `\n  footer: ${JSON.stringify(a.footerText)}` +
          `\n  combo : ${JSON.stringify(a.comboValues)}` +
          `\n  closeEditText=${a.hasCloseEditText}`
        : "") +
      (r.opened ? `\n  options: ${JSON.stringify(r.opened.options)}` : "") +
      (r.turnDel
        ? `\n  turnDel: ${r.turnDel} 左栏回合删除按钮=${a.turnDelBtns} 常显opacity=${a.turnDelOpacity} 有「删除本回合」=${a.hasDeleteTurnText}` +
          `\n  alert : ${JSON.stringify(r.afterTurnDel?.alertText)}` +
          `\n  alertOverflow: ${JSON.stringify(r.afterTurnDel?.alertOverflow)}` +
          `\n  deld  : overflowX=${r.afterTurnDel?.overflowX} 竖排=${r.afterTurnDel?.verticalCount}`
        : "") +
      (r.multiSteps
        ? `\n  multi : ${r.multiSteps.map((s) => `${s[0]}=${s[1]}`).join(" ")}` +
          `\n  multi : 勾选框=${r.afterMulti?.turnCheckBoxes} 已勾=${r.afterMulti?.checkedTurns} 按钮=${JSON.stringify(r.afterMulti?.multiBarText)}` +
          `\n  multi : overflowX=${r.afterMulti?.overflowX} 竖排=${r.afterMulti?.verticalCount} 删除按钮透明=${r.afterMulti?.turnDelOpacity}` +
          `\n  alert : ${JSON.stringify(r.afterMultiDel?.alertText)}` +
          `\n  alertOverflow: ${JSON.stringify(r.afterMultiDel?.alertOverflow)}` +
          `\n  deld  : overflowX=${r.afterMultiDel?.overflowX} 竖排=${r.afterMultiDel?.verticalCount}`
        : "") +
      (r.crossPills
        ? `\n  cross : pills=${JSON.stringify(r.crossPills)}` +
          `\n  cross : 默认=${JSON.stringify(r.crossDefaultValue)} 切客户2后=${JSON.stringify(r.crossAfterSwitchValue)}` +
          `\n  cross : 切后pills=${JSON.stringify(r.crossAfterSwitchPills)}` +
          `\n  cross : 下拉选项=${JSON.stringify(r.crossOptions)}` +
          `\n  cross : overflowX=${r.afterCross?.overflowX} 竖排=${r.afterCross?.verticalCount}`
        : "") +
      `\n  errors(${(r.errors || []).length}): ${(r.errors || []).slice(0, 4).join(" | ")}`,
  );
  if (b.vertical?.length) console.log("  before 竖排样本:", JSON.stringify(b.vertical));
  if (a.vertical?.length) console.log("  after 竖排样本:", JSON.stringify(a.vertical));
  if (b.bodyText && (r.click && r.click !== "clicked")) {
    console.log("  body:", JSON.stringify(b.bodyText.slice(0, 300)));
  }
  if (r.fatal && b.bodyText) console.log("  body:", JSON.stringify(b.bodyText.slice(0, 200)));
}

fs.writeFileSync(path.join(OUTDIR, "report.json"), JSON.stringify(out, null, 2), "utf8");
console.log(`\n报告：${path.join(OUTDIR, "report.json")}`);
