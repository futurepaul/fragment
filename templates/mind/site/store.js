// What the page knows of the mind, and how it learns it (docs/optchat.md,
// "Operations" and "Records on `log`"): queries for what is there when it
// opens, and the `log` channel, followed live, for everything after. The
// main agent's reply streams as `log`'s draft `turn:<thread>`.
//
// A hand-off is goose's turn in the chat-records contract (docs/
// chat-records.md): the page follows `work` (turn.start, steps, prompts,
// turn.end, each naming its `turn`) and `chat`'s drafts (goose's words as
// they stream) itself. A turn is its task's by the task's `turn` when the
// mind records it; else exactly, by turn.start's `cause.seq` and the task's
// message on `chat` (its text ends `(task <id>, thread <thread>)`); else,
// as a last resort, in order. Steps a task record carries stand in while
// no `work` is read.
//
// Screens read `S` and call `changed()`; the page renders once a frame.

/// The fragment: `./__fragment.js`, or `./mock.js` in the page's dev mode.
export let F = null;

/// The page in the shell (`?embed=shell`): the shell's sidebar is its rail
/// and its topbar its header (mind.js).
export const EMBED = new URLSearchParams(location.search).get("embed") === "shell" && window.parent !== window;

/// A message's files: at most this many, each at most this big (a chat's,
/// docs/chat-records.md), uploaded as the fragment's blobs.
export const FILES_MAX = 8;
export const FILE_MAX_BYTES = 25 * 1024 * 1024;
const SHA256 = /^[0-9a-f]{64}$/;
/// Where a blob of the fragment is read (docs/api.md, Blobs).
export const blobUrl = (sha256) => F?.blobUrl?.(sha256) ?? `./__blob/${sha256}`;

/// The log's last records a page reads as it opens: each thread's latest
/// turn state and a hand-off's latest steps are among them.
const LOG_LAST = 300;
/// `work`'s and `chat`'s last records read as the page opens: the recent
/// hand-offs' turns, and the task messages that name them.
const WORK_LAST = 200;
const CHAT_LAST = 200;
/// A thread's messages read at once, and again for "earlier".
export const THREAD_PAGE = 160;
/// The recent threads the rail shows, and a page of "everything".
export const RECENT = 14;
export const EVERYTHING_PAGE = 40;
/// A draft no word came for in this long is let go (its writer went away).
const DRAFT_STALE_MS = 120_000;
/// After a message of one's own, the thread shows it is being heard until
/// its turn says otherwise, or this long passes.
export const PENDING_MS = 20_000;

export const S = {
  me: null, // { id, principal, role }
  person: null, // { name, username, picture } from `__people`, when it answers
  personas: [], // [{ id, name, emoji, instructions, hands }]
  defaultPersona: null,
  chosen: null, // the persona picked for new chats (null: the default)
  threads: new Map(), // id -> { id, title, persona, started, last, summary, topics, count }
  recent: [], // thread ids, newest first
  topics: null, // [{ id, name, description, count }] once read
  status: null, // the `status` query's answer
  statusAt: 0,
  about: null, // the person's about-me
  turns: new Map(), // thread -> { state, error, at }
  drafts: new Map(), // thread -> { text, at }: the main agent's reply as it streams
  said: new Map(), // thread -> the text of its latest `talk`, which a late draft frame repeats
  landed: new Map(), // message i -> the key of what it took the place of (a pending message's; "" for a draft)
  tasks: new Map(), // task id -> { id, thread, text, state, steps, report, started, ended, turn? }
  handDrafts: new Map(), // bridge turn -> { text, at }: goose's words as they stream
  turnTask: new Map(), // bridge turn -> task id, known for sure
  work: new Map(), // bridge turn -> { turn, start, at, steps: [], end, endAt, seen }: goose's turn as `work` has it
  chatTask: new Map(), // `chat` seq -> task id: the message that handed the task over
  msgs: new Map(), // thread -> { byI: Map<i, message>, more, loaded, loading }
  pending: new Map(), // thread -> [{ key, text, at, persona, files, uploading?, failed? }]: said, not yet in the log
  stopping: new Set(), // threads whose Stop was asked
  suggestions: null, // topic names offered (`suggest`)
  suggesting: false,
  titles: new Map(), // thread -> title, for threads only a search named
  error: null, // a problem to show the person
  closed: null, // the fragment ended this page's socket
};

const listeners = new Set();
let frame = 0;
/// Something changed: every listener runs once, on the next frame.
export function changed() {
  if (frame) return;
  frame = requestAnimationFrame(() => {
    frame = 0;
    for (const l of listeners) l();
  });
}
export const onChange = (fn) => listeners.add(fn);

export function problem(text) {
  S.error = text;
  changed();
}

// ---- reading what comes in ----

const str = (v) => (typeof v === "string" ? v : "");
const num = (v) => (Number.isFinite(v) ? v : null);

function message(b) {
  if (!Number.isInteger(b.i)) return null;
  return { i: b.i, kind: str(b.kind) || "user", text: str(b.text), at: num(b.at) ?? Date.now(), thread: str(b.thread) || null, persona: str(b.persona) || null, task: str(b.task) || null, attachments: filesOf(b.attachments) };
}

/// A message's files, as its record or the `thread` query has them: each
/// a blob of the fragment by its hash.
export function filesOf(list) {
  if (!Array.isArray(list)) return [];
  return list
    .filter((a) => a && typeof a === "object" && SHA256.test(a.sha256))
    .slice(0, FILES_MAX)
    .map((a) => ({ sha256: a.sha256, size: Number.isFinite(a.size) ? a.size : null, type: str(a.type), name: str(a.name) }));
}

/// A task's steps as kept: an array, or its JSON (the table's `steps`).
function steps(v) {
  if (Array.isArray(v)) return v.filter((s) => s && typeof s === "object");
  if (typeof v === "string" && v) {
    try {
      const a = JSON.parse(v);
      return Array.isArray(a) ? a.filter((s) => s && typeof s === "object") : [];
    } catch {
      return [];
    }
  }
  return null;
}

export function putTask(b) {
  if (typeof b.id !== "string") return;
  const old = S.tasks.get(b.id) ?? { id: b.id, steps: [] };
  const next = { ...old };
  for (const k of ["thread", "text", "state", "report", "turn"]) if (typeof b[k] === "string") next[k] = b[k];
  for (const k of ["started", "ended", "i", "seq"]) if (Number.isFinite(b[k])) next[k] = b[k];
  const st = steps(b.steps);
  if (st) next.steps = st;
  if (typeof b.turn === "string") S.turnTask.set(b.turn, b.id);
  S.tasks.set(b.id, next);
}

export function putThread(t) {
  if (!t || typeof t.id !== "string") return null;
  const old = S.threads.get(t.id) ?? { id: t.id, topics: [], count: 0 };
  const next = { ...old };
  for (const k of ["title", "persona", "summary"]) if (typeof t[k] === "string") next[k] = t[k];
  for (const k of ["started", "last", "count"]) if (Number.isFinite(t[k])) next[k] = t[k];
  if (Array.isArray(t.topics)) next.topics = t.topics.filter((x) => x && typeof x.id === "string");
  S.threads.set(t.id, next);
  return next;
}

function bump(id) {
  S.recent = [id, ...S.recent.filter((x) => x !== id)];
}

let threadsAgain = 0;
/// The recent list read again soon (a new thread, a summary rebuilt).
export function threadsSoon() {
  clearTimeout(threadsAgain);
  threadsAgain = setTimeout(loadRecent, 600);
}

function onLog(record) {
  const b = record.body;
  if (!b || typeof b !== "object") return;
  switch (b.type) {
    case "msg": {
      const m = message(b);
      if (!m) return;
      if (m.thread) {
        const box = S.msgs.get(m.thread);
        if (box) box.byI.set(m.i, m);
        if (m.kind === "user") {
          // the log caps a long message: the same words, as far as they go
          const head = (s) => s.trim().slice(0, 1000);
          const list = S.pending.get(m.thread);
          const at = list?.findIndex((p) => head(p.text) === head(m.text));
          if (at >= 0) {
            S.landed.set(m.i, `p:${list[at].key}`);
            for (const f of list[at].files ?? []) if (f.preview) URL.revokeObjectURL(f.preview);
            list.splice(at, 1);
          }
        }
        if (m.kind === "talk") {
          if (S.drafts.delete(m.thread)) S.landed.set(m.i, "");
          S.said.set(m.thread, m.text);
        }
        const t = S.threads.get(m.thread);
        if (t) {
          if (!(t.last >= m.at)) {
            t.last = m.at;
            bump(t.id);
          }
        } else threadsSoon();
      }
      break;
    }
    case "turn": {
      if (typeof b.thread !== "string") return;
      S.turns.set(b.thread, { state: str(b.state), error: str(b.error), at: num(record.at) ?? Date.now(), seen: Date.now() });
      if (b.state === "settling") S.said.delete(b.thread);
      if (b.state !== "thinking" && b.state !== "settling") {
        S.drafts.delete(b.thread);
        S.stopping.delete(b.thread);
      }
      break;
    }
    case "task":
      putTask(b);
      break;
    case "topics": {
      const t = S.threads.get(b.thread);
      if (t && Array.isArray(b.topics)) t.topics = b.topics.filter((x) => x && typeof x.id === "string");
      break;
    }
    case "thread": {
      const fresh = !S.threads.has(b.id);
      const t = putThread({ id: b.id, title: b.title });
      if (t && fresh) {
        t.last ??= record.at ?? Date.now();
        t.started ??= t.last;
        bump(t.id);
        threadsSoon();
      }
      break;
    }
    case "suggest":
      if (Array.isArray(b.names)) {
        S.suggestions = b.names.filter((n) => typeof n === "string" && n.trim());
        S.suggesting = false;
      }
      break;
    default:
      return;
  }
  changed();
}

// A draft has no end of its own (no null frame): its `talk` record or its
// turn's end replaces it. A frame that comes after either (drafts are sent
// at most 4 a second, records at once) is the reply already shown: dropped.
function onLogDraft(d) {
  if (typeof d.turn !== "string" || !d.turn.startsWith("turn:")) return;
  const thread = d.turn.slice(5);
  if (typeof d.text !== "string") S.drafts.delete(thread);
  else {
    const turn = S.turns.get(thread);
    if (turn && turn.state !== "thinking" && turn.state !== "settling") return;
    if (S.said.get(thread)?.startsWith(d.text)) return;
    S.drafts.set(thread, { text: d.text, at: Date.now() });
  }
  changed();
}

function workOf(turn) {
  let w = S.work.get(turn);
  if (!w) {
    w = { turn, start: null, at: null, steps: [], end: null, endAt: null, seen: Date.now() };
    S.work.set(turn, w);
  }
  return w;
}

// A goose draft names its turn; the turn's first word is when it is seen.
function onChatDraft(d) {
  if (typeof d.turn !== "string") return;
  if (typeof d.text !== "string") S.handDrafts.delete(d.turn);
  else {
    workOf(d.turn);
    S.handDrafts.set(d.turn, { text: d.text, at: Date.now() });
  }
  changed();
}

// The mind's message handing a task over: which `chat` seq is which task.
function onChat(record) {
  const b = record.body;
  const text = typeof b === "string" ? b : b && typeof b.text === "string" ? b.text : "";
  const m = text.match(/\(task ([A-Za-z0-9_.:-]{1,64}), thread [A-Za-z0-9_-]{1,64}\)\s*$/);
  if (m && Number.isInteger(record.seq)) {
    S.chatTask.set(record.seq, m[1]);
    changed();
  }
}

// goose's turn as `work` has it (docs/chat-records.md, "work").
function onWork(record) {
  const b = record.body;
  if (!b || typeof b !== "object" || typeof b.turn !== "string") return;
  const w = workOf(b.turn);
  const at = num(record.at) ?? Date.now();
  if (b.kind === "turn.start") {
    w.start = b;
    w.at = at;
  } else if (b.kind === "turn.step" && Number.isInteger(b.step)) {
    const k = w.steps.findIndex((s) => s.kind === "turn.step" && s.step === b.step);
    if (k >= 0) w.steps[k] = b;
    else w.steps.push(b);
  } else if (b.kind === "turn.prompt" && typeof b.prompt === "string") {
    if (!w.steps.some((s) => s.kind === "turn.prompt" && s.prompt === b.prompt)) w.steps.push({ ...b });
  } else if (b.kind === "turn.prompt.closed" && typeof b.prompt === "string") {
    const p = w.steps.find((s) => s.kind === "turn.prompt" && s.prompt === b.prompt);
    if (p) Object.assign(p, { outcome: b.outcome, option: b.option, by: b.by });
  } else if (b.kind === "turn.end") {
    w.end = b;
    w.endAt = at;
    S.handDrafts.delete(b.turn);
  } else return;
  changed();
}

export const running = (task) => !!task && !/^(done|idle|ended|answered|error|failed|lost|stopped)$/.test(task.state ?? "");

/// The task a bridge turn is: as the mind records it (`turn`), else as its
/// turn.start's cause names the task's message on `chat`, else (neither
/// read) the oldest running hand-off no turn holds, the turns taken in the
/// order they were first seen: goose takes hand-offs in the order they
/// were opened.
export function taskOfTurn(turn) {
  const known = S.turnTask.get(turn);
  if (known) return S.tasks.get(known) ?? null;
  const seq = S.work.get(turn)?.start?.cause?.seq;
  const sure = Number.isInteger(seq) ? S.chatTask.get(seq) : null;
  if (sure) {
    S.turnTask.set(turn, sure);
    return S.tasks.get(sure) ?? null;
  }
  const held = new Set(S.turnTask.values());
  for (const t of S.tasks.values()) if (t.turn) held.add(t.id);
  const open = [...S.tasks.values()].filter((t) => running(t) && !held.has(t.id)).sort((a, b) => (a.started ?? a.i ?? 0) - (b.started ?? b.i ?? 0));
  const exact = (w) => Number.isInteger(w.start?.cause?.seq) && S.chatTask.has(w.start.cause.seq);
  const unknown = [...S.work.values()].filter((w) => !S.turnTask.has(w.turn) && !exact(w) && !w.end).sort((a, b) => a.seen - b.seen);
  const k = unknown.findIndex((w) => w.turn === turn);
  return k >= 0 ? (open[k] ?? null) : null;
}

/// The bridge turn running `task`, if the page knows it.
export function turnOfTask(task) {
  if (task.turn) return task.turn;
  for (const turn of S.work.keys()) if (taskOfTurn(turn)?.id === task.id) return turn;
  return null;
}

/// A hand-off as the page shows it: its steps (from `work`, else as the
/// task record carries them), its state (the task's, or its turn's end
/// before the mind has said), and when it started and ended.
export function handOff(task) {
  const turn = turnOfTask(task);
  const w = turn ? S.work.get(turn) : null;
  const steps = w && w.steps.length ? w.steps : (task.steps ?? []);
  let state = task.state ?? "running";
  if (running(task) && w?.end) state = w.end.outcome === "idle" ? "done" : w.end.outcome === "stopped" ? "stopped" : "error";
  return { turn, steps, state, started: task.started ?? w?.at ?? null, ended: task.ended ?? w?.endAt ?? null };
}

/// Whether a hand-off is still going (as far as the page knows).
export const handRunning = (task) => running({ state: handOff(task).state });

/// goose's words so far on `task`, if it is streaming them.
export function handDraftOf(task) {
  const turn = turnOfTask(task);
  const d = turn ? S.handDrafts.get(turn) : null;
  return d && Date.now() - d.at < DRAFT_STALE_MS ? d.text : null;
}

/// A turn's lock lapses this long after its last touch (docs/optchat.md).
const TURN_LAPSE_MS = 15 * 60_000;

/// Whether the mind is answering in `thread` now: its latest turn record
/// says so, and the mind's status does not say otherwise.
export function busy(thread) {
  const t = S.turns.get(thread);
  if (!t || (t.state !== "thinking" && t.state !== "settling")) return false;
  const st = S.status;
  if (st?.turn && st.turn.running !== false && st.turn.thread === thread) return true;
  // a status read after the record came that names no turn here: it ended
  // unsaid (a crash); so did one older than the turn lock lasts
  if (st && S.statusAt > t.seen + 1500) return false;
  return Date.now() - t.at < TURN_LAPSE_MS;
}

export function draftOf(thread) {
  const d = S.drafts.get(thread);
  if (!d) return null;
  if (Date.now() - d.at > DRAFT_STALE_MS) {
    S.drafts.delete(thread);
    return null;
  }
  return d.text;
}

export const persona = (id) => S.personas.find((p) => p.id === id) ?? null;
/// The persona new chats start with: the one picked, else the default.
export const currentPersona = () => persona(S.chosen) ?? persona(S.defaultPersona) ?? S.personas[0] ?? { id: null, name: "Mind", emoji: "🌿", instructions: "", hands: false };
/// A thread's persona: the one picked for its next message, else its own.
export const threadPersona = (thread) => persona(S.threads.get(thread)?.persona) ?? currentPersona();

// ---- loading ----

export async function loadRecent() {
  try {
    const r = await F.call("threads", { limit: RECENT });
    for (const t of r.threads ?? []) putThread(t);
    const ids = (r.threads ?? []).map((t) => t.id);
    // threads made here and not yet listed stay on top
    const mine = S.recent.filter((id) => !ids.includes(id) && (S.threads.get(id)?.last ?? 0) > (S.threads.get(ids[0])?.last ?? 0));
    S.recent = [...mine, ...ids];
  } catch (e) {
    problem(`Could not read your chats: ${e.message}`);
  }
  changed();
}

/// One thread's messages, read once (and its tasks).
export async function loadThread(id) {
  let box = S.msgs.get(id);
  if (box?.loaded || box?.loading) return;
  box ??= { byI: new Map(), more: false, loaded: false, loading: false };
  box.loading = true;
  S.msgs.set(id, box);
  try {
    const [r, t] = await Promise.all([F.call("thread", { id, limit: THREAD_PAGE }), F.call("tasks", { thread: id }).catch(() => null)]);
    if (r.thread) putThread(r.thread);
    for (const m of r.messages ?? []) {
      const msg = message({ ...m, thread: m.thread ?? id });
      if (msg) box.byI.set(msg.i, msg);
    }
    box.more = !!r.more;
    for (const task of t?.tasks ?? []) putTask(task);
  } catch (e) {
    if (e.status !== 404) problem(`Could not open this chat: ${e.message}`);
  }
  box.loading = false;
  box.loaded = true;
  changed();
}

/// The page of a thread before what is loaded.
export async function loadEarlier(id) {
  const box = S.msgs.get(id);
  if (!box || box.loading || !box.more) return;
  const first = Math.min(...box.byI.keys());
  box.loading = true;
  try {
    const r = await F.call("thread", { id, before: first, limit: THREAD_PAGE });
    for (const m of r.messages ?? []) {
      const msg = message({ ...m, thread: m.thread ?? id });
      if (msg) box.byI.set(msg.i, msg);
    }
    box.more = !!r.more;
  } catch (e) {
    problem(`Could not read further back: ${e.message}`);
  }
  box.loading = false;
  changed();
}

/// A thread's title, for one a search names that the page has not seen.
export async function titleOf(id) {
  const t = S.threads.get(id);
  if (t?.title) return t.title;
  if (S.titles.has(id)) return S.titles.get(id);
  S.titles.set(id, null);
  try {
    const r = await F.call("thread", { id, limit: 1 });
    if (r.thread) putThread(r.thread);
    S.titles.set(id, r.thread?.title ?? "");
  } catch {
    S.titles.set(id, "");
  }
  changed();
  return S.titles.get(id);
}

/// Says `text` in `thread` as `personaId`, with `files` (picked, dropped or
/// pasted) uploaded first as the fragment's blobs (`fragment.blob`): shown
/// at once, then as the log has it.
export async function say(thread, text, personaId, files = []) {
  const item = {
    key: crypto.randomUUID(),
    text,
    at: Date.now(),
    persona: personaId,
    files: files.slice(0, FILES_MAX).map((file) => ({ file, name: file.name || "a file", type: file.type, size: file.size, sent: null, preview: /^image\//.test(file.type) ? URL.createObjectURL(file) : null })),
  };
  const list = S.pending.get(thread) ?? [];
  list.push(item);
  S.pending.set(thread, list);
  const t = S.threads.get(thread);
  if (t) {
    t.last = item.at;
    bump(thread);
  }
  await send(thread, item);
}

/// A pending message, its files uploaded (each once: one that is up is
/// not sent again), then posted; its id is its own, so a try again of an
/// unchanged post is the same record (docs/api.md).
async function send(thread, item) {
  item.failed = null;
  item.uploading = item.files.some((f) => !f.sent);
  changed();
  try {
    for (const f of item.files) if (!f.sent) f.sent = await F.blob(f.file, { name: f.name });
    item.uploading = false;
    const body = { text: item.text, thread };
    if (item.persona) body.persona = item.persona;
    if (item.files.length) body.attachments = item.files.map((f) => ({ sha256: f.sent.sha256, name: f.sent.name || f.name, type: f.sent.type || f.type, size: f.sent.size ?? f.size }));
    await F.post("say", body, { id: item.key });
  } catch (e) {
    item.uploading = false;
    item.failed = e.message || "not sent";
  }
  changed();
}

export const resend = (thread, item) => send(thread, item);

export async function stop(thread) {
  S.stopping.add(thread);
  changed();
  try {
    await F.call("stop", { thread });
  } catch (e) {
    S.stopping.delete(thread);
    problem(`Could not stop: ${e.message}`);
  }
}

/// Wires the page to the fragment.
export function start(fragment) {
  F = fragment;
  F.subscribe("log", onLog, { last: LOG_LAST, onDraft: onLogDraft });
  // hand-offs: the task messages and goose's words as they stream (`chat`),
  // and goose's turns (`work`)
  F.subscribe("chat", onChat, { last: CHAT_LAST, onDraft: onChatDraft });
  F.subscribe("work", onWork, { last: WORK_LAST });
  F.live(
    "personas",
    {},
    (r) => {
      S.personas = (r.personas ?? []).filter((p) => p && typeof p.id === "string");
      S.defaultPersona = typeof r.default === "string" ? r.default : S.personas[0]?.id ?? null;
      changed();
    },
    (e) => problem(`Could not read personas: ${e.message}`),
  );
  F.live(
    "status",
    {},
    (r) => {
      S.status = r;
      S.statusAt = Date.now();
      changed();
    },
    () => {},
  );
  F.live(
    "topics",
    {},
    (r) => {
      S.topics = (r.topics ?? []).filter((t) => t && typeof t.id === "string");
      changed();
    },
    () => {},
  );
  F.call("tasks", {})
    .then((r) => {
      for (const t of r.tasks ?? []) putTask(t);
      changed();
    })
    .catch(() => {});
  F.call("settings", {})
    .then((r) => {
      S.about = typeof r.about === "string" ? r.about : "";
      changed();
    })
    .catch(() => {});
  loadRecent();
  F.me().then((hello) => {
    S.me = hello;
    changed();
    people(hello.principal);
  });
  F.closed?.(({ code }) => {
    S.closed = code === 4004 ? "This mind was deleted." : "Your access changed: reload the page.";
    changed();
  });
}

/// Who the person is (`__people`): their name and picture for the rail.
async function people(principal) {
  try {
    const r = F.people ? await F.people([principal]) : await (await fetch(`./__people?id=${encodeURIComponent(principal)}`, { credentials: "same-origin" })).json();
    const p = r?.profiles?.[principal];
    if (p) {
      S.person = { name: p.name || p.username || "", username: str(p.username), picture: typeof p.picture === "string" ? p.picture : "" };
      changed();
    }
  } catch {
    // a rail without a name is fine
  }
}
