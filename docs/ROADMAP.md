# ROADMAP — fragment as a thin wrapper over celld + S3 + code.storage

Decided 2026-09-07. This is intent, not exploration (those live in
`docs/explorations/`). The end state: **celld for compute + its own state,
code.storage for everything file-like, S3 for celld state only.** All three
providers swappable (Cloudflare for celld, any git host for code.storage,
any S3 for state). Zero transition code — hard cuts only, per the Finite
engineering style (`finite-mono/finitechat/docs/engineering-style.md` and
the finite-brain local rule, which fragment adopts for this work).

## Decisions (made)

1. **Write path:** workflows serialize through their cell (option A);
   the CLI commits directly with expected-parent CAS (option B).
2. **Manifest authority:** `fragment.json` is a git file at the repo root;
   the cell caches it. `manifest-set` becomes edit-and-commit.
3. **JWT custody:** the code.storage org private key lives on the host
   only, delivered via systemd `LoadCredentialEncrypted=`
   (`CELLD_VAR_CODESTORAGE_ORG_KEY`), never in any repo or bucket;
   rotation is a new key version with overlap; the host mints short-lived
   scoped JWTs per fragment repo. If a copy is ever persisted, it is
   wrapped under `FRAGMENT_HOST_SECRET` (HKDF + AES-GCM), like all
   cell-held secrets.
4. **Phasing:** phases 1+2 merged into ONE hard cut — the cross-cutting
   concerns compound; staging them creates more complexity than it
   removes.
5. **No R2 blob tier, ever:** files move to git+LFS, so file bytes never
   touch our bucket. `blob.rs`, `blob-tier.js`, and `blobsd` delete
   without replacement; the S3 8 MiB conditional-write limit is
   irrelevant because nothing file-shaped lives in S3.

## Canonical sources (per code.storage's own onboarding docs)

- Source of truth: `https://code.storage/docs/llms.txt` + the OpenAPI spec
  at `https://code.storage/docs/openapi.json`. Documented behavior only.
- Runtime uses the official SDK: `@pierre/storage` (TS) — `GitStorage`,
  fluent `createCommit()` builder, `getFileStream()`/`headFile()`/
  `listFilesWithMetadata()` for the file plane, `getEphemeralRemoteURL()`.
  The Rust CLI has no SDK and codes against the OpenAPI spec: Create
  Commit from Files (push), List Files / Get File (pull), Get Branch
  (expected-parent), Reset Branch to Commit (rollback), Create Branch
  (preview). The default branch cannot be deleted.
- The org private key env var is **`PIERRE_PRIVATE_KEY`** — on the host it
  is delivered via systemd `LoadCredentialEncrypted=` (celld passes it
  through as `CELLD_VAR_PIERRE_PRIVATE_KEY`); never committed, never in
  client code. JWTs are customer-signed by us (scopes `org:read`,
  `repo:write`, `git:read`, `git:write`; per-repo scoping via the `repo`
  claim; ref policies protect `live`). Git remotes (if ever used) take
  username `t`, password = JWT.
- **No LFS (spec-corrected 2026-09-07).** The HTTP API has no LFS
  endpoints; LFS exists only git-client-side over the git remote, which
  fragment doesn't use. All files ride the chunked commit-pack inline
  (`POST /api/repos/{repo}/commit-pack`, NDJSON, ≤4 MiB decoded chunks,
  no total size limit). The chunk boundary stays one function so a tier
  could slot in later if ever needed. Spec vocabulary: expected-parent is
  `expected_target_sha`; CAS failure is 409 with
  `result.status="precondition_failed"`; previews are
  `POST /branches/create` with `target_is_ephemeral: true`.
- Org: **`finite`** (verified live 2026-09-07: key authenticates, org
  reachable, zero repos — clean slate). Local dev reads
  `PIERRE_PRIVATE_KEY` from `.env` (gitignored, untracked, never in
  history); prod delivers it via the systemd credential.

## The truth map (invariants — every change is checked against this)

| Thing | Source of truth | Derived/copies must be |
|---|---|---|
| File bytes + history + live pointer | code.storage git (+ their LFS storage) | local folder = disposable working copy; cell RAM = LRU cache, never persisted |
| Tree index (path, size, sha per pinned commit) | derived from git | cell SQLite; names its source (pinned SHA), invalidation (webhook/poll), stale-read behavior (serve pinned until refreshed) |
| Cell/cluster state (WAL, registry, events) | the S3 bucket | — |
| Compute/audit trail | events ledger | webhook deliveries recorded as events, deduped by delivery ID |
| Secrets | cell only, wrapped under `FRAGMENT_HOST_SECRET` | never in the repo, by rule |

Hard rule: **no file bytes persist in DO SQLite or the bucket.** S3 holds
celld state only; nothing approaches 8 MiB by construction.

## Wire contract (runtime and CLI implement against this, no divergence)

- One code.storage repo per fragment. `main` = working files; `live` =
  the blessed serve point. Preview = ephemeral ref; promote = go live;
  rollback = re-point `live`.
- Runtime file plane: the cell pins `main@SHA` for working-copy reads,
  serves blessed content from `live@SHA`; tree index in cell SQLite;
  contents streamed on demand; RAM LRU (cap 64 MiB).
- Workflow writes: server-side commit builder via the cell-held JWT,
  expected-parent CAS, single-flight per workflow (existing semantics).
- CLI writes: commit builder directly with CAS; conflict → refetch head,
  rebuild, retry (bounded, e.g. 3 attempts, explicit error after).
- LFS: files over 1 MiB become LFS objects via the SDK; pointers in git.
- Storage tokens: `GET /api/f/{name}/storage-token` (editor+, NIP-98)
  returns `{ "token": "<jwt>", "repo": "<repo>", "api": "https://api.<cluster>.code.storage" }`
  — a short-lived code.storage JWT scoped to that fragment's repo (repo
  claim + git scopes), signed by the host from the org key; the CLI uses
  it for direct commits. CLI backend precedence: `FRAGMENT_CODESTORAGE_URL`
  env > `config.json` `codestorage` key > the token response's `api`.
- Fragment identity: npub secret generated **client-side** at create;
  transmitted once over the creator's authenticated channel and stored
  wrapped (level-c fix from `docs/encryption-research.md` stage 1).
- Webhooks: HMAC-verified push/sync events → validate, persist as events
  (dedup by delivery ID), then interpret (refresh pinned SHA, notify
  rooms). Poll branch head every 5 min as the contained backstop.
- Oversized pushes: no special path. The commit API takes files of any
  size (LFS beyond 1 MiB).

## Workstream A — runtime (owner: runtime/, wrangler.jsonc, docs/api.md)

1. Workflow bodies execute on native Workflows (`WorkflowEntrypoint`)
   under the unchanged authoring surface (`workflows/*.mjs`, `async
   run(ctx)`, manifest triggers). Keep or deliberately change each
   documented promise (single-flight, retry/backoff defaults, cron rules,
   loop-safety) — `cli/GUIDE.md` workflow sections and
   `templates/basic/fragment.d.ts` move in the same commit.
2. `notify_outbox` → celld Queues (one writer, one consumer shape).
3. Delete the blob tier (runtime side) and `blobsd/` (service + runbook
   section in `docs/deploy-vps.md`).
4. The code.storage file plane per the wire contract: tree index,
   streaming reads, commit-builder writes, webhook ingestion, poll
   backstop, storage-token minting endpoint.
5. Level-c secrets fix runtime-side (wrapped storage, client-supplied
   npub on create).

## Workstream B — CLI (owner: cli/)

1. `sync.rs` (~1,100 lines) → a ~300-line shim over the code.storage
   HTTP commit builder (commit-without-clone; no local git state, no
   git2 dependency). `--mirror-from` walk-and-copy overlay unchanged
   (fbrain sources are not git).
2. `deploy` = commit + move `live`; `--preview` = ephemeral ref;
   `rollback` = re-point. `manifest-set` = edit file + commit.
   `blob.rs` deleted outright.
3. Client-side npub generation + one-time wrapped-secret transmission.
4. `cli/GUIDE.md`: rewrite "Sync in depth", delete "Blob-first pushes",
   update the command reference and install notes (code.storage host
   config). Runtime agent owns the workflow sections of the same file —
   stay in your lanes.

## Testing (per the engineering style — non-negotiable)

- Every idempotent mutation gets the triple: success, replay, conflicting
  body. The workflow write path (commit from `step.do` under replay) is
  the flagship: prove a replayed run commits exactly once.
- Every state-machine transition gets valid + invalid tests; every
  storage invariant gets a restart test (tree index survives wake;
  pinned SHA never regresses — late webhook must not move the pin
  backward).
- Webhooks follow validate → persist → interpret; no irreversible work
  directly from an external event.
- Cache invalidation as protocol: the tree-index cache documents source
  of truth, invalidation trigger, stale-read behavior.
- Typed errors at crate boundaries; no `anyhow` in the new sync/commit
  modules. `cargo clippy --all-targets -- -D warnings` stays clean.
- Tests run against a local mock of the code.storage HTTP shape (the
  documented API); real-credential integration is flagged, not faked.

## Sequence

1. **Now:** substrate config (bucket SSE, VPS FDE, WireGuard overlay) —
   ops, parallel to everything.
2. **The cut:** workstreams A + B land together, one review, one deploy.
3. **Republish:** meatproxy + linecount as fresh repos + cells (push
   folders, re-set secrets, re-grant, accept new view tokens — solo-user
   blast radius, zero migration code). `strategy-vault`: republish or
   `rm`, owner's call at the time.
4. **Write-up:** self-hoster compliance guide (threat model, what is
   encrypted where, events-as-audit-log) — completes the level-(b/c)
   posture from `docs/explorations/encryption-and-compliance.md`.

## Flagged risks (accepted)

- Workflow write path is the hardest new component — hence the flagship
  test requirement above.
- code.storage latency: accepted on their speed positioning; chatty
  webviews mutate celld state (rooms), not files.
- Webhook delivery is a new failure surface; contained by the poll
  backstop and the events-ledger dedup.
- One big cut means one big debug when something wakes wrong; accepted
  in exchange for never carrying two sync systems.
