#!/usr/bin/env bash
# Internal half of build-binaries.sh. It runs inside the pinned project
# toolchain image with the source snapshot at the fixed path /workspace.
set -euo pipefail

readonly TARGET=x86_64-unknown-linux-gnu
readonly OUT=/out
readonly JOBS=${JOBS:-4}

for required in SOURCE_DATE_EPOCH VERGEN_GIT_SHA VERGEN_GIT_DESCRIBE HOST_UID HOST_GID; do
    [[ -n "${!required:-}" ]] || { echo "missing required build input: ${required}" >&2; exit 2; }
done

cd /workspace
build() {
    cargo build --locked --release --target "${TARGET}" -j "${JOBS}" "$@"
}

# One cargo invocation per feature set. A shared invocation would unify the
# features of all selected packages.
build -p outbe-chain \
    --features outbe-chain/production-enclave,outbe-chain/test-protocol-overrides
build -p outbe-radicle-sidecar -p outbe-cli -p outbe-keygen -p outbe-genesis-cli -p outbe-feeder
# `sev-snp` adds the SEV-SNP report verifier and the sealing key probe.
build -p outbe-tee-enclave --features local-e2e,sev-snp

binaries=(
    outbe-chain
    outbe-radicle
    outbe-cli
    outbe-keygen
    outbe-genesis-cli
    outbe-feeder
    outbe-tee-enclave-mock
    outbe-tee-enclave-local-e2e
)
install -d -m 0755 "${OUT}/bin" "${OUT}/lib" "${OUT}/metadata"
for name in "${binaries[@]}"; do
    install -m 0755 "target/${TARGET}/release/${name}" "${OUT}/bin/${name}"
done

# The service for a confidential VM: the production entry point without any
# development code. It is built last and installed at once, because the build
# above also writes a file with this name that contains the mock feature.
# `sev-snp` is the confidential VM backend for SEV-SNP and TDX. `native-dcap`
# links the pinned Intel quote verification library, which verifies TDX
# quotes. The loop below copies that library into the image.
build -p outbe-tee-enclave --bin outbe-tee-enclave --features sev-snp,native-dcap
install -m 0755 "target/${TARGET}/release/outbe-tee-enclave" "${OUT}/bin/outbe-tee-enclave"

# Shared libraries the binaries need and the dstack guest image (Debian) does
# not ship. They come from this pinned image, not from the build host.
for binary in "${OUT}"/bin/*; do
    ldd "${binary}"
done | awk '$3 ~ /^\// { print $1, $3 }' | LC_ALL=C sort -u | while read -r name path; do
    case "${name}" in
        libc.so.6 | libm.so.6 | libgcc_s.so.1 | libpthread.so.0 | libdl.so.2 | librt.so.1) ;;
        libssl.so.3 | libcrypto.so.3 | libstdc++.so.6 | libz.so.1) ;;
        *) install -m 0644 "$(readlink -f "${path}")" "${OUT}/lib/${name}" ;;
    esac
done

dpkg-query -W -f='${binary:Package}=${Version}\n' | LC_ALL=C sort \
    > "${OUT}/metadata/builder-system-packages.txt"
{
    echo "source_commit=${VERGEN_GIT_SHA}"
    echo "source_describe=${VERGEN_GIT_DESCRIBE}"
    echo "source_date_epoch=${SOURCE_DATE_EPOCH}"
    echo "target=${TARGET}"
    echo "rustc=$(rustc --version)"
} > "${OUT}/metadata/build-inputs.txt"

(
    cd "${OUT}"
    find bin lib metadata -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum
) > "${OUT}/SHA256SUMS"

chown -R "${HOST_UID}:${HOST_GID}" "${OUT}" /workspace
