# syntax=docker/dockerfile:1.7
# Our Hermes computer image (docs/computers.md; decisions 15, 18, 21; spike
# S3b's boot list). Everything Hermes-specific lives here, never in the
# platform: Hermes' desktop image, the bridge as its Relay connector,
# hermes-boot around it, sandbox-shim, the screen.
# The image holds no secret.
#
# Build from the repo root (it carries the fragment CLI, of the root
# workspace; .dockerignore keeps the context to what the build reads):
#   docker build --platform linux/amd64 -f images/hermes/Dockerfile .

# Upstream's main (no stable release of its new pipeline is published yet),
# pinned by digest: `main-desktop` as of 2026-10-06, built from
# NousResearch/hermes-agent@90194c64ff99. The first stable is
# `vX.Y.Z-desktop`, pinned here the same way (a tag and its index digest).
ARG HERMES=nousresearch/hermes-agent:main-desktop@sha256:7c6c7debc9e8fe623ffd5afce1c80116e8cff4e818ef42eb649d43562139d140

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
# Upstream's settings, as its image sets them: the flatten below restates
# them, and checks it does.
RUN mkdir -p /opt/fragment && env | sort > /opt/fragment/upstream.env
COPY --from=docker.io/cloudflare/sandbox:1.0.0 /usr/local/bin/sandbox-shim /usr/local/bin/sandbox-shim
# cua-driver, the backend of Hermes' computer_use (screenshots of the agent's
# desktop, XTest input): without it Hermes offers the agent no computer_use.
# Pinned (the release current at Hermes v0.21.5's; Hermes' image has none,
# and its PM's pin is older), named to Hermes by HERMES_CUA_DRIVER_CMD
# below, which Hermes takes over its PM's.
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
# agent-browser, the CLI Hermes' built-in browser tools drive (each
# profile's `browser.backend: "off"`: boot/src/hermes.rs). Hermes' image
# carries none (its default browser is Browser Use's browser_exec), and its
# browser tools look for it only in its PM's store or on PATH, never
# installing it to see whether to offer themselves: without it an agent has
# no browser_* tool. Fetched by Hermes' own PM, at the version and hash its
# lock pins, and put on PATH: PM writes beside a sealed store (the image's)
# only into its home's (HERMES_HOME/tools, which /data would replace), so it
# fetches into a scratch home, and its one binary (a native one) is
# installed from there.
RUN HERMES_HOME=/tmp/pm-agent-browser /opt/hermes/.venv/bin/python -c 'from pm import ensure, installed_package; ensure("agent-browser", explicit=True); print(installed_package("agent-browser").binary)' > /tmp/agent-browser.path \
 && install -m 0755 "$(tail -n 1 /tmp/agent-browser.path)" /usr/local/bin/agent-browser \
 && rm -rf /tmp/pm-agent-browser /tmp/agent-browser.path \
 && /usr/local/bin/agent-browser --version | grep -q '^agent-browser [0-9]'

# Root for the session (Paul, 2026-10-05: "hermes needs to be able to
# install binaries (not persisted)"). Hermes stays its unprivileged user, as
# upstream runs it, and reaches root by passwordless sudo: `sudo apt-get
# install`, a binary into /usr/local/bin, `sudo npm install -g`. The
# computer is one person's VM and holds no secret (decisions 13, 43, 44),
# so root inside it opens nothing of anyone else's. The system's
# directories stay root's, as on a CI runner. What it installs lasts until
# the computer's next start from this image (docs/computers.md, "Root in
# our Hermes image"). apt's lists are kept, fresh as of this build. sudo
# keeps npm's global prefix (npm_config_prefix, below), so `sudo npm
# install -g` puts a program in /usr/local/bin.
RUN apt-get -o Acquire::Retries=3 update \
 && DEBIAN_FRONTEND=noninteractive apt-get -o Acquire::Retries=3 install -y --no-install-recommends sudo \
 && printf 'Defaults env_keep += "npm_config_prefix"\nhermes ALL=(ALL:ALL) NOPASSWD: ALL\n' > /etc/sudoers.d/hermes \
 && chmod 0440 /etc/sudoers.d/hermes \
 && visudo -c -q
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

# None of Hermes' bundled skills: Hermes copies them only into the home
# its sync runs in, the gateway's default profile, which runs no turns (an
# agent's profile has its own, the managed set and the platform skill:
# docs/computers.md). Without them stage2 runs no sync (it syncs only when
# `/opt/hermes/skills` is there) and the gateway's two return at once.
RUN rm -r /opt/hermes/skills

# The gateway is hermes-boot's main program (preloaded): no s6 gateway
# service, which upstream's reconciler starts from a restored
# gateway_state.json, a second gateway (S3b, cut b). The build fails if no
# script of that name is there to remove, or another still runs it.
RUN rm /etc/cont-init.d/02-reconcile-profiles \
 && ! grep -rqs container_boot /etc/cont-init.d

# Hermes' tool progress, each line a step (docs/hermes-relay.md): its
# sender (gateway/run_turn_runner.py, `send_progress_messages`) waits out
# its 1.5 s edit interval for a line, then goes back to its queue, and sends
# that line only with a newer one. So a tool call within 1.5 s of the last
# progress line that is its turn's last (a skill read, then a quick model
# call to the terminal) never shows, and its step is never recorded. Patched
# to send once the interval is out (the debt ledger, "The Hermes image
# patches Hermes' progress sender"); the build fails if upstream's loop
# changed. Its .pyc goes, so the bytecode step below compiles the patch.
RUN /opt/hermes/.venv/bin/python -c "import sys; p = sys.argv[1]; s = open(p).read(); old = '                        await asyncio.sleep(remaining)\n                        continue\n'; assert s.count(old) == 1, 'upstream progress sender changed: ' + p; open(p, 'w').write(s.replace(old, '                        await asyncio.sleep(remaining)\n'))" /opt/hermes/gateway/run_turn_runner.py \
 && rm -f /opt/hermes/gateway/__pycache__/run_turn_runner.*.pyc

# Bytecode in the image (S3b, cut a: -6.2 s). unchecked-hash: /opt/hermes is
# sealed, so nothing can make a .pyc stale.
# A few vendored files are not Python 3 (compileall exits 1 for them); the
# count proves the rest compiled.
RUN /opt/hermes/.venv/bin/python -m compileall -q -j0 --invalidation-mode unchecked-hash /opt/hermes > /dev/null || true; \
    test "$(find /opt/hermes -name '*.pyc' | wc -l)" -gt 8000

# What the boot reads instead of finding it each time: the lean plugin list;
# the image's Chromium, /opt/fragment/bin/chromium
# (the full Chromium Hermes' build pins and names in
# /etc/hermes/agent-browser-executable-path, always with the flags a
# container needs, which Hermes adds only where it sees Docker:
# boot/src/hermes.rs); the platform skill every profile
# has, from the fragment CLI above (`fragment skill`: boot/src/skills.rs);
# and the files the gateway loads, read ahead on the second CPU while /data
# is restored (S3b, cut h).
RUN /opt/fragment/bin/hermes-boot build-info \
 && mkdir -p /tmp/rh \
 && HERMES_HOME=/tmp/rh HOME=/tmp/rh /opt/hermes/.venv/bin/python -c "import sys, os, gateway.run, hermes_cli.gateway, gateway.relay.adapter, gateway.relay.ws_transport; fs = {m.__cached__ for m in list(sys.modules.values()) if getattr(m, '__cached__', None)}; fs |= {l.split()[-1] for l in open('/proc/self/maps') if '.so' in l}; open('/opt/fragment/readahead.list', 'w').write(chr(10).join(sorted(f for f in fs if os.path.isfile(f))) + chr(10))" \
 && rm -rf /tmp/rh \
 && mkdir -p /run/computer /var/lib/fragment-run \
 && install -d -m 1777 /tmp/node-compile-cache

# One layer (S3b, cut i: -0.2 s per warm start); the image's settings are
# restated, without upstream's VOLUME. Hermes' PM finds its tool store (its
# Python, Node, npm, uv, Chromium) by HERMES_RUNTIME_DIR, and Hermes no
# longer falls back to a system node, npm or uv: unset, both would look in
# /data/hermes/tools. Three differ from upstream's:
# - Hermes' home is /data/hermes, under the only directory a computer
#   keeps, not /opt/data. Hermes and its setup read HERMES_HOME; what still
#   names /opt/data never runs here (main-wrapper.sh, upstream's main
#   program; the dashboard's run, which exits first without
#   HERMES_DASHBOARD) or runs only for root (the `hermes` shim's HOME).
# - Hermes' file tools (write_file, patch) may write its home, its agents'
#   work directories (/data/work, the terminal's cwd since the seam) and
#   /tmp (HERMES_WRITE_SAFE_ROOT, defense in depth: its terminal is not held
#   to it), the last for the scratch an install is made from. /usr/local is not
#   theirs to write: they run as Hermes' user, and root is the terminal's,
#   through sudo.
# - npm's global prefix is /usr/local (npm_config_prefix), as it was before
#   Hermes' PM: its npm's own is its Node's directory in the store, which no
#   PATH names, so `sudo npm install -g` would put a program nowhere an
#   agent runs it from, and write into Hermes' sealed store. Hermes' PM
#   leaves npm_config_* out of its own npm runs.
# Lazy installs, which upstream now turns on (into /data/hermes/installs),
# are off: Hermes' managed overlay (security.allow_lazy_installs,
# boot/src/hermes.rs).
# Node's compile cache is kept under /tmp (NODE_COMPILE_CACHE, one directory
# every user may write, each its own inside), never in /data: npm turns it on
# at every start, under TMPDIR, which Hermes points into its home
# (`cache/scratch`), so Hermes' own `npx --version` probes rewrote a file the
# save keeps while the computer was held (docs/durable-computers.md, "What
# changes under the hold"). A cache a start rebuilds.
FROM scratch
COPY --from=hermes / /
ENV PATH="/opt/hermes/bin:/opt/hermes/.venv/bin:/data/hermes/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin" \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PLAYWRIGHT_BROWSERS_PATH=/opt/hermes/tools \
    HERMES_RUNTIME_DIR=/opt/hermes/tools \
    HERMES_PYTHON=/opt/hermes/.venv/bin/python \
    CC=gcc \
    CXX=g++ \
    npm_config_prefix=/usr/local \
    HERMES_WEB_DIST=/opt/hermes/hermes_cli/web_dist \
    HERMES_TUI_DIR=/opt/hermes/ui-tui \
    HERMES_HOME=/data/hermes \
    HERMES_WRITE_SAFE_ROOT=/data/hermes:/data/work:/tmp \
    XDG_RUNTIME_DIR=/tmp/hermes-runtime \
    NODE_COMPILE_CACHE=/tmp/node-compile-cache \
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
# Every setting of upstream's image is restated, but the two above that
# differ (its PATH's home, read as ours): the build fails on one the
# flatten drops or sets otherwise, as a new base's would be.
RUN env | sort > /tmp/env.now \
 && lost="$(grep -Ev '^(HOME|HOSTNAME|PWD|SHLVL|_|HERMES_HOME|HERMES_WRITE_SAFE_ROOT)=' /opt/fragment/upstream.env | sed '/^PATH=/s#/opt/data#/data/hermes#' | grep -vxFf /tmp/env.now || true)" \
 && rm /tmp/env.now \
 && if [ -n "$lost" ]; then printf 'upstream sets what the flatten does not:\n%s\n' "$lost"; exit 1; fi
# which build this is, served beside the screen (the platform's upgrade and
# rollback lane reads it through the computer's port): a layer of its own,
# so another build shares the one above
ARG IMAGE_VERSION=1
RUN echo "$IMAGE_VERSION" > /opt/fragment/screen/version.txt
EXPOSE 6080
ENTRYPOINT ["/opt/fragment/bin/hermes-boot", "pre-init"]
