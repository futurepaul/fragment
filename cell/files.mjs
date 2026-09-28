// The files viewer, served by the platform on every fragment as __files
// (with ./__files.js and ./__files.css): the fragment's content files, on
// live and main, as a tree beside a reader, the way the notes template's
// vault viewer shows them, without its bundle. Markdown renders, and its
// [[wikilinks]] and relative links open here; any other text shows with
// line numbers; pictures, video, and sound play inline; anything else is a
// download. A file's text becomes DOM through textContent only (no
// innerHTML), as the chat's page renders an agent's: a file can never add
// markup or script to this origin, and HTML in one shows as text.
//
// A file opens by the hash (`#/notes/a.md`, or `#/notes/a.md#a-heading`),
// README.md or index.md at the root when none is named. The list is
// `__files` asked for JSON and the bytes `__file?path=`, as anyone who may
// see the fragment reads them; `__watch` (viewers and up) says when main
// moves, and the tree and the open file follow. Framed (a desktop's pane),
// the reader's bar also offers to open a file as a pane of its own: it asks
// the page around it, `{fragment: "open", url, title}`.

const ICON = {
  menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
  search: '<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/>',
  chevron: '<path d="m9 6 6 6-6 6"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  file: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>',
  text: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M9 13h6M9 17h4"/>',
  image: '<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="9" cy="10" r="1.6"/><path d="m21 16-5-5-9 9"/>',
  media: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="m10 9 5 3-5 3z"/>',
  raw: '<path d="M14 4h6v6M20 4l-9 9"/><path d="M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/>',
  pane: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M14 4v16"/>',
  download: '<path d="M12 4v11M7 10l5 5 5-5M5 20h14"/>',
};
const svg = (name) => `<svg viewBox="0 0 24 24" aria-hidden="true">${ICON[name]}</svg>`;

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
const slug = (text) => text.toLowerCase().trim().replace(/[^\p{L}\p{N}\s-]/gu, "").replace(/\s+/g, "-");

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

document.body.innerHTML = `
  <div class="files" id="files">
    <aside class="side">
      <div class="side-top">
        <div class="brand" id="brand"></div>
        <label class="filter">${svg("search")}<input id="filter" type="search" placeholder="Filter files" autocomplete="off" spellcheck="false" aria-label="Filter files"></label>
      </div>
      <nav class="tree" id="tree" aria-label="Files"></nav>
      <div class="side-foot" id="count"></div>
    </aside>
    <div class="scrim" id="scrim"></div>
    <main class="main">
      <header class="bar">
        <button class="icon menu" id="menu" type="button" title="Files" aria-label="Show the files">${svg("menu")}</button>
        <div class="crumbs" id="crumbs"></div>
        <span class="meta" id="meta"></span>
        <a class="icon" id="raw" target="_blank" rel="noopener" title="The file itself" aria-label="The file itself" hidden>${svg("raw")}</a>
        <button class="icon" id="pop" type="button" title="Open in a pane of its own" aria-label="Open in a pane of its own" hidden>${svg("pane")}</button>
      </header>
      <div class="reader" id="reader"></div>
    </main>
  </div>`;
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
      row.innerHTML = `<span class="chev">${svg("chevron")}</span>${svg("folder")}`;
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
      row.innerHTML = svg(ICON_OF[kind(f.path)]);
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
  a.innerHTML = svg("download");
  a.append("Download");
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

// ---- markdown → DOM, with textContent only: front matter dropped;
// headings, paragraphs, nested and task lists, quotes (and Obsidian's
// callouts), fenced code, rules, pipe tables; inline code, bold, italic,
// strikethrough, highlights, links, images, and [[wikilinks]] (![[embeds]]
// of pictures). A link to one of this fragment's files opens here; any
// other keeps only http(s) and mailto. ----

const FENCE = /^ {0,3}(`{3,}|~{3,})[ \t]*([^\s`]*)/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/;
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const QUOTE = /^ {0,3}>/;
const ITEM = /^( *)([-*+]|\d{1,9}[.)])(?:[ \t]+(.*))?$/;
const ROW_SEP = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;
const interrupts = (line) => FENCE.test(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line) || /^ {0,3}(?:[-*+]|1[.)])[ \t]+\S/.test(line);
const cells = (row) => row.trim().replace(/^\|/, "").replace(/(?<!\\)\|$/, "").split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, "|"));

function markdown(text, at) {
  const lines = text
    .replace(/\r\n?/g, "\n")
    .replace(/^---\n[\s\S]*?\n---(?:\n|$)/, "")
    .split("\n")
    .map((l) => l.replace(/^\t+/, (tabs) => "    ".repeat(tabs.length)));
  const doc = el("article", "doc");
  doc.append(blocks(lines, at));
  return doc;
}

// `tight`: a tight list item's paragraphs, without <p>.
function blocks(lines, at, tight = false) {
  const out = document.createDocumentFragment();
  for (let i = 0; i < lines.length; ) {
    const line = lines[i];
    let m;
    if (!line.trim()) {
      i++;
    } else if ((m = line.match(FENCE))) {
      const close = new RegExp(`^ {0,3}\\${m[1][0]}{${m[1].length},}[ \\t]*$`);
      const text = [];
      for (i++; i < lines.length && !close.test(lines[i]); i++) text.push(lines[i]);
      i++;
      const pre = el("pre");
      if (m[2]) pre.dataset.lang = m[2];
      pre.append(el("code", null, text.join("\n")));
      out.append(pre);
    } else if ((m = line.match(HEADING))) {
      out.append(heading(m[1].length, m[2] || "", at));
      i++;
    } else if (RULE.test(line)) {
      out.append(el("hr"));
      i++;
    } else if (QUOTE.test(line)) {
      const inner = [];
      for (; i < lines.length && (QUOTE.test(lines[i]) || (lines[i].trim() && inner.at(-1)?.trim() && !interrupts(lines[i]))); i++) inner.push(lines[i].replace(/^ {0,3}> ?/, ""));
      out.append(quote(inner, at));
    } else if ((m = line.match(ITEM)) && m[1].length < 4) {
      i = list(lines, i, out, at);
    } else if (line.includes("|") && ROW_SEP.test(lines[i + 1] || "") && cells(line).length === cells(lines[i + 1]).length) {
      const rows = [];
      for (; i < lines.length && lines[i].includes("|"); i++) rows.push(lines[i]);
      out.append(table(rows, at));
    } else {
      const para = [line.trimStart()];
      let level = 0;
      for (i++; i < lines.length && lines[i].trim(); i++) {
        level = /^ {0,3}=+[ \t]*$/.test(lines[i]) ? 1 : /^ {0,3}-+[ \t]*$/.test(lines[i]) ? 2 : 0;
        if (level) {
          i++;
          break;
        }
        if (interrupts(lines[i])) break;
        para.push(lines[i].trimStart());
      }
      const text = para.join("\n");
      out.append(level ? heading(level, text.trim(), at) : inline(tight ? document.createDocumentFragment() : el("p"), text.trimEnd(), at));
    }
  }
  return out;
}

function heading(level, text, at) {
  const h = inline(el(`h${level}`), text, at);
  h.dataset.anchor = slug(h.textContent);
  return h;
}

// A quote; `> [!note] Title` makes it one of Obsidian's callouts.
function quote(lines, at) {
  const bq = el("blockquote");
  const callout = lines[0].match(/^\[!([\w-]+)\][+-]?[ \t]*(.*)$/);
  if (callout) {
    bq.className = "callout";
    bq.dataset.kind = callout[1].toLowerCase();
    bq.append(inline(el("div", "callout-title"), callout[2] || callout[1], at));
    lines = lines.slice(1);
  }
  bq.append(blocks(lines, at));
  return bq;
}

// A list from line `i`, each item's lines handed to `blocks` (a nested list
// is an item's indented lines); returns the line after it. A blank line
// inside makes it loose: its items' paragraphs get <p>.
function list(lines, i, out, at) {
  const first = lines[i].match(ITEM);
  const indent = first[1].length;
  const ordered = /\d/.test(first[2]);
  const sibling = (m) => m && m[1].length < indent + 2;
  const items = [];
  let loose = false;
  let col = 0;
  for (; i < lines.length; i++) {
    const line = lines[i];
    const m = line.match(ITEM);
    const lead = line.length - line.trimStart().length;
    if (sibling(m) && !RULE.test(line)) {
      if (/\d/.test(m[2]) !== ordered) break;
      col = m[3] === undefined ? m[1].length + m[2].length + 1 : line.length - m[3].length;
      items.push([m[3] ?? ""]);
    } else if (!line.trim()) {
      const next = lines.slice(i + 1).find((l) => l.trim());
      const nm = next?.match(ITEM);
      if (next === undefined || !((sibling(nm) && /\d/.test(nm[2]) === ordered) || next.length - next.trimStart().length >= indent + 2)) break;
      loose = true;
      items.at(-1).push("");
    } else if (lead >= indent + 2) {
      items.at(-1).push(line.slice(Math.min(lead, col)));
    } else if (items.at(-1).at(-1)?.trim() && !interrupts(line)) {
      items.at(-1).push(line);
    } else {
      break;
    }
  }
  const ul = el(ordered ? "ol" : "ul");
  const start = parseInt(first[2], 10);
  if (ordered && start !== 1) ul.start = start;
  for (const item of items) {
    const li = el("li");
    const task = item[0].match(/^\[([ xX])\](?:[ \t]+|$)/);
    if (task) {
      const box = el("input");
      box.type = "checkbox";
      box.checked = task[1] !== " ";
      box.disabled = true;
      li.className = "task";
      li.append(box);
      item[0] = item[0].slice(task[0].length);
    }
    li.append(blocks(item, at, !loose));
    ul.append(li);
  }
  out.append(ul);
  return i;
}

function table(rows, at) {
  const align = cells(rows[1]).map((c) => (c.endsWith(":") ? (c.startsWith(":") ? "center" : "right") : ""));
  const t = el("table");
  rows.forEach((row, n) => {
    if (n === 1) return;
    const tr = el("tr");
    cells(row).forEach((c, k) => {
      const cell = inline(el(n ? "td" : "th"), c, at);
      if (align[k]) cell.style.textAlign = align[k];
      tr.append(cell);
    });
    t.append(tr);
  });
  const wrap = el("div", "table");
  wrap.append(t);
  return wrap;
}

// Inline spans, the first that starts earliest winning: `code`, [[a
// wikilink]] or ![[an embed]], ![an image](src), [a link](href "title"),
// <https://an.autolink>, a bare https:// URL, **bold** or __bold__,
// ~~struck~~, ==marked==, *italic* or _italic_, and a backslash's escape.
const INLINE = new RegExp(
  [
    /(?<ticks>`+)(?<code>[\s\S]*?[^`])\k<ticks>(?!`)/,
    /(?<embed>!?)\[\[(?<wiki>[^\]\n]+)\]\]/,
    /(?<bang>!?)\[(?<label>(?:[^\][\n]|\[[^\]\n]*\])*)\]\(\s*<?(?<href>[^\s()<>]*(?:\([^\s()<>]*\)[^\s()<>]*)*)>?(?:\s+"(?<title>[^"]*)")?\s*\)/,
    /<(?<auto>(?:https?:\/\/|mailto:)[^\s<>]+)>/,
    /(?<bare>https?:\/\/[^\s<>]*[^\s<>.,;:!?'")\]*_~])/,
    /\*\*(?<strong>[^\s*](?:[\s\S]*?[^\s*])?)\*\*/,
    /(?<![\w\\])__(?<strong2>[^\s_](?:[\s\S]*?[^\s_])?)__(?!\w)/,
    /~~(?<del>[^\s~](?:[\s\S]*?[^\s~])?)~~/,
    /==(?<mark>[^\s=](?:[\s\S]*?[^\s=])?)==/,
    /\*(?<em>[^\s*](?:[\s\S]*?[^\s*])?)\*/,
    /(?<![\w\\])_(?<em2>[^\s_](?:[\s\S]*?[^\s_])?)_(?!\w)/,
    /\\(?<escaped>[!-/:-@[-`{-~])/,
  ]
    .map((r) => r.source)
    .join("|"),
  "g",
);

// `linked`: inside a link's label, where a link is only its text.
function inline(parent, text, at, linked = false) {
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    if (m.index > last) plain(parent, text.slice(last, m.index));
    parent.append(span(m, at, linked));
    last = m.index + m[0].length;
  }
  if (last < text.length) plain(parent, text.slice(last));
  return parent;
}

// Text, with a hard break where a line ends in two spaces or a backslash.
function plain(parent, text) {
  text.split(/(?: {2,}|\\)\n/).forEach((part, n) => {
    if (n) parent.append(el("br"));
    parent.append(part);
  });
}

function span(m, at, linked) {
  const g = m.groups;
  // a link's label may hold a picture (a badge), never another link
  const linkish = g.wiki !== undefined || (g.href !== undefined && !g.bang) || g.auto !== undefined || g.bare !== undefined;
  if (g.code !== undefined) return el("code", null, g.code.replace(/\n/g, " ").replace(/^ (.+) $/, "$1"));
  if (linkish && linked) return document.createTextNode(m[0]);
  if (g.wiki !== undefined) return wikilink(g.wiki, g.embed, at);
  if (g.href !== undefined) return g.bang ? image(g.href, g.label, at) : link(g.href, g.label, g.title, at);
  if (g.auto !== undefined || g.bare !== undefined) return link(g.auto ?? g.bare, null, null, at);
  if (g.escaped !== undefined) return document.createTextNode(g.escaped);
  const [tag, inner] = [
    ["strong", g.strong ?? g.strong2],
    ["del", g.del],
    ["mark", g.mark],
    ["em", g.em ?? g.em2],
  ].find(([, v]) => v !== undefined);
  return inline(el(tag), inner, at, linked);
}

// Where a link goes: one of this fragment's files (from the linking file's
// folder, else the root; opened here), a heading of this one, or an http(s)
// or mailto URL. Anything else (javascript:, data:, a path that names no
// file here) is no link: null.
function destination(href, at) {
  // a browser drops controls and spaces from a URL: "java\tscript:" is javascript:
  const url = href.replace(/[\x00-\x20\x7f]/g, "");
  if (/^[a-z][a-z\d+.-]*:|^\/\//i.test(url)) return /^(?:https?:|mailto:|\/\/)/i.test(url) ? { url } : null;
  const [p, hash] = url.split("#");
  const anchor = hash ? decode(hash) : undefined;
  if (!p) return anchor ? { path: current, anchor } : null;
  const path = [resolve(at, decode(p)), resolve("", decode(p))]
    .flatMap((t) => [t.toLowerCase(), `${t.toLowerCase()}.md`])
    .map((t) => lower.get(t))
    .find(Boolean);
  return path ? { path, anchor } : null;
}

// A relative path from `at`, the linking file's folder ("" at the root).
function resolve(at, rel) {
  const parts = rel.startsWith("/") ? [] : at.split("/").filter(Boolean);
  for (const seg of rel.split("/")) {
    if (seg === "..") parts.pop();
    else if (seg && seg !== ".") parts.push(seg);
  }
  return parts.join("/");
}

// A [[wikilink]]'s file: its path from the root or from this file's folder,
// with or without .md, else a file of that name (this folder's first).
function wiki(target, at) {
  const t = target.trim().replace(/^\.?\//, "").toLowerCase();
  for (const p of [t, resolve(at, t).toLowerCase()]) {
    const path = lower.get(p) ?? lower.get(`${p}.md`);
    if (path) return path;
  }
  const hits = (byName.get(base(t)) || []).filter((p) => !t.includes("/") || p.toLowerCase().replace(/\.md$/, "").endsWith(t.replace(/\.md$/, "")));
  return hits.find((p) => folder(p) === at) ?? hits[0] ?? null;
}

function wikilink(inner, embed, at) {
  const [ref, label] = inner.split("|");
  const [name, anchor] = ref.split("#");
  const path = name.trim() ? wiki(name, at) : current;
  if (embed && path && kind(path) === "image") {
    const img = el("img");
    img.src = fileUrl(path);
    img.alt = name;
    if (/^\d+$/.test(label || "")) img.width = +label;
    return img;
  }
  const a = el(path ? "a" : "span", path ? "wikilink" : "wikilink missing", (label || ref).trim());
  if (path) internal(a, path, anchor);
  else a.title = `No file named ${name.trim()}`;
  return a;
}

function link(href, label, title, at) {
  const to = destination(href, at);
  const a = el(to ? "a" : "span", to ? null : "missing");
  if (label === null) a.textContent = href;
  else inline(a, label, at, true);
  if (title) a.title = title;
  if (to?.url) {
    a.href = to.url;
    a.target = "_blank";
    a.rel = "noopener noreferrer";
  } else if (to) {
    internal(a, to.path, to.anchor);
  }
  return a;
}

// A link the viewer opens: a file here, and a heading in it.
function internal(a, path, anchor) {
  a.href = route(path, anchor);
  a.dataset.path = path;
}

function image(src, alt, at) {
  const to = destination(src, at);
  const url = to?.path ? fileUrl(to.path) : to?.url && /^(?:https?:)?\/\//i.test(to.url) ? to.url : null;
  if (!url) return el("span", "missing", alt || src);
  const img = el("img");
  img.src = url;
  img.alt = alt;
  img.loading = "lazy";
  return img;
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
