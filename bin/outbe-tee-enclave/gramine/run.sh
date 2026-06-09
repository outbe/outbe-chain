#!/usr/bin/env bash
# Run outbe-tee-enclave under gramine-direct in Docker (Gramine LibOS, NO SGX
# hardware — validates the Gramine integration, not confidentiality).
#   ./run.sh <host-binary> <host-socket-dir> [--dkg-seed <hex32>]
# NOTE: Gramine pathname UDS are process-internal, so the socket is NOT visible
# on the host mount; reaching the enclave from a non-Gramine process needs TCP.
set -euo pipefail
BIN="${1:?path to outbe-tee-enclave binary}"; SOCKDIR="${2:?host socket dir}"; shift 2
docker run --rm \
  --security-opt seccomp=unconfined \
  -v "$(readlink -f "$BIN"):/app/outbe-tee-enclave:ro" \
  -v "$(readlink -f "$SOCKDIR"):/sock" \
  outbe-tee-enclave-gramine \
  --socket /sock/tee.sock "$@"
