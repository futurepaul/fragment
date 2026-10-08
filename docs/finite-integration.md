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
| Human sign-in | WorkOS shared login (FIN-11) | **built (slice B):** fragment's own WorkOS environment, sign-up off, Paul invites; a person is keyed by `(workos:<client id>, user id)`; `/auth/link` adds a second sign-in to the signed-in person. A preview with test levers also signs in e2e people (`<name>@e2e.test`) under the issuer `e2e.test`, which no real sign-in has (docs/secrets.md). MCP clients (Claude, ChatGPT, any other) connect a person through the platform as an OAuth 2.1 authorization server (docs/api.md, Connected clients): the person signs in as here, and says yes on the platform's page | finite.computer's login becomes a second issuer; a person links it with `/auth/link` (explicit, never by email). The authorization server stays fragment's own while its resources (its MCP servers) are; were Core to issue MCP clients' tokens, its tokens would name the person's identity, and fragment would check them where it checks its own |
| Identity and key registry | Core / BANKS (FIN-11) | **built (slice A):** one registry cell for the fleet (`cell/src/registry.rs`), rules 1–7 behind `/api/identities` (docs/api.md, Identities): resolve a key live on every signed request whose answer depends on who is asking (a page or a file everyone who may see it gets alike asks nothing), register a person or an agent (with a key proof), add a key with a proof, revoke one, check an owner's key; its inner routes are typed calls (`cell/src/registry/calls.rs`) answering one `Identity` type, and a call that acts resolves who asks (a signing key or a platform session, `calls::By`) in the same turn: one round trip, and a key revoked a moment before cannot act. It also holds connected clients (`cell/src/registry/oauth.rs`): a client's registration, and each connection a person made (its client, its resource, its tokens' SHA-256), a credential like a session that names the person and grants nothing | a BANKS client behind the same calls; public keys and owner facts move, no private key does; connections stay fragment's, as sessions' per-origin exchange does |
| Browser sessions | Core session + a small per-service adapter (FIN-11) | **built (slice B):** a session on the platform origin, in the registry, looked up live; each fragment origin (under `fragment.boats`, cross-site from the platform's `fragment.club`: docs/cloudflare-v1.md, decision 5) gets its own cookie through a single-use redemption (the pattern Sites v2 uses: finite-sites ADR 0025), at most 4 a fragment per platform session, ended by `__signout`; a frame's own partitioned session from a frame redemption, bound to the platform's page that framed it (the shell's tabs: `/auth/frame`, for a frame of the platform's own page only); a fragment not the person's nor shared with them asks once before it learns who they are (docs/api.md, Asking first); a browser opening a fragment's URL with no session there is sent through that exchange and back; a CLI key joins a person by a browser approval and the key's own claim | the platform session comes from finite.computer's login instead; the per-origin exchange stays |
| Resource permissions | each service | fragment members and roles, per fragment | unchanged |
| Account deletion | Core: closing a person's account (FIN-10's accounts; FIN-11's identities and keys) | **built (2026-10-07; docs/api.md, Operators):** an operator's wipe of a person (`GET\|POST /api/people/{person}/wipe`, `fragment operator wipe`), signed by an operator key no person holds, resumable, its progress and lock in the registry (`cell/src/registry/wipe.rs`; its steps `fragment_core::wipe`): every fragment they own ended and its code.storage repo deleted, their and their agents' memberships elsewhere left, their computer (container, saves, record), ledger and lists emptied, and every registry row of theirs and their agents' deleted, so their next sign-in is a new person. It leaves WorkOS alone: the user, and Pipes' connections keyed by it | Core's account deletion calls the same wipe (with an operator key of Core's) for the identity the account maps to, and deletes the WorkOS user and its connections itself; with BANKS, the wipe's last step (the registry's rows) becomes BANKS' delete of the identity and its keys |
| Billing, orgs, entitlements | Core + Stripe (FIN-10) | **built (docs/cloudflare-v1.md, phase 3; docs/ledger.md):** a usage ledger per person (one `Ledger` Durable Object, named by their identity, on `fragment_core::ledger`): plans (`guest`, `seat` with $50 of credit a month, `seat_always_on` with $100; a new person's is `FRAGMENT_DEFAULT_PLAN`), purchased credit (operators' grants), seat states, overdrafts, and each fragment's monthly cap; a guest makes no fragments (Paul, 2026-10-03); at zero agents stop, past the overdraft the person's fragments go read-only (their cron and triggers start no runs). Operators command it (`POST /api/ledger/{person}/grant\|plan\|seat\|overdraft`); no Stripe | Stripe's two hooks become commands on the same ledger: a paid checkout a `GrantCredit` (its payment id the grant's id), a subscription's events a `SetSeat` and `SetPlan`; Core supplies the entitlement facts behind them |
| Model access and metering | Finite Private + usage accounting (FIN-13) | **built (phase 3):** models through the platform's model route on Workers AI and AI Gateway by tier (`cheap`, `medium`; `high` off) and `vision` (the deployment's vision model, GLM-5.3 Flash, for a runtime's image calls), reserved before each call and settled from its last usage on the payer's ledger (an agent's owner; a fragment's owner for its AI steps), every row priced from the price book (`fragment_core::price`: list price, AI Gateway's 5% credits fee, the margin) and kept once by its reference; a fragment's hosting (requests, dynamic workers, storage, its preview cards' browser time) metered to its owner through the `fragment-ledger` queue; images on Workers AI (FLUX.1 [schnell]) through the same binding, metered in neurons by their tiles and steps; video steps off until they run on Cloudflare (the debt ledger) | model calls move to Finite Private with Core's authorization; the ledger's rows (reference, payer, agent, fragment, usage, charge) go to Core as FIN-10's usage reports |

| Private keys and platform secrets | callers (FIN-11); Core with SOPS (FIN-60) | cell secrets sealed per Durable Object under the host secret (`cell/src/keys.rs`); the deployment's own secrets in its account's Cloudflare Secrets Store, bound to the Workers by name and read only by `keys.rs`, a minute's cache at most (`docs/secrets.md`, since 2026-10-05) | platform secrets follow FIN-60 when fragment runs inside Finite's deploy; `keys.rs` reads them from its bindings, wherever the store's values come from |
| Google Workspace and other connections | Core: consent, encrypted refresh tokens, short-lived access tokens (FIN-10) | **built (phase 4; Paul, 2026-10-04; docs/computers.md):** a provider catalog per deployment (connections, the operator's keys, people's own keys). WorkOS Pipes holds and refreshes connections' tokens; a guest holds per-agent placeholders in standard environment variables (`GOOGLE_OAUTH_ACCESS_TOKEN=fcx_google_<tag>`, a platform-keyed MAC naming the agent), which a computer's swap replaces with a short-lived token toward the provider's own hosts, for an agent of the owner's that may use every connection its owner has unless its owner narrows it to a list (decision 44) | an agent or computer asks Core for a short-lived token for its owner's connection; the placeholders and the catalog stay |
| Chat channels (SimpleX, web chat) | Hermes on the agent's runtime (FIN-15) | the blessed chat template (`templates/chat`, served from the platform's release: decision 40; docs/chat-records.md): replies streamed, steps and approvals as cards, files and voice memos both ways, and an agent's reply pushed to the chat's people while they are away (fragment push, from the template's own job); no SimpleX | SimpleX belongs to a Hermes agent; a Hermes agent can join fragment chats through a bridge |
| Agent runtimes | Hermes on Sprites (FIN-21); self-hosting first-class (Paul, 2026-09-29) | goose on a computer of its owner's (on branch `claude/optchat`: the generic Computer, the goose image with its bridge inside it: docs/computers.md, docs/optchat.md); Hermes went on that branch; Sprites and sandcastle went at the cut (tag `celld-final`), the in-fragment goose agents on 2026-10-07 (issue #156) | the generic Computer on Cloudflare, the agent runtime from its own image with its bridge inside it (docs/cloudflare-v1.md, decision 21 and phase 4) |
| Reaching a computer or agent | stable `<agentid>.agents.finite.computer`, owner-only (FIN-15); bearer vs Iroh open (FIN-39) | an agent through the chats and channels it follows; a computer through its owner's authenticated tab onto its ports (docs/cloudflare-v1.md, decision 11) | decide once for both (see below) |

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

- **Reaching a computer.** fragment's come back on Cloudflare
  (docs/cloudflare-v1.md, phase 4). For one on a Sprite, the options: the
  platform calls the Sprite URL with the Sprites org token and the
  computer's token moves to its own header (what fragment.club did until
  the cut); Iroh, where the computer dials out and
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
