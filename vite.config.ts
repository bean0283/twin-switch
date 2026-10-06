import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;
// GitHub Pages serves only the public demo from the repository subpath.
// Normal WebUI and Tauri builds intentionally keep Vite's root base.
// @ts-expect-error process is a nodejs global
const base = process.env.VITE_PAGES_DEMO === "1" ? "/twin-switch/" : "/";

// https://vite.dev/config/
export default defineConfig(async () => ({
  base,
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri` and Cargo output.
      //
      // 本仓库是 Cargo 工作区（根 Cargo.toml 的 `[workspace]`），因此 cargo 的编译产物
      // 落在**项目根目录**的 `target/`，而不是 `src-tauri/target/`。只忽略 `src-tauri`
      // 会让 Vite 去监听 `target/debug/deps/*.dll`，一旦 Rust 正在编译（DLL 被占用），
      // 监听器就抛 `EBUSY: resource busy or locked` 并导致 `beforeDevCommand` 非零退出。
      ignored: ["**/src-tauri/**", "**/target/**"],
    },
  },
}));
