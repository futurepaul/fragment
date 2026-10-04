// The uplink's platform end (sandcastle's docs/node.md, "The uplink";
// docs/self-host.md, seam 2). A sandcastle node the platform cannot reach
// dials `/api/nodes/uplink`, and one `Node` Durable Object per node id
// holds that WebSocket (hibernatable). Its `fetch` sends a request down
// the socket as one stream of frames and streams the answer back; a `101`
// comes back as a WebSocketPair bridged to the stream. `NodeContainer`
// (node.mjs) calls it in place of the node's URL for a node FRAGMENT_NODES
// lists with `"uplink": true`, naming the node in the request's host
// (`<id>.node.internal`): its calls are signed as before, and the node
// checks them as before. The frames, their order and their limits
// mirror sandcastle's `uplink::frame`, which is the reference (its tests
// cover them).

import { authHeader, nodeObject, nodesOf, signedAt } from "./node.mjs";

const HELLO = 1;
const PING = 2;
const PONG = 3;
const OPEN = 4;
const HEAD = 5;
const DATA = 6;
const END = 7;
const RESET = 8;
const WINDOW = 9;
const WS_MESSAGE = 10;
const WS_CLOSE = 11;
// ws-message: the last fragment of its message; the message is binary
const FIN = 1;
const BINARY = 2;

const HEADER_BYTES = 10;
const FRAME_PAYLOAD_BYTES_MAX = 256 << 10;
// a message carries one or more whole frames (a call's open, its small
// body and its end together: workerd's TCP holds back a small write until
// the last is acknowledged, and a Worker cannot set TCP_NODELAY)
const MESSAGE_BYTES_MAX = HEADER_BYTES + FRAME_PAYLOAD_BYTES_MAX;
const FRAMES_PER_MESSAGE_MAX = 64;
const DATA_BYTES_MAX = 64 << 10;
const HEAD_BYTES_MAX = 64 << 10;
const HEADERS_MAX = 100;
const PING_BYTES_MAX = 8;
const RESET_BYTES_MAX = 1024;
const WS_CLOSE_REASON_BYTES_MAX = 123;
const WS_MESSAGE_BYTES_MAX = 4 << 20;
const STREAMS_MAX = 128;
const WINDOW_BYTES = 256 << 10;
const STREAM_BUFFERED_BYTES_MAX = 4 << 20;
const BUFFERED_BYTES_MAX = 32 << 20;
// a dial's signature, at most this far from our clock (node.mjs's window)
const WINDOW_S = 60;
const DIALS_PER_WINDOW = 32;
// a call waits this long for a node that is dialing (a restart, a reconnect)
const UPLINK_WAIT_MS = 5000;
// a call the node had not answered when its uplink dropped, and that is
// safe to ask again (a GET, not an upgrade: `wait`, inspect, a guest's
// page), waits this long for the node's next dial, and is asked again there
const AGAIN_WAIT_MS = 30_000;
// after one way of a bridged WebSocket closes, the other has this long to
const WS_CLOSE_WAIT_MS = 10_000;

const NODE_HEADER = "x-sandcastle-node";
const NONCE_HEADER = "x-sandcastle-nonce";
const AUTH = "x-sandcastle-auth";
// the uplink's sockets, as this object's hibernation API tags them
const TAG = "uplink";
// the dials' nonces taken inside the window: [[nonce, t], …]
const DIALS = "uplink/dials";
// headers of one hop, or the WebSocket's own: never sent down a stream
const HOP = new Set(["connection", "keep-alive", "proxy-connection", "transfer-encoding", "te", "trailer", "upgrade", "host", "content-length", "sec-websocket-key", "sec-websocket-version", "sec-websocket-extensions", "sec-websocket-accept"]);
// close codes a Worker's WebSocket may send
const SENDABLE_CLOSE = (code) => code === 1000 || (code >= 3000 && code <= 4999) || (code >= 1001 && code <= 1014 && ![1004, 1005, 1006].includes(code));

const enc = new TextEncoder();
const dec = new TextDecoder();
const EMPTY = new Uint8Array();

const dialString = (node, nonce, t) => `sandcastle-uplink-v1\n${node}\n${nonce}\n${t}`;
const helloString = (node, nonce, t) => `sandcastle-uplink-hello-v1\n${node}\n${nonce}\n${t}`;

function encode(kind, stream, payload = EMPTY, flags = 0) {
  if (payload.length > FRAME_PAYLOAD_BYTES_MAX) throw new Error(`a frame of ${payload.length} bytes`);
  const out = new Uint8Array(HEADER_BYTES + payload.length);
  const v = new DataView(out.buffer);
  out[0] = kind;
  out[1] = flags;
  v.setUint32(2, stream);
  v.setUint32(6, payload.length);
  out.set(payload, HEADER_BYTES);
  return out;
}

const u32 = (n) => {
  const b = new Uint8Array(4);
  new DataView(b.buffer).setUint32(0, n);
  return b;
};

// A message: one or more whole frames (uplink::frame::decode_all), at most
// MESSAGE_BYTES_MAX and FRAMES_PER_MESSAGE_MAX of them, each checked whole;
// throws on anything else, and the connection ends.
function decodeAll(message) {
  const b = new Uint8Array(message);
  if (b.length > MESSAGE_BYTES_MAX) throw new Error(`a message of ${b.length} bytes`);
  const frames = [];
  // Bounded by the message: each turn takes at least a header from it.
  for (let at = 0; at < b.length || frames.length === 0; ) {
    if (frames.length === FRAMES_PER_MESSAGE_MAX) throw new Error(`a message of more than ${FRAMES_PER_MESSAGE_MAX} frames`);
    if (b.length - at < HEADER_BYTES) throw new Error(`a frame of ${b.length - at} bytes, short of its header`);
    const len = new DataView(b.buffer, b.byteOffset + at + 6, 4).getUint32(0);
    if (len > b.length - at - HEADER_BYTES) throw new Error(`a frame that says ${len} bytes and carries ${b.length - at - HEADER_BYTES}`);
    frames.push(decode(b.subarray(at, at + HEADER_BYTES + len)));
    at += HEADER_BYTES + len;
  }
  return frames;
}

// Frames, encoded, as one message.
function joined(frames) {
  if (frames.length === 1) return frames[0];
  const out = new Uint8Array(frames.reduce((n, f) => n + f.length, 0));
  let at = 0;
  for (const f of frames) {
    out.set(f, at);
    at += f.length;
  }
  return out;
}

// One frame, checked whole (uplink::frame::decode).
function decode(b) {
  if (b.length < HEADER_BYTES) throw new Error(`a frame of ${b.length} bytes, short of its header`);
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const [kind, flags, stream, len] = [b[0], b[1], v.getUint32(2), v.getUint32(6)];
  if (len > FRAME_PAYLOAD_BYTES_MAX) throw new Error(`a payload of ${len} bytes`);
  if (len !== b.length - HEADER_BYTES) throw new Error(`a frame that says ${len} bytes and carries ${b.length - HEADER_BYTES}`);
  if (kind < HELLO || kind > WS_CLOSE) throw new Error(`an unknown kind ${kind}`);
  if ((kind <= PONG) !== (stream === 0)) throw new Error(`kind ${kind} on stream ${stream}`);
  if (flags & ~(kind === WS_MESSAGE ? FIN | BINARY : 0)) throw new Error(`kind ${kind} with flags ${flags}`);
  const payload = b.subarray(HEADER_BYTES);
  const bad =
    ((kind === HELLO) && (len === 0 || len > 128)) ||
    ((kind === PING || kind === PONG) && len > PING_BYTES_MAX) ||
    ((kind === OPEN || kind === HEAD) && (len === 0 || len > HEAD_BYTES_MAX)) ||
    (kind === DATA && (len === 0 || len > DATA_BYTES_MAX)) ||
    (kind === END && len !== 0) ||
    (kind === RESET && len > RESET_BYTES_MAX) ||
    (kind === WINDOW && (len !== 4 || v.getUint32(HEADER_BYTES) === 0 || v.getUint32(HEADER_BYTES) > WINDOW_BYTES)) ||
    (kind === WS_CLOSE && (len === 1 || len > 2 + WS_CLOSE_REASON_BYTES_MAX));
  if (bad) throw new Error(`kind ${kind} with a payload of ${len} bytes`);
  return { kind, flags, stream, payload };
}

// `s` cut to `max` bytes of UTF-8, on a character's boundary.
function cut(s, max) {
  const b = enc.encode(s);
  if (b.length <= max) return b;
  // Bounded by four steps: back past the continuation bytes (10xxxxxx)
  // of the character the cut would split.
  let end = max;
  while (end > 0 && (b[end] & 0xc0) === 0x80) end--;
  return b.subarray(0, end);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// A random Sec-WebSocket-Key: the node's router answers the upgrade.
function wsKey() {
  return btoa(String.fromCharCode(...crypto.getRandomValues(new Uint8Array(16))));
}

// A node of FRAGMENT_NODES that dials in, by the id a dial names, or null.
function dialer(env, id) {
  const node = nodesOf(env)?.byId.get(id || "");
  return node && node.uplink ? node : null;
}

// The router's half of `/api/nodes/uplink`: a listed node's dial, to its
// object, which checks the rest.
export function routeNodeUplink(request, env) {
  if (!nodesOf(env)) return Response.json({ error: "this platform takes no uplink" }, { status: 404 });
  const id = request.headers.get(NODE_HEADER);
  if (!dialer(env, id)) return Response.json({ error: "not one of this platform's nodes that dial in" }, { status: 403 });
  if ((request.headers.get("upgrade") || "").toLowerCase() !== "websocket") return Response.json({ error: "the uplink is a WebSocket" }, { status: 426 });
  return nodeObject(env, id).fetch(new Request(`https://${id}.node.internal/__uplink/dial`, request));
}

// One stream: a call down the uplink, and its answer.
class Exchange {
  constructor(uplink, ws, id, method, opening, again) {
    this.uplink = uplink;
    this.ws = ws;
    this.id = id;
    this.method = method;
    // its open, kept to ask again on the next uplink when `again` allows
    this.opening = opening;
    this.again = again;
    // the answer's status, once its head came; the order's state after it
    this.status = null;
    this.ended = false;
    this.closedIn = false;
    this.closedOut = false;
    this.fragment = null;
    this.parts = [];
    this.partsBytes = 0;
    // our window to send the request's body
    this.credit = WINDOW_BYTES;
    this.creditWaiters = [];
    // the answer's body: what came, what was granted, what is queued
    this.body = null;
    this.received = 0;
    this.granted = 0;
    this.queued = 0;
    this.pair = null;
    this.done = false;
    this.head = new Promise((resolve, reject) => (this.settle = { resolve, reject }));
    this.head.catch(() => {});
  }

  send(kind, payload, flags) {
    this.ws.send(encode(kind, this.id, payload, flags));
  }

  // Frames that are ready together, as one message.
  flush(frames) {
    if (frames.length) this.ws.send(joined(frames));
  }

  // Checks `f` against what came before (uplink::frame::Exchange, the
  // platform's side); answers what is wrong, or null.
  order(f) {
    switch (f.kind) {
      case RESET:
      case WINDOW:
        return null;
      case HEAD:
        return this.status === null ? null : "a second head";
      case DATA:
      case END:
        if (this.status === null) return "body before the head";
        if (this.status === 101) return "body frames on a WebSocket";
        if (this.ended) return "body after its end";
        return null;
      case WS_MESSAGE:
      case WS_CLOSE:
        if (this.status !== 101) return "WebSocket frames on a stream that is not one";
        if (this.closedIn) return "WebSocket frames after its close";
        if (f.kind === WS_MESSAGE && this.fragment !== null && this.fragment !== !!(f.flags & BINARY)) return "a fragment that changes its message's type";
        return null;
      default:
        return `kind ${f.kind} never travels this way`;
    }
  }

  // The bytes held for this stream change by `n`.
  hold(n) {
    this.uplink.held += n;
  }

  // Waits for room in the node's window for `n` bytes.
  async room(n) {
    // Bounded by the node's grants: the stream's end wakes it too.
    while (this.credit < n && !this.done) await new Promise((r) => this.creditWaiters.push(r));
    if (this.done) throw new Error("the call ended");
    this.credit -= n;
  }

  // The request's body, windowed, then its end. `pending` (the open) goes
  // with what the body has ready at once, so a call with a small body is
  // one message; a body that makes the next read wait sends what is ready.
  async pump(body, pending) {
    const reader = body.getReader();
    const TICK = Symbol("tick");
    const size = () => pending.reduce((n, f) => n + f.length, 0);
    try {
      let next = reader.read();
      // Bounded by the body: its reader ends it.
      for (;;) {
        let r = pending.length ? await Promise.race([next, sleep(0).then(() => TICK)]) : await next;
        if (r === TICK) {
          this.flush(pending);
          pending = [];
          r = await next;
        }
        if (r.done || this.done) break;
        const bytes = r.value instanceof Uint8Array ? r.value : new Uint8Array(r.value);
        for (let at = 0; at < bytes.length; at += DATA_BYTES_MAX) {
          const chunk = bytes.subarray(at, at + DATA_BYTES_MAX);
          // never hold frames back while waiting for the node's window
          if (this.credit < chunk.length) {
            this.flush(pending);
            pending = [];
          }
          await this.room(chunk.length);
          const frame = encode(DATA, this.id, chunk);
          if (size() + frame.length > MESSAGE_BYTES_MAX || pending.length + 2 > FRAMES_PER_MESSAGE_MAX) {
            this.flush(pending);
            pending = [];
          }
          pending.push(frame);
        }
        next = reader.read();
      }
      if (!this.done) this.flush([...pending, encode(END, this.id)]);
      // the node answered before the body was done: the rest is not wanted
      else reader.cancel().catch(() => {});
    } catch (e) {
      if (!this.done) this.reset(`the request's body failed: ${(e && e.message) || e}`);
      reader.cancel().catch(() => {});
    }
  }

  onHead(payload) {
    let h;
    try {
      h = JSON.parse(dec.decode(payload));
    } catch {
      return this.reset("a head that is not its JSON");
    }
    const ok = Number.isInteger(h.status) && h.status >= 100 && h.status <= 599 && Array.isArray(h.headers) && h.headers.length <= HEADERS_MAX;
    if (!ok) return this.reset("a head out of bounds");
    this.status = h.status;
    const headers = new Headers();
    for (const [k, v] of h.headers) {
      if (HOP.has(String(k).toLowerCase())) continue;
      try {
        headers.append(k, v);
      } catch {}
    }
    if (h.status === 101) return this.settle.resolve(this.bridge(headers));
    if (h.status < 200) return this.reset(`an answer of ${h.status}`);
    const nobody = this.method === "HEAD" || [204, 205, 304].includes(h.status);
    if (nobody) return this.settle.resolve(new Response(null, { status: h.status, headers }));
    const self = this;
    const stream = new ReadableStream(
      {
        start(c) {
          self.body = c;
        },
        pull() {
          self.grant();
        },
        cancel(reason) {
          self.reset(`the caller stopped reading: ${reason}`);
        },
      },
      { highWaterMark: WINDOW_BYTES, size: (chunk) => chunk.byteLength },
    );
    this.settle.resolve(new Response(stream, { status: h.status, headers }));
  }

  // Grants the node the window its body's reader has freed.
  grant() {
    if (!this.body || this.done) return;
    const queued = WINDOW_BYTES - (this.body.desiredSize ?? WINDOW_BYTES);
    const taken = this.queued - queued;
    if (taken > 0) {
      this.queued = queued;
      this.hold(-taken);
    }
    const consumed = this.received - this.queued;
    if (consumed - this.granted >= WINDOW_BYTES / 2) {
      this.send(WINDOW, u32(consumed - this.granted));
      this.granted = consumed;
    }
  }

  onData(payload) {
    const n = payload.length;
    if (this.queued + n > WINDOW_BYTES) return this.reset("data past its window");
    if (this.uplink.held + n > BUFFERED_BYTES_MAX) return this.reset("the platform holds too much for this uplink");
    this.received += n;
    if (!this.body) return;
    this.queued += n;
    this.hold(n);
    this.body.enqueue(payload.slice());
  }

  onEnd() {
    this.ended = true;
    try {
      this.body?.close();
    } catch {}
    this.finish();
  }

  onWindow(n) {
    if (this.credit + n > WINDOW_BYTES) return this.reset("a window past its size");
    this.credit += n;
    for (const w of this.creditWaiters.splice(0)) w();
  }

  // A 101: a WebSocketPair, its server end bridged to the stream.
  bridge(headers) {
    const [client, server] = Object.values(new WebSocketPair());
    server.accept();
    this.pair = server;
    server.addEventListener("message", (e) => this.sendMessage(e.data));
    server.addEventListener("close", (e) => this.closeOut(e.code, e.reason));
    server.addEventListener("error", () => this.reset("the caller's WebSocket failed"));
    const protocol = headers.get("sec-websocket-protocol");
    return new Response(null, { status: 101, webSocket: client, headers: protocol ? { "sec-websocket-protocol": protocol } : {} });
  }

  sendMessage(data) {
    if (this.done || this.closedOut) return;
    const binary = typeof data !== "string";
    const bytes = binary ? new Uint8Array(data) : enc.encode(data);
    if (bytes.length > WS_MESSAGE_BYTES_MAX) return this.reset(`a WebSocket message past ${WS_MESSAGE_BYTES_MAX} bytes`);
    const flags = binary ? BINARY : 0;
    if (bytes.length === 0) return this.send(WS_MESSAGE, EMPTY, flags | FIN);
    for (let at = 0; at < bytes.length; at += FRAME_PAYLOAD_BYTES_MAX) {
      const last = at + FRAME_PAYLOAD_BYTES_MAX >= bytes.length;
      this.send(WS_MESSAGE, bytes.subarray(at, at + FRAME_PAYLOAD_BYTES_MAX), flags | (last ? FIN : 0));
    }
  }

  onMessage(f) {
    const binary = !!(f.flags & BINARY);
    if (this.partsBytes + f.payload.length > WS_MESSAGE_BYTES_MAX) return this.reset(`a WebSocket message past ${WS_MESSAGE_BYTES_MAX} bytes`);
    if (this.uplink.held + f.payload.length > BUFFERED_BYTES_MAX || this.partsBytes + f.payload.length > STREAM_BUFFERED_BYTES_MAX) return this.reset("the platform holds too much for this uplink");
    this.parts.push(f.payload.slice());
    this.partsBytes += f.payload.length;
    this.hold(f.payload.length);
    if (!(f.flags & FIN)) {
      this.fragment = binary;
      return;
    }
    this.fragment = null;
    const whole = new Uint8Array(this.partsBytes);
    let at = 0;
    for (const p of this.parts) {
      whole.set(p, at);
      at += p.length;
    }
    this.hold(-this.partsBytes);
    this.parts = [];
    this.partsBytes = 0;
    try {
      this.pair.send(binary ? whole.buffer : dec.decode(whole));
    } catch (e) {
      this.reset(`the caller's WebSocket: ${(e && e.message) || e}`);
    }
  }

  // The node closed its WebSocket: the caller's closes too, and the node
  // has its answer from here (a caller need not answer a close).
  onClose(payload) {
    this.closedIn = true;
    const v = new DataView(payload.buffer, payload.byteOffset, payload.byteLength);
    const code = payload.length >= 2 ? v.getUint16(0) : 1000;
    const reason = payload.length > 2 ? dec.decode(payload.subarray(2)) : "";
    try {
      this.pair.close(SENDABLE_CLOSE(code) ? code : 1000, reason);
    } catch {}
    if (!this.closedOut) {
      this.closedOut = true;
      this.send(WS_CLOSE, payload.slice());
    }
    this.finish();
  }

  // The caller closed its WebSocket (or answered the node's close).
  closeOut(code, reason) {
    if (this.done || this.closedOut) return;
    this.closedOut = true;
    this.send(WS_CLOSE, payloadOfClose(code, reason));
    this.finish();
    if (!this.done) setTimeout(() => !this.done && this.reset("the node did not close its WebSocket"), WS_CLOSE_WAIT_MS);
  }

  // The stream is over when its answer has ended, or its WebSocket has
  // closed both ways.
  finish() {
    if (this.status === 101 ? this.closedIn && this.closedOut : this.ended) this.end(null);
  }

  // Ends the stream here, telling the node.
  reset(why) {
    if (this.done) return;
    try {
      this.send(RESET, cut(why, RESET_BYTES_MAX));
    } catch {}
    this.end(new Error(why));
  }

  // Ends the stream here: `e` its failure, or null.
  end(e) {
    if (this.done) return;
    this.done = true;
    this.uplink.forget(this);
    this.hold(-(this.queued + this.partsBytes));
    this.queued = 0;
    this.partsBytes = 0;
    for (const w of this.creditWaiters.splice(0)) w();
    if (!e) return;
    this.settle.reject(e);
    try {
      this.body?.error(e);
    } catch {}
    try {
      this.pair?.close(1011, String(e.message).slice(0, 100));
    } catch {}
  }
}

function payloadOfClose(code, reason) {
  if (!code || code === 1005) return EMPTY;
  const r = cut(reason || "", WS_CLOSE_REASON_BYTES_MAX);
  const out = new Uint8Array(2 + r.length);
  new DataView(out.buffer).setUint16(0, code);
  out.set(r, 2);
  return out;
}

// The `Node` object's work (entry.mjs declares the class): it takes the
// node's dials, and tunnels calls to the node.
export class Uplink {
  #ctx;
  #env;
  #streams = new Map();
  #next;
  // calls waiting for a dial
  #waiters = [];
  // bytes held for every stream's reader, together
  held = 0;

  constructor(ctx, env) {
    this.#ctx = ctx;
    this.#env = env;
    // a stream's id: from anywhere, so ids after a hibernation are new ones
    this.#next = crypto.getRandomValues(new Uint32Array(1))[0] || 1;
  }

  fetch(request) {
    if (new URL(request.url).pathname === "/__uplink/dial") return this.#dial(request);
    return this.#call(request);
  }

  // The newest open uplink, if any.
  #socket() {
    let best = null;
    let at = -1;
    for (const ws of this.#ctx.getWebSockets(TAG)) {
      const a = ws.deserializeAttachment();
      if (a && a.at > at && ws.readyState === 1) [best, at] = [ws, a.at];
    }
    return best;
  }

  async #dial(request) {
    const h = request.headers;
    const id = h.get(NODE_HEADER);
    const node = dialer(this.#env, id);
    const nonce = h.get(NONCE_HEADER) || "";
    // this object is that node's alone
    if (!node || this.#ctx.id.toString() !== this.#env.NODE.idFromName(id).toString()) return Response.json({ error: "not one of this platform's nodes that dial in" }, { status: 403 });
    if (!/^[0-9a-f]{32}$/.test(nonce)) return Response.json({ error: "a dial's nonce is 32 lowercase hex digits" }, { status: 400 });
    const t = await signedAt(node, h.get(AUTH), (t) => dialString(id, nonce, t));
    if (t === null) return Response.json({ error: "a bad signature" }, { status: 401 });
    // the book of the window's dials: one seen before is a replay
    const now = Math.floor(Date.now() / 1000);
    const seen = ((await this.#ctx.storage.get(DIALS)) || []).filter(([, at]) => Math.abs(now - at) <= WINDOW_S);
    if (seen.some(([n]) => n === nonce)) return Response.json({ error: "a dial seen before" }, { status: 401 });
    if (seen.length >= DIALS_PER_WINDOW) return Response.json({ error: `more than ${DIALS_PER_WINDOW} dials inside ${WINDOW_S} s` }, { status: 429 });
    seen.push([nonce, t]);
    await this.#ctx.storage.put(DIALS, seen);
    const hello = encode(HELLO, 0, enc.encode(await authHeader(node, now, helloString(id, nonce, now))));
    // from here on, no await until the hello is sent: nothing may use the
    // socket before it (a call asked again on it included)
    const [client, server] = Object.values(new WebSocketPair());
    this.#ctx.acceptWebSocket(server, [TAG]);
    server.serializeAttachment({ at: Date.now(), nonce });
    server.send(hello);
    // a newer dial replaces the older connection, and its streams
    for (const old of this.#ctx.getWebSockets(TAG)) {
      if (old === server) continue;
      this.#lost(old, "the node dialed again");
      try {
        old.close(4000, "replaced by a newer dial");
      } catch {}
    }
    console.log(JSON.stringify({ uplink: id, dialed: true }));
    for (const w of this.#waiters.splice(0)) w();
    return new Response(null, { status: 101, webSocket: client });
  }

  // The uplink, or one that dials within `ms`.
  async #connected(ms) {
    const ws = this.#socket();
    if (ws) return ws;
    await Promise.race([new Promise((r) => this.#waiters.push(r)), sleep(ms)]);
    return this.#socket();
  }

  async #call(request) {
    const ws = await this.#connected(UPLINK_WAIT_MS);
    const u = new URL(request.url);
    if (!ws) return Response.json({ error: `the node ${u.hostname.split(".")[0]} has no uplink open` }, { status: 503 });
    if (this.#streams.size >= STREAMS_MAX) return Response.json({ error: `the uplink has ${STREAMS_MAX} calls in flight` }, { status: 503 });
    // Bounded by STREAMS_MAX: at most that many ids are taken.
    do this.#next = (this.#next % 0xffffffff) + 1;
    while (this.#streams.has(this.#next));
    const id = this.#next;
    const upgrade = (request.headers.get("upgrade") || "").toLowerCase() === "websocket";
    const headers = [];
    for (const [k, v] of request.headers) if (!HOP.has(k)) headers.push([k, v]);
    const body = upgrade || request.method === "GET" || request.method === "HEAD" ? null : request.body;
    if (upgrade) headers.push(["connection", "Upgrade"], ["upgrade", "websocket"], ["sec-websocket-key", wsKey()], ["sec-websocket-version", "13"]);
    const length = request.headers.get("content-length");
    if (!body) headers.push(["content-length", "0"]);
    else if (length) headers.push(["content-length", length]);
    if (headers.length > HEADERS_MAX) return Response.json({ error: `at most ${HEADERS_MAX} headers` }, { status: 431 });
    const open = enc.encode(JSON.stringify({ method: request.method, path: u.pathname + u.search, headers }));
    if (open.length > HEAD_BYTES_MAX) return Response.json({ error: `a head of at most ${HEAD_BYTES_MAX} bytes` }, { status: 431 });
    const opening = encode(OPEN, id, open);
    const x = new Exchange(this, ws, id, request.method, opening, request.method === "GET" && !upgrade);
    this.#streams.set(id, x);
    if (body) x.pump(body, [opening]);
    else x.flush([opening, encode(END, id)]);
    // a call that fails here is the node's 502, as a direct call's network
    // failure is NodeContainer's error
    return x.head.catch((e) => Response.json({ error: String((e && e.message) || e) }, { status: 502 }));
  }

  // Asks `x` again on the node's next uplink (it dropped before the node
  // answered), or fails it when none comes within AGAIN_WAIT_MS.
  async #askAgain(x, why) {
    x.ws = null;
    const ws = await this.#connected(AGAIN_WAIT_MS);
    if (x.done) return;
    if (!ws) return x.end(new Error(`${why}, and the node did not dial again within ${AGAIN_WAIT_MS / 1000} s`));
    x.ws = ws;
    console.log(JSON.stringify({ uplink: "asked again", stream: x.id }));
    x.flush([x.opening, encode(END, x.id)]);
  }

  forget(x) {
    if (this.#streams.get(x.id) === x) this.#streams.delete(x.id);
  }

  // Every stream on `ws` fails, its connection gone; but one the node had
  // not answered, and that is safe to ask again, waits for the next.
  #lost(ws, why) {
    for (const x of [...this.#streams.values()]) {
      if (x.ws !== ws) continue;
      if (x.again && x.status === null) this.#askAgain(x, why);
      else x.end(new Error(why));
    }
  }

  webSocketMessage(ws, message) {
    if (typeof message === "string") return this.#broken(ws, "a text message");
    let frames;
    try {
      frames = decodeAll(message);
    } catch (e) {
      return this.#broken(ws, e.message);
    }
    for (const f of frames) this.#frame(ws, f);
  }

  #frame(ws, f) {
    if (f.kind === PING) return ws.send(encode(PONG, 0, f.payload));
    if (f.kind === PONG) return;
    if (f.kind === HELLO) return this.#broken(ws, "a hello from the node");
    const x = this.#streams.get(f.stream);
    // a stream that is over: the node sent this before it learned
    if (!x || x.ws !== ws) return;
    const wrong = x.order(f);
    if (wrong) return x.reset(wrong);
    switch (f.kind) {
      case HEAD:
        return x.onHead(f.payload);
      case DATA:
        return x.onData(f.payload);
      case END:
        return x.onEnd();
      case RESET:
        return x.end(new Error(`the node: ${dec.decode(f.payload)}`));
      case WINDOW:
        return x.onWindow(new DataView(f.payload.buffer, f.payload.byteOffset, 4).getUint32(0));
      case WS_MESSAGE:
        return x.onMessage(f);
      case WS_CLOSE:
        return x.onClose(f.payload);
    }
  }

  webSocketClose(ws, code, reason) {
    this.#lost(ws, `the node's uplink closed (${code} ${reason})`);
    try {
      ws.close(1000, "closed");
    } catch {}
  }

  webSocketError(ws) {
    this.#lost(ws, "the node's uplink failed");
  }

  #broken(ws, why) {
    console.log(JSON.stringify({ uplink: "broken", why }));
    this.#lost(ws, `the node broke the protocol: ${why}`);
    try {
      ws.close(1002, String(why).slice(0, 100));
    } catch {}
  }
}
