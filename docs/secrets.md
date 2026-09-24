# Secrets

Status: agreed with Paul 2026-09-23 (design; built across phases 2, 4, 5,
and 8). Principle: **every secret has one home, and code holds a
capability, never a key.**

## Where secrets live

Each secret is stored in the cell that owns it, encrypted at rest, and
nowhere else. The key is HKDF-SHA256 of the fleet's host secret salted
with the cell's npub; the value is AES-256-GCM sealed as
`w1.<key id>.<nonce‖ciphertext>`, where the key id names which host
secret sealed it. Rotating the host secret: set the new one as
`FRAGMENT_HOST_SECRET` and the old as `FRAGMENT_HOST_SECRET_PREVIOUS`;
values sealed under the old one still open (phase 2 slice B,
`crates/core/src/secrets.rs`). The cell reseals such a value when it
first reads it (slice D: a job's fetch).

| Secret | Home |
|---|---|
| A person's model credential (their OpenRouter key, minted by the platform with their budget as its limit), their GitHub token, other personal keys | the person's own cell |
| A key an app needs (a third-party API key, a webhook signing key) | the fragment's supervisor |
| The Sprites org token a person's computers run under | the person's own cell (ours by default; theirs if they bring their own Sprites org) |
| The fleet's host secret, the code.storage org key, the OpenRouter management key | the fleet's own configuration (`vars` rendered at deploy) |

Never in git, a log, a command line, a channel record, or a computer's
disk. Rotating a secret means changing it in its home; everything that
uses it reads it from there.

## How code uses a secret

- **App code** (the facet) has no network and no keys. It asks the
  platform through a capability ("call OpenRouter", "fetch this API with
  secret X"), and the platform adds the credential on the way out
  (`globalOutbound` is `null`; the capability is the only way out).
  Built in slice D: a job's `job.fetch` names a secret as `{{NAME}}` in a
  header, and the supervisor opens it only as the request leaves
  (`step_fetch` in `cell/src/jobs.rs`). That one function is the egress
  point a native egress in the celld fork would take over. `job.ai.*`
  (slice F) uses the same path for the fragment's `OPENROUTER_API_KEY`.
- **Agents in cells** call models through the platform, which attaches
  the user's own model credential.
- **Computers** (Sprites) use **Sprites connectors**, Fly's credential
  proxy (https://docs.sprites.dev/concepts/connectors/). The platform
  creates one connector per credential through the Sprites API: an
  OpenRouter connector with the person's key, a GitHub connector (OAuth),
  and a Custom API connector for anything else, including the platform's
  own API. Each connector's access policy admits only that person's
  Sprites (a Sprite label) and an allow-list of paths. A Sprite calls
  `https://api.sprites.dev/v1/gateway/<provider>/<connection>/<path>` with
  no credential; the gateway identifies the Sprite from Fly's request
  signature and attaches the stored one. goose on the Sprite points its
  OpenRouter provider at the gateway, so the agent's own shell has no key
  to read. This replaces the goose handoff's proposal of a platform
  inference proxy for computers.

## Model credentials are per person

*Amended 2026-09-23 (ROADMAP decision 14):* for now the platform pays for
AI up to each person's monthly budget. Each person still gets a key of
their own: the platform's OpenRouter management key mints one per person
with a credit limit equal to their budget, kept in the person's cell, so
OpenRouter itself stops them at the limit. Connecting their own account,
below, comes back later as the way past it.

Later, a person **connects OpenRouter** (OpenRouter's OAuth PKCE flow,
https://openrouter.ai/docs/guides/overview/auth/oauth): they approve, and
the platform receives a key on their own OpenRouter account, billed to
them, which they can revoke there. The key's origin stays pluggable:
Stripe Projects (`stripe projects add openrouter/api`) provisions an
OpenRouter account today only from a developer's own CLI, with no
platform API; Stripe agreed to acquire OpenRouter (2026-08-19), so a
platform path may follow. Whatever the origin, the key lands in the same
home and reaches computers through the same connector.

## Open, to settle in the phase that builds each piece

- The connector gateway's 120 s upstream timeout against long model
  calls (a 96 s reasoning step was seen in the goose spike): whether a
  streaming response stays open past it (phase 8).
- How many connectors one Sprites org holds (phase 8).

