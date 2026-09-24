# Phase 3: hosted on fragment.club

Status: started 2026-09-23 on Paul's go (costs approved; fragment.club
itself, no staging name). The ROADMAP keeps the acceptance; this file
keeps the slices. Update it in the same commit as the work.

## Shape

```
browser/CLI ──https──▶ Fly proxy (TLS: fragment.club, *.fragment.club)
                          │  :8080 on both Machines
                          ▼
     Machine A (ord) ◀── 6PN :8081 ──▶ Machine B (ord)
     celld fork + volume                celld fork + volume
                          │
                          ▼
            Tigris bucket, single-region ord
            (deployments, cell replicas, blobs under r2/)
```

- **Nodes:** two always-on `shared-cpu-2x` 2 GB Machines in `ord`, each
  with a 3 GB volume for `CELLD_WATCH` (a watch dir is paired with its
  bucket for life). The image holds the celld fork at `b5f57ea` and a
  small Rust launcher (`crates/node`) that binds the internal listener
  on the Machine's private address and advertises it to the other node.
  The internal port is never a Fly service.
- **The cell:** `cargo xtask deploy <fleet>` builds `cell/`, renders its
  `vars` from the fleet's file (`fleets/<fleet>.json`, no secret values)
  and secret files, and runs `celld deploy` against the bucket. Nodes
  adopt a new deployment in place within 30 seconds.
- **Secrets:** celld 0.5.1 takes Worker variables only from the deployed
  config, so the host secret and the code.storage key live in the
  deployment objects in the bucket. celld treats the bucket as the
  fleet's root of authority (a bucket credential can deploy code that
  reads any variable), so this adds no new holder; the bucket
  credentials are the fleet's most sensitive secret (Fly secrets on the
  app, a file on Paul's machine).
- **Bucket:** Tigris single-region `ord`: strongly consistent, with
  conditional writes evaluated in one region (a global bucket can check
  a precondition against a stale copy from another region).
- **Estimated cost:** two Machines ≈ $23/month, volumes ≈ $1, Tigris a
  few dollars at this size, plus AI per decision 14.

## Slices

**A. A deployable fleet** (repo only). The creation allowlist
(`FRAGMENT_CREATORS`: until sign-in, only these keys may create
fragments; e2e proves a stranger is refused); `crates/node`, the
Dockerfile, and `fly.toml`; `fleets/fragment-club.json`; `cargo xtask
deploy`.

**B. Bring-up.** The Fly app, the bucket, the volumes, the bucket
credentials as app secrets, the image, both Machines; `celld diagnose`
before any state lands; the cell deployed; health on the `fly.dev`
address (path-mode URLs until DNS moves).

*A and B done 2026-09-23.* The fleet runs at `https://fragment-club.fly.dev`:
two Machines (`80e934ef656e68`, `83d677b7395168`) on the single-region
bucket `fragment-club-ord`, peers reaching each other over 6PN with
signed probes, the storage contract checked. The operator runbook is
`docs/operate.md`. What the bring-up found and fixed:

- **The cell ignored the TLS proxy.** Fly ends TLS and forwards HTTP, so
  the cell saw `http://` URLs: every NIP-98 signature (over the https
  URL) would fail, and links and cookie scopes would name http. The
  router now takes `x-forwarded-proto: https` as the arrival URL (e2e
  `auth`).
- **Fly's remote builders are refused the image push** (401 from the
  builder's registry proxy, both Fly's and Depot's) with this org's
  token, while the same token pushes directly. `xtask deploy --nodes`
  builds with the local Docker and pushes with a throwaway login
  (debt ledger).
- **celld 0.5.1 takes Worker variables only from the deployed config**
  (the `CELLD_VAR_` passthrough is gone), so the fleet's secrets live in
  the bucket (Shape, above).
- A failed `xtask` command printed its environment, which held the Fly
  token: it went to a log and to the session transcript. `xtask` now
  shows only the program and its arguments (tested); the token is due
  for rotation (Paul).

**C. The e2e against the fleet.** `cargo xtask e2e --fleet
fragment-club` (`crates/e2e/src/hosted.rs`): the path a person takes,
with nothing faked. Its fragments are `e2e-<run>-*`, made by an e2e key
the fleet lists as a creator, and deleted with their repos after.

*Done 2026-09-23: 24 of 24.* Sections: health; creators (a stranger is
refused); todo (`fragment init` against the real code.storage, a page
over https, replay and conflict, twenty mutations, two Chrome tabs live
over wss); inbox (a webhook delivery runs a job that fetches
example.com and records its title); blobs (a 2 MiB file as a pointer,
served by range from Tigris, pulled into another folder); ai (live
OpenRouter: a text answer on `z-ai/glm-5.3-flash` for $0.00001, a 32 KB
image on the default image model). Numbers, from Paul's machine (two
runs): a mutation 51–53 ms p50, 62–109 ms p95; one run had a single
2.6 s outlier in 20 (unexplained: a cold path or a fleet-proof retry);
`fragment init` 3–5 s. A cold cell's
load needs an idle eviction the e2e cannot force; cell-to-Sprite
latency waits for computers (phase 8).

What it found, against the real services:

- **Every fragment shared one repo named after the org.** code.storage
  names a new repo by the token's `repo` claim (the body's `repo_name`
  must equal it); the cell signed the create with the org's claim and
  sent the name as `id`, which the service ignores, and the fake took
  `id`. Only this run's test fragments used it (the repo did not exist
  before today, and each run's cleanup deleted it). The cell now signs
  with the name; the fake names repos as the service does, so e2e
  `create` ("the repo exists under the name") catches it.
- **Finding an existing repo looked at one page.** The org's list is 20
  a page (at most 100), newest first, and a name is not searchable; a
  re-created name in an org of more than 20 repos failed. The cell now
  pages; the fake pages as the service does, and e2e `create` seeds 150
  newer repos first.
- **The plan's text model reasons first**: a small `max_tokens` bought
  only reasoning and an empty answer. `job.ai.text` passes OpenRouter's
  `reasoning` through (e2e `ai` checks it arrives).
- The CLI printed human lines before `--json` output in `init` and
  `deploy`, and path-mode share links lost their trailing slash (a
  redirect that could drop the token). Both fixed.

**D. The real code.storage.** *Deferred to phase 8 (2026-09-23).*
Webhooks are one subscription per org, made in code.storage's dashboard,
signed with its secret, and each delivery names its repo by the opaque
url form (`repository.url`), so an org route needs an index from that
form to a fragment. Nothing needs it yet: the CLI refreshes a fragment
after every sync and deploy, the cell learns of its own commits, and the
five-minute poll catches the rest. The first writer outside those is a
computer pushing with git (phase 8), which brings the index and the
route.

**E. Egress by address.** A job's fetch must not reach the fleet's
private network even through a public name that resolves to a private
address (the internal listener's operator API is unauthenticated).
Before anyone but Paul can create fragments. *Waiting on Paul (2026-09-23):* a firewall on
the Machine cannot tell a job's fetch from celld's own peer traffic (the
same process, the same port), so the check belongs where celld resolves
a Worker's fetch: a DNS resolver on its three outbound clients
(`crates/celld/js.rs`) that refuses loopback, private, link-local, and
ULA addresses (and IP literals of them) when `CELLD_EGRESS_PUBLIC_ONLY=1`,
set on hosted fleets only. It is a fork change (upstreamable), and the
image builds the fork from GitHub, so it needs a push to
`futurepaul/celld`.

**F. Cutover.** Certificates for `fragment.club` and `*.fragment.club`;
Paul points the records at Fly (Namecheap); the suffix goes on; one live
OpenRouter call (text and a small image); a push to Paul's phone; the
operator runbook (`docs/operate.md`). The VPS keeps running until Paul
retires it. *Certificates added 2026-09-23 (pending DNS); the
live OpenRouter call and the runbook are done (C, above).* Paul's steps
at Namecheap: first `CNAME _acme-challenge → fragment.club.nwd56j0.flydns.net.`
(no traffic moves; both certificates issue), then, to cut over, `@` and
`*` to `A 66.241.125.20` and `AAAA 2a09:8280:1::199:8a1c:0` (replacing
`152.236.5.66`). After that the fleet file gains `FRAGMENT_HOST_SUFFIX`
and the URL `https://fragment.club`, the cell is redeployed, and the
hosted e2e runs on fragment hosts.
