# Single-container Jani cloud prototype: jani-cloud (the gateway) plus the
# `jani` binary it starts once per signed-in person.
#
# The build context must contain jani/ and siwi/ side by side (siwi is a path
# dependency of the gateway). From the directory that holds both checkouts:
#
#   docker build -f jani/cloud/Dockerfile -t jani-cloud .
#
# NOT BUILT OR RUN in the environment this was written in (no Docker there).
# Treat it as the intended recipe and expect to fix small things on first build.

FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY jani /src/jani
COPY siwi /src/siwi
RUN cargo build --release --locked --manifest-path jani/Cargo.toml -p jani \
 && cargo build --release --manifest-path jani/cloud/Cargo.toml

FROM debian:bookworm-slim
# bubblewrap: per-person confinement. git: seeds each worktree.
# ca-certificates: TLS to the model provider and to accounts. tini: reaps children.
RUN apt-get update \
 && apt-get install -y --no-install-recommends bubblewrap git ca-certificates tini \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/jani/target/release/jani /usr/local/bin/jani
COPY --from=build /src/jani/cloud/target/release/jani-cloud /usr/local/bin/jani-cloud

ENV JANI_CLOUD_BIND=0.0.0.0:8080 \
    JANI_CLOUD_DATA=/data \
    JANI_CLOUD_JANI_BIN=/usr/local/bin/jani \
    JANI_CLOUD_SANDBOX=bwrap
VOLUME /data
EXPOSE 8080
# Runs as root on purpose: it lets the gateway start each workspace as its own
# Unix user (JANI_CLOUD_UID_BASE) and lets bubblewrap create namespaces. See the
# README for the container flags bubblewrap needs.
ENTRYPOINT ["tini", "--", "jani-cloud"]
