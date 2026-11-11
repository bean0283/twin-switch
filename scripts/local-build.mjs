#!/usr/bin/env node
/**
 * 本地验证包构建：仓库版本固定不动，构建时临时抬高一版。
 *
 * ## 规则
 *
 * - 仓库里 5 处版本号长期锁死在 `src-tauri/Cargo.toml` 的 base 版本（当前 0.4.14），
 *   **不随本地打包递增**；
 * - 本地打包时临时写成 base 的 patch+1（0.4.14 → 0.4.15，恒定，不叠加），
 *   构建结束后原样还原（含 `Cargo.lock`），工作区保持干净；
 * - 路径走「临时改写 → 构建 → finally 还原」，所以中途失败/中断也不会把版本留在仓库里。
 *
 * ## 为什么必须改文件，而不是 `--config` 覆盖
 *
 * 应用运行时版本来自 `env!("CARGO_PKG_VERSION")`（`update.rs::APP_VERSION`），
 * 是编译期从 `Cargo.toml` 烤进二进制的；`--config` 只能改打包配置，
 * 改不了 exe 的 FileVersion 和界面显示的当前版本。
 *
 * ## updater 签名
 *
 * 本包只用于本机验证：默认关掉 `bundle.createUpdaterArtifacts`。
 * 有了 minisign 私钥要发版，请走 `npm run build:signed`（`scripts/release.mjs`）。
 *
 * ## 用法
 *
 *   npm run build:local              # 抬版 → 构建 → 还原
 *   npm run build:local -- --dry-run # 只演练改写/还原，不跑构建
 *   npm run build:local -- --keep    # 构建后不还原（调试用，记得手动 git checkout）
 *   npm run build:local -- --bundles nsis,msi
 */

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const argv = process.argv.slice(2);
const hasFlag = (n) => argv.includes(`--${n}`);
const dryRun = hasFlag("dry-run");
const keep = hasFlag("keep");
const bundles = (() => {
  const i = argv.indexOf("--bundles");
  return i >= 0 ? argv[i + 1] : "nsis";
})();

const log = (...a) => console.log("[local-build]", ...a);
const die = (msg) => {
  console.error("[local-build] 失败：" + msg);
  process.exit(1);
};

/** 5 处版本号。顺序无关，但 `src-tauri/Cargo.toml` 是 base 的唯一来源。 */
const VERSION_FILES = [
  { path: "src-tauri/Cargo.toml", kind: "cargo" },
  { path: "crates/wb-switch-core/Cargo.toml", kind: "cargo" },
  { path: "package.json", kind: "json" },
  { path: "src-tauri/tauri.conf.json", kind: "json" },
  { path: "src/components/update-entry.tsx", kind: "tsx" },
];

/** 每种文件的版本字段写法不同，抽出统一的「读当前值 / 换成新值」。 */
const READERS = {
  cargo: (t) => t.match(/^version\s*=\s*"([^"]*)"/m)?.[1],
  json: (t) => t.match(/"version"\s*:\s*"([^"]*)"/)?.[1],
  tsx: (t) => t.match(/fallbackVersion \|\| "([^"]*)"/)?.[1],
};

const WRITERS = {
  cargo: (t, v) => t.replace(/^version\s*=\s*"[^"]*"/m, `version = "${v}"`),
  json: (t, v) => t.replace(/("version"\s*:\s*")[^"]*(")/, `$1${v}$2`),
  tsx: (t, v) => t.replace(/(fallbackVersion \|\| ")[^"]*(")/, `$1${v}$2`),
};

function bumpPatch(version) {
  const parts = version.split(".").map(Number);
  if (parts.length !== 3 || parts.some((n) => !Number.isInteger(n) || n < 0)) {
    die(`版本号不是 x.y.z 形式：${version}`);
  }
  const [major, minor, patch] = parts;
  return `${major}.${minor}.${patch + 1}`;
}

// ---------------------------------------------------------------------------

const changed = []; // 还原用：按写入前的原文回滚
let lockBackup = null;

function applyLocalVersion(localVersion) {
  for (const item of VERSION_FILES) {
    const abs = join(ROOT, item.path);
    if (!existsSync(abs)) die(`找不到 ${item.path}`);
    const original = readFileSync(abs, "utf8");
    const patched = WRITERS[item.kind](original, localVersion);
    const got = READERS[item.kind](patched);
    if (got !== localVersion) die(`${item.path} 改写失败（读回 ${got}）`);
    changed.push({ abs, original });
    if (!dryRun) writeFileSync(abs, patched, "utf8");
  }
}

function restore() {
  const hadLock = Boolean(lockBackup);
  for (const { abs, original } of changed) {
    writeFileSync(abs, original, "utf8");
  }
  if (lockBackup) {
    writeFileSync(join(ROOT, "Cargo.lock"), lockBackup, "utf8");
    lockBackup = null;
  }
  // 自检：还原后必须与构建前逐字节一致，否则宁可报错也不留脏版本号
  for (const { abs, original } of changed) {
    if (readFileSync(abs, "utf8") !== original) die(`还原失败：${abs}`);
  }
  log(`已还原 ${changed.length} 处版本号${hadLock ? "与 Cargo.lock" : ""}`);
}

// 中断也要还原，否则下次 git diff 里全是版本号噪音
for (const sig of ["SIGINT", "SIGTERM"]) {
  process.on(sig, () => {
    if (!keep) restore();
    process.exit(130);
  });
}

// ---------------------------------------------------------------------------

const baseVersion = (() => {
  const text = readFileSync(join(ROOT, "src-tauri", "Cargo.toml"), "utf8");
  const v = READERS.cargo(text);
  if (!v) die("读不到 src-tauri/Cargo.toml 里的 version");
  return v;
})();
const localVersion = bumpPatch(baseVersion);

// 一致性预检：base 没对齐就先修好，别把错版本打成包
for (const item of VERSION_FILES) {
  const got = READERS[item.kind](readFileSync(join(ROOT, item.path), "utf8"));
  if (got !== baseVersion) {
    die(`版本号不一致：${item.path} = ${got}，但 src-tauri/Cargo.toml = ${baseVersion}`);
  }
}

log(`仓库版本 ${baseVersion}（保持不变）→ 本次打包用 ${localVersion}`);

lockBackup = existsSync(join(ROOT, "Cargo.lock")) ? readFileSync(join(ROOT, "Cargo.lock"), "utf8") : null;
applyLocalVersion(localVersion);

if (dryRun) {
  log(`dry-run：5 处将临时写成 ${localVersion}，构建后还原回 ${baseVersion}`);
  restore();
  log("dry-run 结束，未执行构建");
  process.exit(0);
}

// 关闭 updater 签名产物：本机没有私钥时开着它会直接构建失败。
// 落在 target/ 下（gitignore 内），不用临时目录，免去路径带空格的引号问题。
const overridePath = join(ROOT, "target", "ts-local-build.json");
writeFileSync(overridePath, JSON.stringify({ bundle: { createUpdaterArtifacts: false } }, null, 2));

const npm = process.platform === "win32" ? "npm.cmd" : "npm";
log(`开始构建（npm run tauri -- build --bundles ${bundles}）`);
const r = spawnSync(
  npm,
  ["run", "tauri", "--", "build", "--bundles", bundles, "--config", "target/ts-local-build.json"],
  { cwd: ROOT, stdio: "inherit", env: process.env, shell: process.platform === "win32" },
);
const status = r.status;

rmSync(overridePath, { force: true });
if (!keep) restore();

if (status !== 0) die(`构建失败（退出码 ${status}）`);

const setup = join(ROOT, "target", "release", "bundle", "nsis", `TwinSwitch_${localVersion}_x64-setup.exe`);
log(`构建完成：${existsSync(setup) ? setup : "（未找到 " + setup + "，检查 --bundles）"}`);
log(`仓库版本仍是 ${baseVersion}`);
