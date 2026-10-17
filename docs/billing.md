# Billing, organizations and the operator's admin

Status: **decided, and built through slice 9** (Paul, 2026-10-08: "I
agree with your recommendations, please proceed"). "Build order" marks
slices 4 to 9 built on 2026-10-08 (cell/src/billing.rs, stripe.rs,
orgs.rs, registry/admin.rs, cell/shell/billing.js); going live, slice 10,
is Paul's. This is the design of record, as
docs/ledger.md is for the ledger; its decisions are 51 to 59 in
docs/cloudflare-v1.md ("Decided", below, maps them). It stands on
decisions 45 to 50 (people are npubs found by email). The build follows
"Build order"; each slice updates its section here as it lands.

**What is sold** (Paul, 2026-10-15: "for stripe, let's just do the $200
plan and the $25 credit pack"): one seat, `seat_always_on` ($200 a
month, an always-on computer, $100 of credit included), and the $25
credit pack. The $100 seat is no longer sold: Checkout, an org admin's
seats and trial codes take only `seat_always_on`, and refuse a `seat`
(400, "not sold"); `xtask stripe setup` never makes its Product or Price,
and `check` never asks for them. The `seat` plan stays, for what does
not sell it: operators' comps, and a deployment's `default_plan` (dev and
the e2e make their people seats with it). Decision 25's status records
it. Below, a "kind" a buyer chooses is that one kind.

**Nobody joins anything without their own yes** (Paul, 2026-10-17, from
the security audit's H1: "keep teams, but nobody joins an org without
accepting"). An org is made only once a Checkout is paid; everything an
org gives someone (a seat, an admin's place) is an offer, theirs once
they accept it; signing in attaches nothing; and the card's owner always
manages what it pays for. A wipe cancels what the wiped person would pay
(the audit's M4): "cancel the subscription immediately, keep the Stripe
customer" (invoices for tax and refunds) and drop the platform's link to
it. "The model" and "A wipe", below, say how; "Build order", slice 11.

Paul (2026-10-08): "before we go live, we'll need stripe billing.
guests signups will be free, but to be able to have your own agents and
fragments you need a paid seat. $100 for a sleeping agent without
simplex, $200 for an up-to-always-on agent." Trials as finite-mono has
them (a card first). Organizations "where an admin can have central
billing for their org and add and upgrade seats (and remove seats)". An
admin dashboard "that lets us list all our users, invite new users,
create trial codes, etc." The Stripe account is finite-mono's.

## What was there before it (2026-10-08)

The starting point the slices were built from, kept as the record of what
they changed; "Build order" says what is there now.

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
  pays through an org of one, named by their email. It is made when
  their first Checkout is paid, never before: a Checkout begun and left
  makes no org and no Stripe customer. A team is the same thing with more
  seats. One code path, whatever the size. Only a paid Checkout and an
  operator's comp make an org.
- **People in an org** are its admins and its seat holders, by npub, each
  in it by their own yes: an offer they accepted, their own paid
  Checkout, or an operator's comp of them (operators are trusted). One
  function writes a row's person (registry/offers.rs `enroll`), and each
  caller names the yes it acts on.
  - An admin manages seats and billing, and need not hold a seat (a
    finance person can be a guest admin).
  - An org keeps at least one admin who said yes (an admin's place
    offered is not one).
  - A person is in at most one org, and holds at most one seat.
- **A seat** has a kind, and either a holder (an npub) or an offer to an
  email. A paid seat's kind is `seat_always_on`, the one sold; a comped
  one may also be `seat` (a computer that sleeps).
- **Offers.** An org's admin offers a seat, an admin's place, or both,
  to an email, while the org pays (an org that pays for nothing offers
  nothing: it buys again first).
  - It waits on the email as a share invite does (decision 48), but
    nothing of it is anyone's until whoever signs in with that email,
    verified, accepts it; they may decline it. Signing in attaches
    nothing: before 2026-10-17 a sign-in took the oldest row offered to
    its email, an admin's place included, which let anyone enroll anyone
    (the audit's H1).
  - The platform mails it through Cloudflare Email Sending, saying who
    offered it and where to answer (`/settings`; a guest's welcome shows
    it too). The person sees every offer to an email of theirs, its
    org's name and who made it (`GET /api/seat`'s `offered`).
  - One of the org's own members given a seat has it at once: they are
    in the org by their yes already. An admin's place is power over the
    org, so it is offered even to one of its members (its accept joins
    their row: the review of #367).
  - An admin's offer is accepted only while its org pays: one that
    lapsed buys again before anyone takes its offers (an operator's comp
    stands alone).
  - A paid seat offered bills from the offer (decision 52).
- **A move.** Accepting an offer while in another org moves them: their
  old row goes, its seat with it (prorated). Alone there (offers are no
  one: they do not count), that org ends, every offer it made
  withdrawn, its subscription canceled at once. Their going is refused
  only when they are the last admin of an org other people are in (no
  one would manage it: `fragment_core::org::moving`).
- **Leaving.** A seat's holder who is no admin leaves by their own yes
  (`POST /api/seat/leave`): their seat goes, prorated. An admin steps
  down first.
- **Who pays** is the person whose Checkout made the org's subscription
  (the registry's `org_payers`), while it has not ended (one set to
  cancel at its period's end still counts: it may be renewed).
  - No other admin removes them as an admin, or takes their seat (403).
  - **Only they open its billing** (Stripe's portal, which renews the
    subscription and changes its card and email); its other admins
    manage its seats in the shell, never its billing. This is the
    default the review of #367 set, Paul's to reverse.
  - As they go (stepping down, leaving, moving, or wiped), the
    subscription is canceled at once, its pending prorations deleted
    (below), and once that lands the org forgets their customer (kept at
    Stripe for its invoices): no one left in it opens that customer's
    portal or buys on it.
  - Another admin buys the org again (it pays for nothing now) on a
    customer of their own, and then pays for it.

  So no card pays for an org its owner no longer manages, and no admin
  can give themselves the paid seat and drop the card's owner.
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

The registry is the one writer of a person's plan and seat: an operator
comps or ends a seat through it (the ledger's own `seat` command went on
2026-10-16, its `seq` free to run ahead of the registry's and drop its
pushes). It tells two objects, one queued push a person, one at a time,
each read fresh as it goes, the seat in its own order (the ledger's
`seq`: one older than the ledger's is refused as stale, naming the
ledger's, which the registry's next order passes):

- **The person's ledger:**
  - their plan: `seat` or `seat_always_on` while their seat is good;
  - `canceled` when the seat is lapsed or removed (today's rule:
    agents stop, no wakes, fragments keep serving on what credit is
    left);
  - `guest` for someone who never had a seat.
- **Their computer:** `AlwaysOn` while their plan is
  `seat_always_on` and they have not chosen to let it sleep. "Up to
  always-on": a $200 seat may stay awake. Off otherwise, so a downgrade
  lets it idle to sleep. A push before they have a computer finds none;
  the computer's making asks for a push, and a making that cannot ask
  fails, so it is made again (the same one) and asks again.

A removed, lapsed or downgraded person keeps everything they own. Paying
again restores it. Nothing is deleted by billing; the wipe stays the
operator's.

### A wipe

An operator's wipe (docs/api.md, "Operators: wiping a person") has a
`billing` step, before its last (`fragment_core::wipe::STEPS`). In one
registry turn the person's place in their org goes, and what each org
then asks of Stripe is queued and recorded (`wiped_orgs`), so a call cut
anywhere (a failed cancel, a restart) goes on with it; the step is done
once Stripe has it all (`fragment_core::org::wiped`):

- **Their org ends** when no one else is in it or they are its last
  admin: every row of it goes (the others' seats end, their plans
  pushed; offers withdrawn), its subscription is canceled at once, and
  the org's row is dropped once the cancel lands. Wiping the sole admin
  of an org others hold seats in so ends their seats: no one is left to
  manage or pay for it (they keep everything they own, and buy or are
  offered a seat again). The operator's dry run names them, and so does
  the step's note.
- **An org they pay for** (their Checkout made its subscription, and it
  has not ended) has its subscription canceled at once even when it goes
  on without them, as when a payer goes any other way: its other paid
  seats lapse until its admins buy again (on a customer of their own),
  and once the cancel lands the org forgets the wiped person's customer.
  A wiped person's card is charged nothing more.
- **Any other** goes on without them, its seats' count pushed.

The Stripe customer is never deleted: it keeps the invoices (tax,
refunds, which stay by hand in Stripe's dashboard). The registry's link
to it goes with the org's row, or, an org that goes on, as it forgets
its payer.

### Where each fact lives

For docs/MODEL.md's truth map:

| Fact | Where | Written by |
|---|---|---|
| An org, its admins, its seats (holder or offered email, kind, paid or comped) | the registry | a paid Checkout (an org of one, its buyer), org admins (offers; their own members at once), the people offered (accept, decline, leave) and operators (comps), through the API; a wipe (its `billing` step) |
| Who pays for an org | the registry's `org_payers`: the buyer of its current subscription | the Checkout's completion |
| A subscription's status, trial end and period end | Stripe; the registry keeps a copy | the webhook, the Checkout return and a daily reconcile, each by fetching it from Stripe |
| How many paid seats | the registry's seats; Stripe's item quantity is a copy | every seat change pushes it; the reconcile repairs a drift |
| A person's plan and seat standing | their ledger | the registry alone |
| A computer's always-on | the computer | the registry, from its owner's plan and choice |
| Trial codes and their uses | the registry | operators; redemptions |
| What an operator did | the registry's admin log | every admin route |
| Whether a person is a guest | `GET /api/seat`'s `guest`, from the registry's seat and, holding none, their ledger's plan (`fragment_core::org::guest`) | nothing: read at each ask, never kept |
| What a seat and a pack cost, and the credit a seat includes | core's constants (`fragment_core::stripe`, `ledger::included`); Stripe's Prices must match them | a code change; pages read `GET /api/seat`'s `offer` |

The registry is already one Durable Object for the fleet (a debt-ledger
entry). Orgs add small tables and little traffic: seat changes,
webhooks, a daily alarm. Splitting them into an `Org` object per org is
the move if it ever outgrows that, not before.

## Stripe

### Objects

- **Customer:** one per org, made by Stripe's Checkout when the org's
  first one is paid (`customer_email`, the buyer's), never before; the
  registry keeps the one its fetched subscription names (the only writer
  of `orgs.customer`). An org's customer is its payer's: only its last
  payer buys again on it, anyone else on a customer of their own (which
  the org then keeps), and no Checkout ever names another org's (the
  audit's H1: an org made before payment once put a victim's card on an
  attacker's customer). The org forgets it once its payer goes.
  Customers are never looked up by email: finite-mono has its own
  customers with the same emails. A customer is never deleted: an org
  that ends, or a wipe, keeps it for its invoices and drops the
  registry's link to it.
- **Products and prices:** two Products, the two things sold: "Fragment
  always-on seat", its Price monthly at $200 by the lookup key
  `fragment_seat_always_on_month`, and "Fragment credit, $25", its Price
  once at $25 by `fragment_credit_pack`. Their amounts are core's
  constants (`crates/core/src/stripe.rs`: `SEAT_CENTS`, `PACK_CENTS`),
  the price's truth: Checkout refuses a Price of any other amount
  (cell/src/billing.rs), `xtask stripe check` expects them, and the shell
  shows them from `GET /api/seat`'s `offer`. A price change is a code
  change (the constant, and a new Price with the key moved to it), not a
  config edit. The $100 seat's
  (`fragment_seat_month`) is not made, and a subscription with an item on
  it is not fragment's (2026-10-15). Tax as finite-mono's: code
  `txcd_10103001`, tax-exclusive, automatic tax on (decision 54).
- **Subscription:** one per org, with one item, the seat's. Its quantity
  is the org's paid seats, offered ones included. Its metadata:
  `fragment_org`, `fragment_deployment`.
- **Checkout** makes the subscription (`mode: subscription`).
  - It carries `client_reference_id` (the org: the one its payment will
    make, its id minted and carried in the metadata, or the org buying
    again), the metadata, a card always, and `allow_promotion_codes`
    (discounts are Stripe's, below).
  - It bills `customer_email` (a new org's buyer, or anyone but its last
    payer buying an org again; Checkout makes the customer), or the
    org's own `customer` when its last payer buys it again, for its paid
    seats (`quantity`), so Stripe's page shows the whole. Only an admin
    of their org that pays for nothing (lapsed, its payer gone, or
    comped and ended) buys again in it.
  - Paid, it makes the org, its buyer its admin and its paid seat's
    holder, and who pays (`apply_checkout`). A Checkout paid whose buyer
    can no longer take its seat (they joined another org meanwhile, left
    theirs, are being wiped, or another subscription pays already) seats
    no one, and its subscription is canceled at once.
  - The return URL is the shell's welcome,
    `/welcome?checkout={CHECKOUT_SESSION_ID}`, and Checkout's Back goes
    to `/welcome?checkout=canceled` ("The shell", below). A credit
    pack's are `/settings?pack=…`.
  - After that, seats change through the API, not Checkout.
- **Seat changes** call `POST /v1/subscriptions/<id>` with the item's
  new quantity. That covers adding a seat and removing one; with one
  kind sold, there is no moving between kinds.
  - Changes are prorated (`create_prorations`) onto the next invoice.
  - Each carries an `Idempotency-Key` (the push's own).
  - The order is: the seat row first, then Stripe. A change queues its
    org (`quantity_syncs`) in its own turn, and the registry's alarm
    pushes the counts read fresh, as plans are pushed: two changes at
    once never leave Stripe a seat short, and a push that fails is tried
    again. A subscription fetched with other counts (a webhook, the
    reconcile) is pushed back to the seats'.
- **No paid seat left** (a member left, moved or was wiped, an offer
  declined) cancels the subscription at once (`fragment_core::stripe::
  push`): `DELETE /v1/subscriptions/{id}`, the subscription fetched first
  and never one not this deployment's, no proration (what is owed back
  is a refund by hand in Stripe's dashboard), its customer kept. Then
  the prorations its seat changes left pending on its customer are
  deleted (`GET /v1/invoiceitems?customer=&pending=true`, each of its
  subscription's `DELETE /v1/invoiceitems/{id}`), so the card is charged
  nothing more (Paul, 2026-10-17). Cancels are queued (`stripe_cancels`)
  and tried again as pushes are, so a failed one is not lost; one Stripe
  has no such subscription of is done. An admin's removal of the org's last paid seat
  is still refused: the portal cancels at the period's end, and that is
  the admin's way to stop paying. An org no one is in or offered is
  dropped once nothing of it bills.
- **Billing portal:** our own portal configuration, never the default
  (finite-mono's readiness audit pins the default). It offers card,
  invoices, billing address and cancel at period end. Subscription
  update stays off, because seats are ours. The shell asks the cell for
  a portal session, which only who pays for the org gets (403 to its
  other admins: "Who pays", above).
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
   - An event for an org that is gone (it ended, its subscription
     canceled) is answered 200 and dropped.
   - A live subscription naming an org the registry has no row of (its
     Checkout's completion lost, its buyer's return never made) has the
     Checkout that made it looked up (`GET /v1/checkout/sessions?
     subscription=`) and applied as its completion would be; with none
     to find, it is canceled after an hour's grace, unless an org takes
     it meanwhile, so no card pays for an org that is not there.
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
  - what is sold (`for_sale`: `seat_always_on` only, else `NotSold`);
  - the seat count to the item's quantity;
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
  - The key's grants: Customers, Checkout Sessions, Subscriptions,
    invoice items (a cancel deletes its pending prorations, 2026-10-17:
    Paul's to add to the live key) and portal sessions written; Products
    and Prices read.
- **The deployment's config** gains `stripe: {key, webhook_secret,
  portal}` (store names and the portal configuration's id). Without
  it, billing is off: seats are comped only. That covers dev, the
  e2e's other sections, and a self-hosted deployment.
- **`cargo xtask stripe check --config <file>`** is finite-mono's
  readiness audit in Rust, read-only. It checks:
  - the seat's and the pack's Prices by lookup key (amount, USD,
    monthly or once, tax behavior), and no other;
  - the portal configuration's features and return URL;
  - this deployment's endpoint (enabled, exactly our events, our API
    version).

  `deploy` runs it first and refuses on a miss, as it does for a
  missing secret.
- **`cargo xtask stripe setup --config <file>`** makes what is missing,
  idempotently: the always-on seat's Product and Price, the pack's
  Product and Price, the portal configuration and the endpoint. It runs
  on the sandbox for previews; Paul runs it for live.

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
  - **Valid:** buy (nothing made before the payment), offer and remove a
    seat; an offer accepted, declined, a move, a leave; a 30-day trial; a
    pack; a comped seat.
  - **Invalid:** a forged or foreign event; a member changing seats; a
    `seat` bought, added, or made a trial code's kind; a code used twice;
    a full code; a guest creating; a lapsed person waking; who pays
    removed by another admin; a move that would strand an org.
  - **Replay:** an event twice; events out of order; an offer accepted
    or declined twice.
  - **Restart:**
    - the node dies between Stripe's change and our row, and the
      reconcile repairs it;
    - no webhook at all, and the Checkout return carries it;
    - a wipe's `billing` step cut by a failed cancel and a restart, then
      finished.
  - **The audit's H1**, each of its steps refused (`consent`): no org or
    customer before a payment; an offer, an admin's place or a seat,
    attaches no one, at once or at a sign-in; someone offered buys their
    own seat on a customer of their own, never the offering org's; who
    pays is never moved out from under the others. **Its M4** (`wiped`):
    a wipe cancels the subscription, keeps its customer, and drops the
    org.
- **The hosted lane**, on a preview with the sandbox.
  - It creates subscriptions through the API with `pm_card_visa`,
    since Checkout's page needs a person.
  - A test clock carries a trial past its end and a renewal into a
    decline (`pm_card_chargeCustomerFail`).
  - Each preview registers its own sandbox endpoint (decision 58).

## Trials

- **Codes** are finite-mono's shape, kept in the registry:
  - `trial_codes (id, code, name, kind, days 1–30, capacity 1–10000,
    expires_at, active, created_by)`, its kind `seat_always_on` (a code
    of another is refused at its making; one made before 2026-10-15 is
    refused as ended);
  - `trial_uses (code, person, session, expires_at, subscription)`.
- **Redeeming** (a guest who never had a subscription):
  - **Where:** they type the code in the shell's welcome, or open the
    mailed link `/welcome?trial=<code>`, which fills it in (signed out,
    sign-in comes back to it).
  - **Checks:** in one transaction, the registry checks the code is
    active and unexpired, that it has a place, and that the person never
    trialed. The places taken are uses with a subscription plus uses
    whose session has not yet expired. It then records the use.
  - **Checkout:** one always-on seat, `trial_period_days` (a month at
    most: 30), a card always, no promotion codes. It expires in 30
    minutes, Stripe's least.
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
  - `/admin`: a shell path, plain JS like the shell, served to an
    operator's session alone; to anyone else, signed in or not, the 404
    any missing path is (Paul, 2026-10-16: "currently anyone can visit
    the `/admin` url");
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
  - **comp it:** a comped seat in a new org of one, theirs at once when
    the email is a person's, else offered, theirs once they accept it;
  - **trial it:** a mail with a trial link.
- **Trial codes.**
  - **Create:** name, days, capacity and expiry, with the code
    generated or typed; its kind is the always-on seat's.
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

**The welcome** (`/welcome`; shell.js `welcome`, billing.js
`seatOffer`) is a new person's first run. It never goes through
settings (Paul, 2026-10-13: "fixed onboarding (shouldn't go through the
settings page)").

- **Who sees it:** a guest with nothing shared with them, who lands
  there from `/`. It holds a line of what this is, the one seat ($200 a
  month, always on, its time not metered, $100 of credit included each
  month) and a trial-code field, then Checkout, a line on what more
  credit costs once they hold a seat ($25 a pack, kept until spent), and
  who is signed in, with Sign out: one card on the landscape, as the
  launcher's panel is (Skyler's look).
- **What comes back to it:** Checkout's return
  (`?checkout=<session>`), its Back (`?checkout=canceled`, said), and a
  mailed trial's link (`?trial=<code>`, filled in). Signed out, sign-in
  comes back to it with its query. A refusal (a code that is none, a
  full one) is said under the seats.
- **A paid return** sets up the seat, then goes on to the first run, at
  `/`, in its chat:
  - *Setting up your seat,* on the welcome's card: the Checkout applied
    (`POST /api/billing/sessions/<id>`), kept in the address until its
    seat holds, so a reload applies it again; then the seat's plan
    reaching the ledger, a moment after (at most 30 s). A seat that does
    not come (a Checkout not theirs, or unpaid) is said, with Try again
    and Choose a seat.
  - *Then the first agent, in its chat* (docs/api.md, "The first run, in
    its chat"; Skyler's design, Paul, 2026-10-13): the chat opens at
    once with a card of the making as the platform says it (the agent
    made, in the chat, its computer's phase); the person may type at
    once, and send once the agent is in the chat. Each thing is made
    once (a retry or a reload reuses what was made).
- **What an org offers them** (a seat, an admin's place) shows first on
  it, with who offered it and what accepting does (a move from an org
  they are in), with Accept, which goes on to the first run, and Decline
  (billing.js `offers`).
- **Who skips it:** someone who is no guest (a seat bought, an offer
  accepted, an operator's comp, or the deployment's plan) goes straight
  to the first run, or home with a chat of their own. A Checkout's
  return for someone with a chat of their own is applied, then home.
- **An invitee** (a guest someone shared something with) lands on what
  was shared: home, with a note of what a guest may do and what a seat
  gives. Its "Get a seat" opens the welcome, whose "Not now" goes back.
  So does Billing's for a guest whose seat ended (removed, or a comp
  ended): they keep their chats, and the welcome offers them the seat.
- **Who is a guest, and what it costs:** `GET /api/seat` alone says
  (docs/api.md, Seats and orgs): its `guest` (no seat held, and no plan
  of their own on their ledger: `fragment_core::org::guest`), and its
  `offer`, the prices every line above shows (core's constants), so the
  shell, Billing and the welcome never disagree.
- **Later:** a place is kept, hidden, beside the seats for "Import your
  finite.computer agent" (fbfc's import, not built).

Settings gains **Billing**, which replaces the read-only Credit section,
for a seat once someone holds one:

- **A guest** sees what a seat gives and its price, and "Get a seat",
  which opens the welcome.
- **A seat holder** sees their seat, its kind, who provides it, this
  month's usage and credit, and "Buy $25 of credit" (its `offer`'s price). On `seat_always_on`
  it adds "let it sleep". One who is no admin has "Leave" beside their
  org.
- **Anyone offered something** sees each offer, its org, who made it and
  what accepting does, with Accept and Decline; a refusal (a move that
  would strand an org) is said under it.
- **An org admin** sees the seats table: email, kind, offered (shown
  "invited") or held, and who pays. From it they:
  - offer a seat by email (an always-on one; someone in the org has it
    at once);
  - remove a seat, or withdraw its offer;
  - offer an admin's place;
  - open "Payment and invoices" (Stripe's portal), only who pays (its
    other admins see neither it nor a way to take who pays's seat or
    place; who pays stepping down is warned it cancels at once).

  The table shows once someone besides its admin is in the org (a seat,
  held or invited, or another admin). An admin alone in it, who pays for
  themselves, sees "Payment and invoices" and "Add seats for others",
  which shows the table.
- **A lapsed person** sees why, and how to pay again.

The shell offers a guest nothing to make.

## The API, in sketch

| Route | Who |
|---|---|
| `POST /api/billing/checkout {kind?, trialCode?}` (`kind` only `seat_always_on`, the default) | someone in no org (an org of one, made once paid), or an admin of their org that pays for nothing |
| `POST /api/billing/sessions/<id>` | the Checkout return |
| `POST /api/billing/portal` | who pays for the org (403 to its other admins: Paul's to reverse) |
| `POST /api/billing/packs {member?}` | a seat holder, or an org admin for a member |
| `GET /api/seat` | a person: their seat, org, and every offer to an email of theirs |
| `POST /api/seat/offers/<id>/accept`, `/decline` | the person offered it |
| `POST /api/seat/leave` | a seat holder who is no admin |
| `GET /api/org` | an admin |
| `POST /api/org/seats {email, kind?}` (`kind` as Checkout's; an offer), `DELETE /api/org/seats/<id>` | an org admin, while it pays |
| `POST /api/org/admins {email}` (an offer), `DELETE /api/org/admins/<id>` | an org admin, while it pays; who pays steps down only themselves |
| `PUT /api/computers/<id>/sleep {allowed}` | the computer's owner on `seat_always_on` |
| `POST /api/stripe/webhook` | Stripe, signed |
| `/api/admin/people`, `/people/<npub>`, `/invites`, `/trial-codes`, `/orgs`, `/health`, `/log` | operators |

### Limits

- Seats per org: 200, offered ones included.
- Admins per org: 10, offered ones included.
- Offers mailed per org per day (UTC): 50 (`fragment_core::org::
  OFFER_MAILS_PER_ORG_DAILY_MAX`; the next is 429).
- Orgs offering one email something at once: 10 (`OFFERS_PER_EMAIL_MAX`;
  it accepts or declines one first).
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
7. **Packs** (built 2026-10-08, branch `claude/billing-packs`): one
   $25 pack (decision 55).
8. **The operator's admin** (built 2026-10-08, branch
   `claude/operator-admin`; docs/api.md, Operators: the admin): the
   API, the CLI (`fragment operator …`), `/admin`, and the audit log.
9. **The shell's Billing page** (built 2026-10-08, branch
   `claude/shell-billing`; cell/shell/billing.js). Its first run moved
   to the welcome (built 2026-10-13, branch `claude/first-run`).
10. **Go live** (Paul's; its tooling built 2026-10-08, branch
    `claude/stripe-xtask`: `xtask stripe check|setup`, and a branch's own
    endpoint at each deploy):
    - live Products, Prices, portal and endpoint (`xtask stripe setup`);
    - the restricted key and signing secret (`secret set`);
    - `stripe check` green;
    - WorkOS sign-up on (#254's fourth slice);
    - README and GUIDE no longer saying invite-only;
    - docs/finite-integration.md: its "Billing, orgs, entitlements" row,
      and "organizations with several members" and Stripe leave "Not
      built in fragment, on purpose".
11. **Consent, and a wipe's billing** (built 2026-10-17, branch
    `claude/org-consent`; the security audit's H1 and M4, Paul's calls
    of 2026-10-17: keep teams, nobody joins without accepting; a wipe
    cancels and keeps the customer):
    - orgs made only once paid; Checkout on `customer_email` or the
      org's own customer, never another's;
    - offers, accepted, declined, a move, a leave; signing in attaches
      nothing; one writer of a row's person (`enroll`);
    - who pays, guarded; offers' gates (an org that pays, a person not
      being wiped, an email's offers, an org's mails a day);
    - no paid seat left cancels at once (`stripe_cancels`); an org with
      no one goes once nothing of it bills;
    - a wipe's `billing` step;
    - after the review of #367 (same day): who pays is whoever's card
      may still be charged, and their going cancels at once; only they
      open billing (a default for Paul); the org's customer is its
      payer's, forgotten when they go; an admin's place is offered even
      to a member; pending prorations deleted on a cancel; an orphan
      subscription found or canceled.

## Decided (Paul, 2026-10-08)

Paul took every recommendation. In docs/cloudflare-v1.md:

| Question | Answer | Decision |
|---|---|---|
| Who may buy a seat | any guest; trials and comps for the rest | 51 |
| Orgs | a self-paying person is an org of one; one org per person; an admin may be seatless | 51 |
| When an offered seat bills | from the offer | 52 |
| Upgrades and downgrades | at once, prorated | 52 |
| Lapse | today's `canceled`; nothing deleted; paying restores; retention later | 53 |
| A removed member's fragments | stay theirs; org-owned fragments later | 53 |
| `past_due` | good standing; Stripe's retries are the grace | 53 |
| Tax | Stripe automatic tax, prices before tax | 54 |
| Packs | one $25 pack, kept until spent | 55 |
| What is sold (Paul, 2026-10-15) | the $200 always-on seat and the $25 pack; no $100 seat | 25 (its status) |
| Teams (Paul, 2026-10-17) | kept: several admins, seats for others; nobody joins an org without accepting | 51 (its status) |
| Who opens an org's billing (the review of #367, a default) | only who pays for it; its other admins manage seats, never billing; Paul's to reverse | 51 (its status) |
| A wipe's subscription (Paul, 2026-10-17) | canceled at once; the Stripe customer kept (invoices, tax, refunds), the link to it dropped | 53 (its status) |
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
