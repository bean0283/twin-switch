// 把候选 SVO 光栅化成 PNG（大图看细节 + 小图看 16px 可辨识度）。
// 用法：NODE_PATH=<managed workspace>/node_modules node rasterize.mjs <outDir> <svg...>
import { Resvg } from "@resvg/resvg-js";
import fs from "node:fs";
import path from "node:path";

const [outDir, ...svgs] = process.argv.slice(2);
if (!outDir || svgs.length === 0) {
  console.error("用法: node rasterize.mjs <outDir> <svg...>");
  process.exit(1);
}
fs.mkdirSync(outDir, { recursive: true });

const SIZES = [512, 128, 64, 32, 16];

for (const file of svgs) {
  const svg = fs.readFileSync(file, "utf8");
  const base = path.basename(file, ".svg");
  for (const w of SIZES) {
    const r = new Resvg(svg, {
      fitTo: { mode: "width", value: w },
      background: "rgba(0,0,0,0)",
    });
    const png = r.render().asPng();
    const out = path.join(outDir, `${base}@${w}.png`);
    fs.writeFileSync(out, png);
  }
  console.log(`✓ ${base} → ${SIZES.join("/")} px`);
}
