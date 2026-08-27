# blobsd technical debt ledger

Format per engineering-style: observed source, risk, first proof, delete
condition. A shortcut without a delete condition is unfinished design, not
debt — every entry below carries its exit.

---

## Seeded from docs/blob-tier.md (binding spec, both lanes adopt)

### 1. Coarse public GET at instance level

- **Observed source:** `BLOBSD_PUBLIC_GET` gates ALL object reads on the
  instance; there are no per-fragment ACLs. The spec accepts this coarseness
  for the fleet.
- **Risk:** hash-capability is the only protection for "private" bytes on a
  public-GET instance. Sha256 hashes are unguessable, but unguessability is
  not access control: a leaked URL (logs, referrers, screenshots) serves
  bytes to anyone, forever, because the cache headers are immutable.
- **First proof:** `tests/integ.rs::private_instance_gates_reads_on_internal_bearer`
  pins the instance-level gate; public mode has no per-object check by
  design.
- **Delete condition:** per-fragment signed read URLs, or a
  broker-integrated sidecar ACL check in the GET path. Until then, operators
  choosing `BLOBSD_PUBLIC_GET=true` accept fleet-wide read publicity.

### 2. bloasd list rebuild-after-loss caveat

- **Observed source:** `uploads` (SQLite, `data/blobsd.db`) is the ONLY
  ownership/list index. The bucket holds bytes addressed by content hash and
  knows nothing about owners.
- **Risk:** losing the descriptor db empties `/list` and orphans ownership
  forever, while every object stays perfectly addressable and servable by
  hash. Restores from old snapshots can also strand rows whose objects were
  deleted by hash later (drift in the other direction).
- **First proof:** `tests/integ.rs::descriptors_persist_across_app_restart`
  shows list derives purely from the db; `docs/blob-tier.md` storage section
  names the fail direction (bytes survive, list dies).
- **Delete condition:** documented recovery is a re-upload scan: rebuild
  ownership by walking `blobs/*` in the bucket and replaying uploads (or
  add bucket-side ownership metadata + a reconcile job). Implement when the
  fleet first cares who owns orphaned bytes; until then recovery is manual
  and documented here.

---

## Accepted during this lane

### 3. Buffered publish hop (object_store CAS has no streaming payload)

- **Observed source:** `object_store::PutPayload` is buffered in every
  release (checked through 0.13.x, docs verified 2026-08). Ingest is
  streamed (stage file, incremental sha256, flat memory, capped), but the
  conditional object PUT holds one transient copy of staged bytes.
- **Risk:** publish RSS is bounded, not flat:
  `MAX_CONCURRENT_PUBLISHES (4) x BLOBSD_MAX_BLOB_BYTES` worst case
  (~256 MiB at the default cap). A hostile fleet of max-size uploads sits at
  that ceiling, never above it.
- **First proof:** `src/handlers.rs::upload` (chunk-list payload +
  `publish_permits` semaphore); `blobsd/docs/deps-audit.md` "Race guarantee"
  section documents why streaming CAS was unavailable.
- **Delete condition:** object_store ships `PutPayload::from_stream`-style
  CAS, or we adopt a marker-lease (put-if-absent a 0-byte lock, stream the
  object, drop the lock with TTL recovery). Revisit when the publish ceiling
  first shows in production memory profiles.

### 4. Descriptor mutex is process-global (single writer connection)

- **Observed source:** one rusqlite `Connection` behind a std Mutex; every
  query runs on a blocking thread (`spawn_blocking`).
- **Risk:** descriptor throughput is serialized. Irrelevant at current
  write rates (uploads are network-dominated); /list reads also queue behind
  writes.
- **First proof:** `src/db.rs` (`DbInner::lock_conn`); poisoning maps to
  `StoreError::CorruptState` (fail closed, never continue on half-state).
- **Delete condition:** a small reader pool or WAL read snapshots if
  /list latency ever degrades under mixed traffic.

### 5. Unknown-object DELETE returns 404 (absence is truth, no retry idiom)

- **Observed source:** `DELETE /<sha>` on an already-deleted or never-known
  hash answers 404, not 204. A client retrying after an ambiguous 5xx can
  observe 404 on its second attempt.
- **Risk:** CLI treat-404-as-error loops would flag a succeeded delete as
  failed. Mitigated upstream: the CLI's push flow HEADs before mutating, and
  deletion is not retried blind.
- **First proof:** `tests/integ.rs::upload_head_get_range_delete_full_cycle`
  (second DELETE -> 404), README endpoint table.
- **Delete condition:** if the CLI lane needs blind-retry deletes, make the
  second DELETE answer 204 (idempotent-gone) — one-line handler change plus
  test updates. Choose when the caller contract is known, not before.

### 6. GET performs a HEAD + GET pair against the bucket

- **Observed source:** every GET answers `bucket.head` (size + content-type
  authority) before opening the (possibly ranged) stream.
- **Risk:** 2x requests to the bucket backend per object read; on rate- or
  latency-sensitive providers this halves effective read throughput.
- **First proof:** `src/handlers.rs::get_blob`.
- **Delete condition:** derive size from `GetResult.meta` in the same
  request (stream first, set headers from the result) once content-type
  resolution can tolerate attribute absence, or cache object metadata at the
  Caddy layer for the immutable classes.
