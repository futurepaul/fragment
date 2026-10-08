// Small pieces every screen of the mind's page uses: building DOM, keyed
// updates, times, and reading the mind's own text formats (summary lines,
// view lines, tool calls) for a person.

import { icon } from "./icons.js";
import { renderMarkdown } from "./markdown.js";

export { icon };

/// `h("div.a.b", {props}, ...children)`: an element. `text` sets its text,
/// `on<event>` listens, `value`, `hidden`, `disabled` and `checked` are
/// set as properties, anything else as an attribute (`true`: present).
export function h(tag, props, ...kids) {
  const [name, ...classes] = tag.split(".");
  const e = document.createElement(name || "div");
  if (classes.length) e.className = classes.join(" ");
  if (props) {
    for (const [k, v] of Object.entries(props)) {
      if (v === undefined || v === null || v === false) continue;
      if (k === "class") e.className = `${e.className} ${v}`.trim();
      else if (k === "text") e.textContent = v;
      else if (k.startsWith("on") && typeof v === "function") e.addEventListener(k.slice(2), v);
      else if (k === "dataset") Object.assign(e.dataset, v);
      else if (k === "style") e.style.cssText = v;
      else if (k === "value" || k === "hidden" || k === "disabled" || k === "checked") e[k] = v;
      else e.setAttribute(k, v === true ? "" : String(v));
    }
  }
  for (const kid of kids.flat(Infinity)) {
    if (kid === undefined || kid === null || kid === false) continue;
    e.append(kid instanceof Node ? kid : String(kid));
  }
  return e;
}

/// Brings `parent`'s children to `items`, in order, keeping the node of a
/// key whose `sig` is unchanged. A changed one is patched by `update(node)`
/// when the item has one, else made again. New nodes after the first pass
/// get the class `enter` (their arrival animates), unless `quiet` (or the
/// item is: it takes the place of something already shown).
export function reconcile(parent, items, { quiet = false } = {}) {
  const old = new Map();
  for (const n of parent.children) if (n.__key !== undefined) old.set(n.__key, n);
  const keep = new Set();
  let prev = null;
  for (const it of items) {
    let n = old.get(it.key);
    if (n && n.__sig !== it.sig) {
      if (it.update) it.update(n);
      else {
        const fresh = it.make();
        n.replaceWith(fresh);
        n = fresh;
      }
      n.__key = it.key;
      n.__sig = it.sig;
    } else if (!n) {
      n = it.make();
      n.__key = it.key;
      n.__sig = it.sig;
      if (!quiet && !it.quiet && parent.__painted) n.classList.add("enter");
    }
    keep.add(n);
    const want = prev ? prev.nextSibling : parent.firstChild;
    if (want !== n) parent.insertBefore(n, want);
    prev = n;
  }
  for (const n of [...parent.children]) if (!keep.has(n)) n.remove();
  parent.__painted = true;
}

/// Markdown in a `.md` block.
export function md(text, cls = "") {
  const box = h(`div.md${cls ? "." + cls : ""}`);
  box.append(renderMarkdown(text));
  return box;
}

// ---- times ----
const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

function startOfDay(t) {
  const d = new Date(t);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/// "now", "5m", "3h", "Tue", "Sep 12", "Sep 2025": a list's short time.
export function ago(at, now = Date.now()) {
  if (!Number.isFinite(at)) return "";
  const d = now - at;
  if (d < MIN) return "now";
  if (d < HOUR) return `${Math.floor(d / MIN)}m`;
  if (d < DAY && startOfDay(at) === startOfDay(now)) return `${Math.floor(d / HOUR)}h`;
  if (d < 6 * DAY) return new Date(at).toLocaleDateString([], { weekday: "short" });
  if (new Date(at).getFullYear() === new Date(now).getFullYear()) return new Date(at).toLocaleDateString([], { month: "short", day: "numeric" });
  return new Date(at).toLocaleDateString([], { month: "short", year: "numeric" });
}

export const clock = (at) => new Date(at).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

/// "Today", "Yesterday", "Tuesday", "September 12", "September 12, 2025".
export function dayLabel(at, now = Date.now()) {
  const days = Math.round((startOfDay(now) - startOfDay(at)) / DAY);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 6) return new Date(at).toLocaleDateString([], { weekday: "long" });
  const sameYear = new Date(at).getFullYear() === new Date(now).getFullYear();
  return new Date(at).toLocaleDateString([], sameYear ? { month: "long", day: "numeric" } : { month: "long", day: "numeric", year: "numeric" });
}

/// A time with its day: "Today, 9:41 AM", "Sep 12, 9:41 AM".
export const when = (at) => `${dayLabel(at)}, ${clock(at)}`;

/// The stretch of time a list groups by: "Today", "Yesterday", "This week",
/// "Earlier this month", then "September 2026".
export function period(at, now = Date.now()) {
  const days = Math.round((startOfDay(now) - startOfDay(at)) / DAY);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return "This week";
  const a = new Date(at);
  const n = new Date(now);
  if (a.getMonth() === n.getMonth() && a.getFullYear() === n.getFullYear()) return "Earlier this month";
  return a.toLocaleDateString([], { month: "long", year: "numeric" });
}

/// The greeting for the hour.
export function greeting(now = new Date()) {
  const hr = now.getHours();
  if (hr < 5) return "Up late";
  if (hr < 12) return "Good morning";
  if (hr < 18) return "Good afternoon";
  return "Good evening";
}

// ---- the mind's text, for a person ----

/// A summary line for a person: its kind tags go, and the items they began
/// are set apart by dashes ("user: …; talk: …" reads "… — …").
export function cleanSummary(text) {
  return String(text ?? "")
    .replace(/\s+/g, " ")
    .replace(/\*\*|`/g, "")
    .replace(/\*([^*\s][^*]*?)\*/g, "$1")
    .replace(/\s*;\s*(user|talk|tool|echo|note|work)\s*:\s*/gi, " — ")
    .replace(/^\s*(user|talk|tool|echo|note|work)\s*:\s*/i, "")
    .trim();
}

/// Words of `q` marked inside `root`'s text (after it is rendered).
export function markIn(root, q) {
  const words = [...new Set(String(q ?? "").toLowerCase().match(/[\p{L}\p{N}]{2,}/gu) ?? [])];
  if (!words.length) return root;
  const re = new RegExp(`(${words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`, "giu");
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const texts = [];
  while (walker.nextNode()) texts.push(walker.currentNode);
  for (const t of texts) {
    if (!re.test(t.data)) continue;
    re.lastIndex = 0;
    t.replaceWith(highlight(t.data, q));
  }
  return root;
}

/// A thread id: `t_` and 16 hex (docs/optchat.md, `say`).
export function threadId() {
  const b = crypto.getRandomValues(new Uint8Array(8));
  return `t_${[...b].map((x) => x.toString(16).padStart(2, "0")).join("")}`;
}

export function firstLine(text, max = 80) {
  const line = String(text ?? "").trim().split("\n")[0].trim();
  return line.length > max ? `${line.slice(0, max - 1).trimEnd()}…` : line;
}

const safeJson = (s) => {
  try {
    return JSON.parse(s);
  } catch {
    return null;
  }
};

/// A `tool` message (the call's name and its JSON input) as `{name, args}`,
/// read leniently: `zoom {"id":1,"n":2}`, `zoom({…})`, `zoom: {…}`, or
/// `{"name": "zoom", "arguments": …}`.
export function parseTool(text) {
  const t = String(text ?? "").trim();
  const whole = safeJson(t);
  if (whole && typeof whole === "object" && typeof whole.name === "string") {
    const a = whole.arguments ?? whole.input ?? whole.args ?? {};
    return { name: whole.name, args: typeof a === "string" ? safeJson(a) ?? { text: a } : a };
  }
  const m = t.match(/^([A-Za-z_][\w.-]*)\s*[:(]?\s*([\s\S]*?)\)?\s*$/);
  if (!m) return { name: "tool", args: { text: t } };
  const args = m[2] ? safeJson(m[2]) : {};
  return { name: m[1], args: args && typeof args === "object" ? args : { text: m[2] } };
}

/// The mind's tools that look things up on the web (applib/web.mjs).
export const WEB_TOOLS = new Set(["web_search", "web_fetch", "research"]);

/// A URL's host as a person reads it ("example.com"), or "".
export function hostOf(url) {
  try {
    return new URL(String(url)).hostname.replace(/^www\./, "");
  } catch {
    return "";
  }
}

/// The `id+n|text` lines of a view, a zoom or a search answer.
export function viewLines(text) {
  const out = [];
  for (const line of String(text ?? "").split("\n")) {
    const m = line.match(/^(\d+)\+(\d+)\|(.*)$/);
    if (m) out.push({ id: Number(m[1]), n: Number(m[2]), text: m[3] });
  }
  return out;
}

/// A hand-off's report as the log holds it: a `work` message `[<task>] …`
/// (before the mind followed UniiChat, a `user` one).
export function reportOf(text) {
  const m = String(text ?? "").match(/^\[([A-Za-z0-9_:.-]{2,64})\] ([\s\S]*)$/);
  return m ? { task: m[1], text: m[2] } : null;
}

/// Whether a message may be a hand-off's report: `work`, or (logged before
/// that kind was) a `user` message `[<task>] …`.
export const isReportKind = (m) => m.kind === "work" || m.kind === "user";

/// A thread's messages with each long text the log holds as several in a
/// row (each piece after the first `cont`) joined back into one.
export function joined(list) {
  const out = [];
  for (const m of list) {
    const prev = out.at(-1);
    if (m.cont && prev && prev.kind === m.kind && prev.i + prev.pieces === m.i) {
      out[out.length - 1] = { ...prev, text: prev.text + m.text, pieces: prev.pieces + 1 };
      continue;
    }
    out.push({ ...m, pieces: 1 });
  }
  return out;
}

/// `text` with the words of `q` marked.
export function highlight(text, q) {
  const frag = document.createDocumentFragment();
  // an FTS snippet's own markers go: the page marks the words itself
  const plain = String(text ?? "").replace(/<\/?(b|mark|em)>/gi, "");
  const words = [...new Set(String(q ?? "").toLowerCase().match(/[\p{L}\p{N}]{2,}/gu) ?? [])];
  if (!words.length) {
    frag.append(plain);
    return frag;
  }
  const re = new RegExp(`(${words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`, "giu");
  let last = 0;
  for (const m of plain.matchAll(re)) {
    if (m.index > last) frag.append(plain.slice(last, m.index));
    frag.append(h("mark", { text: m[0] }));
    last = m.index + m[0].length;
  }
  if (last < plain.length) frag.append(plain.slice(last));
  return frag;
}

export const plural = (n, one, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

/// Bytes as a person reads them (decimal: the view's budget is 128 000).
export function kb(bytes) {
  if (!Number.isFinite(bytes)) return "";
  if (bytes < 1000) return `${bytes} B`;
  return `${Math.round(bytes / 1000)} KB`;
}

/// A file's size: "812 B", "40 KB", "3.4 MB".
export function size(bytes) {
  if (!Number.isFinite(bytes) || bytes < 1_000_000) return kb(bytes);
  return `${(bytes / 1_000_000).toFixed(bytes < 10_000_000 ? 1 : 0)} MB`;
}

/// A button with an icon and an accessible name.
export function iconButton(name, label, onclick, cls = "") {
  return h(`button.icon-btn${cls ? "." + cls : ""}`, { type: "button", "aria-label": label, title: label, onclick }, icon(name));
}

/// Copies `text`; the button says so for a moment.
export function copyButton(text, label = "Copy") {
  const b = h("button.copy", { type: "button", "aria-label": label, title: label }, icon("copy"));
  b.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(text);
      b.replaceChildren(icon("check"));
      b.classList.add("done");
      setTimeout(() => {
        b.replaceChildren(icon("copy"));
        b.classList.remove("done");
      }, 1400);
    } catch {
      b.title = "Copy it by hand";
    }
  });
  return b;
}
