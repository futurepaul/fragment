// The files viewer, served by the platform on every fragment as __files
// (with ./__files.js and ./__files.css): the fragment's content files, on
// live and main, as a tree beside a reader, the way the notes template's
// vault viewer shows them. Markdown renders through the platform's renderer
// (./__markdown.js), and its [[wikilinks]] and relative links open here; any
// other text shows with line numbers; pictures, video, and sound play
// inline; anything else is a download. A file's text becomes DOM through
// the DOM's constructors only, never as markup, as the chat's page renders
// an agent's: a file can never add markup or script to this origin, and
// HTML in one shows as text (xtask's check refuses an HTML sink here).
//
// A file opens by the hash (`#/notes/a.md`, or `#/notes/a.md#a-heading`),
// README.md or index.md at the root when none is named. The list is
// `__files` asked for JSON and the bytes `__file?path=`, as anyone who may
// see the fragment reads them; `__watch` (viewers and up) says when main
// moves, and the tree and the open file follow. Framed (a pane of a page),
// the reader's bar also offers to open a file as a pane of its own: it asks
// the page around it, `{fragment: "open", url, title}`.

import { markdown as render, resolvePath, scheme, slug, withoutFrontMatter } from "./__markdown.js";

// Each icon's shapes, on a 24-unit grid (files.css strokes them).
const ICON = {
  menu: [["path", { d: "M4 7h16M4 12h16M4 17h16" }]],
  search: [["circle", { cx: 11, cy: 11, r: 6.5 }], ["path", { d: "m20 20-4.2-4.2" }]],
  chevron: [["path", { d: "m9 6 6 6-6 6" }]],
  folder: [["path", { d: "M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" }]],
  file: [["path", { d: "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" }], ["path", { d: "M14 3v5h5" }]],
  text: [["path", { d: "M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" }], ["path", { d: "M14 3v5h5M9 13h6M9 17h4" }]],
  image: [["rect", { x: 3, y: 4, width: 18, height: 16, rx: 2 }], ["circle", { cx: 9, cy: 10, r: 1.6 }], ["path", { d: "m21 16-5-5-9 9" }]],
  media: [["rect", { x: 3, y: 4, width: 18, height: 16, rx: 2 }], ["path", { d: "m10 9 5 3-5 3z" }]],
  raw: [["path", { d: "M14 4h6v6M20 4l-9 9" }], ["path", { d: "M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4" }]],
  pane: [["rect", { x: 3, y: 4, width: 18, height: 16, rx: 2 }], ["path", { d: "M14 4v16" }]],
  download: [["path", { d: "M12 4v11M7 10l5 5 5-5M5 20h14" }]],
};
const SVG_NS = "http://www.w3.org/2000/svg";
// An icon as DOM: an <svg> of its shapes, hidden from assistive technology.
function icon(name) {
  const shapes = ICON[name];
  if (!shapes) throw new Error(`files: no icon ${name}`);
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  for (const [tag, attrs] of shapes) {
    const shape = document.createElementNS(SVG_NS, tag);
    for (const [key, value] of Object.entries(attrs)) shape.setAttribute(key, String(value));
    svg.append(shape);
  }
  return svg;
}

// What a file is, by its extension; "text" is a guess the bytes confirm.
const KINDS = {
  markdown: "md markdown",
  image: "png jpg jpeg gif webp avif svg ico bmp",
  video: "mp4 webm mov m4v",
  audio: "mp3 wav ogg oga m4a flac aac",
  binary: "pdf zip gz tgz tar wasm woff woff2 ttf otf sqlite db bin exe dmg psd",
};
const KIND = Object.fromEntries(Object.entries(KINDS).flatMap(([kind, exts]) => exts.split(" ").map((e) => [e, kind])));
const kind = (path) => KIND[(path.match(/\.([^./]+)$/) || [])[1]?.toLowerCase()] || "text";
const ICON_OF = { markdown: "text", image: "image", video: "media", audio: "media", binary: "file", text: "text" };

// A text file past this is a download; one up to the second is read on hover.
const TEXT_MAX = 1024 * 1024;
const PREFETCH_MAX = 256 * 1024;
// Past this many files, folders start closed.
const OPEN_FOLDERS_MAX = 100;

const $ = (id) => document.getElementById(id);
function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}
// An element with its attributes and children: the page is built of these.
function h(tag, attrs, ...children) {
  const e = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (key === "class") e.className = value;
    else if (key === "hidden") e.hidden = value;
    else e.setAttribute(key, value);
  }
  e.append(...children);
  return e;
}
const decode = (s) => {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
};
const fileUrl = (path) => `__file?path=${encodeURIComponent(path)}`;
const route = (path, anchor) => `#/${path.split("/").map(encodeURIComponent).join("/")}${anchor ? `#${encodeURIComponent(anchor)}` : ""}`;
const folder = (path) => path.slice(0, Math.max(path.lastIndexOf("/"), 0));
const base = (path) => path.slice(path.lastIndexOf("/") + 1);
const bytes = (n) => (n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1048576).toFixed(1)} MB`);

const fragmentName = document.body.dataset.fragment || "";
const framed = parent !== window;

let files = [];                // [{path, size}], by path
let index = new Map();         // path -> its row
let lower = new Map();         // lowercased path -> path
let byName = new Map();        // lowercased file name, and without .md -> [path]
const opened = new Set();      // the folders shown open
let current = null;            // the file in the reader
let reading = 0;               // the latest open: an earlier read's answer is dropped
const texts = new Map();       // a hovered file's read, taken by its open

// ---- the page ----

document.body.replaceChildren(
  h("div", { class: "files", id: "files" },
    h("aside", { class: "side" },
      h("div", { class: "side-top" },
        h("div", { class: "brand", id: "brand" }),
        h("label", { class: "filter" }, icon("search"), h("input", { id: "filter", type: "search", placeholder: "Filter files", autocomplete: "off", spellcheck: "false", "aria-label": "Filter files" })),
      ),
      h("nav", { class: "tree", id: "tree", "aria-label": "Files" }),
      h("div", { class: "side-foot", id: "count" }),
    ),
    h("div", { class: "scrim", id: "scrim" }),
    h("main", { class: "main" },
      h("header", { class: "bar" },
        h("button", { class: "icon menu", id: "menu", type: "button", title: "Files", "aria-label": "Show the files" }, icon("menu")),
        h("div", { class: "crumbs", id: "crumbs" }),
        h("span", { class: "meta", id: "meta" }),
        h("a", { class: "icon", id: "raw", target: "_blank", rel: "noopener", title: "The file itself", "aria-label": "The file itself", hidden: true }, icon("raw")),
        h("button", { class: "icon", id: "pop", type: "button", title: "Open in a pane of its own", "aria-label": "Open in a pane of its own", hidden: true }, icon("pane")),
      ),
      h("div", { class: "reader", id: "reader" }),
    ),
  ),
);
$("brand").textContent = fragmentName;
const drawer = (open) => $("files").classList.toggle("nav", open);
const narrow = () => matchMedia("(max-width: 640px)").matches;

// ---- the list and the tree ----

async function load() {
  const r = await fetch("__files", { headers: { accept: "application/json" } });
  const body = await r.json().catch(() => null);
  if (!r.ok) throw new Error(body?.message || `http ${r.status}`);
  files = body.files;
  index = new Map(files.map((f) => [f.path, f]));
  lower = new Map(files.map((f) => [f.path.toLowerCase(), f.path]));
  byName = new Map();
  for (const { path } of files) {
    const name = base(path).toLowerCase();
    for (const key of new Set([name, name.replace(/\.md$/, "")])) byName.set(key, [...(byName.get(key) || []), path]);
  }
  $("count").textContent = `${files.length} ${files.length === 1 ? "file" : "files"}`;
  tree();
}

function tree() {
  const q = $("filter").value.trim().toLowerCase();
  const shown = q ? files.filter((f) => f.path.toLowerCase().includes(q)) : files;
  const root = { dirs: new Map(), files: [] };
  for (const f of shown) {
    let node = root;
    for (const part of f.path.split("/").slice(0, -1)) {
      if (!node.dirs.has(part)) node.dirs.set(part, { dirs: new Map(), files: [] });
      node = node.dirs.get(part);
    }
    node.files.push(f);
  }
  const rows = [];
  const walk = (node, prefix, depth) => {
    for (const [name, child] of [...node.dirs].sort((a, b) => a[0].localeCompare(b[0], undefined, { numeric: true }))) {
      const path = prefix + name;
      const open = !!q || opened.has(path);
      const row = el("button", "row dir");
      row.type = "button";
      row.dataset.dir = path;
      row.setAttribute("aria-expanded", String(open));
      row.append(h("span", { class: "chev" }, icon("chevron")), icon("folder"));
      row.append(el("span", "name", name));
      row.style.setProperty("--depth", depth);
      rows.push(row);
      if (open) walk(child, `${path}/`, depth + 1);
    }
    for (const f of node.files) {
      const row = el("a", `row file${f.path === current ? " active" : ""}`);
      row.href = route(f.path);
      row.dataset.path = f.path;
      row.title = `${f.path} · ${bytes(f.size)}`;
      row.append(icon(ICON_OF[kind(f.path)]));
      row.append(el("span", "name", base(f.path)));
      row.style.setProperty("--depth", depth);
      rows.push(row);
    }
  };
  walk(root, "", 0);
  $("tree").replaceChildren(...(rows.length ? rows : [el("div", "none", q ? "No file matches" : "No files yet")]));
}

// The open file's row, its folders opened, in view.
function mark() {
  if (current) {
    const parts = current.split("/");
    for (let n = 1; n < parts.length; n++) opened.add(parts.slice(0, n).join("/"));
  }
  tree();
  document.querySelector("#tree .row.active")?.scrollIntoView({ block: "nearest" });
}

$("tree").addEventListener("click", (e) => {
  const dir = e.target.closest(".row.dir");
  if (dir) {
    const path = dir.dataset.dir;
    if (!opened.delete(path)) opened.add(path);
    tree();
  } else if (e.target.closest(".row.file")) {
    drawer(false);
  }
});
$("filter").addEventListener("input", tree);
$("filter").addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    const first = document.querySelector("#tree .row.file");
    if (first) location.hash = first.getAttribute("href");
  } else if (e.key === "Escape") {
    e.target.value = "";
    tree();
  }
});
addEventListener("keydown", (e) => {
  if (e.key === "/" && !e.target.closest?.("input, textarea")) {
    e.preventDefault();
    drawer(true);
    $("filter").focus();
  }
});
$("menu").onclick = () => drawer(!$("files").classList.contains("nav"));
$("scrim").onclick = () => drawer(false);

// ---- reading ----

// A file's text, or null when its bytes are not text (a NUL early on, or not UTF-8).
async function read(path) {
  const r = await fetch(fileUrl(path));
  if (!r.ok) throw new Error((await r.json().catch(() => null))?.message || `http ${r.status}`);
  const data = new Uint8Array(await r.arrayBuffer());
  if (data.subarray(0, 8000).includes(0)) return null;
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(data);
  } catch {
    return null;
  }
}

// A hovered file's read, taken once (a later open reads again), or a fresh one.
function take(path) {
  const pending = texts.get(path) || read(path);
  texts.delete(path);
  return pending;
}

document.addEventListener("pointerover", (e) => {
  const f = index.get(e.target.closest?.("a[data-path]")?.dataset.path);
  if (!f || f.path === current || texts.has(f.path) || f.size > PREFETCH_MAX || !["markdown", "text"].includes(kind(f.path))) return;
  if (texts.size >= 32) texts.delete(texts.keys().next().value);
  const pending = read(f.path);
  pending.catch(() => {});
  texts.set(f.path, pending);
}, { passive: true });

function bar(path, f) {
  const crumbs = $("crumbs");
  crumbs.replaceChildren();
  if (path) {
    path.split("/").forEach((part, n, all) => {
      if (n) crumbs.append(el("span", "sep", "/"));
      crumbs.append(el(n === all.length - 1 ? "strong" : "span", null, part));
    });
  }
  $("meta").textContent = f ? bytes(f.size) : "";
  $("raw").hidden = !f;
  if (f) {
    $("raw").href = fileUrl(path);
    $("raw").toggleAttribute("download", kind(path) === "binary");
  }
  $("pop").hidden = !(f && framed);
  document.title = path ? `${base(path)} · ${fragmentName}` : `${fragmentName}: files`;
}

function say(title, text) {
  const box = el("div", "say");
  box.append(el("h2", null, title));
  if (text) box.append(el("p", null, text));
  return box;
}

function download(f, why) {
  const box = say(base(f.path), [why, `${bytes(f.size)}.`].filter(Boolean).join(" "));
  const a = el("a", "button");
  a.href = fileUrl(f.path);
  a.download = base(f.path);
  a.append(icon("download"), "Download");
  box.append(a);
  return box;
}

// Opens `path` in the reader; `keep` holds its place (a live refresh).
async function open(path, anchor, keep) {
  const n = ++reading;
  const reader = $("reader");
  const top = keep ? reader.scrollTop : 0;
  current = path;
  const f = index.get(path);
  bar(path, f);
  mark();
  drawer(false);
  let view;
  if (!f) {
    view = say("No such file", `${path} is not in ${fragmentName}.`);
  } else if (kind(path) === "image") {
    view = el("div", "media");
    const img = el("img");
    img.src = fileUrl(path);
    img.alt = path;
    img.onload = () => {
      if (current === path) $("meta").textContent = `${img.naturalWidth} × ${img.naturalHeight} · ${bytes(f.size)}`;
    };
    view.append(img);
  } else if (kind(path) === "video" || kind(path) === "audio") {
    view = el("div", "media");
    const player = el(kind(path));
    player.src = fileUrl(path);
    player.controls = true;
    player.preload = "metadata";
    view.append(player);
  } else if (kind(path) === "binary") {
    view = download(f);
  } else if (f.size > TEXT_MAX) {
    view = download(f, "Too large to show here.");
  } else {
    const text = await take(path).catch((e) => e);
    if (n !== reading) return;
    if (text instanceof Error) view = say("This file could not be read", text.message);
    else if (text === null) view = download(f);
    else if (kind(path) === "markdown") view = markdown(text, folder(path));
    else view = code(text);
  }
  reader.replaceChildren(view);
  reader.scrollTop = top;
  if (anchor) reach(anchor);
}

function reach(anchor) {
  document.querySelector(`#reader [data-anchor="${CSS.escape(slug(anchor))}"]`)?.scrollIntoView({ block: "start" });
}

function landing() {
  current = null;
  bar(null);
  mark();
  const n = files.length;
  $("reader").replaceChildren(
    n ? say("Pick a file", `${n} ${n === 1 ? "file" : "files"} in ${fragmentName}.`) : say("No files yet", `Nothing is in ${fragmentName} yet.`),
  );
  if (n && narrow()) drawer(true);
}

function go() {
  const [p, anchor] = location.hash.replace(/^#\/?/, "").split("#");
  const path = decode(p) || ["readme.md", "index.md"].map((name) => lower.get(name)).find(Boolean);
  if (!path) return landing();
  if (path === current && anchor) return reach(decode(anchor));
  open(path, anchor && decode(anchor));
}

$("reader").addEventListener("click", (e) => {
  const a = e.target.closest("a[href^='#/']");
  if (a && a.getAttribute("href") === location.hash) {
    // the hash is already this: no hashchange comes
    e.preventDefault();
    go();
  }
});
$("pop").onclick = () => {
  if (current) parent.postMessage({ fragment: "open", url: new URL(fileUrl(current), location.href).href, title: current }, "*");
};

// A text file, a line to a row, numbered by CSS (a copy takes the text alone).
function code(text) {
  const pre = el("pre", "code");
  const lines = text.replace(/\r?\n$/, "").split(/\r?\n/);
  pre.style.setProperty("--digits", String(lines.length).length);
  for (const line of lines) pre.append(el("span", "line", line));
  return pre;
}

// ---- following main ----

// Main's moves, for viewers and up. A feed that never opened was refused
// (a visitor below viewer), and the page keeps what it loaded; one that
// closes after opening comes back, and catches up on what it missed.
function watch(wait = 0) {
  const url = new URL("__watch", location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(url);
  let heard = false;
  ws.onmessage = (e) => {
    const frame = JSON.parse(e.data);
    if (frame.type === "hello" && wait) changed(null);
    if (frame.type === "changed") changed(frame.paths);
    heard = true;
  };
  ws.onclose = (e) => {
    if (heard && e.code !== 4003 && e.code !== 4004) setTimeout(() => watch(Math.min((wait || 500) * 2, 30000)), wait || 1000);
  };
}

// Changed paths (null: any), one refresh for a burst.
let stale = new Set();
let every = false;
let refreshing = null;
function changed(paths) {
  if (paths) paths.forEach((p) => stale.add(p));
  else every = true;
  for (const p of paths || [...texts.keys()]) texts.delete(p);
  clearTimeout(refreshing);
  refreshing = setTimeout(refresh, 150);
}

async function refresh() {
  const again = current && (every || stale.has(current));
  stale = new Set();
  every = false;
  await load().catch(() => {});
  if (again) open(current, null, true);
  else if (!current) go();
}

// ---- markdown, through the platform's renderer: its links and pictures
// are this fragment's files (from the linking file's folder, else the
// root; opened here) or the web's (http(s) and mailto), and nothing else.
// A picture on the web is fetched by each reader's browser, so its host
// learns who opened the file and when (a note's author can watch its
// readers this way); keeping pictures to the fragment's own files is
// Paul's call, not made here ----

function markdown(text, at) {
  const doc = el("article", "doc");
  doc.append(
    render(withoutFrontMatter(text), {
      resolve: (target, how) => (how === "wiki" ? wikiTarget(target, at) : destination(target, at)),
      image: (src, how) => picture(src, how, at),
    }),
  );
  return doc;
}

// Where a link goes: one of this fragment's files, a heading of this one,
// or an http(s) or mailto URL; anything else (javascript:, data:, a path
// that names no file here) is no link: null.
function destination(href, at) {
  // the scheme as a browser reads it (the renderer's one reading)
  const s = scheme(href);
  if (s !== null) return ["http", "https", "mailto"].includes(s) ? { href } : null;
  const [p, hash] = href.split("#");
  const anchor = hash ? decode(hash) : undefined;
  if (!p) return anchor && current ? internal(current, anchor) : null;
  const path = [resolvePath(at, decode(p)), resolvePath("", decode(p))]
    .flatMap((t) => [t.toLowerCase(), `${t.toLowerCase()}.md`])
    .map((t) => lower.get(t))
    .find(Boolean);
  return path ? internal(path, anchor) : null;
}

// A link the viewer opens: a file here, and a heading in it.
const internal = (path, anchor) => ({ href: route(path, anchor), path });

// A [[wikilink]]'s file: its path from the root or from this file's folder,
// with or without .md, else a file of that name (this folder's first).
function wiki(target, at) {
  const t = target.trim().replace(/^\.?\//, "").toLowerCase();
  for (const p of [t, resolvePath(at, t).toLowerCase()]) {
    const path = lower.get(p) ?? lower.get(`${p}.md`);
    if (path) return path;
  }
  const hits = (byName.get(base(t)) || []).filter((p) => !t.includes("/") || p.toLowerCase().replace(/\.md$/, "").endsWith(t.replace(/\.md$/, "")));
  return hits.find((p) => folder(p) === at) ?? hits[0] ?? null;
}

// [[name#heading]]: the file it names (this one, with no name) and the heading.
function wikiTarget(ref, at) {
  const [name, anchor] = ref.split("#");
  const path = name.trim() ? wiki(name, at) : current;
  return path ? internal(path, anchor?.trim() || undefined) : null;
}

// A picture's URL: one of this fragment's pictures, or one on the web.
function picture(src, how, at) {
  if (how === "wiki") {
    const path = wiki(src, at);
    return path && kind(path) === "image" ? { src: fileUrl(path) } : null;
  }
  const to = destination(src, at);
  if (to?.path) return { src: fileUrl(to.path) };
  return to && ["http", "https"].includes(scheme(to.href)) ? { src: to.href } : null;
}

// ---- start ----

try {
  await load();
  // a small fragment's top folders start open
  if (files.length <= OPEN_FOLDERS_MAX) for (const { path } of files) if (path.includes("/")) opened.add(path.slice(0, path.indexOf("/")));
} catch (e) {
  $("tree").replaceChildren(el("div", "none", `The files could not be listed: ${e.message}`));
}
addEventListener("hashchange", go);
go();
watch();
