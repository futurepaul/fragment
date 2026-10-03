// Markdown to DOM with textContent only (never innerHTML), so an agent's
// text can add no markup or script to the page: paragraphs, headings,
// lists (task lists too), quotes, fenced code, inline code, bold, italic,
// links (http and https only), bare URLs, pipe tables, and images from this
// fragment's own files (`__file?path=`) or blobs (`__blob/<sha256>`).

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

const LIST_ITEM = /^\s*([-*+]|\d+[.)])\s+/;

export function renderMarkdown(text) {
  const root = document.createDocumentFragment();
  const lines = String(text || "").replace(/\r\n/g, "\n").split("\n");
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
    const heading = line.match(/^(#{1,6})\s+(.*)$/);
    if (heading) {
      root.append(inline(el(`h${Math.min(heading[1].length + 2, 6)}`), heading[2]));
      i++;
      continue;
    }
    if (/^\s*>/.test(line)) {
      const quote = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) quote.push(lines[i++].replace(/^\s*>\s?/, ""));
      const bq = el("blockquote");
      bq.append(renderMarkdown(quote.join("\n")));
      root.append(bq);
      continue;
    }
    if (/^\s*\|.*\|\s*$/.test(line) && /^\s*\|?\s*:?-{2,}/.test(lines[i + 1] || "")) {
      const rows = [];
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) rows.push(lines[i++]);
      root.append(table(rows));
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
          const box = el("input");
          Object.assign(box, { type: "checkbox", checked: task[1] !== " ", disabled: true });
          li.append(box, " ");
          inline(li, task[2]);
        } else inline(li, item);
        list.append(li);
      }
      root.append(list);
      continue;
    }
    const para = [];
    while (i < lines.length && lines[i].trim() && !/^(```|#{1,6}\s|\s*>|\s*([-*+]|\d+[.)])\s+)/.test(lines[i])) para.push(lines[i++]);
    root.append(inline(el("p"), para.join("\n")));
  }
  return root;
}

function table(rows) {
  const cells = (row) => row.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim());
  const wrap = el("div", "md-table");
  const t = el("table");
  const head = el("tr");
  for (const c of cells(rows[0])) head.append(inline(el("th"), c));
  t.append(head);
  for (const row of rows.slice(2)) {
    const tr = el("tr");
    for (const c of cells(row)) tr.append(inline(el("td"), c));
    t.append(tr);
  }
  wrap.append(t);
  return wrap;
}

// Inline spans: `code`, **bold**, *italic* or _italic_, an image of this
// fragment's own, [text](url), and bare URLs.
const INLINE =
  /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*\s][^*]*\*|_[^_\s][^_]*_)|(!\[([^\]]*)\]\(((?:\.\/)?(?:__file\?path=[^)\s]+|__blob\/[0-9a-f]{64}))\))|(\[[^\]]+\]\((https?:\/\/[^)\s]+)\))|(https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"])/g;

/// Appends `text`'s inline spans to `parent`; returns `parent`.
export function inline(parent, text) {
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    if (m.index > last) appendText(parent, text.slice(last, m.index));
    const [whole] = m;
    if (m[1]) parent.append(el("code", null, whole.slice(1, -1)));
    else if (m[2]) parent.append(inline(el("strong"), whole.slice(2, -2)));
    else if (m[3]) parent.append(inline(el("em"), whole.slice(1, -1)));
    else if (m[4]) parent.append(image(m[6], m[5]));
    else if (m[7]) parent.append(link(m[8], whole.slice(1, whole.indexOf("]("))));
    else parent.append(link(whole, whole));
    last = m.index + whole.length;
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
  img.src = src;
  img.alt = alt;
  img.loading = "lazy";
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
