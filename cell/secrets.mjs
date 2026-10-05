// The deployment's own secrets where the runtime has no Secrets Store
// (docs/self-host.md, seam 12). On Cloudflare, and under `wrangler dev`,
// each secret is a Secrets Store binding, and the cell reads it as
// `await env.<BINDING>.get()` (cell/src/keys.rs, through workers-rs's
// `SecretStore`; agent/src/keys.rs the same). celld has no such binding, so
// here each binding the deployment names is given a stand-in with the same
// face: an object whose async `get()` answers the value, or null. The Rust
// never knows which it read.
//
// `FRAGMENT_SECRETS`, a Worker variable, names them, and where they are:
//
//   {"backend": "vars",
//    "secrets": [{"binding": "HOST_SECRET", "secret_name": "fragment-host-secret"}, …]}
//
// the same pairs a deploy writes into `secrets_store_secrets`. A backend
// answers one secret at a time; `BACKENDS` is the one place another slots in
// (next: OpenBao, Vault's KV v2 API, a `secret_name` its key), and nothing
// else changes, the cell's Rust least of all. Unset, as on Cloudflare,
// `withSecrets` hands the env on as it came.
//
// A backend's answers are read again at every `get()`: keys.rs keeps each
// for a minute (`secrets_store::CACHE_MS_MAX`), as it keeps the store's.

// The secrets one Worker is bound to, at most: the cell's bindings
// (`secrets_store::CACHE_ENTRIES_MAX` is under it) with room to spare.
const SECRETS_MAX = 64;
// A binding is an identifier, as a Worker binding's name is; a secret's
// name is the store's (devstack's `store::valid_name`).
const BINDING = /^[A-Z][A-Z0-9_]{0,63}$/;
const SECRET_NAME = /^[A-Za-z0-9_-]{1,255}$/;

// Each backend: given the env as the runtime handed it, a reader of one
// secret (`{binding, secret_name}`) that answers its value, or null.
const BACKENDS = {
  // The value is the Worker variable named as its binding (celld dev's
  // `.dev.vars`, devstack's `Secrets::Shim`), which its stand-in shadows.
  vars: (env) => (secret) => {
    const value = env[secret.binding];
    return typeof value === "string" ? value : null;
  },
};

// Cloudflare's Secrets Store binding is a `Fetcher` whose RPC `get()`
// answers the secret, and workers-rs takes a binding for one only when its
// class is named so (`Env::secret_store`, `EnvBinding::TYPE_NAME`): so is
// this stand-in.
class Fetcher {
  #read;
  constructor(read) {
    this.#read = read;
  }
  async get() {
    const value = await this.#read();
    return typeof value === "string" && value !== "" ? value : null;
  }
}

function config(text) {
  let parsed;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    throw new Error(`FRAGMENT_SECRETS is JSON: ${e.message}`);
  }
  const { backend, secrets } = parsed || {};
  if (!Object.hasOwn(BACKENDS, backend)) throw new Error(`FRAGMENT_SECRETS' backend is one of ${Object.keys(BACKENDS).join(", ")}, not ${JSON.stringify(backend)}`);
  if (!Array.isArray(secrets) || secrets.length > SECRETS_MAX) throw new Error(`FRAGMENT_SECRETS' secrets are a list of at most ${SECRETS_MAX}`);
  const seen = new Set();
  for (const s of secrets) {
    if (!s || !BINDING.test(s.binding) || !SECRET_NAME.test(s.secret_name)) throw new Error(`FRAGMENT_SECRETS names each secret's binding and its name: ${JSON.stringify(s)}`);
    if (seen.has(s.binding)) throw new Error(`FRAGMENT_SECRETS binds ${s.binding} twice`);
    seen.add(s.binding);
  }
  return { backend, secrets };
}

// Each env the runtime hands over, as the Rust is given it: one per env.
const given = new WeakMap();

// `env` with a stand-in for each secret `FRAGMENT_SECRETS` names, every
// other binding as it was (the stand-ins are its own properties; the rest
// its prototype's). Without `FRAGMENT_SECRETS`, `env` itself. A config that
// is wrong throws, at the first request.
export function withSecrets(env) {
  const text = env && env.FRAGMENT_SECRETS;
  if (typeof text !== "string" || text === "") return env;
  const kept = given.get(env);
  if (kept) return kept;
  const { backend, secrets } = config(text);
  const read = BACKENDS[backend](env);
  const out = Object.create(env);
  for (const secret of secrets) {
    Object.defineProperty(out, secret.binding, { value: new Fetcher(() => read(secret)), enumerable: true });
  }
  given.set(env, out);
  return out;
}
