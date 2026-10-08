# syntax=docker/dockerfile:1.7
# Our goose computer image (docs/computers.md; docs/optchat.md, "goose on
# the computer"): goose as each agent's hands, the bridge with its `goose`
# runtime between it and the fragment API (docs/bridge.md), the fragment CLI
# (its agents' `fragment`, and a mind's `fragment mcp`), sandbox-shim, the
# tools an agent reaches for, and each agent's desktop (fragment-desktop:
# Xvnc, matchbox and a Chromium its browser tools drive, which its owner
# watches on its screen). Everything runs as root: the computer is one
# person's VM with no secret in it (decisions 13, 43, 44), and what an agent
# installs outside /data lasts until the next start from this image. The
# image holds no secret.
#
# Build from the repo root (it carries the fragment CLI, of the root
# workspace; the root .dockerignore keeps the context to what it reads):
#   docker build --platform linux/amd64 -f images/goose/Dockerfile .

# The bridge and fragment-desktop, static (Alpine's Rust targets musl): the
# images' own workspace. A target cache is every build's on this Docker,
# and cargo takes a path crate as built when its sources are older than its
# last build: the sources copied in are touched first.
FROM rust:1.99-alpine AS build
WORKDIR /src
COPY images/Cargo.toml images/Cargo.lock ./
COPY images/bridge bridge
COPY images/goose/desktop goose/desktop
RUN --mount=type=cache,id=images-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=images-target,target=/src/target \
    find bridge goose/desktop -type f -exec touch {} + \
 && cargo build --release --locked -p fragment-bridge -p goose-desktop \
 && mkdir -p /out && cp target/release/fragment-bridge target/release/fragment-desktop /out/

# The fragment CLI (cli/), static: in an agent's shell it acts as the agent
# goose runs for (FRAGMENT_AS_AGENT, through the computer's API egress, no
# key: cli/GUIDE.md, "As an agent"), and `fragment mcp <mind>` serves a
# mind's view, zoom, date and search to goose (the bridge adds it to a
# mind's sessions: BRIDGE_GOOSE_CLI).
FROM rust:1.99-alpine AS cli
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY cli cli
COPY crates crates
COPY templates templates
COPY xtask xtask
RUN --mount=type=cache,id=cli-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cli-target,target=/src/target \
    find cli crates templates -type f -exec touch {} + \
 && cargo build --release --locked -p fragment-cli \
 && mkdir -p /out && cp target/release/fragment /out/

# goose: our fork, futurepaul/goose `fragment/optmem` (upstream v1.53.0 and
# the surgery of docs/optchat.md, "The goose fork": GOOSE_NO_COMPACTION,
# GOOSE_STABLE_SYSTEM_PROMPT), pinned by its commit. Its rust-toolchain.toml
# pins its Rust (rustup fetches it). The portable features: no local
# inference, nothing past libc, libm and libgcc_s at run time.
FROM rust:1-trixie AS goose
RUN apt-get -o Acquire::Retries=3 update \
 && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
      build-essential clang cmake libclang-dev libdbus-1-dev libssl-dev libxcb1-dev pkg-config protobuf-compiler
ADD --keep-git-dir=false https://github.com/futurepaul/goose.git#4cfb2d7dcc441e703bec0918ba8c4b71c01fd13d /src
WORKDIR /src
RUN --mount=type=cache,id=goose-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=goose-target,target=/src/target \
    CARGO_PROFILE_RELEASE_STRIP=symbols cargo build --release --locked -p goose-cli --bin goose --no-default-features --features portable-default \
 && mkdir /out && cp target/release/goose /out/ \
 && /out/goose --version | grep -q '^ *1\.53\.0$'

# noVNC, the screen page's RFB client (pinned by hash).
FROM alpine:3.20 AS novnc
ADD --checksum=sha256:b1003a11b6e6e8d8f7f5e5586daae7f8ca651d8aee0aa155ff9ac841c48f52c6 https://github.com/novnc/noVNC/archive/refs/tags/v1.7.0.tar.gz /novnc.tgz
RUN mkdir /novnc && tar -xzf /novnc.tgz -C /novnc --strip-components=1

# The browser tools: Microsoft's Playwright MCP, pinned by its lock, which
# attaches over CDP to the Chromium on each agent's desktop (so it needs no
# browser of its own: none is downloaded), and the Node it runs on (the
# repo's pinned Node: crates/devstack/src/node_release.rs).
FROM node:24.21.0-trixie-slim AS browser
WORKDIR /opt/fragment/browser
COPY images/goose/browser/package.json images/goose/browser/package-lock.json ./
RUN PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm ci --omit=dev --no-audit --no-fund \
 && node node_modules/@playwright/mcp/cli.js --version

# cua-driver, the desktop's keyboard, mouse and screen tools as MCP (the
# release Hermes' computer_use pinned), pinned by hash.
FROM debian:trixie-slim AS cua
ADD --checksum=sha256:51de56e37e1e57cca613cff06e22a3c64bf8285ed97e518660f3d1cb2a4e6fcf https://github.com/trycua/cua/releases/download/cua-driver-rs-v0.28.3/cua-driver-rs-0.28.3-linux-x86_64-binary.tar.gz /tmp/cua-driver.tgz
RUN mkdir /out && tar -C /out -xzf /tmp/cua-driver.tgz cua-driver

# What an agent works with: git, curl, python3, ripgrep, jq, and procps;
# its desktop: TigerVNC's Xvnc (RFB on a Unix socket, no TCP), matchbox
# (each window full screen), Chromium (asked nothing at its first run: its
# policy below), fonts, xdotool (screen_click's clicks); tini as PID 1, which
# reaps what an agent's shell leaves behind (the bridge is its child, and
# handles SIGTERM itself: gone within 3 s), and each desktop's supervisor,
# detached once it starts. apt's lists are kept, fresh as of this build, so
# `apt-get install` works at once.
FROM debian:trixie-slim
RUN apt-get -o Acquire::Retries=3 update \
 && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
      ca-certificates curl git jq procps python3 ripgrep tini \
      tigervnc-standalone-server matchbox-window-manager chromium fonts-dejavu-core fonts-liberation2 xdotool \
 && mkdir -p /run/computer /run/desktop /etc/chromium/policies/managed
COPY images/goose/chromium-policy.json /etc/chromium/policies/managed/fragment.json
COPY --from=docker.io/cloudflare/sandbox:1.0.0 /usr/local/bin/sandbox-shim /usr/local/bin/sandbox-shim
COPY --from=build /out/fragment-bridge /out/fragment-desktop /usr/local/bin/
COPY --from=cli /out/fragment /usr/local/bin/fragment
COPY --from=goose /out/goose /usr/local/bin/goose
COPY --from=browser /usr/local/bin/node /usr/local/bin/node
COPY --from=browser /opt/fragment/browser /opt/fragment/browser
COPY --from=cua /out/cua-driver /usr/local/bin/cua-driver
COPY images/goose/screen /opt/fragment/screen
COPY --from=novnc /novnc/core /opt/fragment/screen/novnc/core
COPY --from=novnc /novnc/vendor /opt/fragment/screen/novnc/vendor
# The bridge's settings (docs/bridge.md). Each agent's goose keeps its own
# state under /tmp/goose (a turn is a fresh session, never loaded again),
# its tools work in /data/work with HOME /data/work/home (docs/computers.md,
# the seam), and the interception CA is appended to the system's bundle once
# it appears (docs/computers.md, "Connections and operator keys"). Each
# agent's desktop is under /run/desktop (never /data: nothing of a desktop,
# its browser's profile included, is in a save), its screen named there by
# convention, and started by its screen's first viewer or its first tool.
ENV BRIDGE_RUNTIME=goose \
    BRIDGE_STATE_DIR=/data/bridge \
    BRIDGE_MEDIA_DIR=/tmp/bridge-media \
    BRIDGE_GOOSE_BIN=/usr/local/bin/goose \
    BRIDGE_GOOSE_CLI=/usr/local/bin/fragment \
    BRIDGE_GOOSE_DESKTOP=/usr/local/bin/fragment-desktop \
    BRIDGE_GOOSE_SKILLS=1 \
    BRIDGE_GOOSE_ROOT=/tmp/goose \
    BRIDGE_GOOSE_WORK=/data/work \
    BRIDGE_GOOSE_HOME=/data/work/home \
    BRIDGE_TRUST_CA=/etc/cloudflare/certs/cloudflare-containers-ca.crt \
    BRIDGE_SCREEN_LISTEN=0.0.0.0:6080 \
    BRIDGE_SCREEN_DIR=/opt/fragment/screen \
    BRIDGE_SCREENS_DIR=/run/desktop \
    BRIDGE_SCREEN_START="/usr/local/bin/fragment-desktop start" \
    FRAGMENT_DESKTOPS=/run/desktop \
    FRAGMENT_API=http://api.fragment.internal \
    FRAGMENT_MODEL=http://model.fragment.internal \
    PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 \
    CUA_DRIVER_RS_TELEMETRY_ENABLED=false \
    CUA_DRIVER_RS_UPDATE_CHECK=false \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
# which build this is, served beside the screen (the platform's upgrade and
# rollback lane reads it through the computer's port): a layer of its own
ARG IMAGE_VERSION=1
RUN echo "$IMAGE_VERSION" > /opt/fragment/screen/version.txt
EXPOSE 6080
# no /data: the platform's restore makes it (EXDEV under overlayfs for a
# directory from an image layer)
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/fragment-bridge", "run"]
