// The fragment-ctx.mjs module source, injected as a sibling module into every
// loaded worker (workflow runs, app.mjs, rooms.mjs). ctx talks back to the
// cell over the loopback internal API with a per-run token.
export const CTX_SHIM_SOURCE = `
export async function makeCtx(env) {
  const base = env.FRAGMENT_INTERNAL_URL;
  const tok = env.FRAGMENT_RUN_TOKEN;
  const scope = env.FRAGMENT_SCOPE || "run";
  const call = async (path, opts = {}) => {
    const headers = Object.assign({}, opts.headers || {}, { "x-fragment-token": tok });
    const r = await fetch(base + path, Object.assign({}, opts, { headers }));
    if (!r.ok) throw new Error("fragment ctx " + path + " -> " + r.status + ": " + (await r.text()));
    return r;
  };
  // Secrets: workflow runs (scope "run") need them synchronously, so they
  // await the fetch. Served apps (scope "draft") almost never touch secrets,
  // and every request through the app used to pay a loopback for them — so
  // for draft scope the fetch runs in the background and fills the object
  // in when it lands.
  const secretsAll = {};
  if (scope === "run") {
    Object.assign(secretsAll, await call("/secrets/all").then((r) => r.json()).catch(() => ({})));
  } else {
    call("/secrets/all").then((r) => r.json()).then((s) => Object.assign(secretsAll, s)).catch(() => {});
  }
  const ctx = {
    http: (url, init) => fetch(url, init),
    files: {
      async read(path) {
        const r = await call("/files/read?path=" + encodeURIComponent(path));
        return r.text();
      },
      async readBytes(path) {
        const r = await call("/files/read?path=" + encodeURIComponent(path));
        return r.arrayBuffer();
      },
      async write(path, data) {
        const body = typeof data === "string" ? data : data;
        await call("/files/write?path=" + encodeURIComponent(path), { method: "PUT", body });
      },
      async list(prefix = "") {
        const r = await call("/files/list?prefix=" + encodeURIComponent(prefix));
        return (await r.json()).paths;
      },
      // like list(), but returns [{path,size,updatedAt,rev}] (and only ever
      // sees the working copy's live metadata on liveFiles fragments)
      async index(prefix = "") {
        const r = await call("/files/list?prefix=" + encodeURIComponent(prefix));
        return (await r.json()).files;
      },
    },
    secrets: secretsAll,
    scope,
    async inbox() {
      const r = await call("/inbox/pending");
      return (await r.json()).messages;
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
