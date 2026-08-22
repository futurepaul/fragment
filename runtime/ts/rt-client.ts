// The __rt.js browser client served per fragment. Usage:
//   <script src="/f/<name>/__rt.js"></script>  (or the /d/<slug>/ equivalent)
//   const notes = fragment.room("notes");
//   notes.on("msg", (m) => ...); notes.send({...}); notes.setState({...});
export const RT_CLIENT_SOURCE = `
window.fragment = (() => {
  function base() {
    // Derive the prefix from this script's own URL — it is always served
    // at <prefix>/__rt.js — so pages work at /f/<name>/, /d/<slug>/, AND at
    // a bare canonical subdomain root (where location.pathname has no
    // prefix at all). ?view= tokens propagate to the socket.
    let prefix = "";
    try {
      const src = document.currentScript && document.currentScript.src;
      if (src) {
        const m = new URL(src, location.href).pathname.match(/^(.*)\/__rt\.js$/);
        if (m) prefix = m[1];
      }
    } catch {}
    if (!prefix) {
      const parts = location.pathname.split("/").filter(Boolean);
      if (parts[0] === "f" || parts[0] === "d") prefix = "/" + parts[0] + "/" + parts[1];
    }
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    const view = new URLSearchParams(location.search).get("view");
    return { url: (room) => proto + "//" + location.host + prefix + "/__room/" + encodeURIComponent(room) + (view ? "?view=" + view : "") };
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
  return { room };
})();
`;
