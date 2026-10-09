// Agent markdown to DOM: textContent only, no raw HTML. Covers the GFM
// agents write without a dependency. Nesting is bounded even in drafts.
import { copy, feedback } from "./response-actions.js";

const DEPTH_MAX = 32;
const LIST_ITEM = /^( *)([-*+]|\d+[.)])\s+(.*)$/;
const FENCE = /^ {0,3}(`{3,}|~{3,})(.*)$/;
const RULE = /^ {0,3}(?:(?:\*\s*){3,}|(?:-\s*){3,}|(?:_\s*){3,})$/;
function el(tag, cls, text) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

export function renderMarkdown(text, depth = 0) {
  const root = document.createDocumentFragment();
  if (depth >= DEPTH_MAX) { root.append(el("p", null, String(text))); return root; }
  const lines = String(text || "").replace(/\r\n?/g, "\n").replace(/\t/g, "    ").split("\n");
  let i = 0;
  // Each pass consumes at least one line, bounded by the record's text.
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) { i++; continue; }
    const fence = line.match(FENCE);
    if (fence) {
      const code = [];
      i++;
      while (i < lines.length) {
        const close = lines[i].match(FENCE);
        if (close && close[1][0] === fence[1][0] && close[1].length >= fence[1].length && !close[2].trim()) { i++; break; }
        code.push(lines[i++]);
      }
      root.append(codeBlock(code.join("\n"), fence[2].trim().match(/^[\w+#.-]{1,40}/)?.[0] ?? ""));
      continue;
    }
    const heading = line.match(/^ {0,3}(#{1,6})\s+(.*)$/);
    if (heading) {
      root.append(inline(el(`h${heading[1].length}`), heading[2].replace(/\s+#+\s*$/, ""), depth));
      i++;
      continue;
    }
    if (RULE.test(line)) { root.append(el("hr")); i++; continue; }
    if (/^ {0,3}>/.test(line)) {
      const quote = [];
      while (i < lines.length && /^ {0,3}>/.test(lines[i])) quote.push(lines[i++].replace(/^ {0,3}> ?/, ""));
      const node = el("blockquote");
      node.append(renderMarkdown(quote.join("\n"), depth + 1));
      root.append(node);
      continue;
    }
    if (isTable(lines, i)) {
      const header = cells(line), alignment = cells(lines[i + 1]);
      i += 2;
      const rows = [];
      while (i < lines.length && lines[i].trim() && lines[i].includes("|")) rows.push(cells(lines[i++]));
      root.append(table(header, alignment, rows, depth));
      continue;
    }
    const item = line.match(LIST_ITEM);
    if (item) {
      const ordered = /^\d/.test(item[2]), indent = item[1].length;
      const list = el(ordered ? "ol" : "ul");
      if (ordered) list.start = Number.parseInt(item[2], 10);
      while (i < lines.length) {
        const match = lines[i].match(LIST_ITEM);
        if (!match || match[1].length !== indent || /^\d/.test(match[2]) !== ordered) break;
        const contentIndent = lines[i].length - match[3].length;
        const content = [match[3]];
        i++;
        while (i < lines.length) {
          if (!lines[i].trim()) {
            const next = lines[i + 1];
            if (next && next.match(/^ */)[0].length > indent) { content.push(""); i++; continue; }
            break;
          }
          const spaces = lines[i].match(/^ */)[0].length;
          if (spaces <= indent) break;
          content.push(lines[i++].slice(Math.min(spaces, contentIndent)));
        }
        const li = el("li");
        const task = content[0].match(/^\[([ xX])\]\s+(.*)$/);
        if (task) {
          const box = el("input");
          Object.assign(box, { type: "checkbox", checked: task[1] !== " ", disabled: true });
          box.setAttribute("aria-label", task[1] === " " ? "Incomplete task" : "Completed task");
          li.className = "md-task";
          li.append(box, " ");
          content[0] = task[2];
        }
        const rendered = renderMarkdown(content.join("\n"), depth + 1);
        // Tight lists keep the first paragraph inline with their marker.
        if (rendered.firstChild?.nodeName === "P") {
          const first = rendered.firstChild;
          li.append(...first.childNodes);
          first.remove();
        }
        li.append(rendered);
        list.append(li);
      }
      root.append(list);
      continue;
    }
    const paragraph = [lines[i++]];
    while (i < lines.length && lines[i].trim() && !FENCE.test(lines[i]) && !RULE.test(lines[i]) && !/^ {0,3}(#{1,6}\s|>)/.test(lines[i]) && !LIST_ITEM.test(lines[i]) && !isTable(lines, i)) paragraph.push(lines[i++]);
    root.append(inline(el("p"), paragraph.join("\n"), depth));
  }
  return root;
}

function codeBlock(text, language) {
  const wrap = el("div", "md-code"), header = el("div", "md-code-head");
  const button = el("button", "md-code-copy", "Copy");
  button.type = "button";
  button.setAttribute("aria-label", "Copy code");
  button.onclick = () => feedback(button, () => copy(text));
  header.append(el("span", "md-language", language || "Code"), button);
  const pre = el("pre"), code = el("code", null, text);
  if (language) code.dataset.language = language;
  pre.append(code);
  wrap.append(header, pre);
  return wrap;
}

// Escaped pipes and pipes in code spans are cell text, not delimiters.
function cells(row) {
  row = row.trim().replace(/^\|/, "").replace(/(?<!\\)\|$/, "");
  const result = [];
  let part = "", ticks = 0;
  for (let i = 0; i < row.length; i++) {
    const ch = row[i];
    if (ch === "\\" && row[i + 1] === "|") { part += "|"; i++; }
    else if (ch === "`") {
      let count = 1;
      while (row[i + 1] === "`") { count++; i++; }
      ticks = ticks === count ? 0 : ticks || count;
      part += "`".repeat(count);
    } else if (ch === "|" && !ticks) { result.push(part.trim()); part = ""; }
    else part += ch;
  }
  result.push(part.trim());
  return result;
}
function isTable(lines, i) {
  if (!lines[i]?.includes("|") || !lines[i + 1]) return false;
  const separators = cells(lines[i + 1]);
  return separators.length === cells(lines[i]).length && separators.every((cell) => /^:?-{3,}:?$/.test(cell));
}
function table(header, alignment, rows, depth) {
  const wrap = el("div", "md-table");
  wrap.tabIndex = 0;
  wrap.setAttribute("role", "region");
  wrap.setAttribute("aria-label", "Table");
  const node = el("table");
  const makeRow = (row, tag) => {
    const tr = el("tr");
    for (let i = 0; i < header.length; i++) {
      const cell = inline(el(tag), row[i] ?? "", depth);
      if (alignment[i].endsWith(":")) cell.style.textAlign = alignment[i].startsWith(":") ? "center" : "right";
      tr.append(cell);
    }
    return tr;
  };
  const head = el("thead"), body = el("tbody");
  head.append(makeRow(header, "th"));
  for (const row of rows) body.append(makeRow(row, "td"));
  node.append(head, body);
  wrap.append(node);
  return wrap;
}

const INLINE = /(?<escape>\\[\\`*{}\[\]()#+.!_>~|\-])|(?<code>(?<ticks>`+)([^`]|(?!\k<ticks>)`)+\k<ticks>)|(?<strong>\*\*[^*]+\*\*|__[^_]+__)|(?<strike>~~[^~]+~~)|(?<em>\*[^*\s][^*]*\*|_[^_\s][^_]*_)|(?<image>!\[(?<alt>[^\]]*)\]\((?<src>(?:\.\/)?(?:__file\?path=[^)\s]+|__blob\/[0-9a-f]{64}))\))|(?<link>\[(?<label>[^\]]+)\]\((?<href>https?:\/\/[^)\s]+|mailto:[^)\s]+)\))|<(?<auto>https?:\/\/[^<>\s]+|[\w.+-]+@[\w.-]+\.[a-zA-Z]{2,})>|(?<url>https?:\/\/[^\s<>()]+[^\s<>().,;:!?'\"])|(?<email>\b[\w.+-]+@[\w.-]+\.[a-zA-Z]{2,}\b)/g;

export function inline(parent, text, depth = 0) {
  text = String(text);
  if (depth >= DEPTH_MAX) { appendText(parent, text); return parent; }
  let last = 0;
  for (const match of text.matchAll(INLINE)) {
    if (match.index > last) appendText(parent, text.slice(last, match.index));
    const g = match.groups;
    if (g.escape) parent.append(g.escape.slice(1));
    else if (g.code) parent.append(el("code", null, g.code.slice(g.ticks.length, -g.ticks.length).replace(/\n/g, " ")));
    else if (g.strong) parent.append(inline(el("strong"), g.strong.slice(2, -2), depth + 1));
    else if (g.strike) parent.append(inline(el("del"), g.strike.slice(2, -2), depth + 1));
    else if (g.em) parent.append(inline(el("em"), g.em.slice(1, -1), depth + 1));
    else if (g.image) parent.append(image(g.src, g.alt));
    else if (g.link) parent.append(link(g.href, g.label));
    else {
      const label = g.auto ?? g.url ?? g.email;
      parent.append(link(/^https?:/.test(label) ? label : `mailto:${label}`, label));
    }
    last = match.index + match[0].length;
  }
  if (last < text.length) appendText(parent, text.slice(last));
  return parent;
}
function appendText(parent, text) {
  text.split("\n").forEach((part, n) => {
    if (n) parent.append(el("br"));
    parent.append(part);
  });
}
export function image(src, alt) {
  const a = link(src, "");
  a.className = "shot-link";
  const img = el("img", "shot");
  Object.assign(img, { src, alt, loading: "lazy" });
  a.append(img);
  return a;
}
export function link(href, label) {
  const a = el("a", null, label);
  a.href = href;
  a.target = "_blank";
  a.rel = "noopener noreferrer";
  return a;
}
