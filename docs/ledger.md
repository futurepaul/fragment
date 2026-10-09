# The usage ledger

Status: **built** (phase 3 of docs/cloudflare-v1.md, decisions 24–28 and
36–37; spike S4). The pure core is `crates/core/src/ledger.rs` (the state
machine) and `crates/core/src/price.rs` (the price book), with the public
wire types in `crates/proto/src/ledger.rs`. The cell runs it: the
`Ledger` Durable Object (`cell/src/ledger.rs`), the model route
(`cell/src/models.rs`), AI steps (`cell/src/ai.rs`), and each fragment's
meters and write gate (`cell/src/meter.rs`); docs/api.md (Models, Ledger)
is the wire contract. It replaced the OpenRouter-backed ledger and
`crates/core/src/budget.rs` (a hard cut).

## The model

- **One ledger per payer.** A payer is a person. The caller decides who
  pays a row and sends it to that person's ledger, which keys on nothing
  else (decision 36):
  - an agent's model calls, operator-key calls and compute bill the
    agent's owner;
  - a fragment's hosting (requests, storage, dynamic workers,
    screenshots) and its AI steps bill the fragment's owner.
- **Guests pay for nothing.** A guest's ledger refuses reservations and
  meter batches (`guest_payer`). A guest owns nothing billable, so a
  guest makes no fragment either (`guest_creates`, 403: "guests can't
  create fragments"; Paul, 2026-10-03): every create asks its maker's
  ledger first (`may_spend(create)`). A guest still edits the fragments
  shared with them, whose writes bill those fragments' owners.
- **Plans** (decision 25): `guest`; `seat` ($100 a month, $50 of credit
  included, a computer that sleeps); `seat_always_on` ($200 a month,
  $100 included, an always-on computer whose awake time is not metered).
  Stripe charges the seat itself; the ledger only knows the credit.
- **Seat state** comes from a hook: `active`, `past_due`, `canceled`.
  - A seat gets its included credit only while `active`.
  - `past_due` keeps this month's credit. The next month's arrives when
    the seat is active again.
  - `canceled` stops agents. Fragments keep serving and writing on
    whatever credit is left.
  - Hooks arrive out of order, so each carries `seq` (the hook's own
    order). A change older than the last applied changes nothing.
- **Credit.**
  - Included credit arrives at the start of each UTC month (lazily: the
    first mutation of the month, and every read, see it). It expires at
    the month's end. Skipped months grant nothing.
  - A seat that becomes active, or a plan that grows, mid-month gets the
    difference, once. Nothing is taken back when a plan shrinks.
  - Purchased credit (grants) stays until spent.
  - A charge draws included credit first, then purchased.
  - Included credit that arrives while the person owes pays the debt
    first. So a debt is paid once, and a negative purchased balance
    means the included credit is gone.
- **Caps** (decision 26). Each fragment has a monthly cap on its owner's
  ledger, $5 until the owner sets one. Its spend is every charge on that
  ledger in that fragment this month, plus what is held in it. At the
  cap the fragment is closed to AI steps and agent turns by anyone but
  its owner (and the owner's agents acting for them). A reservation may
  cross the cap; only a fragment already at it is closed. Caps never stop
  writes or wakes. A late row counts in its own month.
- **Zero** (decision 27). At a balance of zero or less, agents stop: no
  turns, no wakes, no AI steps (`agents_stopped`, `no_credit`).
  Fragments keep serving and taking writes.
- **Overdraft and read-only.** At a balance of minus the overdraft ($2
  unless an operator sets another) the person's fragments go read-only.
  They stay read-only until the balance is above zero again. An operator
  who changes the overdraft decides it afresh. Read-only, the person
  makes no new fragment, and their fragments' cron and triggers start no
  runs: each is recorded `blocked`, saying why (Paul, 2026-10-03).
- **Meters always record.** Usage that happened is charged, past zero
  and past the overdraft. A reservation must fit the balance less what is
  held (`credit_short`); a settle larger than its reservation is charged
  in full.
- **Standing** is a pure function, `standing_of(plan, seat, balance,
  read_only)`: `ok`, `agents_stopped {why}` or `read_only {why}`, with
  `why` one of `guest`, `seat_canceled`, `no_credit`, `overdrawn`.

## The price book

A price is micro-dollars per a fixed count of its unit, so Cloudflare's
own prices are exact integers. A row's charge is
`ceil(list × (1 + fee) × (1 + margin))`, computed exactly in 128 bits and
rounded up once per row. The fee is AI Gateway's 5% on credits, so it
applies to AI (tokens and neurons) only. The margin is the operator's,
50% by default. Each row also keeps its list price (for reconciling with
the gateway's log `cost`) and its cost basis (list plus fee).

| Meter (`Usage`) | Unit | Default list price | Source |
|---|---|---|---|
| `tokens` | tokens per model: input (uncached), cached input, cache write, output | per million: Flash $0.15 / $0.03 / $0.15 / $0.50; GLM-5.3 $1.40 / $0.26 / $1.40 / $4.40; DeepSeek V4 Flash (the cheap tier's since 2026-10-09; GLM-5.3 Flash is its fallback) $0.44 / $0.014 / $0.44 / $1.32; Opus 5.5 $4 / $0.20 / $5 / $20 | Workers AI catalog (`/ai/models/search`; DeepSeek's checked against its neurons on 2026-10-09); the AI model catalog page for Opus (S4) |
| `neurons` | thousandths of a neuron | $0.011 per thousand neurons; an image (FLUX.1 [schnell]) is 4.80 neurons a 512×512 tile and 9.60 a step (`fragment_core::media`); a transcription (Whisper large-v3-turbo) is 46.63 neurons a minute of audio, reserved at its bytes read as 16 kbps and settled at the length Whisper heard (`fragment_core::transcribe`) | Workers AI pricing (its image rows for FLUX.1 [schnell], its audio row for whisper-large-v3-turbo, read 2026-10-07); S4 matched it to tokens on every call |
| `awake` | ms, per instance type | `2vcpu-6gib`: $0.064224 an hour | Containers pricing: 6 GiB memory and a 12 GB disk provisioned, plus 5% of 2 vCPU (CPU is billed on active use, which the Computer DO cannot see) |
| `storage` | byte-hours, by class | per GB-month (10^9 bytes × 720 h): R2 $0.015, SQLite $0.20, git $0.015 | R2 and Durable Objects pricing; code.storage publishes no price to us, so git is at R2's |
| `requests` | requests | $0.45 per million | Workers Standard $0.30 plus the Durable Object request $0.15 |
| `dynamic_workers` | unique dynamic workers per UTC day | $0.002 each | Dynamic Workers pricing |
| `browser` | ms of Browser Rendering | $0.09 an hour | Browser Run pricing (developers.cloudflare.com/browser-run/pricing, updated 2026-04-21): $0.09 per browser hour past the 10 a month Workers Paid includes |
| `images` | unique transformations | $0.50 per thousand | Cloudflare Images pricing |
| `key` | calls the provider answered | per call at list: Perplexity $0.005, Google Places $0.035, xAI $0.12, ElevenLabs $0.15 (`micros` per `per` calls), or the price a catalog row names | each vendor's pricing page (`DEFAULT_KEYS` in `price.rs`, each with its source and what an estimate assumes); the book's `keys` are the deployment's catalog's operator rows |

Checked against S4's real numbers: a $0.004 Flash turn is charged
$0.0063; the gateway's own `cost` equals our list price on every call;
neurons and tokens agree within half a micro-dollar on the spike's 17
calls. The defaults are consts in `price.rs`, each with its source. The
operator's book comes from the deploy's configuration and is versioned:
a ledger takes only a newer version, and a hold keeps the price it was
held at. The book's version is the higher of the configuration's
`price_book_version` and the code's (`DEFAULT_BOOK_VERSION`, 3 since
DeepSeek V4 Flash was priced), so a change to the defaults reaches every
ledger at its next call.

A model call that fell back (docs/api.md, Models) keeps its one hold,
reserved at its tier's model's worst case, and is settled from the
fallback's usage at the fallback's prices. The cheap tier's hold, at
DeepSeek V4 Flash's prices (the request's bytes as tokens, the capped
`max_tokens` out), covers its GLM-5.3 Flash fallback's worst case whole.
A fallback dearer than its tier's model (the medium tier's GLM-5.3 is
not) could be charged past its hold, in full, as any usage that
happened is.

## The state machine

Pure: no I/O, no clock (time is an argument). The state has two parts:

- **The head** (`Ledger`): the plan, the seat, the balance's parts, the
  read-only latch, the overdraft, the price book, and the reservations
  held (at most 256). Small and bounded; kept whole.
- **The store** (`trait Store`): what grows with use, by id. References
  (`Entry`), batches, commands, each fragment's spend by month, and caps.
  The Durable Object keeps these in SQLite tables; `Memory` keeps them in
  maps for tests.

A mutation decides before it writes, so a refusal writes nothing. Every
mutation first brings the ledger to its time's month.

| Mutation | Idempotent by | Answer |
|---|---|---|
| `grant(GrantCredit)` | its `id` (Stripe's payment id, or the operator's) | `()` |
| `set_plan(SetPlan)` | its `id` | `()` |
| `set_seat(SetSeat)` | its `id`; ordered by `seq` | `()` |
| `set_overdraft(SetOverdraft)` | its `id` | `()` |
| `set_cap(SetFragmentCap)` | its `id` | `()` |
| `set_book(SetPriceBook)` | its `id`; ordered by `version` | `()` |
| `reserve(Reserve)` | the call's `ref` | `held {amount}`, `settled {charge}` or `released` |
| `settle(Settle)` | the call's `ref` | `{charge, basis}`: `usage`, `reservation` or `expired` |
| `release(Release)` | the call's `ref` | `back`, or `settled {charge}` |
| `meter(Meter)` | the batch's id; each row by its `ref` | `{charged, rows_new, rows_before, refused: [{index, why}]}` |
| `sweep(now)` | (time) | `{expired: [{ref, charge}], forgotten}` |

- **Replays.** The same id again answers as the first did and changes
  nothing. The same id with another body is refused
  (`conflicting_body`). A refusal is not remembered, so the same id may
  be tried again (a step after a top-up).
- **A replayed reservation answers where its reference stands.** While
  held that is its first answer. Once settled it is `settled {charge}`,
  so a retried step learns its call was paid and never makes it again
  (bug 2).
- **References are one namespace.** A meter row cannot reuse a
  reservation's reference, and the reverse. Sources prefix theirs
  (`aig:`, `step:`, `awake:`), so two sources never collide.
- **Settles.** A settle without usage, or with a usage the book cannot
  price, is charged its reservation: the money path fails closed. A
  settle after a release is refused (`ended`): a call that used anything
  settles; only one that used nothing is released (bug 3).
- **Sweeps** run from the Durable Object's alarm at `next_sweep_ms`.
  A hold older than 6 hours is settled at its reservation (its caller
  died, and whether it used anything is unknown). References and batches
  older than 90 days are forgotten, and a row from before that is
  refused as `stale`, so a forgotten reference is never charged again.
  Commands are never forgotten.
- **Reads** (`standing`, `gate`, `fragment_open`, `may_spend`, `status`)
  roll a copy of the head to their own month and change nothing. A
  spend is an agent's turn, an AI step, a wake, a write, or a create (a
  fragment made, a write to one of the maker's own: refused for a guest
  as `guest_creates`, otherwise as a write is).
  `may_spend(spend, fragment, by_owner)` is the whole question when the
  payer owns the fragment (or there is none). When another person owns
  it, that owner's ledger also answers `fragment_open`.

Every refusal is a typed `Refused`, with `code()` (the platform's
`ErrorCode`: every money refusal is 402 `budget_used_up`) and `message()`
for people.

## Limits

| Limit | Value | Why |
|---|---|---|
| `BATCH_ROWS_MAX` | 1,000 rows | one request and one transaction, far under the body limit |
| `RESERVATION_MAX` | $25 | the dearest call the tiers allow is about $10 |
| `HOLDS_MAX` | 256 | a computer's agents and a person's jobs hold a few dozen; past this, something leaks |
| `GRANT_MAX` | $10,000 | more is a typo |
| `CAP_MAX` | $10,000 | more is no cap |
| `OVERDRAFT_MAX` | $1,000 | someone trusted with more is someone to invoice |
| `price::CHARGE_MAX` | $100,000 a row | no call or sample costs that; a row that would is a bug upstream |
| `price::QUANTITY_MAX` | 10^15 per quantity | keeps the 128-bit math from overflowing |
| `ID_MAX_BYTES` | 256, printable ASCII | ids and names |
| `HOLD_MAX_MS` | 6 hours | no call holds for hours |
| `REF_KEEP_MS` | 90 days | the audit retention; sources retry within hours |
| `CLOCK_SKEW_MS` | 5 minutes | a row's source runs on its own clock |

## The Ledger Durable Object

One `Ledger` DO per person, named by the person's identity (cell/src/ledger.rs).
Each route is one `transactionSync` over the head and the store (a
`js.rs` helper: workers-rs 0.8.5 has none). The bodies are the core's
types (inner) or proto's (public); answers are JSON of the answer types,
and refusals are `ErrorBody {error: code(), message: message()}` with the
typed `refused` beside it, so a caller tells a read-only owner from a
guest. A statement that fails marks the store at fault, and the route
rolls back whole. A new person's ledger starts as the core makes it (a
guest), then takes `FRAGMENT_DEFAULT_PLAN` under `plan:default`.

Tables:

- `head`: the head (`Ledger`), kept whole as one JSON row: small and
  bounded (at most 256 holds), checked as it is read back;
- `entries (ref PRIMARY KEY, kind, at_ms, …)`, `batches (id PRIMARY KEY,
  digest, answer, at_ms)`, `commands (id PRIMARY KEY, command, at_ms)`;
- `spend (month, fragment, micros, PRIMARY KEY (month, fragment))`,
  `caps (fragment PRIMARY KEY, micros)`.

Inner routes (platform code only, through the DO's stub):

| Route | Body | Answer |
|---|---|---|
| `POST /reserve` | `Reserve` | `Reserved` |
| `POST /settle` | `Settle` | `Settled` |
| `POST /release` | `Release` | `Released` |
| `POST /meter` | `Meter` | `Metered` |
| `POST /may-spend` | `{spend, fragment, byOwner}` | `{}` or the refusal |
| `POST /fragment-open` | `{fragment}` | `{}` or `cap_reached` |
| `POST /grant`, `/plan`, `/seat`, `/overdraft`, `/cap` | proto's `GrantCredit`, `SetPlan`, `SetSeat`, `SetOverdraft`, `SetFragmentCap` | `{}` |
| `POST /status` | `{}` | proto's `LedgerStatus` |
| `POST /test` | `{op: clock {offsetMs} \| sweep \| entries {prefix} \| totals}` | test fleets only |

Before each route the DO compares its book's version with the
configured one and applies `SetPriceBook {id: "book:<version>"}` when the
configuration is newer (`configured_book`: the core's defaults until the
deployment's configuration carries one; the debt ledger). Its alarm runs
`sweep` at `next_sweep_ms`.

Public routes (a hard cut of `/api/budget`; docs/api.md, Ledger):

- `GET /api/ledger` (the person; an agent reads its owner's) →
  `LedgerStatus`; `fragment ledger [--json]`.
- `PUT /api/f/<name>/cap {id, micros}` (the fragment's owner; kept as
  `cap:<fragment>:<id>`); `fragment cap <name> <usd>|default`.
- Operators: `POST /api/ledger/<user>/grant|plan|seat|overdraft` with
  proto's bodies, their ids kept as `<kind>:<id>`; `fragment ledger
  grant <user> <usd> --why <text>`.

## How each meter reaches it

Each source keeps an outbox in its own SQLite and flushes one batch per
payer through the ledger Queue, under a batch id of its own
(`<source>:<seq>`). A crash between the ledger's apply and the outbox's
mark sends the same batch again, which answers as before.

- **Model calls** (the computer's model intercept, lesson 7). Before the
  call it reserves a worst case (tokens in from the body's size, out at
  the tier's capped `max_tokens`) under the intercept's own call id. It
  streams, takes only the last, cumulative usage (S4), and settles with
  it. A call that failed before any token is released. A call whose
  stream broke settles without usage, at its worst case. The gateway's
  log id rides along for reconciliation; the gateway's per-user spend
  rule is only a backstop. The payer is the agent's owner; the fragment
  is the agent's.
- **AI steps** (a job's `ai.*`, voice input). The step's reference
  (`step:<fragment>@<incarnation>/run/<run>/attempt/<attempt>/step/<index>`:
  a replay is a new attempt, so a step released before reserves again) is
  reserved before the call; what the call bought is kept in the Fragment
  DO beside the step (by its key without the attempt, so a replay finds
  it), and then it is settled. A retried step that finds what it kept
  reuses it and never calls the model again (bug 2); one whose
  reservation answers `settled` with nothing kept (the node died between
  the two) fails rather than buy again. A final failure releases (bug 3),
  as does a step whose retries ran out and any hold of a run that ended.
  A text step settles its tokens; an image settles `neurons` from the
  JPEG it got (its tiles, read from its header) and its steps, having
  reserved a 1024×1024 image's. The payer is the fragment's owner,
  `capped` when the run's principal is neither the owner nor an agent of
  theirs.
- **The in-fragment agent's turns**: as model calls, the fragment's owner
  paying.
- **Operator keys** (decision 37): reserved and settled the same way as
  model calls. Before the call, the intercept reserves one `key` call at
  its price (`key:<computer>:<12 hex>`, as the agent, on its owner's
  ledger). A refusal, or a ledger that does not answer, refuses the call:
  a key is the operator's money. Once the provider answered (under 500),
  the call settles at that one call, and the computer counts it with its
  charge (docs/computers.md). A call the provider did not answer is
  released. A settle or release that does not land is charged by the
  sweep.
- **Not metered: a person's own keys** (decisions 37 and 60). Their own
  key's calls, an agent's model calls on their own OpenRouter account
  among them, are paid by their account at the provider. The computer
  counts them by agent and month (`uses`, charge 0), and nothing reaches
  the ledger: no reservation, no charge, no platform fee (Paul,
  2026-10-08).
- **Compute** (the Computer DO). It meters each awake interval at sleep,
  and every few minutes while awake, as `awake {instance, ms}` rows
  (`awake:<computer>:<from ms>`). The payer is the computer's owner. The
  ledger waives an always-on seat's awake time; no wake starts when
  `may_spend(wake)` refuses. The time a computer is kept for its failed
  saves (a sleep's failed save to `computers.unsaved_max_ms` after it) is
  never metered: the platform's, not its owner's (Paul, 2026-10-08;
  docs/computers.md, "What its owner is told").
- **Storage** (the Fragment DO's alarm, daily). It samples the fragment's
  own SQLite and its blobs, and meters `bytes × hours since the last
  sample` (`store:<fragment>@<incarnation>:<class>:<at>`) to the
  fragment's owner. Its app facet's database and its git repository are
  not sampled (the debt ledger). A computer's backups bill the computer's
  owner.
- **Requests** (the Fragment DO, which sees each one the router hands
  it): a row per minute (`req:<fragment>@<incarnation>:<minute>`) to the
  fragment's owner, closed once the minute has passed.
- **Dynamic workers** (the Fragment DO): one row the first time a code
  version loads on a UTC day (`dw:<fragment>@<incarnation>:<version>:<day>`).

The fragment's three meters share its outbox (`meter_rows`), flushed by
its alarm one batch at a time (at most 200 rows, under the queue's 128 KB
message) as `frag:<fragment>@<incarnation>:<n>`; the queue's consumer
applies it and tells the fragment, which forgets it, and one not
acknowledged within five minutes is sent again. A guest's ledger refuses
the batch, so a guest's fragments are billed nothing.

- **Preview cards** (decision 31; docs/api.md, Cards): each try at a
  deploy's card is a `browser` row in the fragment's outbox
  (`card:<fragment>@<incarnation>:<live, 12 hex>:<wanted at>:<try>`),
  its session's time from acquire to close (a close that was lost adds
  the session's 10 s inactivity timeout), to the fragment's owner. A
  stale shot and a failed one are billed too: their browser time was
  spent. A guest's or a read-only owner's deploys are not shot.
- **Images** (the deploy path): `images` rows to the fragment's owner,
  when a transformation is made.

## Where Stripe plugs in

- **Credit:** a paid credit pack's Checkout becomes `GrantCredit {id:
  "pack:<Checkout's id>", by: "stripe"}` (docs/billing.md, decision 55).
  Its id may come again any time (its webhook, its return); commands are
  never forgotten.
- **A seat's state:** the registry pushes `SetPlan` (the seat's kind)
  and `SetSeat` (active while the seat is good, else canceled) in its own
  `seq`, from the seat and its org's subscription as fetched from Stripe
  (docs/billing.md, "What a seat does").

## Assumptions (Paul's to confirm)

- `past_due` keeps agents running on this month's credit, and gets no new
  month's credit until active again.
- A canceled seat keeps paying for its fragments' hosting from what is
  left; `guest` is for people who never had a seat.
- Mid-month upgrades grant the difference at once; downgrades take
  nothing back.
- A debt is paid from the next month's included credit first.
- A cap counts every charge in the fragment on its owner's ledger,
  the owner's own agents' included. Only others are stopped by it.
- An agent's own usage cap (decision 14) is not built: the same
  mechanism, keyed by the agent's fragment, would carry it.
- An abandoned hold is charged its worst case after 6 hours.
- The default instance rate assumes a 12 GB disk and 5% CPU; git storage
  is priced at R2's until code.storage's price is known.
- A card's shot is a Browser Session, which Cloudflare also bills by
  concurrent browsers ($2.00 a month for each past 10, the month's
  average of each day's peak). That is a deployment-wide charge no one
  shot causes, so it is not metered per person; the margin carries it
  (each fragment has at most one shot out: its alarm takes it).
