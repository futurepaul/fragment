// A Hermes a page reaches by its key (docs/runtime-seam.md in fragment):
// the page's own iroh key, an admission for it from the page's platform
// (`{endpoint, relay, admission, host, token, expiresAt}`), then Hermes'
// REST and its gateway socket over one connection to the computer, each on
// a stream of its own. Hermes runs in loopback mode behind the admission:
// its session token goes in a header, or the socket's URL, and never
// leaves that connection. Nothing here is fragment's but the default
// access's address and the client's.

/** Why a call failed: `signin` (sign in first), `access` (this person may
 * not), `starting` (its Hermes is being made; ask again soon), `request`
 * (try again), `unsupported` (an answer this client does not read). */
export class HermesError extends Error {
  constructor(message, kind = "request") {
    super(message);
    this.kind = kind;
  }
}

/** The platform's computer client, served on every fragment host. */
const CLIENT = "/__computer/client.js";
/** An admission is renewed this long before its end. */
const MARGIN_S = 60;

let loaded = null;
function client() {
  loaded ??= import(CLIENT).catch((e) => {
    loaded = null;
    throw new HermesError(`the computer client did not load: ${e.message}`);
  });
  return loaded;
}

const message = (e) => e?.message ?? String(e);

/** The access a fragment's page has: `POST ./__hermes/access {peer}`. */
export function fragmentAccess(url = "./__hermes/access") {
  return async (peer) => {
    const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ peer }), credentials: "same-origin", cache: "no-store" });
    const body = await r.json().catch(() => ({}));
    if (r.ok) return access(body);
    const kind = { 401: "signin", 403: "access", 404: "access", 409: "starting" }[r.status] ?? "request";
    throw new HermesError(body.message || `the platform answered ${r.status}`, kind);
  };
}

function access(v) {
  const strings = ["endpoint", "relay", "admission", "host", "token"];
  if (strings.some((k) => typeof v?.[k] !== "string") || typeof v?.expiresAt !== "number") {
    throw new HermesError("the platform's answer is not one this page reads", "unsupported");
  }
  return { endpoint: v.endpoint, relay: v.relay, admission: v.admission, host: v.host, token: v.token, expiresAt: v.expiresAt };
}

export class Hermes {
  /** `access`: an async function of the page's key answering an access
   * (`fragmentAccess()`). */
  constructor(access = fragmentAccess()) {
    this.access = access;
    this.peer = null;
    /** The admitted connection and the access it was made with. */
    this.held = null;
    this.making = null;
    this.renewal = null;
  }

  /** The admitted connection, made when there is none. */
  async connect() {
    if (this.held) return this.held;
    this.making ??= (async () => {
      try {
        const { Peer } = await client();
        this.peer ??= new Peer();
        const a = await this.access(this.peer.id());
        let computer;
        try {
          computer = await this.peer.connect(a.endpoint, a.relay, a.admission, a.host);
        } catch (e) {
          throw new HermesError(`Hermes was not reached: ${message(e)}`);
        }
        this.held = { computer, ...a };
        this.renew(this.held);
        return this.held;
      } finally {
        this.making = null;
      }
    })();
    return this.making;
  }

  /** Its admission renewed on the same connection before it ends. */
  renew(held) {
    clearTimeout(this.renewal);
    const due = Math.max(1, held.expiresAt - MARGIN_S - Date.now() / 1000);
    this.renewal = setTimeout(async () => {
      if (this.held !== held) return;
      try {
        const a = await this.access(this.peer.id());
        if (a.endpoint !== held.endpoint) return this.drop(held);
        held.expiresAt = await held.computer.admit(a.admission);
        this.renew(held);
      } catch {
        // it ends with its admission; the next call connects again
      }
    }, due * 1000);
  }

  drop(held) {
    if (this.held !== held) return;
    this.held = null;
    clearTimeout(this.renewal);
    held.computer.close();
  }

  /** One call to Hermes' API; one that fails on the connection, or that
   * Hermes refuses (it was made again), connects again once. */
  async call(path, { method = "GET" } = {}) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const held = await this.connect();
      let r;
      try {
        r = await held.computer.fetch(method, `/${path}`, JSON.stringify({ "x-hermes-session-token": held.token }), new Uint8Array());
      } catch (e) {
        this.drop(held);
        if (attempt === 0) continue;
        throw new HermesError(`Hermes did not answer: ${message(e)}`);
      }
      if (r.status === 401 && attempt === 0) {
        this.drop(held);
        continue;
      }
      if (r.status >= 200 && r.status < 300) return JSON.parse(new TextDecoder().decode(r.body));
      if ([401, 403].includes(r.status)) throw new HermesError("Hermes no longer takes this page's session", "access");
      throw new HermesError(`Hermes answered ${r.status}`, r.status === 404 ? "unsupported" : "request");
    }
  }

  /** Its chats, newest first. */
  async sessions() {
    return parseSessions(await this.call("api/sessions"));
  }

  /** One chat's stored messages. */
  async messages(id) {
    return parseMessages(await this.call(`api/sessions/${encodeURIComponent(id)}/messages`));
  }

  /** A gateway socket, open and ready, on a stream of its own. */
  async gateway() {
    for (let attempt = 0; attempt < 2; attempt++) {
      const held = await this.connect();
      let socket;
      try {
        socket = await held.computer.websocket(`/api/ws?token=${encodeURIComponent(held.token)}`, ["hermes-gateway-v1"]);
      } catch (e) {
        this.drop(held);
        if (attempt === 0) continue;
        throw new HermesError(`Hermes' socket did not open: ${message(e)}`);
      }
      return Gateway.open(socket);
    }
  }
}

/** Hermes' JSON-RPC over one socket (the client's `Socket`: `send`, and
 * `next` until it answers undefined): `request` answers a method's result,
 * and `onEvent` hears its events (`{type, session_id, payload}`). */
export class Gateway {
  static open(socket) {
    return new Promise((resolve, reject) => {
      const g = new Gateway(socket);
      let ready = false;
      const off = g.onEvent((e) => {
        if (e.type !== "gateway.ready") return;
        ready = true;
        off();
        resolve(g);
      });
      g.closed.then(() => ready || reject(new HermesError("Hermes' socket did not open")));
    });
  }

  constructor(socket) {
    this.socket = socket;
    this.open = true;
    this.next = 1;
    this.waiting = new Map();
    this.listeners = new Set();
    this.closed = this.read().finally(() => {
      this.open = false;
      for (const { reject } of this.waiting.values()) reject(new HermesError("Hermes' socket closed"));
      this.waiting.clear();
    });
  }

  /** Every message until the socket ends. */
  async read() {
    for (;;) {
      let text;
      try {
        text = await this.socket.next();
      } catch {
        return;
      }
      if (text === undefined) return;
      let v;
      try { v = JSON.parse(text); } catch { continue; }
      if (v.method === "event" && v.params) {
        for (const f of this.listeners) f(v.params);
        continue;
      }
      const w = this.waiting.get(v.id);
      if (!w) continue;
      this.waiting.delete(v.id);
      if (v.error) w.reject(new HermesError(v.error.message || "Hermes refused the request"));
      else w.resolve(v.result);
    }
  }

  onEvent(f) {
    this.listeners.add(f);
    return () => this.listeners.delete(f);
  }

  request(method, params = {}) {
    if (!this.open) return Promise.reject(new HermesError("Hermes' socket is closed"));
    const id = this.next++;
    return new Promise((resolve, reject) => {
      this.waiting.set(id, { resolve, reject });
      this.socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params })).catch((e) => {
        this.waiting.delete(id);
        reject(new HermesError(`Hermes' socket did not take it: ${message(e)}`));
      });
    });
  }

  close() {
    this.socket.close();
  }
}

/** `GET api/sessions`, read as a list: `{id, title, preview, source, messageCount, lastActive}`. */
export function parseSessions(v) {
  if (!Array.isArray(v?.sessions)) throw new HermesError("Hermes' chats are not a list this page reads", "unsupported");
  return v.sessions.filter((s) => typeof s?.id === "string" && s.id).map((s) => ({
    id: s.id,
    title: typeof s.title === "string" ? s.title : "",
    preview: typeof s.preview === "string" ? s.preview : "",
    source: typeof s.source === "string" ? s.source : "unknown",
    messageCount: Number(s.message_count) || 0,
    lastActive: Number(s.last_active ?? s.started_at) || 0,
  }));
}

/** `GET api/sessions/{id}/messages`, read as rows: `{role, text, tools}`. */
export function parseMessages(v) {
  if (!Array.isArray(v?.messages)) throw new HermesError("Hermes' messages are not a list this page reads", "unsupported");
  return v.messages.filter((m) => m?.display_kind !== "hidden").map((m) => ({
    role: typeof m.role === "string" ? m.role : "unknown",
    text: typeof m.display_content === "string" ? m.display_content : textOf(m.content),
    tool: typeof m.tool_name === "string" ? m.tool_name : null,
    calls: (Array.isArray(m.tool_calls) ? m.tool_calls : []).map((c) => c?.function?.name ?? c?.name).filter((n) => typeof n === "string"),
  }));
}

function textOf(content) {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map((p) => (typeof p === "string" ? p : p?.text ?? "")).join("");
  return "";
}
