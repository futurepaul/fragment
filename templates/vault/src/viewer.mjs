// vault viewer client — hash routing, markdown via marked, code files via
// @pierre/diffs' File component (Shiki highlighting), plain <pre> fallback.
import { marked } from "marked";
import { File as PierreFile, preloadHighlighter } from "@pierre/diffs";

// self-diagnosis: surface any module/boot failure in the sidebar so a blank
// page is never silent
const showErr = (msg) => {
  const t = document.getElementById("tree");
  if (t) t.innerHTML = `<div class="empty">⚠ ${String(msg).slice(0, 300)}</div>`;
};
window.addEventListener("error", (e) => showErr(e.message || e.error));
window.addEventListener("unhandledrejection", (e) => showErr(e.reason));

marked.setOptions?.({ gfm: true, breaks: false });

const $ = (sel) => document.querySelector(sel);
const qs = location.search || ""; // carries the ?view= token when present
const withView = (url) => url + (qs ? (url.includes("?") ? "&" : "?") + qs.slice(1) : "");
const TEXTIE = /\.(md|markdown|txt|json|js|mjs|cjs|ts|tsx|jsx|rs|py|sh|bash|zsh|toml|yml|yaml|css|html|htm|xml|csv|log|diff|patch|sql|go|java|kt|rb|php|c|h|cpp|hpp|swift|lua|dockerfile)$/i;
const IMG = /\.(png|jpe?g|gif|webp|svg|ico)$/i;
const LANG = { js: "javascript", mjs: "javascript", cjs: "javascript", ts: "typescript", tsx: "typescript", jsx: "javascript", rs: "rust", py: "python", sh: "bash", bash: "bash", zsh: "bash", rb: "ruby", go: "go", yml: "yaml", yaml: "yaml", md: "markdown", json: "json", css: "css", html: "html", xml: "xml", sql: "sql", toml: "toml", diff: "diff", patch: "diff" };

let files = [];            // [{path,size,updatedAt,rev}]
let byPath = new Map();    // lower path -> path
let byName = new Map();    // lower basename-sans-ext -> [path]
let pierre = null;
let filterText = "";

// pierre warms up async; code views fall back to <pre> until/unless it works
try {
  pierre = new PierreFile({ theme: { dark: "pierre-dark", light: "pierre-light" } });
  preloadHighlighter?.({
    themes: ["pierre-dark", "pierre-light"],
    langs: ["markdown", "json", "javascript", "typescript", "rust", "python", "bash", "yaml", "css", "html", "diff", "sql", "toml"],
  }).catch?.(() => {});
} catch { pierre = null; }

function buildIndex() {
  byPath = new Map(files.map((f) => [f.path.toLowerCase(), f.path]));
  byName = new Map();
  for (const f of files) {
    const base = f.path.split("/").pop().replace(/\.[^.]+$/, "").toLowerCase();
    if (!byName.has(base)) byName.set(base, []);
    byName.get(base).push(f.path);
  }
}

function resolveWiki(target) {
  const t = target.trim().replace(/^\.\//, "").toLowerCase();
  if (byPath.has(t)) return byPath.get(t);
  if (byPath.has(t + ".md")) return byPath.get(t + ".md");
  const base = t.split("/").pop();
  const hits = byName.get(base);
  if (hits?.length) return hits[0];
  // partial suffix match (folders in link)
  const direct = files.find((f) => f.path.toLowerCase().endsWith("/" + t + ".md"));
  if (direct) return direct.path;
  return null;
}

const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

function wikify(html) {
  // [[target]] and [[target|display]] -> hash links
  html = html.replace(/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/g, (_, target, display) => {
    const p = resolveWiki(target);
    const label = display || target;
    return p
      ? `<a class="wl" href="#/${encodeURIComponent(p)}">${esc(label)}</a>`
      : `<span class="wl missing" title="unresolved">${esc(label)}</span>`;
  });
  // relative .md links -> hash routes
  html = html.replace(/href="([^"#]*?\.md)"/gi, (m, href) => {
    const p = resolveWiki(href);
    return p ? `href="#/${encodeURIComponent(p)}"` : m;
  });
  return html;
}

const fmtSize = (n) => (n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(1) + " KB" : (n / 1048576).toFixed(1) + " MB");
const fmtWhen = (ts) => {
  if (!ts) return "";
  const d = new Date(ts), s = (Date.now() - ts) / 1000;
  if (s < 60) return "just now";
  if (s < 3600) return Math.floor(s / 60) + "m ago";
  if (s < 86400) return Math.floor(s / 3600) + "h ago";
  if (s < 86400 * 7) return Math.floor(s / 86400) + "d ago";
  return d.toISOString().slice(0, 10);
};

// ---------- sidebar ----------
function renderTree() {
  const nav = $("#tree");
  nav.innerHTML = "";
  const visible = files.filter((f) => !filterText || f.path.toLowerCase().includes(filterText));
  // build nested tree from paths
  const root = { dirs: new Map(), files: [] };
  for (const f of visible) {
    const parts = f.path.split("/");
    let node = root;
    for (let i = 0; i < parts.length - 1; i++) {
      if (!node.dirs.has(parts[i])) node.dirs.set(parts[i], { dirs: new Map(), files: [] });
      node = node.dirs.get(parts[i]);
    }
    node.files.push(f);
  }
  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  };
  const fileLink = (f) => {
    const a = el("a", "file", f.path.split("/").pop().replace(/\.(md|markdown)$/i, ""));
    a.href = "#/" + encodeURIComponent(f.path);
    a.dataset.path = f.path;
    return a;
  };
  const draw = (node, into, depth) => {
    const dirs = [...node.dirs.entries()].sort((a, b) => a[0].localeCompare(b[0]));
    for (const [name, child] of dirs) {
      const d = el("details", "dir");
      d.open = depth < 1;
      const s = el("summary", null, name);
      s.style.paddingLeft = depth * 12 + 10 + "px";
      d.appendChild(s);
      draw(child, d, depth + 1);
      into.appendChild(d);
    }
    for (const f of node.files.sort((a, b) => a.path.localeCompare(b.path))) {
      const a = fileLink(f);
      a.style.paddingLeft = depth * 12 + 22 + "px";
      into.appendChild(a);
    }
  };
  draw(root, nav, 0);
  if (!visible.length) nav.appendChild(el("div", "empty", filterText ? "no matches" : "empty vault — drop files in the folder"));
}

function renderRecent() {
  const nav = $("#recent");
  const recent = files.filter((f) => f.updatedAt).sort((a, b) => b.updatedAt - a.updatedAt).slice(0, 12);
  nav.innerHTML = "";
  for (const f of recent) {
    const a = document.createElement("a");
    a.className = "file recent-item";
    a.href = "#/" + encodeURIComponent(f.path);
    const name = document.createElement("span");
    name.textContent = f.path.split("/").pop();
    const when = document.createElement("span");
    when.className = "when";
    when.textContent = fmtWhen(f.updatedAt);
    a.append(name, when);
    nav.appendChild(a);
  }
}

// ---------- content ----------
function shellNote(title, body) {
  $("#content").innerHTML = `<div class="note"><h1>${esc(title)}</h1>${body}</div>`;
  setActive();
  $("#main").scrollTop = 0;
}

function setActive() {
  const cur = decodeURIComponent(location.hash.replace(/^#\/?/, ""));
  document.querySelectorAll("#tree a.file").forEach((a) => a.classList.toggle("active", a.dataset.path === cur));
}

async function renderPierre(path, text, ext) {
  const box = document.createElement("div");
  box.className = "codefile";
  try {
    if (!pierre) throw new Error("pierre unavailable");
    const out = pierre.render({
      file: { content: text, path: path.split("/").pop(), language: LANG[ext] || "text" },
      containerWrapper: true,
    });
    const node = out.container || out.root || out.el;
    if (!node) throw new Error("no container");
    box.appendChild(node);
  } catch {
    const pre = document.createElement("pre");
    pre.className = "plaincode";
    pre.textContent = text;
    box.appendChild(pre);
  }
  $("#content").innerHTML = "";
  const h = document.createElement("div");
  h.className = "pathhead";
  h.textContent = path;
  const wrap = document.createElement("div");
  wrap.className = "note code";
  wrap.append(h, box);
  $("#content").appendChild(wrap);
  setActive();
}

async function open(path) {
  if (!path) {
    // landing: _index.md / README.md at root, else help
    const landing = byPath.get("_index.md") || byPath.get("readme.md") || byPath.get("index.md");
    if (landing) return open(landing);
    return shellNote("vault", `<p class="hint">Pick a file from the sidebar, or link pages with <code>[[wikilinks]]</code>.</p>`);
  }
  const f = files.find((x) => x.path === path);
  const ext = (path.match(/\.([a-z0-9]+)$/i) || [])[1]?.toLowerCase() || "";

  if (IMG.test(path)) {
    $("#content").innerHTML = `<div class="note"><div class="pathhead">${esc(path)}</div><img class="imgview" src="${withView("api/file?path=" + encodeURIComponent(path))}" alt=""></div>`;
    setActive();
    return;
  }
  const r = await fetch(withView("api/file?path=" + encodeURIComponent(path)));
  if (!r.ok) return shellNote(path, `<p class="hint">not found (404)</p>`);
  const text = await r.text();

  if (/\.(md|markdown)$/i.test(path)) {
    const stripped = text.replace(/^---\r?\n[\s\S]*?\r?\n---\r?\n/, "");
    const html = marked.parse(stripped);
    $("#content").innerHTML = `<div class="note md">${wikify(html)}</div>`;
    setActive();
    document.title = path.split("/").pop().replace(/\.md$/i, "") + " — vault";
    return;
  }
  if (TEXTIE.test(path) || f?.size < 200000) return renderPierre(path, text, ext);
  shellNote(path, `<p class="hint">binary file (${fmtSize(f?.size || 0)}) — <a href="${withView("api/file?path=" + encodeURIComponent(path))}">download</a></p>`);
}

function route() {
  open(decodeURIComponent(location.hash.replace(/^#\/?/, "")));
}

// hover prefetch: warm the HTTP cache before the click lands (responses
// carry cache-control, so the later navigation is instant)
const prefetched = new Set();
document.addEventListener("pointerover", (e) => {
  const a = e.target.closest && e.target.closest('a[href^="#/"]');
  if (!a) return;
  const p = decodeURIComponent(a.getAttribute("href").slice(2));
  if (p && !prefetched.has(p)) {
    prefetched.add(p);
    fetch(withView("api/file?path=" + encodeURIComponent(p))).catch(() => {});
  }
}, { passive: true });

async function boot() {
  try {
    const r = await fetch(withView("api/tree"));
    const v = await r.json();
    files = v.files || [];
  } catch (e) {
    showErr("api/tree: " + e);
    files = [];
  }
  buildIndex();
  renderTree();
  renderRecent();
  $("#filter").addEventListener("input", (e) => {
    filterText = e.target.value.trim().toLowerCase();
    renderTree();
  });
  window.addEventListener("hashchange", route);
  route();
}
boot();
