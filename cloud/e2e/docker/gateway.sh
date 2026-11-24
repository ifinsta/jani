#!/bin/sh
# Lets the end-to-end harness run the gateway from the built image:
#
#   docker build -f jani/cloud/Dockerfile -t jani-cloud .
#   docker build -f jani/cloud/e2e/docker/Dockerfile.test -t jani-cloud:test jani/cloud/e2e/docker
#   GATEWAY_BIN=$PWD/docker/gateway.sh GATEWAY_IMAGE=jani-cloud:test E2E_SANDBOX=bwrap \
#   DOCKER_FLAGS="--security-opt seccomp=unconfined --security-opt apparmor=unconfined \
#     --security-opt systempaths=unconfined --cap-add SYS_ADMIN --cap-add NET_ADMIN" npm test
#
# The container shares the host network (the harness's accounts and scripted
# provider listen on loopback) and /tmp (the workspaces' data directory).
f=$(mktemp /tmp/jc-env.XXXXXX)
env | grep -E '^(IFINSTA_|JANI_|RUST_LOG)' | grep -v '^JANI_CLOUD_JANI_BIN=' > "$f"
echo 'JANI_CLOUD_JANI_BIN=/usr/local/bin/jani' >> "$f"
# shellcheck disable=SC2086
exec docker run --rm --network host ${DOCKER_FLAGS} -v /tmp:/tmp --env-file "$f" "${GATEWAY_IMAGE:-jani-cloud}"
