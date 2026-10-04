# syntax=docker/dockerfile:1.7
# Our Hermes computer image (docs/computers.md; decisions 15, 18, 21; spike
# S3b's boot list). Everything Hermes-specific lives here, never in the
# platform: Hermes v0.21.5's desktop image, the bridge as its Relay
# connector, hermes-boot around it, Litestream, sandbox-shim, the screen.
# The image holds no secret.
#
# Build from the repo root (it carries the fragment CLI, of the root
# workspace; .dockerignore keeps the context to what the build reads):
#   docker build --platform linux/amd64 -f images/hermes/Dockerfile .
# (or linux/arm64: Hermes' image is published for both, and every stage
# here is built for the platform asked for; docs/self-host.md, seam 2).

ARG HERMES=nousresearch/hermes-agent:v2026.9.24-desktop

# sandbox-shim, as images/stub/Dockerfile takes it: Cloudflare's build for
# amd64, and for arm64 built from the same source.
FROM docker.io/cloudflare/sandbox:1.0.0 AS shim-amd64
FROM rust:1.99-alpine AS shim-arm64
RUN apk add --no-cache build-base
ADD https://github.com/cloudflare/sandbox-sdk.git#f9e972a14123bef3b93e9bb337bab6e83419fe7e /src
WORKDIR /src
RUN cargo rustc --release --locked --package sandbox-tools --bin sandbox-shim -- -C target-feature=+crt-static \
 && mkdir -p /usr/local/bin && cp target/release/sandbox-shim /usr/local/bin/sandbox-shim
FROM shim-${TARGETARCH} AS shim

# Litestream's release for the platform, pinned by hash.
FROM scratch AS litestream-amd64
ADD --checksum=sha256:cfb371176d164437ae869f8351cfde49bd1804ae71c61923f75c9cba9c9c006d https://github.com/benbjohnson/litestream/releases/download/v0.5.17/litestream-0.5.17-linux-x86_64.tar.gz /litestream.tgz
FROM scratch AS litestream-arm64
ADD --checksum=sha256:f8ca4a050095c1efbda2c4365172e61bf9d955ea0d9ac42f448b52e51819baa5 https://github.com/benbjohnson/litestream/releases/download/v0.5.17/litestream-0.5.17-linux-arm64.tar.gz /litestream.tgz
FROM litestream-${TARGETARCH} AS litestream

# The bridge and hermes-boot, static (Alpine's Rust targets musl): the
# images' own workspace.
# A target cache is every build's on this Docker, another checkout's
# included, and cargo takes a path crate as built when its sources are older
# than its last build: the sources copied in are touched first, so a build
# of other sources since never stands for these (the step runs only when
# they changed).
FROM rust:1.99-alpine AS build
WORKDIR /src
COPY images/Cargo.toml images/Cargo.lock ./
COPY images/bridge bridge
COPY images/hermes/boot hermes/boot
RUN --mount=type=cache,id=images-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=images-target,target=/src/target \
    find bridge hermes/boot -type f -exec touch {} + \
 && cargo build --release --locked -p fragment-bridge -p hermes-boot \
 && mkdir -p /out && cp target/release/fragment-bridge target/release/hermes-boot /out/

# The fragment CLI (cli/), static, for the agents' terminals: it acts as the
# agent its profile names, through the computer's API egress, with no key
# (cli/GUIDE.md, "As an agent"; docs/computers.md). Its sources are touched
# as the build stage's are.
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

# noVNC, the screen page's RFB client (pinned by hash).
FROM alpine:3.20 AS novnc
ADD --checksum=sha256:b1003a11b6e6e8d8f7f5e5586daae7f8ca651d8aee0aa155ff9ac841c48f52c6 https://github.com/novnc/noVNC/archive/refs/tags/v1.7.0.tar.gz /novnc.tgz
RUN mkdir /novnc && tar -xzf /novnc.tgz -C /novnc --strip-components=1

FROM ${HERMES} AS hermes
COPY --from=shim /usr/local/bin/sandbox-shim /usr/local/bin/sandbox-shim
COPY --from=litestream /litestream.tgz /tmp/litestream.tgz
RUN tar -C /usr/local/bin -xzf /tmp/litestream.tgz litestream && rm /tmp/litestream.tgz
# cua-driver, the backend of Hermes' computer_use (screenshots of the agent's
# desktop, XTest input): without it Hermes offers the agent no computer_use.
# Pinned (the release current at Hermes v0.21.5's), named to Hermes by
# HERMES_CUA_DRIVER_CMD below, so Hermes never runs its upstream installer.
ADD --checksum=sha256:51de56e37e1e57cca613cff06e22a3c64bf8285ed97e518660f3d1cb2a4e6fcf https://github.com/trycua/cua/releases/download/cua-driver-rs-v0.28.3/cua-driver-rs-0.28.3-linux-x86_64-binary.tar.gz /tmp/cua-driver.tgz
RUN tar -C /usr/local/bin -xzf /tmp/cua-driver.tgz cua-driver && rm /tmp/cua-driver.tgz \
 && CUA_DRIVER_RS_TELEMETRY_ENABLED=false CUA_DRIVER_RS_UPDATE_CHECK=false /usr/local/bin/cua-driver --version | grep -qx 'cua-driver 0\.28\.3'
# gws, Google's Workspace CLI (github.com/googleworkspace/cli), pinned. Its
# wrapper hands it the agent's Google connection: the placeholder in
# GOOGLE_OAUTH_ACCESS_TOKEN as its access token (GOOGLE_WORKSPACE_CLI_TOKEN),
# which the computer's egress swaps for the real token on Google's hosts.
ADD --checksum=sha256:4db473dde4b1ab872e4ff35d769b0d4af1f1a6441a605e79d5cf8ada9c87e920 https://github.com/googleworkspace/cli/releases/download/v0.22.5/google-workspace-cli-x86_64-unknown-linux-musl.tar.gz /tmp/gws.tgz
RUN mkdir -p /opt/gws && tar -C /opt/gws -xzf /tmp/gws.tgz ./gws && rm /tmp/gws.tgz \
 && printf '%s\n' '#!/bin/sh' '# gws with the agent'"'"'s Google connection (images/hermes/Dockerfile).' \
      '[ -n "$GOOGLE_WORKSPACE_CLI_TOKEN" ] || [ -z "$GOOGLE_OAUTH_ACCESS_TOKEN" ] || export GOOGLE_WORKSPACE_CLI_TOKEN="$GOOGLE_OAUTH_ACCESS_TOKEN"' \
      'exec /opt/gws/gws "$@"' > /usr/local/bin/gws \
 && chmod 0755 /usr/local/bin/gws \
 && /usr/local/bin/gws --version | grep -q '0\.22\.5'
COPY --from=build /out/ /opt/fragment/bin/
# The check the platform runs after it restores /data, before the gate
# (docs/computers.md): the last hold's copies of Hermes' databases put back,
# then each one's quick_check (hermes-boot's held.rs).
RUN printf '#!/bin/sh\n# The platform runs this after a restore, before the gate (docs/computers.md).\nexec /opt/fragment/bin/hermes-boot check-restore\n' > /usr/local/bin/computer-check \
 && chmod 755 /usr/local/bin/computer-check
COPY --from=cli /out/fragment /usr/local/bin/fragment
COPY images/hermes/preload.py /opt/fragment/preload.py
COPY images/hermes/screen /opt/fragment/screen
COPY --from=novnc /novnc/core /opt/fragment/screen/novnc/core
COPY --from=novnc /novnc/vendor /opt/fragment/screen/novnc/vendor

# Hermes' home is under /data, the only directory a computer keeps. Its
# setup reads $HERMES_HOME; three of its scripts name /opt/data outright.
RUN sed -i 's#/opt/data#/data/hermes#g' /opt/hermes/docker/main-wrapper.sh /opt/hermes/docker/hermes-exec-shim.sh /etc/s6-overlay/s6-rc.d/dashboard/run \
 && ! grep -q '/opt/data' /opt/hermes/docker/main-wrapper.sh /opt/hermes/docker/hermes-exec-shim.sh /etc/s6-overlay/s6-rc.d/dashboard/run

# Setup gated on stamps (S3b, cut d): the config migration and the bundled
# skills sync run once per (Hermes revision, their input). The build fails
# if upstream's script no longer reads as patched.
RUN sed -i \
      -e 's#^    s6-setuidgid hermes "\$INSTALL_DIR/.venv/bin/python" "\$INSTALL_DIR/scripts/docker_config_migrate.py" \\$#    /opt/fragment/bin/hermes-boot stamped migrate "$HERMES_HOME/config.yaml" -- s6-setuidgid hermes "$INSTALL_DIR/.venv/bin/python" "$INSTALL_DIR/scripts/docker_config_migrate.py" \\#' \
      -e 's#^    as_hermes "\$INSTALL_DIR/.venv/bin/python" "\$INSTALL_DIR/tools/skills_sync.py" \\$#    /opt/fragment/bin/hermes-boot stamped skills "$HERMES_HOME/skills/.bundled_manifest" -- s6-setuidgid hermes "$INSTALL_DIR/.venv/bin/python" "$INSTALL_DIR/tools/skills_sync.py" \\#' \
      /opt/hermes/docker/stage2-hook.sh \
 && test "$(grep -c 'hermes-boot stamped' /opt/hermes/docker/stage2-hook.sh)" = 2

# The gateway is hermes-boot's main program (preloaded): no per-profile s6
# gateway services, which would start a second gateway (S3b, cut b).
RUN printf '#!/bin/sh\n# hermes-boot runs the one gateway itself (images/hermes).\nexit 0\n' > /etc/cont-init.d/02-reconcile-profiles

# Bytecode in the image (S3b, cut a: -6.2 s). unchecked-hash: /opt/hermes is
# sealed, so nothing can make a .pyc stale.
# A few vendored files are not Python 3 (compileall exits 1 for them); the
# count proves the rest compiled.
RUN /opt/hermes/.venv/bin/python -m compileall -q -j0 --invalidation-mode unchecked-hash /opt/hermes > /dev/null || true; \
    test "$(find /opt/hermes -name '*.pyc' | wc -l)" -gt 8000

# What the boot reads instead of finding it each time: the lean plugin list,
# Hermes' revision, the browser's path; the platform skill every profile
# has, from the fragment CLI above (`fragment skill`: boot/src/skills.rs);
# and the files the gateway loads, read ahead on the second CPU while /data
# is restored (S3b, cut h).
RUN /opt/fragment/bin/hermes-boot build-info \
 && mkdir -p /tmp/rh \
 && HERMES_HOME=/tmp/rh HOME=/tmp/rh /opt/hermes/.venv/bin/python -c "import sys, os, gateway.run, hermes_cli.gateway, gateway.relay.adapter, gateway.relay.ws_transport; fs = {m.__cached__ for m in list(sys.modules.values()) if getattr(m, '__cached__', None)}; fs |= {l.split()[-1] for l in open('/proc/self/maps') if '.so' in l}; open('/opt/fragment/readahead.list', 'w').write(chr(10).join(sorted(f for f in fs if os.path.isfile(f))) + chr(10))" \
 && rm -rf /tmp/rh \
 && mkdir -p /run/computer /var/lib/fragment-run

# One layer (S3b, cut i: -0.2 s per warm start); the image's settings are
# restated, without upstream's VOLUME.
FROM scratch
COPY --from=hermes / /
ENV PATH="/opt/hermes/bin:/opt/hermes/.venv/bin:/data/hermes/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PLAYWRIGHT_BROWSERS_PATH=/opt/hermes/.playwright \
    npm_config_install_links=false \
    HERMES_WEB_DIST=/opt/hermes/hermes_cli/web_dist \
    HERMES_TUI_DIR=/opt/hermes/ui-tui \
    HERMES_HOME=/data/hermes \
    HERMES_WRITE_SAFE_ROOT=/data/hermes \
    HERMES_DISABLE_LAZY_INSTALLS=1 \
    HERMES_LAZY_INSTALL_TARGET=/data/hermes/lazy-packages \
    XDG_RUNTIME_DIR=/tmp/hermes-runtime \
    SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt \
    REQUESTS_CA_BUNDLE=/etc/ssl/certs/ca-certificates.crt \
    S6_CMD_RECEIVE_SIGNALS=1 \
    S6_SERVICES_GRACETIME=1000 \
    S6_KILL_GRACETIME=1000 \
    HERMES_CUA_DRIVER_CMD=/usr/local/bin/cua-driver \
    CUA_DRIVER_RS_TELEMETRY_ENABLED=false \
    CUA_DRIVER_RS_UPDATE_CHECK=false \
    FRAGMENT_API=http://api.fragment.internal \
    FRAGMENT_MODEL=http://model.fragment.internal
WORKDIR /opt/hermes
# which build this is, served beside the screen (the platform's upgrade and
# rollback lane reads it through the computer's port): a layer of its own,
# so another build shares the one above
ARG IMAGE_VERSION=1
RUN echo "$IMAGE_VERSION" > /opt/fragment/screen/version.txt
EXPOSE 6080
ENTRYPOINT ["/opt/fragment/bin/hermes-boot", "pre-init"]
