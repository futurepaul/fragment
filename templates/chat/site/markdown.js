// Minimal markdown → DOM. Builds nodes with textContent only (no innerHTML),
// so model output can never inject markup or script into the page.
// Covers what chat replies use: paragraphs, headings, lists, quotes, fenced
// code, inline code, bold, italic, links, and pipe tables.

export function renderMarkdown(text) {
  const root = document.createDocumentFragment();
  const lines = String(text || "").replace(/\r\n/g, "\n").split("\n");
  let i = 0;
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
    if (!line.trim()) { i++; continue; }
    const heading = line.match(/^(#{1,6})\s+(.*)$/);
    if (heading) { root.append(inline(el(`h${Math.min(heading[1].length + 2, 6)}`), heading[2])); i++; continue; }
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
    const listMatch = line.match(/^(\s*)([-*+]|\d+[.)])\s+/);
    if (listMatch) {
      const ordered = /\d/.test(listMatch[2]);
      const list = el(ordered ? "ol" : "ul");
      while (i < lines.length && /^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
        const item = lines[i++].replace(/^\s*([-*+]|\d+[.)])\s+/, "");
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

// Inline spans: `code`, **bold**, *italic*/_italic_, [text](url), bare URLs.
const INLINE = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*\s][^*]*\*|_[^_\s][^_]*_)|(\[[^\]]+\]\((https?:\/\/[^)\s]+)\))|(https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"])/g;

function inline(parent, text) {
  let last = 0;
  for (const m of text.matchAll(INLINE)) {
    if (m.index > last) appendText(parent, text.slice(last, m.index));
    const [whole] = m;
    if (m[1]) parent.append(el("code", null, whole.slice(1, -1)));
    else if (m[2]) parent.append(inline(el("strong"), whole.slice(2, -2)));
    else if (m[3]) parent.append(inline(el("em"), whole.slice(1, -1)));
    else if (m[4]) parent.append(link(m[5], whole.slice(1, whole.indexOf("]("))));
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

function link(href, label) {
  const a = el("a", null, label);
  a.href = href;
  a.target = "_blank";
  a.rel = "noopener noreferrer";
  return a;
}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}
