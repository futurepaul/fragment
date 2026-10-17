// The platform's markdown renderer, served on every fragment as
// ./__markdown.js: markdown to DOM through the DOM's own constructors and
// setters, never through markup, so the text of a file or a message can add
// no element, attribute or script to the page's origin. HTML in it shows as
// the text it is (a `<br>` is a line break, an HTML comment block is left
// out, a character reference is its character). The files viewer
// (`__files`), the chat template and the notes viewer render with it; any
// page may.
//
// It renders headings (each with a `data-anchor`, its `slug`), paragraphs,
// lists (ordered, nested, tight or loose, and tasks), quotes and Obsidian's
// callouts, fenced and indented code, rules and pipe tables; inline code,
// bold, italic, strikethrough, highlights, links (with titles, `<a path
// with spaces>`, and reference links with their definitions), autolinks,
// bare URLs and emails, pictures, [[wikilinks]] and ![[embeds]], hard
// breaks and a backslash's escapes.
//
// Its caller decides three things (`options`):
// - `resolve(target, kind)`: where a link goes, `kind` "link" (a
//   [label](target), an <autolink>, a bare URL or email) or "wiki" (a
//   [[target]]). It answers `{href, path?}` (`path`: one of the page's own
//   files, which the page opens itself; without it the link opens in a
//   tab), or null: no link, its words shown as `md-missing`. Unset: http(s)
//   and mailto links only.
// - `image(src, kind)`: a picture's `{src, href?}` (`href`: the picture is
//   a link too), or null: its words shown instead. Unset: no pictures.
// - `code(text, lang)`: a code block's node. Unset: `<pre><code>`.
// and `breaks`: every newline in a paragraph breaks its line, as in a chat
// (else only a hard break does, two spaces or a backslash before it).
//
// Whatever a hook answers, the renderer holds every link to http(s), mailto
// or a relative URL and every picture to http(s) or a relative URL: no
// hook's mistake makes a `javascript:` link. Its own elements are a fixed
// set (`ELEMENTS`), and it sets only their plain properties. A page that
// resolves links itself reads a URL's scheme with `scheme`, as the
// renderer does, and a relative path with `resolvePath`.
//
// Its time is linear in its text: no pattern here backtracks over a run of
// one character (a heading's closing marks are found by string operations,
// a table's separator row by its cells), inline spans run only on text of
// at most INLINE_MAX_CHARS, and a table is at most COLUMNS_MAX columns and
// CELLS_MAX cells. xtask's markdown_dom test times hostile text against it.

/// Nesting (quotes, lists, emphasis) stops here: deeper text shows as text.
export const DEPTH_MAX = 32;
/// A paragraph (or a heading, a cell) longer than this shows as plain text:
/// inline spans are lazy patterns, so their time on a hostile run of
/// delimiters grows with the square of the text they read.
export const INLINE_MAX_CHARS = 4096;
/// Text longer than this shows as itself, in one block, unparsed.
export const TEXT_MAX_CHARS = 1 << 20;
/// A table wider than this is no table; one of more cells than this shows
/// as its text (each row makes a cell per column, whatever it holds).
export const COLUMNS_MAX = 64;
export const CELLS_MAX = 8192;
/// Reference definitions a text may give (`[label]: url`).
export const REFS_MAX = 1024;
/// The elements the renderer makes (its unit test holds it to them).
export const ELEMENTS = [
  "p", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "input", "blockquote", "div", "pre", "code", "hr",
  "table", "thead", "tbody", "tr", "th", "td", "a", "span", "img", "strong", "em", "del", "mark", "br",
];

const LINK_SCHEMES = ["http", "https", "mailto"];
const SRC_SCHEMES = ["http", "https"];

const FENCE = /^ {0,3}(`{3,}|~{3,})[ \t]*([^\s`]*)/;
const FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
// a heading's opening marks; its words and closing marks are read by `headingOf`
const HEADING_OPEN = /^ {0,3}(#{1,6})(?=[ \t]|$)/;
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const QUOTE = /^ {0,3}>/;
const ITEM = /^( *)([-*+]|\d{1,9}[.)])(?:[ \t]+(.*))?$/;
const SEP_CELL = /^:?-+:?$/;
const COMMENT_OPEN = /^ {0,3}<!--/;
const DEFINITION = /^ {0,3}\[([^\[\]\n]{1,999})\]:[ \t]*(?:<([^<>\n]*)>|([^\s<>]+))(?:[ \t]+"([^"\n]*)")?[ \t]*$/;
const FRONT_MATTER = /^---\n[\s\S]*?\n---(?:\n|$)/;
const ENTITY = /&(?:#(\d{1,7})|#[xX]([0-9a-fA-F]{1,6})|([a-zA-Z]{2,8}));/g;
const BREAK_TAG = /<br\s*\/?>/i;
const NAMED = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ", copy: "©", reg: "®", trade: "™", hellip: "…",
  mdash: "—", ndash: "–", lsquo: "‘", rsquo: "’", ldquo: "“", rdquo: "”", laquo: "«", raquo: "»", middot: "·",
  bull: "•", times: "×", divide: "÷", deg: "°", plusmn: "±", para: "¶", sect: "§", euro: "€", pound: "£", yen: "¥", cent: "¢",
};
const interrupts = (line) => FENCE.test(line) || HEADING_OPEN.test(line) || RULE.test(line) || QUOTE.test(line) || /^ {0,3}(?:[-*+]|1[.)])[ \t]+\S/.test(line);

// Inline spans, the one that starts earliest winning (and of those the
// first listed): `code`, [[a wikilink]] or ![[an embed]], ![a picture](src),
// [a link](href "title") or (<a href with spaces>), [a link][its
// reference], <https://an.autolink> or <an@email>, a bare https:// URL or
// email, **bold** or __bold__, ~~struck~~, ==marked==, *italic* or
// _italic_, and a backslash's escape. A wikilink's target and a label's
// nested brackets stop at the next `[`, so a run of brackets is read once.
const INLINE = new RegExp(
  [
    /(?<ticks>`+)(?<code>[\s\S]*?[^`])\k<ticks>(?!`)/,
    /(?<embed>!?)\[\[(?<wiki>[^\[\]\n]+)\]\]/,
    /(?<bang>!?)\[(?<label>(?:[^\][\n]|\[[^\[\]\n]*\])*)\]\(\s*(?:<(?<hrefa>[^<>\n]*)>|(?<href>[^\s()<>]*(?:\([^\s()<>]*\)[^\s()<>]*)*))(?:\s+"(?<title>[^"]*)")?\s*\)/,
    /(?<rbang>!?)\[(?<rtext>(?:[^\][\n]|\[[^\[\]\n]*\])*)\]\[(?<rref>[^\[\]\n]{0,999})\]/,
    /<(?<auto>(?:https?:\/\/|mailto:)[^\s<>]+)>/,
    /<(?<automail>[\w.+-]+@[\w-]+(?:\.[\w-]+)*\.[a-zA-Z]{2,})>/,
    /(?<bare>https?:\/\/[^\s<>]*[^\s<>.,;:!?'")\]*_~])/,
    /(?<![\w.+-])(?<mail>[\w.+-]+@[\w-]+(?:\.[\w-]+)*\.[a-zA-Z]{2,})(?![\w-])/,
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

/// `text` as DOM: a DocumentFragment of its blocks.
export function markdown(text, options = {}) {
  const o = settled(options);
  const all = String(text ?? "").replace(/\r\n?/g, "\n");
  if (all.length > TEXT_MAX_CHARS) {
    const out = document.createDocumentFragment();
    out.append(el("pre", null, all));
    return out;
  }
  const lines = all.split("\n").map((l) => l.replace(/^\t+/, (tabs) => "    ".repeat(tabs.length)));
  o.refs = definitions(lines);
  return blocks(lines, o, 0, false);
}

/// `text`'s inline spans, appended to `parent` (a line of a card, a quote).
export function inline(parent, text, options = {}) {
  return spans(parent, String(text ?? ""), settled(options), 0, false);
}

/// A heading's anchor: its words, lower case, dashed.
export const slug = (text) => text.toLowerCase().trim().replace(/[^\p{L}\p{N}\s-]/gu, "").replace(/\s+/g, "-");

/// A file's text without its YAML front matter.
export const withoutFrontMatter = (text) => String(text).replace(/\r\n?/g, "\n").replace(FRONT_MATTER, "");

/// A URL's scheme as a browser reads it (lower case; it drops controls and
/// spaces as it reads one, so "java\tscript:" is javascript:), or null for
/// a relative URL. A protocol-relative one (`//host`) is the web's: "https".
export function scheme(url) {
  const read = String(url).replace(/[\x00-\x20\x7f]/g, "");
  if (read.startsWith("//")) return "https";
  return read.match(/^([a-z][a-z\d+.-]*):/i)?.[1].toLowerCase() ?? null;
}

/// A relative path from `at`, a folder ("" at the root): `..` and `.`
/// followed, a leading `/` from the root.
export function resolvePath(at, rel) {
  const parts = rel.startsWith("/") ? [] : at.split("/").filter(Boolean);
  for (const seg of rel.split("/")) {
    if (seg === "..") parts.pop();
    else if (seg && seg !== ".") parts.push(seg);
  }
  return parts.join("/");
}

function settled(options) {
  for (const hook of ["resolve", "image", "code"]) {
    if (options[hook] !== undefined && typeof options[hook] !== "function") throw new TypeError(`markdown: options.${hook} is a function`);
  }
  return {
    resolve: options.resolve ?? webLink,
    image: options.image ?? (() => null),
    code: options.code ?? plainCode,
    breaks: options.breaks === true,
    refs: new Map(),
  };
}

const webLink = (target, kind) => (kind === "link" && ["http", "https", "mailto"].includes(scheme(target)) ? { href: target } : null);

function plainCode(text, lang) {
  const pre = el("pre");
  if (lang) pre.dataset.lang = lang;
  pre.append(el("code", null, text));
  return pre;
}

function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

// `href` if it is a URL of one of `schemes` or a relative one, else null.
function safe(href, schemes) {
  if (typeof href !== "string" || !href.replace(/[\x00-\x20\x7f]/g, "")) return null;
  const s = scheme(href);
  return s === null || schemes.includes(s) ? href : null;
}

// A hook's answer for a link, held to the safe schemes: {href, path?} or null.
function destination(answer) {
  if (answer === null || answer === undefined) return null;
  if (typeof answer !== "object") throw new TypeError("markdown: resolve answers {href, path?} or null");
  const href = safe(answer.href, LINK_SCHEMES);
  if (href === null) return null;
  if (answer.path !== undefined && typeof answer.path !== "string") throw new TypeError("markdown: a link's path is a string");
  return { href, path: answer.path };
}

// The reference definitions (`[label]: url "title"`) outside code, each line
// taken out of the text; labels match without case.
function definitions(lines) {
  const refs = new Map();
  let fence = null;
  // bounded by the lines: one look each
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const f = line.match(FENCE);
    if (fence) {
      if (closes(line, fence)) fence = null;
      continue;
    }
    if (f) {
      fence = f[1];
      continue;
    }
    const d = line.length <= 2048 ? line.match(DEFINITION) : null;
    if (!d || refs.size >= REFS_MAX) continue;
    const label = d[1].trim().toLowerCase();
    if (!refs.has(label)) refs.set(label, { href: d[2] ?? d[3], title: d[4] });
    lines[i] = "";
  }
  return refs;
}

// `tight`: a tight list item's lines, whose paragraphs get no <p>.
function blocks(lines, o, depth, tight) {
  const out = document.createDocumentFragment();
  if (depth >= DEPTH_MAX) {
    out.append(el("p", null, lines.join("\n")));
    return out;
  }
  // Each pass consumes at least one line (a block's lines, or a blank one).
  for (let i = 0; i < lines.length; ) {
    const line = lines[i];
    let m;
    let h;
    if (!line.trim()) {
      i++;
    } else if ((m = line.match(FENCE))) {
      const text = [];
      for (i++; i < lines.length && !closes(lines[i], m[1]); i++) text.push(lines[i]);
      i++;
      out.append(codeBlock(text.join("\n"), m[2].match(/^[\w+#.-]{1,40}/)?.[0] ?? "", o));
    } else if (line.startsWith("    ")) {
      // indented code: its lines and the blank ones between them
      const text = [];
      for (; i < lines.length && (lines[i].startsWith("    ") || !lines[i].trim()); i++) text.push(lines[i].slice(4));
      while (text.length && !text.at(-1).trim()) text.pop();
      out.append(codeBlock(text.join("\n"), "", o));
    } else if ((h = headingOf(line))) {
      out.append(heading(h.level, h.text, o, depth));
      i++;
    } else if (RULE.test(line)) {
      out.append(el("hr"));
      i++;
    } else if (COMMENT_OPEN.test(line)) {
      // an HTML comment block is left out, to its end
      for (; i < lines.length && !lines[i].includes("-->"); i++);
      i++;
    } else if (QUOTE.test(line)) {
      const inner = [];
      for (; i < lines.length && (QUOTE.test(lines[i]) || (lines[i].trim() && inner.at(-1)?.trim() && !interrupts(lines[i]))); i++) inner.push(lines[i].replace(/^ {0,3}> ?/, ""));
      out.append(quote(inner, o, depth));
    } else if ((m = line.match(ITEM)) && m[1].length < 4) {
      i = list(lines, i, out, o, depth);
    } else if (tableAt(lines, i)) {
      // its rows: to a blank line, a line with no pipe, or another block
      const rows = [];
      for (; i < lines.length && lines[i].trim() && lines[i].includes("|") && (rows.length < 2 || !interrupts(lines[i])); i++) rows.push(lines[i]);
      out.append(table(rows, o, depth));
    } else {
      const para = [line.trimStart()];
      let level = 0;
      for (i++; i < lines.length && lines[i].trim(); i++) {
        level = /^ {0,3}=+[ \t]*$/.test(lines[i]) ? 1 : /^ {0,3}-+[ \t]*$/.test(lines[i]) ? 2 : 0;
        if (level) {
          i++;
          break;
        }
        if (interrupts(lines[i]) || tableAt(lines, i)) break;
        para.push(lines[i].trimStart());
      }
      const text = para.join("\n");
      out.append(level ? heading(level, text.trim(), o, depth) : spans(tight ? document.createDocumentFragment() : el("p"), text.trimEnd(), o, depth, false));
    }
  }
  return out;
}

// Whether `line` closes a block its `fence` opened: the same mark, as long or longer.
function closes(line, fence) {
  const m = line.match(FENCE_CLOSE);
  return !!m && m[1][0] === fence[0] && m[1].length >= fence.length;
}

function codeBlock(text, lang, o) {
  const node = o.code(text, lang);
  if (!(node instanceof Node)) throw new TypeError("markdown: options.code answers a node");
  return node;
}

// A heading's level and words, or null: its closing marks are cut by
// string operations (a pattern for them backtracks over a run of spaces).
function headingOf(line) {
  const m = line.match(HEADING_OPEN);
  if (!m) return null;
  let text = line.slice(m[0].length).trim();
  let end = text.length;
  while (end > 0 && text[end - 1] === "#") end--;
  if (end === 0) text = "";
  else if (end < text.length && (text[end - 1] === " " || text[end - 1] === "\t")) text = text.slice(0, end).trimEnd();
  return { level: m[1].length, text };
}

function heading(level, text, o, depth) {
  const h = spans(el(`h${level}`), text, o, depth, false);
  h.dataset.anchor = slug(h.textContent);
  return h;
}

// A quote; `> [!note] Title` makes it one of Obsidian's callouts.
function quote(lines, o, depth) {
  const bq = el("blockquote");
  const callout = lines[0].match(/^\[!([\w-]{1,32})\][+-]?[ \t]*(.*)$/);
  if (callout) {
    bq.className = "md-callout";
    bq.dataset.kind = callout[1].toLowerCase();
    bq.append(spans(el("div", "md-callout-title"), callout[2] || callout[1], o, depth + 1, false));
    lines = lines.slice(1);
  }
  bq.append(blocks(lines, o, depth + 1, false));
  return bq;
}

// A list from line `i`, each item's lines handed to `blocks` (a nested list
// is an item's indented lines); answers the line after it. A blank line
// inside makes it loose: its items' paragraphs get <p>.
function list(lines, i, out, o, depth) {
  const first = lines[i].match(ITEM);
  const indent = first[1].length;
  const ordered = /\d/.test(first[2]);
  const sibling = (m, line) => !!m && m[1].length < indent + 2 && !RULE.test(line) && /\d/.test(m[2]) === ordered;
  const items = [];
  let loose = false;
  let col = 0;
  // Each pass consumes at least one line, or ends the list.
  while (i < lines.length) {
    const line = lines[i];
    const m = line.match(ITEM);
    const lead = line.length - line.trimStart().length;
    if (m && m[1].length < indent + 2 && !RULE.test(line)) {
      if (!sibling(m, line)) break;
      col = m[3] === undefined ? m[1].length + m[2].length + 1 : line.length - m[3].length;
      items.push([m[3] ?? ""]);
      i++;
    } else if (!line.trim()) {
      // the blank lines are the list's only if what follows them is its
      // next item, or indented into this one
      let next = i + 1;
      while (next < lines.length && !lines[next].trim()) next++;
      const after = lines[next];
      if (after === undefined || !(sibling(after.match(ITEM), after) || after.length - after.trimStart().length >= indent + 2)) break;
      loose = true;
      for (; i < next; i++) items.at(-1).push("");
    } else if (lead >= indent + 2) {
      items.at(-1).push(line.slice(Math.min(lead, col)));
      i++;
    } else if (items.at(-1).at(-1)?.trim() && !interrupts(line)) {
      // a lazy continuation of the item's paragraph
      items.at(-1).push(line);
      i++;
    } else {
      break;
    }
  }
  if (!items.length) throw new Error("markdown: a list starts at an item");
  const node = el(ordered ? "ol" : "ul");
  const start = parseInt(first[2], 10);
  if (ordered && start !== 1) node.start = start;
  for (const item of items) {
    const li = el("li");
    const task = item[0].match(/^\[([ xX])\](?:[ \t]+|$)/);
    if (task) {
      const box = el("input");
      box.type = "checkbox";
      box.checked = task[1] !== " ";
      box.disabled = true;
      box.setAttribute("aria-label", box.checked ? "Completed task" : "Incomplete task");
      li.className = "md-task";
      li.append(box, " ");
      item[0] = item[0].slice(task[0].length);
    }
    li.append(blocks(item, o, depth + 1, !loose));
    node.append(li);
  }
  out.append(node);
  return i;
}

// A table starts at `i`: a row with a pipe over a separator row of as many
// cells, at most COLUMNS_MAX.
function tableAt(lines, i) {
  const next = lines[i + 1];
  if (!lines[i].includes("|") || next === undefined || !next.includes("-")) return false;
  const separators = cells(next);
  if (separators.length > COLUMNS_MAX || !separators.every((c) => SEP_CELL.test(c))) return false;
  return cells(lines[i]).length === separators.length;
}

// A row's cells: an escaped pipe, and a pipe in a code span, are text.
function cells(row) {
  row = row.trim().replace(/^\|/, "").replace(/(?<!\\)\|$/, "");
  const result = [];
  let part = "";
  let ticks = 0;
  for (let i = 0; i < row.length; i++) {
    const ch = row[i];
    if (ch === "\\" && row[i + 1] === "|") {
      part += "|";
      i++;
    } else if (ch === "`") {
      let count = 1;
      while (row[i + 1] === "`") {
        count++;
        i++;
      }
      ticks = ticks === count ? 0 : ticks || count;
      part += "`".repeat(count);
    } else if (ch === "|" && !ticks) {
      result.push(part.trim());
      part = "";
    } else {
      part += ch;
    }
  }
  result.push(part.trim());
  return result;
}

function table(rows, o, depth) {
  const header = cells(rows[0]);
  // a cell per column for each row, whatever it holds: past CELLS_MAX, its text
  if (header.length * (rows.length - 1) > CELLS_MAX) return el("p", null, rows.join("\n"));
  const align = cells(rows[1]).map((c) => (c.endsWith(":") ? (c.startsWith(":") ? "center" : "right") : ""));
  const row = (texts, tag) => {
    const tr = el("tr");
    for (let k = 0; k < header.length; k++) {
      const cell = spans(el(tag), texts[k] ?? "", o, depth, false);
      if (align[k]) cell.style.textAlign = align[k];
      tr.append(cell);
    }
    return tr;
  };
  const head = el("thead");
  head.append(row(header, "th"));
  const body = el("tbody");
  for (const r of rows.slice(2)) body.append(row(cells(r), "td"));
  const t = el("table");
  t.append(head, body);
  const wrap = el("div", "md-table");
  wrap.tabIndex = 0;
  wrap.setAttribute("role", "region");
  wrap.setAttribute("aria-label", "Table");
  wrap.append(t);
  return wrap;
}

// `linked`: inside a link's label, where a link is only its words.
function spans(parent, text, o, depth, linked) {
  if (depth >= DEPTH_MAX || text.length > INLINE_MAX_CHARS) {
    plain(parent, text, o);
    return parent;
  }
  let last = 0;
  // bounded by the text: each match consumes at least two characters
  for (const m of text.matchAll(INLINE)) {
    if (m.index > last) plain(parent, text.slice(last, m.index), o);
    parent.append(span(m, o, depth, linked));
    last = m.index + m[0].length;
  }
  if (last < text.length) plain(parent, text.slice(last), o);
  return parent;
}

// Text: a line break at each newline (`breaks`), else where a line ends in
// two spaces or a backslash; and at a `<br>`. Its character references are
// their characters. Each line is read once, by string operations.
function plain(parent, text, o) {
  const lines = text.split("\n");
  for (let n = 0; n < lines.length; n++) {
    let line = lines[n];
    const last = n === lines.length - 1;
    let broken = !last && o.breaks;
    if (!last && !o.breaks && line.endsWith("\\")) {
      line = line.slice(0, -1);
      broken = true;
    } else if (!last && !o.breaks && line.endsWith("  ")) {
      line = line.trimEnd();
      broken = true;
    }
    const parts = line.includes("<") ? line.split(BREAK_TAG) : [line];
    parts.forEach((part, k) => {
      if (k) parent.append(el("br"));
      if (part) parent.append(characters(part));
    });
    if (broken) parent.append(el("br"));
    else if (!last) parent.append("\n");
  }
}

// `text` with its character references as their characters (a name this
// does not know stays as it is).
function characters(text) {
  if (!text.includes("&")) return text;
  return text.replace(ENTITY, (whole, dec, hex, name) => {
    if (name !== undefined) return Object.hasOwn(NAMED, name) ? NAMED[name] : whole;
    const code = dec !== undefined ? Number(dec) : parseInt(hex, 16);
    const valid = code > 0 && code <= 0x10ffff && !(code >= 0xd800 && code <= 0xdfff);
    return valid ? String.fromCodePoint(code) : "�";
  });
}

function span(m, o, depth, linked) {
  const g = m.groups;
  if (g.code !== undefined) return el("code", null, g.code.replace(/\n/g, " ").replace(/^ (.+) $/, "$1"));
  const href = g.hrefa ?? g.href;
  const linkish = g.wiki !== undefined || (href !== undefined && !g.bang) || (g.rref !== undefined && !g.rbang) || g.auto !== undefined || g.automail !== undefined || g.bare !== undefined || g.mail !== undefined;
  // a link's label may hold a picture (a badge), never another link
  if (linkish && linked) return document.createTextNode(m[0]);
  if (g.wiki !== undefined) return wikilink(g.wiki, g.embed === "!", o);
  if (href !== undefined && g.bang) return image(href, g.label, "link", o, 0) ?? el("span", "md-missing", g.label || href);
  if (href !== undefined) return link(href, g.label, g.title, o, depth);
  if (g.rref !== undefined) {
    // a reference link: its definition's, or the words it was written with
    const def = o.refs.get((g.rref || g.rtext).trim().toLowerCase());
    if (!def) return document.createTextNode(m[0]);
    if (g.rbang) return image(def.href, g.rtext, "link", o, 0) ?? el("span", "md-missing", g.rtext || def.href);
    return link(def.href, g.rtext, def.title, o, depth);
  }
  if (g.auto !== undefined || g.bare !== undefined) return link(g.auto ?? g.bare, null, null, o, depth);
  if (g.automail !== undefined || g.mail !== undefined) {
    const address = g.automail ?? g.mail;
    return link(`mailto:${address}`, address, null, o, depth);
  }
  if (g.escaped !== undefined) return document.createTextNode(g.escaped);
  const [tag, inner] = [
    ["strong", g.strong ?? g.strong2],
    ["del", g.del],
    ["mark", g.mark],
    ["em", g.em ?? g.em2],
  ].find(([, v]) => v !== undefined);
  return spans(el(tag), inner, o, depth + 1, linked);
}

// A link where the page says it goes; `label` null shows the target itself.
function link(target, label, title, o, depth) {
  const to = destination(o.resolve(target, "link"));
  const a = el(to ? "a" : "span", to ? null : "md-missing");
  if (label === null) a.textContent = target;
  else spans(a, label, o, depth + 1, true);
  if (title) a.title = title;
  if (to) point(a, to);
  return a;
}

// A page's own file opens in the page; anything else, in a tab of its own.
function point(a, to) {
  a.href = to.href;
  if (to.path !== undefined) {
    a.dataset.path = to.path;
  } else {
    a.target = "_blank";
    a.rel = "noopener noreferrer";
  }
}

// A picture the page shows, or null when it shows none of `src`.
function image(src, alt, kind, o, width) {
  const answer = o.image(src, kind);
  if (answer === null || answer === undefined) return null;
  if (typeof answer !== "object") throw new TypeError("markdown: image answers {src, href?} or null");
  const url = safe(answer.src, SRC_SCHEMES);
  if (url === null) return null;
  const img = el("img", "md-image");
  img.src = url;
  img.alt = alt ?? "";
  img.loading = "lazy";
  if (width) img.width = width;
  const href = answer.href === undefined ? null : safe(answer.href, LINK_SCHEMES);
  if (href === null) return img;
  const a = el("a", "md-image-link");
  a.href = href;
  a.target = "_blank";
  a.rel = "noopener noreferrer";
  a.append(img);
  return a;
}

// [[target|label]], or ![[target|width]]: a picture when the page shows one
// of it, else a link to it.
function wikilink(inner, embed, o) {
  const bar = inner.indexOf("|");
  const ref = (bar < 0 ? inner : inner.slice(0, bar)).trim();
  const label = bar < 0 ? "" : inner.slice(bar + 1).trim();
  if (embed) {
    const picture = image(ref, ref, "wiki", o, /^\d{1,4}$/.test(label) ? Number(label) : 0);
    if (picture) return picture;
  }
  const to = destination(o.resolve(ref, "wiki"));
  const a = el(to ? "a" : "span", to ? "md-wikilink" : "md-wikilink md-missing", label || ref);
  if (to) point(a, to);
  return a;
}
