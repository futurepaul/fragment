# Operating a fleet

How a hosted fleet runs and how to change it. The fleet `fragment-club`
is the only one; everything below takes the fleet's name, and reads its
settings from `fleets/<fleet>.json` (no secret values there: only the
paths of files that hold them, all under `~/.config/finite-next/secrets/`).

## What runs where

- **Fly app `fragment-club`** (org `personal`, region `ord`): two
  `shared-cpu-2x` 2 GB Machines, each with a 3 GB volume `celld_data` at
  `/data`. Each runs the node image: the celld fork at the rev in
  `crates/devstack` (`CELLD_FORK_REV`) and `fragment-node`
  (`crates/node`), which binds celld's peer listener to the Machine's
  private address (`[fdaa:…]:8081`, never a Fly service) and pairs the
  volume with the bucket for life (`/data/bucket`).
- **Bucket `fragment-club-ord`** (Tigris, single-region `ord`): the
  deployments, every cell's replicas, the node leases, the peer secret,
  and blobs under `r2/`. Its keys are Fly secrets on the app (the nodes)
  and `fragment-club-bucket.env` (the operator). **Whoever holds them
  controls the fleet**, including the Worker variables (the host secret,
  the code.storage key), which celld 0.5.1 keeps in the deployment.
- **code.storage org `finite`**: one repo per fragment, named after it.
- **DNS** at Namecheap: `fragment.club` and `*.fragment.club` to the app
  (A `66.241.125.20`, AAAA `2a09:8280:1::199:8a1c:0`), and
  `_acme-challenge.fragment.club` CNAME `fragment.club.nwd56j0.flydns.net.`
  for the certificates.

## Everyday commands

```
cargo xtask deploy fragment-club            # ship the cell (cell/): build, render vars, celld deploy, wait for adoption
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

Worker variables live in the fleet file (`vars`, or `secret_vars` for
file-held values). Edit it, then `cargo xtask deploy fragment-club`.
`FRAGMENT_CREATORS` lists who may create fragments until sign-in exists
(phase 4): Paul's npub and the e2e key's.

Rotating the host secret: move the current value to
`FRAGMENT_HOST_SECRET_PREVIOUS` (a new `secret_vars` entry), put a new one
in the host-secret file, deploy. Sealed values open under either and are
resealed as they are read.

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
