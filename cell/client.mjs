// fragment's browser library, served by the platform on every fragment as
// ./__fragment.js (it replaces the TypeScript runtime's __rt.js):
//
//   import * as fragment from "./__fragment.js";
//   await fragment.call("add_todo", { text: "milk" });        // an operation
//   await fragment.post("chat", { text: "hi" });               // a record, no app code
//   const file = await fragment.blob(picked);                  // a blob: {sha256, size, type, name}
//   fragment.live("list", {}, (r) => render(r.todos));         // a query, re-run on changes
//   fragment.subscribe("activity", (rec) => log(rec.body));    // a channel, from a cursor
//   fragment.subscribe("chat", show, { last: 100 });           // or from near its end
//   fragment.subscribe("chat", show, { onDraft: (d) => … });   // and its drafts, as written
//   fragment.presence.set({ name: "paul" });                   // who is here
//   fragment.presence.on((list) => showWho(list));             // now, and on each change
//   await fragment.push.register("everyone");                  // web push (after a click)
//   fragment.closed(({ code }) => showGone(code));             // the fragment ended this page's socket
//
// One socket per page carries subscriptions, presence, change signals, and
// the live queries' runs (they go over HTTP only while it is not open); it
// reconnects by itself, after a jittered wait (a deploy drops every page
// at once; they should not all come back in the same second), and resumes
// each channel after its last record. A close the fragment means for good
// (4003: this page's access was revoked; 4004: the fragment was deleted)
// ends it: the page stops reconnecting and `closed` handlers hear why.
// A channel's backlog comes a page at a time: the library asks for the next
// page until the last one, which makes the channel live, so no record is
// skipped however far behind the page starts.
// A live query runs one at a time: changes while it runs ask for one more
// run after it, however many came. Presence comes whole when the socket
// opens, then one change at a time; this page sends its own at most once
// every 150 ms, the latest last (the fragment drops more than 10 a second).
// A call keeps its id across retries, so a retried mutation is a replay,
// never a second write; so does a post.

const base = new URL("./", import.meta.url);

export class FragmentError extends Error {
  constructor(status, body) {
    super((body && body.message) || `http ${status}`);
    this.status = status;
    this.code = body && body.error;
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// One call's POST ({id, input}, as JSON: a cross-site form cannot send it),
// sent again with the same id when the network fails. Returns the result.
async function request(path, id, input) {
  for (let attempt = 0; ; attempt++) {
    let resp;
    try {
      resp = await fetch(new URL(path, base), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ id, input }),
        credentials: "same-origin",
      });
    } catch (e) {
      if (attempt < 3) {
        await sleep(300 * (attempt + 1));
        continue;
      }
      throw e;
    }
    const body = await resp.json().catch(() => null);
    if (!resp.ok) throw new FragmentError(resp.status, body);
    return body.result;
  }
}

export function call(op, input = {}, { id = crypto.randomUUID() } = {}) {
  return request(`__op/${op}`, id, input);
}

/// Appends `body` (at most 64 KiB of JSON) to a channel fragment.json
/// declares with a `post` role, as this page's principal: the platform
/// appends it, no app code runs. Returns the record; the same id again
/// returns that record and appends nothing (another body under it throws,
/// 409). A post spends a public call when the page holds only the public
/// role, as a call does.
export function post(channel, body, { id = crypto.randomUUID() } = {}) {
  return request(`__op/channels/${encodeURIComponent(channel)}`, id, body);
}

const hex = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");

/// Uploads `file` (a File or a Blob, at most 256 MiB) as one of this
/// fragment's blobs, as this page's person, who must be an editor (a
/// chat's attachment). Its bytes are hashed here, so the same file again
/// stores nothing new. Returns what a record names it by, `{sha256, size,
/// type, name}`; a page reads it at `./__blob/<sha256>`, as the type given
/// here when that is passive media (an image, a video, audio, a PDF).
export async function blob(file, { name = file.name ?? "", type = file.type || "application/octet-stream" } = {}) {
  const bytes = await file.arrayBuffer();
  const sha256 = hex(await crypto.subtle.digest("SHA-256", bytes));
  // the same bytes under the same hash again: a retry stores nothing twice
  for (let attempt = 0; ; attempt++) {
    let resp;
    try {
      resp = await fetch(new URL(`__blob/${sha256}`, base), { method: "PUT", headers: { "content-type": type }, body: bytes, credentials: "same-origin" });
    } catch (e) {
      if (attempt < 3) {
        await sleep(300 * (attempt + 1));
        continue;
      }
      throw e;
    }
    const answer = await resp.json().catch(() => null);
    if (!resp.ok) throw new FragmentError(resp.status, answer);
    return { sha256, size: bytes.byteLength, type, name };
  }
}

let socket = null;
let backoff = 1000;
let rerun = null;
let hello = null;
let presenceData = null;
let presenceSentAt = 0;
let presenceTimer = null;
let ended = null; // { code, reason } once the fragment closed the socket for good
let liveIds = 0;
const helloWaiters = [];
const closedHandlers = new Set();
// Close codes after which reconnecting cannot help (live.rs, fragment.rs).
const FINAL_CLOSE_CODES = [4003, 4004];
// Under the fragment's 10 presence changes a second (limits::PRESENCE_PER_S).
const PRESENCE_EVERY_MS = 150;
const subs = new Map(); // channel -> { after, last, handlers }
const lives = new Set(); // { id, op, input, onResult, onError, running, again }
const asked = new Map(); // live id -> the live query whose run the socket has not answered
const present = new Map(); // socket id -> { id, principal, data }
const presenceHandlers = new Set();

function open() {
  return socket !== null && socket.readyState === WebSocket.OPEN;
}

function send(message) {
  if (open()) socket.send(JSON.stringify(message));
}

// Resumes after the cursor; a subscription that asked for the last N
// records starts there until its first page says where that is.
function subscribeFrame(channel, s) {
  return s.last != null ? { type: "subscribe", channel, last: s.last } : { type: "subscribe", channel, after: s.after };
}

// A page awaiting `me()` wants the socket too: its hello is the answer,
// and a socket closed before it (a deploy restarting the runtime) must be
// dialled again, or the page waits forever.
function wanted() {
  return subs.size > 0 || lives.size > 0 || presenceHandlers.size > 0 || presenceData !== null || helloWaiters.length > 0;
}

function connect() {
  if (socket || ended) return;
  const url = new URL("__live", base);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  // the frames this library reads: presence one change at a time
  url.searchParams.set("v", "2");
  const ws = new WebSocket(url);
  socket = ws;
  ws.onopen = () => {
    backoff = 1000;
    for (const [channel, s] of subs) send(subscribeFrame(channel, s));
    if (presenceData !== null) sendPresence();
    for (const l of lives) run(l);
  };
  ws.onmessage = (event) => {
    const m = JSON.parse(event.data);
    if (m.type === "hello") {
      hello = { id: m.id, principal: m.principal, role: m.role };
      present.clear();
      for (const p of m.presence) present.set(p.id, p);
      helloWaiters.splice(0).forEach((resolve) => resolve(hello));
      showPresence();
    } else if (m.type === "result") {
      const l = asked.get(m.id);
      asked.delete(m.id);
      if (!l) return;
      // past its principal's budget for socket queries: this run goes over HTTP
      if (m.error === "rate_limited") viaHttp(l);
      else settle(l, m.error ? { error: new FragmentError(m.status, m) } : { result: m.result });
    } else if (m.type === "record") {
      const s = subs.get(m.channel);
      if (s && m.seq > s.after) {
        s.after = m.seq;
        for (const h of s.handlers) h(m);
      }
    } else if (m.type === "subscribed") {
      const s = subs.get(m.channel);
      if (s) {
        if (m.next > s.after) s.after = m.next;
        s.last = null;
        // not live yet: the next page, from the cursor
        if (m.more) send(subscribeFrame(m.channel, s));
      }
    } else if (m.type === "draft") {
      const s = subs.get(m.channel);
      if (s) for (const h of s.drafts) h(m);
    } else if (m.type === "presence") {
      if (m.data === null) present.delete(m.id);
      else present.set(m.id, { id: m.id, principal: m.principal, data: m.data });
      showPresence();
    } else if (m.type === "changed") {
      clearTimeout(rerun);
      rerun = setTimeout(() => lives.forEach(run), 50);
    } else if (m.type === "error") {
      console.warn("fragment:", m.message);
    }
  };
  ws.onclose = (event) => {
    socket = null;
    // runs the socket will not answer now go over HTTP
    const unanswered = [...asked.values()];
    asked.clear();
    unanswered.forEach(viaHttp);
    if (FINAL_CLOSE_CODES.includes(event.code)) {
      ended = { code: event.code, reason: event.reason };
      closedHandlers.forEach((h) => h(ended));
      return;
    }
    // between half and one and a half times the backoff
    if (wanted()) setTimeout(connect, backoff * (0.5 + Math.random()));
    backoff = Math.min(backoff * 2, 30000);
  };
}

// One run of a live query: over the socket when it is open, else over
// HTTP; one at a time, and one asked for while it runs follows it.
function run(l) {
  if (!lives.has(l)) return;
  if (l.running) {
    l.again = true;
    return;
  }
  l.running = true;
  l.again = false;
  if (open()) {
    asked.set(l.id, l);
    send({ type: "query", id: l.id, op: l.op, input: l.input });
  } else {
    viaHttp(l);
  }
}

async function viaHttp(l) {
  let outcome;
  try {
    outcome = { result: await call(l.op, l.input) };
  } catch (e) {
    outcome = { error: e };
  }
  settle(l, outcome);
}

// A run's outcome to its handlers (a throw from onResult goes to onError),
// then the run asked for meanwhile.
function settle(l, { result, error }) {
  l.running = false;
  if (lives.has(l)) {
    try {
      if (error !== undefined) throw error;
      l.onResult(result);
    } catch (e) {
      if (l.onError) l.onError(e);
      else console.error(e);
    }
  }
  if (l.again) run(l);
}

/// Runs a query now and again after every change; returns a stop function.
export function live(op, input, onResult, onError) {
  const l = { id: `live-${++liveIds}`, op, input, onResult, onError, running: false, again: false };
  lives.add(l);
  connect();
  run(l);
  return () => lives.delete(l);
}

/// Follows a channel from after `after` (0: from the start), or from its
/// last `last` records (at most 1000); returns a stop function. `onDraft`
/// hears the channel's drafts while it is live: `{principal, turn, text,
/// at}`, each the whole text so far (`text: null` once its writer stopped);
/// the record its writer then posts with the same `turn` replaces it.
export function subscribe(channel, onRecord, { after = 0, last = null, onDraft = null } = {}) {
  let s = subs.get(channel);
  if (!s) {
    s = { after, last, handlers: new Set(), drafts: new Set() };
    subs.set(channel, s);
    send(subscribeFrame(channel, s));
  }
  s.handlers.add(onRecord);
  if (onDraft) s.drafts.add(onDraft);
  connect();
  return () => {
    s.handlers.delete(onRecord);
    if (onDraft) s.drafts.delete(onDraft);
    if (s.handlers.size === 0) {
      subs.delete(channel);
      send({ type: "unsubscribe", channel });
    }
  };
}

function presenceList() {
  return [...present.values()];
}

function showPresence() {
  const list = presenceList();
  for (const h of presenceHandlers) h(list);
}

function sendPresence() {
  clearTimeout(presenceTimer);
  presenceTimer = null;
  presenceSentAt = Date.now();
  send({ type: "presence", data: presenceData });
}

export const presence = {
  /// Shares `data` (at most 4 KiB) with everyone on the page; null leaves.
  /// Changes closer together than 150 ms go as one, the latest.
  set(data) {
    presenceData = data;
    connect();
    const wait = presenceSentAt + PRESENCE_EVERY_MS - Date.now();
    if (wait <= 0) sendPresence();
    else if (presenceTimer === null) presenceTimer = setTimeout(sendPresence, wait);
  },
  /// Calls `handler(list)` now (once the socket said hello) and whenever
  /// who is here changes; returns a stop function.
  on(handler) {
    presenceHandlers.add(handler);
    connect();
    if (hello) handler(presenceList());
    return () => presenceHandlers.delete(handler);
  },
};

/// Calls `handler({ code, reason })` if the fragment ends this page's socket
/// for good (4003: its access was revoked; 4004: the fragment was deleted);
/// the page no longer reconnects. Returns a stop function.
export function closed(handler) {
  closedHandlers.add(handler);
  if (ended) handler(ended);
  return () => closedHandlers.delete(handler);
}

/// Who this page is to the fragment: { id, principal, role }.
export function me() {
  connect();
  return hello ? Promise.resolve(hello) : new Promise((resolve) => helloWaiters.push(resolve));
}

// ---- web push: the fragment's service worker (./__sw.js) and VAPID key
// (./__push-key); a subscription is stored tagged `who`, and the app
// pushes to a tag (call.push / job.push). Every failure resolves to
// { ok: false, reason }.
const b64u = {
  decode(s) {
    let b = String(s || "").replaceAll("-", "+").replaceAll("_", "/");
    while (b.length % 4) b += "=";
    return Uint8Array.from(atob(b), (c) => c.charCodeAt(0));
  },
  encode(buf) {
    return btoa(String.fromCharCode(...new Uint8Array(buf))).replaceAll("+", "-").replaceAll("/", "_").replaceAll("=", "");
  },
};

async function pushPost(path, body) {
  const resp = await fetch(new URL(path, base), { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body), credentials: "same-origin" });
  const j = await resp.json().catch(() => ({}));
  return resp.ok ? j : { ok: false, reason: "server", error: j.message || `http ${resp.status}`, status: resp.status };
}

export const push = {
  async register(who = "") {
    const sw = navigator.serviceWorker;
    if (!sw || typeof PushManager === "undefined") return { ok: false, reason: "unsupported" };
    if (typeof Notification !== "undefined" && Notification.permission === "denied") return { ok: false, reason: "denied" };
    try {
      await sw.register(new URL("__sw.js", base));
      const reg = await sw.ready;
      const { key } = await (await fetch(new URL("__push-key", base), { credentials: "same-origin" })).json();
      if (!key) return { ok: false, reason: "no-key" };
      const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64u.decode(key) });
      const answer = await pushPost("__push-sub", { who, endpoint: sub.endpoint, p256dh: b64u.encode(sub.getKey("p256dh")), auth: b64u.encode(sub.getKey("auth")) });
      if (answer.ok === false) await sub.unsubscribe().catch(() => {});
      return answer;
    } catch (e) {
      const denied = e && (e.name === "NotAllowedError" || e.name === "PermissionDeniedError");
      return denied ? { ok: false, reason: "denied" } : { ok: false, reason: "error", error: String((e && e.message) || e) };
    }
  },
  async unregister() {
    const sw = navigator.serviceWorker;
    if (!sw) return { ok: false, reason: "unsupported" };
    try {
      const sub = await (await sw.ready).pushManager.getSubscription();
      if (!sub) return { ok: true, removed: 0 };
      const answer = await pushPost("__push-unsub", { endpoint: sub.endpoint });
      await sub.unsubscribe().catch(() => {});
      return answer;
    } catch (e) {
      return { ok: false, reason: "error", error: String((e && e.message) || e) };
    }
  },
};
