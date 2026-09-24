# Integrating with finite.computer

fragment is where we experiment; finite.computer (Finite V3) is where
people will pay, sign in, and keep their keys and connections. The goal
(Paul, 2026-09-24): finite.computer users use fragment, and fragment
relies on finite.computer's Core for everything Core owns, with no second
version of it. We do not wait for V3 to ship. Instead fragment builds a
minimal stand-in for each Core concern **in V3's shape**, behind a seam,
so that moving over replaces the stand-in and changes nothing else.

This file is the list. **Any fragment change that touches one of these
concerns updates its row here in the same commit.**

Sources: the Finite V3 project in Linear (read 2026-09-24). Status
matters: an agreed direction is not a finished interface.

| Issue | What | Status on 2026-09-24 |
|---|---|---|
| FIN-10 | Core platform contracts: accounts, orgs, billing (Stripe in Core), agent lifecycle, Google credentials | accepted by Alex 2026-09-07; wider review pending |
| FIN-11 | BANKS: identity and public-key registry, shared WorkOS login, designated owners | direction agreed with Alex; interfaces, migration, review open |
| FIN-13 | Finite Private authorization and usage accounting | Austin's; not designed yet |
| FIN-14 | Add-on access and entitlements (Sites, Brain, connections) | not designed yet |
| FIN-15 | Chat: native Hermes web chat, then optional SimpleX | accepted by Alex; review open |
| FIN-21 | Hermes runtime on Fly Sprites, backups, restore | accepted 2026-09-04; Sprites chosen 2026-09-14; pilot gates open |
| FIN-39 | Authenticated access to a hosted agent; bearer vs Iroh transport | in progress |
| FIN-60 | Core-managed runtime secret delivery (SOPS + encrypted app storage) | in review |

## The rules fragment follows now (FIN-11's model)

1. **Identities, not keys.** A person or an agent is a stable identity
   with an opaque ID. Grants (fragment membership, ownership, invites)
   name identities, so replacing a key rewrites no grant.
2. **A person's identity is keyed by a verified `(issuer, subject)`**
   from sign-in, never by email. Email is a mutable attribute. Matching
   an email never merges or reassigns identities.
3. **Keys stay with their callers.** The platform holds no person's
   private key. Browsers act through a session; the CLI and agents sign
   with their own keys (NIP-98). An agent's key lives sealed in its own
   cell, which is that agent's runtime: the caller holding its key.
4. **An identity has one or more active public keys.** A key is added
   with proof of possession, by the identity's human (for an agent, its
   owner), and revoked on its own. Replacing a key adds the new one
   before revoking the old. An agent always has at least one.
5. **Every agent has one designated human owner.** The owner can read
   whatever the agent can read. That is read visibility only: no writes,
   no resharing, no ownership. Sharing with an agent says so.
   *fragment's reading (slice A):* the owner reads a fragment their agent
   is a member of as a `viewer` (status, files, channels, events, runs,
   queries, the site), whatever the agent's role; an editor's reads carry
   capabilities (the inbox token, storage tokens, secret names) that
   would let the owner act, so they stay closed. Operations that act
   (mutations, jobs) need the owner's own membership.
6. **Services own permissions.** The registry never stores a grant;
   fragments keep their members and roles. Organization membership grants
   nothing by itself.
7. **Live lookups that fail visibly.** No authorization cache outlives a
   revocation. When the registry cannot answer, protected requests fail;
   they never fall back to a stale answer.

fragment-only, and fine to stay so (services own their policy): a
fragment's own key and identity, anonymous visitors (`anon:`), and
unguessable view links. Visibility `link` means anyone holding the link
is a viewer; `public` means anyone at all, no link needed.

## The seams

| Concern | Owner in V3 | fragment now | The swap |
|---|---|---|---|
| Human sign-in | WorkOS shared login (FIN-11) | **built (slice B):** fragment's own WorkOS environment, sign-up off, Paul invites; a person is keyed by `(workos:<client id>, user id)`; `/auth/link` adds a second sign-in to the signed-in person | finite.computer's login becomes a second issuer; a person links it with `/auth/link` (explicit, never by email) |
| Identity and key registry | Core / BANKS (FIN-11) | **built (slice A):** one registry cell for the fleet (`cell/src/registry.rs`), rules 1–7 behind `/api/identities` (docs/api.md, Identities): resolve a key live on every signed request, register a person or an agent (with a key proof), add a key with a proof, revoke one, check an owner's key | a BANKS client behind the same routes; public keys and owner facts move, no private key does |
| Browser sessions | Core session + a small per-service adapter (FIN-11) | **built (slice B):** a session on the platform origin, in the registry, looked up live; each fragment origin gets its own cookie through a single-use redemption (the pattern Sites v2 uses: finite-sites ADR 0025); a CLI key joins a person by a browser approval and the key's own claim | the platform session comes from finite.computer's login instead; the per-origin exchange stays |
| Resource permissions | each service | fragment members and roles, per fragment | unchanged |
| Billing, orgs, entitlements | Core + Stripe (FIN-10) | a monthly allowance that Paul sets by hand ($20, decision 14), held by a billing org that is always the person's own personal org; no Stripe | Core supplies the entitlement facts; the allowance row is replaced by them |
| Model access and metering | Finite Private + usage accounting (FIN-13) | per-person OpenRouter keys with a credit limit, and fragment's own ledger. Each usage row records FIN-10's report fields: source reference, agent, billing org, period, unit, quantity, delivered idempotently | model calls move to Finite Private with Core's authorization; the same rows go to Core |
| Private keys and platform secrets | callers (FIN-11); Core with SOPS (FIN-60) | fragment's sealed cell secrets and fleet config (`docs/secrets.md`) | platform secrets follow FIN-60 when fragment runs inside Finite's deploy |
| Google Workspace and other connections | Core: consent, encrypted refresh tokens, short-lived access tokens (FIN-10) | not built | an agent or computer asks Core for a short-lived token for its assigned connection |
| Chat channels (SimpleX, web chat) | Hermes on the agent's runtime (FIN-15) | fragment chats are fragments (`templates/chat`); no SimpleX | SimpleX belongs to a Hermes agent; a Hermes agent can join fragment chats through a bridge |
| Agent runtimes | Hermes on Sprites (FIN-21) | goose agents in cells; computers on Sprites (`fragment computer serve`) | share one Sprite adapter and its lessons (FIN-66) |
| Reaching a computer or agent | stable `<agentid>.agents.finite.computer`, owner-only (FIN-15); bearer vs Iroh open (FIN-39) | open: the Sprite URL's org auth and the computer's token both want `authorization` | decide once for both (see below) |

## Not built in fragment, on purpose

Stripe, Google Workspace custody, SimpleX, Finite Private, Hermes web
chat, organizations with several members. Each has an owner above; a
fragment experiment that needs one uses a stub named for the V3 contract
it stands in for, and records it here.

## Moving fragment's people over (when BANKS ships)

1. Each person signs in to fragment with their finite.computer login
   once. fragment records the second `(issuer, subject)` on the same
   identity; a person who never links keeps working.
2. fragment registers each identity's active public keys (CLI keys,
   agent keys) and each agent's owner in BANKS, through BANKS's owner-
   authorized key operations. No private key moves.
3. fragment's identity IDs map to BANKS identity IDs one to one (a
   mapping table if they differ). Grants do not change, because they
   name identities.
4. The registry cell becomes a BANKS client; its tests run against both.

Rehearse on a copy of fragment.club's state first, including a revoked
key (restoring old state must not revive it) and an unlinked person.

## Open questions shared with V3

- **Reaching a computer on a Sprite.** Options: the platform calls the
  Sprite URL with the Sprites org token and the computer's token moves
  to its own header (works today); Iroh, where the computer dials out and
  is addressed by its key (finite-mono PR #913 proves dashboard-to-agent
  reads over Iroh); Fly private networking, if Sprites join it (unknown).
  One answer should serve both fragment computers and V3's agents.
- **The usage report** (FIN-13): units, granularity and corrections are
  Austin's to decide; fragment's rows keep FIN-10's fields until then.
- **BANKS's interface** (FIN-11): fragment's registry interface is one
  concrete proposal for it (docs/api.md, Identities: the routes, and the
  key proof: a NIP-98 event by the new key for the same request, naming
  the signer in a `p` tag). Tell Alex where they differ.
- **What "reads whatever the agent reads" covers** (FIN-11 rule 5):
  fragment caps the owner at `viewer` (rule 5 above). Whether BANKS's
  services should do the same is Alex's call.
