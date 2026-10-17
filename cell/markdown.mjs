// The platform's markdown renderer, served on every fragment as
// ./__markdown.js: markdown to DOM through the DOM's own constructors and
// setters, never through markup, so the text of a file or a message can add
// no element, attribute or script to the page's origin. HTML in it shows as
// the text it is. The files viewer (`__files`), the chat template and the
// notes viewer render with it; any page may.
//
// It renders headings (each with a `data-anchor`, its `slug`), paragraphs,
// lists (ordered, nested, tight or loose, and tasks), quotes and Obsidian's
// callouts, fenced code, rules and pipe tables; inline code, bold, italic,
// strikethrough, highlights, links (with titles), autolinks, bare URLs and
// emails, pictures, [[wikilinks]] and ![[embeds]], hard breaks and a
// backslash's escapes.
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
// - `code(text, lang)`: a fenced block's node. Unset: `<pre><code>`.
// and `breaks`: every newline in a paragraph breaks its line, as in a chat
// (else only a hard break does, two spaces or a backslash before it).
//
// Whatever a hook answers, the renderer holds every link to http(s), mailto
// or a relative URL and every picture to http(s) or a relative URL: no
// hook's mistake makes a `javascript:` link. Its own elements are a fixed
// set (`ELEMENTS`), and it sets only their plain properties.

/// Nesting (quotes, lists, emphasis) stops here: deeper text shows as text.
export const DEPTH_MAX = 32;
/// A run of inline text longer than this shows as plain text: the inline
/// patterns' lazy spans would otherwise take quadratic time on a hostile
/// run of delimiters.
export const INLINE_MAX_CHARS = 20_000;
/// The elements the renderer makes (its unit test holds it to them).
export const ELEMENTS = [
  "p", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "input", "blockquote", "div", "pre", "code", "hr",
  "table", "thead", "tbody", "tr", "th", "td", "a", "span", "img", "strong", "em", "del", "mark", "br",
];

const LINK_SCHEMES = ["http", "https", "mailto"];
const SRC_SCHEMES = ["http", "https"];

const FENCE = /^ {0,3}(`{3,}|~{3,})[ \t]*([^\s`]*)/;
const FENCE_CLOSE = /^ {0,3}(`{3,}|~{3,})[ \t]*$/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/;
const RULE = /^ {0,3}([-*_])(?:[ \t]*\1){2,}[ \t]*$/;
const QUOTE = /^ {0,3}>/;
const ITEM = /^( *)([-*+]|\d{1,9}[.)])(?:[ \t]+(.*))?$/;
const ROW_SEP = /^ {0,3}\|?[ \t]*:?-+:?[ \t]*(?:\|[ \t]*:?-+:?[ \t]*)*\|?[ \t]*$/;
const FRONT_MATTER = /^---\n[\s\S]*?\n---(?:\n|$)/;
const interrupts = (line) => FENCE.test(line) || HEADING.test(line) || RULE.test(line) || QUOTE.test(line) || /^ {0,3}(?:[-*+]|1[.)])[ \t]+\S/.test(line);

// Inline spans, the one that starts earliest winning (and of those the
// first listed): `code`, [[a wikilink]] or ![[an embed]], ![a picture](src),
// [a link](href "title"), <https://an.autolink> or <an@email>, a bare
// https:// URL or email, **bold** or __bold__, ~~struck~~, ==marked==,
// *italic* or _italic_, and a backslash's escape.
const INLINE = new RegExp(
  [
    /(?<ticks>`+)(?<code>[\s\S]*?[^`])\k<ticks>(?!`)/,
    /(?<embed>!?)\[\[(?<wiki>[^\]\n]+)\]\]/,
    /(?<bang>!?)\[(?<label>(?:[^\][\n]|\[[^\]\n]*\])*)\]\(\s*<?(?<href>[^\s()<>]*(?:\([^\s()<>]*\)[^\s()<>]*)*)>?(?:\s+"(?<title>[^"]*)")?\s*\)/,
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
  const lines = String(text ?? "")
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((l) => l.replace(/^\t+/, (tabs) => "    ".repeat(tabs.length)));
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

function settled(options) {
  for (const hook of ["resolve", "image", "code"]) {
    if (options[hook] !== undefined && typeof options[hook] !== "function") throw new TypeError(`markdown: options.${hook} is a function`);
  }
  return {
    resolve: options.resolve ?? webLink,
    image: options.image ?? (() => null),
    code: options.code ?? plainCode,
    breaks: options.breaks === true,
  };
}

const webLink = (target, kind) => (kind === "link" && /^(?:https?:|mailto:)/i.test(target) ? { href: target } : null);

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

// `href` if it is a URL of one of `schemes` or a relative one, else null. A
// browser drops controls and spaces as it reads a scheme ("java\tscript:"
// is javascript:), so the scheme is read as the browser will.
function safe(href, schemes) {
  if (typeof href !== "string") return null;
  const read = href.replace(/[\x00-\x20\x7f]/g, "");
  if (!read) return null;
  const scheme = read.match(/^([a-z][a-z\d+.-]*):/i);
  if (scheme) return schemes.includes(scheme[1].toLowerCase()) ? href : null;
  return href;
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
    if (!line.trim()) {
      i++;
    } else if ((m = line.match(FENCE))) {
      const text = [];
      for (i++; i < lines.length && !closes(lines[i], m[1]); i++) text.push(lines[i]);
      i++;
      const node = o.code(text.join("\n"), m[2].match(/^[\w+#.-]{1,40}/)?.[0] ?? "");
      if (!(node instanceof Node)) throw new TypeError("markdown: options.code answers a node");
      out.append(node);
    } else if ((m = line.match(HEADING))) {
      out.append(heading(m[1].length, m[2] || "", o, depth));
      i++;
    } else if (RULE.test(line)) {
      out.append(el("hr"));
      i++;
    } else if (QUOTE.test(line)) {
      const inner = [];
      for (; i < lines.length && (QUOTE.test(lines[i]) || (lines[i].trim() && inner.at(-1)?.trim() && !interrupts(lines[i]))); i++) inner.push(lines[i].replace(/^ {0,3}> ?/, ""));
      out.append(quote(inner, o, depth));
    } else if ((m = line.match(ITEM)) && m[1].length < 4) {
      i = list(lines, i, out, o, depth);
    } else if (tableAt(lines, i)) {
      const rows = [];
      for (; i < lines.length && lines[i].trim() && lines[i].includes("|"); i++) rows.push(lines[i]);
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

// A table starts at `i`: a row with a pipe over a separator row of as many cells.
function tableAt(lines, i) {
  const next = lines[i + 1];
  return lines[i].includes("|") && next !== undefined && ROW_SEP.test(next) && cells(lines[i]).length === cells(next).length;
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

// Text, with a line break at each newline (`breaks`), else at a hard break.
function plain(parent, text, o) {
  const parts = o.breaks ? text.split("\n") : text.split(/(?: {2,}|\\)\n/);
  parts.forEach((part, n) => {
    if (n) parent.append(el("br"));
    parent.append(part);
  });
}

function span(m, o, depth, linked) {
  const g = m.groups;
  if (g.code !== undefined) return el("code", null, g.code.replace(/\n/g, " ").replace(/^ (.+) $/, "$1"));
  const linkish = g.wiki !== undefined || (g.href !== undefined && !g.bang) || g.auto !== undefined || g.automail !== undefined || g.bare !== undefined || g.mail !== undefined;
  // a link's label may hold a picture (a badge), never another link
  if (linkish && linked) return document.createTextNode(m[0]);
  if (g.wiki !== undefined) return wikilink(g.wiki, g.embed === "!", o);
  if (g.href !== undefined && g.bang) return image(g.href, g.label, "link", o, 0) ?? el("span", "md-missing", g.label || g.href);
  if (g.href !== undefined) return link(g.href, g.label, g.title, o, depth);
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
