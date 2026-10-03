// A brain's markdown as its search sees it (app.mjs), and a query as plain
// words. Pure functions, no I/O.
//
// A page is cut into sections, as Finite Brain's search cuts it: the text
// under each heading (ATX, `#` to `######`, outside fenced code), with the
// headings above it (its ancestry) and the page's title (frontmatter's
// `title`, else its first `# heading`, else its file name). Text before
// the first heading, with frontmatter's summary, description and tags, is
// a section of its own; a page with no headings is one. A section longer
// than SECTION_MAX_CHARS splits at a paragraph; a page keeps at most
// SECTIONS_MAX of them.

export const SECTION_MAX_CHARS = 8000;
export const SECTIONS_MAX = 500;
export const QUERY_WORDS_MAX = 16;
export const TITLE_MAX_CHARS = 300;

const FENCE = /^ {0,3}(`{3,}|~{3,})/;
const ATX = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$/;

/// Markdown's inline marks, gone: what a heading or a title reads as.
export function plain(text) {
  return String(text)
    .replace(/\[\[([^\]|]+)\|([^\]]+)\]\]/g, "$2")
    .replace(/\[\[([^\]]+)\]\]/g, "$1")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[*_`~]+/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

const unquote = (v) => v.trim().replace(/^(["'])(.*)\1$/, "$2").trim();

/// The page's frontmatter (a `---` block at its very top): its title,
/// summary, description and tags, and the text after it.
export function frontmatter(text) {
  const m = /^---[ \t]*\r?\n([\s\S]*?)\r?\n(?:---|\.\.\.)[ \t]*(?:\r?\n|$)/.exec(text);
  if (!m) return { meta: {}, body: text };
  const meta = {};
  let list = null;
  for (const line of m[1].split(/\r?\n/)) {
    const item = /^\s+-\s+(.*)$/.exec(line);
    if (item && list) {
      list.push(unquote(item[1]));
      continue;
    }
    list = null;
    const kv = /^([A-Za-z_][\w-]*):\s*(.*)$/.exec(line);
    if (!kv) continue;
    const [, key, value] = kv;
    if (!["title", "summary", "description", "tags"].includes(key)) continue;
    if (key === "tags") {
      const inline = /^\[(.*)\]$/.exec(value.trim());
      meta.tags = inline ? inline[1].split(",").map(unquote).filter(Boolean) : value.trim() ? value.split(",").map(unquote).filter(Boolean) : [];
      if (!value.trim()) list = meta.tags;
    } else {
      meta[key] = unquote(value);
    }
  }
  return { meta, body: text.slice(m[0].length) };
}

/// A heading line's level and text, or null.
function heading(line) {
  const m = ATX.exec(line);
  if (!m) return null;
  const text = plain((m[2] ?? "").replace(/(^|[ \t])#+$/, ""));
  return text ? { level: m[1].length, text } : null;
}

/// A body too long for one section, cut at paragraphs (or hard, when a
/// paragraph alone is longer).
function split(body) {
  const text = body.trim();
  if (!text) return [];
  if (text.length <= SECTION_MAX_CHARS) return [text];
  const parts = [];
  let rest = text;
  while (rest.length > SECTION_MAX_CHARS) {
    let cut = rest.lastIndexOf("\n\n", SECTION_MAX_CHARS);
    if (cut < SECTION_MAX_CHARS / 2) cut = SECTION_MAX_CHARS;
    parts.push(rest.slice(0, cut).trim());
    rest = rest.slice(cut).trim();
  }
  if (rest) parts.push(rest);
  return parts;
}

/// The file name a path reads as, when its page has no title.
export function stem(path) {
  return (path.split("/").pop() || path).replace(/\.(md|markdown)$/i, "").replace(/[-_]+/g, " ").trim() || path;
}

/// `path`'s markdown in sections: `{title, sections: [{heading, ancestry,
/// body}], truncated}` (`heading` null for the text before the first).
export function sections(path, text) {
  const { meta, body } = frontmatter(String(text).replace(/^﻿/, ""));
  const lines = body.split(/\r?\n/);
  const heads = [];
  let fence = null;
  lines.forEach((line, at) => {
    const f = FENCE.exec(line);
    if (f) {
      if (!fence) fence = f[1][0];
      else if (f[1][0] === fence) fence = null;
      return;
    }
    if (fence) return;
    const h = heading(line);
    if (h) heads.push({ at, ...h });
  });
  const h1 = heads.find((h) => h.level === 1);
  const title = (plain(meta.title ?? "") || h1?.text || stem(path)).slice(0, TITLE_MAX_CHARS);
  const out = [];
  const push = (head, ancestry, text) => {
    const parts = split(text);
    if (!parts.length && head) parts.push("");
    for (const part of parts) out.push({ heading: head, ancestry, body: part });
  };
  const lead = [meta.summary, meta.description, (meta.tags ?? []).join(", ")].filter(Boolean);
  push(null, [], [...lead, lines.slice(0, heads[0]?.at ?? lines.length).join("\n")].join("\n\n"));
  const above = [];
  heads.forEach((h, i) => {
    above.length = h.level - 1;
    const ancestry = above.filter(Boolean);
    above[h.level - 1] = h.text;
    push(h.text, ancestry, lines.slice(h.at + 1, heads[i + 1]?.at ?? lines.length).join("\n"));
  });
  const truncated = out.length > SECTIONS_MAX;
  return { title, sections: out.slice(0, SECTIONS_MAX), truncated };
}

/// A query as words: runs of letters, digits and marks, lowercased. Nothing
/// else in it means anything: FTS5's syntax (OR, NOT, NEAR, quotes, column:,
/// *, ^) is text like any other, split away or kept as a word.
export function words(q) {
  return String(q).normalize("NFKC").toLowerCase().split(/[^\p{L}\p{N}\p{M}]+/u).filter(Boolean);
}

/// The FTS5 expression for `ws`: each word a quoted phrase matching a word
/// or a word's start, all of them required.
export function match(ws) {
  return ws.map((w) => `"${w.replaceAll('"', '""')}"*`).join(" ");
}
