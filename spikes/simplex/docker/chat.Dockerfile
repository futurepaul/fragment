# syntax=docker/dockerfile:1.7
# The simplex-chat CLI (the Haskell core and its WebSocket bot API), pinned
# by version and checksum: v7.0.3, the latest stable release on 2026-10-05.
# Both the person's client and the connector prototype run this image.
#
# docker build -f spikes/simplex/docker/chat.Dockerfile -t simplex-spike-chat:7.0.3 spikes/simplex/docker
FROM ubuntu:24.04@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55
ARG TARGETARCH
ARG VERSION=v7.0.3
# the release's own sha256 for each asset (GitHub's asset digest, and the
# release's _sha256sums)
ARG SHA_ARM64=2d2e62351f11bc51ae659618584722b38ea5a6796b806c9a388ee6e3124c90ac
ARG SHA_AMD64=895fb14cfaa662d1c0947f2f871141fc89680fe3e70bff64c366cbe9e59aa4f0
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl libgmp10 libssl3t64 zlib1g libffi8 sqlite3 socat \
 && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "$TARGETARCH" in \
      arm64) asset=simplex-chat-ubuntu-24_04-aarch64; sha=$SHA_ARM64 ;; \
      amd64) asset=simplex-chat-ubuntu-24_04-x86_64; sha=$SHA_AMD64 ;; \
      *) echo "no simplex-chat build for $TARGETARCH"; exit 1 ;; \
    esac; \
    curl -fsSL -o /usr/local/bin/simplex-chat "https://github.com/simplex-chat/simplex-chat/releases/download/$VERSION/$asset"; \
    echo "$sha  /usr/local/bin/simplex-chat" | sha256sum -c -; \
    chmod +x /usr/local/bin/simplex-chat
COPY entry.sh /usr/local/bin/spike-entry
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/spike-entry"]
