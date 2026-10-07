// Markdown to DOM with textContent only (never innerHTML), so a page's
// text adds no markup or script: headings, paragraphs, lists (task lists
// too), quotes, rules, fenced code, pipe tables, inline code, bold,
// italic, links, images, and [[wikilinks]]. `link(target)` says where a
// wikilink or a relative link goes: `{href, missing}`; `file(path)` where
// a relative image is served.

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const LIST_ITEM = /^\s*([-*+]|\d+[.)])\s+/;
const BLOCK = /^(```|#{1,6}\s|\s*>|\s*([-*+]|\d+[.)])\s+|\s*(-{3,}|\*{3,})\s*$)/;

export function render(text, ctx) {
  const root = document.createDocumentFragment();
  const lines = String(text || "").replace(/\r\n/g, "\n").replace(/^---\n[\s\S]*?\n---\n/, "").split("\n");
  let i = 0;
  // bounded: each pass consumes at least one line
  while (i < lines.length) {
    const line = lines[i];
    if (/^```/.test(line)) {
      const code = [];
      i++;
      while (i < lines.length && !/^```/.test(lines[i])) code.push(lines[i++]);
      i++;
      const pre = el("pre");
      pre.append(el("code", null, code.join("\n")));
      root.append(pre);
      continue;
    }
    if (!line.trim()) {
      i++;
      continue;
    }
    if (/^\s*(-{3,}|\*{3,})\s*$/.test(line)) {
      root.append(el("hr"));
      i++;
      continue;
    }
    const heading = line.match(/^(#{1,6})\s+(.*)$/);
    if (heading) {
      root.append(inline(el(`h${heading[1].length}`), heading[2], ctx));
      i++;
      continue;
    }
    if (/^\s*>/.test(line)) {
      const quote = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) quote.push(lines[i++].replace(/^\s*>\s?/, ""));
      const bq = el("blockquote");
      bq.append(render(quote.join("\n"), ctx));
      root.append(bq);
      continue;
    }
    if (/^\s*\|.*\|\s*$/.test(line) && /^\s*\|?\s*:?-{2,}/.test(lines[i + 1] || "")) {
      const rows = [];
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) rows.push(lines[i++]);
      root.append(table(rows, ctx));
      continue;
    }
    const listMatch = line.match(LIST_ITEM);
    if (listMatch) {
      const list = el(/\d/.test(listMatch[1]) ? "ol" : "ul");
      while (i < lines.length && LIST_ITEM.test(lines[i])) {
        const item = lines[i++].replace(LIST_ITEM, "");
        const li = el("li");
        const task = item.match(/^\[([ xX])\]\s+(.*)$/);
        if (task) {
          li.className = "task";
          li.append(Object.assign(el("input"), { type: "checkbox", checked: task[1] !== " ", disabled: true }), " ");
          inline(li, task[2], ctx);
        } else inline(li, item, ctx);
        list.append(li);
      }
      root.append(list);
      continue;
    }
    const para = [];
    while (i < lines.length && lines[i].trim() && !BLOCK.test(lines[i])) para.push(lines[i++]);
    if (!para.length) para.push(lines[i++]);
    root.append(inline(el("p"), para.join("\n"), ctx));
  }
  return root;
}

function table(rows, ctx) {
  const cells = (row) => row.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
  const wrap = el("div", "table");
  const t = el("table");
  const head = el("tr");
  for (const c of cells(rows[0])) head.append(inline(el("th"), c, ctx));
  t.append(head);
  for (const row of rows.slice(2)) {
    const tr = el("tr");
    for (const c of cells(row)) tr.append(inline(el("td"), c, ctx));
    t.append(tr);
  }
  wrap.append(t);
  return wrap;
}

// Inline spans: `code`, [[wikilink|label]], an image, a link, **bold**,
// *italic* or _italic_, and bare URLs.
const INLINE = /(`[^`]+`)|(\[\[([^\]|]+)(?:\|([^\]]+))?\]\])|(!\[([^\]]*)\]\(([^)\s]+)\))|(\[([^\]]+)\]\(([^)\s]+)\))|(\*\*[^*]+\*\*)|(\*[^*\s][^*]*\*|_[^_\s][^_]*_)|(https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"])/g;

function inline(parent, text, ctx) {
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    if (m.index > last) appendText(parent, text.slice(last, m.index));
    const [whole] = m;
    if (m[1]) parent.append(el("code", null, whole.slice(1, -1)));
    else if (m[2]) parent.append(wikilink(m[3], m[4] ?? m[3], ctx));
    else if (m[5]) parent.append(image(m[7], m[6], ctx));
    else if (m[8]) parent.append(/^https?:\/\//i.test(m[10]) ? external(m[10], m[9]) : wikilink(m[10], m[9], ctx));
    else if (m[11]) parent.append(inline(el("strong"), whole.slice(2, -2), ctx));
    else if (m[12]) parent.append(inline(el("em"), whole.slice(1, -1), ctx));
    else parent.append(external(whole, whole));
    last = m.index + whole.length;
  }
  if (last < text.length) appendText(parent, text.slice(last));
  return parent;
}

function appendText(parent, text) {
  text.split("\n").forEach((part, n) => {
    if (n) parent.append(" ");
    parent.append(part);
  });
}

function wikilink(target, label, ctx) {
  const { href, missing } = ctx.link(target.trim());
  const a = el("a", missing ? "missing" : "wiki", label.trim());
  a.href = href;
  if (missing) a.title = "No page yet";
  return a;
}

function external(href, label) {
  const a = el("a", "external", label);
  Object.assign(a, { href, target: "_blank", rel: "noopener noreferrer" });
  return a;
}

function image(src, alt, ctx) {
  const img = el("img");
  img.alt = alt;
  img.loading = "lazy";
  img.src = /^https:\/\//i.test(src) ? src : ctx.file(src);
  return img;
}
