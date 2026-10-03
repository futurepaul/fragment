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
| A key an app needs (a third-party API key, a webhook signing key) | the fragment's supervisor |
| The deployment's host secret, the code.storage org key, the WorkOS API key, the operator's keys a computer's swap sends (`FRAGMENT_KEY_<NAME>`, decision 37) | Worker secrets of the platform Worker (`cargo xtask deploy` uploads them from files named in the deployment's config; `.dev.vars` in dev), never a Worker variable or an app's env. Models and images need none: the Worker's AI binding is pre-authenticated (spike S4) |
| A fragment's own nostr key, an agent's nostr key | made in their cell and kept sealed for it; opened only to sign (an agent's NIP-98 headers) |
| A person's connections (Google, GitHub, …) | WorkOS Pipes holds and refreshes them; a computer's swap asks for a short-lived token per call and holds it in memory at most ten minutes (decision 22). A computer's guest holds only placeholders (docs/computers.md) |
| A browser's sessions (the platform's, and one per fragment origin) | the registry cell, as SHA-256 hashes of random tokens; the tokens live only in HttpOnly cookies |

Never in git, a log, a command line, or a channel record. Rotating a secret means changing it in its home; everything that
uses it reads it from there.

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

