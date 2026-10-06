// The deployment's own secrets where the runtime has no Secrets Store
// (docs/self-host.md, seam 12). On Cloudflare, and under `wrangler dev`,
// each secret is a Secrets Store binding, and the cell reads it as
// `await env.<BINDING>.get()` (cell/src/keys.rs, through workers-rs's
// `SecretStore`; agent/src/keys.rs the same). celld has no such binding, so
// here each binding the deployment names is given a stand-in with the same
// face: an object whose async `get()` answers the value. The Rust never
// knows which it read.
//
// `FRAGMENT_SECRETS`, a Worker variable, names them, and where they are:
//
//   {"backend": "openbao", "addr": "http://127.0.0.1:8802", "mount": "fragment",
//    "secrets": [{"binding": "HOST_SECRET", "secret_name": "fragment-host-secret"}, …]}
//
// the same pairs a deploy writes into `secrets_store_secrets`. A backend
// answers one secret at a time; `BACKENDS` is the one place one slots in:
//
// - `openbao`: Vault's KV v2 API (OpenBao, or a company's HashiCorp Vault),
//   `GET <addr>/v1/<mount>/data/[<prefix>/]<secret_name>`, the value its
//   field `value`, read with the token in the Worker variable
//   `FRAGMENT_SECRETS_TOKEN` (and `X-Vault-Namespace` when `namespace` is
//   set). The token is the one secret given outside the service, and the
//   stand-ins hide it from the rest of the env.
// - `vars`: the Worker variable named as its binding (celld dev's
//   `.dev.vars`, devstack's `Secrets::Shim`).
//
// Unset, as on Cloudflare, `withSecrets` hands the env on as it came.
//
// A read that fails throws a `SecretsError` whose `code` says why
// (`missing`, `refused`, `sealed`, `down`, `failed`, `malformed`), and
// workers-rs shows it so: `SecretsError [sealed]: …`. Each read is bounded
// (`READ_MS_MAX`), so a service that is down or hangs is an error, never a
// hang. Nothing is cached here: keys.rs keeps each value read for a minute
// per isolate (`secrets_store::CACHE_MS_MAX`), as it keeps the store's, and
// a failure is never kept, so the next request reads again.

// The secrets one Worker is bound to, at most: the cell's bindings
// (`secrets_store::CACHE_ENTRIES_MAX` is under it) with room to spare.
const SECRETS_MAX = 64;
// A binding is an identifier, as a Worker binding's name is; a secret's
// name is the store's (devstack's `store::valid_name`).
const BINDING = /^[A-Z][A-Z0-9_]{0,63}$/;
const SECRET_NAME = /^[A-Za-z0-9_-]{1,255}$/;
// One read of the service, its answer read whole, at most.
const READ_MS_MAX = 5000;
// An answer, at most: one value (64 KiB, the store's own limit) and its
// metadata.
const ANSWER_BYTES_MAX = 256 * 1024;
// The Worker variable the service's token is in.
const TOKEN_VAR = "FRAGMENT_SECRETS_TOKEN";
const TOKEN = /^[\x21-\x7e]{1,1024}$/;
// The service's origin (no path: KV's is `/v1/…`), a mount, a prefix in it,
// a namespace: segments of letters, digits, `_`, `-` and `.`, never `..`.
const ADDR = /^https?:\/\/([A-Za-z0-9.-]+|\[[0-9A-Fa-f:]+\])(:[0-9]{1,5})?$/;
const SEGMENTS = /^[A-Za-z0-9_.-]+(\/[A-Za-z0-9_.-]+)*$/;
const PATH_BYTES_MAX = 128;
// The field a secret's value is in.
const VALUE_FIELD = "value";

// Why a secret could not be read; `code` is the kind.
export class SecretsError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = "SecretsError";
    this.code = kind;
  }
}

// The first of an answer's `errors`, for a message: bounded, one line.
function said(body) {
  const first = body && Array.isArray(body.errors) && typeof body.errors[0] === "string" ? body.errors[0].replace(/\s+/g, " ").trim() : "";
  return first ? `: ${first.slice(0, 200)}` : "";
}

// Whether `v` is a path of segments, within bounds.
function segments(v) {
  return typeof v === "string" && v.length <= PATH_BYTES_MAX && SEGMENTS.test(v) && !v.split("/").some((s) => s === "." || s === "..");
}

// Each backend: the keys of `FRAGMENT_SECRETS` it takes beyond `backend`
// and `secrets`, a check of them, and, given the env and its config, a
// reader of one secret (`{binding, secret_name}`) that answers its value.
const BACKENDS = {
  // The value is the Worker variable named as its binding, which its
  // stand-in shadows.
  vars: {
    keys: [],
    check: () => {},
    reader: (env) => (secret) => {
      const value = env[secret.binding];
      return typeof value === "string" ? value : null;
    },
  },
  openbao: {
    keys: ["addr", "mount", "prefix", "namespace"],
    check: (cfg) => {
      if (typeof cfg.addr !== "string" || !ADDR.test(cfg.addr)) throw new Error(`FRAGMENT_SECRETS' addr is the service's origin (https://vault.example:8200, no path), not ${JSON.stringify(cfg.addr)}`);
      if (!segments(cfg.mount)) throw new Error(`FRAGMENT_SECRETS' mount is a KV v2 mount's path, not ${JSON.stringify(cfg.mount)}`);
      for (const key of ["prefix", "namespace"]) {
        if (cfg[key] !== undefined && !segments(cfg[key])) throw new Error(`FRAGMENT_SECRETS' ${key}, when set, is a path, not ${JSON.stringify(cfg[key])}`);
      }
    },
    reader: (env, cfg) => {
      const token = env[TOKEN_VAR];
      if (typeof token !== "string" || !TOKEN.test(token)) throw new Error(`FRAGMENT_SECRETS' backend openbao reads its token from the Worker variable ${TOKEN_VAR}, which is ${token === undefined ? "not set" : "not a token"}`);
      const where = `OpenBao at ${cfg.addr}`;
      const under = cfg.prefix ? `${cfg.prefix}/` : "";
      return async (secret) => {
        const path = `${cfg.mount}/${under}${secret.secret_name}`;
        const headers = { "x-vault-token": token, accept: "application/json" };
        if (cfg.namespace) headers["x-vault-namespace"] = cfg.namespace;
        const abort = new AbortController();
        const timer = setTimeout(() => abort.abort(), READ_MS_MAX);
        let status, text;
        try {
          // a token never follows a redirect (a standby's points elsewhere)
          const response = await fetch(`${cfg.addr}/v1/${cfg.mount}/data/${under}${secret.secret_name}`, { headers, signal: abort.signal, redirect: "manual" });
          status = response.status;
          const length = Number(response.headers.get("content-length") || 0);
          if (length > ANSWER_BYTES_MAX) throw new SecretsError("failed", `${where} answered ${status} for ${path} with ${length} bytes; at most ${ANSWER_BYTES_MAX} are read`);
          text = await response.text();
        } catch (e) {
          if (e instanceof SecretsError) throw e;
          if (abort.signal.aborted) throw new SecretsError("down", `${where} did not answer for ${path} within ${READ_MS_MAX / 1000} s`);
          const cause = e && e.cause && e.cause.code ? ` ${e.cause.code}` : "";
          throw new SecretsError("down", `${where} is not answering (${e && e.message}${cause})`);
        } finally {
          clearTimeout(timer);
        }
        if (text.length > ANSWER_BYTES_MAX) throw new SecretsError("failed", `${where} answered ${status} for ${path} with over ${ANSWER_BYTES_MAX} bytes`);
        let body = null;
        try {
          body = JSON.parse(text);
        } catch {
          // a proxy's page, or nothing: said below by its status
        }
        switch (status) {
          case 200: {
            const value = body && body.data && body.data.data ? body.data.data[VALUE_FIELD] : undefined;
            if (typeof value !== "string" || value === "") throw new SecretsError("malformed", `${path} in ${where} has no "${VALUE_FIELD}" field: a secret's value is its "${VALUE_FIELD}" (bao kv put -mount=${cfg.mount} ${under}${secret.secret_name} ${VALUE_FIELD}=…)`);
            return value;
          }
          case 404:
            throw new SecretsError("missing", `${path} is not in ${where} (bao kv put -mount=${cfg.mount} ${under}${secret.secret_name} ${VALUE_FIELD}=…)`);
          case 403:
            throw new SecretsError("refused", `${where} refused the cell's token for ${path} (403${said(body)}): the token is wrong or has lapsed, or its policy does not cover the path`);
          case 503:
            throw new SecretsError("sealed", `${where} is sealed, or not ready (503${said(body)})`);
          default:
            throw new SecretsError("failed", `${where} answered ${status} for ${path}${said(body)}`);
        }
      };
    },
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
  const known = ["backend", "secrets", ...BACKENDS[backend].keys];
  const unknown = Object.keys(parsed).filter((k) => !known.includes(k));
  if (unknown.length) throw new Error(`FRAGMENT_SECRETS' backend ${backend} takes ${known.join(", ")}, not ${unknown.join(", ")}`);
  BACKENDS[backend].check(parsed);
  if (!Array.isArray(secrets) || secrets.length > SECRETS_MAX) throw new Error(`FRAGMENT_SECRETS' secrets are a list of at most ${SECRETS_MAX}`);
  const seen = new Set();
  for (const s of secrets) {
    if (!s || !BINDING.test(s.binding) || !SECRET_NAME.test(s.secret_name)) throw new Error(`FRAGMENT_SECRETS names each secret's binding and its name: ${JSON.stringify(s)}`);
    if (s.binding === TOKEN_VAR) throw new Error(`FRAGMENT_SECRETS binds ${TOKEN_VAR}, the service's token's variable`);
    if (seen.has(s.binding)) throw new Error(`FRAGMENT_SECRETS binds ${s.binding} twice`);
    seen.add(s.binding);
  }
  return parsed;
}

// Each env the runtime hands over, as the Rust is given it: one per env.
const given = new WeakMap();

// `env` with a stand-in for each secret `FRAGMENT_SECRETS` names, every
// other binding as it was (the stand-ins are its own properties; the rest
// its prototype's), less the service's token. Without `FRAGMENT_SECRETS`,
// `env` itself. A config that is wrong throws, at the first request.
export function withSecrets(env) {
  const text = env && env.FRAGMENT_SECRETS;
  if (typeof text !== "string" || text === "") return env;
  const kept = given.get(env);
  if (kept) return kept;
  const cfg = config(text);
  const read = BACKENDS[cfg.backend].reader(env, cfg);
  const out = Object.create(env);
  for (const secret of cfg.secrets) {
    Object.defineProperty(out, secret.binding, { value: new Fetcher(() => read(secret)), enumerable: true });
  }
  // the token stays the shim's
  Object.defineProperty(out, TOKEN_VAR, { value: undefined, enumerable: false });
  given.set(env, out);
  return out;
}
