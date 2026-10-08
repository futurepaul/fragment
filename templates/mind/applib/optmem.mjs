// The mind's memory (docs/optchat.md; OptChat's spec, sections 3 to 7):
// the log's binary tree of one-line summaries, the view that tiles the
// log, and what the compactor is asked. Pure: no SQLite, no clock, no
// model. app.mjs keeps one Mem on its instance, folded from the log at
// its first call (`fold`), and changes it only through these functions.
//
// A node (l, i) covers messages [i·2^l, (i+1)·2^l) and is named `id+n`:
// id = i·2^l, its first message, and n = 2^l, how many it covers. Level 0
// summarizes one message; a parent merges its two children. The view is a
// list of parts, tree nodes that tile [0, T) oldest first, appended one
// part per message and coarsened by merging the most due pair (`fit`),
// never split. Only a level-0 part can be unbuilt (a parent enters the view
// only once built), and nodes are built in message order (rule 3 of
// `ready`), so the view's unbuilt parts are always its tail.

export const NODE = 512;
export const VIEW = 128_000;
export const JOBS = 8;
export const TRIES = 5;
export const CAP = 30_000;
export const PLACEHOLDER = "(not summarized yet: zoom it)";
// The batched compactor (docs/optchat.md, "Importing chats": a deviation
// from the spec, which builds one node a call): one call builds up to
// BATCH level-0 nodes in a row, or up to BATCH merges, a line each. A
// level-0 batch's messages are at most BATCH_MAX_BYTES, within the first
// BATCH_WINDOW messages from `first`.
export const BATCH = 8;
export const BATCH_MAX_BYTES = 192 * 1024;
export const BATCH_WINDOW = 64;

// A realistic summary line of exactly NODE bytes (spec 4.2: models cannot
// count bytes; a real example gives them the size). Checked below.
export const SCALE =
  'user: move the blog from Jekyll to Astro, keep every URL as is, drop the comments widget ("nobody uses it"), one PR a step; ' +
  "tool: read _config.yml and 41 posts, 3 have custom permalinks; talk: proposed 301s for those 3 and a new layout; " +
  "user: fine, but /feed.xml must stay byte-identical, a reader app parses it; echo: astro build failed on 2019-04-02 " +
  "(unquoted date in front matter), fixed by quoting; work: preview deployed to staging, 2 images 404 (case mismatch in path); " +
  "user: ship Friday after Anna reviews";

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

assert(utf8(SCALE) === NODE, `SCALE is ${utf8(SCALE)} bytes, not ${NODE}`);

const PLACEHOLDER_BYTES = utf8(PLACEHOLDER);
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

/// At most `max` characters of `text`, its head and tail kept with a note
/// of what was cut between them (spec 7: tool results; here every logged
/// message, docs/optchat.md, "The 16 MiB cap is debt").
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

/// A text on one line: each newline a single space (spec 5.1).
export const flat = (s) => String(s).replace(/\r\n|\r|\n/g, " ");

/// A message as the tree's level 0 reads it: `kind: text`.
export const line0 = (kind, text) => `${kind}: ${text}`;

export const key = (l, i) => `${l}:${i}`;
export const width = (l) => 2 ** l;
export const startOf = (p) => p.i * 2 ** p.l;
const isPow2 = (n) => Number.isSafeInteger(n) && n >= 1 && 2 ** Math.round(Math.log2(n)) === n;

/// An empty memory: no messages, no nodes, an empty view.
export function newMem() {
  return { T: 0, text: [[]], size: [[]], view: [], bytes: 0 };
}

export function isBuilt(mem, l, i) {
  return l < mem.text.length && mem.text[l][i] !== undefined;
}

/// A node's text, or null when it is not built.
export function built(mem, l, i) {
  return isBuilt(mem, l, i) ? mem.text[l][i] : null;
}

function partBytes(mem, p) {
  return isBuilt(mem, p.l, p.i) ? mem.size[p.l][p.i] : PLACEHOLDER_BYTES;
}

function store(mem, l, i, text) {
  while (mem.text.length <= l) {
    mem.text.push([]);
    mem.size.push([]);
  }
  mem.text[l][i] = text;
  mem.size[l][i] = utf8(text);
}

/// A memory of T messages and the nodes built so far, its view folded
/// again from message 0 by the same append + fit as it grew (spec 5.2, "At
/// load"). `nodes` is any iterable of {l, i, text}.
export function fold(T, nodes) {
  assert(Number.isSafeInteger(T) && T >= 0, "T is a count");
  const mem = newMem();
  for (const n of nodes) {
    assert(Number.isSafeInteger(n.l) && n.l >= 0 && Number.isSafeInteger(n.i) && n.i >= 0, "a node's place is two counts");
    assert((n.i + 1) * 2 ** n.l <= T, `node ${n.i * 2 ** n.l}+${2 ** n.l} covers messages past the log's ${T}`);
    assert(typeof n.text === "string" && n.text.length > 0, "a node has text");
    store(mem, n.l, n.i, n.text);
  }
  for (let i = 0; i < T; i++) append(mem);
  return mem;
}

/// A new message: its level-0 part appended, then the view refitted.
/// Answers the message's id.
export function append(mem) {
  const i = mem.T++;
  const p = { l: 0, i };
  mem.view.push(p);
  mem.bytes += partBytes(mem, p);
  fit(mem);
  return i;
}

/// A node built: kept, its part's size made real if it is in the view as a
/// placeholder, then the view refitted. Nodes are written once, each after
/// its sources.
export function setNode(mem, l, i, text) {
  assert(typeof text === "string" && text.length > 0, "a node has text");
  assert(!isBuilt(mem, l, i), `node ${i * 2 ** l}+${2 ** l} is written once`);
  assert((i + 1) * 2 ** l <= mem.T, `node ${i * 2 ** l}+${2 ** l} covers messages that exist`);
  assert(l === 0 || (isBuilt(mem, l - 1, 2 * i) && isBuilt(mem, l - 1, 2 * i + 1)), "a parent is built after its children");
  store(mem, l, i, text);
  if (l === 0) {
    // an unbuilt part is in the view's tail: look from its end
    for (let k = mem.view.length - 1; k >= 0; k--) {
      const p = mem.view[k];
      if (p.l === 0 && p.i === i) {
        mem.bytes += mem.size[0][i] - PLACEHOLDER_BYTES;
        break;
      }
      if (startOf(p) < i) break;
    }
  }
  fit(mem);
}

/// Spec 5.2: while the view is over VIEW bytes, merge the adjacent pair
/// whose parent is built and whose age over its weight is the largest
/// (`due`, OptMem's age rule; the oldest wins a tie). A pair whose parent
/// is not built is passed over; with none left, the view waits over budget.
export function fit(mem) {
  const v = mem.view;
  while (mem.bytes > VIEW) {
    let best = -1;
    let bestDue = -Infinity;
    for (let k = 0; k + 1 < v.length; k++) {
      const a = v[k];
      const b = v[k + 1];
      if (a.l === b.l && a.i % 2 === 0 && b.i === a.i + 1 && isBuilt(mem, a.l + 1, a.i / 2)) {
        const due = (mem.T - a.i * 2 ** a.l) / 2 ** (a.l + 2);
        if (due > bestDue) {
          best = k;
          bestDue = due;
        }
      }
    }
    if (best < 0) break;
    const a = v[best];
    const parent = { l: a.l + 1, i: a.i / 2 };
    mem.bytes += mem.size[parent.l][parent.i] - partBytes(mem, a) - partBytes(mem, v[best + 1]);
    v.splice(best, 2, parent);
  }
}

/// The first message whose view line is unbuilt, or T (spec 4.1, `first`).
export function first(mem) {
  for (const p of mem.view) if (!isBuilt(mem, p.l, p.i)) return startOf(p);
  return mem.T;
}

/// Spec 4.1: the nodes the compactor may build now, in its scan order
/// (level by level from 0, oldest first), at most `max`: each (1) not
/// built nor in `busy` (keys), (2) with its sources (its message; or both
/// children built), and (3) with every view line before its end a summary.
/// Rule 3 makes level 0 one node at a time, in message order.
export function ready(mem, { busy = new Set(), max = JOBS } = {}) {
  const f = first(mem);
  const out = [];
  for (let l = 0; 2 ** l <= mem.T; l++) {
    const n = 2 ** l;
    // below `first`, every message's level-0 node is built
    for (let i = l === 0 ? f : 0; (i + 1) * n <= mem.T; i++) {
      const end = l === 0 ? i : (i + 1) * n;
      if (end > f) break;
      if (isBuilt(mem, l, i) || busy.has(key(l, i))) continue;
      if (l > 0 && !(isBuilt(mem, l - 1, 2 * i) && isBuilt(mem, l - 1, 2 * i + 1))) continue;
      if (out.length >= max) return out;
      out.push({ l, i });
    }
  }
  return out;
}

/// The text a node is without a model call, when its source fits in NODE
/// bytes (spec 3, "Free nodes"); else null. `line(i)` is message i's
/// `kind: text`.
export function freeText(mem, l, i, line) {
  if (l === 0) {
    const t = line(i);
    return utf8(t) <= NODE ? t : null;
  }
  const t = `${mem.text[l - 1][2 * i]}\n${mem.text[l - 1][2 * i + 1]}`;
  return utf8(t) <= NODE ? t : null;
}

/// Every free node that is ready, built now, until none is: what the
/// compactor would build at once with no call. Answers them, in the order
/// built, for the caller to store.
export function buildFree(mem, line) {
  const made = [];
  for (;;) {
    let any = false;
    for (const { l, i } of ready(mem, { max: Infinity })) {
      const t = freeText(mem, l, i, line);
      if (t === null) continue;
      setNode(mem, l, i, t);
      made.push({ l, i, text: t });
      any = true;
    }
    if (!any) return made;
  }
}

/// Spec 5.1: the view as a call sees it, `<chat>` and one `id+n|text` line
/// per part, newlines flattened, covering the parts that start before
/// `upto` (all of them by default) up to the first one not built: no call
/// ever sees a placeholder (spec 6). `settled` says no such part was left
/// out.
export function render(mem, upto = mem.T) {
  const lines = [];
  let settled = true;
  for (const p of mem.view) {
    const s = startOf(p);
    if (s >= upto) break;
    if (!isBuilt(mem, p.l, p.i)) {
      settled = false;
      break;
    }
    lines.push(`${s}+${2 ** p.l}|${flat(mem.text[p.l][p.i])}`);
  }
  const text = `<chat>\n${lines.map((l) => `${l}\n`).join("")}</chat>`;
  return { text, bytes: utf8(text), parts: lines.length, settled };
}

/// The view as parts, for a page: every part, a placeholder for the unbuilt.
export function parts(mem) {
  return mem.view.map((p) => {
    const b = isBuilt(mem, p.l, p.i);
    return { id: startOf(p), n: 2 ** p.l, text: b ? mem.text[p.l][p.i] : PLACEHOLDER, built: b };
  });
}

/// Whether id+n names a node of this log (spec 7.1's requirement).
export function isLine(mem, id, n) {
  return Number.isSafeInteger(id) && id >= 0 && isPow2(n) && id % n === 0 && id + n <= mem.T;
}

/// Spec 7.1: line id+n opened into its two children's lines, or for n = 1
/// the message whole (`message(id)` → {kind, text}). An unbuilt child shows
/// as the placeholder.
export function zoom(mem, id, n, message) {
  if (!isLine(mem, id, n)) return `No line ${id}+${n}.`;
  if (n === 1) {
    const m = message(id);
    return `${id}+0|${m.kind}: ${m.text}`;
  }
  const l = Math.round(Math.log2(n));
  const h = n / 2;
  const i = id / n;
  return [2 * i, 2 * i + 1].map((c) => `${c * h}+${h}|${isBuilt(mem, l - 1, c) ? flat(mem.text[l - 1][c]) : PLACEHOLDER}`).join("\n");
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

/// Spec 4.2: what one compactor call sees, with no ids anywhere. `context`
/// is the view's lines before the node's end, bare (`<chat>` …), all of
/// them summaries by rule 3; `step` is SCALE and the message whole (level
/// 0, `line(i)`) or the two lines to merge, written out again.
export function compactInput(mem, l, i, line) {
  const context = contextBefore(mem, l === 0 ? i : (i + 1) * 2 ** l);
  const ask =
    l === 0
      ? `Compress this message into one line, in at most ${NODE} bytes:\n${line(i)}`
      : `Merge these two lines into one, in at most ${NODE} bytes:\n${flat(mem.text[l - 1][2 * i])}\n${flat(mem.text[l - 1][2 * i + 1])}`;
  return { context, step: `For scale, this line is exactly ${NODE} bytes:\n${SCALE}\n\n${ask}` };
}

/// A compactor call's context (spec 4.2): the view's lines that start
/// before `end`, bare, in `<chat>`; every one a summary (rule 3).
function contextBefore(mem, end) {
  const lines = [];
  for (const p of mem.view) {
    if (startOf(p) >= end) break;
    assert(isBuilt(mem, p.l, p.i), `the compactor sees only summaries (rule 3): ${startOf(p)}+${2 ** p.l} is not built`);
    lines.push(flat(mem.text[p.l][p.i]));
  }
  return `<chat>\n${lines.map((t) => `${t}\n`).join("")}</chat>`;
}

/// The level-0 nodes one batched call builds (docs/optchat.md, "Importing
/// chats"): `first`'s, the one rule 3 makes ready, then each later message
/// not built whose line is over NODE, at most `max`, their lines at most
/// `maxBytes` in all, within `window` messages of `first` and before
/// `upto`. A short message between them is a free node, built when `first`
/// reaches it; one another run holds (`busy`) ends the batch.
export function batch(mem, line, { busy = new Set(), upto = mem.T, max = BATCH, maxBytes = BATCH_MAX_BYTES, window = BATCH_WINDOW } = {}) {
  const f = first(mem);
  const out = [];
  let bytes = 0;
  const end = Math.min(upto, mem.T, f + window);
  for (let j = f; j < end && out.length < max; j++) {
    if (isBuilt(mem, 0, j)) continue;
    if (busy.has(key(0, j))) break;
    const b = utf8(line(j));
    if (b <= NODE) continue;
    if (out.length && bytes + b > maxBytes) break;
    bytes += b;
    out.push({ l: 0, i: j });
  }
  return out;
}

const BATCH_ANSWER = (n, what) =>
  `Answer with exactly ${n} lines, one per ${what} in order, each starting with its number and ") ", and nothing else.`;

/// What one batched level-0 call sees: the view before the batch's first
/// message as its context (spec 4.2), and as its step SCALE and the batch's
/// messages in order, each whole and numbered; a message between them that
/// is a line already (a short one, or one built) shows as that line,
/// unnumbered.
export function compactBatchInput(mem, nodes, line) {
  assert(nodes.length >= 2 && nodes.every((n, k) => n.l === 0 && (k === 0 || n.i > nodes[k - 1].i)), "a level-0 batch is two or more messages in order");
  const number = new Map(nodes.map((n, k) => [n.i, k + 1]));
  const blocks = [];
  for (let j = nodes[0].i; j <= nodes[nodes.length - 1].i; j++) {
    const k = number.get(j);
    blocks.push(k === undefined ? `--- a line already ---\n${flat(built(mem, 0, j) ?? line(j))}` : `--- ${k} ---\n${line(j)}`);
  }
  const n = nodes.length;
  const ask =
    `Compress each of these ${n} messages into one line, in at most ${NODE} bytes each. They come next in the chat, in order; ` +
    `a short message between them is a line already, shown for context.\n\n${blocks.join("\n")}\n---\n\n${BATCH_ANSWER(n, "message")}`;
  return { context: contextBefore(mem, nodes[0].i), step: `For scale, this line is exactly ${NODE} bytes:\n${SCALE}\n\n${ask}` };
}

/// What one batched merge call sees: the view's lines before the earliest
/// end among the merges as its context (none sees past its own stretch),
/// and as its step SCALE and each merge's two lines, numbered.
export function compactMergeBatchInput(mem, nodes) {
  assert(nodes.length >= 2 && nodes.every((n) => n.l > 0 && isBuilt(mem, n.l - 1, 2 * n.i) && isBuilt(mem, n.l - 1, 2 * n.i + 1)), "a merge batch is two or more merges of built lines");
  const end = Math.min(...nodes.map((n) => (n.i + 1) * 2 ** n.l));
  const blocks = nodes.map((n, k) => `--- ${k + 1} ---\n${flat(mem.text[n.l - 1][2 * n.i])}\n${flat(mem.text[n.l - 1][2 * n.i + 1])}`);
  const n = nodes.length;
  const ask =
    `Merge each of these ${n} pairs of adjacent lines into one line, in at most ${NODE} bytes each. ` +
    `Each pair covers a stretch of the chat; the pairs are in the chat's order.\n\n${blocks.join("\n")}\n---\n\n${BATCH_ANSWER(n, "pair")}`;
  return { context: contextBefore(mem, end), step: `For scale, this line is exactly ${NODE} bytes:\n${SCALE}\n\n${ask}` };
}

/// A batched reply's lines by their number (1 to k): a line starting `n) `
/// (or `[n]`, `n.`, `n:`) starts line n, the first such wins, and a line
/// that starts with no number goes on the one before it.
export function batchLines(reply, k) {
  const out = new Map();
  let cur = null;
  for (const raw of String(reply ?? "").split(/\r?\n/)) {
    const m = /^\s*(?:[-*]\s+)?(?:\*\*)?(?:\[(\d{1,3})\]|(\d{1,3})[).:])(?:\*\*)?\s*(.*)$/.exec(raw);
    if (m) {
      const n = Number(m[1] ?? m[2]);
      cur = n >= 1 && n <= k && !out.has(n) ? n : null;
      if (cur !== null) out.set(cur, m[3].trim());
    } else if (cur !== null && raw.trim()) {
      out.set(cur, `${out.get(cur)} ${raw.trim()}`.trim());
    }
  }
  for (const [n, t] of out) if (!t) out.delete(n);
  return out;
}

/// A line's state in a batch is done when its last try fits or it has
/// TRIES of them; else it is asked again.
const lineOpen = (tries) => tries.length === 0 || (utf8(tries[tries.length - 1]) > NODE && tries.length < TRIES);
const shortest = (tries) => tries.reduce((best, t) => (best === null || utf8(t) < utf8(best) ? t : best), null);

export function batchStart(k) {
  assert(Number.isSafeInteger(k) && k >= 2 && k <= BATCH, "a batch is 2 to BATCH lines");
  return { k, tries: Array.from({ length: k }, () => []), calls: 0 };
}

/// Spec 4.3 for a batch, line by line: a reply's lines are tries of the
/// lines still open; when none is open, or after TRIES calls, each line is
/// its shortest try (null: it never got one). Else the retry to send in
/// the same conversation: each line over the limit cut where it falls, and
/// each one missing named.
export function batchTry(state, reply) {
  const got = batchLines(reply, state.k);
  const tries = state.tries.map((t, n) => (lineOpen(t) && got.has(n + 1) ? [...t, got.get(n + 1)] : t));
  const s = { k: state.k, tries, calls: state.calls + 1 };
  const open = tries.flatMap((t, n) => (lineOpen(t) ? [n] : []));
  if (!open.length || s.calls >= TRIES) return { state: s, lines: tries.map(shortest) };
  const say = open.map((n) => {
    const t = tries[n];
    if (!t.length) return `${n + 1}) is missing.`;
    const last = t[t.length - 1];
    return `${n + 1}) is ${utf8(last)} bytes; the limit is ${NODE}. It must end where it is cut here:\n${n + 1}) ${cutBytes(last, NODE)}| ← LIMIT`;
  });
  const nums = open.map((n) => n + 1).join(", ");
  return { state: s, retry: `${say.join("\n")}\n\nWrite line${open.length > 1 ? "s" : ""} ${nums} again, each starting with its number and ") ", and nothing else.` };
}

/// A batch's lines as they stand (its calls cut short): each line's
/// shortest try that fits, else null.
export function batchDone(state) {
  return state.tries.map((t) => shortest(t.filter((x) => utf8(x) <= NODE)));
}

/// The compactor's first request: COMPACT, then one user message of two
/// text blocks, the context first so it caches across calls.
export function compactMessages(system, input) {
  return [
    { role: "system", content: system },
    { role: "user", content: [{ type: "text", text: input.context }, { type: "text", text: input.step }] },
  ];
}

/// Spec 4.3's feedback: the line cut where the limit falls.
export function retryMessage(line) {
  return `That line is ${utf8(line)} bytes; the limit is ${NODE}. It must end where it is cut here:\n${cutBytes(line, NODE)}| ← LIMIT`;
}

/// Spec 4.3, one reply: trimmed, kept as a try; done when it fits or at
/// TRIES (the shortest try wins, the first of equals), else the retry to
/// send in the same conversation. An empty reply fails the node.
export function compactTry(tries, reply) {
  const line = String(reply ?? "").trim();
  if (!line) return { fail: "the compactor answered nothing" };
  const all = [...tries, line];
  if (utf8(line) <= NODE || all.length >= TRIES) {
    let best = all[0];
    for (const t of all) if (utf8(t) < utf8(best)) best = t;
    return { tries: all, text: best };
  }
  return { tries: all, retry: retryMessage(line) };
}
