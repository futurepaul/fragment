# Billing, organizations and the operator's admin

Status: **decided, not built** (Paul, 2026-10-08: "I agree with your
recommendations, please proceed"). This is the design of record, as
docs/ledger.md is for the ledger; its decisions are 51 to 59 in
docs/cloudflare-v1.md ("Decided", below, maps them). It stands on
decisions 45 to 50 (people are npubs found by email). The build follows
"Build order"; each slice updates its section here as it lands.

Paul (2026-10-08): "before we go live, we'll need stripe billing.
guests signups will be free, but to be able to have your own agents and
fragments you need a paid seat. $100 for a sleeping agent without
simplex, $200 for an up-to-always-on agent." Trials as finite-mono has
them (a card first). Organizations "where an admin can have central
billing for their org and add and upgrade seats (and remove seats)". An
admin dashboard "that lets us list all our users, invite new users,
create trial codes, etc." The Stripe account is finite-mono's.

## What is there today

- **Plans exist; nothing sells them.** `guest`, `seat` ($100: $50 of
  credit a month, a computer that sleeps) and `seat_always_on` ($200:
  $100 of credit, an always-on computer, its awake time not metered) are
  decision 25 and `crates/proto/src/ledger.rs`. A plan lives only in
  the person's `Ledger` Durable Object. It is set by
  `FRAGMENT_DEFAULT_PLAN` at the ledger's birth, or by an operator
  (`POST /api/ledger/<person>/plan|seat|grant`; the CLI has only
  `fragment ledger grant`).
- **The ledger left Stripe two hooks** (docs/ledger.md, "Where Stripe
  plugs in"): a payment becomes `GrantCredit {id: <payment id>, by:
  "stripe"}`, and a subscription becomes `SetSeat`/`SetPlan`.
- **A guest creates nothing.** Every create asks the maker's ledger
  (`may_spend(create)`, 403 `guests can't create fragments`), and a
  guest's ledger refuses wakes and meter batches.
- **Always-on is built but unwired.** The computer's state machine has
  `Event::AlwaysOn` (never sleeps, never meters awake time), but only a
  test lever sets it; docs/api.md: "the plan itself does not reach a
  computer yet".
- **SimpleX is not built** (parked 2026-10-05; decision 32 puts it on
  always-on computers only). So "$100 without SimpleX" asks nothing of
  billing: SimpleX needs an always-on computer, which only `seat_always_on`
  has.
- **No organizations**, no seat invites, no operator listing of people,
  no admin page, no mail. The registry is one Durable Object holding
  identities, keys, sign-ins and usernames. Operators are the config's
  `operators`. The wipe is their only page-free power, signed by the CLI.
- **WorkOS** is sign-in and Pipes only. It holds no organizations, and
  decision 46 keeps it that way: it holds only the login.

## What finite-mono gives us

Its billing is in the Next.js dashboard (`finitecomputer-v2/apps/
dashboard`, the `stripe` npm SDK, API version `2026-04-22.dahlia`); its
Rust Core never talks to Stripe and takes a normalized subscription from
the dashboard. Worth taking:

- **The webhook refetches.** It verifies the signature, then retrieves
  the subscription and never trusts the event's copy. An event older
  than the last one applied to the same subscription changes nothing.
  It handles five events: `checkout.session.completed` and `.expired`,
  `customer.subscription.created`, `.updated` and `.deleted`.
- **Trials are its own codes on Stripe's trial**
  (`src/lib/trial-checkout.ts`, Core's `trials.rs`).
  - **Codes:** `XXXX-XXXX-XXXX-XXXX`, 16 base32 characters with no
    O/I/0/1, read case-insensitively.
  - **Campaigns:** each has a capacity of 1 to 10,000 and trial days
    from 1 to 30. An admin creates a campaign, edits its code and
    raises its capacity, each change compare-and-set.
  - **Checkout:** `trial_period_days`, `payment_method_collection:
    "always"`, promotion codes off, and an expiry of 31 minutes.
  - **Holding a place:** the place is reserved before the person sees
    the Checkout link, and freed by `checkout.session.expired`.
  - **Who may trial:** one trial per account, and only an account that
    never had a subscription.
- **Who may create:** `active` and `trialing` grant creation. Any
  other status blocks new creation but keeps a paid runtime running.
  A lapsed trial's runtime is stopped and restored on payment.
- **Checkout's extras:** automatic tax is on (Product tax code
  `txcd_10103001`, price tax-exclusive), with `customer_update.address:
  auto`.
- **Billing portal:** the default one, for card, invoices and cancel
  at period end.
- **A readiness audit** (`scripts/stripe-production-readiness.ts`):
  read-only, it checks the Product, the Price, tax, the portal and the
  webhook destination's exact event set before Checkout goes live.
- **Its tests:** SDK mocks, signed test webhooks, and test-clock
  harnesses run by hand against a sandbox. There is no fake server.
- **Admin:** a Next.js page, gated by membership in one internal WorkOS
  organization. It has Users (only those with a runtime, no pagination),
  Launch Codes (no-payment codes handed out by hand), Free trials, and
  an audit table it writes but never shows. There is no impersonation
  and no billing view.
- **No organizations:** one "personal org" per user, quantity always 1,
  no teams. Our organizations are new design.

## The model

### An org is the billing account

- **Every seat belongs to an org.** A person who pays for themselves
  pays through an org of one, made at their first Checkout and named by
  their email. A team is the same thing with more seats. One code path,
  whatever the size.
- **People in an org** are its admins and its seat holders, by npub.
  - An admin manages seats and billing, and need not hold a seat (a
    finance person can be a guest admin).
  - An org keeps at least one admin.
  - A person is in at most one org, and holds at most one seat.
- **A seat** has a kind (`seat` or `seat_always_on`), and either a
  holder (an npub) or a pending email.
  - A pending seat waits on the email as a share invite does
    (decision 48). Whoever signs in with that verified email takes it.
    The platform mails the link through Cloudflare Email Sending.
  - A person who already holds a seat is asked in the shell whether to
    move. Their old seat goes, prorated.
- **A seat is `paid` or `comped`.**
  - Paid seats are counted on the org's one Stripe subscription.
  - Comped seats are an operator's, outside Stripe: the team, friends,
    and every seat before Stripe ships. Only operators make or end them.
  - This replaces "a seat needs an operator's grant" (decision 49).
- **The org's standing** comes from its subscription's status, which
  is Stripe's:
  - `trialing`, `active` and `past_due` are good. Stripe's retry
    schedule is the grace period.
  - Anything else is lapsed: `canceled`, `unpaid`, `incomplete`,
    `incomplete_expired` and `paused`.
  - The retry schedule and its last step (cancel, or mark unpaid) are
    account-wide settings shared with finite-mono. Treating both final
    states as lapsed makes us indifferent to which one it picks.
  - Comped seats are always good.

### What a seat does

The registry is the one writer of a person's plan. It tells two
objects, in its own order (the ledger's `seq`):

- **The person's ledger:**
  - their plan: `seat` or `seat_always_on` while their seat is good;
  - `canceled` when the seat is lapsed or removed (today's rule:
    agents stop, no wakes, fragments keep serving on what credit is
    left);
  - `guest` for someone who never had a seat.
- **Their computer:** `AlwaysOn` while their plan is
  `seat_always_on` and they have not chosen to let it sleep. "Up to
  always-on": a $200 seat may stay awake. Off otherwise, so a downgrade
  lets it idle to sleep.

A removed, lapsed or downgraded person keeps everything they own. Paying
again restores it. Nothing is deleted by billing; the wipe stays the
operator's.

### Where each fact lives

For docs/MODEL.md's truth map:

| Fact | Where | Written by |
|---|---|---|
| An org, its admins, its seats (holder or pending email, kind, paid or comped) | the registry | org admins and operators, through the API |
| A subscription's status, trial end and period end | Stripe; the registry keeps a copy | the webhook, the Checkout return and a daily reconcile, each by fetching it from Stripe |
| How many paid seats of each kind | the registry's seats; Stripe's item quantities are a copy | every seat change pushes them; the reconcile repairs a drift |
| A person's plan and seat standing | their ledger | the registry alone |
| A computer's always-on | the computer | the registry, from its owner's plan and choice |
| Trial codes and their uses | the registry | operators; redemptions |
| What an operator did | the registry's admin log | every admin route |

The registry is already one Durable Object for the fleet (a debt-ledger
entry). Orgs add small tables and little traffic: seat changes,
webhooks, a daily alarm. Splitting them into an `Org` object per org is
the move if it ever outgrows that, not before.

## Stripe

### Objects

- **Customer:** one per org, made at the org's first Checkout. Its
  metadata: `fragment_org`, `fragment_deployment`. Customers are never
  looked up by email: finite-mono has its own customers with the same
  emails.
- **Products and prices:** two Products ("Fragment seat" and "Fragment
  always-on seat"), so an invoice's lines are clear. Their monthly USD
  Prices are found by lookup key (`fragment_seat_month`,
  `fragment_seat_always_on_month`), so a price change is a new Price
  with the key moved, and no config edit. Tax as finite-mono's: code
  `txcd_10103001`, tax-exclusive, automatic tax on (decision 54).
- **Subscription:** one per org, with an item per seat kind in use.
  Each item's quantity is that kind's paid seats, pending ones included.
  Metadata as the customer's.
- **Checkout** makes the subscription (`mode: subscription`).
  - It carries `client_reference_id` (the org), the metadata, a card
    always, and `allow_promotion_codes` (discounts are Stripe's,
    below).
  - The return URL is `/settings?checkout={CHECKOUT_SESSION_ID}`.
  - After that, seats change through the API, not Checkout.
- **Seat changes** call `POST /v1/subscriptions/<id>` with the items'
  new quantities. That covers adding a seat, removing one, and moving
  one between kinds (an upgrade or a downgrade).
  - Changes are prorated (`create_prorations`) onto the next invoice.
  - Each carries an `Idempotency-Key` (the push's own).
  - The order is: the seat row first, then Stripe. A change queues its
    org (`quantity_syncs`) in its own turn, and the registry's alarm
    pushes the counts read fresh, as plans are pushed: two changes at
    once never leave Stripe a seat short, and a push that fails is tried
    again. A subscription fetched with other counts (a webhook, the
    reconcile) is pushed back to the seats'.
- **Billing portal:** our own portal configuration, never the default
  (finite-mono's readiness audit pins the default). It offers card,
  invoices, billing address and cancel at period end. Subscription
  update stays off, because seats are ours. The shell asks the cell for
  a portal session.
- **Credit packs** (decision 25: "more credit can be bought"; decision
  55). Checkout `mode: payment` for a fixed pack, its session
  id the `GrantCredit` id on the buyer's ledger. An org admin may buy
  one for a member. No usage goes to Stripe: the ledger stays ours, and
  Stripe bills seats and packs.
- **Discounts** are Stripe promotion codes that Paul makes in Stripe's
  dashboard. They need no code of ours beyond `allow_promotion_codes`.
  Trials are ours (below), because they need a capacity and one per
  person.

### Keeping our copy right

The subscription's copy is refreshed three ways, and each fetches the
subscription from Stripe rather than trusting what it was handed:

1. **The Checkout return.** The shell posts the session id. The cell
   retrieves the session, checks it is the caller's org's, and fetches
   the subscription. A buyer has their seat as they land, with no
   polling for a webhook (finite-mono polls up to 90 s).
2. **The webhook**, `POST /api/stripe/webhook`. It verifies
   `Stripe-Signature` itself:
   - HMAC-SHA256 over `<t>.<body>`;
   - a 300 s tolerance;
   - a constant-time compare;
   - any matching `v1`, so a secret can rotate.

   The body is at most 256 KB. An event without our
   `fragment_deployment` is answered 200 and dropped: that covers
   finite-mono's events, and another preview's.
   - It takes `checkout.session.completed` (a subscription to fetch,
     or a pack to grant) and `customer.subscription.created`,
     `.updated` and `.deleted`.
   - An event older than the last one applied to its subscription
     changes nothing.
   - A failure answers 500, and Stripe retries for up to three days.
3. **A daily reconcile**, on the registry's alarm. It refetches every
   org's subscription and pushes quantities that drifted. It logs what
   it fixed, and the admin page shows it.

### The account we share with finite-mono

Checked against finite-mono's code (2026-10-08):

- **Its webhook ignores ours.** It drops subscriptions without its
  Price, and expired sessions without its `finite_trial_*` metadata.
  It acts only on `mode: subscription` sessions, which then fail its
  Price check.
- **We use our own event destination.** Its readiness audit wants an
  exact five-event set on its own destination, found by a unique URL.
  Ours has another URL.
- **We use our own portal configuration**, as above.
- **Our code refuses to change a customer or subscription without our
  metadata.** The restricted key can reach finite-mono's objects; the
  code never does.
- **Settings both products share:** the retry schedule (above), tax
  registrations, the statement descriptor, and Stripe's customer mails
  (their branding is the account's).
- **Dev, previews and the e2e** use a Stripe Sandbox of that account,
  fragment's alone, rather than the shared test mode.

### Code, secrets, config

- **`crates/core/src/billing.rs`** (pure, host-tested):
  - the signature check;
  - status to standing;
  - seat counts to item quantities;
  - trial capacity;
  - code normalization.
- **`cell/src/stripe.rs`:** a small client on the Worker's fetch.
  - Form-encoded, `Stripe-Version: 2026-04-22.dahlia` (finite-mono's).
  - An idempotency key on every POST.
  - Typed errors, and only the fields we read parsed.
  - No SDK: none fits wasm32 worth its weight.
- **Secrets** (`cargo xtask secret set`, Paul's): a restricted key and
  the webhook signing secret, bound by name. They are read only by
  `cell/src/keys.rs`, and `secrets_store.rs`'s cache bound grows by
  two.
  - The key's grants: Customers, Checkout Sessions, Subscriptions and
    portal sessions written; Products and Prices read.
- **The deployment's config** gains `stripe: {key, webhook_secret,
  portal}` (store names and the portal configuration's id). Without
  it, billing is off: seats are comped only. That covers dev, the
  e2e's other sections, and a self-hosted deployment.
- **`cargo xtask stripe check --config <file>`** is finite-mono's
  readiness audit in Rust, read-only. It checks:
  - both Prices by lookup key (amount, USD, monthly, tax behavior);
  - the portal configuration's features and return URL;
  - this deployment's endpoint (enabled, exactly our events, our API
    version).

  `deploy` runs it first and refuses on a miss, as it does for a
  missing secret.
- **`cargo xtask stripe setup --config <file>`** makes what is missing,
  idempotently: Products, Prices, the portal configuration and the
  endpoint. It runs on the sandbox for previews; Paul runs it for live.

### Tests

- **Host:**
  - signatures (valid; wrong secret; stale; two `v1`s; tampered
    body);
  - status mapping;
  - quantity diffs;
  - capacity at the last place.
- **A Stripe fake** in `crates/fakes`, wired as the WorkOS fake is.
  - What it serves: customers, Checkout sessions (a lever completes
    one as a card would), subscriptions with items, and portal
    sessions.
  - Events: it delivers signed events to the node.
  - Levers:
    - a trial's end, `past_due`, `canceled` and `unpaid`;
    - dropping, repeating or reordering an event;
    - sending a foreign event (finite-mono's, or another
      deployment's).
- **An e2e section, `billing`.** Each case below is a check:
  - **Valid:** buy, add, upgrade, downgrade and remove a seat; a
    pending seat claimed at sign-in; a trial; a pack; a comped seat.
  - **Invalid:** a forged or foreign event; a member changing seats; a
    code used twice; a full code; a guest creating; a lapsed person
    waking.
  - **Replay:** an event twice; events out of order.
  - **Restart:**
    - the node dies between Stripe's change and our row, and the
      reconcile repairs it;
    - no webhook at all, and the Checkout return carries it.
- **The hosted lane**, on a preview with the sandbox.
  - It creates subscriptions through the API with `pm_card_visa`,
    since Checkout's page needs a person.
  - A test clock carries a trial past its end and a renewal into a
    decline (`pm_card_chargeCustomerFail`).
  - Each preview registers its own sandbox endpoint (decision 58).

## Trials

- **Codes** are finite-mono's shape, kept in the registry:
  - `trial_codes (id, code, name, kind, days 1–30, capacity 1–10000,
    expires_at, active, created_by)`;
  - `trial_uses (code, person, session, expires_at, subscription)`.
- **Redeeming** (a guest who never had a subscription):
  - **Where:** they type the code in the shell, or open the mailed link
    `/?trial=<code>`.
  - **Checks:** in one transaction, the registry checks the code is
    active and unexpired, that it has a place, and that the person never
    trialed. The places taken are uses with a subscription plus uses
    whose session has not yet expired. It then records the use.
  - **Checkout:** one seat of the code's kind, `trial_period_days`, a
    card always, no promotion codes. It expires in 30 minutes,
    Stripe's least.
  - **Freeing a place:** the use's own expiry frees it. This needs no
    `checkout.session.expired` handler, unlike finite-mono's.
- **At the trial's end** Stripe charges the card. A decline is
  `past_due`, then Stripe's retries, then lapse, as for any seat. As a
  belt to the braces, `trial_settings.end_behavior.missing_payment_method`
  is `cancel`.
- **Who gets one:** one trial per person ever, for a seat of one. Team
  trials are later.

## The operator's admin

- **Who:** the npubs in the deployment's `operators`. The page acts
  through the operator's platform session. The wipe still takes a
  signed CLI call, as today.
- **Where:**
  - `/admin`: a shell path, plain JS like the shell;
  - `/api/admin/*`;
  - `fragment admin …` in the CLI;
  - a row in SPECIAL-CASE-INVENTORY.md (security: it acts on every
    person).
- **People.**
  - **The list:** email, npub, joined, last sign-in, plan, org, seat
    kind, paid or comped, and standing. It searches by email and pages
    by cursor, 100 at a time. It reads the registry alone, in one query.
  - **A person's page** adds their ledger status (usage this month,
    credit), their computer, and their fragments' count.
  - **Its actions:** comp a seat, end a comp, and grant credit (built
    today). The wipe stays in the CLI.
- **Invite.** Give an email and a kind, then either:
  - **comp it:** a pending comped seat in a new org of one;
  - **trial it:** a mail with a trial link.
- **Trial codes.**
  - **Create:** name, kind, days, capacity and expiry, with the code
    generated or typed.
  - **List:** each code's uses (who, when, the subscription's status).
  - **Change:** raise the capacity, deactivate it, or edit the code.
    Each change is compare-and-set.
- **Orgs.**
  - **The list:** name, admins, seats by kind, comped count, Stripe
    status, period end, and a link to the customer in Stripe's
    dashboard.
  - **An org's page:** its seats.
- **Billing health:** the last webhook, the last reconcile, what it
  fixed, and sync errors.
- **The audit log:** every admin write is a row (when, which operator,
  action, target, detail), shown on the page.

## The shell

Settings gains **Billing**, which replaces the read-only Credit section:

- **A guest** sees the two seats and a trial-code field, then
  Checkout. A team sets its seat counts there.
- **A seat holder** sees their seat, its kind, who provides it, and
  this month's usage and credit. On `seat_always_on` it adds "let it
  sleep".
- **An org admin** sees the seats table: email, kind, and pending or
  held. From it they:
  - add a seat by email;
  - change a seat's kind;
  - remove a seat;
  - add an admin;
  - open "Payment and invoices" (Stripe's portal).
- **A lapsed person** sees why, and how to pay again.

The first run starts once a seat is held, so a guest's first run is
"get a seat".

## The API, in sketch

| Route | Who |
|---|---|
| `POST /api/billing/checkout {seats: {seat, seatAlwaysOn}, trialCode?}` | a guest, or an org admin with no subscription |
| `POST /api/billing/sessions/<id>` | the Checkout return |
| `POST /api/billing/portal` | an org admin |
| `POST /api/billing/packs {pack, for?}` | a seat holder, or an org admin for a member |
| `GET /api/org`, `PATCH /api/org` | a member; an admin |
| `POST /api/org/seats {email, kind}`, `PATCH /api/org/seats/<id> {kind}`, `DELETE /api/org/seats/<id>` | an org admin |
| `POST /api/org/admins {email}`, `DELETE /api/org/admins/<npub>` | an org admin |
| `PUT /api/computers/<id>/sleep {allowed}` | the computer's owner on `seat_always_on` |
| `POST /api/stripe/webhook` | Stripe, signed |
| `/api/admin/people`, `/people/<npub>`, `/invites`, `/trial-codes`, `/orgs`, `/health`, `/log` | operators |

### Limits

- Seats per org: 200.
- Admins per org: 10.
- Pending seats mailed per org per day: 50.
- Trial days: 1 to 30.
- A code's capacity: 1 to 10,000.
- Packs: a fixed list.
- A webhook body: 256 KB.
- Signature tolerance: 300 s.
- The reconcile: 100 orgs per alarm run, under the sandbox's rate limit
  (25 calls a second).

## Build order, as draft PRs

1. **This plan** (docs only), stacked on #254.
2. **#254's first and third slices:**
   - identity as an npub, with a verified, unique email;
   - pending invites on an email, claimed at sign-in, and the Cloudflare
     Email Sending mailer.

   Seats reuse both. Its second slice (usernames out) is best landed
   before billing too, since it touches every e2e section.
3. **The ledger's monthly allowance** (approved 2026-10-07). Billing
   needs three things from the ledger:
   - a plan, which sets the month's allowance;
   - whether the seat is in good standing;
   - a credit grant.

   The cut should keep exactly those. With that done, the ledger no
   longer needs `past_due`.
4. **Orgs and seats, without Stripe** (built 2026-10-08, branch
   `claude/orgs-seats`; docs/api.md, Seats and orgs):
   - the registry's tables and the org API;
   - comped seats by operators;
   - pending seats by email;
   - the plan pushed to ledgers, and always-on to computers.

   Launch could run on comped seats from here.
5. **Stripe** (built 2026-10-08: a person's own seat, branch
   `claude/billing-stripe`; an org's admins' seats and the counts pushed
   to Stripe, branch `claude/org-paid-seats`; docs/api.md, Billing):
   - the client, the fake and the config;
   - `xtask stripe check|setup`;
   - Checkout;
   - the return, the webhook and the reconcile;
   - the portal and seat changes;
   - the `billing` e2e section.
6. **Trials** (built 2026-10-08, branch `claude/billing-trials`;
   docs/api.md, Billing and Operators): codes, capacity, Checkout with
   trial days.
7. **Packs:** one $25 pack (decision 55).
8. **The operator's admin:** the API, the CLI, `/admin`, and the audit
   log. The API can start alongside 4.
9. **The shell's Billing page.**
10. **Go live** (Paul's):
    - live Products, Prices, portal and endpoint (`xtask stripe setup`);
    - the restricted key and signing secret (`secret set`);
    - `stripe check` green;
    - WorkOS sign-up on (#254's fourth slice);
    - README and GUIDE no longer saying invite-only;
    - docs/finite-integration.md: its "Billing, orgs, entitlements" row,
      and "organizations with several members" and Stripe leave "Not
      built in fragment, on purpose".

## Decided (Paul, 2026-10-08)

Paul took every recommendation. In docs/cloudflare-v1.md:

| Question | Answer | Decision |
|---|---|---|
| Who may buy a seat | any guest; trials and comps for the rest | 51 |
| Orgs | a self-paying person is an org of one; one org per person; an admin may be seatless | 51 |
| When a pending seat bills | from the invite | 52 |
| Upgrades and downgrades | at once, prorated | 52 |
| Lapse | today's `canceled`; nothing deleted; paying restores; retention later | 53 |
| A removed member's fragments | stay theirs; org-owned fragments later | 53 |
| `past_due` | good standing; Stripe's retries are the grace | 53 |
| Tax | Stripe automatic tax, prices before tax | 54 |
| Packs | one $25 pack, kept until spent | 55 |
| Trials | our codes on Stripe's trial; time-held places | 56 |
| A $200 computer | awake by default, "let it sleep" | 57 |
| Previews | `xtask deploy --branch` registers its sandbox endpoint and uploads its signing secret | 58 |
| The admin page | an operator's browser session; the wipe stays CLI-signed | 59 |
| Annual prices | none; monthly only | 51 |

Still Paul's, before the slices that need them:

- **Stripe sandbox.** A Stripe Sandbox for fragment in finite-mono's
  account, and a restricted key for it in the dev account's store
  (`cargo xtask secret set`). The Stripe slice is built and tested
  against the fake until then.
- **Live mode.** At go-live: `xtask stripe setup` in live mode, the
  live key and signing secret, and WorkOS sign-up on.

## Not in v1

- Annual plans, team trials, and invoices paid by bank transfer.
- Fragments owned by an org.
- WorkOS Organizations and SSO (an enterprise's directory can map onto
  our orgs later).
- Several orgs per person.
- Usage sent to Stripe: the ledger stays the meter.
- Refunds, which stay in Stripe's dashboard.
- Paying with a key (decision 50).
