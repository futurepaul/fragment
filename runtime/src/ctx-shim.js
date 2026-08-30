// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
const CTX_SHIM_SOURCE = `
export async function makeCtx(env) {
  const base = env.FRAGMENT_INTERNAL_URL;
  const tok = env.FRAGMENT_RUN_TOKEN;
  const hsec = env.FRAGMENT_HOST_SECRET || "";
  const scope = env.FRAGMENT_SCOPE || "run";
  let cause = null;
  try { cause = env.FRAGMENT_CAUSE ? JSON.parse(env.FRAGMENT_CAUSE) : null; } catch {}
  const call = async (path, opts = {}) => {
    const headers = Object.assign({}, opts.headers || {}, { "x-fragment-token": tok });
    if (hsec) headers["x-fragment-host-secret"] = hsec;
    const r = await fetch(base + path, Object.assign({}, opts, { headers }));
    if (!r.ok) throw new Error("fragment ctx " + path + " -> " + r.status + ": " + (await r.text()));
    return r;
  };
  // engines without AbortSignal.any get a tiny bridge: abort the combined
  // controller the moment either source signal fires
  const abortOnEither = (a, b) => {
    const c = new AbortController();
    const f = (e) => c.abort(e.target.reason);
    if (a.aborted) c.abort(a.reason); else a.addEventListener("abort", f, { once: true });
    if (b.aborted) c.abort(b.reason); else b.addEventListener("abort", f, { once: true });
    return c.signal;
  };
  // Secrets: workflow runs (scope "run") need them synchronously, so they
  // await the fetch. Served apps (scope "draft") almost never touch secrets,
  // and every request through the app used to pay a loopback for them \u2014 so
  // for draft scope the fetch runs in the background and fills the object
  // in when it lands. Apps that DO read secrets should await
  // ctx.secrets.ready first (found live: an agent-built app read the
  // racing object and called it "unreliable").
  const secretsAll = {};
  if (scope === "run" || env.FRAGMENT_EAGER_SECRETS) {
    // runs always need secrets synchronously; served apps get them
    // eagerly when the manifest declares any (the one-loopback cost is
    // opt-in by declaring \u2014 lazy fill left first-render reads empty)
    Object.assign(secretsAll, await call("/secrets/all").then((r) => r.json()).catch(() => ({})));
  } else {
    call("/secrets/all").then((r) => r.json()).then((s) => Object.assign(secretsAll, s)).catch(() => {});
  }
  const ctx = {
    // plain fetch, plus the cause chain: inside a run, every outbound
    // request carries the run's hop budget so receiving fragments can
    // refuse cycles before author code runs.
    http: (url, init) => {
      // default 30s timeout: a hung outbound fetch used to eat the run's
      // full 300s host budget before failing. A caller-provided signal
      // races it (whichever aborts first wins), so apps keep full control.
      const timeout = AbortSignal.timeout(30000);
      const own = init && init.signal;
      const signal = !own ? timeout
        : typeof AbortSignal.any === "function" ? AbortSignal.any([own, timeout])
        : abortOnEither(own, timeout);
      if (!cause) return fetch(url, Object.assign({}, init, { signal }));
      const headers = new Headers((init && init.headers) || {});
      headers.set("x-fragment-hops", String((cause.depth || 0) + 1));
      headers.set("x-fragment-cause", String(cause.origin || "fragment"));
      return fetch(url, Object.assign({}, init, { headers, signal }));
    },
    files: {
      async read(path) {
        const r = await call("/files/read?path=" + encodeURIComponent(path));
        return r.text();
      },
      async readBytes(path) {
        const r = await call("/files/read?path=" + encodeURIComponent(path));
        return r.arrayBuffer();
      },
      // returns {ok, deduped, rev}: writing identical content is a no-op.
      // opts.ifRev pins the write to a row state from ctx.files.stat() \u2014
      // a moved row rejects with Error.conflict (and .currentRev) instead
      // of clobbering: read-modify-write loops stay safe across slow awaits
      async write(path, data, opts) {
        let body = typeof data === "string" ? data : data;
        const headers = { "x-fragment-token": tok, ...(hsec ? { "x-fragment-host-secret": hsec } : {}) };
        // blob-first for big bodies: over the 64KiB inline carve-out, hash
        // client-side and send with x-fragment-hash so the cell streams the
        // bytes through to the tier instead of refusing them (the SDK-style
        // generators return whole Uint8Arrays \u2014 images and clips are MBs)
        if (typeof body !== "string" || body.length > 65536) {
          const bytes = typeof body === "string" ? new TextEncoder().encode(body) : body instanceof ArrayBuffer ? new Uint8Array(body) : body;
          if (bytes.byteLength > 65536) {
            const digest = await crypto.subtle.digest("SHA-256", bytes);
            headers["x-fragment-hash"] = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
            body = bytes;
          }
        }
        let qs = "/files/write?path=" + encodeURIComponent(path);
        if (opts && Number.isInteger(opts.ifRev)) qs += "&if_rev=" + opts.ifRev;
        const r = await fetch(base + qs, { method: "PUT", body, headers });
        if (r.status === 409) {
          const j = await r.json().catch(() => ({}));
          const e = new Error("rev conflict on " + path + ": row is at rev " + (j.currentRev ?? "?") + (j.ifRev !== undefined ? ", write pinned to " + j.ifRev : ""));
          e.conflict = true;
          e.currentRev = j.currentRev;
          throw e;
        }
        if (!r.ok) throw new Error("fragment ctx files/write -> " + r.status + ": " + (await r.text()));
        return r.json();
      },
      // place remote bytes as a file: streams the URL into the blob tier
      // and commits the row (dedup + append-only gates as usual). The
      // public-CDN half of generated-media placement; equally useful for
      // "archive this URL into my folder".
      async ingest(url, path) {
        const r = await call("/files/ingest", { method: "POST", body: JSON.stringify({ url: String(url), path }) });
        return (await r.json()).file;
      },
      // live row metadata INCLUDING tombstones ({deleted:true} with the
      // tombstone's rev); null when the path has no history at all
      async stat(path) {
        const r = await call("/files/stat?path=" + encodeURIComponent(path));
        return (await r.json()).stat;
      },
      async list(prefix = "") {
        const r = await call("/files/list?prefix=" + encodeURIComponent(prefix));
        return (await r.json()).paths;
      },
      // like list(), but returns [{path,size,updatedAt,rev}] (and only ever
      // sees the working copy's live metadata \u2014 the default, unless frozen)
      async index(prefix = "") {
        const r = await call("/files/list?prefix=" + encodeURIComponent(prefix));
        return (await r.json()).files;
      },
    },
    secrets: secretsAll,
    scope,
    // inbox() returns pending messages; ack(ids) marks what you observed
    // as done. Skipping the ack means you will see them again next run \u2014
    // never ack messages you didn't actually handle.
    async inbox() {
      const r = await call("/inbox/pending");
      return (await r.json()).messages;
    },
    async inboxAck(ids) {
      await call("/inbox/ack", { method: "POST", body: JSON.stringify({ ids: ids || [] }) });
    },
    events: {
      async append(kind, data) {
        await call("/events", { method: "POST", body: JSON.stringify({ kind, summary: String(kind), data }) });
      },
    },
    log(msg) {
      return call("/log", { method: "POST", body: JSON.stringify({ msg: String(msg) }) }).catch(() => {});
    },
    state: {
      async get(k) {
        const r = await call("/wstate?k=" + encodeURIComponent(k));
        return (await r.json()).v;
      },
      async put(k, v) {
        await call("/wstate?k=" + encodeURIComponent(k), { method: "PUT", body: JSON.stringify({ v }) });
      },
    },
    rooms: {
      async getState(room) {
        const r = await call("/rooms/state?room=" + encodeURIComponent(room));
        return (await r.json()).state;
      },
      async setState(room, value) {
        await call("/rooms/state", { method: "POST", body: JSON.stringify({ value: value ?? null }) });
      },
    },
    // Web Push \u2014 closed-tab notifications. Sends payload {title (required,
    // <= 80 chars), body (<= 200), url? (<= 500), tag? (collapse key)} to
    // EVERY stored browser subscription for who. Subscriptions are
    // platform-managed (the site's sw-client registers/drops them via the
    // cell's push_subs table): a 404/410 from the push service drops the
    // subscription permanently; any other failure bumps a counter and the
    // subscription is dropped after 5. Budget: 20 endpoint POSTs per call.
    // Returns {sent, dropped, detail}. No subscriptions is NOT an error \u2014
    // you get {sent: 0, dropped: 0, detail: "no subscriptions for ..."}.
    async push(who, payload) {
      const r = await call("/push/send", { method: "POST", body: JSON.stringify({ who: String(who ?? ""), payload: payload || {} }) });
      return r.json();
    },
  };
  return ctx;
}
`;
export {
  CTX_SHIM_SOURCE
};
