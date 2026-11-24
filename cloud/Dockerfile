# Single-container Jani cloud prototype: jani-cloud (the gateway) plus the
# `jani` binary it starts once per signed-in person.
#
# The build context must contain jani/ and siwi/ side by side (siwi is a path
# dependency of the gateway). From the directory that holds both checkouts:
#
#   docker build -f jani/cloud/Dockerfile -t jani-cloud .
#
# Behind a TLS-intercepting proxy, pass its CA bundle as a build secret:
#
#   docker build --secret id=cabundle,src=/path/to/ca-bundle.crt -f jani/cloud/Dockerfile -t jani-cloud .
#
# The build stage uses Debian bookworm (older glibc) and the runtime stage uses
# Ubuntu 24.04, whose glibc is newer, so the binaries run unchanged.

FROM rust:1.95-bookworm AS build
# The image already has exactly this toolchain; pinning it stops rustup from
# trying to download the rustfmt/clippy components rust-toolchain.toml lists.
ENV RUSTUP_TOOLCHAIN=1.95.0
WORKDIR /src
COPY jani /src/jani
COPY siwi /src/siwi
RUN --mount=type=secret,id=cabundle,required=false \
    --mount=type=cache,target=/usr/local/cargo/registry \
    if [ -f /run/secrets/cabundle ]; then export SSL_CERT_FILE=/run/secrets/cabundle CARGO_HTTP_CAINFO=/run/secrets/cabundle; fi; \
    cargo build --release --locked --manifest-path jani/Cargo.toml -p jani \
 && cargo build --release --locked --manifest-path jani/cloud/Cargo.toml

FROM ubuntu:24.04
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
# Unix user (JANI_CLOUD_UID_BASE), put it in a cgroup, and lets bubblewrap create
# namespaces. bubblewrap needs container flags Docker does not grant by default:
# see "Running the image" in the README. The image carries no language
# toolchains; build FROM it to give workspaces python, node and the like.
ENTRYPOINT ["tini", "--", "jani-cloud"]
