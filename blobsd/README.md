# blobsd

The blob tier's server: a minimal, idiomatic, Blossom-conformant blob store
per `docs/blob-tier.md` (binding contract). Bytes are content-addressed in an
S3-compatible bucket (`blobs/<sha256>`); descriptors and ownership live in a
local SQLite database. Cells never touch blob bytes.

**Standalone crate:** fragment-rs has no root Cargo workspace, so blobsd
keeps its own `Cargo.lock` and is intentionally NOT wired into any
fragment-rs build. Build/test it from this directory.

## Run local (MinIO dev stack)

Bring up the same MinIO the fragment dev environment uses:

```sh
scripts/dev up        # from the repo root: MinIO :9000, bucket s3://fragment-dev
```

or just MinIO:

```sh
docker run -d --name fragment-dev-minio -p 127.0.0.1:9000:9000 \
  -v "$PWD/.dev/minio-data:/data" \
  -e MINIO_ROOT_USER=minio -e MINIO_ROOT_PASSWORD=miniosecret \
  minio/minio:RELEASE.2025-07-23T15-54-02Z server /data
# create the bucket once (mc or rclone — either is fine):
docker run --rm --network host --entrypoint /bin/sh minio/mc:latest -c "
  mc alias set dev http://127.0.0.1:9000 minio miniosecret && mc mb --ignore-existing dev/fragment-dev"
```

Then:

```sh
cd blobsd
export BLOBSD_LISTEN=127.0.0.1:8090
export BLOBSD_DATA_DIR=./data
export BLOBSD_PUBLIC_URL=http://127.0.0.1:8090
export BLOBSD_BUCKET=fragment-dev
export S3_ENDPOINT=http://127.0.0.1:9000
export AWS_ACCESS_KEY_ID=minio
export AWS_SECRET_ACCESS_KEY=miniosecret
export AWS_REGION=us-east-1
export BLOBSD_ALLOW_NPUBS=npub1...            # CSV of allowed signers
export BLOBSD_INTERNAL_TOKEN=$(openssl rand -hex 32)
export BLOBSD_PUBLIC_GET=true
cargo run
```

Invalid or unknown `BLOBSD_*` configuration exits non-zero with one human
reason on stderr — there are no silent defaults beyond
`BLOBSD_MAX_BLOB_BYTES` (spec default 64 MiB).

## Config surface

| variable | meaning |
|---|---|
| `BLOBSD_LISTEN` | socket address to serve on |
| `BLOBSD_DATA_DIR` | descriptor db + upload staging dir (created) |
| `BLOBSD_BUCKET` | S3 bucket name (objects under `blobs/<sha256>`) |
| `S3_ENDPOINT` | S3-compatible endpoint (path-style always) |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` / `AWS_REGION` | S3 credentials |
| `BLOBSD_PUBLIC_URL` | exact string auth-event `x` tags must equal (no trailing slash) |
| `BLOBSD_MAX_BLOB_BYTES` | upload ceiling, default 67108864 |
| `BLOBSD_ALLOW_NPUBS` | CSV of npubs allowed to upload/delete/list |
| `BLOBSD_INTERNAL_TOKEN` | bearer token for private-instance reads (>= 16 chars) |
| `BLOBSD_PUBLIC_GET` | `true`/`false`: serve object reads without the bearer token |

## Endpoints (the entire surface)

| endpoint | auth | notes |
|---|---|---|
| `PUT /upload` | `Authorization: Nostr <base64 kind-24242 event>`, `t=upload` | idempotent on hash: existing descriptor returned verbatim, no rewrite. Optional `?sha256=` / `x-sha256` cross-checked, mismatch = 400 `bad_hash` |
| `GET /<64-hex>` | bearer token unless `BLOBSD_PUBLIC_GET` | streamed, never buffered; single-range `Range:` (206 + `content-range`), multi-range 400, unsatisfiable 416; `etag: "<sha>"`, immutable cache headers |
| `HEAD /<64-hex>` | same gate | same headers, no body |
| `DELETE /<64-hex>` | Nostr event, `t=delete` | deletes descriptor row AND object; unknown hash 404 |
| `GET /list/<npub>` | Nostr event, `t=list`, self-list only | `?limit<=500` (default 100), `?since=<unix>`; descriptors ordered uploaded desc |
| `GET /` | none | `{"name":"blobsd","version":...}` |

Auth events: kind 24242, `t` verb tag, `x` = `BLOBSD_PUBLIC_URL` exactly,
`expiration` > now; id must hash the canonical NIP-01 serialization and the
schnorr signature must verify; signer must be in `BLOBSD_ALLOW_NPUBS`.
Structural failures answer 401, permission failures 403 — codes are stable
JSON: `{"error":"bad_signature"}` etc.

## Observability

One tracing line per request: `{method, path, status, bytes_in, bytes_out,
us}` (hash paths shrink to an 8-hex prefix). Control-plane events
(upload committed / replayed / converged / deleted, deletes) log struct-level
afterwards. `RUST_LOG` filters (default `info`).

## Tests

```sh
cargo test --all-targets                                   # unit + integration (in-memory bucket)
cargo test --test live_minio -- --ignored --nocapture      # LIVE: writes to the real bucket
cargo clippy --all-targets -- -D warnings
```

The live test needs the dev MinIO (`scripts/dev up`) and covers: upload ->
object really in MinIO -> streamed GET -> replay identity -> provider-enforced
put-if-absent -> concurrent identical uploads converge -> DELETE removes the
object.

## Deploy

`deploy/blobsd.service.in` is the systemd unit template (fill `@DATA_DIR@` /
environment file path). Behind Caddy, reverse-proxy the public URL; blobs are
immutable so caching is safe and encouraged:

```Caddyfile
# Caddy snippet for a blobsd public host
blobsd.example.com {
    # immutable content-addressed objects: cache hard
    header /Cache-Control "public, max-age=31536000, immutable"
    reverse_proxy 127.0.0.1:8090
}
```

## Status

Deviations from the spec and tolerated debt live in
`docs/technical-debt-ledger.md`; dependency audit with publish-date
justifications in `docs/deps-audit.md`.
