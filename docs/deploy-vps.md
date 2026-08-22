# Deploying fragment to a VPS (the friends test)

The shape: one Linux VPS running one celld node backed by a real bucket,
Caddy in front for TLS. This is the celld self-hosting story in miniature —
the same thing an org would do internally, at toy scale.

Target of record: a fresh Latitude VPS, a Latitude object-storage bucket,
and `fragment.club`. Swap names freely; nothing here is Latitude-specific
except the endpoint URL.

## What ends up where

```
browser/CLI ──https/wss──▶ Caddy :443 ──▶ celld :8080 (loopback only)
                                              │
                                              ▼
                                  Latitude bucket (s3://…)
                                  = state, coordination, backup
```

- **celld** listens on `127.0.0.1:8080` (public Worker listener, loopback —
  Caddy is the only exposed surface) and spawns its internal listener on a
  loopback port (single-node fleet; the operator API must never be public).
- **State** lives in the bucket. The local watch dir (`/var/lib/celld/watch`)
  is celld's fast-local index of that bucket — it must persist across
  restarts and never be reused against a different bucket (zombie cells; we
  hit this locally, see README).
- **Backups** = the bucket. Point `sqlite3`/`rclone` at it whenever you want
  a snapshot. This is the celld pitch: the bucket is the source of truth.

## 0. Before touching the VPS

- **DNS**: `fragment.club` A record → VPS IP. `www` CNAME → `fragment.club`.
  Do this first so Caddy's ACME issuance works on the first try.
- **Latitude bucket**: created with a name you like (say `fragment-prod`).
  Note the endpoint (Standard class: `https://<account-id>.s3.latitude.sh` —
  the account ID is in the Object storage sidebar) and create an access key
  (Access keys tab; limited scope to this bucket, read-write).

**One honest risk — now resolved for one provider**: celld requires a store
with **conditional writes and read-after-write consistency** (S3, R2, GCS,
Tigris, Azure Blob qualify). **Latitude object storage fails the probe**
(verified 2026-08-21 on the high-performance class: it accepts
`If-Match`/`If-None-Match` but does not enforce them; Object Lock is no
substitute — WORM can't fence writes and would block celld's segment GC).
fragment.club therefore runs on **Cloudflare R2** (`AWS_REGION=auto`), which
passes. Everything else in this runbook is provider-agnostic.

## 1. Base system (as root)

```sh
apt update && apt -y upgrade
apt -y install curl rsync nodejs npm   # node/npm for runtime deps + esbuild
useradd --system --home /var/lib/celld --shell /usr/sbin/nologin celld
```

(If your distro's node is ancient, use nvm/nodesource — esbuild needs a
modern-ish node only to install.)

**Caddy** (official repo):

```sh
apt -y install debian-keyring debian-archive-keyring apt-transport-https
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | tee /etc/apt/sources.list.d/caddy-stable.list
apt update && apt -y install caddy
```

**celld**:

```sh
curl -fsSL https://celld.dev/install.sh | sh    # → ~/.local/bin/celld
cp ~/.local/bin/celld /usr/local/bin/celld
```

Pin the version you verified with (`celld --version`; 0.3.0 as of writing)
and note it somewhere — alpha software, upgrades are not always rolling.

**Firewall**: allow 22, 80, 443. Nothing else. (Latitude also has a
firewall in its dashboard — set both to the same policy.)

```sh
apt -y install ufw && ufw allow 22/tcp && ufw allow 80/tcp && ufw allow 443/tcp && ufw enable
```

## 2. Bucket credentials

The access key goes in exactly one place on the VPS:

```sh
install -m 600 /dev/null /etc/celld.env
editor /etc/celld.env        # fill from deploy/env.example
```

`/etc/celld.env` holds `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
`AWS_REGION`, and (recommended) `FRAGMENT_HOST_SECRET` — generate with
`openssl rand -hex 32`. Root-only, never committed, never pasted anywhere.
The systemd unit reads it via `EnvironmentFile=`.

## 3. celld service

```sh
cp deploy/celld.service /etc/systemd/system/
# edit: set --bucket s3://<your-bucket> and --endpoint https://<account-id>.s3.latitude.sh
systemctl daemon-reload
systemctl enable --now celld
journalctl -u celld -f      # watch the bucket probe + restore
```

First boot runs the conditional-write probe. It either passes (Latitude
storage qualifies — proceed) or refuses (fall back to R2, §0). celld with an
empty bucket deploys nothing yet — that's fine; it's waiting for a runtime.

## 4. Caddy

```sh
cp deploy/Caddyfile /etc/caddy/Caddyfile   # (root owns fragment.club)
systemctl reload caddy
curl -s https://fragment.club/__internal/ping   # → pong (celld answers)
```

(DNS not propagated yet? `curl --resolve fragment.club:443:127.0.0.1 ...`)

## 5. Deploy the runtime

The repo only lives on your Mac so far; rsync it up (or push to a private
remote once one exists):

```sh
# from the Mac, repo root
rsync -a --delete \
  --exclude .git --exclude .dev --exclude target --exclude node_modules \
  ./ VPS_USER@fragment.club:fragment-rs/
```

On the VPS:

```sh
cd ~/fragment-rs/runtime && npm install --no-audit --no-fund
cd ~/fragment-rs && npm i -g esbuild   # or apt/pnpm — just needs to be on PATH
set -a; . /etc/celld.env; set +a
celld deploy runtime --bucket s3://<your-bucket> --endpoint https://<account-id>.s3.latitude.sh
systemctl restart celld     # celld loads deployments at startup
```

`celld deploy` bundles `runtime/` with esbuild and writes it to the bucket —
the Mac never needs bucket credentials.

## 6. Point the CLI and ship something

On your Mac (fresh world — the local azurite world does not travel):

```sh
fragment --host https://fragment.club create buddy-wire
cd ../test-fragment-out                 # the codex-built chat app
fragment --host https://fragment.club sync buddy-wire --dir .
fragment --host https://fragment.club publish buddy-wire --note v1
fragment --host https://fragment.club bless buddy-wire <slug>
```

…or set `FRAGMENT_HOST=https://fragment.club` once and drop the flags.

https://fragment.club/f/buddy-wire/ is now a live, multiplayer AIM-style
chat. That URL is the whole friends test for browser-only friends.

## 7. Friends

- **Viewers (browser)**: for `visibility: public` fragments, just send the
  URL. For private ones, `fragment open <name>` prints a `?view=` token link.
- **Collaborators (CLI)**: they need the `fragment` binary —
  `cargo build --release` from this repo for now (release artifacts and an
  install script once this goes public). Then:
  ```sh
  fragment login                 # generates their npub
  fragment whoami                # they send you this npub
  # you:
  fragment grant <name> --editor npub1…   # or --viewer
  ```
- **No invites, no accounts**: npub + NIP-98 per request, or unguessable
  view tokens. That's the entire permission model.

## 8. Operating notes

- **Restarting celld** is safe: SIGTERM drains (25s), cells restore from the
  bucket, zero acknowledged writes lost. Rooms clients reconnect
  automatically (`__rt.js`).
- **Upgrading the runtime** = re-run `celld deploy` + restart. Fragments are
  data; the runtime is shared machinery.
- **Backups** = bucket. Versioning on the Latitude bucket is a cheap extra
  safety net if it's off by default.
- **This fleet is one trust domain** (celld alpha): friends only, no hostile
  strangers, nothing sensitive in secrets.
- **Single node**: if the VPS dies, stand up a new one against the same
  bucket — cells and deployments come back from the bucket (after the lease
  expires, ~20s). The bucket outlives the node.
- **The watch dir is paired to the bucket** (§ What ends up where). New
  node, new watch dir is automatic (`/var/lib/celld/watch` is born empty);
  just never point an existing watch dir at a different bucket.
