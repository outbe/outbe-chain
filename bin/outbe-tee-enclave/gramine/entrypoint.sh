#!/usr/bin/env bash
# Build + run the outbe-tee-enclave as a real Gramine SGX enclave.
#
# This is ONE artifact with two run modes, chosen automatically:
#   - SGX hardware present (prod / testnet SGX boxes) -> gramine-sgx (CONFIDENTIAL,
#     real attestation + sealing).
#   - no SGX hardware (local dev machine)             -> gramine-direct (same
#     enclave, LibOS, NOT confidential — for functional testing only).
#
# Either way the enclave is signed (gramine-sgx-sign) so it has a real measured
# identity (MRENCLAVE / MRSIGNER). Signing needs NO SGX hardware — it is a build
# step — which is what lets a dev machine produce and test a genuine enclave.
#
# All args (e.g. --socket 127.0.0.1:7000 --dkg-seed ...) pass straight through to
# the enclave (loader.insecure__use_cmdline_argv).
set -euo pipefail

ENTRY=/app/outbe-tee-enclave
ARCH_LIBDIR=/lib/x86_64-linux-gnu

if [ ! -x "$ENTRY" ]; then
    echo "entrypoint: $ENTRY missing — mount the binary (-v host_bin:$ENTRY)" >&2
    exit 2
fi

cd /app

# Writable dir for the sealed offer-key blob (--tee-dir). Bind-mount a persistent
# host dir here (e.g. -v host_tee:/tee) for the restart fast-path to survive a
# container restart; otherwise it is container-local (sealing still works within
# one container lifetime). The manifest declares it as an allowed (untrusted)
# file — the enclave AES-GCM-seals the blob itself.
TEE_DIR="${OUTBE_TEE_DIR:-/tee}"
mkdir -p "$TEE_DIR"

# 1. Render the manifest for the mounted binary.
gramine-manifest \
    -Dlog_level="${GRAMINE_LOG_LEVEL:-error}" \
    -Darch_libdir="$ARCH_LIBDIR" \
    -Dentrypoint="$ENTRY" \
    -Dtee_dir="$TEE_DIR" \
    outbe-tee-enclave.manifest.template \
    outbe-tee-enclave.manifest

# 2. Sign the enclave -> SIGSTRUCT (.sig) + .manifest.sgx, computing the real
#    MRENCLAVE/MRSIGNER. The dev signing key is baked into the image; production
#    re-signs with the operator's own key (mount it / regenerate before deploy).
gramine-sgx-sign \
    --manifest outbe-tee-enclave.manifest \
    --output outbe-tee-enclave.manifest.sgx >/dev/null 2>&1
echo "entrypoint: enclave identity (signed, no SGX hardware needed to measure):" >&2
gramine-sgx-sigstruct-view outbe-tee-enclave.sig 2>/dev/null \
    | grep -iE "mr_enclave|mr_signer|isv_prod_id|isv_svn|debug" | sed 's/^/  /' >&2

# 3. Run under real SGX when the device is present, else gramine-direct.
if [ -e /dev/sgx_enclave ] || [ -e /dev/sgx/enclave ]; then
    echo "entrypoint: SGX hardware detected -> gramine-sgx (CONFIDENTIAL)" >&2
    exec gramine-sgx outbe-tee-enclave "$@"
else
    echo "entrypoint: no SGX hardware -> gramine-direct (LOCAL TEST, not confidential)" >&2
    exec gramine-direct outbe-tee-enclave "$@"
fi
