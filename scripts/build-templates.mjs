// Build template client bundles: templates/*/src -> templates/*/assets.
// Run from templates/ (deps installed there): node ../scripts/build-templates.mjs
// assets/ are committed so the CLI (and users) never needs npm.
import { createRequire } from "module";
const require = createRequire(new URL("../templates/package.json", import.meta.url));
const { build } = require("esbuild");
const { readdirSync, rmSync, statSync, mkdirSync } = require("node:fs");

// shiki lazy-loads every language and theme as its own chunk (350+ of them,
// ~20MB). Missing chunks fall back to a plain <pre> in the viewer, so we
// build with splitting and then keep only a curated set + shared chunks.
const KEEP = new Set([
  "markdown", "json", "javascript", "typescript", "rust", "python", "bash",
  "shellscript", "yaml", "css", "html", "xml", "diff", "sql", "toml", "ini",
  "c", "cpp", "go", "java", "ruby", "swift", "lua", "dockerfile", "graphql",
  // pierre themes the viewer actually uses
  "pierre-dark", "pierre-light",
]);

const chunksDir = new URL("../templates/vault/assets/chunks/", import.meta.url);
// esbuild doesn't clean outdir; start fresh so stale chunks don't linger
rmSync(chunksDir, { recursive: true, force: true });
mkdirSync(chunksDir, { recursive: true });

await build({
  entryPoints: ["vault/src/viewer.mjs"],
  outdir: "vault/assets",
  entryNames: "[name]",
  bundle: true,
  format: "esm",
  splitting: true,
  chunkNames: "chunks/[name]-[hash]",
  target: "es2022",
  minify: true,
  logLevel: "warning",
});

let kept = 0, pruned = 0;
for (const f of readdirSync(chunksDir)) {
  // strip the "-HASH8.js" suffix; note the dash is consumed, so shared
  // chunks land at bare "chunk"
  const base = f.replace(/-([A-Z0-9]{8})\.js$/, "");
  if (base === "chunk" || KEEP.has(base)) { kept++; continue; }
  rmSync(new URL(f, chunksDir), { force: true });
  pruned++;
}
let total = 0;
for (const f of readdirSync(chunksDir)) total += statSync(new URL(f, chunksDir)).size;
console.log(`viewer.js ${(statSync(new URL("../templates/vault/assets/viewer.js", import.meta.url)).size / 1024).toFixed(0)}KB; chunks kept ${kept}, pruned ${pruned}, ${(total / 1024).toFixed(0)}KB`);
