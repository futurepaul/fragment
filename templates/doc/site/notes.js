// Notes on a page's paragraphs, in its margin on a wide page and under
// each paragraph on a narrow one: the page half of applib/notes.mjs. Any
// element inside the column marked `data-notes` (else `<main>`) that has a
// `data-a` id takes notes; a note button comes beside the one under the
// pointer, or the one tapped, and quotes the words selected in it. The
// margin needs the column inside a `.notes-page` (notes.css lays the two
// out side by side when the window is wide enough); without one, notes are
// always under their paragraph. Notes whose paragraph is gone (its id
// changed) are listed at the end of the column.
//
// The notes are the app's rows, followed with `fragment.live`, so every
// open page sees a note as it lands. Other readers' cursors are opt-in:
// `<html data-cursors>` shares this page's pointer (presence) and draws
// theirs, each anchored to the paragraph under it so it lands on the same
// words at any width; without it the page sends no presence at all.
import * as fragment from "./__fragment.js";

const QUOTE_MAX_CHARS = 280;
const TEXT_MAX_CHARS = 2000;
const ANCHOR = /^[A-Za-z0-9_-]{1,64}$/;
const PROFILES_PER_ASK = 64;
const COLORS = ["#c2410c", "#0f766e", "#7c3aed", "#a16207", "#be185d", "#2563eb"];

const doc = document.querySelector("[data-notes]") ?? document.querySelector("main") ?? document.body;
const cursorsOn = document.documentElement.hasAttribute("data-cursors");

// ---- small DOM helper: text always goes in as text
function h(tag, attrs = {}, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v === false || v == null) continue;
    if (k === "class") el.className = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const kid of kids.flat()) if (kid != null && kid !== false) el.append(kid);
  return el;
}

// A presence cursor's arrow, drawn through the DOM (never from markup).
function pointer() {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  for (const [k, v] of [["viewBox", "0 0 16 20"], ["width", "16"], ["height", "20"], ["aria-hidden", "true"]]) svg.setAttribute(k, v);
  const path = document.createElementNS(ns, "path");
  path.setAttribute("d", "M1 1 L1 16 L5 12 L8 19 L11 18 L8 11 L14 11 Z");
  svg.append(path);
  return svg;
}

// Every paragraph a note can hang on, in document order.
const blocks = new Map();
for (const el of doc.querySelectorAll("[data-a]")) {
  const a = el.dataset.a;
  if (!ANCHOR.test(a)) console.warn(`notes: data-a="${a}" takes no notes (1 to 64 of A-Z a-z 0-9 _ -)`);
  else if (blocks.has(a)) console.warn(`notes: data-a="${a}" is on two elements; notes go to the first`);
  else blocks.set(a, el);
}
const order = new Map([...blocks.keys()].map((a, i) => [a, i]));

// What notes.js adds to the page: the margin (inside .notes-page), the
// note button, the list of notes whose paragraph is gone, and, with
// cursors on, their layer and who is here.
const layoutEl = doc.closest(".notes-page");
const gutter = layoutEl ? layoutEl.appendChild(h("aside", { class: "notes-gutter", "aria-label": "Notes" })) : null;
const noteBtn = document.body.appendChild(h("button", { type: "button", class: "notes-btn", hidden: true }, "+ Note"));
const orphanList = h("div");
const orphans = doc.appendChild(h("section", { class: "notes-orphans", hidden: true }, h("h2", {}, "Notes on text that has changed"), orphanList));
const statusEl = document.body.appendChild(h("p", { class: "notes-status", role: "status", hidden: true }, "Notes didn't load. They'll come back when the page reconnects."));
const cursorsEl = cursorsOn ? document.body.appendChild(h("div", { class: "notes-cursors", "aria-hidden": "true" })) : null;
const hereEl = cursorsOn ? document.body.appendChild(h("div", { class: "notes-here", hidden: true })) : null;

let me = null; // { id, principal, role } once the socket says hello
let notes = [];
let loadError = null;
let composing = null; // { key, anchor, replyTo, quote }
const drafts = new Map(); // composer key -> its text so far
const expanded = new Set(); // anchors whose notes are open under the paragraph (narrow)
let current = null; // the paragraph the note button is for
let lit = null; // the paragraph whose thread is in hand
let present = [];
let isWide = false; // the margin is shown: measured on each render and resize
// A thread that grows (a reply box opening) moves the ones below it.
const slotSizes = new ResizeObserver(() => requestAnimationFrame(layout));

// ---- who: names and pictures from the fragment's `__people`, in batches
const profiles = new Map();
const asking = new Set();
let askTimer = 0;
function want(principal) {
  if (typeof principal !== "string" || !principal.startsWith("npub1") || profiles.has(principal)) return;
  profiles.set(principal, null);
  asking.add(principal);
  if (!askTimer) askTimer = setTimeout(ask, 20);
}
async function ask() {
  askTimer = 0;
  const ids = [...asking];
  asking.clear();
  for (let i = 0; i < ids.length; i += PROFILES_PER_ASK) {
    const batch = ids.slice(i, i + PROFILES_PER_ASK);
    let found = {};
    try {
      const r = await fetch(`./__people?${batch.map((id) => `id=${encodeURIComponent(id)}`).join("&")}`, { credentials: "same-origin" });
      if (r.ok) found = (await r.json()).profiles ?? {};
    } catch {
      // shown by their npub's end until the page loads again
    }
    for (const id of batch) profiles.set(id, found[id] ?? {});
  }
  render();
  drawPresence();
}
function colorOf(principal) {
  let x = 0;
  for (const c of String(principal)) x = (x * 31 + c.charCodeAt(0)) >>> 0;
  return COLORS[x % COLORS.length];
}
function who(principal) {
  if (typeof principal !== "string" || !principal.startsWith("npub1")) return { name: "A visitor", picture: null, agent: false, color: colorOf(principal) };
  want(principal);
  const p = profiles.get(principal);
  const name = p?.title ?? p?.name ?? p?.email ?? null;
  return { name, picture: typeof p?.picture === "string" ? p.picture : null, agent: p?.kind === "agent", color: colorOf(principal) };
}
// The page's owner, as their own notes say: a stranger's `__people` may
// give no name for them, so they show as the owner.
function ownerPrincipal() {
  return notes.find((n) => n.role === "owner" && !n.agent)?.author ?? null;
}
function shortName(principal) {
  return principal?.startsWith("npub1") ? `npub…${principal.slice(-6)}` : "A visitor";
}
function presentName(principal) {
  const w = who(principal);
  if (w.name) return w.name;
  if (principal && principal === ownerPrincipal()) return "The owner";
  return shortName(principal);
}
const byOwner = (note) => note.role === "owner" && !note.agent;
function nameOf(note) {
  const w = who(note.agent ?? note.author);
  if (w.name) return w.name;
  if (byOwner(note)) return "The owner";
  return shortName(note.agent ?? note.author);
}
function avatar(principal, name) {
  const w = who(principal);
  if (w.picture) return h("img", { class: "notes-avatar", src: w.picture, alt: "" });
  return h("span", { class: "notes-avatar", style: `background:${w.color}` }, (name || "?").replace(/^npub…/, "").charAt(0).toUpperCase() || "?");
}
function ago(at) {
  const s = Math.max(0, (Date.now() - at) / 1000);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return new Date(at).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

const signedIn = () => typeof me?.principal === "string" && me.principal.startsWith("npub1");
const mayChange = (note) => me !== null && (me.role === "owner" || note.author === me.principal);
// the margin shows when notes.css lays it out (a wide window, a .notes-page)
const measureWide = () => gutter !== null && getComputedStyle(gutter).display !== "none";
const wide = () => isWide;

// ---- threads
function threads() {
  const roots = [];
  const replies = new Map();
  for (const n of notes) {
    if (n.reply_to === null) roots.push(n);
    else (replies.get(n.reply_to) ?? replies.set(n.reply_to, []).get(n.reply_to)).push(n);
  }
  return roots.map((root) => ({ root, replies: replies.get(root.id) ?? [] }));
}

async function act(op, input, errEl) {
  try {
    await fragment.call(op, input);
    return true;
  } catch (err) {
    if (errEl) {
      errEl.textContent = err?.message || "That didn't go through. Try again.";
      errEl.hidden = false;
    }
    return false;
  }
}

function noteView(note) {
  const name = nameOf(note);
  const del = mayChange(note)
    ? h("button", { type: "button", class: "notes-link", onclick: () => confirm(note.reply_to === null ? "Delete this thread?" : "Delete this note?") && act("remove_note", { id: note.id }) }, "Delete")
    : null;
  return h("div", { class: `notes-note${byOwner(note) ? " by-owner" : ""}`, "data-id": String(note.id) },
    h("div", { class: "notes-head" },
      avatar(note.agent ?? note.author, name),
      h("b", {}, name),
      byOwner(note) ? h("span", { class: "notes-tag owner" }, "owner") : null,
      note.agent || who(note.author).agent ? h("span", { class: "notes-tag" }, "agent") : null,
      h("time", { datetime: new Date(note.at).toISOString(), title: new Date(note.at).toLocaleString() }, ago(note.at)),
    ),
    h("p", { class: "notes-text" }, note.text),
    del ? h("div", { class: "notes-actions" }, del) : null,
  );
}

function threadView({ root, replies }) {
  const err = h("p", { class: "notes-err", hidden: true });
  if (root.resolved) {
    return h("article", { class: "notes-thread resolved", "data-anchor": root.anchor, "data-thread": String(root.id) },
      h("span", {}, `Resolved · ${nameOf(root)}: `, h("span", { class: "notes-muted" }, root.text.slice(0, 60) + (root.text.length > 60 ? "…" : ""))),
      mayChange(root) ? h("button", { type: "button", class: "notes-link", onclick: () => act("resolve", { id: root.id, resolved: false }, err) }, "Reopen") : null,
      err,
    );
  }
  const key = `r:${root.id}`;
  const replying = composing?.key === key;
  return h("article", { class: `notes-thread${lit === root.anchor ? " lit" : ""}`, "data-anchor": root.anchor, "data-thread": String(root.id), onclick: () => light(root.anchor) },
    root.quote ? h("blockquote", { class: "notes-quote" }, root.quote) : null,
    noteView(root),
    replies.map((r) => noteView(r)),
    replying
      ? composer({ key, anchor: root.anchor, replyTo: root.id, quote: "" })
      : h("div", { class: "notes-actions" },
          h("button", { type: "button", class: "notes-link", onclick: (e) => { e.stopPropagation(); openComposer({ key, anchor: root.anchor, replyTo: root.id, quote: "" }); } }, "Reply"),
          mayChange(root) ? h("button", { type: "button", class: "notes-link", onclick: (e) => { e.stopPropagation(); act("resolve", { id: root.id, resolved: true }, err); } }, "Resolve") : null,
        ),
    err,
  );
}

function composer({ key, anchor, replyTo, quote }) {
  if (!signedIn()) {
    const back = `${location.pathname}${location.search}`;
    return h("div", { class: "notes-composer", "data-key": key },
      h("p", { class: "notes-muted" }, "Notes are for signed-in readers."),
      h("div", { class: "notes-composer-actions" },
        h("button", { type: "button", class: "notes-ghost", onclick: closeComposer }, "Close"),
        h("a", { class: "notes-signin", href: `./__signin?return=${encodeURIComponent(back)}` }, "Sign in to leave one"),
      ),
    );
  }
  const err = h("p", { class: "notes-err", hidden: true });
  const text = h("textarea", { rows: 3, maxlength: TEXT_MAX_CHARS, placeholder: replyTo ? "Reply" : "Ask a question or add a note" });
  text.value = drafts.get(key) ?? "";
  text.addEventListener("input", () => drafts.set(key, text.value));
  const post = h("button", { type: "submit" }, replyTo ? "Reply" : "Post");
  const form = h("form", { class: "notes-composer", "data-key": key, onclick: (e) => e.stopPropagation() },
    quote ? h("blockquote", { class: "notes-quote" }, quote) : null,
    text,
    h("div", { class: "notes-composer-actions" }, h("button", { type: "button", class: "notes-ghost", onclick: closeComposer }, "Cancel"), post),
    err,
  );
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const body = text.value.trim();
    if (!body) return;
    post.disabled = true;
    const input = replyTo ? { text: body, reply_to: replyTo } : { text: body, anchor, ...(quote ? { quote } : {}) };
    if (await act("annotate", input, err)) {
      drafts.delete(key);
      composing = null;
      expanded.add(anchor);
      render();
    } else {
      post.disabled = false;
    }
  });
  text.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) form.requestSubmit();
    if (e.key === "Escape") closeComposer();
  });
  return form;
}

function openComposer(c) {
  composing = c;
  expanded.add(c.anchor);
  lit = c.anchor;
  hideNoteBtn();
  render();
  document.querySelector(`[data-key="${c.key}"] textarea`)?.focus();
}
function closeComposer() {
  composing = null;
  render();
}
function light(anchor) {
  if (lit === anchor) return;
  lit = anchor;
  render();
}

// ---- render: the margin on a wide page, under each paragraph on a narrow one
function render() {
  isWide = measureWide();
  const focused = document.activeElement?.closest?.("[data-key]")?.dataset.key ?? null;
  const caret = document.activeElement?.selectionStart ?? null;
  for (const el of doc.querySelectorAll(".notes-inline")) el.remove();
  gutter?.replaceChildren();
  orphanList.replaceChildren();

  const byAnchor = new Map();
  for (const t of threads()) (byAnchor.get(t.root.anchor) ?? byAnchor.set(t.root.anchor, []).get(t.root.anchor)).push(t);
  if (composing && !composing.replyTo && !byAnchor.has(composing.anchor)) byAnchor.set(composing.anchor, []);

  const anchors = [...byAnchor.keys()].sort((a, b) => (order.get(a) ?? Infinity) - (order.get(b) ?? Infinity));
  let lost = 0;
  for (const a of anchors) {
    const ts = byAnchor.get(a);
    const views = ts.map(threadView);
    if (composing && !composing.replyTo && composing.anchor === a) views.push(composer(composing));
    const block = blocks.get(a);
    if (!block) {
      lost += ts.length;
      orphanList.append(...ts.map((t) => h("div", { class: "notes-orphan" }, threadView(t))));
      continue;
    }
    if (isWide) {
      gutter.append(h("div", { class: "notes-slot", "data-anchor": a }, views));
    } else {
      const open = ts.filter((t) => !t.root.resolved).length;
      const isOpen = expanded.has(a) || (composing !== null && composing.anchor === a);
      const toggle = ts.length
        ? h("button", { type: "button", class: "notes-toggle", "aria-expanded": String(isOpen), onclick: () => { isOpen ? expanded.delete(a) : expanded.add(a); render(); } },
            open ? `${open} ${open === 1 ? "note" : "notes"}` : `${ts.length} resolved`)
        : null;
      block.after(h("div", { class: "notes-inline", "data-anchor": a }, toggle, isOpen ? views : null));
    }
  }
  orphans.hidden = lost === 0;
  statusEl.hidden = loadError === null;

  for (const [a, el] of blocks) {
    const ts = byAnchor.get(a) ?? [];
    el.classList.toggle("notes-has", ts.some((t) => !t.root.resolved));
    el.classList.toggle("notes-lit", lit === a);
  }
  slotSizes.disconnect();
  for (const slot of gutter?.querySelectorAll(":scope > .notes-slot") ?? []) slotSizes.observe(slot);
  if (focused) {
    const t = document.querySelector(`[data-key="${focused}"] textarea`);
    if (t) {
      t.focus();
      if (caret != null) t.setSelectionRange(caret, caret);
    }
  }
  layout();
}

// Threads sit level with their paragraph, pushed down past the one above.
function layout() {
  if (!wide()) {
    if (gutter) gutter.style.height = "";
    drawPresence();
    return;
  }
  const top0 = gutter.getBoundingClientRect().top;
  let y = 0;
  for (const slot of gutter.querySelectorAll(":scope > .notes-slot")) {
    const block = blocks.get(slot.dataset.anchor);
    const top = Math.max(block ? block.getBoundingClientRect().top - top0 : y, y);
    slot.style.top = `${top}px`;
    y = top + slot.offsetHeight + 10;
  }
  gutter.style.height = `${y}px`;
  drawPresence();
}

// ---- the note button: beside the paragraph under the pointer, or the one
// tapped or selected in; it quotes the selection when there is one
function blockFor(node) {
  const el = node?.nodeType === 1 ? node : node?.parentElement;
  const b = el?.closest?.("[data-a]");
  return b && blocks.get(b.dataset.a) === b ? b : null;
}
function selectionIn(block) {
  const sel = getSelection();
  if (!sel || sel.isCollapsed || !sel.rangeCount) return "";
  const r = sel.getRangeAt(0);
  if (!block.contains(r.commonAncestorContainer)) return "";
  return [...sel.toString().replace(/\s+/g, " ").trim()].slice(0, QUOTE_MAX_CHARS).join("");
}
// In the page's coordinates: the button is the body's, which is not positioned.
function showNoteBtn(block, y) {
  current = block;
  noteBtn.hidden = false;
  if (wide()) {
    noteBtn.classList.remove("floating");
    const d = doc.getBoundingClientRect();
    noteBtn.style.left = `${d.right + scrollX + 6}px`;
    noteBtn.style.top = `${(y ?? block.getBoundingClientRect().top) + scrollY}px`;
  } else {
    noteBtn.classList.add("floating");
    noteBtn.style.left = "";
    noteBtn.style.top = "";
  }
}
function hideNoteBtn() {
  noteBtn.hidden = true;
  current = null;
}
doc.addEventListener("pointermove", (e) => {
  if (e.pointerType !== "mouse" || !wide()) return;
  const b = blockFor(e.target);
  if (b && b !== current) showNoteBtn(b);
});
doc.addEventListener("click", (e) => {
  if (wide() || e.target.closest("a, button, summary, textarea, .notes-inline")) return;
  const b = blockFor(e.target);
  if (b) {
    lit = b.dataset.a;
    showNoteBtn(b);
    render();
  }
});
document.addEventListener("click", (e) => {
  if (!wide() && !noteBtn.hidden && e.target !== noteBtn && !blockFor(e.target)) hideNoteBtn();
});
document.addEventListener("selectionchange", () => {
  const sel = getSelection();
  if (!sel || sel.isCollapsed || !sel.rangeCount) return;
  const b = blockFor(sel.getRangeAt(0).commonAncestorContainer);
  if (!b) return;
  showNoteBtn(b, sel.getRangeAt(0).getBoundingClientRect().top);
});
// keep the selection: the quote is read on click
noteBtn.addEventListener("pointerdown", (e) => e.preventDefault());
noteBtn.addEventListener("click", () => {
  if (!current) return;
  const a = current.dataset.a;
  openComposer({ key: `a:${a}`, anchor: a, replyTo: null, quote: selectionIn(current) });
});

// ---- presence, with cursors on: this page's pointer, and the others'
function pointerPos(e) {
  let b = blockFor(e.target);
  if (!b) {
    for (const el of blocks.values()) if (el.getBoundingClientRect().top <= e.clientY) b = el;
    b ??= blocks.values().next().value;
  }
  if (!b) return {};
  const d = doc.getBoundingClientRect();
  const r = b.getBoundingClientRect();
  const round = (v) => Math.round(v * 1e4) / 1e4;
  return {
    a: b.dataset.a,
    fx: round(Math.min(2, Math.max(-0.5, (e.clientX - d.left) / Math.max(1, d.width)))),
    fy: round(Math.min(6, Math.max(-2, (e.clientY - r.top) / Math.max(1, r.height)))),
  };
}

const cursorEls = new Map();
function drawPresence() {
  if (!cursorsOn) return;
  const seen = new Set();
  const d = doc.getBoundingClientRect();
  for (const p of present) {
    if (!me || p.id === me.id || !p.data?.a) continue;
    const block = blocks.get(p.data.a);
    if (!block) continue;
    const r = block.getBoundingClientRect();
    const x = Math.min(innerWidth - 24, d.left + p.data.fx * d.width) + scrollX;
    const y = r.top + p.data.fy * r.height + scrollY;
    let el = cursorEls.get(p.id);
    if (!el) {
      el = h("div", { class: "notes-cursor" }, pointer(), h("span"));
      cursorsEl.append(el);
      cursorEls.set(p.id, el);
    }
    el.style.setProperty("--c", who(p.principal).color);
    el.querySelector("span").textContent = presentName(p.principal);
    // a label that would cross the right edge hangs to the left instead
    el.classList.toggle("flip", x - scrollX > innerWidth - 140);
    el.style.transform = `translate(${x}px, ${y}px)`;
    seen.add(p.id);
  }
  for (const [id, el] of cursorEls) {
    if (!seen.has(id)) {
      el.remove();
      cursorEls.delete(id);
    }
  }

  const people = new Map();
  for (const p of present) if (!people.has(p.principal)) people.set(p.principal, p);
  hereEl.hidden = people.size < 2;
  if (!hereEl.hidden) {
    const faces = [...people.keys()].slice(0, 5).map((pr) => {
      const name = pr === me?.principal ? "You" : presentName(pr);
      const face = avatar(pr, name);
      face.title = name;
      return face;
    });
    hereEl.replaceChildren(h("span", { class: "faces" }, faces), h("span", {}, `${people.size} here`));
  }
}

// ---- start
new ResizeObserver(() => layout()).observe(doc);
addEventListener("resize", () => {
  if (measureWide() !== isWide) {
    hideNoteBtn();
    render();
  } else {
    drawPresence();
  }
});
document.fonts?.ready.then(layout);

fragment.live("notes", {}, (r) => { notes = r.notes; loadError = null; render(); }, (err) => { loadError = err; render(); });
fragment.me().then((m) => { me = m; render(); });
if (cursorsOn) {
  document.addEventListener("pointermove", (e) => {
    if (e.pointerType === "mouse") fragment.presence.set(pointerPos(e));
  });
  document.documentElement.addEventListener("mouseleave", () => fragment.presence.set({}));
  document.addEventListener("visibilitychange", () => fragment.presence.set(document.hidden ? null : {}));
  fragment.presence.on((list) => { present = list; for (const p of list) want(p.principal); drawPresence(); });
  fragment.presence.set({});
}
render();
