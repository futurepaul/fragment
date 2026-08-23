// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
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
  // Secrets: workflow runs (scope "run") need them synchronously, so they
  // await the fetch. Served apps (scope "draft") almost never touch secrets,
  // and every request through the app used to pay a loopback for them \u2014 so
  // for draft scope the fetch runs in the background and fills the object
  // in when it lands. Apps that DO read secrets should await
  // ctx.secrets.ready first (found live: an agent-built app read the
  // racing object and called it "unreliable").
  const secretsAll = {};
  if (scope === "run") {
    Object.assign(secretsAll, await call("/secrets/all").then((r) => r.json()).catch(() => ({})));
  } else {
    // served apps: fetched in the background \u2014 a value may be absent for
    // the first moments after an isolate spins up. Apps that need a
    // secret at render time should read it from a workflow's output
    // (files/state), not from ctx.secrets.
    call("/secrets/all").then((r) => r.json()).then((s) => Object.assign(secretsAll, s)).catch(() => {});
  }
  const ctx = {
    // plain fetch, plus the cause chain: inside a run, every outbound
    // request carries the run's hop budget so receiving fragments can
    // refuse cycles before author code runs.
    http: (url, init) => {
      if (!cause) return fetch(url, init);
      const headers = new Headers((init && init.headers) || {});
      headers.set("x-fragment-hops", String((cause.depth || 0) + 1));
      headers.set("x-fragment-cause", String(cause.origin || "fragment"));
      return fetch(url, Object.assign({}, init, { headers }));
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
      // returns {ok, deduped, rev}: writing identical content is a no-op
      async write(path, data) {
        const body = typeof data === "string" ? data : data;
        const r = await call("/files/write?path=" + encodeURIComponent(path), { method: "PUT", body });
        return r.json();
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
    async ai(prompt, opts = {}) {
      const r = await call("/infer", { method: "POST", body: JSON.stringify({ prompt: String(prompt), model: opts.model }) });
      return (await r.json()).text;
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
        await call("/rooms/state?room=" + encodeURIComponent(room), { method: "POST", body: JSON.stringify({ value }) });
      },
    },
  };
  return ctx;
}
`;
export {
  CTX_SHIM_SOURCE
};
