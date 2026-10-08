# Secrets

Status: agreed with Paul 2026-09-23 (design; built across phases 2, 4,
and 5; computers' secrets went with them at the cut, tag `celld-final`).
The deployment's own secrets moved from files to its Cloudflare Secrets
Store on 2026-10-05 (Paul: "let's just get this done right"; below).
Principle: **every secret has one home, and code holds a
capability, never a key.**

## Where secrets live

Each secret is stored in the cell that owns it, encrypted at rest, and
nowhere else. Sealing is `crates/core/src/seal.rs`, used by
`cell/src/keys.rs` (since phase 2 of docs/cloudflare-v1.md; before it,
the celld fork's native `KEYS`): the key is HKDF-SHA256 of the
deployment's host secret salted with **the sealing Durable Object's class
and id**, so a value opens only in the object that sealed it; it is
AES-256-GCM sealed as `w2.<key id>.<nonce‖ciphertext>`, where the key id
names which host secret sealed it. The host secret is a Secrets Store
secret bound to the platform Worker as `HOST_SECRET`: an app's isolate
gets an env the platform builds. Rotating it is by name (below): the new one bound as
`HOST_SECRET` and the old as `HOST_SECRET_PREVIOUS`; values sealed under
the old one still open, and come back resealed, which the cell stores.

| Secret | Home |
|---|---|
| A person's GitHub token, other personal keys | the person's own cell |
| A person's own key for an `own` provider of the catalog (docs/computers.md) | their computer's cell (one computer per person for now, decision 13), sealed for it; set and removed by the person (`PUT`/`DELETE /api/connections/{provider}/key`), opened only to swap it in |
| A key an app needs (a third-party API key, a webhook signing key) | the fragment's supervisor |
| The deployment's host secret, the code.storage org key, WorkOS's client id and API key, the operator's keys a computer's swap sends (decision 37) | the account's Cloudflare Secrets Store, each bound to the Workers by name (below), never a Worker variable, a Worker secret, a file, or an app's env. Models and images need none: the Worker's AI binding is pre-authenticated (spike S4) |
| A preview's test secret (`FRAGMENT_TEST_SECRET`, below) | a file on the deploying machine, uploaded as a Worker secret of a branch's platform Worker |
| An operator key (a wipe's: docs/api.md, Operators) | a file on the operator's machine, its secret's 64 hex (`fragment operator key <file>` makes it, 0600, and prints only its npub); the deployment holds its npub alone (`operators`), never the secret. The hosted e2e's is named on its command line (`--operator-key-file`, below) |
| A fragment's own nostr key, an agent's nostr key | made in their cell and kept sealed for it; opened only to sign (an agent's NIP-98 headers) |
| A person's connections (Google, …) | WorkOS Pipes holds and refreshes them; a computer's swap asks Pipes for its short-lived token on each call and holds none (decision 22). A computer's guest holds only placeholders (docs/computers.md) |
| The key computers' placeholders are tagged with | derived from the host secret (HKDF-SHA256, its own salt), never stored or provisioned apart: in the platform Worker alone, never in a container. Rotating the host secret rotates every placeholder (guests read theirs again within seconds; tags under `HOST_SECRET_PREVIOUS` still verify during a rotation) |
| A browser's sessions (the platform's, and one per fragment origin) | the registry cell, as SHA-256 hashes of random tokens; the tokens live only in HttpOnly cookies |

Never in git, a log, a command line, or a channel record. Rotating a secret means changing it in its home; everything that
uses it reads it from there.

## The deployment's secrets: its Secrets Store

The secrets the Workers read live in the account's Cloudflare Secrets
Store (open beta: one store per account, 100 secrets, values up to
64 KiB, and a value once saved is never given back, by the API or the
dashboard). The deployment's config (`deploy/example.jsonc`) names each
by its store name; `cargo xtask deploy` binds each to the Workers under a
name fixed in code (`fragment_core::secrets_store`) and reads no value:

| Config field | Bound as | To |
|---|---|---|
| `host_secret` | `HOST_SECRET` | the platform Worker |
| `host_secret_previous` (while a rotation runs) | `HOST_SECRET_PREVIOUS` | the platform Worker |
| `codestorage.private_key` | `CODESTORAGE_KEY` | the platform Worker |
| `workos.client_id`, `workos.api_key` | `WORKOS_CLIENT`, `WORKOS_KEY` | the platform Worker |
| `stripe.key`, `stripe.webhook_secret` (seats sold: docs/billing.md) | `STRIPE_KEY` (the account's restricted key), `STRIPE_WEBHOOK` (this deployment's endpoint's signing secret) | the platform Worker |
| a provider's `key` (an operator key) | `OPERATOR_KEY_<NAME>` (`perplexity` → `OPERATOR_KEY_PERPLEXITY`) | the platform Worker |

- **Reading.** `cell/src/keys.rs` is the one place they are read.
  A value read is kept per isolate for **at most 60 seconds**
  (`secrets_store::CACHE_MS_MAX`, `Cache`), then read again: a value set
  again in the store is in use in every warm isolate within a minute,
  with no deploy. For that minute an isolate may still use the value
  before it, which every secret here allows (a vendor's old key works
  until the vendor revokes it; the host secret rotates by name). A
  binding whose secret the store lacks is an error where it is read
  (`HostFailed`); there is no fallback to a Worker secret or a variable.
- **Before a deploy.** It lists the store (read-only) before it builds or
  makes anything, and refuses when a secret the config names is not
  there, naming each, the field naming it, and the `cargo xtask secret
  set` that fixes it. The test secret is the only Worker secret a deploy
  still uploads (`--secrets-file`, a branch's alone).
- **Setting.** Through wrangler on the pinned Node
  (`crates/devstack/src/store.rs`), never with `--value`: a value is
  never on a command line, in output, or in a file outside a run's own
  scratch.

  ```
  cargo xtask secret set <name> --config <file>              # wrangler's hidden prompt
  printf %s "$VALUE" | cargo xtask secret set <name> --config <file>   # or standard input
  cargo xtask secret set <name> --config <file> --from-file <path>   # a file's contents, once
  cargo xtask secret gen <name> --config <file>              # 32 random bytes as hex, made here
  cargo xtask secret list --config <file>                    # names and times, never values
  ```

  The store is the account's one (the config's `account_id`): `set` and
  `gen` make it, named `fragment`, when there is none, and say so; `list`
  says there is none and makes nothing. `set` updates a secret already
  there (but for the config's host secret, below); `gen` makes new
  secrets only. They answer one line, `<name>: created in the account's
  Secrets Store fragment (<id>)` (or `updated`; `gen` adds `(32 random
  bytes as hex, made here and never shown)`). `list` answers the store's
  secrets, each name with its created and modified times, then whether
  the config names any it lacks. There is no `rm`: deleting a secret is
  irreversible, and Paul's, with `wrangler secrets-store secret delete`
  or on the dashboard. `--local <state dir>` in place of `--config` acts
  on wrangler's local store instead (`cell/.wrangler/state` is `cargo
  xtask dev`'s).
- **Rotating a key** (code.storage's, WorkOS's, an operator key): `cargo
  xtask secret set <its name> --config <file>` with the new value; warm
  isolates use it within a minute, no deploy. Revoke the old one at its
  vendor after that minute.
- **Rotating the host secret** is by name, never in place: every value at
  rest is sealed under it, and a store gives no value back, so one set
  again in place would leave them unopenable (`set` refuses the config's
  `host_secret` and `host_secret_previous` once they exist, and `gen`
  never replaces anything). Make a new one, `cargo xtask secret gen
  fragment-host-secret-<date> --config <file>`; in the config, name it as
  `host_secret` and the one before as `host_secret_previous`; deploy.
  Values sealed under the old one open, come back resealed, and are
  stored so as they are read; once that has run its course, remove
  `host_secret_previous` and deploy again (the old secret stays in the
  store until Paul deletes it).
- **Dev and the e2e** bind the same names (`store::Bound::conventional`,
  `deploy/example.jsonc`'s) in wrangler's local store, in the node's own
  state directory (`cell/.wrangler/state`; the e2e's
  `target/e2e/<run>/cell/.wrangler/state`), never `--remote`: devstack
  seeds it with the values it makes (its host secret, the code.storage
  fake's org key, the WorkOS fake's client and key, the e2e's test
  operator keys) before the node starts, once per state (a stamp beside
  it records what was seeded), and `cargo xtask dev --clean` clears it
  with the rest of the state.

**Two secrets stay local files**, named by path in the config, and are no
store secrets:

- `dns_token_file`: the deploying machine's own Cloudflare DNS token,
  which xtask itself uses to make the deployment's DNS record. No Worker
  reads it.
- `test_secret_file`: a preview's test levers' secret (below). The hosted
  e2e runner must send its value, and a store never gives one back; so
  it is a file, uploaded as the Worker secret `FRAGMENT_TEST_SECRET` on a
  branch deploy.

## Placeholders and the operator's keys (Paul, 2026-10-04)

A computer's guest is given each credential its agent may use as a
placeholder in a standard environment variable
(`PERPLEXITY_API_KEY=fck_perplexity_<tag>`; docs/computers.md). The
placeholder is not a secret: its tag is an HMAC of (computer, agent
fragment, provider) under the tag key above, so it names its agent and
works only from that computer's egress, toward its provider's own hosts.
A guest that prints one, or a provider that echoes one, leaks nothing
usable. The intercept adds the real credential and logs the provider
only, never a tag or a value.

The operator's keys are store secrets, each named by its catalog row's
`key` (`deploy/example.jsonc`, `deploy/e2e.jsonc`) and bound as
`OPERATOR_KEY_<NAME>`. The platform's four are Paul's to set (`cargo
xtask secret set <name> --config <file>`, which prompts):

| Key | Store secret | Bound as |
|---|---|---|
| Perplexity | `fragment-perplexity-api-key` | `OPERATOR_KEY_PERPLEXITY` |
| Google Places | `fragment-google-places-api-key` | `OPERATOR_KEY_GOOGLE_PLACES` |
| xAI | `fragment-xai-api-key` | `OPERATOR_KEY_XAI` |
| ElevenLabs | `fragment-elevenlabs-api-key` | `OPERATOR_KEY_ELEVENLABS` |

A deploy whose store lacks one stops before anything changes, naming the
command that sets it. Rotating one: set it again; the swap reads it
through the minute's cache, so it is in use within a minute, with no
deploy. The local e2e and dev use test values of their own, in the local
store, sent only to the upstream fake.

## The test secret (previews and the local e2e)

The hosted e2e (`cargo xtask e2e --hosted`, crates/e2e/src/hosted.rs)
proves a branch deployment on its real vendors without real accounts:
its people sign in through the deployment's test levers (docs/api.md,
Test levers), which exist only where a test secret is.

- **Its home.** A file named in the deployment's config,
  `test_secret_file` (32 random bytes or more, as text: `openssl rand
  -hex 32`), and no store secret: the runner sends its value, which a
  store never gives back. `cargo xtask deploy` uploads it as
  the Worker secret `FRAGMENT_TEST_SECRET` for a `--branch` deploy only:
  a config of a deployment of its own (production: `platform_host` and
  `fragment_suffix`) that names one is refused before anything is read,
  and the cell honours none on such a deployment if one is set by hand
  (it logs `levers.refused`). The local e2e makes one per run, in its
  `.dev.vars`.
- **Its use.** The hosted e2e reads the same file and sends it in the
  `x-fragment-test-secret` header of `/api/test/*` requests, to the
  platform's host only (never to a fragment's host, where app code reads
  the headers). Never on a command line (the suite takes the file's
  path), in a log, or in a report.
- **What it opens.** Without it a lever is the 404 any missing route is.
  With it, on a preview: e2e people only (`<name>@e2e.test`, under an
  issuer of their own, so never a real person), seats whose paid calls
  are capped at what the run lends them (none unless asked); levers on
  `e2e-…` fragments and e2e people's ledgers only; no registry lever.
  A preview makes at most 1000 e2e people and lends them at most 2000
  paid calls a day, so a secret that leaked spends that much, and
  touches no one real. Rotate it by changing the file and deploying the
  branch again; take it away with `wrangler secret delete
  FRAGMENT_TEST_SECRET --name fragment-<branch>` (removing
  `test_secret_file` from the config leaves the Worker secret as it was).

## Operator keys (a wipe's)

An operator wipes a person (docs/api.md, Operators) by signing with a key
the deployment's `operators` lists (`FRAGMENT_OPERATORS`, a Worker
variable: public keys only). One no person holds is the one to wipe with:
the wipe never removes it, as it removes the keys of whom it wipes.

- **Its home.** A file on the operator's machine, holding the key's
  secret (64 hex) and nothing else, 0600: `fragment operator key <file>`
  makes it (and refuses a path that exists) and prints its npub, which
  goes in the deployment's config (`operators`) and reaches the Worker on
  the next deploy. The CLI reads it by path (`--key-file`, or
  `FRAGMENT_OPERATOR_KEY_FILE`); never on a command line, in a log, or in
  a report.
- **The hosted e2e's** is a file `cargo xtask e2e --hosted …
  --operator-key-file <file>` names (Paul's machine:
  `~/.config/finite-next/secrets/e2e-operator-key`), its npub in the e2e
  preview's config's `operators`; the runner reads it by path and wipes
  only the e2e people it signed in. The config names no file (a key is
  its holder's), and the deploy reads and uploads nothing of it.
- **Rotating** one: make a new file, list its npub, deploy, then take the
  old npub out and deploy again. A key that leaked wipes anyone until its
  npub is taken out of `operators`.

## How code uses a secret

- **App code** (the facet) has no network and no keys. It runs in an
  isolate of its own from the Worker Loader, with an env the platform
  builds that holds only `FILES` and `globalOutbound: null`
  (`cell/src/js.rs`), so no author code names a deployment secret (the
  e2e's `keys` section deploys an app that looks through its env and its
  global scope for one). It asks the platform through a capability
  ("draw an image", "fetch this API with secret X"), and the platform
  adds the credential on the way out: a job's `job.fetch` names a secret
  as `{{NAME}}` in a header, and the supervisor opens it only as the
  request leaves (`step_fetch` in `cell/src/jobs.rs`, the one egress
  point). `job.ai.*` steps need no key: text and images go through the
  Worker's AI binding (cell/src/ai.rs, models.rs).
- **Agents in cells** call models through the platform's model route
  (cell/src/models.rs), which holds no key either: the Worker's AI
  binding is pre-authenticated, and the payer's ledger meters each call.

## Models need no per-person credential

*Superseded 2026-10-03 (docs/cloudflare-v1.md, phase 3; decision 23):*
the per-person OpenRouter keys (minted with a management key, their
limit the month's allowance) are gone. Models are Workers AI through the
deployment's AI Gateway, called through the AI binding, and each call is
reserved and settled on its payer's usage ledger (docs/ledger.md), which
is what stops a person, not a vendor's key limit. No BYOK (Paul,
2026-10-02).

