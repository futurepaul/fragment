# The fleet's node image: the celld fork (pinned by `cargo xtask deploy
# --nodes`, which passes CELLD_REV) with our native services built in, and
# fragment-node, which starts celld on the Machine's private address and
# volume (crates/node). Built with the local Docker from the repo root
# (.dockerignore keeps the context small).
ARG RUST_VERSION=1.97.1

FROM rust:${RUST_VERSION}-bookworm AS celld
ARG CELLD_REPO=https://github.com/futurepaul/celld.git
ARG CELLD_REV
RUN test -n "${CELLD_REV}" && \
    git clone --quiet "${CELLD_REPO}" /build/celld && \
    git -C /build/celld checkout --quiet --detach "${CELLD_REV}"
# The fork's seam builds our native services (crates/native: KEYS) from
# fragment-next beside it as `fragment/` (docs/hardening.md, H1).
COPY . /build/fragment
WORKDIR /build/celld
RUN cargo build --release --locked -p celld

FROM rust:${RUST_VERSION}-bookworm AS node
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p fragment-node

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=celld /build/celld/target/release/celld /usr/local/bin/celld
COPY --from=node /src/target/release/fragment-node /usr/local/bin/fragment-node
ENTRYPOINT ["/usr/local/bin/fragment-node"]
