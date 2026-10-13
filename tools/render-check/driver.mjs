// 渲染自查驱动：headless Edge + CDP。
//
// 每个尺寸**单独开一个 Edge**（旧进程没退干净会静默绑定失败），端口每次随机。
// 判据见 measure()：横向溢出 / 竖排文字 / 页脚与拖拽手柄重叠 / console 报错。
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";

const EDGE = "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe";
// ⚠️ 默认必须是 `localhost`，不能是 `127.0.0.1`：`vite --host 127.0.0.1` 实际只监听 `[::1]`
//    （IPv6），用 `127.0.0.1` 直接连不上（curl 拿 `000`）⇒ 整轮自查全是假数据。
//    仍然可以用 `RC_BASE` 覆盖。
const BASE = process.env.RC_BASE || "http://localhost:1420";
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
    text: (alert ? alert.textContent || '' : '').replace(/\\s+/g, ' ').trim().slice(0, 420),
  };
})()`;

/**
 * 「合并后的 SOLO 不该再出现在客户端标签条上」。
 *
 * 判据落在**真实的标签条 DOM** 上，不是看源码：客户端切换条是一个 `role="group"`、
 * `aria-label` 以「Trae 客户端」开头的容器，里面每个 `<button>` 就是一个客户端。
 * 官方把 SOLO 并进 Trae CN 之后本机没有 SOLO 自己的程序（`runnable = false`）
 * ⇒ 它必须从标签条上消失，承载方 Trae CN 必须还在。
 *
 * ⚠️ 只认**标签条内部**的按钮。页面上别处也会出现「TRAE SOLO CN」字样（账号页的
 *    「一并带入」提示条就会写来源客户端名），那不算漏出来 —— 所以不能全文搜字符串。
 * ⚠️ 自带轮询：页面挂载后还要拉一次 `trae_list_clients` 才有标签条。
 *
 * 同一份探针还顺手验**国际版临时开关**（`INTL_GATE = false`）：标签条上不许出现国际版。
 * ⚠️ 桩里必须同时放 `trae-intl`（**已安装可用**的那种形态）—— 否则「国际版不在标签条上」
 *    可能只是因为它本来就没装，验不出「开关真的把它藏了」。
 */
const CLIENT_PILL_PROBE = `(async () => {
  const readPills = () => {
    const out = [];
    for (const g of document.querySelectorAll('[role="group"]')) {
      const label = g.getAttribute('aria-label') || '';
      if (!label.startsWith('Trae 客户端')) continue;
      for (const b of g.querySelectorAll('button')) {
        out.push({ group: label, text: (b.textContent || '').replace(/\\s+/g, ' ').trim() });
      }
    }
    return out;
  };
  const deadline = Date.now() + 15000;
  let pills = readPills();
  while (pills.length === 0 && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 200));
    pills = readPills();
  }
  const solo = pills.filter((p) => /SOLO/i.test(p.text));
  const intl = pills.filter((p) => /国际版|Trae（国际/i.test(p.text));
  const groups = [...document.querySelectorAll('[role="group"]')]
    .map((g) => g.getAttribute('aria-label') || '')
    .filter((l) => l.startsWith('Trae 客户端'));
  return {
    // 有标签条 **且** 一条 SOLO、一条国际版都没有，才算过。
    ok: pills.length > 0 && solo.length === 0 && intl.length === 0,
    pillCount: pills.length,
    labels: pills.map((p) => p.text),
    soloPills: solo.map((p) => p.text + ' @ ' + p.group),
    intlPills: intl.map((p) => p.text + ' @ ' + p.group),
    groups,
    skeletons: document.querySelectorAll('[data-slot="skeleton"]').length,
  };
})()`;

/**
 * 账号卡片头像 → token 弹窗（T54 新增；T60 起多走一趟「刷新」）。
 *
 * 判据落在真实 DOM 上：点**第一张账号卡的头像按钮**（`aria-label="查看 token…"`），
 * 弹窗里必须出现 —— token 块（含「复制 token」按钮）、有效期一行、「刷新 token 有效期」按钮。
 *
 * ★ T60 追加的第二段：点「刷新 token 有效期」→ 弹「强制刷新」确认窗 → 点「强制刷新」
 *   → 断言弹窗里那条「本次刷新 前 → 后」对照条真的渲染出来，**并且判出「变短了」**。
 *   这是「有效期由服务端签发、刷新不等于延长」这句文案唯一的抓手；光断言文案在不在，
 *   挡不住「对照条根本没画」。
 *
 * ⚠️ 只认 `role="dialog"` 里的内容：页面上别处也有「token」字样（回滚卡、编辑器），
 *    全文搜会把别处的文案算进来。
 * ⚠️ 头像按钮必须**真的是 button**（`type=button` 的兄弟节点约定）：探针顺手断言这点，
 *    因为「点不动」是这类改动最常见的回归。
 * ⚠️ 对照条的判据读 **DOM 属性** `data-refresh-shift`，不看正文文本 —— `index.html`
 *    的看门狗脚本源码也在 `<body>` 里，全文搜这类判定从第一毫秒起就恒真/恒假。
 *
 * ★ T60b 追加：读 `data-issued-validity-days`，断言文案里写出来的「官方签发的天数」
 *   与产品一致（**Trae 14 / WorkBuddy 28**）。两个产品共用这一个组件，写死一个数字
 *   就会让其中一个界面撒谎，光看「文案里有天数字」是抓不到的。
 */
const TOKEN_DIALOG_PROBE = `(async () => {
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + 20000;
  let trigger = null;
  while (!trigger && Date.now() < deadline) {
    trigger = [...document.querySelectorAll('button[aria-label]')].find((b) =>
      /查看 token/.test(b.getAttribute('aria-label') || ''),
    );
    if (!trigger) await wait(200);
  }
  if (!trigger) return { ok: false, reason: '没找到账号头像按钮（aria-label 含「查看 token」）', errors: [] };
  trigger.click();
  let dlg = null;
  const d2 = Date.now() + 15000;
  while (!dlg && Date.now() < d2) {
    dlg = document.querySelector('[role="dialog"]');
    if (!dlg) await wait(200);
  }
  if (!dlg) return { ok: false, reason: '点了头像没弹出弹窗', errors: [] };
  const text = (dlg.textContent || '').replace(/\\s+/g, ' ').trim();
  const buttons = [...dlg.querySelectorAll('button')].map((b) =>
    (b.textContent || '').replace(/\\s+/g, ' ').trim(),
  );
  const hasCopy = buttons.some((t) => /复制 token/.test(t));
  const hasRefresh = buttons.some((t) => /刷新 token 有效期/.test(t));
  const hasExpiry = /有效期/.test(text);
  const hasMasked = /…/.test(text);
  // ⚠️ 单位回归（线上事故）：exp 是毫秒时被再乘 1000 ⇒ 界面显示「58765/4/20」。
  //    只要有效期里出现 ≥ 3000 的年份就是单位算错了 —— 这条是 UI 层的唯一抓手。
  const year = Number((text.match(/(\\d{4,6})\\/\\d{1,2}\\/\\d{1,2}/) || [])[1] || 0);
  const saneYear = !year || (year >= 2000 && year < 3000);

  // ---- T60 第二段：把「刷新 → 确认 → 强制刷新」这条链路真走一遍 ----
  let refreshFlow = 'not-started';
  let alertText = null;
  let forceAlert = null;
  const refreshBtn = [...dlg.querySelectorAll('button')].find((b) =>
    /刷新 token 有效期/.test(b.textContent || ''),
  );
  if (!refreshBtn) {
    refreshFlow = 'refresh-button-missing';
  } else if (refreshBtn.disabled) {
    refreshFlow = 'refresh-button-disabled';
  } else {
    refreshBtn.click();
    let alert = null;
    const d3 = Date.now() + 10000;
    while (!alert && Date.now() < d3) {
      alert = document.querySelector('[role="alertdialog"]');
      if (!alert) await wait(150);
    }
    if (!alert) {
      refreshFlow = 'force-confirm-missing';
    } else {
      forceAlert = alert;
      alertText = (alert.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 220);
      const go = [...alert.querySelectorAll('button')].find((b) =>
        /强制刷新/.test(b.textContent || ''),
      );
      refreshFlow = 'force-button-missing';
      if (go) {
        go.click();
        refreshFlow = 'clicked';
        await wait(200);
        // 确认窗关掉、对照条挂上来（刷新成功后还会重新读一遍档案 ⇒ 中间可能短暂 loading）。
        const d4 = Date.now() + 12000;
        while (!dlg.querySelector('[data-refresh-shift]') && Date.now() < d4) await wait(150);
        if (!dlg.querySelector('[data-refresh-shift]')) refreshFlow = 'shift-bar-missing';
        // ⚠️ 再等确认窗真的从 DOM 里消失：Radix 的退场动画要几百毫秒，不等等的话
        //    alertGone 会恒为 false（时序噪音，看着像「弹窗关不掉」），截图里也会糊着两层弹窗。
        //    ⚠️ 本段是**模板字符串的内容**，注释里不许出现反引号（会提前截断模板）。
        const d5 = Date.now() + 4000;
        while (document.querySelector('[role="alertdialog"]') && Date.now() < d5) await wait(100);
      }
    }
  }
  const shiftEl = dlg.querySelector('[data-refresh-shift]');
  const hasShift = !!shiftEl;
  const shiftValue = shiftEl ? shiftEl.getAttribute('data-refresh-shift') : null;
  const shiftShortened = shiftValue === 'shortened';
  const shiftText = shiftEl ? (shiftEl.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 220) : null;
  const alertGone = !document.querySelector('[role="alertdialog"]');
  // ★ T60b：界面文案里写出来的「官方签发的有效期天数」。对照条与强制刷新确认窗都带这个
  //    属性，谁先出现就读谁 —— 两个产品不一样（Trae 14 / WorkBuddy 28），共用一个写死的
  //    数字会让其中一个界面撒谎，这条判据就是抓这个。
  const issuedRaw =
    (shiftEl && shiftEl.getAttribute('data-issued-validity-days')) ||
    (forceAlert && forceAlert.getAttribute('data-issued-validity-days')) ||
    null;
  const issuedDays = issuedRaw === null ? null : Number(issuedRaw);

  return {
    ok: hasCopy && hasRefresh && hasExpiry && hasMasked && saneYear && hasShift && shiftShortened,
    hasCopy, hasRefresh, hasExpiry, hasMasked, saneYear, year,
    hasShift, shiftValue, shiftShortened, shiftText, refreshFlow, alertText, alertGone,
    issuedDays,
    title: (dlg.querySelector('[data-slot="dialog-title"]')?.textContent || '').trim(),
    buttons,
    text: text.slice(0, 260),
    skeletons: document.querySelectorAll('[data-slot="skeleton"]').length,
  };
})()`;

/**
 * 设置页探针（T62）。
 *
 * 判三件事：
 *  1. **表单挂上来了**（`data-settings-loaded`）—— 挂不上时还要分清是「读失败」
 *     （`data-settings-error`）还是「白屏」，两种病不同药。
 *  2. **三个输入框等宽 + 同一水平线** —— 用户报过「三个输入框没有对齐」。
 *     这条只能量 `getBoundingClientRect()`：`天`/`份`/`GB` 宽度不同，谁把单位写成
 *     行内 span，三个框就会各自缩成不同宽度（实测 144 / 135 / 213 px）。看正文抓不到。
 *  3. **改一个值 → 保存 → 摘要跟着变** —— 否则「没存进去」和「存进去了但界面没刷新」
 *     长得一模一样（后者会让用户以为设置没生效，反复点保存）。
 *
 * ⚠️ 页面上的策略原文来自后端（`data-policy-summary`）；这里只断言它「有内容且跟着新值变」，
 *    不去逐字比对后端文案 —— 那样桩一漂就误报。
 */
const SETTINGS_PROBE = `(async () => {
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + 20000;
  let card = null;
  while (!card && Date.now() < deadline) {
    card = document.querySelector('[data-settings-loaded]');
    if (!card) await wait(200);
  }
  if (!card) {
    const err = document.querySelector('[data-settings-error]');
    // ⚠️ 失败分支也要带上 alignW 这些键：判定是按「有没有 alignW」分流用例的，
    //    少了它们这一档会**静默掉出**判定范围 —— 最典型的「失败长得像通过」。
    //    ⚠️ 本段是**模板字符串的内容**，注释里不许出现反引号（会提前截断模板）。
    return {
      ok: false,
      alignW: false,
      alignTop: false,
      alignUnit: false,
      savedOk: false,
      widths: [],
      tops: [],
      reason: err ? '页面报了「读取设置失败」' : '设置表单没挂上来（data-settings-loaded 缺失）',
      skeletons: document.querySelectorAll('[data-slot="skeleton"]').length,
      body: (document.body.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 200),
    };
  }
  const field = (n) => card.querySelector('[data-field="' + n + '"]');
  const val = (n) => { const el = field(n); return el ? el.value : null; };
  const keys = ['keepDays', 'keepCount', 'maxTotalGb'];
  const boxes = keys.map((n) => {
    const el = field(n);
    if (!el) return null;
    const r = el.getBoundingClientRect();
    const unit = el.parentElement ? el.parentElement.querySelector('span') : null;
    const ur = unit ? unit.getBoundingClientRect() : null;
    return {
      n,
      w: Math.round(r.width * 10) / 10,
      left: Math.round(r.left * 10) / 10,
      top: Math.round(r.top * 10) / 10,
      unitLeft: ur ? Math.round(ur.left * 10) / 10 : null,
    };
  });
  const present = boxes.filter(Boolean);
  const widths = present.map((b) => b.w);
  const tops = present.map((b) => b.top);
  const unitLefts = present.map((b) => b.unitLeft).filter((v) => v !== null);
  const alignW =
    present.length === 3 && Math.max.apply(null, widths) - Math.min.apply(null, widths) <= 1;
  const alignTop =
    present.length === 3 && Math.max.apply(null, tops) - Math.min.apply(null, tops) <= 1;
  // 单位槽宽度一致 ⇒ 「天 / 份 / GB」落在同一条竖线上
  const alignUnit =
    unitLefts.length !== 3 ||
    Math.max.apply(null, unitLefts.map((v, i) => v - present[i].left)) -
      Math.min.apply(null, unitLefts.map((v, i) => v - present[i].left)) <= 2;

  const policyEl = card.querySelector('[data-policy-summary]');
  const policyBefore = policyEl ? (policyEl.textContent || '').replace(/\\s+/g, ' ').trim() : null;

  // 改「份数」→ 保存 → 摘要必须跟着变
  let savedOk = false;
  let policyAfter = null;
  let saveDisabled = null;
  const count = field('keepCount');
  if (count) {
    count.focus();
    // React 受控输入：必须走原型上的 value setter + 原生 input 事件，
    // 直接 el.value = '2' 改不动 React 的 state（onChange 收不到）。
    const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, 'value').set;
    setter.call(count, '2');
    count.dispatchEvent(new Event('input', { bubbles: true }));
    await wait(200);
    const save = card.querySelector('[data-action="save"]');
    saveDisabled = save ? !!save.disabled : null;
    if (save && !save.disabled) {
      save.click();
      const d2 = Date.now() + 6000;
      while (Date.now() < d2) {
        const p = card.querySelector('[data-policy-summary]');
        const txt = p ? (p.textContent || '').replace(/\\s+/g, ' ').trim() : '';
        if (txt && txt.indexOf('2 份') >= 0) { policyAfter = txt; savedOk = true; break; }
        await wait(150);
      }
      if (!policyAfter) {
        const p = card.querySelector('[data-policy-summary]');
        policyAfter = p ? (p.textContent || '').replace(/\\s+/g, ' ').trim() : null;
      }
    }
  }
  return {
    ok: alignW && alignTop && !!policyBefore && savedOk,
    alignW, alignTop, alignUnit, widths, tops, boxes, saveDisabled,
    policyBefore, policyAfter, savedOk,
    formBefore: { keepDays: val('keepDays'), keepCount: val('keepCount'), maxTotalGb: val('maxTotalGb') },
    keepCountAfter: val('keepCount'),
    skeletons: document.querySelectorAll('[data-slot="skeleton"]').length,
    title: (document.querySelector('h1')?.textContent || '').trim(),
    text: (card.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 300),
  };
})()`;

/**
 * 清理页的「保留策略 + 从最老的清」探针（T62）。
 *
 * 判三件事：
 *  1. 工具残留那一类头上画出了**保留策略原文**（`data-backup-policy`，来自后端，与判定同源）；
 *  2. 「超出策略」与「最旧」两个标记都画出来了；
 *  3. **第一个批次就是最旧的那一批** —— 用户明确要求「优先清理最老的版本，而不是清理到只剩一份」。
 *     这条只能靠 DOM 顺序判（`data-item-oldest` 落在第一个 `[data-item-title]` 上）。
 */
const CLEANUP_POLICY_PROBE = `(async () => {
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + 20000;
  let banner = null;
  while (!banner && Date.now() < deadline) {
    banner = document.querySelector('[data-backup-policy]');
    if (!banner) await wait(200);
  }
  const titles = [...document.querySelectorAll('[data-item-title]')];
  const oldestIdx = titles
    .map((t, i) => (t.getAttribute('data-item-oldest') === '1' ? i : -1))
    .filter((i) => i >= 0);
  const overIdx = titles
    .map((t, i) => (t.getAttribute('data-item-over') === '1' ? i : -1))
    .filter((i) => i >= 0);
  const policy = banner ? (banner.getAttribute('data-backup-policy') || '').trim() : null;
  // T61：影子客户端必须**折成一条**（真机上它本来会刷 17 行 TRAE SOLO CN · Cache…）。
  // 判据只看 DOM 上的钩子，不看正文全文 —— 那会连 index.html 的看门狗源码一起扫到。
  const mergedRows = titles.filter((t) => t.getAttribute('data-item-merged') === '1');
  // 注意事项也要按结构取（每条 Alert 自带 data-cleanup-note）。
  const noteTexts = [...document.querySelectorAll('[data-cleanup-note]')].map(
    (el) => el.getAttribute('data-cleanup-note') || ''
  );
  const shadowNote = noteTexts.find((n) => n.includes('TRAE SOLO CN')) || '';
  const shadowLies = noteTexts.some((n) => n.includes('TRAE SOLO CN') && n.includes('正在运行'));
  return {
    ok: !!banner && oldestIdx.length > 0 && overIdx.length > 0 && oldestIdx[0] === 0,
    hasBanner: !!banner,
    policy,
    itemCount: titles.length,
    oldestIdx, overIdx,
    // 最旧那一批必须排在最前 —— 清理顺序 = 先从最老的清
    firstIsOldest: oldestIdx[0] === 0,
    // 「清理到只剩一份」的老毛病：把除最新之外全标成推荐
    recommendedCount: overIdx.length,
    order: titles.map((t) => t.getAttribute('data-item-oldest') + t.getAttribute('data-item-over')),
    // ---- T61 ----
    mergedCount: mergedRows.length,
    mergedOnlyOnce: mergedRows.length === 1,
    mergedTitle: mergedRows[0] ? mergedRows[0].getAttribute('data-item-title') || '' : '',
    mergedTitleNamesHost: !!mergedRows[0] && (mergedRows[0].getAttribute('data-item-title') || '').includes('已并入'),
    shadowNoteSaysMerged: shadowNote.includes('已并入'),
    shadowNoteLiesRunning: shadowLies,
    skeletons: document.querySelectorAll('[data-slot="skeleton"]').length,
    text: (document.body.textContent || '').replace(/\\s+/g, ' ').trim().slice(0, 240),
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

  // 「合并后的 SOLO 不该再出现在客户端标签条上」：五个有客户端选择器的页面全都要覆盖
  // （v0.2.25 之前账号管理页自己内联了一套标签条，正是它漏了）。
  { name: "trae-pill-switch-1600", route: "/trae-switch", width: 1600, height: 900, probe: CLIENT_PILL_PROBE },
  { name: "trae-pill-switch-1360", route: "/trae-switch", width: 1360, height: 800, probe: CLIENT_PILL_PROBE },
  { name: "trae-pill-switch-730", route: "/trae-switch", width: 730, height: 800, probe: CLIENT_PILL_PROBE },
  { name: "trae-pill-records-1360", route: "/trae-records", width: 1360, height: 800, probe: CLIENT_PILL_PROBE },
  { name: "trae-pill-home-1360", route: "/", width: 1360, height: 800, probe: CLIENT_PILL_PROBE },
  { name: "trae-pill-import-1360", route: "/workbuddy-import", width: 1360, height: 800, probe: CLIENT_PILL_PROBE },
  { name: "trae-pill-export-1360", route: "/workbuddy-export", width: 1360, height: 800, probe: CLIENT_PILL_PROBE },

  // ★ 本轮新增（T54）：账号卡片头像 → token 弹窗（看 / 复制 / 有效期 / 续期）。
  //   桩里 `oauth_717036` 是网页凭证账号（有 oauth.json）⇒ 点它的头像才拿得到 token。
  //   ⚠️ 无头浏览器没有剪贴板权限 ⇒ 只断言「按钮在 + 文案对」，不真的点复制。
  { name: "trae-token-dialog-1600", route: "/trae-switch", width: 1600, height: 900, probe: TOKEN_DIALOG_PROBE },
  { name: "trae-token-dialog-1360", route: "/trae-switch", width: 1360, height: 800, probe: TOKEN_DIALOG_PROBE },
  { name: "trae-token-dialog-730", route: "/trae-switch", width: 730, height: 800, probe: TOKEN_DIALOG_PROBE },

  // ★ 本轮新增（T56）：WorkBuddy 账号页头像 → 同一套 token 弹窗。
  //   两个产品共用一个组件，所以这里验的是「WB 页也接上了 + 数据来自 WB 那条命令」。
  { name: "wb-token-dialog-1600", route: "/workbuddy-switch", width: 1600, height: 900, probe: TOKEN_DIALOG_PROBE },
  { name: "wb-token-dialog-1360", route: "/workbuddy-switch", width: 1360, height: 800, probe: TOKEN_DIALOG_PROBE },
  { name: "wb-token-dialog-730", route: "/workbuddy-switch", width: 730, height: 800, probe: TOKEN_DIALOG_PROBE },

  // ★ T62 新增：设置页（整库备份保留策略）。
  //   重点判「三个输入框等宽对齐」（用户报过没对齐）+「保存后摘要跟着变」。
  { name: "settings-1600", route: "/settings", width: 1600, height: 900, probe: SETTINGS_PROBE },
  { name: "settings-1360", route: "/settings", width: 1360, height: 800, probe: SETTINGS_PROBE },
  { name: "settings-730", route: "/settings", width: 730, height: 800, probe: SETTINGS_PROBE },

  // ★ T62 新增：清理页的保留策略条 + 「先从最老的清」排序。
  { name: "cleanup-policy-1600", route: "/trae-cleanup", width: 1600, height: 900, probe: CLEANUP_POLICY_PROBE },
  { name: "cleanup-policy-1360", route: "/trae-cleanup", width: 1360, height: 800, probe: CLEANUP_POLICY_PROBE },
  { name: "cleanup-policy-730", route: "/trae-cleanup", width: 730, height: 800, probe: CLEANUP_POLICY_PROBE },
];

const only = process.argv[2];
/**
 * ⚠️ 省时开关：`RC_WIDTH=1360` 只跑指定宽度。
 *
 * 迭代期一个改动往往只影响一档（最常见是 1360），三档全跑要三倍时间（每档约 1.5~2.5 分钟，
 * 大头是浏览器冷启动 + 页面挂载，不是断言）。**收尾仍然要三档全跑**，那时不传 `RC_WIDTH`。
 */
const onlyWidth = process.env.RC_WIDTH ? Number(process.env.RC_WIDTH) : 0;
const list = (only ? CASES.filter((c) => c.name.includes(only)) : CASES).filter(
  (c) => !onlyWidth || c.width === onlyWidth,
);

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
    // ⚠️ 按**键名**分流，不按「有没有 `ok`」 —— 三份探针都有 `ok`，会串到同一个分支去。
    if ("hasCopy" in r.probe || !("soloPills" in r.probe) && !("found" in r.probe)) {
      console.log(
        r.probe.reason
          ? `  token : ❌ ${r.probe.reason}`
          : `  token : 复制=${r.probe.hasCopy} 刷新按钮=${r.probe.hasRefresh} 有效期=${r.probe.hasExpiry} 打码=${r.probe.hasMasked} 年份合理=${r.probe.saneYear}(${r.probe.year})` +
              `\n  token : 对照条=${r.probe.hasShift}(${r.probe.shiftValue}) 判出变短=${r.probe.shiftShortened} 链路=${r.probe.refreshFlow} 确认窗已关=${r.probe.alertGone}` +
              `\n  token : 对照条原文=${JSON.stringify(r.probe.shiftText)}` +
              `\n  token : 确认窗原文=${JSON.stringify(r.probe.alertText)}` +
              `\n  token : 标题=${JSON.stringify(r.probe.title)} 按钮=${JSON.stringify(r.probe.buttons)}` +
              `\n  token : 正文=${JSON.stringify(r.probe.text)}`,
      );
    } else if ("soloPills" in r.probe) {
      console.log(
        `  pills : ${r.probe.pillCount} 个标签 = ${JSON.stringify(r.probe.labels)}` +
          `\n  pills : SOLO 项 = ${JSON.stringify(r.probe.soloPills)}  国际版项 = ${JSON.stringify(r.probe.intlPills)}  skeleton=${r.probe.skeletons}` +
          `\n  pills : 容器 = ${JSON.stringify(r.probe.groups)}`,
      );
    } else {
      console.log(
        r.probe.found
          ? `  merge : found=true 按钮=${JSON.stringify(r.probe.btn)} 提示条=${JSON.stringify(r.probe.banner)} 视口=${r.probe.viewportW}` +
              `\n  merge : 文案=${JSON.stringify(r.probe.text)}`
          : `  merge : found=false alerts=${r.probe.alerts} skeleton=${r.probe.skeletons} buttons=${r.probe.buttons}` +
              `\n  merge : 正文=${JSON.stringify(r.probe.text)}`,
      );
    }
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

// 汇总判据二：客户端标签条上**一条 SOLO、一条国际版都不能有**（T53 起国际版也临时藏了），
// 且标签条得画出来。⚠️ 判据用 `/SOLO/i` 与 `/国际版/` 匹配标签文字，不是全文搜「TRAE SOLO CN」
// —— 账号页的「一并带入」提示条会写来源客户端名，全文搜会把那条正确文案判成漏。
const pillCases = out.filter((r) => r.probe && "soloPills" in r.probe);
if (pillCases.length) {
  const noBar = pillCases.filter((r) => (r.probe.pillCount ?? 0) === 0);
  const leaked = pillCases.filter((r) => (r.probe.soloPills || []).length > 0);
  const intlLeaked = pillCases.filter((r) => (r.probe.intlPills || []).length > 0);
  const messy = pillCases.filter(
    (r) => (r.before?.overflowX ?? 0) > 0 || (r.before?.verticalCount ?? 0) > 0,
  );
  console.log(
    `\n【判定】客户端标签条 ${pillCases.length - noBar.length}/${pillCases.length} 档画出来了` +
      (noBar.length ? `  ❌ 没标签条：${noBar.map((r) => r.case).join(", ")}` : "  ✅") +
      (leaked.length
        ? `  ❌ SOLO 仍在标签条上：${leaked.map((r) => `${r.case} → ${r.probe.soloPills.join(" / ")}`).join("; ")}`
        : "  ✅ 标签条上没有 SOLO") +
      (intlLeaked.length
        ? `  ❌ 国际版仍在标签条上（T53 开关没生效）：${intlLeaked
            .map((r) => `${r.case} → ${r.probe.intlPills.join(" / ")}`)
            .join("; ")}`
        : "  ✅ 标签条上没有国际版") +
      (messy.length ? `  ❌ 版式问题：${messy.map((r) => r.case).join(", ")}` : "  ✅ 版式干净"),
  );
}

// 汇总判据三（T54 / T56 / T60）：点头像弹出的 token 弹窗必须**三档都能开**、要素齐全，
// 且「刷新 → 确认 → 强制刷新 → 前后对照条」这条链路真的走通、判出「变短」。
// Trae 页与 WorkBuddy 页共用同一个组件，两个页面各有一组用例，这里一起判。
const tokenCases = out.filter((r) => r.probe && "hasCopy" in r.probe);
if (tokenCases.length) {
  const bad = tokenCases.filter((r) => !r.probe.ok);
  const messy = tokenCases.filter(
    (r) => (r.before?.overflowX ?? 0) > 0 || (r.before?.verticalCount ?? 0) > 0,
  );
  const badYear = tokenCases.filter((r) => r.probe.saneYear === false);
  // ★ T60：对照条没画 / 画了但没判出「变短」各算一类，别让它塌缩成同一句。
  const noShift = tokenCases.filter((r) => r.probe.hasShift !== true);
  const wrongShift = tokenCases.filter(
    (r) => r.probe.hasShift === true && r.probe.shiftShortened !== true,
  );
  // ★ T60b：**两个产品官方签发的天数不一样**（Trae 14 天 / WorkBuddy 28 天）。用例名带产品
  //    前缀，这里按前缀分别断言 —— T60 初版两边共用写死的 28 天，Trae 弹窗就在撒谎。
  //    ⚠️ 反证：把 `ISSUED_VALIDITY_DAYS.trae` 改成 28，这一条必须报错。
  const wantIssued = (name) => (name.startsWith("trae-token-dialog-") ? 14 : 28);
  const wrongIssued = tokenCases.filter((r) => r.probe.issuedDays !== wantIssued(r.case));
  console.log(
    `\n【判定】token 弹窗 ${tokenCases.length - bad.length}/${tokenCases.length} 档开出来且要素齐全` +
      (bad.length
        ? `  ❌ ${bad
            .map(
              (r) =>
                `${r.case}（${r.probe.reason || `复制=${r.probe.hasCopy} 刷新=${r.probe.hasRefresh} 有效期=${r.probe.hasExpiry} 打码=${r.probe.hasMasked} 年份=${r.probe.year} 对照条=${r.probe.hasShift} 变短=${r.probe.shiftShortened} 链路=${r.probe.refreshFlow}`}）`,
            )
            .join("; ")}`
        : "  ✅ 复制 / 刷新按钮 / 有效期 / 打码 全在") +
      (badYear.length
        ? `  ❌ 有效期年份不合理（单位算错？）：${badYear.map((r) => `${r.case} → ${r.probe.year}`).join(", ")}`
        : "  ✅ 有效期年份都在 2000~2999") +
      (noShift.length
        ? `  ❌ 没画出「本次刷新前 → 后」对照条：${noShift
            .map((r) => `${r.case}（链路=${r.probe.refreshFlow}）`)
            .join(", ")}`
        : "  ✅ 各档都走通「刷新 → 确认 → 强制刷新 → 对照条」") +
      (wrongShift.length
        ? `  ❌ 对照条没判出「刷新后反而变短」：${wrongShift
            .map((r) => `${r.case} → data-refresh-shift=${r.probe.shiftValue}`)
            .join(", ")}`
        : "  ✅ 对照条都把「变短」标了出来") +
      (wrongIssued.length
        ? `  ❌ 文案里的「官方签发天数」与产品不符：${wrongIssued
            .map((r) => `${r.case} → 实际 ${r.probe.issuedDays} 天，应为 ${wantIssued(r.case)} 天`)
            .join("; ")}`
        : "  ✅ Trae 档写 14 天、WorkBuddy 档写 28 天") +
      (messy.length ? `  ❌ 版式问题：${messy.map((r) => r.case).join(", ")}` : "  ✅ 版式干净"),
  );
  // 抽样打印一档的对照条原文，便于人工核对文案 —— 判据过 ≠ 文案说得对。
  const sample = tokenCases.find((r) => r.probe.shiftText);
  if (sample) {
    console.log(`  样例（${sample.case}）：${sample.probe.shiftText}`);
    console.log(`  强制刷新确认窗（${sample.case}）：${(sample.probe.alertText || "").slice(0, 160)}`);
  }
}

// 汇总判据四（T62）：设置页必须能开、三个输入框**等宽对齐**、保存后摘要跟着变。
const settingsCases = out.filter((r) => r.probe && "alignW" in r.probe);
if (settingsCases.length) {
  const bad = settingsCases.filter((r) => !r.probe.ok);
  const misalignedW = settingsCases.filter((r) => r.probe.alignW === false);
  const misalignedTop = settingsCases.filter((r) => r.probe.alignTop === false);
  const misalignedUnit = settingsCases.filter((r) => r.probe.alignUnit === false);
  const noSave = settingsCases.filter((r) => r.probe.savedOk !== true);
  const messy = settingsCases.filter(
    (r) => (r.before?.overflowX ?? 0) > 0 || (r.before?.verticalCount ?? 0) > 0,
  );
  console.log(
    `\n【判定】设置页 ${settingsCases.length - bad.length}/${settingsCases.length} 档开出来` +
      (bad.length
        ? `  ❌ ${bad.map((r) => `${r.case}（${r.probe.reason || "见明细"}）`).join("; ")}`
        : "  ✅ 策略表单 + 摘要都在") +
      (misalignedW.length
        ? `  ❌ 三个输入框不等宽：${misalignedW
            .map((r) => `${r.case} → ${JSON.stringify(r.probe.widths)}`)
            .join("; ")}`
        : "  ✅ 三个输入框等宽") +
      (misalignedTop.length
        ? `  ❌ 三个输入框不在同一水平线：${misalignedTop
            .map((r) => `${r.case} → top ${JSON.stringify(r.probe.tops)}`)
            .join("; ")}`
        : "  ✅ 三个输入框同一水平线") +
      (misalignedUnit.length
        ? `  ❌ 单位（天/份/GB）没落在同一条竖线上：${misalignedUnit.map((r) => r.case).join(", ")}`
        : "  ✅ 单位槽对齐") +
      (noSave.length
        ? `  ❌ 改值→保存后摘要没跟着变：${noSave
            .map((r) => `${r.case}（保存键 disabled=${r.probe.saveDisabled}，摘要=${JSON.stringify((r.probe.policyAfter || "").slice(0, 40))}）`)
            .join("; ")}`
        : "  ✅ 改值保存后摘要同步更新") +
      (messy.length ? `  ❌ 版式问题：${messy.map((r) => r.case).join(", ")}` : "  ✅ 版式干净"),
  );
  const s2 = settingsCases.find((r) => r.probe.policyBefore);
  if (s2) {
    console.log(`  设置页策略原文（${s2.case}）：${s2.probe.policyBefore}`);
    console.log(`  保存后（${s2.case}）：${s2.probe.policyAfter}`);
  }
}

// 汇总判据五（T62）：清理页的保留策略条 + 「先从最老的清」排序。
const policyCases = out.filter((r) => r.probe && "firstIsOldest" in r.probe);
if (policyCases.length) {
  const noBanner = policyCases.filter((r) => !r.probe.hasBanner);
  const noMark = policyCases.filter(
    (r) => (r.probe.oldestIdx || []).length === 0 || (r.probe.overIdx || []).length === 0,
  );
  const wrongOrder = policyCases.filter((r) => r.probe.firstIsOldest !== true);
  // T61：影子客户端（已并入、本机已无自己程序）必须折成**一条**并写明「已并入」，
  // 注意事项里**不许**再说它「正在运行」。反证：把后端改回 `is_running` + 逐目录列，
  // 这里会立刻报「拆成了 N 行」或「注意事项还在谎报正在运行」。
  const splitShadow = policyCases.filter((r) => r.probe.mergedCount !== 1);
  const namelessShadow = policyCases.filter((r) => r.probe.mergedTitleNamesHost !== true);
  const lyingShadow = policyCases.filter(
    (r) => r.probe.shadowNoteLiesRunning === true || r.probe.shadowNoteSaysMerged !== true,
  );
  const messy = policyCases.filter(
    (r) => (r.before?.overflowX ?? 0) > 0 || (r.before?.verticalCount ?? 0) > 0,
  );
  console.log(
    `\n【判定】清理页保留策略 ${policyCases.length - noBanner.length}/${policyCases.length} 档画出来` +
      (noBanner.length ? `  ❌ 没策略条：${noBanner.map((r) => r.case).join(", ")}` : "  ✅ 策略条都在") +
      (noMark.length
        ? `  ❌ 「超出策略」/「最旧」标记没画出来：${noMark
            .map((r) => `${r.case}（最旧 ${JSON.stringify(r.probe.oldestIdx)}，超出 ${JSON.stringify(r.probe.overIdx)}）`)
            .join("; ")}`
        : "  ✅ 「最旧」「超出策略」两个标记都在") +
      (wrongOrder.length
        ? `  ❌ 最旧的一批没排在最前面（清理顺序不是先从最老的清）：${wrongOrder
            .map((r) => `${r.case} → 标记序列 ${JSON.stringify(r.probe.order)}`)
            .join("; ")}`
        : "  ✅ 最旧的一批排在最前（先从最老的清）") +
      (splitShadow.length
        ? `  ❌ 影子客户端的残留没折成一条（看着像它还装着）：${splitShadow
            .map((r) => `${r.case} → ${r.probe.mergedCount} 条`)
            .join("; ")}`
        : "  ✅ 影子客户端的残留折成一条") +
      (namelessShadow.length
        ? `  ❌ 影子那条没写明「已并入谁」：${namelessShadow.map((r) => `${r.case} → ${r.probe.mergedTitle}`).join("; ")}`
        : "  ✅ 影子那条写明了「已并入谁」") +
      (lyingShadow.length
        ? `  ❌ 影子客户端的提示还在说谎（不许说「正在运行」）：${lyingShadow
            .map((r) => `${r.case} → 说已并入=${r.probe.shadowNoteSaysMerged} / 说正在运行=${r.probe.shadowNoteLiesRunning}`)
            .join("; ")}`
        : "  ✅ 影子客户端只说「已并入、只剩历史数据」，没说「正在运行」") +
      (messy.length ? `  ❌ 版式问题：${messy.map((r) => r.case).join(", ")}` : "  ✅ 版式干净"),
  );
  const s3 = policyCases.find((r) => r.probe.policy);
  if (s3) console.log(`  清理页策略原文（${s3.case}）：${s3.probe.policy}`);
}

console.log(`\n报告：${path.join(OUTDIR, "report.json")}`);
