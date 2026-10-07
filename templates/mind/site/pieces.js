// The pieces several screens share. Above all, "anywhere you see a snippet
// of an old chat, you can open it": a search hit opens in place to the
// messages around it (`context`), a line of memory opens to the two lines
// it was made from and down to its message (`node`), and a thread's card
// opens to its messages (`thread`), each without leaving the screen.

import { F, S, persona, putThread, titleOf } from "./store.js";
import { ago, cleanSummary, clock, firstLine, h, highlight, icon, markIn, md, parseTool, plural, reportOf, when } from "./ui.js";

export const go = (path) => {
  if (location.hash !== `#${path}`) location.hash = path;
};

/// A persona's face: its emoji in a soft disc.
export function avatar(p, cls = "") {
  return h(`span.avatar${cls ? "." + cls : ""}`, { "aria-hidden": "true", text: p?.emoji || "🌿" });
}

/// Who wrote a message, for a person.
export function who(m) {
  if (m.kind === "user") return reportOf(m.text) ? "Computer" : "You";
  if (m.kind === "talk") return persona(m.persona)?.name ?? "Mind";
  if (m.kind === "note") return "Note";
  if (m.kind === "tool") return "Looked";
  if (m.kind === "echo") return "Found";
  return m.kind;
}

/// A summary line with its kind tags as small labels ("user: …" reads as
/// a "you" label and the words): the agent's own view, made legible.
export function tagged(text) {
  const frag = document.createDocumentFragment();
  const parts = String(text ?? "").split(/(?:^|(?<=[;.\s]))(user|talk|tool|echo|note|work):\s*/gi);
  // split alternates: text, tag, text, tag, …
  for (let k = 0; k < parts.length; k++) {
    if (k % 2 === 1) {
      const kind = parts[k].toLowerCase();
      frag.append(h(`span.tag.tag-${kind}`, { text: { user: "you", talk: "mind", work: "computer" }[kind] ?? kind }));
    } else {
      // a short message's line is its words, markdown and all: shown plain
      const seg = parts[k].replace(/\s*;\s*$/, "").replace(/\*\*|`/g, "").trim();
      if (seg) frag.append(h("span.seg", { text: `${seg} ` }));
    }
  }
  return frag;
}

/// A topic's small chip, which opens it.
export function topicChip(id, p) {
  const t = (S.topics ?? []).find((x) => x.id === id);
  if (!t) return null;
  return h("a.chip", { href: `#/topic/${encodeURIComponent(id)}`, title: p ? `${Math.round(p * 100)}% sure` : t.name }, icon("hash"), t.name);
}

/// A row of a message near another (a search's context, a node's message).
export function miniMessage(m, { focus = false, q = "" } = {}) {
  const rep = m.kind === "user" ? reportOf(m.text) : null;
  const p = persona(m.persona);
  const kind = rep ? "work" : m.kind;
  const body = m.kind === "tool" || m.kind === "echo" ? h("pre.mini-pre", { text: m.text.length > 1200 ? `${m.text.slice(0, 1200)}…` : m.text }) : md(rep ? rep.text : m.text);
  if (focus && q) markIn(body, q);
  return h(
    `div.mini.mini-${kind}${focus ? ".focus" : ""}`,
    null,
    h("div.mini-head", null, m.kind === "talk" ? avatar(p, "xs") : null, h("span.mini-who", { text: who(m) }), h("time", { text: when(m.at), datetime: new Date(m.at).toISOString() })),
    h("div.mini-body", null, body),
  );
}

/// Messages as a short excerpt: what was said, and a turn's steps as one
/// quiet line (the thread shows them; an excerpt only notes them).
export function excerpt(msgs, { focus = null, q = "" } = {}) {
  const out = [];
  let looked = 0;
  const flush = () => {
    if (looked) out.push(h("div.mini-looked", null, icon("layers"), `Looked through memory ×${looked}`));
    looked = 0;
  };
  for (const m of msgs) {
    if (m.kind === "tool" || m.kind === "echo") {
      const call = m.kind === "tool" ? parseTool(m.text) : null;
      if (call?.name === "computer") {
        flush();
        out.push(h("div.mini-looked.hands", null, icon("monitor"), `Handed to the computer: ${firstLine(call.args?.task ?? "", 70)}`));
      } else if (call) looked++;
      continue;
    }
    flush();
    out.push(miniMessage(m, { focus: m.i === focus, q }));
  }
  flush();
  return out;
}

/// The messages around `i` in its thread, read in place, widening on
/// asking; with a way into the chat they are from.
export function contextView(i, { q = "", before = 2, after = 3 } = {}) {
  const box = h("div.context", { "aria-live": "polite" }, h("div.context-loading", null, icon("loader", "spin"), "Opening…"));
  let b = before;
  let a = after;
  async function load() {
    try {
      // the log is one: ask wider, and keep the hit's own thread
      const r = await F.call("context", { i, before: b * 3, after: a * 3 });
      const all = (r.messages ?? []).filter((m) => Number.isInteger(m.i)).sort((x, y) => x.i - y.i);
      const thread = all.find((m) => m.i === i)?.thread ?? null;
      const mine = all.filter((m) => m.thread === thread);
      const at = mine.findIndex((m) => m.i === i);
      const said = (m) => m.kind !== "tool" && m.kind !== "echo";
      let from = at;
      for (let n = 0; from > 0 && n < b; ) if (said(mine[--from])) n++;
      let to = at;
      for (let n = 0; to < mine.length - 1 && n < a; ) if (said(mine[++to])) n++;
      const list = h("div.context-list", null, excerpt(mine.slice(Math.max(0, from), to + 1), { focus: i, q }));
      const moreUp = h("button.ghost.small", { type: "button", onclick: () => ((b += 4), load()) }, icon("up"), "Earlier");
      const moreDown = h("button.ghost.small", { type: "button", onclick: () => ((a += 4), load()) }, icon("down"), "Later");
      const bar = h("div.context-bar", null, moreUp, moreDown, h("span.grow"), thread ? h("a.ghost.small", { href: `#/t/${thread}` }, "Open chat", icon("open")) : null);
      box.replaceChildren(list, bar);
    } catch (e) {
      box.replaceChildren(h("div.soft-error", { text: `Could not open it: ${e.message}` }));
    }
  }
  load();
  return box;
}

/// A search hit: its snippet, opening in place to its context.
export function hitRow(hit, q) {
  const row = h(`div.hit.hit-${hit.kind}`);
  const head = h(
    "button.hit-head",
    { type: "button", "aria-expanded": "false" },
    h("span.hit-who", { text: who({ kind: hit.kind, text: hit.snippet ?? "", persona: hit.persona }) }),
    h("span.hit-text", null, highlight(hit.snippet ?? hit.text ?? "", q)),
    h("time.hit-time", { text: ago(hit.at), title: Number.isFinite(hit.at) ? when(hit.at) : "" }),
    icon("expand", "hit-toggle"),
  );
  let open = null;
  head.addEventListener("click", () => {
    if (open) {
      open.remove();
      open = null;
      row.classList.remove("open");
      head.setAttribute("aria-expanded", "false");
      return;
    }
    open = contextView(hit.i, { q });
    row.append(open);
    row.classList.add("open");
    head.setAttribute("aria-expanded", "true");
  });
  row.append(head);
  return row;
}

/// A line of the view (`id+n|text`): opening it shows the two lines it was
/// made from, down to its one message; a line of one message opens to it.
export function memRow(line, { depth = 0, fresh = false } = {}) {
  const { id, n, built = true } = line;
  const text = built === false ? "" : line.text;
  const range = n <= 1 ? `#${id}` : `#${id}–${id + n - 1}`;
  const row = h(`div.mem${built === false ? ".unbuilt" : ""}${fresh ? ".fresh" : ""}`, { dataset: { depth: String(depth), n: String(n) } });
  const scale = Math.max(0, Math.round(Math.log2(Math.max(1, n))));
  const head = h(
    "button.mem-head",
    { type: "button", "aria-expanded": "false" },
    h("span.mem-gauge", { "aria-hidden": "true", style: `--w:${scale}` }, h("i")),
    h("span.mem-text", null, built === false ? h("em.unbuilt-text", { text: "not summarized yet" }) : tagged(text)),
    h("span.mem-range", { text: range, title: n <= 1 ? "one message" : plural(n, "message") }),
  );
  row.append(head);
  let kids = null;
  head.addEventListener("click", async () => {
    if (kids) {
      kids.remove();
      kids = null;
      row.classList.remove("open");
      head.setAttribute("aria-expanded", "false");
      return;
    }
    row.classList.add("open");
    head.setAttribute("aria-expanded", "true");
    kids = h("div.mem-kids", null, h("div.context-loading", null, icon("loader", "spin"), "Opening…"));
    row.append(kids);
    const mine = kids;
    try {
      const r = await F.call("node", { id, n });
      if (kids !== mine) return;
      if (r.message) {
        const m = r.message;
        const thread = m.thread;
        mine.replaceChildren(
          miniMessage(m, { focus: true }),
          h(
            "div.context-bar",
            null,
            h("button.ghost.small", { type: "button", onclick: (e) => e.currentTarget.parentElement.replaceWith(contextView(m.i ?? id)) }, icon("expand"), "Around it"),
            h("span.grow"),
            thread ? h("a.ghost.small", { href: `#/t/${thread}` }, "Open chat", icon("open")) : null,
          ),
        );
      } else {
        mine.replaceChildren(...(r.children ?? []).map((c) => memRow(c, { depth: depth + 1 })));
      }
    } catch (e) {
      mine.replaceChildren(h("div.soft-error", { text: `Could not open it: ${e.message}` }));
    }
  });
  return row;
}

/// A thread as a card: its title, what it came to (its summary), its
/// topics; it opens in place to its messages.
export function threadCard(t, { open = false } = {}) {
  const p = persona(t.persona);
  const card = h("article.tcard");
  let summary = cleanSummary(t.summary);
  // a summary that opens with the title says it twice: keep what it came to
  const lead = (t.title ?? "").slice(0, 24).toLowerCase();
  if (lead && summary.toLowerCase().startsWith(lead) && summary.includes(" — ")) summary = summary.slice(summary.indexOf(" — ") + 3);
  const head = h(
    "button.tcard-head",
    { type: "button", "aria-expanded": "false" },
    avatar(p, "sm"),
    h(
      "span.tcard-main",
      null,
      h("span.tcard-title", { text: t.title || firstLine(summary, 60) || "A chat" }),
      summary ? h("span.tcard-sum", { text: summary }) : null,
    ),
    h("time.tcard-time", { text: ago(t.last ?? t.started), title: Number.isFinite(t.last) ? when(t.last) : "" }),
  );
  const chips = (t.topics ?? []).map((x) => topicChip(x.id, x.p)).filter(Boolean);
  const foot = h("div.tcard-foot", null, chips.length ? h("span.chips", null, chips) : null, t.count ? h("span.tcard-count", { text: plural(t.count, "message") }) : null);
  card.append(head, foot);
  let body = null;
  const toggle = async () => {
    if (body) {
      body.remove();
      body = null;
      card.classList.remove("open");
      head.setAttribute("aria-expanded", "false");
      return;
    }
    card.classList.add("open");
    head.setAttribute("aria-expanded", "true");
    body = h("div.tcard-body", null, h("div.context-loading", null, icon("loader", "spin"), "Opening…"));
    card.append(body);
    const mine = body;
    try {
      const r = await F.call("thread", { id: t.id, limit: 40 });
      if (r.thread) putThread(r.thread);
      const msgs = (r.messages ?? []).sort((a, b) => a.i - b.i);
      mine.replaceChildren(
        r.more ? h("div.tcard-more", { text: "Earlier messages are in the chat." }) : "",
        ...excerpt(msgs),
        h("div.context-bar", null, h("span.grow"), h("a.ghost.small", { href: `#/t/${t.id}` }, "Continue this chat", icon("open"))),
      );
    } catch (e) {
      mine.replaceChildren(h("div.soft-error", { text: `Could not open it: ${e.message}` }));
    }
  };
  head.addEventListener("click", toggle);
  if (open) toggle();
  return card;
}

/// A thread's title, as known or as soon as it is (for search groups).
export function threadTitle(id) {
  const span = h("span", { text: S.threads.get(id)?.title || S.titles.get(id) || "A chat" });
  if (!S.threads.get(id)?.title) titleOf(id).then((t) => t && (span.textContent = t));
  return span;
}

export { clock };
