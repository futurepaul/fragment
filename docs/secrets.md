# Secrets

Status: agreed with Paul 2026-09-23 (design; built across phases 2, 4,
and 5; computers' secrets went with them at the cut, tag `celld-final`).
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
names which host secret sealed it. The host secret is a Worker secret
(`FRAGMENT_HOST_SECRET`), in the platform Worker's env only: an app's
isolate gets an env the platform builds. Rotating it: set the new one as
`FRAGMENT_HOST_SECRET` and the old as `FRAGMENT_HOST_SECRET_PREVIOUS`;
values sealed under the old one still open, and come back resealed, which
the cell stores.

| Secret | Home |
|---|---|
| A person's GitHub token, other personal keys | the person's own cell |
| A person's own key for an `own` provider of the catalog (docs/computers.md) | their computer's cell (one computer per person for now, decision 13), sealed for it; set and removed by the person (`PUT`/`DELETE /api/connections/{provider}/key`), opened only to swap it in |
| A key an app needs (a third-party API key, a webhook signing key) | the fragment's supervisor |
| The deployment's host secret, the code.storage org key, the WorkOS API key, the sign-in client's secret (`FRAGMENT_OIDC_CLIENT_SECRET`, docs/self-host.md seam 4: WorkOS AuthKit's OAuth application's, or another provider's), the operator's keys a computer's swap sends (`FRAGMENT_KEY_<NAME>`, decision 37), a preview's test secret (`FRAGMENT_TEST_SECRET`, below) | Worker secrets of the platform Worker (`cargo xtask deploy` uploads them from files named in the deployment's config; `.dev.vars` in dev), never a Worker variable or an app's env. Models and images need none: the Worker's AI binding is pre-authenticated (spike S4) |
| A fragment's own nostr key, an agent's nostr key | made in their cell and kept sealed for it; opened only to sign (an agent's NIP-98 headers) |
| A person's connections (Google, …) | WorkOS Pipes holds and refreshes them; a computer's swap asks for a short-lived token per call and holds it in memory at most ten minutes (decision 22). A computer's guest holds only placeholders (docs/computers.md) |
| The key computers' placeholders are tagged with | derived from the host secret (HKDF-SHA256, its own salt), never stored or provisioned apart: in the platform Worker alone, never in a container. Rotating the host secret rotates every placeholder (guests read theirs again within seconds; tags under `FRAGMENT_HOST_SECRET_PREVIOUS` still verify during a rotation) |
| A browser's sessions (the platform's, and one per fragment origin) | the registry cell, as SHA-256 hashes of random tokens; the tokens live only in HttpOnly cookies. An OpenID Connect sign-in's PKCE verifier waits beside its state there (ten minutes at most), and its session's id_token is kept sealed for the registry, as the provider's logout hint |

Never in git, a log, a command line, or a channel record. Rotating a secret means changing it in its home; everything that
uses it reads it from there.

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

The operator's keys are Worker secrets (`FRAGMENT_KEY_<NAME>`), each
uploaded by `cargo xtask deploy` from the `key_file` its catalog row names
(`deploy/example.jsonc`, `deploy/e2e.jsonc`). The platform's four are
Paul's to supply, as text files (mode 600), at:

| Key | File | Worker secret |
|---|---|---|
| Perplexity | `~/.config/fragment/secrets/perplexity-api-key` | `FRAGMENT_KEY_PERPLEXITY` |
| Google Places | `~/.config/fragment/secrets/google-places-api-key` | `FRAGMENT_KEY_GOOGLE_PLACES` |
| xAI | `~/.config/fragment/secrets/xai-api-key` | `FRAGMENT_KEY_XAI` |
| ElevenLabs | `~/.config/fragment/secrets/elevenlabs-api-key` | `FRAGMENT_KEY_ELEVENLABS` |

A deploy that cannot read one stops before anything changes. Rotating one:
change its file and deploy again (the swap reads the secret per call). The
local e2e and dev use test values of their own, sent only to the upstream
fake.

## The test secret (previews and the local e2e)

The hosted e2e (`cargo xtask e2e --hosted`, crates/e2e/src/hosted.rs)
proves a branch deployment on its real vendors without real accounts:
its people sign in through the deployment's test levers (docs/api.md,
Test levers), which exist only where a test secret is.

- **Its home.** A file named in the deployment's config,
  `test_secret_file` (32 random bytes or more, as text, made as the host
  secret is: `openssl rand -hex 32`). `cargo xtask deploy` uploads it as
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

## How code uses a secret

- **App code** (the facet) has no network and no keys. It asks the
  platform through a capability ("draw an image", "fetch this API with
  secret X"), and the platform adds the credential on the way out
  (`globalOutbound` is `null`; the capability is the only way out).
  Built in slice D: a job's `job.fetch` names a secret as `{{NAME}}` in a
  header, and the supervisor opens it only as the request leaves
  (`step_fetch` in `cell/src/jobs.rs`). That one function is the egress
  point a native egress in the celld fork would take over. `job.ai.*`
  steps need no key: text and images go through the Worker's AI binding
  (cell/src/ai.rs, models.rs).
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

