// The notes viewer: a folder of notes as a tree beside a reader, routed by
// the hash (`#/<path>`). Markdown renders through the platform's renderer
// (../__markdown.js: DOM only, never markup, so HTML in a note shows as the
// text it is); [[wikilinks]] and relative links open here; other text shows
// as itself; pictures show inline; anything else is a download. The notes
// template's page and the brain's share it (the brain's site/assets is a
// symlink here), and brain.js beside it uses its ids (#tree, #content,
// #main) and draws a page again through its hashchange route.
//
// It reads the fragment's app: `api/tree` (the notes) and `api/file?path=`
// (one note's bytes), carrying the page's `?view=` token when it has one,
// and follows the room the page hands it (`window.fragment.room`), whose
// state says which files changed.
import { markdown, resolvePath, scheme, withoutFrontMatter } from "../__markdown.js";

const $ = (sel) => document.querySelector(sel);
function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

// self-diagnosis: a module or boot failure says so in the sidebar (as text),
// so a blank page is never silent
const showErr = (msg) => $("#tree")?.replaceChildren(el("div", "empty", `⚠ ${String(msg).slice(0, 300)}`));
window.addEventListener("error", (e) => showErr(e.message || e.error));
window.addEventListener("unhandledrejection", (e) => showErr(e.reason));

const qs = location.search || ""; // carries the ?view= token when present
const withView = (url) => url + (qs ? (url.includes("?") ? "&" : "?") + qs.slice(1) : "");
const fileUrl = (path) => withView("api/file?path=" + encodeURIComponent(path));
const hashOf = (path) => "#/" + encodeURIComponent(path);
const decode = (s) => {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
};
const folder = (path) => path.slice(0, Math.max(path.lastIndexOf("/"), 0));
const IMG = /\.(png|jpe?g|gif|webp|svg|ico)$/i;
// A file past this many bytes that is not named as text is offered as a download.
const SHOWN_MAX = 200_000;
const TEXTIE = /\.(md|markdown|txt|json|js|mjs|cjs|ts|tsx|jsx|rs|py|sh|bash|zsh|toml|yml|yaml|css|html|htm|xml|csv|log|diff|patch|sql|go|java|kt|rb|php|c|h|cpp|hpp|swift|lua|dockerfile)$/i;
// The recent list's length.
const RECENT_MAX = 12;

let files = []; // [{path, size, updatedAt, rev}]
let byPath = new Map(); // lower path -> path
let byName = new Map(); // lower basename, and without its extension -> [path]
let filterText = "";

function buildIndex() {
  byPath = new Map(files.map((f) => [f.path.toLowerCase(), f.path]));
  byName = new Map();
  for (const f of files) {
    const name = f.path.split("/").pop().toLowerCase();
    for (const key of new Set([name, name.replace(/\.[^.]+$/, "")])) {
      if (!byName.has(key)) byName.set(key, []);
      byName.get(key).push(f.path);
    }
  }
}

// A link's or a [[wikilink]]'s note: by its path, with or without .md, by
// its name, or by a path ending in it.
function resolveWiki(target) {
  const t = target.trim().replace(/^\.\//, "").toLowerCase();
  if (byPath.has(t)) return byPath.get(t);
  if (byPath.has(t + ".md")) return byPath.get(t + ".md");
  const hits = byName.get(t.split("/").pop());
  if (hits?.length) return hits[0];
  return files.find((f) => f.path.toLowerCase().endsWith("/" + t + ".md"))?.path ?? null;
}

// Where a note's links go, from the note's folder `at`: another note (by
// its path from that folder or the root, with or without .md, else by its
// name: opened here) or the web (http(s) and mailto); anything else is no
// link.
function linksFrom(at) {
  return (target, kind) => {
    if (kind === "wiki") {
      const path = resolveWiki(target.split("#")[0]);
      return path ? { href: hashOf(path), path } : null;
    }
    // the scheme as a browser reads it (the renderer's one reading)
    const s = scheme(target);
    if (s !== null) return ["http", "https", "mailto"].includes(s) ? { href: target } : null;
    const rel = decode(target.split("#")[0]);
    if (!rel) return null;
    const exact = [resolvePath(at, rel), resolvePath("", rel)].flatMap((p) => [p.toLowerCase(), `${p.toLowerCase()}.md`]).map((p) => byPath.get(p)).find(Boolean);
    const path = exact ?? resolveWiki(rel);
    return path ? { href: hashOf(path), path } : null;
  };
}

// A note's picture: one of the vault's (from the note's folder, else the
// root), or one on the web. A picture on the web is fetched by each
// reader's browser, so its host learns who opened the note and when (an
// editor can watch a note's readers this way); keeping pictures to the
// vault's own is Paul's call, not made here.
function picture(at) {
  return (src, kind) => {
    if (kind === "wiki") {
      const path = resolveWiki(src);
      return path && IMG.test(path) ? { src: fileUrl(path) } : null;
    }
    const s = scheme(src);
    if (s !== null) return ["http", "https"].includes(s) ? { src } : null;
    const path = [resolvePath(at, decode(src)), resolvePath("", decode(src))].map((p) => byPath.get(p.toLowerCase())).find(Boolean);
    return path && IMG.test(path) ? { src: fileUrl(path) } : null;
  };
}

const fmtSize = (n) => (n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(1) + " KB" : (n / 1048576).toFixed(1) + " MB");
const fmtWhen = (ts) => {
  if (!ts) return "";
  const s = (Date.now() - ts) / 1000;
  if (s < 60) return "just now";
  if (s < 3600) return Math.floor(s / 60) + "m ago";
  if (s < 86400) return Math.floor(s / 3600) + "h ago";
  if (s < 86400 * 7) return Math.floor(s / 86400) + "d ago";
  return new Date(ts).toISOString().slice(0, 10);
};

// ---------- sidebar ----------
function renderTree() {
  const nav = $("#tree");
  const visible = files.filter((f) => !filterText || f.path.toLowerCase().includes(filterText));
  // a nested tree from the paths
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
  const fileLink = (f) => {
    const a = el("a", "file", f.path.split("/").pop().replace(/\.(md|markdown)$/i, ""));
    a.href = hashOf(f.path);
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
      d.append(s);
      draw(child, d, depth + 1);
      into.append(d);
    }
    for (const f of node.files.sort((a, b) => a.path.localeCompare(b.path))) {
      const a = fileLink(f);
      a.style.paddingLeft = depth * 12 + 22 + "px";
      into.append(a);
    }
  };
  nav.replaceChildren();
  draw(root, nav, 0);
  if (!visible.length) nav.append(el("div", "empty", filterText ? "no matches" : "empty vault — drop files in the folder"));
}

function renderRecent() {
  const recent = files
    .filter((f) => f.updatedAt)
    .sort((a, b) => b.updatedAt - a.updatedAt)
    .slice(0, RECENT_MAX);
  $("#recent").replaceChildren(
    ...recent.map((f) => {
      const a = el("a", "file recent-item");
      a.href = hashOf(f.path);
      a.append(el("span", null, f.path.split("/").pop()), el("span", "when", fmtWhen(f.updatedAt)));
      return a;
    }),
  );
}

// ---------- content ----------
function show(note) {
  $("#content").replaceChildren(note);
  setActive();
}

// A page of the viewer's own: a title and a line of hint.
function shellNote(title, ...hint) {
  const note = el("div", "note");
  const p = el("p", "hint");
  p.append(...hint);
  note.append(el("h1", null, title), p);
  show(note);
  $("#main").scrollTop = 0;
}

function setActive() {
  const cur = decode(location.hash.replace(/^#\/?/, ""));
  document.querySelectorAll("#tree a.file").forEach((a) => a.classList.toggle("active", a.dataset.path === cur));
}

// A file headed by its path: a picture, its text, or a download.
function withHead(path, cls, body) {
  const note = el("div", cls);
  note.append(el("div", "pathhead", path), body);
  return note;
}

let opening = 0; // the latest open: an earlier read's answer is dropped

async function open(path) {
  const n = ++opening;
  if (!path) {
    // landing: _index.md / README.md at the root, else a hint
    const landing = byPath.get("_index.md") || byPath.get("readme.md") || byPath.get("index.md");
    if (landing) return open(landing);
    const code = el("code", null, "[[wikilinks]]");
    return shellNote("vault", "Pick a file from the sidebar, or link pages with ", code, ".");
  }
  const f = files.find((x) => x.path === path);
  if (IMG.test(path)) {
    const img = el("img", "imgview");
    img.src = fileUrl(path);
    img.alt = "";
    return show(withHead(path, "note", img));
  }
  const r = await fetch(fileUrl(path));
  if (n !== opening) return;
  if (!r.ok) return shellNote(path, `not found (${r.status})`);
  const text = await r.text();
  if (n !== opening) return;
  if (/\.(md|markdown)$/i.test(path)) {
    const note = el("div", "note md");
    note.append(markdown(withoutFrontMatter(text), { resolve: linksFrom(folder(path)), image: picture(folder(path)) }));
    show(note);
    document.title = path.split("/").pop().replace(/\.md$/i, "") + " — vault";
    return;
  }
  if (TEXTIE.test(path) || (f && f.size < SHOWN_MAX)) {
    const pre = el("pre", "plaincode", text);
    return show(withHead(path, "note code", pre));
  }
  const download = el("a", null, "download");
  download.href = fileUrl(path);
  shellNote(path, `binary file (${fmtSize(f?.size || 0)}) — `, download);
}

function route() {
  open(decode(location.hash.replace(/^#\/?/, "")));
}

// hover prefetch: warm the HTTP cache before the click lands
const prefetched = new Set();
document.addEventListener(
  "pointerover",
  (e) => {
    const a = e.target.closest?.('a[href^="#/"]');
    if (!a) return;
    const p = decode(a.getAttribute("href").slice(2));
    if (p && !prefetched.has(p)) {
      prefetched.add(p);
      fetch(fileUrl(p)).catch(() => {});
    }
  },
  { passive: true },
);

// live updates: the room's state names the files that changed; the tree
// refreshes in place, and the open note again if it was one of them
let lastPing = 0;
let refreshTimer = null;
function scheduleRefresh(paths) {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(async () => {
    try {
      const r = await fetch(withView("api/tree"), { cache: "reload" });
      const v = await r.json();
      files = v.files || [];
      buildIndex();
      renderTree();
      renderRecent();
      const cur = decode(location.hash.replace(/^#\/?/, ""));
      if (cur && paths.includes(cur)) route();
      else setActive();
    } catch {}
  }, 60);
}
try {
  const room = window.fragment && window.fragment.room("vault");
  room.on("hello", (h) => {
    if (h?.state?.at) lastPing = h.state.at;
  });
  room.on("state", (v) => {
    if (!v || typeof v.at !== "number" || v.at <= lastPing) return;
    lastPing = v.at;
    scheduleRefresh(Array.isArray(v.paths) ? v.paths : []);
  });
} catch {}

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
