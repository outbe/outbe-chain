#!/usr/bin/env bash
# TEST ONLY: render/sign a mounted enclave and choose gramine-sgx or
# gramine-direct. Release images use entrypoint.sh and cannot sign at runtime.
set -euo pipefail

readonly ENTRY=/app/outbe-tee-enclave
readonly ARCH_LIBDIR=/lib/x86_64-linux-gnu
readonly TEST_SIGNING_KEY=/run/secrets/outbe-test-sgx-key.pem

if [[ ! -x "${ENTRY}" ]]; then
  echo "test entrypoint: ${ENTRY} missing — mount the enclave binary" >&2
  exit 2
fi
if [[ ! -f "${TEST_SIGNING_KEY}" ]]; then
  echo "test entrypoint: explicit test SGX signing key is required" >&2
  exit 2
fi

cd /app
TEE_DIR="${OUTBE_TEE_DIR:-/tee}"
mkdir -p "${TEE_DIR}"

gramine-manifest \
  -Dlog_level="${GRAMINE_LOG_LEVEL:-error}" \
  -Darch_libdir="${ARCH_LIBDIR}" \
  -Dentrypoint="${ENTRY}" \
  -Dtee_dir="${TEE_DIR}" \
  outbe-tee-enclave.manifest.template \
  outbe-tee-enclave.manifest

gramine-sgx-sign \
  --key "${TEST_SIGNING_KEY}" \
  --manifest outbe-tee-enclave.manifest \
  --output outbe-tee-enclave.manifest.sgx >/dev/null 2>&1
echo "test entrypoint: ephemeral test identity:" >&2
gramine-sgx-sigstruct-view outbe-tee-enclave.sig 2>/dev/null \
  | grep -iE "mr_enclave|mr_signer|isv_prod_id|isv_svn|debug" | sed 's/^/  /' >&2

if [[ -e /dev/sgx_enclave || -e /dev/sgx/enclave ]]; then
  echo "test entrypoint: SGX hardware detected -> gramine-sgx" >&2
  exec gramine-sgx outbe-tee-enclave "$@"
fi

echo "test entrypoint: no SGX hardware -> gramine-direct (TEST ONLY)" >&2
exec gramine-direct outbe-tee-enclave "$@"
