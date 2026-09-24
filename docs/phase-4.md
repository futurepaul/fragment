# Phase 4: sign-in, identities, and budgets (proposed)

Status: **go from Paul 2026-09-24; slices A and B built the same day**
(below, "as built"). Stopped here for Paul to try signing in locally
before slice C (budgets). The ROADMAP keeps
the acceptance (phase 4, decisions 12, 14, 15); this file keeps the
slices. Every slice follows the rules in `docs/finite-integration.md`
and updates its rows there.

Decided with Paul on 2026-09-24:

- fragment has its own WorkOS environment, sign-up switched off; Paul
  invites people.
- Identity follows finite.computer's BANKS model (FIN-11): grants name
  stable identities; keys stay with their callers (browsers use
  sessions, the CLI and agents sign); an agent's owner can read what the
  agent can read.
- The view link stays: `link` means anyone holding the unguessable link
  is a viewer; `public` means anyone, no link needed. This is already
  how the cell works (`cell/src/serve.rs`); nothing changes.

## Slice A: identities and the registry (no sign-in yet)

- An `Identity` cell per identity (`id:` + 32 hex, opaque): its kind
  (person, agent, fragment), its public keys (added, revoked), its
  sign-in subjects (`(issuer, subject)`, empty until slice B), an agent's
  owner, and the index of its fragments (today's `Principal` cell, now
  per identity). A `Key` cell per public key says which identity holds
  it; a `Subject` cell per `(issuer, subject)` does the same for sign-in.
- The router resolves every verified key to its identity, live, and
  fails with 503 `registry_unavailable` when it cannot (no cache that
  outlives a revocation). The caller carries both: the key it signed
  with and the identity it acts as. Members, owners, invites, the ledger,
  and channel records name identities.
- Keys: add one with proof of possession (the request is signed by an
  active key of the identity, and carries a NIP-98 proof by the new
  key), revoke one on its own, never leave an agent keyless. A revoked
  key is 401 from the next request.
- Agents: creating an agent registers its identity, its first key, and
  its owner in one step (FIN-11's trusted initial registration). A
  fragment grants the agent's owner `viewer` when the owner is not a
  member and a member agent they own can read: read only, never write.
- Until slice B, `fragment login` makes a key and a person for it (on
  every fleet: anyone could sign before, and `FRAGMENT_CREATORS` still
  decides who creates), so the e2e and dev keep working.
- Existing state: hard cut. fragment.club holds only test fragments
  (the e2e's and push-check); they are deleted and re-created after the
  deploy. The rehearsal of a real migration waits for BANKS, where it
  matters (`docs/finite-integration.md`).
- e2e: a replaced key keeps every grant; a revoked key is refused; an
  agent's owner reads what it reads and cannot write through it; the
  registry down is a visible 503, not an allow; everything that passes
  today still passes with identities underneath.

### Slice A as built (2026-09-24)

- **One registry cell, not a cell per identity and per key.** The
  `Registry` cell (`cell/src/registry.rs`) holds identities, every key
  each has held, agents' owners, and (empty until slice B) sign-in
  subjects. One cell makes each key change one transaction (an agent is
  never left keyless; a revoked key never comes back) and costs one hop
  per signed request, where the three-cell shape needed two or a cache.
  It is also the shape BANKS will have: one service. If it ever limits
  throughput, shard it by key; at the friends alpha it does not.
- **Numbers:** a signed request resolved by the registry, NIP-98
  included, 0.9 ms median on a local node (21 requests); an unsigned
  `/healthz` 0.1 ms.
- **The router** resolves every signed request and hands the fragment
  the identity, the key, the kind, and an agent's owner (headers only it
  sets). A key no one registered, or a revoked one, is 401; a registry
  that cannot answer is 503 `registry_unavailable` for every signed
  request.
- **Key proofs:** a NIP-98 event by the new key for the same method and
  URL, with `["p", <signer>]`. Adding a key (`fragment keys rotate`) and
  registering an agent (`fragment agent create`) both use it.
- **Agents:** the agent service answers a proof by the new agent's key;
  the owner's CLI registers the agent with it. The agent service checks
  its owner live (`GET /api/identities/{owner}/keys/{npub}`, signed by
  the agent), so a rotated owner key keeps control and a revoked one
  loses it.
- **Owners read, never act:** an agent's owner reads a fragment the
  agent is in as a `viewer`, queries included (a query is read-only,
  docs/MODEL.md), and gets 403 for a mutation or a job, saying why.
  Capped at `viewer` whatever the agent's role (docs/finite-integration.md,
  rule 5).
- **Registration** was open on every fleet until slice B replaced it.
- **Not registered:** a fragment's own key. It is the principal of the
  fragment's own triggered runs and acts nowhere else yet.
- **e2e:** the `identities` section (55 checks), and every other section
  moved onto identities (members, records, runs, the storage token's
  subject, and what an app sees name identities).
- **Not yet:** sockets opened with a key outlive its revocation (debt
  ledger); fragment.club still runs the phase 3 cell (the hard cut
  happens at slice D's deploy).

## Slice B: sign-in and sessions

- WorkOS AuthKit on the platform origin: `/auth/login` →
  WorkOS → `/auth/callback` exchanges the code server-side and finds or
  creates the person for `(issuer, subject)`. Email is stored as an
  attribute and never matched. A session cookie on `fragment.club`
  (HttpOnly, Secure, SameSite=Lax), revocable, with an expiry.
- Each fragment origin gets its own cookie through a single-use,
  short-lived redemption URL minted by the platform for that fragment
  and person (finite-sites ADR 0025). Every request re-checks the
  membership; removing a member closes their session's sockets.
- `fragment login` becomes a browser sign-in: the CLI makes its key,
  opens `fragment.club/cli?key=…`, the signed-in person approves, and
  the CLI's next request (signed by the new key: proof of possession)
  adds the key to the person. A printed URL covers headless machines.
- Creating a fragment needs a person; `FRAGMENT_CREATORS` goes.
- Invites are accepted in the browser; templates stop needing `public`
  operations for their own members.
- A WorkOS fake in `crates/fakes` (authorize, code exchange, a scripted
  user) drives the e2e; `cargo xtask dev` uses it by default.
- e2e: the ROADMAP's list (CLI and browser decide identically across
  public, link, and members; no session; not invited; revoked member;
  cross-fragment cookie; session restart), plus: an email change keeps
  the identity; a second subject can be linked to the same person.

### Slice B as built (2026-09-24)

- **Sign-in** (`cell/src/auth.rs`, `cell/src/registry/signin.rs`):
  `/auth/login` → WorkOS AuthKit → `/auth/callback`, the code exchanged
  server-side, the state bound to the browser by a cookie. A person is
  keyed by `(workos:<client id>, WorkOS user id)`; the email is an
  attribute, refreshed each sign-in, never matched. `/auth/link` adds a
  second sign-in to the signed-in person (explicit; never by email).
  `POST /auth/logout` ends the session, every fragment session made from
  it, and WorkOS's (the browser is sent to its logout URL).
- **Sessions** live in the registry cell (SHA-256 of 32 random bytes),
  30 days, looked up live on every request. A fragment origin gets its own
  cookie only through a 60-second, single-use redemption the platform
  mints for that fragment (finite-sites ADR 0025); shown to another
  fragment it is refused and stays unspent; a cookie from one fragment is
  nobody on another.
- **The CLI:** `fragment login` makes a key and opens
  `<host>/cli?key=<npub>`; the signed-in person compares the key's last
  eight characters with the terminal's and approves; the CLI's claim
  (signed by the key: the proof of possession) joins it to them.
  `--no-wait` prints the link for a headless machine. A person who signs
  in may revoke their last key; an agent keeps one.
- **People make fragments**; an agent is 403. `FRAGMENT_CREATORS` is gone
  (sign-up is off in WorkOS, so everyone who signs in was invited).
- **Invites in the browser:** `__join?invite=` on the fragment's origin,
  a Join button (a form posting back; SameSite=Lax keeps it same-site);
  `fragment invite create` prints the link.
- **The WorkOS fake** (`crates/fakes/src/workos.rs`): authorize (a form
  for a person at a browser, `login_hint` for tests), the code exchange,
  logout. `cargo xtask dev` runs it on :8794, or uses a real environment
  when `WORKOS_CLIENT_ID_FILE` and `WORKOS_API_KEY_FILE` name its files.
- **e2e:** the `signin` section (42 checks, among them 36 decisions where
  a browser with a session and the CLI signing the same request must
  agree, across public, link, and members); every person in every other
  section now signs in through the fake and approves a key.
- **Not built:** the platform API with a browser session (the desktop,
  phase 6, will need it; the CLI and agents sign); a sign-in page of our
  own (AuthKit's hosted page is used).

### Trying it locally (Paul)

```
cargo xtask dev
FRAGMENT_HOST=http://127.0.0.1:8790 fragment login
```

The login opens http://127.0.0.1:8790/cli?key=…: sign in on the fake's
page with any email, compare the key ending, approve; the terminal
finishes. Then `cargo xtask try chat`, open the link, and use "sign in" on
the fragment (`__signin`). With your real WorkOS environment instead of
the fake: add `http://127.0.0.1:8790/auth/callback` to its redirect URIs,
then run dev with `WORKOS_CLIENT_ID_FILE=~/.config/finite-next/secrets/fragment-workos-client-id`
and `WORKOS_API_KEY_FILE=~/.config/finite-next/secrets/fragment-workos-api-key`.

## Slice C: budgets

- The ledger lives in the billing org's cell; for now each person's own
  personal org (FIN-10 keeps personal orgs; an agent's billing org and
  its owner stay separate facts).
- `job.ai` steps reserve their worst case and settle to the reported
  cost; a step that does not fit is held ("budget used up") and replays
  after a top-up; the monthly reset on the 1st (UTC).
- Usage rows carry FIN-10's report fields (source reference, agent,
  billing org, period, unit, quantity) and are recorded idempotently.
- Per-person OpenRouter keys with a credit limit, minted with a
  management key; `fragment budget`; costs on runs; the platform bar.
- e2e: the ROADMAP's budget list, against the OpenRouter fake.

## Slice D: hosted

- WorkOS configured for fragment.club, the fleet redeployed (Paul
  approves the deploy), the test fragments re-created, the first friend
  invited. The hosted e2e signs in with an enrolled CLI key; Paul checks
  the browser sign-in himself.

## What Paul sets up (I can't create accounts)

1. **Before slice D (slices A–C run on the fake):** a WorkOS account
   for fragment with AuthKit. In its Production environment switch
   sign-up off, add the redirect URI `https://fragment.club/auth/callback`
   and the logout redirect `https://fragment.club/`. Save the API key to
   `~/.config/finite-next/secrets/fragment-workos-api-key` and the client
   ID to `…/fragment-workos-client-id`. A Staging environment with
   `http://127.0.0.1:8790/auth/callback` lets you try the real sign-in
   locally. AuthKit is free for the first million monthly active users
   (workos.com/pricing, checked 2026-09-24).
2. **Before slice C goes live:** an OpenRouter management key (it mints
   the per-person keys), saved to
   `~/.config/finite-next/secrets/openrouter-management-key`. Exposure
   stays at most $20 a month per invited person (decision 14).

## Numbers

- One extra cell hop per signed request (key → identity), with no cache
  by rule 7; slice A measures it before building on it.
- WorkOS: $0 at our size. OpenRouter: at most $20 per person per month.
- Size: A and B each about the size of phase 2's slice B; C about slice
  F's AI work; D a deploy and a checklist.
