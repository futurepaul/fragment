# Operating a fleet

How a hosted fleet runs and how to change it. The fleet `fragment-club`
is the only one; everything below takes the fleet's name, and reads its
settings from `fleets/<fleet>.json` (no secret values there: only the
paths of files that hold them, all under `~/.config/finite-next/secrets/`).

## What runs where

- **Fly app `fragment-club`** (org `personal`, region `ord`): two
  `shared-cpu-2x` 2 GB Machines, each with a 3 GB volume `celld_data` at
  `/data`. Each runs the node image: the celld fork at the rev in
  `crates/devstack` (`CELLD_FORK_REV`: v0.5.1, the alarm fix, public-only
  Worker egress, and the hardening pass: `KEYS` built in from
  `crates/native`) and `fragment-node` (`crates/node`), which binds
  celld's peer listener to the Machine's private address
  (`[fdaa:…]:8081`, never a Fly service) and pairs the volume with the
  bucket for life (`/data/bucket`). The rendered fly.toml turns on the
  fork's settings: `CELLD_EGRESS_PUBLIC_ONLY=1` keeps every Worker fetch
  off the private network, `CELLD_INTERNAL_PEER_ONLY=1` has the internal
  listener serve only fleet-signed peers, `CELLD_DYNAMIC_LOCKDOWN=1` and
  `CELLD_FACET_MAX_BYTES` bound what an app can do (docs/hardening.md),
  and `CELLD_MAX_REQUEST_BODY_BYTES` (256 MiB, a blob's limit) replaces
  celld's 1 GiB default for any request body. The live Machines take it
  at the next `cargo xtask deploy --nodes`.
- **Two scripts, one application:** the cell (`fragment`, the fleet's
  application: the fleet-wide pointer names it) and the agents'
  (`fragment-agent`, from `agent/`), which the cell's `AGENTS` binding
  loads from its own pointer. It has no ingress: the router hands it
  `/api/agents` and `/api/a/*`. `cargo xtask deploy <fleet>` publishes
  it first with `celld deploy --named` (the fork's), then the cell,
  whose adoption loads both.
- **The fleet's secrets** (the host secret, the code.storage org key,
  the WorkOS API key, the OpenRouter management key) are Fly secrets on
  the app, `FRAGMENT_KEYS_*`: the node's environment, read only by `KEYS`.
  `cargo xtask deploy --nodes` stages them from the files the fleet file's
  `node_secrets` names, before it rolls the Machines.
- **Bucket `fragment-club-ord`** (Tigris, single-region `ord`): the
  deployments, every cell's replicas, the node leases, the peer secret,
  and blobs under `r2/`. Its keys are Fly secrets on the app (the nodes)
  and `fragment-club-bucket.env` (the operator). **Whoever holds them
  controls the fleet** (they can deploy code). The Worker variables in
  each deployment hold no secret since the hardening pass; the
  deployments before it do (docs/technical-debt-ledger.md).
- **code.storage org `finite`**: one repo per fragment, named after it.
- **DNS** at Namecheap: `fragment.club` and `*.fragment.club` to the app
  (A `66.241.125.20`, AAAA `2a09:8280:1::199:8a1c:0`), and
  `_acme-challenge.fragment.club` CNAME `fragment.club.nwd56j0.flydns.net.`
  for the certificates.

## Everyday commands

```
cargo xtask deploy fragment-club            # ship the agents' script (--named) and the cell: build, render vars, celld deploy, wait for adoption
cargo xtask deploy fragment-club --nodes    # ship the node image (celld fork, fragment-node): local build, rolling update
cargo xtask e2e --fleet fragment-club       # the hosted e2e (spends a few cents on OpenRouter)
cargo xtask fleet fragment-club diagnose    # leases and the storage contract (peers are unreachable from outside Fly)
cargo xtask fleet fragment-club cell list   # the fleet's Durable Objects
```

- A cell deploy needs no restart: nodes adopt it within 30 s, and the
  command waits until `/healthz` answers the new `x-fragment-deploy` id.
- A node deploy builds the image with the local Docker (OrbStack) for
  `linux/amd64` and pushes it with a throwaway registry login: Fly's
  remote builders are refused the push with this org's token. Fly then
  replaces the Machines one at a time; celld drains each (40 s) before
  it stops, and the next waits for the replacement's health.
- Peer checks from inside the fleet:
  `flyctl ssh console -a fragment-club --machine <id> -C "/usr/local/bin/celld diagnose --bucket s3://fragment-club-ord --endpoint https://fly.storage.tigris.dev --region auto --listen 127.0.0.1:18080 --internal-listen 127.0.0.1:18081"`
  (the spare ports: the node holds 8080).
- Logs: `flyctl logs -a fragment-club`.

## Changing configuration

Worker variables live in the fleet file (`vars`, or `var_files` for
values kept out of the repo that are not secret). Edit it, then `cargo
xtask deploy fragment-club`; the deploy refuses a variable that is a
fleet secret, by name or by value. The fleet's secrets are `node_secrets`
(file paths): change one by editing its file (or the entry), then `cargo
xtask deploy fragment-club --nodes`, which stages them as Fly secrets and
rolls the Machines. Budgets (phase 4 slice C):
`FRAGMENT_KEYS_OPENROUTER_MANAGEMENT_KEY` in `node_secrets` mints each
person's OpenRouter key; `FRAGMENT_BUDGET_USD` (default 20) and
`FRAGMENT_OPERATORS` (who may top up: `fragment budget top-up <id>
<usd>`) in `vars`.

Sign-in is WorkOS (phase 4 slice B): `WORKOS_CLIENT_ID` in `var_files`,
`FRAGMENT_KEYS_WORKOS_API_KEY` in `node_secrets`; the environment's
redirect URI is `https://fragment.club/auth/callback`. Only people create
fragments, and sign-up is off in WorkOS, so everyone who can sign in was
invited there.

Pending sign-ins are capped by `FRAGMENT_SIGNINS_PENDING_MAX` in `vars`
(default 100000). `/auth/login` is anonymous, so the cap is what a flood
of starts meets: a sign-in is good for ten minutes and is kept through
the next cap's worth of starts, so people's sign-ins in progress are let
go only while starts outrun the cap over ten minutes (cap / 600 s: about
166 a second at the default, 1.7 at 1000). At the cap, the registry holds
that many rows of a 64-hex state, a return path (at most 2 KiB, usually
`/`), and a time, about 200 bytes each with its indexes: some 20 MB at
the default, swept once they expire. Each start costs one insert and one
delete of the oldest by rowid, whatever the cap. Raise it when people
must keep signing in through a heavier flood; lower it when the
registry's storage matters more.

Rotating the host secret: add `FRAGMENT_KEYS_HOST_SECRET_PREVIOUS` to
`node_secrets` pointing at a copy of the current value, put a new one in
the host-secret file, `cargo xtask deploy fragment-club --nodes`. Sealed
values open under either and come back resealed as they are read.

A deployment that changes both the cell and `KEYS`: nodes first (the old
cell keeps working on them), then the cell.

Changing a secret alone (a rotation): write the new value over its file
(the bucket's keys: the two `AWS_*` lines of `fragment-club-bucket.env`),
then `cargo xtask deploy fragment-club --secrets`, which sets the
`node_secrets` and the bucket's keys on the app; Fly restarts the
Machines one at a time. Then revoke the old value at its issuer.

## Bringing a fleet up (what was done for fragment-club)

1. `flyctl apps create <app> --org <org>`.
2. The bucket: `flyctl storage create -a <app> -n <name> -o <org> -y >
   <credentials file>` makes the Tigris project, its keys (also set as
   app secrets), and a global bucket; then create the fleet's bucket
   single-region with an S3 `CreateBucket` whose `LocationConstraint` is
   the region (`ord`), signed with those keys. A global bucket may check
   a conditional write against another region's copy.
3. `cargo xtask fleet <fleet> diagnose`: the storage contract (create,
   reject-create, update, reject-stale) before any state lands.
4. `cargo xtask deploy <fleet> --nodes`: the first Machine and its
   volume. Its health check fails until a deployment exists (celld
   waits: "no deployment yet"), which is expected.
5. `cargo xtask deploy <fleet>`: the cell; the node serves it.
6. `flyctl scale count 2 --region <region> -a <app> -y`: the second
   Machine, with its own volume.
7. The peer check above; then the hosted e2e.
8. Certificates: `flyctl certs add <apex>` and `flyctl certs add
   "*.<apex>"`; the `_acme-challenge` CNAME validates both before any
   traffic moves.

## Rules

- A volume serves one bucket for life (`fragment-node` refuses another).
  A new bucket means new volumes.
- celld upgrades: read the release's upgrade notes first; some need every
  node stopped before the new version starts (`flyctl scale count 0`,
  then deploy, then scale back).
- Never publish the internal port (8081) as a Fly service: its operator
  API has no authentication.
- A re-created fragment name finds its old repo in code.storage (and its
  files); the cell does not delete repos. The hosted e2e deletes its own.
- The old runtime's repos (`linecount`, `events-rfc`, …) are in the same
  org; a new fragment with one of those names attaches to that repo.
