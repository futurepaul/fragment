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
  const end = l === 0 ? i : (i + 1) * 2 ** l;
  const lines = [];
  for (const p of mem.view) {
    if (startOf(p) >= end) break;
    assert(isBuilt(mem, p.l, p.i), `the compactor sees only summaries (rule 3): ${startOf(p)}+${2 ** p.l} is not built`);
    lines.push(flat(mem.text[p.l][p.i]));
  }
  const context = `<chat>\n${lines.map((t) => `${t}\n`).join("")}</chat>`;
  const ask =
    l === 0
      ? `Compress this message into one line, in at most ${NODE} bytes:\n${line(i)}`
      : `Merge these two lines into one, in at most ${NODE} bytes:\n${flat(mem.text[l - 1][2 * i])}\n${flat(mem.text[l - 1][2 * i + 1])}`;
  return { context, step: `For scale, this line is exactly ${NODE} bytes:\n${SCALE}\n\n${ask}` };
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
