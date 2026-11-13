#!/usr/bin/env node
/**
 * 本地验证包构建：仓库版本号保持不动，原样打出来。
 *
 * ## 规则
 *
 * - 仓库里的版本号就是打包版本（当前 0.4.14），打包**不改动任何版本号**；
 * - 运行时版本来自 `env!("CARGO_PKG_VERSION")`（`update.rs::APP_VERSION`），
 *   仓库版本即界面显示的当前版本；
 * - 打包产物落在 `target/release/bundle/<bundles>/`，文件名自带版本号。
 *
 * ## updater 签名
 *
 * 本包只用于本机验证：默认关掉 `bundle.createUpdaterArtifacts`。
 * 有了 minisign 私钥要发版，请走 `npm run build:signed`（`scripts/release.mjs`）。
 *
 * ## 用法
 *
 *   npm run build:local              # 直接按仓库版本构建
 *   npm run build:local -- --bundles nsis,msi
 */

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const argv = process.argv.slice(2);
const bundles = (() => {
  const i = argv.indexOf("--bundles");
  return i >= 0 ? argv[i + 1] : "nsis";
})();

const log = (...a) => console.log("[local-build]", ...a);
const die = (msg) => {
  console.error("[local-build] 失败：" + msg);
  process.exit(1);
};

// 一致性预检：5 处版本号没对齐就先停下，别打出错版本的包
const VERSION_FILES = [
  { path: "src-tauri/Cargo.toml", kind: "cargo" },
  { path: "crates/wb-switch-core/Cargo.toml", kind: "cargo" },
  { path: "package.json", kind: "json" },
  { path: "src-tauri/tauri.conf.json", kind: "json" },
  { path: "src/components/update-entry.tsx", kind: "tsx" },
];

const READERS = {
  cargo: (t) => t.match(/^version\s*=\s*"([^"]*)"/m)?.[1],
  json: (t) => t.match(/"version"\s*:\s*"([^"]*)"/)?.[1],
  tsx: (t) => t.match(/fallbackVersion \|\| "([^"]*)"/)?.[1],
};

const baseVersion = (() => {
  const text = readFileSync(join(ROOT, "src-tauri", "Cargo.toml"), "utf8");
  const v = READERS.cargo(text);
  if (!v) die("读不到 src-tauri/Cargo.toml 里的 version");
  return v;
})();

for (const item of VERSION_FILES) {
  const got = READERS[item.kind](readFileSync(join(ROOT, item.path), "utf8"));
  if (got !== baseVersion) {
    die(`版本号不一致：${item.path} = ${got}，但 src-tauri/Cargo.toml = ${baseVersion}`);
  }
}

log(`按仓库版本构建，不改版本号（${baseVersion}）`);

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

if (status !== 0) die(`构建失败（退出码 ${status}）`);

const setup = join(ROOT, "target", "release", "bundle", "nsis", `TwinSwitch_${baseVersion}_x64-setup.exe`);
log(`构建完成：${existsSync(setup) ? setup : "（未找到 " + setup + "，检查 --bundles）"}`);
