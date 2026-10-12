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

// ★ 本轮新增判据：Trae 账号页顶部的「已并入 → 一并带入」提示条。
//
// ⚠️ 判据必须抓得住「老问题」：SOLO 并入 Trae CN 之后，账号页**根本没有这条提示**
// —— 用户只看到切号失败，不知道另外几个账号还留在 SOLO 的账号库里。所以一旦找不到
// 按钮就返回 `found: false`，回退验证时必须是这个结果（否则判据是空的）。
//
// ⚠️⚠️ 它是 **async 且自己轮询**（配合 `awaitPromise: true`）：页面什么时候「数据到位」
// 由页面自己说了算，比在外层猜一个 sleep 可靠得多 —— 本轮就是在外层猜时长上连栽三次
// （冷启动拿到占位页、`<script>` 源码被当正文、挂载了但还在 `<Skeleton>`）。
const MERGE_PROBE = `(async () => {
  const find = () => [...document.querySelectorAll('button')]
    .find((x) => (x.textContent || '').includes('一并带入'));
  const deadline = Date.now() + 15000;
  let btn = find();
  while (!btn && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 200));
    btn = find();
  }
  if (!btn) {
    // 找不到时带上**诊断**：光一个 found=false 分不清「页面没就绪」和「真没这条」。
    return {
      found: false,
      alerts: document.querySelectorAll('[data-slot="alert"]').length,
      skeletons: document.querySelectorAll('[data-slot="skeleton"]').length,
      buttons: [...document.querySelectorAll('button')].length,
      text: document.body.textContent.replace(/\\s+/g, ' ').trim().slice(0, 260),
    };
  }
  const alert = btn.closest('[data-slot="alert"]');
  const r = btn.getBoundingClientRect();
  const a = alert ? alert.getBoundingClientRect() : null;
  return {
    found: true,
    btn: { w: Math.round(r.width), h: Math.round(r.height), right: Math.round(r.right) },
    banner: a ? { w: Math.round(a.width), right: Math.round(a.right) } : null,
    viewportW: window.innerWidth,
    text: (alert ? alert.textContent || '' : '').replace(/\\s+/g, ' ').trim().slice(0, 140),
  };
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

/**
 * 等页面**就绪**（挂载 **且** 数据到位），而不是睡固定时长。
 *
 * ⚠️ 为什么必须有它：`vite dev` 首次 transform 一个页面要几秒（服务端缓存热了之后就快了），
 * 首档永远最慢。固定 `sleep(3000)` 只会拿到 `index.html` 的「正在启动…」占位，
 * probe 便报「提示条没渲染」——**是工具的时序问题，不是页面问题**。
 *
 * ⚠️⚠️ 判据有两道，缺一不可：
 *
 * ① **挂载**不能用 `document.body.textContent.length` —— `<script>` 标签的内容也算
 *    `textContent`，而 `index.html` 里那段自救脚本的源码就有几百字符，于是「React 还没挂载」
 *    会被判成「已挂载」（实测：正文里赫然带着 `var timeoutMs = 10000; window.setTimeout(...)`）。
 *    改用 `#root` 有子元素（**不用** `window.__WB_MOUNTED__`，见 ②）。
 * ② **挂载 ≠ 就绪**：挂载之后页面才开始拉数据，此时渲染的是 `<Skeleton>` 占位。
 *    只看挂载会拿到「还在加载」的中间态，probe 照样找不到东西
 *    （实测：正文已经完整、`TwinSwitch v0.2.24` 都出来了，但 `skeleton=2` ⇒ 提示条还没渲染）。
 *    所以还要等 `data-slot="skeleton"` 归零。
 *
 * ⚠️⚠️ `index.html` 里有个 **10 秒看门狗**：超时未置位 `window.__WB_MOUNTED__` 就把占位
 * 换成「应用启动失败」，之后 React **再也挂不上**（实测首档就栽在这上面）。
 * 所以注入脚本会提前把它置位（见 runCase 里的 `source`）—— 相应地，这里**不能**拿它当判据。
 * ③ **失败判据更不能用 `document.body.textContent`**（第 331 行那个坑的翻版，本轮又踩了一次）：
 *    看门狗那段脚本就在 `<body>` 里，它**自己的源码**写着 `title.textContent = "应用启动失败"`
 *    —— 于是 `body.textContent.includes('应用启动失败')` **从第一毫秒起就恒为真**，
 *    这个函数一进门就 `return false`，**整个驱动其实从未等待过任何一档**。
 *    实测症状：三档 `mounted` 全是 false，但其中一档的 probe 明明已经找到了目标元素。
 *    ⇒ 判据必须看 **DOM 结构**（占位块 `.wb-boot` 还在不在、它的标题写了什么），不看文本。
 * ④ **就绪 = 占位块没了**：`#root` 里那个 `.wb-boot` 是 `index.html` 的内联占位，React
 *    挂载时会把它清掉。所以「占位块消失 + 骨架归零」才是真的画完了。
 */
async function waitForMount(cdp, timeoutMs = 45000) {
  const expr = `(() => {
    const root = document.getElementById('root');
    const boot = root ? root.querySelector('.wb-boot') : null;
    const bootTitle = root ? root.querySelector('.wb-boot-title') : null;
    return {
      kids: root ? root.childElementCount : 0,
      skeletons: document.querySelectorAll('[data-slot="skeleton"]').length,
      boot: !!boot,
      failed: !!bootTitle && (bootTitle.textContent || '').includes('应用启动失败'),
    };
  })()`;
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    const r = await cdp.send("Runtime.evaluate", { expression: expr, returnByValue: true });
    const v = r.result.value || {};
    // 看门狗已经点火（占位标题被换成「应用启动失败」）⇒ 这个页面废了，早点退出别白等。
    if (v.failed) return false;
    if (v.kids > 0 && !v.boot && v.skeletons === 0) return true;
    await sleep(200);
  }
  return false;
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
    // ⚠️ 提前置位 `__WB_MOUNTED__`：`index.html` 里有个 10 秒看门狗，超时未置位就把占位
    //    换成「应用启动失败」—— 而 `vite dev` 首次 transform 一整个页面常常超过 10 秒，
    //    于是**首档永远看到失败页、React 再也挂不上**（实测踩到）。自查环境里先骗过看门狗，
    //    真正的「就绪」由 `waitForMount()` 判（`#root` 有子元素 + 没有 `<Skeleton>`）。
    const source = `window.__RC_DATA__ = ${data};\nwindow.__WB_MOUNTED__ = true;\n${stub}`;
    await cdp.send("Page.addScriptToEvaluateOnNewDocument", { source });
    await cdp.send("Page.navigate", { url: `${BASE}${c.route}` });
    await sleep(c.wait ?? 1500);
    // 等就绪（见 `waitForMount` 的四条注释）。⚠️ 不能只看「睡够了几秒」。
    result.mounted = await waitForMount(cdp);
    if (!result.mounted) {
      // ⚠️ 冷启动兜底：`vite dev` 第一次 transform 整页模块图常常要十几秒，这段时间页面
      //    一直停在 `.wb-boot` 占位上。**再导航一次**就命中已经热好的模块图（实测：不补
      //    这一下，每轮的第一档必栽、后面几档全过 —— 那纯粹是自查工具的冷启动，不是页面问题）。
      //    ⚠️ 只补**一次**：真失败（页面自身报错）时第二次照样过不了，不会把失败刷成通过。
      result.remounted = true;
      await cdp.send("Page.navigate", { url: `${BASE}${c.route}` });
      await sleep(1500);
      result.mounted = await waitForMount(cdp);
    }

    const first = await cdp.send("Runtime.evaluate", { expression: MEASURE, returnByValue: true });
    result.before = first.result.value;

    // 通用自定义探针：`probe` 是一段 JS 表达式，结果原样记进 `result.probe`。
    // `awaitPromise`：探针可以自己轮询到页面就绪（见 MERGE_PROBE 的注释）。
    if (c.probe) {
      const pr = await cdp.send("Runtime.evaluate", {
        expression: c.probe,
        returnByValue: true,
        awaitPromise: true,
      });
      result.probe = pr.result.value ?? null;
    }

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
  // ★ 本轮重点：Trae **账号页**顶部的「已并入的客户端还有账号没带过来」提示条。
  //
  // 2026-10 官方把 TRAE SOLO 并进 Trae CN 之后，工具自己的账号库不会跟着并
  // ⇒ 用户在 Trae CN 下看不到、也切不过 SOLO 下备份过的账号，只看到「切号失败」。
  // 这条提示就是那个缺口唯一的出口，三档都要站得住。
  { name: "trae-switch-1600", route: "/trae-switch", width: 1600, height: 900, probe: MERGE_PROBE },
  { name: "trae-switch-1360", route: "/trae-switch", width: 1360, height: 800, probe: MERGE_PROBE },
  { name: "trae-switch-730", route: "/trae-switch", width: 730, height: 800, probe: MERGE_PROBE },
];

const only = process.argv[2];
const list = only ? CASES.filter((c) => c.name.includes(only)) : CASES;

// ⚠️ 先热一次 `vite dev`：它第一次 transform 整页模块图要十几秒，这段时间首档只能看到
//    `index.html` 的启动占位 —— 量到的是「工具冷启动」而不是「页面画没画出来」。
//    这里先跑一档丢弃结果的预热（`runCase` 内部还有一次「没就绪就重导航」的兜底）。
{
  const warm = await runCase({ name: "prewarm", route: list[0]?.route || "/", width: 1360, height: 800 });
  console.log(`预热 ${warm.mounted ? "✅ 已就绪" : "⚠️ 未就绪（后续各档仍会各自重试一次）"}`);
}

const out = [];
for (const c of list) {
  const r = await runCase(c);
  out.push(r);
  const b = r.before || {};
  const a = r.after || {};
  console.log(
    `\n[${r.case}] ${r.width}x${r.height} ${r.route}` +
      (r.fatal ? `\n  ❌ FATAL ${r.fatal}` : "") +
      `\n  mounted=${r.mounted}${r.remounted ? "（冷启动重试过一次）" : ""}` +
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
  if (r.probe) {
    console.log(
      r.probe.found
        ? `  merge : found=true 按钮=${JSON.stringify(r.probe.btn)} 提示条=${JSON.stringify(r.probe.banner)} 视口=${r.probe.viewportW}` +
            `\n  merge : 文案=${JSON.stringify(r.probe.text)}`
        : `  merge : found=false alerts=${r.probe.alerts} skeleton=${r.probe.skeletons} buttons=${r.probe.buttons}` +
            `\n  merge : 正文=${JSON.stringify(r.probe.text)}`,
    );
  }
  if (b.vertical?.length) console.log("  before 竖排样本:", JSON.stringify(b.vertical));
  if (a.vertical?.length) console.log("  after 竖排样本:", JSON.stringify(a.vertical));
  if (b.bodyText && (r.click && r.click !== "clicked")) {
    console.log("  body:", JSON.stringify(b.bodyText.slice(0, 300)));
  }
  if (r.fatal && b.bodyText) console.log("  body:", JSON.stringify(b.bodyText.slice(0, 200)));
}

fs.writeFileSync(path.join(OUTDIR, "report.json"), JSON.stringify(out, null, 2), "utf8");

// 汇总判据：本轮新增的「一并带入」提示条必须**三档都渲染出来**，且版式干净。
// 单独给结论，免得要从一屏 before/after 里自己找。
const mergeCases = out.filter((r) => r.case.startsWith("trae-switch-"));
if (mergeCases.length) {
  const missing = mergeCases.filter((r) => !r.probe?.found);
  const messy = mergeCases.filter(
    (r) => (r.before?.overflowX ?? 0) > 0 || (r.before?.verticalCount ?? 0) > 0,
  );
  const errs = mergeCases.filter((r) => (r.errors || []).length > 0);
  const unmounted = mergeCases.filter((r) => r.mounted === false);
  const remounted = mergeCases.filter((r) => r.remounted);
  console.log(
    `\n【判定】trae-switch 提示条 ${mergeCases.length - missing.length}/${mergeCases.length} 档渲染出来` +
      (missing.length ? `  ❌ 未渲染：${missing.map((r) => r.case).join(", ")}` : "  ✅") +
      (messy.length ? `  ❌ 版式问题：${messy.map((r) => r.case).join(", ")}` : "  ✅ 版式干净") +
      (errs.length ? `  ❌ 有 console 报错：${errs.map((r) => r.case).join(", ")}` : "  ✅ 无报错") +
      (unmounted.length
        ? `  ❌ 有档没挂载完（工具时序，不是页面问题）：${unmounted.map((r) => r.case).join(", ")}`
        : "  ✅ 三档都挂载完") +
      (remounted.length
        ? `  ℹ️ 走过一次冷启动重试：${remounted.map((r) => r.case).join(", ")}`
        : ""),
  );
}

console.log(`\n报告：${path.join(OUTDIR, "report.json")}`);
