// fragment's browser library, served by the platform on every fragment as
// ./__fragment.js (it replaces the TypeScript runtime's __rt.js):
//
//   import * as fragment from "./__fragment.js";
//   await fragment.call("add_todo", { text: "milk" });        // an operation
//   fragment.live("list", {}, (r) => render(r.todos));         // a query, re-run on changes
//   fragment.subscribe("activity", (rec) => log(rec.body));    // a channel, from a cursor
//   fragment.presence.set({ name: "paul" });                   // who is here
//   fragment.presence.on((list) => showWho(list));
//   await fragment.push.register("everyone");                  // web push (after a click)
//
// One socket per page carries subscriptions, presence, and change signals;
// it reconnects by itself and resumes each channel after its last record.
// A call keeps its id across retries, so a retried mutation is a replay,
// never a second write.

const base = new URL("./", import.meta.url);

export class FragmentError extends Error {
  constructor(status, body) {
    super((body && body.message) || `http ${status}`);
    this.status = status;
    this.code = body && body.error;
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function call(op, input = {}, { id = crypto.randomUUID() } = {}) {
  for (let attempt = 0; ; attempt++) {
    let resp;
    try {
      resp = await fetch(new URL(`__op/${op}`, base), {
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

let socket = null;
let backoff = 1000;
let rerun = null;
let hello = null;
let presenceData = null;
const helloWaiters = [];
const subs = new Map(); // channel -> { after, handlers }
const lives = new Set(); // { op, input, onResult, onError }
const presenceHandlers = new Set();

function send(message) {
  if (socket && socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(message));
}

function wanted() {
  return subs.size > 0 || lives.size > 0 || presenceHandlers.size > 0 || presenceData !== null;
}

function connect() {
  if (socket) return;
  const url = new URL("__live", base);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(url);
  socket = ws;
  ws.onopen = () => {
    backoff = 1000;
    for (const [channel, s] of subs) send({ type: "subscribe", channel, after: s.after });
    if (presenceData !== null) send({ type: "presence", data: presenceData });
    for (const l of lives) run(l);
  };
  ws.onmessage = (event) => {
    const m = JSON.parse(event.data);
    if (m.type === "hello") {
      hello = { id: m.id, principal: m.principal, role: m.role };
      helloWaiters.splice(0).forEach((resolve) => resolve(hello));
    } else if (m.type === "record") {
      const s = subs.get(m.channel);
      if (s && m.seq > s.after) {
        s.after = m.seq;
        for (const h of s.handlers) h(m);
      }
    } else if (m.type === "presence") {
      for (const h of presenceHandlers) h(m.list);
    } else if (m.type === "changed") {
      clearTimeout(rerun);
      rerun = setTimeout(() => lives.forEach(run), 50);
    } else if (m.type === "error") {
      console.warn("fragment:", m.message);
    }
  };
  ws.onclose = () => {
    socket = null;
    if (wanted()) setTimeout(connect, backoff);
    backoff = Math.min(backoff * 2, 30000);
  };
}

async function run(l) {
  try {
    l.onResult(await call(l.op, l.input));
  } catch (e) {
    if (l.onError) l.onError(e);
    else console.error(e);
  }
}

/// Runs a query now and again after every change; returns a stop function.
export function live(op, input, onResult, onError) {
  const l = { op, input, onResult, onError };
  lives.add(l);
  connect();
  run(l);
  return () => lives.delete(l);
}

/// Follows a channel from after `after` (0: from the start); returns a stop function.
export function subscribe(channel, onRecord, { after = 0 } = {}) {
  let s = subs.get(channel);
  if (!s) {
    s = { after, handlers: new Set() };
    subs.set(channel, s);
    send({ type: "subscribe", channel, after });
  }
  s.handlers.add(onRecord);
  connect();
  return () => {
    s.handlers.delete(onRecord);
    if (s.handlers.size === 0) {
      subs.delete(channel);
      send({ type: "unsubscribe", channel });
    }
  };
}

export const presence = {
  /// Shares `data` (at most 4 KiB) with everyone on the page; null leaves.
  set(data) {
    presenceData = data;
    connect();
    send({ type: "presence", data });
  },
  /// Calls `handler(list)` whenever who is here changes; returns a stop function.
  on(handler) {
    presenceHandlers.add(handler);
    connect();
    return () => presenceHandlers.delete(handler);
  },
};

/// Who this page is to the fragment: { id, principal, role }.
export function me() {
  connect();
  return hello ? Promise.resolve(hello) : new Promise((resolve) => helloWaiters.push(resolve));
}

// ---- notifications: thin, defensive wrappers. ask() only prompts from a
// click (browsers ignore it otherwise); show() fires only while the page
// is hidden. Nothing here throws or rejects.
function notifySupported() {
  try {
    return typeof Notification !== "undefined";
  } catch (e) {
    return false;
  }
}

export const notify = {
  supported: notifySupported,
  permission() {
    if (!notifySupported()) return "unsupported";
    try {
      return Notification.permission;
    } catch (e) {
      return "unsupported";
    }
  },
  ask() {
    return new Promise((resolve) => {
      if (!notifySupported()) return resolve("unsupported");
      let done = false;
      const settle = (p) => {
        if (!done) {
          done = true;
          clearTimeout(t);
          resolve(p);
        }
      };
      const t = setTimeout(() => settle("default"), 60000);
      try {
        const r = Notification.requestPermission((p) => settle(p));
        if (r && typeof r.then === "function") r.then(settle, () => settle("denied"));
      } catch (e) {
        settle("denied");
      }
    });
  },
  show(title, opts = {}) {
    try {
      if (!notifySupported() || Notification.permission !== "granted" || !document.hidden) return false;
      const n = new Notification(String(title ?? ""), { body: opts.body == null ? undefined : String(opts.body), tag: opts.tag == null ? undefined : String(opts.tag) });
      n.onclick = () => {
        window.focus();
        n.close();
        if (opts.url) location.assign(String(opts.url));
      };
      return true;
    } catch (e) {
      return false;
    }
  },
};

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

async function post(path, body) {
  const resp = await fetch(new URL(path, base), { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body), credentials: "same-origin" });
  const j = await resp.json().catch(() => ({}));
  return resp.ok ? j : { ok: false, reason: "server", error: j.message || `http ${resp.status}`, status: resp.status };
}

export const push = {
  async register(who = "") {
    const sw = navigator.serviceWorker;
    if (!sw || typeof PushManager === "undefined") return { ok: false, reason: "unsupported" };
    if (notifySupported() && Notification.permission === "denied") return { ok: false, reason: "denied" };
    try {
      await sw.register(new URL("__sw.js", base));
      const reg = await sw.ready;
      const { key } = await (await fetch(new URL("__push-key", base), { credentials: "same-origin" })).json();
      if (!key) return { ok: false, reason: "no-key" };
      const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64u.decode(key) });
      const answer = await post("__push-sub", { who, endpoint: sub.endpoint, p256dh: b64u.encode(sub.getKey("p256dh")), auth: b64u.encode(sub.getKey("auth")) });
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
      const answer = await post("__push-unsub", { endpoint: sub.endpoint });
      await sub.unsubscribe().catch(() => {});
      return answer;
    } catch (e) {
      return { ok: false, reason: "error", error: String((e && e.message) || e) };
    }
  },
};
