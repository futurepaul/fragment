# blobsd dependency audit

Status: ACTIVE audit boundary. Every direct dependency is recorded here with
its exact locked version, crates.io publish date, and why it is acceptable
under the repo rule: **nothing published after 2026-05-22**.

## Audit boundary

blobsd is a standalone crate: fragment-rs has no root Cargo workspace, so
`blobsd/Cargo.lock` is this crate's own reproducibility boundary and is
committed. The table below covers every DIRECT dependency (each pinned `=`
exact). Transitive closure policy:

- Core crates are pinned `=` so no direct dep can drift.
- Network-path transitive crates that landed post-cutoff inside a range were
  force-pinned below the cutoff with `cargo update --precise`: `hyper 1.9.0`
  (2026-03-31), `tokio-macros 2.7.0` (2026-04-03), `hyper-util 0.1.20`
  (2026-02-02).
- Remaining transitive crates (proc-macro/codegen-level: syn, quote, libc,
  etc.) ride the committed lockfile; they do not parse untrusted input and
  their ranges are held fixed by the lock. Any `cargo update` is an audit
  event: re-run the dates below against the diff.

## Version strategy vs celld

The brief asked to mirror celld's locked versions to reuse audit work. celld's
live lock has drifted past the 2026-05-22 cutoff for several core crates; the
binding spec rule wins, so blobsd pins the newest pre-cutoff release of each.
Crates marked "same as celld" are byte-for-byte the versions celld already
audits; the rest are one release behind celld's lock, differing only in
post-cutoff patch releases.

## Direct dependency table

| crate | locked | published | vs celld | justification |
|---|---|---|---|---|
| axum | =0.7.9 | 2024-11-16 | same | HTTP framework; celld's server framework, audit reused |
| tokio | =1.52.3 | 2026-05-08 | celld: 1.53.1 (2026-07-20, post-cutoff) | async runtime; newest pre-cutoff 1.x |
| tower | =0.5.3 (dev) | 2026-01-12 | same | `ServiceExt::oneshot` drives the router in tests |
| object_store | =0.11.2 (aws) | 2024-12-24 | same | S3 client with etag-dialect conditional PUT (S3ConditionalPut::ETagMatch); the bucket abstraction mirrors celld's bucket.rs architecture (ordinary client + no-retry CAS client) |
| rusqlite | =0.31.0 (bundled) | 2024-02-17 | same | descriptor store; bundled libsqlite3-sys 0.28.0 (2024-01-27) matches celld's linked version |
| sha2 | =0.10.9 | 2025-04-30 | same | incremental content hashing |
| base64 | =0.22.1 | 2024-04-30 | same | auth event decode |
| serde | =1.0.228 | 2025-09-27 | celld: 1.0.229 (post-cutoff) | DTOs + event parse |
| serde_json | =1.0.150 | 2026-05-21 | celld: 1.0.151 (post-cutoff) | canonical NIP-01 id rehash + wire JSON; compact serialization parity with JS JSON.stringify is load-bearing for signature verification |
| thiserror | =2.0.18 | 2026-01-18 | celld: 2.0.19 (post-cutoff) | typed error enums (style rule: no anyhow) |
| tracing | =0.1.44 | 2025-12-18 | same | structured logs, one line per request |
| tracing-subscriber | =0.3.23 (env-filter) | 2026-03-13 | same | env-filtered boot logging |
| futures-util | =0.3.32 | 2026-02-15 | celld: 0.3.33 (post-cutoff) | stream adapters over object payloads |
| bytes | =1.11.1 | 2026-02-03 | celld: 1.12.1 (post-cutoff) | chunked body/payload currency |
| http | =1.4.0 | 2025-11-24 | celld: 1.5.0 (post-cutoff) | header/status types |
| http-body | =1.0.1 | 2024-07-12 | celld: 1.1.0 (post-cutoff) | counting-body middleware (bytes_out metric) |
| http-body-util | =0.1.3 | 2025-03-11 | celld: 0.1.4 (post-cutoff) | StreamBody adapter for streamed GET answers |
| secp256k1 | =0.29.1 (std) | 2024-09-06 | NEW (not in celld) | BIP-340 schnorr verification over libsecp256k1 — the same primitive @noble/curves signs with in the CLI/runtime. Verification contexts only in serving code (`Secp256k1::verification_only`); the signing context exists solely for the hidden test-support fixture module. Rust bindings over the most widely audited C crypto library in the Rust ecosystem |
| bech32 | =0.11.0 | 2024-02-23 | NEW (not in celld) | npub (NIP-19) decoding for the allowlist and self-list; the pure-Rust codec maintained by the same org as the @scure/base JS codec the runtime and CLI use, so all three lanes agree on npub semantics |
| tempfile | =3.27.0 | 2026-03-11 | same | stage-file ownership with guaranteed cleanup on every upload path |

## Race guarantee (conditional upload), stated honestly

Object publish is put-if-absent at the PROVIDER: `PutMode::Create` sends
`If-None-Match: *` (etag dialect) through a retries-OFF client, exactly
celld's CAS discipline. Verified live against MinIO
(`tests/live_minio.rs`): a create over an existing key is cleanly rejected
(`PutCreate::AlreadyExists`), never ambiguous.

Known limitation, ledger-tracked: object_store has no streaming CAS payload
in any version — `PutPayload` is buffered by design. Ingest is fully
streamed (hash-while-writing stage file, memory flat, size-capped); only the
publish hop holds one transient copy of staged bytes, bounded by
`BLOBSD_MAX_BLOB_BYTES` and a 4-permit publish semaphore. Convergence
semantics for two concurrent identical uploads are unaffected: the loser's
object write is rejected by the provider, both writers converge on one
descriptor row (`INSERT OR IGNORE`, first writer owns), and both get
byte-identical descriptors. Delete condition for the memory hop: swap to a
marker-lease or streaming-CAS when object_store ships one.

## Not-a-dependency notes

- No `anyhow`: typed error enums only (style rule).
- No `rand`: test fixtures sign deterministically with fixed keys.
- No third-party blossom/nostr server or protocol crate: the blossom
  profile is implemented in-crate per the spec's hard cut.
