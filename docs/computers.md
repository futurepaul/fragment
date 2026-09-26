# Computers: a fragment's own machine

Status, 2026-09-26 (ROADMAP phase E). Built:

- **Computers are identities** (decision 21; docs/api.md, Identities). A
  person pairs a machine with `fragment login --computer <name>`, and from
  then on it signs as that computer, never as them. It works only in
  fragments where it is a member and in the fragments it makes, which are
  its owner's, on its owner's budget. `fragment computers rm` revokes its
  keys and removes its memberships.
- **A fragment declares one** (`"computer": {}`; docs/api.md, Apps), and
  the platform gives it a Sprite: the rest of this page.

## Paul's answers (2026-09-26)

1. **Sprites org:** the org used for Sprites so far. Its token is the
   node secret `FRAGMENT_KEYS_SPRITES_TOKEN`, used only by `KEYS`.
2. **Cost:** list price for awake time and disk, to the fragment owner's
   budget.
3. **Idle policy:** awake while a job step needs it, then asleep 5
   minutes after the last page viewer leaves. Never destroyed
   automatically. Alerts about "accidentally awake" Sprites come later
   (docs/technical-debt-ledger.md).
4. **Grant:** none. Resources are declarative: declaring `"computer"`
   provisions one on deploy, and the owner pays, as for everything a
   fragment spends.
5. **Model calls:** through the platform, signed with the computer's own
   key, on the model the agents in cells use (the next PR).

## How it works

1. **Declared.** A deploy whose live `fragment.json` has `"computer": {}`
   tells the fragment's `Computer` cell (one per fragment, named by it;
   `cell/src/computer.rs`). A deploy without it tells it too.
2. **Made.** The cell's alarm charges a tick, makes the Sprite through
   `KEYS`, and runs its first boot there: the one-line install of the
   CLI's release, then `fragment login --pair`. A `Computer` cell reaches
   only the Sprite named for its own id (`fragment-` and 24 hex:
   crates/native `sprite_of`), so no cell can touch another Sprite in the
   org.
3. **Paired.** The boot's stdin carries a single-use token the registry
   minted for the fragment's owner (an hour good). The CLI makes its key
   on the Sprite and pairs with the token (`POST /api/computers/pair`,
   signed by that key), so the key never leaves the Sprite. The computer
   is named by its fragment (`pet.paul`), and the platform makes it an
   editor there.
4. **Awake and asleep.** A page opening on the fragment wakes it. Each
   tick (a minute) it is charged first, then held awake through the
   Sprite's Tasks API (a hold that expires after two ticks). Each tick,
   the cell asks the fragment for open pages; 5 minutes after the last
   one closes, the hold is released and the Sprite pauses itself. A job
   step will wake it the same way (`Wake`), with `job.computer`. A tick
   that does not fit the budget lets it sleep (`computer.budget`).
5. **Billed at list price.** Sprites meter the CPU and memory a Sprite
   actually uses, which the platform cannot see, so an awake hour is
   billed as the idle footprint measured on one: a tenth of a CPU
   ($0.07 a CPU-hour) and 1.5 GB ($0.04375 a GB-hour), $0.0726 an hour.
   Its disk (its home directory, measured each time it wakes) costs
   $0.000683 a GB-hour awake and $0.000027 asleep, charged when it next
   wakes (`fragment_core::budget::computer`).
6. **Kept or destroyed.** Dropping the block lets it sleep and keeps it.
   `fragment computers rm <fragment>` destroys its Sprite and revokes its
   keys. The fragment's next deploy that declares one makes a new one.

The only credential a computer holds is its own key, in its CLI config
(0600) on the Sprite's disk (decision 21).

## Running the first real one

`fleets/fragment-club.json` names the token's file
(`~/.config/finite-next/secrets/sprites-token`, in `node_secrets`). Until
that file exists, a deploy leaves the secret out and says so
(`xtask/src/deploy.rs`, `OPTIONAL_NODE_SECRETS`): everything else deploys,
and a fragment that declares a computer waits (`computer.failed`, "not set
on this node", retried at least hourly). Once Paul makes the file, a
`cargo xtask deploy fragment-club --secrets` sets it (and restarts the
Machines), and the node and cell deploy as usual. Then, as one person, on a scratch fragment:

1. `fragment create pet-smoke`, then deploy a folder whose
   `fragment.json` is `{"computer": {}}`.
2. Within a minute or two, `fragment events pet-smoke` shows
   `computer.ready`, `fragment computers` lists `pet-smoke.<you>`, and
   `fragment members list pet-smoke` shows it as an editor. A
   `computer.failed` event names the step and Sprites' answer.
3. Open the fragment's page: `sprite list` shows its Sprite running.
   Close it: after 5 minutes and a tick, it is warm, then cold.
   `fragment budget usage` shows `computer.awake` rows.
4. `fragment computers rm pet-smoke.<you>`: the Sprite is gone from
   `sprite list`.

Things only the real one shows: the exec answer's shape (the cell reads
the last number `du -sk` printed), the install's time (the exec waits up
to 180 s), and whether `sprite-env curl` holds it awake from an exec.

## Next

- **Model calls** through the platform, signed by the computer's key (the
  next PR).
- **How a page shows it: a CUA "pet".** A declared `desktop` service
  (Xvfb, Chromium, and a control endpoint, from finite-next's
  `computer/*`) takes a screenshot on each change (at most one a second),
  stores it as a blob, and posts `{sha, at}` to a declared `screen`
  channel: every viewer sees the same frame live. The page calls
  operations (`poke {x, y}`, `type {text}`) that append to a `control`
  channel, which the computer follows and applies once each (keyed by
  seq). An agent drives it through the same operations.
- **`job.computer(tool, args)`**, a durable job step over the `computer
  serve` call protocol, waking the computer for the step.
- **Alerts** about computers awake longer than expected, and what a
  deleted fragment's computer becomes (today it stays, asleep, until
  `fragment computers rm`).
