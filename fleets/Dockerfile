# The fleet's node image: the celld fork (pinned by `cargo xtask deploy
# --nodes`, which passes CELLD_REV) and fragment-node, which starts celld on
# the Machine's private address and volume (crates/node). Built by Fly's
# remote builder from the repo root (.dockerignore keeps it small).
ARG RUST_VERSION=1.97.1

FROM rust:${RUST_VERSION}-bookworm AS celld
ARG CELLD_REPO=https://github.com/futurepaul/celld.git
ARG CELLD_REV
RUN test -n "${CELLD_REV}" && \
    git clone --quiet "${CELLD_REPO}" /celld && \
    git -C /celld checkout --quiet --detach "${CELLD_REV}"
WORKDIR /celld
RUN cargo build --release --locked -p celld

FROM rust:${RUST_VERSION}-bookworm AS node
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p fragment-node

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=celld /celld/target/release/celld /usr/local/bin/celld
COPY --from=node /src/target/release/fragment-node /usr/local/bin/fragment-node
ENTRYPOINT ["/usr/local/bin/fragment-node"]
