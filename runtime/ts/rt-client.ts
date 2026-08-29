// The __rt.js browser client served per fragment. Usage:
//   <script src="/f/<name>/__rt.js"></script>  (or the /d/<slug>/ equivalent)
//   const notes = fragment.room("notes");
//   notes.on("msg", (m) => ...); notes.send({...}); notes.setState({...});
//
// It also carries the platform notify/push surface:
//   fragment.notify.supported()                     → bool (Notification present)
//   fragment.notify.permission()                    → "granted" | "denied" | "default" | "unsupported"
//   fragment.notify.ask()                           → Promise<permission string> — user gesture ONLY
//   fragment.notify.show(title, { body, tag, url }) → fired? (granted + hidden tab only)
//   fragment.push.register(who)                     → Promise<{ok, reason?}>
//   fragment.push.unregister(who)                   → Promise<{ok, reason?}>
// Every entry point is defensive: in an embedded webview with no
// Notification or serviceWorker at all, nothing here throws — calls return
// false, "unsupported", or {ok:false, reason:"unsupported"} instead.
export const RT_CLIENT_SOURCE = `
window.fragment = (() => {
  function prefix() {
    // Derive the prefix from this script's own URL — it is always served
    // at <prefix>/__rt.js — so pages work at /f/<name>/, /d/<slug>/, AND at
    // a bare canonical subdomain root (where location.pathname has no
    // prefix at all). ?view= tokens propagate to the socket.
    // Memoized on first call (warmed at load below, while
    // document.currentScript still points at this script): calls from
    // later event handlers would otherwise see currentScript === null and
    // fall to the pathname heuristic, which is only a fallback.
    if (prefix._p !== undefined) return prefix._p;
    let p = "";
    try {
      const src = document.currentScript && document.currentScript.src;
      if (src) {
        // String ops, not a regex literal: this source lives inside a
        // template literal, where escaped slash and dot looked correct in
        // TS but shipped as unescaped / and . — a SyntaxError on every
        // page that loaded __rt.js as a classic script, degrading rooms
        // to solo.
        const path = new URL(src, location.href).pathname;
        const suffix = "/__rt.js";
        if (path.endsWith(suffix)) p = path.slice(0, -suffix.length);
      }
    } catch {}
    if (!p) {
      const parts = location.pathname.split("/").filter(Boolean);
      if (parts[0] === "f" || parts[0] === "d") p = "/" + parts[0] + "/" + parts[1];
    }
    prefix._p = p;
    return p;
  }
  function base() {
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    const view = new URLSearchParams(location.search).get("view");
    return { url: (room) => proto + "//" + location.host + prefix() + "/__room/" + encodeURIComponent(room) + (view ? "?view=" + view : "") };
  }
  function room(name) {
    const listeners = {};
    const r = {
      state: undefined, presence: [], history: [], ready: false,
      _ws: null, _queue: [], _presence: undefined,
      on(type, fn) { (listeners[type] ||= []).push(fn); return r; },
      _emit(type, data) { (listeners[type] || []).forEach((fn) => { try { fn(data); } catch (e) { console.error(e); } }); },
      _send(obj) {
        const s = JSON.stringify(obj);
        if (r._ws && r._ws.readyState === 1) r._ws.send(s); else r._queue.push(s);
      },
      send(data) { r._send({ type: "msg", data }); },
      setState(value) { r._send({ type: "state:set", value }); },
      setPresence(data) { r._presence = data; r._send({ type: "presence", data }); },
      close() { r._ws && r._ws.close(); },
      connect() {
        const ws = new WebSocket(base().url(name));
        r._ws = ws;
        // onopen: flush queued messages, then re-assert presence so names
        // survive reconnects (the server drops presence on socket close)
        ws.onopen = () => {
          r._queue.splice(0).forEach((m) => ws.send(m));
          if (r._presence !== undefined) r._send({ type: "presence", data: r._presence });
        };
        ws.onmessage = (ev) => {
          let m; try { m = JSON.parse(ev.data); } catch { return; }
          if (m.type === "hello") {
            r.state = m.state; r.presence = m.presence; r.history = m.history; r.ready = true;
            r._emit("hello", m);
          } else if (m.type === "state") { r.state = m.value; r._emit("state", m.value); }
          else if (m.type === "presence") { r.presence = m.list; r._emit("presence", m.list); }
          else if (m.type === "msg") { r._emit("msg", m); }
        };
        ws.onclose = () => { r.ready = false; r._emit("close"); setTimeout(() => r.connect(), 1500); };
        return r;
      },
    };
    return r.connect();
  }

  // ---- notify: thin, defensive Notification wrappers ----
  // ask() is the ONLY prompting entry point, and the lib never calls it on
  // its own — not at load, not from a timer, not from a poll. Browsers
  // only show the permission prompt on user activation, so apps must call
  // ask() from inside an explicit click handler. That rule is the app's
  // to keep, and this comment is the contract.
  function notifySupported() {
    try { return typeof Notification !== "undefined"; } catch (e) { return false; }
  }
  function notifyPermission() {
    if (!notifySupported()) return "unsupported";
    try { return Notification.permission; } catch (e) { return "unsupported"; }
  }
  function notifyAsk() {
    return new Promise((resolve) => {
      if (!notifySupported()) return resolve("unsupported");
      let done = false;
      const settle = (p) => { if (!done) { done = true; clearTimeout(t); resolve(p); } };
      // timeout guard: a webview that shows no prompt and never calls
      // back would otherwise leave the caller's await pending forever
      const t = setTimeout(() => settle("default"), 60000);
      try {
        const r = Notification.requestPermission((p) => settle(p)); // callback form: old Safari
        if (r && typeof r.then === "function") r.then((p) => settle(p), () => settle("denied"));
      } catch (e) { settle("denied"); }
    });
  }
  // show(): fire one local notification. Only fires when permission is
  // granted AND the document is hidden — a visible tab already has the
  // user's eyes. onclick focuses the window, closes the notification and
  // navigates to opts.url when given. Returns whether it actually fired.
  // No persistence, no dedupe: apps own seen-sets and once-only rules.
  function notifyShow(title, opts) {
    opts = opts || {};
    try {
      if (!notifySupported() || Notification.permission !== "granted") return false;
      if (!document.hidden) return false;
      const n = new Notification(title == null ? "" : String(title), {
        body: opts.body == null ? undefined : String(opts.body),
        tag: opts.tag == null ? undefined : String(opts.tag),
      });
      const url = opts.url == null ? null : String(opts.url);
      n.onclick = () => {
        try {
          window.focus();
          n.close();
          if (url) location.assign(url);
        } catch (e) {}
      };
      return true;
    } catch (e) { return false; }
  }

  // ---- push: Web Push against the fragment's own reserved routes
  // (<prefix>/__sw.js, __push-key, __push-sub, __push-unsub — served by
  // the host right next to __rt.js). Subscription keys travel as base64url
  // of the raw bytes, the encoding every push service and webpush library
  // agrees on. Like the notify half, every failure mode resolves to
  // {ok:false, reason} — never a rejected promise, never a throw in a
  // webview without serviceWorker/PushManager.
  function b64uToBytes(s) {
    let b = String(s || "").split("-").join("+").split("_").join("/");
    while (b.length % 4) b += "=";
    const bin = atob(b);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  function bytesToB64u(buf) {
    const u8 = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
    let s = "";
    for (let i = 0; i < u8.length; i++) s += String.fromCharCode(u8[i]);
    return btoa(s).split("+").join("-").split("/").join("_").split("=").join("");
  }
  async function pushRegister(who) {
    let sw = null;
    try { sw = navigator.serviceWorker; } catch (e) {}
    if (!sw || !sw.register) return { ok: false, reason: "unsupported" };
    if (notifySupported() && Notification.permission === "denied") return { ok: false, reason: "denied" };
    try {
      // default scope = the script's own directory, i.e. this fragment's
      // corner of the host (on a canonical subdomain that IS "/")
      await sw.register(prefix() + "/__sw.js");
      const reg = await sw.ready;
      if (!reg || !reg.pushManager || typeof PushManager === "undefined") return { ok: false, reason: "unsupported" };
      const keyResp = await fetch(prefix() + "/__push-key");
      const keyJson = await keyResp.json().catch(() => ({}));
      if (!keyJson || !keyJson.key) return { ok: false, reason: "no-key" };
      const sub = await reg.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: b64uToBytes(keyJson.key), // base64url VAPID point to raw bytes
      });
      const resp = await fetch(prefix() + "/__push-sub", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          who: who,
          endpoint: sub.endpoint,
          p256dh: bytesToB64u(sub.getKey("p256dh")),
          auth: bytesToB64u(sub.getKey("auth")),
        }),
      });
      const j = await resp.json().catch(() => ({}));
      if (!resp.ok) {
        // the browser now holds a subscription the server can't store —
        // drop it so nothing dangles on the push service
        try { await sub.unsubscribe(); } catch (e) {}
        return { ok: false, reason: "server", error: (j && j.error) || ("http " + resp.status), status: resp.status };
      }
      return j; // the server's own verdict, verbatim
    } catch (e) {
      const name = (e && e.name) || "";
      if (name === "NotAllowedError" || name === "PermissionDeniedError") return { ok: false, reason: "denied" };
      return { ok: false, reason: "error", error: String((e && e.message) || e) };
    }
  }
  async function pushUnregister(who) {
    let sw = null;
    try { sw = navigator.serviceWorker; } catch (e) {}
    if (!sw || !sw.register) return { ok: false, reason: "unsupported" };
    let sub = null;
    try {
      const reg = await sw.ready;
      if (reg && reg.pushManager) sub = await reg.pushManager.getSubscription();
    } catch (e) {}
    try {
      const resp = await fetch(prefix() + "/__push-unsub", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ who: who }),
      });
      const j = await resp.json().catch(() => ({}));
      if (!resp.ok) return { ok: false, reason: "server", error: (j && j.error) || ("http " + resp.status), status: resp.status };
      return Object.assign({ unsubscribed: !!sub }, j);
    } catch (e) {
      return { ok: false, reason: "error", error: String((e && e.message) || e) };
    } finally {
      // unregister means unregister: the local subscription dies even when
      // the server leg hiccuped (a stale server row is inert once the
      // push service starts 410-ing it)
      if (sub) { try { await sub.unsubscribe(); } catch (e) {} }
    }
  }

  try { prefix(); } catch (e) {} // warm the memo while currentScript still points here
  return {
    room,
    notify: { supported: notifySupported, permission: notifyPermission, ask: notifyAsk, show: notifyShow },
    push: { register: pushRegister, unregister: pushUnregister },
  };
})();
`;
