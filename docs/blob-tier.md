# The Blob Tier (two-tier storage) — binding contract v1

Status: ACTIVE spec. All lanes implement exactly what is written here.
Deviations = merge rejection. Scope changes go through Paul.

## Principle

Cells are relational: they durably order NAMES (`path -> sha256`) and never
store bodies. Bytes live outside celld entirely, addressed by content hash,
served by `blobsd` (a minimal, idiomatic, conformant Blossom server written
by us). V8 never sits in a blob hot path: large writes go CLI-direct to
blobsd; large reads go browser-direct (public) or stream-through-runtime
(private), never buffered in an isolate.

Wire basis: https://github.com/hzrd149/blossom (BUD-01 subset below).
We run our own conformant implementation. We do NOT depend on any third
party blossom server implementation.

## blobsd (new top-level crate `blobsd/`)

Rust, axum, tokio. Engineering style: borrow finitechat/docs/engineering-style.md
(Tiger Style derivative) — typed error enums (NO anyhow at crate boundary),
explicit limits on every loop/batch/payload/pairing, ~2 paired assertions per
nontrivial function, valid+invalid+replay+restart test shape,
`cargo clippy --all-targets -- -D warnings` clean, no recursion in storage or
protocol paths, explain-why comments on surprising constraints, debt ledger at
`blobsd/docs/technical-debt-ledger.md`.

### Endpoints (the entire surface)

- `PUT /upload` — Blossom-authenticated write.
  Auth: `Authorization: Nostr <base64(kind-24242 event)>`; verify schnorr,
  kind==24242, `t` tag == `upload`, `expiration` tag present and > now,
  `x` tag (server URL) must equal configured `BLOBSD_PUBLIC_URL`.
  Pubkey must be in `BLOBSD_ALLOW_NPUBS` (CSV of npubs, exact match).
  Body limit `BLOBSD_MAX_BLOB_BYTES` (default 67108864 = 64 MiB).
  Idempotent: existing hash => return identical descriptor, do not rewrite.
  sha256 header/body mismatch => 400 `bad_hash`. Success => 200 BlobDescriptor
  `{sha256, size, type, uploaded}`.
- `GET /<64-hex>` — data plane. No auth. Streams (never buffers whole body).
  Supports single-range `Range:` requests (v1: one interval only).
  Headers: `etag: "<sha>"`, `cache-control: public, max-age=31536000, immutable`,
  stored `content-type`.
- `HEAD /<64-hex>` — same headers, no body.
- `DELETE /<64-hex>` — same event auth, `t: delete`, allowlist-checked;
  deletes descriptor row AND object.
- `GET /list/<npub>` — allowlisted self-list only; query `limit<=500`,
  `since=<unix>`; returns array of BlobDescriptors ordered uploaded desc.
- `GET /` — `{name:"blobsd", version}` health.

### Storage

- Bytes: S3-compatible bucket `BLOBSD_BUCKET` (prefix `blobs/<sha>`), key =
  literal lowercase hex sha256. Upload path: stage to local tmpfs file
  (size-capped), hash while streaming to disk, then conditional put.
- Descriptors/ownership: local SQLite `data/blobsd.db`
  `uploads(pubkey TEXT NOT NULL, sha256 TEXT PRIMARY KEY, size INTEGER NOT NULL, mime TEXT NOT NULL DEFAULT '', uploaded_at INTEGER NOT NULL)`
  — foreign-key-free, constrained, WAL mode. `/list` derives from it.
  Corruption policy per style: StoreError enum variant `CorruptState`, fail closed.
- Internal readers (the runtime) authenticate with `Authorization: Bearer ${BLOBSD_INTERNAL_TOKEN}`
  on every request including GET (so private cells can be proxied safely even
  if the instance were exposed). Browser-public reads are governed by
  `BLOBSD_PUBLIC_GET` on the instance level — TRUE for our fleet (accepted
  coarseness; see debt ledger: hashes are unguessable but that is not ACL).

### Config (all validated at boot; invalid = exit non-zero with reason)

`BLOBSD_LISTEN`, `BLOBSD_DATA_DIR`, `BLOBSD_BUCKET`, `S3_ENDPOINT`,
`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION`,
`BLOBSD_PUBLIC_URL`, `BLOBSD_MAX_BLOB_BYTES`, `BLOBSD_ALLOW_NPUBS`,
`BLOBSD_INTERNAL_TOKEN`, `BLOBSD_PUBLIC_GET`.
Cargo deps each recorded with publish-date justification in
`blobsd/docs/deps-audit.md`; nothing published after 2026-05-22.

## Runtime two-tier model (hard cut; nothing shims the old column)

SCHEMA change (both `files` and `draft_files`): drop body storage; rows become
`(path, sha256, size, mime, rev, updated_at, deleted)` exactly — a draft
snapshot is now O(rows) pointer copies and MUST NOT touch byte storage.
History: `file_revisions` stays; the old local `blobs` table DIES (bytes were
already unique-addressed; blossomy tier is the store).

- Commit contract: `PUT /api/f/{n}/file` accepts EITHER raw body with
  `x-fragment-hash` header (runtime verifies hash matches received bytes,
  for callers that have not yet gone blob-first) OR the new ref form
  content-type `application/json` body `{"ref":{"sha256","size","mime"}}`
  where bytes are presumed already in the tier. Both funnel into the SAME
  row-commit turn; revision/notify/watch/event semantics unchanged and
  remain bound to ROW commits only.
- Inline carve-out: bodies <= 65536 bytes may arrive raw WITHOUT hash header;
  the runtime performs the tier upload itself inside the write turn (sync
  fetch to blobsd with internal token) and commits the row. Authors' one-liner
  ergonomics (`ctx.files.write("x.md", "hi")`) survive untouched.
- Oversize raw bodies (>65536 without hash): 413 with hint text pointing at
  blob-first flow (CLI does this automatically).
- Read: `__file` and `ctx.files.read` resolve hash via bloasd loopback stream.
  Server proxies private reads as a stream (heap-flat). When
  `CELLD_VAR_BLOBSD_PUBLIC_GET=1` AND manifest visibility is public or link,
  browsers receive `302` to `${BLOBSD_PUBLIC_URL}/${sha}` instead — V8 exits
  the path entirely. Private: proxied. ctx.files.read additionally enforces
  an 8 MiB decode ceiling (text ops); error hints toward hashed subreads.
- ctx.ai note (acceptance: AI operates on files): workflows fetch file content
  for prompts through ctx.files.read; cap applies identically.
- Watchers/notify: unchanged, row-commit driven (cross-cell acceptance #3).

## CLI

- New generic nostr event signer in scripts/nip98.mjs (kind 24242 builder) —
  reusable for blobsd auth (upload/delete/list variants).
- Push: changed file -> local sha256 -> bloasd HEAD (skip PUT on hit; SIK event
  only when needed) -> commit via ref-form. Concurrency <=4. Pull: materialize
  from bloasd streamed to tmp + atomic rename; repeated pulls of unchanged
  hashes short-circuit off local `.fragment/cache/` (LRU soft-cap 256MB).
- Old wire form removed everywhere (verify/e2e/guide updated; no compat flag).

## Web-hosting conventions (ship in guide + e2e fixtures)

- Fonts: emit `woff2` with `font/woff2` mime; hashes-in-name assets follow
  `name.<8-hex-of-content>.ext` convention documented for authors; runtime
  marks such responses `cache-control: public, max-age=31536000, immutable`
  (heuristic: filename matches `-[0-9a-f]{8}.` segment). Everything else gets
  `max-age=300` for blessed snapshots. MIME map extended: woff/woff2/ttf/otf/
  json/webmanifest/svg-refs/etc.

## Acceptance (must pass against PROD after cutover)

1. Vault "just works": freshly authored vault fragment lists + renders synced
   markdown incl. one >64KB note (blob-first push path).
2. Gallery "just works": `~/dev/finite/fragment-v2/gallery` ships pages +
   images from the tier; bless/rollback re-publish is O(seconds) at scale.
3. Cross-cell notification: upload to A (with notifyUrls->B inbox) lands in
   B's ingest within seconds — row-commit-driven, unchanged.
4. Workflows: cron/trigger workflow reads a >64KB file via ctx.files.read and
   writes a derived output file (both directions proven on blob tier).
5. Web niceties: woff2 font + `[name].[hash8].css` fixture served with correct
   mime + immutable cache headers through the LIVE blessed path.
6. AI-on-files: workflow calls ctx.ai over text pulled from a blob-tier file
   (real call, budgeted prompt) and persists result via ctx.files.write.

## Debt ledger seeds (both lanes must adopt this file format)

- Coarse public GET at instance level; delete condition: per-fragment signed
  read URLs or broker-integrated sidecar ACLs.
- bloasd list rebuild-after-loss caveat (ownership db lost => /list empty;
  objects persist and remain addressable; document recovery = re-upload scan).
