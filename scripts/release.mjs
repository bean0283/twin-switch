#!/usr/bin/env node
/**
 * 发布新版：**带签名构建 → 生成 updater 清单 latest.json → 发布 GitHub Release 并上传资产**。
 *
 * 一条命令覆盖整条链路：
 *
 *   node scripts/release.mjs                # 构建 + 发布
 *   node scripts/release.mjs --no-build     # 复用已有产物，只生成清单并发布
 *   node scripts/release.mjs --no-publish   # 只构建 + 生成 latest.json（不碰 GitHub）
 *   node scripts/release.mjs --notes "..."  # 给 Release 加一句说明
 *
 * ## 签名私钥（构建阶段必需）
 *
 * Tauri 的 updater 要求每个更新包都有 minisign 签名；客户端只认 `tauri.conf.json`
 * 里那把 `plugins.updater.pubkey`。所以：
 *
 * - 私钥放 `$TWIN_SWITCH_KEY_DIR`（默认 `~/.twin-switch-keys/`），**绝不进仓库**；
 * - 私钥丢了 = 以后签不出能被老客户端接受的包，只能让用户手动重装。
 *
 * ## GitHub 令牌（发布阶段必需）
 *
 * 优先读 `GITHUB_TOKEN`；没有就用 `git credential fill` 从系统凭据管理器取
 * （本机 Git Credential Manager 里存着的那把 PAT）。**不会写进任何文件。**
 *
 * ## 为什么要生成 latest.json
 *
 * 客户端查版本走的是 release 资产里的 `latest.json`，**不走 GitHub API**
 * （见 `crates/wb-switch-core/src/modules/update.rs` 的注释：API 有 60 次/小时/IP 限流）。
 * 因此每次发版都必须把 `latest.json` 作为资产传上去，否则老客户端永远看不到新版。
 * `signature` 字段必须是 `.sig` 文件的**完整内容**（含 `untrusted comment:` 那行）。
 */

import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OWNER = "bean0283";
const REPO = "twin-switch";

const argv = process.argv.slice(2);
const hasFlag = (name) => argv.includes(`--${name}`);
const flagValue = (name) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : undefined;
};

const doBuild = !hasFlag("no-build");
const doPublish = !hasFlag("no-publish");
const notes = flagValue("notes") ?? "";

const log = (...args) => console.log("[release]", ...args);
const die = (message) => {
  console.error("[release] 失败：" + message);
  process.exit(1);
};

// ---------------------------------------------------------------------------
// 版本号：以 src-tauri/Cargo.toml 为唯一来源（与 tauri.conf.json 手动保持一致）
// ---------------------------------------------------------------------------

function readVersion() {
  const text = readFileSync(join(ROOT, "src-tauri", "Cargo.toml"), "utf8");
  const m = text.match(/^version\s*=\s*"([^"]+)"/m);
  if (!m) die("读不到 src-tauri/Cargo.toml 里的 version");
  return m[1];
}

/** 交叉核对各处版本号，避免「发出去的包和仓库里的版本对不上」。 */
function assertVersionsAgree(version) {
  // 前端只剩一个兜底串，住在侧栏更新入口里（原来的 `App.tsx` 版本行已经改成组件）。
  const fallbackFile = join(ROOT, "src", "components", "update-entry.tsx");
  const fallback = existsSync(fallbackFile) ? readFileSync(fallbackFile, "utf8") : "";
  const checks = [
    ["package.json", JSON.parse(readFileSync(join(ROOT, "package.json"), "utf8")).version],
    [
      "src-tauri/tauri.conf.json",
      JSON.parse(readFileSync(join(ROOT, "src-tauri", "tauri.conf.json"), "utf8")).version,
    ],
    [
      "crates/wb-switch-core/Cargo.toml",
      readFileSync(join(ROOT, "crates", "wb-switch-core", "Cargo.toml"), "utf8").match(
        /^version\s*=\s*"([^"]+)"/m,
      )?.[1],
    ],
    ["src/components/update-entry.tsx", fallback.includes(`"${version}"`) ? version : null],
  ];
  const bad = checks.filter(([, got]) => got !== version);
  if (bad.length) {
    die(`版本号不一致（Cargo.toml = ${version}）：` + bad.map(([f, v]) => `${f}=${v}`).join(", "));
  }
  log(`版本号四处一致：${version}（第五处 Cargo.lock 由 cargo 自动跟上）`);
}

// ---------------------------------------------------------------------------
// 签名密钥
// ---------------------------------------------------------------------------

function resolveSigningKey() {
  if (process.env.TAURI_SIGNING_PRIVATE_KEY) {
    log("沿用已设置的 TAURI_SIGNING_PRIVATE_KEY");
    return;
  }
  const dir = process.env.TWIN_SWITCH_KEY_DIR ?? join(homedir(), ".twin-switch-keys");
  const key = join(dir, "twin-switch-updater.key");
  if (!existsSync(key)) {
    die(
      `找不到签名私钥：${key}\n` +
        "  用 `npx tauri signer generate -w <路径> -p <口令>` 生成；\n" +
        "  也可以用 TWIN_SWITCH_KEY_DIR 指定目录。",
    );
  }
  // ⚠️ 必须是**密钥内容**，不是路径。
  // Tauri CLI 认的是 `TAURI_SIGNING_PRIVATE_KEY`（内容或路径），
  // 而 `tauri signer generate` 在提示里列出的 `TAURI_SIGNING_PRIVATE_KEY_PATH`
  // 只有 `signer` 子命令吃 —— 用那个变量跑 `tauri build` 会在打包最后一步报
  // 「A public key has been found, but no private key」，白等一整轮编译。
  process.env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(key, "utf8").trim();
  const pwFile = join(dir, "twin-switch-updater.password");
  if (!process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD && existsSync(pwFile)) {
    process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = readFileSync(pwFile, "utf8").trim();
  }
  log(`签名私钥：${key}`);
}

// ---------------------------------------------------------------------------
// 构建
// ---------------------------------------------------------------------------

function build() {
  log("开始构建（npm run tauri build）—— release 编译通常要几分钟");
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  const r = spawnSync(npm, ["run", "tauri", "build"], {
    cwd: ROOT,
    stdio: "inherit",
    env: process.env,
    shell: process.platform === "win32",
  });
  if (r.status !== 0) die(`构建失败（退出码 ${r.status}）`);
  log("构建完成");
}

/** 定位安装包与签名；缺签名说明 `createUpdaterArtifacts` 没生效或没带密钥构建。 */
function locateArtifacts(version) {
  const bundle = join(ROOT, "target", "release", "bundle");
  const setup = join(bundle, "nsis", `TwinSwitch_${version}_x64-setup.exe`);
  const sig = `${setup}.sig`;
  const msi = join(bundle, "msi", `TwinSwitch_${version}_x64_en-US.msi`);
  if (!existsSync(setup)) die(`找不到安装包：${setup}`);
  if (!existsSync(sig)) {
    die(
      `找不到签名文件：${sig}\n` +
        "  说明这次构建没带签名密钥，或者 tauri.conf.json 少了 bundle.createUpdaterArtifacts。\n" +
        "  没有签名的包推送上去，客户端会直接拒绝安装。",
    );
  }
  return { setup, sig, msi: existsSync(msi) ? msi : null };
}

// ---------------------------------------------------------------------------
// latest.json
// ---------------------------------------------------------------------------

/**
 * 生成 updater 清单。
 *
 * ⚠️ `signature` 用的是 `.sig` 文件的**完整原文**（两行：`untrusted comment` + base64），
 * 所以必须交给 JSON.stringify 转义 —— 用 shell heredoc 直接插值会写出带裸换行的
 * 非法 JSON，客户端的清单解析会整份失败（而且失败得很安静：只报「清单解析失败」）。
 */
function writeManifest(version, artifacts) {
  const signature = readFileSync(artifacts.sig, "utf8");
  const archive = `TwinSwitch_${version}_x64-setup.exe`;
  const url = `https://github.com/${OWNER}/${REPO}/releases/latest/download/${archive}`;
  const manifest = {
    version,
    notes,
    pub_date: new Date().toISOString().replace(/\.\d+Z$/, "Z"),
    platforms: {
      "windows-x86_64-nsis": { signature, url },
      "windows-x86_64": { signature, url },
    },
  };
  const out = join(dirname(artifacts.setup), "latest.json");
  writeFileSync(out, JSON.stringify(manifest, null, 2) + "\n", "utf8");
  // 自检：写出来的必须是合法 JSON，且 signature 里的换行确实被转义了。
  JSON.parse(readFileSync(out, "utf8"));
  log(`已生成 ${out}`);
  return { manifestPath: out, archive, setup: artifacts.setup };
}

// ---------------------------------------------------------------------------
// GitHub 发布
// ---------------------------------------------------------------------------

function githubToken() {
  if (process.env.GITHUB_TOKEN) return process.env.GITHUB_TOKEN.trim();
  try {
    // ⚠️ 两个必须的护栏：
    // `GIT_TERMINAL_PROMPT=0` —— 当前 git 没配 credential helper 时，`credential fill`
    //   会转头去终端要账号密码，无人值守的脚本就会**永久挂住**；
    // `timeout` —— 万一还是卡住，宁可报错也别把整条发布流水线吊死。
    const out = execFileSync("git", ["credential", "fill"], {
      input: "protocol=https\nhost=github.com\n\n",
      cwd: ROOT,
      encoding: "utf8",
      timeout: 15_000,
      env: { ...process.env, GIT_TERMINAL_PROMPT: "0" },
    });
    const token = out
      .split("\n")
      .find((l) => l.startsWith("password="))
      ?.slice("password=".length)
      .trim();
    if (token) {
      log("已从系统凭据管理器取到 GitHub 令牌");
      return token;
    }
  } catch {
    // 落到下面的报错
  }
  die(
    "没有可用的 GitHub 令牌。\n" +
      "  ① 推荐：设环境变量 GITHUB_TOKEN=<你的 PAT>（需要 repo 权限）；\n" +
      "  ② 或者先让当前 PATH 上的 git 记住 github.com 的凭据" +
      "（`git config --global credential.helper manager` 后手动 push 一次）。\n" +
      "  注意：脚本只读凭据、不写任何文件；`git credential fill` 已禁交互且有 15 秒超时。",
  );
}

async function api(token, path, init = {}) {
  const res = await fetch(`https://api.github.com${path}`, {
    ...init,
    headers: {
      Authorization: `token ${token}`,
      Accept: "application/vnd.github+json",
      ...(init.headers ?? {}),
    },
  });
  const text = await res.text();
  const body = text ? JSON.parse(text) : null;
  return { status: res.status, body };
}

async function publish(version, { manifestPath, archive, setup }) {
  const token = githubToken();
  const tag = `v${version}`;

  let { status, body: release } = await api(token, `/repos/${OWNER}/${REPO}/releases/tags/${tag}`);
  if (status === 404) {
    const created = await api(token, `/repos/${OWNER}/${REPO}/releases`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        tag_name: tag,
        name: tag,
        body: notes || `TwinSwitch ${tag}`,
        draft: false,
        prerelease: false,
      }),
    });
    if (created.status >= 300) die(`创建 Release 失败：${created.status} ${JSON.stringify(created.body)}`);
    release = created.body;
    log(`已创建 Release ${tag}`);
  } else if (status >= 300) {
    die(`查询 Release 失败：${status} ${JSON.stringify(release)}`);
  } else {
    log(`Release ${tag} 已存在，复用并替换资产`);
  }

  const assets = release.assets ?? [];
  const uploads = [
    { name: archive, path: setup, type: "application/octet-stream" },
    { name: `${archive}.sig`, path: `${setup}.sig`, type: "application/octet-stream" },
    { name: "latest.json", path: manifestPath, type: "application/json" },
  ];

  for (const item of uploads) {
    const existing = assets.find((a) => a.name === item.name);
    if (existing) {
      // 同名资产必须先删再传：GitHub 不允许覆盖，直接传会 422。
      const del = await api(token, `/repos/${OWNER}/${REPO}/releases/assets/${existing.id}`, {
        method: "DELETE",
      });
      if (del.status >= 300) die(`删除旧资产 ${item.name} 失败：${del.status}`);
      log(`已删除旧资产 ${item.name}`);
    }
    const url = `https://uploads.github.com/repos/${OWNER}/${REPO}/releases/${release.id}/assets?name=${encodeURIComponent(item.name)}`;
    const res = await fetch(url, {
      method: "POST",
      headers: { Authorization: `token ${token}`, "Content-Type": item.type },
      body: readFileSync(item.path),
    });
    if (!res.ok) die(`上传 ${item.name} 失败：${res.status} ${await res.text()}`);
    const size = statSync(item.path).size;
    log(`已上传 ${item.name}（${(size / 1048576).toFixed(2)} MB）`);
  }

  log(`发布完成：https://github.com/${OWNER}/${REPO}/releases/tag/${tag}`);
}

// ---------------------------------------------------------------------------

async function main() {
  const version = readVersion();
  assertVersionsAgree(version);
  mkdirSync(join(ROOT, "target"), { recursive: true });

  if (doBuild) {
    resolveSigningKey();
    build();
  } else {
    log("跳过构建（--no-build）");
  }

  const artifacts = locateArtifacts(version);
  const manifest = writeManifest(version, artifacts);

  if (!doPublish) {
    log("跳过发布（--no-publish）；latest.json 已就绪");
    return;
  }
  await publish(version, manifest);

  // 清理：latest.json 是发布产物，不该留在工作区被误提交。
  rmSync(manifest.manifestPath, { force: true });
}

main().catch((error) => die(error?.stack ?? String(error)));
