// A Hermes a page talks to directly, the way Finite's dashboard does
// (docs/hermes-chat.md in fragment): a grant `{baseUrl, accessToken,
// expiresAt}` from the page's own platform, then Hermes' REST with the
// token as a bearer, and its gateway socket with a single-use ticket in the
// subprotocols (never in a URL). Nothing here is fragment's but the default
// grant's address.

/** Why a call failed: `signin` (sign in first), `access` (this person may
 * not), `starting` (its Hermes is being made; ask again soon), `request`
 * (try again), `unsupported` (an answer this client does not read). */
export class HermesError extends Error {
  constructor(message, kind = "request") {
    super(message);
    this.kind = kind;
  }
}

const TICKET = /^[A-Za-z0-9_-]{16,512}$/;
/** A grant within this long of its end is asked for again first. */
const MARGIN_S = 60;

/** The grant source a fragment's page has: `POST ./__hermes/access`. */
export function fragmentAccess(url = "./__hermes/access") {
  return async () => {
    const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: "{}", credentials: "same-origin", cache: "no-store" });
    const body = await r.json().catch(() => ({}));
    if (r.ok) return grant(body);
    const kind = { 401: "signin", 403: "access", 404: "access", 409: "starting" }[r.status] ?? "request";
    throw new HermesError(body.message || `the platform answered ${r.status}`, kind);
  };
}

function grant(v) {
  if (typeof v?.baseUrl !== "string" || typeof v?.accessToken !== "string" || typeof v?.expiresAt !== "number") {
    throw new HermesError("the platform's grant is not one this page reads", "unsupported");
  }
  return { baseUrl: v.baseUrl.endsWith("/") ? v.baseUrl : `${v.baseUrl}/`, accessToken: v.accessToken, expiresAt: v.expiresAt };
}

export class Hermes {
  /** `access`: an async function answering a grant (`fragmentAccess()`). */
  constructor(access = fragmentAccess()) {
    this.access = access;
    this.held = null;
  }

  /** The grant, asked for again when it is near its end (or `fresh`). */
  async grant(fresh = false) {
    if (fresh || !this.held || this.held.expiresAt - Date.now() / 1000 < MARGIN_S) {
      this.held = await this.access();
    }
    return this.held;
  }

  /** One call to Hermes' API; a 401 (its session ended, or it restarted)
   * asks for a fresh grant once. */
  async call(path, init = {}) {
    for (let attempt = 0; attempt < 2; attempt++) {
      const g = await this.grant(attempt > 0);
      const r = await fetch(new URL(path, g.baseUrl), {
        ...init,
        credentials: "omit", cache: "no-store", redirect: "error", referrerPolicy: "no-referrer",
        headers: { ...init.headers, authorization: `Bearer ${g.accessToken}` },
      });
      if (r.status === 401 && attempt === 0) continue;
      if (r.ok) return r.json();
      if ([401, 403].includes(r.status)) throw new HermesError("Hermes no longer takes this page's session", "access");
      throw new HermesError(`Hermes answered ${r.status}`, r.status === 404 ? "unsupported" : "request");
    }
    throw new HermesError("Hermes did not take a fresh session", "access");
  }

  /** Its chats, newest first. */
  async sessions() {
    return parseSessions(await this.call("api/sessions"));
  }

  /** One chat's stored messages. */
  async messages(id) {
    return parseMessages(await this.call(`api/sessions/${encodeURIComponent(id)}/messages`));
  }

  /** A gateway socket, open and ready: its own grant and ticket. */
  async gateway() {
    const { ticket } = await this.call("api/auth/ws-ticket", { method: "POST" });
    if (typeof ticket !== "string" || !TICKET.test(ticket)) throw new HermesError("Hermes' ticket is not one this page reads", "unsupported");
    const url = new URL("api/ws", this.held.baseUrl);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    return Gateway.open(url.href, ["hermes-gateway-v1", `hermes-gateway-ticket.${ticket}`]);
  }
}

/** Hermes' JSON-RPC over one socket: `request` answers a method's result,
 * and `onEvent` hears its events (`{type, session_id, payload}`). */
export class Gateway {
  static open(url, protocols) {
    return new Promise((resolve, reject) => {
      const g = new Gateway(new WebSocket(url, protocols));
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

  constructor(ws) {
    this.ws = ws;
    this.next = 1;
    this.waiting = new Map();
    this.listeners = new Set();
    this.closed = new Promise((resolve) => ws.addEventListener("close", () => {
      for (const { reject } of this.waiting.values()) reject(new HermesError("Hermes' socket closed"));
      this.waiting.clear();
      resolve();
    }));
    ws.addEventListener("message", (m) => {
      let v;
      try { v = JSON.parse(m.data); } catch { return; }
      if (v.method === "event" && v.params) {
        for (const f of this.listeners) f(v.params);
        return;
      }
      const w = this.waiting.get(v.id);
      if (!w) return;
      this.waiting.delete(v.id);
      if (v.error) w.reject(new HermesError(v.error.message || "Hermes refused the request"));
      else w.resolve(v.result);
    });
  }

  get open() {
    return this.ws.readyState === WebSocket.OPEN;
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
      this.ws.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
    });
  }

  close() {
    this.ws.close();
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
