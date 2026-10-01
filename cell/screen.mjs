// A computer's screen beside its chat, served by the platform on every
// fragment as ./__screen.js (with ./__screen.css): a Hermes' screen, its
// bot desktop (docs/one-home.md, phase 4), shown live, with Take over and
// Give back. Its owner's and their editors' pages alone are admitted
// (decision 5: the screen holds the owner's logins); on anyone else's the
// pane stays hidden.
//
//   <link rel="stylesheet" href="./__screen.css">
//   <script type="module">
//     import { mountScreen } from "./__screen.js";
//     mountScreen(document.querySelector("#screen"));
//   </script>
//
// How: the page's own iroh key and an admission for it from the platform
// (`POST ./__hermes/access {peer}`: the computer's endpoint, relay,
// admission, and Hermes' session token), then, over one connection to the
// computer with the platform's computer client (/__computer/client.js),
// Hermes' socket (`/api/ws`: `display.observe` for a ticket and this page's
// viewer id; the lease's `acquire` and `release`; `display.lease` events)
// and its display socket (`/api/display/ws?display_ticket=`: RFB, which
// the client runs, handing this page what to draw). Hermes passes this
// page's pointer and keys to the desktop only while it holds the lease.
//
// The pane says how it stands for its page and its tests: `data-access`
// (`admitted`, `starting`, `refused`), `data-size` (the desktop's, `WxH`),
// `data-frames` (raw rectangles drawn), and `data-lease` (`agent`, `mine`,
// `someone`).

/** The platform's computer client, served on every fragment host. */
const CLIENT = "/__computer/client.js";
/** An admission is renewed this long before its end. */
const MARGIN_S = 60;
/** A Hermes being made is asked again this soon. */
const STARTING_MS = 3000;
/** Waits before connecting again after a failure, the last repeated. */
const RETRY_MS = [500, 1000, 2000, 5000, 10000, 20000];
/** How long Hermes' socket may take to say it is ready. */
const READY_MS = 15000;
/** How long one call on Hermes' socket may take (`display.start` waits for
 * its desktop up to 15 s). */
const CALL_MS = 30000;
/** The wheel's buttons, as RFB counts them. */
const WHEEL_UP = 8;
const WHEEL_DOWN = 16;

const message = (e) => e?.message ?? String(e);
let loaded = null;
const client = () => (loaded ??= import(CLIENT).catch((e) => {
  loaded = null;
  throw e;
}));

export function mountScreen(root, { access = "./__hermes/access" } = {}) {
  root.classList.add("fragment-screen");
  root.hidden = true;
  root.innerHTML = `
    <header class="screen-bar">
      <span class="screen-title">Screen</span>
      <span class="screen-state" id="screen-state"></span>
      <button class="screen-control" id="screen-control" type="button" hidden></button>
    </header>
    <div class="screen-frame"><canvas id="screen-canvas" tabindex="0" aria-label="The computer's screen"></canvas></div>`;
  const stateLine = root.querySelector("#screen-state");
  const control = root.querySelector("#screen-control");
  const canvas = root.querySelector("#screen-canvas");
  const ctx = canvas.getContext("2d");

  const s = {
    peer: null,
    computer: null,
    gateway: null,
    screen: null,
    viewer: null,
    lease: null,
    // the viewer hash Hermes gave this page's lease (known once it took one)
    mine: null,
    renewal: null,
    failures: 0,
    stopped: false,
    buttons: 0,
  };
  const holding = () => s.lease?.holder === "human" && s.mine !== null && s.lease.viewer_hash === s.mine;

  function show(text) {
    stateLine.textContent = text;
  }

  function render() {
    const held = holding();
    canvas.classList.toggle("controlling", held);
    root.dataset.lease = held ? "mine" : s.lease?.holder === "human" ? "someone" : s.lease ? "agent" : "";
    control.hidden = !s.screen;
    control.textContent = held ? "Give back" : "Take over";
    show(!s.screen ? stateLine.textContent : held ? "You have control" : s.lease?.holder === "human" ? "Someone has control" : "Hermes is driving");
  }

  function drop() {
    clearTimeout(s.renewal);
    const { screen, gateway, computer } = s;
    Object.assign(s, { screen: null, gateway: null, computer: null, viewer: null, lease: null, mine: null });
    screen?.close().catch(() => {});
    gateway?.close();
    computer?.close();
    render();
  }

  function again(why) {
    drop();
    if (s.stopped) return;
    show(why);
    const wait = RETRY_MS[Math.min(s.failures, RETRY_MS.length - 1)];
    s.failures += 1;
    setTimeout(start, wait);
  }

  async function admitted() {
    const { Peer } = await client();
    s.peer ??= new Peer();
    const r = await fetch(access, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ peer: s.peer.id() }), credentials: "same-origin", cache: "no-store" });
    const body = await r.json().catch(() => ({}));
    if (r.status === 409) return { starting: true };
    // not the owner's page, signed out, or no Hermes here: no screen
    if ([401, 403, 404].includes(r.status)) return { refused: true };
    if (!r.ok) throw new Error(body.message || `the platform answered ${r.status}`);
    const strings = ["endpoint", "relay", "admission", "host", "token"];
    if (strings.some((k) => typeof body[k] !== "string") || typeof body.expiresAt !== "number") throw new Error("an access this page does not read");
    return body;
  }

  function renew(a) {
    clearTimeout(s.renewal);
    const due = Math.max(1, a.expiresAt - MARGIN_S - Date.now() / 1000);
    const computer = s.computer;
    s.renewal = setTimeout(async () => {
      if (s.computer !== computer) return;
      try {
        const next = await admitted();
        if (next.endpoint !== a.endpoint) return again("Reconnecting…");
        next.expiresAt = await computer.admit(next.admission);
        renew(next);
      } catch {
        // the connection ends with its admission, and is made again
      }
    }, due * 1000);
  }

  async function start() {
    if (s.stopped || s.computer) return;
    let a;
    try {
      a = await admitted();
    } catch (e) {
      root.hidden = false;
      return again(`The screen is not reachable: ${message(e)}`);
    }
    root.dataset.access = a.refused ? "refused" : a.starting ? "starting" : "admitted";
    if (a.refused) {
      root.hidden = true;
      return;
    }
    root.hidden = false;
    if (a.starting) {
      show("Hermes is starting…");
      setTimeout(start, STARTING_MS);
      return;
    }
    show("Connecting…");
    try {
      s.computer = await s.peer.connect(a.endpoint, a.relay, a.admission, a.host);
      renew(a);
      s.gateway = await Gateway.open(await s.computer.websocket(`/api/ws?token=${encodeURIComponent(a.token)}`, []));
      s.gateway.onEvent((e) => {
        if (e.type !== "display.lease" || !e.payload?.lease) return;
        s.lease = e.payload.lease;
        render();
      });
      const gateway = s.gateway;
      gateway.closed.then(() => s.gateway === gateway && again("Reconnecting…"));
      const status = await s.gateway.request("display.status");
      if (!status?.running) {
        show("Starting the screen…");
        await s.gateway.request("display.start");
      }
      await observe();
      s.failures = 0;
    } catch (e) {
      again(`The screen is not reachable: ${message(e)}`);
    }
  }

  /** A ticket, and the screen opened with it. */
  async function observe() {
    const o = await s.gateway.request("display.observe", s.viewer ? { viewer_id: s.viewer } : {});
    if (typeof o?.ticket !== "string" || typeof o?.path !== "string" || typeof o?.viewer_id !== "string") throw new Error("an answer this page does not read");
    s.viewer = o.viewer_id;
    s.lease = o.lease ?? null;
    const path = `${o.path}?display_ticket=${encodeURIComponent(o.ticket)}`;
    const computer = s.computer;
    s.screen = await computer.screen(path, (e) => {
      if (s.computer !== computer) return;
      draw(e);
    });
    render();
  }

  function draw(e) {
    switch (e.kind) {
      case "size":
        canvas.width = e.width;
        canvas.height = e.height;
        root.dataset.size = `${e.width}x${e.height}`;
        break;
      case "raw":
        ctx.putImageData(new ImageData(e.rgba, e.width, e.height), e.x, e.y);
        root.dataset.frames = String(Number(root.dataset.frames || 0) + 1);
        break;
      case "copy":
        ctx.drawImage(canvas, e.fromX, e.fromY, e.width, e.height, e.x, e.y, e.width, e.height);
        break;
      case "closed":
        // another person took control (4000), the link dropped, or Hermes went
        s.screen = null;
        again("Reconnecting…");
        break;
    }
  }

  control.addEventListener("click", async () => {
    if (!s.gateway || !s.viewer) return;
    control.disabled = true;
    try {
      const held = holding();
      const r = await s.gateway.request(held ? "display.lease.release" : "display.lease.acquire", { viewer_id: s.viewer });
      s.lease = r?.lease ?? s.lease;
      s.mine = held ? null : s.lease?.viewer_hash ?? null;
      if (!held) canvas.focus();
    } catch (e) {
      show(`Hermes refused: ${message(e)}`);
    } finally {
      control.disabled = false;
      render();
    }
  });

  // the pointer and keys, while this page holds the screen; the desktop
  // fits its pane (object-fit: contain), centred
  const at = (e) => {
    const r = canvas.getBoundingClientRect();
    const scale = Math.min(r.width / canvas.width, r.height / canvas.height);
    const [ox, oy] = [(r.width - canvas.width * scale) / 2, (r.height - canvas.height * scale) / 2];
    const x = Math.floor((e.clientX - r.left - ox) / scale);
    const y = Math.floor((e.clientY - r.top - oy) / scale);
    return [Math.max(0, Math.min(canvas.width - 1, x)), Math.max(0, Math.min(canvas.height - 1, y))];
  };
  // the DOM's buttons (1 left, 2 right, 4 middle) as RFB's (1 left, 2 middle, 4 right)
  const mask = (b) => (b & 1) | ((b & 4) >> 1) | ((b & 2) << 1);
  const pointer = (e) => {
    if (!holding() || !s.screen) return;
    const [x, y] = at(e);
    s.buttons = mask(e.buttons);
    s.screen.pointer(x, y, s.buttons).catch(() => {});
  };
  for (const kind of ["pointerdown", "pointerup", "pointermove"]) canvas.addEventListener(kind, pointer);
  canvas.addEventListener("pointerdown", () => canvas.focus());
  canvas.addEventListener("contextmenu", (e) => holding() && e.preventDefault());
  canvas.addEventListener("wheel", (e) => {
    if (!holding() || !s.screen) return;
    e.preventDefault();
    const [x, y] = at(e);
    const wheel = e.deltaY < 0 ? WHEEL_UP : WHEEL_DOWN;
    s.screen.pointer(x, y, s.buttons | wheel).then(() => s.screen?.pointer(x, y, s.buttons)).catch(() => {});
  }, { passive: false });
  const key = (down) => (e) => {
    if (!holding() || !s.screen) return;
    e.preventDefault();
    s.screen.key(e.key, down).catch(() => {});
  };
  canvas.addEventListener("keydown", key(true));
  canvas.addEventListener("keyup", key(false));

  // leaving while holding gives the screen back (a clean close does too)
  addEventListener("pagehide", () => {
    if (holding()) s.gateway?.request("display.lease.release", { viewer_id: s.viewer }).catch(() => {});
    s.stopped = true;
    drop();
  });

  start();
  return {
    close() {
      s.stopped = true;
      drop();
    },
  };
}

/** Hermes' JSON-RPC over its socket (the client's `Socket`): `request`
 * answers a method's result, `onEvent` hears its events (`{type,
 * payload}`), and `closed` settles once it ends. */
class Gateway {
  static open(socket) {
    return new Promise((resolve, reject) => {
      const g = new Gateway(socket);
      const timer = setTimeout(() => {
        g.close();
        reject(new Error("Hermes' socket did not say it was ready"));
      }, READY_MS);
      const off = g.onEvent((e) => {
        if (e.type !== "gateway.ready") return;
        clearTimeout(timer);
        off();
        resolve(g);
      });
      g.closed.then(() => {
        clearTimeout(timer);
        reject(new Error("Hermes' socket closed"));
      });
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
      for (const { reject } of this.waiting.values()) reject(new Error("Hermes' socket closed"));
      this.waiting.clear();
    });
  }

  async read() {
    // bounded by the socket: next() answers undefined once it ends
    for (;;) {
      let text;
      try {
        text = await this.socket.next();
      } catch {
        return;
      }
      if (text === undefined) return;
      if (typeof text !== "string") continue;
      let v;
      try {
        v = JSON.parse(text);
      } catch {
        continue;
      }
      if (v.method === "event" && v.params) {
        for (const f of this.listeners) f(v.params);
        continue;
      }
      const w = this.waiting.get(v.id);
      if (!w) continue;
      this.waiting.delete(v.id);
      clearTimeout(w.timer);
      if (v.error) w.reject(new Error(v.error.message || "Hermes refused it"));
      else w.resolve(v.result);
    }
  }

  onEvent(f) {
    this.listeners.add(f);
    return () => this.listeners.delete(f);
  }

  request(method, params = {}) {
    if (!this.open) return Promise.reject(new Error("Hermes' socket is closed"));
    const id = this.next++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting.delete(id);
        reject(new Error(`Hermes did not answer ${method}`));
      }, CALL_MS);
      this.waiting.set(id, { resolve, reject, timer });
      this.socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params })).catch((e) => {
        clearTimeout(timer);
        this.waiting.delete(id);
        reject(new Error(`Hermes' socket did not take it: ${message(e)}`));
      });
    });
  }

  close() {
    this.socket.close().catch(() => {});
  }
}
