// The mind's memory (docs/optchat.md): UniiChat's design (VictorTaelin's
// gist of 2026-10-08, kept as ~/dev/finite/uniichat-spec.md on the build
// box), its sections 1 to 4. Pure: no SQLite, no clock, no model. app.mjs
// keeps one Mem on its instance, loaded from what it saved (`load`), and
// changes it only through these functions, writing what each did
// (`mem.journal`) in the same mutation.
//
// A node (l, i) covers messages [i·2^l, (i+1)·2^l) and is named `id+n`:
// id = i·2^l, its first message, and n = 2^l, how many it covers. Level 0
// compresses one message; a parent merges its two children. A node whose
// source fits in NODE bytes is that source, with no model call (§2).
//
// Two views tile the log [0, T) with nodes, oldest first (§3, §4):
// - the chat's view (`mem.v`), which every turn sees. Each new message
//   appends its line and nothing else; once the view passes VIEW_HIGH bytes
//   one batch merges the most due pairs until it is at most VIEW_LOW (§3.2,
//   the sawtooth), and a batch that cannot get there yet merges what it can
//   at each new message until it does;
// - the compaction view (`mem.c`), which every compaction sees: the chat's
//   merged further, by the same sawtooth from past CVIEW_HIGH down to
//   CVIEW_LOW, made again from the chat's view whenever that one merges.
// A merge joins the most due pair of sibling lines whose parent is built,
// the oldest of equal pairs first (`mergeDue`). Only a level-0 line can be
// unbuilt (a parent joins a view only once built); an unbuilt line counts
// no bytes, and no call ever sees one (§4: a view stops at the first).
//
// What the compactor may build is tracked as it happens, never found by a
// scan of the tree (§7.13): each message not free is `ready` when it is
// logged, and a merge when its second child is built. app.mjs keeps them in
// a table and picks among them (§4, "The order").

export const NODE = 512;
// The chat's view (§3.2), and the compaction view (§4), in bytes.
export const VIEW_HIGH = 128_000;
export const VIEW_LOW = 64_000;
export const CVIEW_HIGH = 32_000;
export const CVIEW_LOW = 16_000;
// Compactions at once (§4, "The order"): a message's node starts once
// fewer than JOBS messages before it are unbuilt.
export const JOBS = 8;
export const TRIES = 5;
// A tool's output is clipped to this many characters, head and tail (§1);
// any other text past it is logged as several messages in a row.
export const CAP = 30_000;
// zoom(id, 1) answers a message in pages of this many characters, so a
// page and its notes stay within CAP as the turn's echo.
export const ZOOM_PAGE = 24_000;
export const PLACEHOLDER = "(not summarized yet: zoom it)";
// §4: "the ruler is 512 dashes".
export const RULER = "-".repeat(NODE);

/// The UTF-8 size of a string, counted without encoding it (a lone
/// surrogate counts as the 3 bytes of U+FFFD, as TextEncoder writes it).
export function utf8(s) {
  let n = 0;
  for (let k = 0; k < s.length; k++) {
    const c = s.charCodeAt(k);
    if (c < 0x80) n += 1;
    else if (c < 0x800) n += 2;
    else if (c >= 0xd800 && c <= 0xdbff && k + 1 < s.length && (s.charCodeAt(k + 1) & 0xfc00) === 0xdc00) {
      n += 4;
      k++;
    } else n += 3;
  }
  return n;
}

function assert(ok, why) {
  if (!ok) throw new Error(`optmem: ${why}`);
}

// A capped text's note of what was cut fits in this many characters.
const CAP_NOTE_ROOM = 80;

/// At most `max` bytes of `s`, cut at a byte offset without splitting a
/// character (the decoder's trailing U+FFFD for a cut one is dropped).
export function cutBytes(s, max) {
  const b = new TextEncoder().encode(s);
  if (b.length <= max) return s;
  const t = new TextDecoder().decode(b.subarray(0, Math.max(0, max)));
  return t.endsWith("�") ? t.slice(0, -1) : t;
}

/// At most `max` characters of a tool's output, its head and tail kept
/// with a note of what was cut between them (§1). Only tool output is cut.
export function capText(text, max = CAP) {
  const s = String(text);
  if (s.length <= max) return s;
  const cps = Array.from(s);
  if (cps.length <= max) return s;
  assert(max > CAP_NOTE_ROOM * 2, "a cap leaves room for its note");
  const room = max - CAP_NOTE_ROOM;
  const head = Math.ceil(room / 2);
  const tail = room - head;
  const cut = cps.length - head - tail;
  return `${cps.slice(0, head).join("")}\n\n[… ${cut} characters cut here …]\n\n${cps.slice(cps.length - tail).join("")}`;
}

/// A text past `max` characters as pieces of at most `max` each, every
/// piece but the last ending at a line's end (else a space) in its last
/// fifth when it has one, never inside a character; joined, the pieces are
/// the text (§1: a long text is never cut, it is logged as several
/// messages in a row).
export function splitText(text, max = CAP) {
  const s = String(text);
  if (s.length <= max) return [s];
  const cps = Array.from(s);
  if (cps.length <= max) return [s];
  assert(max >= 10, "a piece holds some characters");
  const out = [];
  let at = 0;
  // bounded: each piece takes at least a fifth of max characters
  while (cps.length - at > max) {
    const floor = at + Math.floor(max * 0.8);
    let end = at + max;
    let cut = -1;
    for (let k = end - 1; k >= floor && cut < 0; k--) if (cps[k] === "\n") cut = k + 1;
    for (let k = end - 1; k >= floor && cut < 0; k--) if (cps[k] === " ") cut = k + 1;
    if (cut > at) end = cut;
    out.push(cps.slice(at, end).join(""));
    at = end;
  }
  out.push(cps.slice(at).join(""));
  return out;
}

/// A text on one line: each newline a single space (§3).
export const flat = (s) => String(s).replace(/\r\n|\r|\n/g, " ");

/// A message as the tree's level 0 reads it: `kind: text`.
export const line0 = (kind, text) => `${kind}: ${text}`;

export const key = (l, i) => `${l}:${i}`;
export const startOf = (p) => p.i * 2 ** p.l;
/// The last message a node covers.
export const endOf = (l, i) => (i + 1) * 2 ** l - 1;
/// A node's name, `id+n` (§2).
export const nameOf = (l, i) => `${i * 2 ** l}+${2 ** l}`;
const isPow2 = (n) => Number.isSafeInteger(n) && n >= 1 && 2 ** Math.round(Math.log2(n)) === n;
const digits = (n) => String(n).length;

// ---- the memory ----

function newView(tag, high, low) {
  return { tag, high, low, lines: [], bytes: 0, shrink: false };
}

/// An empty memory: no messages, no nodes, empty views.
export function newMem() {
  return { T: 0, text: [[]], size: [[]], v: newView("v", VIEW_HIGH, VIEW_LOW), c: newView("c", CVIEW_HIGH, CVIEW_LOW), journal: [] };
}

function note(mem, entry) {
  if (mem.journal) mem.journal.push(entry);
}

export function isBuilt(mem, l, i) {
  return l < mem.text.length && mem.text[l][i] !== undefined;
}

/// A node's text, or null when it is not built.
export function built(mem, l, i) {
  return isBuilt(mem, l, i) ? mem.text[l][i] : null;
}

function store(mem, l, i, text) {
  while (mem.text.length <= l) {
    mem.text.push([]);
    mem.size.push([]);
  }
  mem.text[l][i] = text;
  mem.size[l][i] = utf8(text);
}

/// A view line's bytes as a call sees it: `id+n|text` and its newline; an
/// unbuilt line, none (no call sees it).
function lineBytes(mem, p) {
  if (!isBuilt(mem, p.l, p.i)) return 0;
  const s = p.i * 2 ** p.l;
  return digits(s) + 1 + digits(2 ** p.l) + 1 + mem.size[p.l][p.i] + 1;
}

function addLine(mem, v, p) {
  v.lines.push(p);
  v.bytes += lineBytes(mem, p);
  note(mem, ["line", v.tag, startOf(p), p.l]);
}

/// Where in a view's lines the line starting at `s` is, or -1 (its lines
/// are in order of their starts).
function placeOf(v, s) {
  let lo = 0;
  let hi = v.lines.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    const at = startOf(v.lines[mid]);
    if (at === s) return mid;
    if (at < s) lo = mid + 1;
    else hi = mid - 1;
  }
  return -1;
}

// A node built: kept, its view lines' bytes made real, then its parent
// readied, or built at once when free (§2: two short lines are joined by a
// newline).
function made(mem, l, i, text) {
  store(mem, l, i, text);
  note(mem, ["node", l, i, text]);
  if (l === 0) {
    for (const v of [mem.v, mem.c]) {
      const k = placeOf(v, i);
      if (k >= 0 && v.lines[k].l === 0) v.bytes += lineBytes(mem, v.lines[k]);
    }
  }
  const sib = i ^ 1;
  if (!isBuilt(mem, l, sib)) return;
  const p = i >> 1;
  const both = `${mem.text[l][2 * p]}\n${mem.text[l][2 * p + 1]}`;
  if (utf8(both) <= NODE) made(mem, l + 1, p, both);
  else note(mem, ["ready", l + 1, p]);
}

// ---- the merge order (§3.1, §3.2) ----

/// A pair's due (§3.2): how long ago it ended, measured in its own lines'
/// size, `(T - last) / 2^l`; written as the spec's code writes it, `(T +
/// 1)/2^l - i` with `i` the left line's place among level l's, the same
/// order shifted by 2.
export const dueOf = (T, l, i) => (T + 1) / 2 ** l - i;

// A binary heap of candidate pairs, the most due first, the oldest of
// equal ones first.
const before = (a, b) => a.due > b.due || (a.due === b.due && a.s < b.s);

function heapPush(h, x) {
  h.push(x);
  let k = h.length - 1;
  while (k > 0) {
    const up = (k - 1) >> 1;
    if (!before(h[k], h[up])) break;
    [h[k], h[up]] = [h[up], h[k]];
    k = up;
  }
}

function heapPop(h) {
  const top = h[0];
  const last = h.pop();
  if (h.length) {
    h[0] = last;
    let k = 0;
    for (;;) {
      const a = 2 * k + 1;
      const b = a + 1;
      let m = k;
      if (a < h.length && before(h[a], h[m])) m = a;
      if (b < h.length && before(h[b], h[m])) m = b;
      if (m === k) break;
      [h[k], h[m]] = [h[m], h[k]];
      k = m;
    }
  }
  return top;
}

/// §3.2: merges the most due pair of sibling lines among `lines` (oldest
/// first, each {l, i}) whose parent `parentBuilt(l + 1, i / 2)` is, the
/// oldest of equal pairs first, and again, while `size` is over `target`.
/// `sizeOf(line)` is a line's share of the size. Answers the lines after,
/// their size, and each merge as {s, l, drop}: the merged line's start and
/// its new level, and the start of the line it took in.
export function mergeDue(lines, T, parentBuilt, sizeOf, size, target) {
  const merges = [];
  if (size <= target || lines.length < 2) return { lines, size, merges };
  const n = lines.length;
  const at = lines.map((p) => ({ l: p.l, i: p.i, ver: 0 }));
  const next = new Int32Array(n);
  const prev = new Int32Array(n);
  for (let k = 0; k < n; k++) {
    next[k] = k + 1 < n ? k + 1 : -1;
    prev[k] = k - 1;
  }
  const alive = new Uint8Array(n).fill(1);
  const heap = [];
  const consider = (k) => {
    if (k < 0 || next[k] < 0) return;
    const a = at[k];
    const b = at[next[k]];
    if (a.l === b.l && a.i % 2 === 0 && b.i === a.i + 1 && parentBuilt(a.l + 1, a.i / 2)) {
      heapPush(heap, { k, ver: a.ver, j: next[k], jver: b.ver, due: dueOf(T, a.l, a.i), s: a.i * 2 ** a.l });
    }
  };
  for (let k = 0; k < n; k++) consider(k);
  // bounded: each merge takes a line away
  while (size > target && heap.length) {
    const c = heapPop(heap);
    const a = at[c.k];
    if (!alive[c.k] || a.ver !== c.ver || next[c.k] !== c.j || !alive[c.j] || at[c.j].ver !== c.jver) continue;
    const b = at[c.j];
    size -= sizeOf(a) + sizeOf(b);
    const drop = b.i * 2 ** b.l;
    a.l += 1;
    a.i /= 2;
    a.ver++;
    alive[c.j] = 0;
    next[c.k] = next[c.j];
    if (next[c.j] >= 0) prev[next[c.j]] = c.k;
    size += sizeOf(a);
    merges.push({ s: c.s, l: a.l, drop });
    consider(prev[c.k]);
    consider(c.k);
  }
  const out = [];
  for (let k = 0; k >= 0 && k < n; k = next[k]) out.push({ l: at[k].l, i: at[k].i });
  return { lines: out, size, merges };
}

// A view merged by the most due pairs down to `target`; answers how many
// merges it made.
function shrink(mem, v, target) {
  const r = mergeDue(v.lines, mem.T, (l, i) => isBuilt(mem, l, i), (p) => lineBytes(mem, p), v.bytes, target);
  v.lines = r.lines;
  v.bytes = r.size;
  for (const m of r.merges) {
    note(mem, ["line", v.tag, m.s, m.l]);
    note(mem, ["drop", v.tag, m.drop]);
  }
  return r.merges.length;
}

// The sawtooth (§3.2): past `high`, a batch merges down to `low`; one that
// cannot get there yet goes on at the next chance until it does.
function sawtooth(mem, v) {
  if (!v.shrink && v.bytes > v.high) {
    v.shrink = true;
    note(mem, ["shrink", v.tag, true]);
  }
  if (!v.shrink) return 0;
  const n = shrink(mem, v, v.low);
  if (v.bytes <= v.low) {
    v.shrink = false;
    note(mem, ["shrink", v.tag, false]);
  }
  return n;
}

// The compaction view made again from the chat's (§4: "merged again …
// when the chat's view merges"), to be merged down to CVIEW_LOW.
function remakeC(mem) {
  const c = mem.c;
  c.lines = mem.v.lines.map((p) => ({ l: p.l, i: p.i }));
  c.bytes = mem.v.bytes;
  note(mem, ["clear", "c"]);
  for (const p of c.lines) note(mem, ["line", "c", startOf(p), p.l]);
  c.shrink = true;
  note(mem, ["shrink", "c", true]);
}

/// Both views fitted: the chat's by its sawtooth, and the compaction view
/// made again from it when it merged, else by its own sawtooth. Answers
/// whether the chat's view merged. A new message's (`append`), and a turn's
/// just before it renders its view, after its wait (docs/optchat.md: an
/// import's backlog built since its last message would otherwise reach the
/// turn unmerged).
export function fit(mem) {
  const merged = sawtooth(mem, mem.v) > 0;
  if (merged) remakeC(mem);
  sawtooth(mem, mem.c);
  return merged;
}

/// A new message, its line `kind: text` (with its files): its line
/// appended to both views, its node built at once when the line fits NODE
/// (§2) or else ready for the compactor, and the views fitted (§3.2: a new
/// message is when the chat's view merges). Answers the message's id.
export function append(mem, line) {
  assert(typeof line === "string", "a message's line is text");
  const i = mem.T++;
  addLine(mem, mem.v, { l: 0, i });
  addLine(mem, mem.c, { l: 0, i });
  if (utf8(line) <= NODE) made(mem, 0, i, line);
  else note(mem, ["ready", 0, i]);
  fit(mem);
  return i;
}

/// A node the compactor built: kept, then the compaction view fitted (it
/// is merged again "once it passes 32 KB", §4, and a node built is what
/// makes it grow between messages). The chat's view merges only at a new
/// message. Nodes are written once, each after its sources.
export function setNode(mem, l, i, text) {
  assert(typeof text === "string" && text.length > 0, "a node has text");
  assert(!isBuilt(mem, l, i), `node ${nameOf(l, i)} is written once`);
  assert((i + 1) * 2 ** l <= mem.T, `node ${nameOf(l, i)} covers messages that exist`);
  assert(l === 0 || (isBuilt(mem, l - 1, 2 * i) && isBuilt(mem, l - 1, 2 * i + 1)), "a parent is built after its children");
  made(mem, l, i, text);
  sawtooth(mem, mem.c);
}

/// The first message whose chat-view line is unbuilt, or T: every message
/// before it is summarized.
export function firstUnbuilt(mem) {
  for (const p of mem.v.lines) if (!isBuilt(mem, p.l, p.i)) return startOf(p);
  return mem.T;
}

// ---- saved and loaded (§3.2: "Save the view … never rebuild it") ----

// A view's lines from their saved rows ({s, l}, in order of `s`), checked
// to tile [0, T) with nodes, every line above level 0 built.
function viewOf(mem, v, rows) {
  let at = 0;
  for (const r of rows) {
    const n = 2 ** r.l;
    assert(Number.isSafeInteger(r.s) && Number.isSafeInteger(r.l) && r.l >= 0 && r.s === at && r.s % n === 0, `view ${v.tag}: a line ${r.s}+${n} where ${at} was due`);
    assert(r.l === 0 || isBuilt(mem, r.l, r.s / n), `view ${v.tag}: line ${r.s}+${n} is a node not built`);
    v.lines.push({ l: r.l, i: r.s / n });
    at += n;
  }
  assert(at === mem.T, `view ${v.tag} covers ${at} of the log's ${mem.T} messages`);
  v.bytes = v.lines.reduce((b, p) => b + lineBytes(mem, p), 0);
}

/// A memory as it was saved: T messages, the nodes built (any iterable of
/// {l, i, text}), each view's lines ({s, l}, by start) and whether its
/// sawtooth is merging.
export function load({ T, nodes, v, c, vshrink = false, cshrink = false }) {
  assert(Number.isSafeInteger(T) && T >= 0, "T is a count");
  const mem = newMem();
  mem.T = T;
  for (const n of nodes) {
    assert(Number.isSafeInteger(n.l) && n.l >= 0 && Number.isSafeInteger(n.i) && n.i >= 0, "a node's place is two counts");
    assert((n.i + 1) * 2 ** n.l <= T, `node ${nameOf(n.l, n.i)} covers messages past the log's ${T}`);
    store(mem, n.l, n.i, n.text);
  }
  viewOf(mem, mem.v, v);
  viewOf(mem, mem.c, c);
  mem.v.shrink = vshrink;
  mem.c.shrink = cshrink;
  return mem;
}

/// A memory that was never saved (a mind made before the views were):
/// built once from its log as it stands (T messages, the nodes built, and
/// `line(i)` for a message's `kind: text`), its messages appended in order
/// and its views fitted as they would have grown; then every node ready
/// now, found by one scan of the tree, the free ones built. Answers the
/// memory, whose journal holds the nodes it built and the nodes ready; its
/// views are for the caller to save whole.
export function fold(T, nodes, line) {
  assert(Number.isSafeInteger(T) && T >= 0, "T is a count");
  const mem = newMem();
  mem.journal = null;
  for (const n of nodes) {
    assert((n.i + 1) * 2 ** n.l <= T, `node ${nameOf(n.l, n.i)} covers messages past the log's ${T}`);
    store(mem, n.l, n.i, n.text);
  }
  for (let i = 0; i < T; i++) {
    mem.T = i + 1;
    addLine(mem, mem.v, { l: 0, i });
    addLine(mem, mem.c, { l: 0, i });
    fit(mem);
  }
  mem.journal = [];
  for (let l = 0; 2 ** l <= T; l++) {
    for (let i = 0; (i + 1) * 2 ** l <= T; i++) {
      if (isBuilt(mem, l, i)) continue;
      if (l === 0) {
        const t = line(i);
        if (utf8(t) <= NODE) made(mem, 0, i, t);
        else note(mem, ["ready", 0, i]);
      } else if (isBuilt(mem, l - 1, 2 * i) && isBuilt(mem, l - 1, 2 * i + 1)) {
        // a free one was built when its second child was; this is a call's
        note(mem, ["ready", l, i]);
      }
    }
  }
  return mem;
}

// ---- what calls see ----

// A view's lines that start before `end` and end by it, as a call sees
// them (§3): `<chat>`, a line `id+n|text` per node with newlines as spaces,
// stopping at the first unbuilt one (§4: no call sees a placeholder or
// half a message). `settled` says none was left out for being unbuilt.
function renderLines(mem, lines, end) {
  const out = [];
  let settled = true;
  for (const p of lines) {
    const s = startOf(p);
    if (s >= end || s + 2 ** p.l > end) break;
    if (!isBuilt(mem, p.l, p.i)) {
      settled = false;
      break;
    }
    out.push(`${s}+${2 ** p.l}|${flat(mem.text[p.l][p.i])}`);
  }
  const text = `<chat>\n${out.map((l) => `${l}\n`).join("")}</chat>`;
  return { text, bytes: utf8(text), parts: out.length, settled };
}

/// The chat's view as a turn sees it (§3): the lines before `upto` (all by
/// default), up to the first not built.
export function render(mem, upto = mem.T) {
  return renderLines(mem, mem.v.lines, upto);
}

/// The view as parts, for a page: every line, a placeholder for the unbuilt.
export function parts(mem) {
  return mem.v.lines.map((p) => {
    const b = isBuilt(mem, p.l, p.i);
    return { id: startOf(p), n: 2 ** p.l, text: b ? mem.text[p.l][p.i] : PLACEHOLDER, built: b };
  });
}

/// Whether id+n names a node of this log (§6's zoom: n a power of 2, id a
/// multiple of n).
export function isLine(mem, id, n) {
  return Number.isSafeInteger(id) && id >= 0 && isPow2(n) && id % n === 0 && id + n <= mem.T;
}

/// §6: line id+n opened into the two lines of n/2 under it (an unbuilt one
/// shows as the placeholder), or for n = 1 the message whole, in pages of
/// ZOOM_PAGE characters (`page` from 1). `message(id)` is {kind, text,
/// more, cont}: its words with its files, and whether the next message
/// goes on with its text or it goes on from the one before (a long text is
/// several messages in a row, §1).
export function zoom(mem, id, n, message, page = 1) {
  if (!isLine(mem, id, n)) return `No line ${id}+${n}.`;
  if (n > 1) {
    const l = Math.round(Math.log2(n));
    const h = n / 2;
    const i = id / n;
    return [2 * i, 2 * i + 1].map((c) => `${c * h}+${h}|${isBuilt(mem, l - 1, c) ? flat(mem.text[l - 1][c]) : PLACEHOLDER}`).join("\n");
  }
  const m = message(id);
  const pages = splitText(`${m.kind}: ${m.text}`, ZOOM_PAGE);
  if (!Number.isSafeInteger(page) || page < 1 || page > pages.length) return `Message ${id} has ${pages.length} page${pages.length > 1 ? "s" : ""}.`;
  const notes = [];
  if (pages.length > 1) notes.push(page < pages.length ? `[page ${page} of ${pages.length}: zoom(${id}, 1, ${page + 1}) gives the next]` : `[page ${page} of ${pages.length}]`);
  if (m.cont && page === 1) notes.push(`[it goes on from message ${id - 1}]`);
  if (m.more && page === pages.length) notes.push(`[it goes on in message ${id + 1}]`);
  return `${id}+0|${pages[page - 1]}${notes.map((x) => `\n${x}`).join("")}`;
}

/// The smallest built node that covers messages a to b (inclusive), or
/// null: when that one is not built, no larger one is.
export function covering(mem, a, b) {
  for (let l = 0; 2 ** l <= mem.T; l++) {
    const n = 2 ** l;
    const i = Math.floor(a / n);
    if (Math.floor(b / n) !== i) continue;
    if ((i + 1) * n > mem.T || !isBuilt(mem, l, i)) return null;
    return { id: i * n, n, text: mem.text[l][i] };
  }
  return null;
}

// ---- compactions (§4) ----

/// What one compaction sees after the system prompt: the compaction view
/// up to the node, built lines only (its message's line excluded; for a
/// merge, the lines up to its last message), and its task, verbatim from
/// §4 (`line(i)` is message i's `kind: text`, whole).
export function compaction(mem, l, i, line) {
  assert((i + 1) * 2 ** l <= mem.T && !isBuilt(mem, l, i), `${nameOf(l, i)} is a node to build`);
  if (l === 0) {
    const view = renderLines(mem, mem.c.lines, i).text;
    const task = `Compaction: compress message ${i} into one line of at most 512 bytes
(about 70 words), the length of this ruler:
${RULER}
<input>
${line(i)}
</input>`;
    return { view, task };
  }
  assert(isBuilt(mem, l - 1, 2 * i) && isBuilt(mem, l - 1, 2 * i + 1), `${nameOf(l, i)}'s halves are built`);
  const view = renderLines(mem, mem.c.lines, (i + 1) * 2 ** l).text;
  const [a, b] = [2 * i, 2 * i + 1].map((c) => `${nameOf(l - 1, c)}|${flat(mem.text[l - 1][c])}`);
  const task = `Compaction: merge lines ${nameOf(l - 1, 2 * i)} and ${nameOf(l - 1, 2 * i + 1)}, adjacent, into one line of at most
512 bytes (about 70 words), the length of this ruler:
${RULER}
<chat> may hold their messages, ${i * 2 ** l} to ${endOf(l, i)}, in more detail: take details
of them from there too.
<input>
${a}
${b}
</input>`;
  return { view, task };
}

/// §4's answer to a line over NODE, sent in the same conversation.
export function tooLong(line) {
  return `Too long: your line is ${utf8(line)} bytes, over the 512-byte limit. Write
the whole line again for the same <input>, cutting just enough of the
least valuable items to fit before this cut:
${cutBytes(line, NODE)}| ← LIMIT`;
}

/// §4, one reply: trimmed, with an `id+n|` head it should not have written
/// taken off, and kept as a try; done when it fits or at TRIES (the
/// shortest try wins, the first of equals), else the retry to send in the
/// same conversation. An empty reply fails the node.
export function compactTry(tries, reply) {
  const line = String(reply ?? "")
    .trim()
    .replace(/^\d+\+\d+\|\s*/, "");
  if (!line) return { fail: "the compactor answered nothing" };
  const all = [...tries, line];
  if (utf8(line) <= NODE || all.length >= TRIES) {
    let best = all[0];
    for (const t of all) if (utf8(t) < utf8(best)) best = t;
    return { tries: all, text: best };
  }
  return { tries: all, retry: tooLong(line) };
}
