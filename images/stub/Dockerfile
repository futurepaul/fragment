# syntax=docker/dockerfile:1.7
# The stub computer image (docs/computers.md): the bridge with its `script`
# runtime and a screen page on 6080, nothing else. The platform's own lanes
# run against it, which is the proof that the platform needs nothing
# Hermes-specific (docs/cloudflare-v1.md, the rule).
#
# Build from images/: docker build --platform linux/amd64 -f stub/Dockerfile .

# The bridge, static (Alpine's Rust targets musl), built where it runs.
FROM rust:1.99-alpine AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY bridge bridge
COPY hermes/boot hermes/boot
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p fragment-bridge \
 && cp target/release/fragment-bridge /fragment-bridge

# busybox for the few commands the Computer DO runs in a container (`true`
# while it polls, `touch` for the restore marker); sandbox-shim for its
# DirectoryBackup.
FROM busybox:1.37-musl
COPY --from=docker.io/cloudflare/sandbox:1.0.0 /usr/local/bin/sandbox-shim /usr/local/bin/sandbox-shim
COPY --from=build /fragment-bridge /usr/local/bin/fragment-bridge
COPY stub/screen /opt/fragment/screen
ENV BRIDGE_RUNTIME=script \
    BRIDGE_STATE_DIR=/data/bridge \
    BRIDGE_MEDIA_DIR=/tmp/bridge-media \
    BRIDGE_SCRIPT_SCRATCH=/tmp/bridge-script \
    BRIDGE_SCREEN_LISTEN=0.0.0.0:6080 \
    BRIDGE_SCREEN_DIR=/opt/fragment/screen
# no /data: the platform's restore makes it, and a directory from an image
# layer cannot be swapped for a restored one (EXDEV under overlayfs)
RUN mkdir -p /run/computer
EXPOSE 6080
# PID 1 is the bridge: it handles SIGTERM itself (gone within 3 s).
ENTRYPOINT ["/usr/local/bin/fragment-bridge", "run"]
