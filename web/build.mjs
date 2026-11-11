// Bundles the web interface into web/dist, which the Rust binary embeds.
import { build, context } from "esbuild";
import { cpSync, mkdirSync, rmSync, copyFileSync } from "node:fs";

const watch = process.argv.includes("--watch");
const out = "dist";

rmSync(out, { recursive: true, force: true });
mkdirSync(`${out}/assets`, { recursive: true });
mkdirSync(`${out}/pdfjs`, { recursive: true });

copyFileSync("src/index.html", `${out}/index.html`);
copyFileSync("src/favicon.svg", `${out}/favicon.svg`);
copyFileSync("src/theme-init.js", `${out}/assets/theme-init.js`);
// pdf.js runs its parser in a worker and loads decoders on demand.
copyFileSync("node_modules/pdfjs-dist/build/pdf.worker.min.mjs", `${out}/assets/pdf.worker.mjs`);
cpSync("node_modules/pdfjs-dist/standard_fonts", `${out}/pdfjs/standard_fonts`, { recursive: true });
cpSync("node_modules/pdfjs-dist/wasm", `${out}/pdfjs/wasm`, { recursive: true });

const options = {
  entryPoints: { app: "src/main.ts", styles: "src/styles.css" },
  outdir: `${out}/assets`,
  bundle: true,
  format: "esm",
  target: ["es2022", "chrome110", "firefox110", "safari16"],
  minify: !watch,
  sourcemap: watch ? "inline" : false,
  legalComments: "none",
  logLevel: "info",
  loader: { ".svg": "text" },
};

if (watch) {
  const ctx = await context(options);
  await ctx.watch();
  console.log("watching…");
} else {
  await build(options);
}
