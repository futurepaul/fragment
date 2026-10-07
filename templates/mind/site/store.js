// What the page knows of the mind, and how it learns it (docs/optchat.md,
// "Operations" and "Records on `log`"): queries for what is there when it
// opens, and the `log` channel, followed live, for everything after. The
// main agent's reply streams as `log`'s draft `turn:<thread>`; goose's, on
// a hand-off, as `chat`'s draft under the bridge's turn id.
//
// Screens read `S` and call `changed()`; the page renders once a frame.

/// The fragment: `./__fragment.js`, or `./mock.js` in the page's dev mode.
export let F = null;

/// The log's last records a page reads as it opens: each thread's latest
/// turn state and a hand-off's latest steps are among them.
const LOG_LAST = 300;
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
  person: null, // { name, picture } from `__people`, when it answers
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
  tasks: new Map(), // task id -> { id, thread, text, state, steps, report, started, ended, turn? }
  handDrafts: new Map(), // bridge turn -> { text, at }: goose's words as they stream
  turnTask: new Map(), // bridge turn -> task id, learned
  msgs: new Map(), // thread -> { byI: Map<i, message>, more, loaded, loading }
  pending: new Map(), // thread -> [{ key, text, at, persona, failed? }]: said, not yet in the log
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
  return { i: b.i, kind: str(b.kind) || "user", text: str(b.text), at: num(b.at) ?? Date.now(), thread: str(b.thread) || null, persona: str(b.persona) || null, task: str(b.task) || null };
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
          const list = S.pending.get(m.thread);
          const at = list?.findIndex((p) => p.text.trim() === m.text.trim());
          if (at >= 0) list.splice(at, 1);
        }
        if (m.kind === "talk") S.drafts.delete(m.thread);
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
      S.turns.set(b.thread, { state: str(b.state), error: str(b.error), at: record.at ?? Date.now() });
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

function onLogDraft(d) {
  if (typeof d.turn !== "string" || !d.turn.startsWith("turn:")) return;
  const thread = d.turn.slice(5);
  if (typeof d.text !== "string") S.drafts.delete(thread);
  else S.drafts.set(thread, { text: d.text, at: Date.now() });
  changed();
}

function onChatDraft(d) {
  if (typeof d.turn !== "string") return;
  if (typeof d.text !== "string") S.handDrafts.delete(d.turn);
  else S.handDrafts.set(d.turn, { text: d.text, at: Date.now() });
  changed();
}

export const running = (task) => !!task && !/^(done|idle|ended|answered|error|failed|lost|stopped)$/.test(task.state ?? "");

/// The task a bridge turn is: as its record names it, else (when the
/// backend does not say) the one hand-off running while it streams.
export function taskOfTurn(turn) {
  const known = S.turnTask.get(turn);
  if (known) return S.tasks.get(known) ?? null;
  const open = [...S.tasks.values()].filter((t) => running(t) && !t.turn && ![...S.turnTask.values()].includes(t.id));
  if (open.length === 1) {
    S.turnTask.set(turn, open[0].id);
    return open[0];
  }
  return null;
}

/// goose's words so far on `task`, if it is streaming them.
export function handDraftOf(task) {
  for (const [turn, d] of S.handDrafts) {
    if (Date.now() - d.at > DRAFT_STALE_MS) continue;
    if (taskOfTurn(turn)?.id === task.id) return d.text;
  }
  return null;
}

/// Whether the mind is answering in `thread` now: its latest turn record
/// says so, and the mind's status does not say otherwise.
export function busy(thread) {
  const t = S.turns.get(thread);
  if (!t || (t.state !== "thinking" && t.state !== "settling")) return false;
  const st = S.status;
  if (!st || st.turn?.thread === thread) return true;
  // a turn the status (read after it) says is not running ended unsaid
  return !(S.statusAt > t.at + 2000);
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

/// Says `text` in `thread` as `personaId`: shown at once, then as the log
/// has it.
export async function say(thread, text, personaId) {
  const item = { key: crypto.randomUUID(), text, at: Date.now(), persona: personaId };
  const list = S.pending.get(thread) ?? [];
  list.push(item);
  S.pending.set(thread, list);
  const t = S.threads.get(thread);
  if (t) {
    t.last = item.at;
    bump(thread);
  }
  changed();
  const body = { text, thread };
  if (personaId) body.persona = personaId;
  try {
    await F.post("say", body, { id: item.key });
  } catch (e) {
    item.failed = e.message || "not sent";
    changed();
  }
}

export async function resend(thread, item) {
  item.failed = null;
  changed();
  const body = { text: item.text, thread };
  if (item.persona) body.persona = item.persona;
  try {
    await F.post("say", body, { id: item.key });
  } catch (e) {
    item.failed = e.message || "not sent";
    changed();
  }
}

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
  // goose's words on a hand-off stream as `chat`'s drafts; its records
  // reach the page through the mind's own `task` records on `log`
  F.subscribe("chat", () => {}, { last: 1, onDraft: onChatDraft });
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
      S.person = { name: p.name || p.username || "", picture: typeof p.picture === "string" ? p.picture : "" };
      changed();
    }
  } catch {
    // a rail without a name is fine
  }
}
