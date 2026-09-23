// fragment's browser library, served by the platform on every fragment as
// ./__fragment.js (it replaces the TypeScript runtime's __rt.js):
//
//   import * as fragment from "./__fragment.js";
//   await fragment.call("add_todo", { text: "milk" });        // an operation
//   fragment.live("list", {}, (r) => render(r.todos));         // a query, re-run on changes
//   fragment.subscribe("activity", (rec) => log(rec.body));    // a channel, from a cursor
//   fragment.presence.set({ name: "paul" });                   // who is here
//   fragment.presence.on((list) => showWho(list));
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
