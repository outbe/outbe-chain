#!/bin/bash
# Generate genesis and per-validator bundles for a four-validator network whose
# validators run inside dstack VMs. By default the VMs run the hardware-free
# development enclave. With TEE_MODE=sev-snp they run the service that attests
# with AMD SEV-SNP reports, with TEE_MODE=tdx the service that attests with
# Intel TDX quotes.
#
# Usage: make-bundles.sh <network-dir> <ip-0> <ip-1> <ip-2> <ip-3>
#
# <network-dir>/keys/validator-<i> must hold the output of
# `outbe-keygen validator`. The result is <network-dir>/publish/validator-<i>.tgz.
# Environment:
#   BIN_DIR   directory with Outbe release binaries
#             (default: $HOME/outbe-tdx-mvp/target/release)
#   CHAIN_ID  chain id (default: 424242, the devnet id)
#   CRS_FILE  Barretenberg CRS file to publish next to the bundles
#             (default: $HOME/.bb-crs/bn254_g1.dat)
#   CRS_URL   URL the VMs fetch the CRS from when it is not on the data disk
#             (default: http://10.77.0.1:18090/bn254_g1.dat)
#   TEE_STATE where the enclave keeps its sealed state: `disk` (default) or
#             `tmpfs`. With `tmpfs` the state is lost on a VM restart, and the
#             registry does not admit the validator again while its enclave
#             lease is live.
#   TEE_MODE  `dev` (default), `sev-snp` or `tdx`. The two hardware modes need
#             the identity of the VM, read with
#             `outbe-tee-enclave --probe-attestation` in a VM that runs the
#             same image with the same application manifest.
#             With `sev-snp`:
#   SNP_MEASUREMENT  launch measurement of the VM, 96 hex digits
#   SNP_HOST_DATA    host data of the VM, 64 hex digits
#   SNP_ARK_SHA256   SHA-256 of the AMD root certificate of the processor
#                    generation, 64 hex digits
#             With `tdx`:
#   TDX_MRTD         build-time measurement of the trust domain, 96 hex digits
#   TDX_RTMR0, TDX_RTMR1, TDX_RTMR2
#                    runtime measurement registers, 96 hex digits each
#   TDX_MRCONFIGID   configuration identity of the VM, 96 hex digits
#   TDX_MIN_TCB_EVALUATION_DATA_NUMBER
#                    lowest accepted TCB evaluation data number of the Intel
#                    collateral (default: 1)
set -euo pipefail

net=${1:?network directory required}
shift
[ "$#" -eq 4 ] || { echo "four validator addresses required" >&2; exit 1; }
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
BIN_DIR=${BIN_DIR:-$HOME/outbe-tdx-mvp/target/release}
CHAIN_ID=${CHAIN_ID:-424242}
BASE=/dstack/persistent/outbe
CRS_FILE=${CRS_FILE:-$HOME/.bb-crs/bn254_g1.dat}
CRS_URL=${CRS_URL:-http://10.77.0.1:18090/bn254_g1.dat}
TEE_STATE=${TEE_STATE:-disk}
case "$TEE_STATE" in
    disk | tmpfs) ;;
    *) echo "TEE_STATE must be disk or tmpfs" >&2; exit 1 ;;
esac
TEE_MODE=${TEE_MODE:-dev}
case "$TEE_MODE" in
    dev) ;;
    sev-snp)
        : "${SNP_MEASUREMENT:?SNP_MEASUREMENT required with TEE_MODE=sev-snp}"
        : "${SNP_HOST_DATA:?SNP_HOST_DATA required with TEE_MODE=sev-snp}"
        : "${SNP_ARK_SHA256:?SNP_ARK_SHA256 required with TEE_MODE=sev-snp}"
        ;;
    tdx)
        : "${TDX_MRTD:?TDX_MRTD required with TEE_MODE=tdx}"
        : "${TDX_RTMR0:?TDX_RTMR0 required with TEE_MODE=tdx}"
        : "${TDX_RTMR1:?TDX_RTMR1 required with TEE_MODE=tdx}"
        : "${TDX_RTMR2:?TDX_RTMR2 required with TEE_MODE=tdx}"
        : "${TDX_MRCONFIGID:?TDX_MRCONFIGID required with TEE_MODE=tdx}"
        TDX_MIN_TCB_EVALUATION_DATA_NUMBER=${TDX_MIN_TCB_EVALUATION_DATA_NUMBER:-1}
        ;;
    *) echo "TEE_MODE must be dev, sev-snp or tdx" >&2; exit 1 ;;
esac

gen=$net/gen
rm -rf "$gen" "$net/publish"
mkdir -p "$gen" "$net/publish"

{
    echo "chain_id: $CHAIN_ID"
    echo "validators:"
    for address in "$@"; do echo "  - $address"; done
    echo "keys_dir: $net/keys"
    echo "tee:"
    if [ "$TEE_MODE" = sev-snp ]; then
        echo "  mode: sev-snp-required"
        echo "  snp_measurement: \"$SNP_MEASUREMENT\""
        echo "  snp_host_data: \"$SNP_HOST_DATA\""
        echo "  snp_ark_sha256: \"$SNP_ARK_SHA256\""
    elif [ "$TEE_MODE" = tdx ]; then
        echo "  mode: tdx-required"
        echo "  tdx_mrtd: \"$TDX_MRTD\""
        echo "  tdx_rtmr0: \"$TDX_RTMR0\""
        echo "  tdx_rtmr1: \"$TDX_RTMR1\""
        echo "  tdx_rtmr2: \"$TDX_RTMR2\""
        echo "  tdx_mrconfigid: \"$TDX_MRCONFIGID\""
        echo "  tdx_minimum_tcb_evaluation_data_number: $TDX_MIN_TCB_EVALUATION_DATA_NUMBER"
    else
        echo "  mode: gramine-direct-dev"
    fi
    echo "enclave_sgx: false"
    echo "remote_base_dir: $BASE"
    echo "remote_keys_dir: $BASE/keys"
    echo "chain_binary: $BIN_DIR/outbe-chain"
    echo "keygen_binary: $BIN_DIR/outbe-keygen"
    echo "contracts_dir: $REPO/contracts/predeploys"
} > "$gen/network.yaml"

(cd "$gen" && "$BIN_DIR/outbe-genesis-cli" create network.yaml)

"$BIN_DIR/outbe-chain" tee network-descriptor \
    --genesis "$gen/genesis.json" --output "$gen/network-descriptor-v1.bin"
# NetworkBindingV1 is the first 66 bytes of the canonical descriptor.
binding=$(head -c 66 "$gen/network-descriptor-v1.bin" | xxd -p -c 200)
chain_id_hex=$(printf '0x%064x' "$CHAIN_ID")

for index in 0 1 2 3; do
    stage=$(mktemp -d)
    tar -xzf "$gen/dist/validator-$index.tgz" -C "$stage"
    # The generated script starts the enclave in a container. Inside the VM the
    # hardware-free development enclave runs as a plain process.
    if [ "$TEE_STATE" = disk ]; then
        tee_dir=$BASE/validator-$index/tee
    else
        tee_dir=/run/outbe/tee
    fi
    if [ "$TEE_MODE" = sev-snp ] || [ "$TEE_MODE" = tdx ]; then
        # The service reads the network descriptor from the data disk. Nothing
        # measures this file (technical debt TD-1 of the design addendum).
        cp "$gen/network-descriptor-v1.bin" "$stage/validator-$index/"
        cat > "$stage/validator-$index/run-enclave.sh" <<SCRIPT
#!/usr/bin/env bash
set -euo pipefail
install -d -m 700 $tee_dir
exec outbe-tee-enclave \\
  --platform $TEE_MODE \\
  --network-descriptor $BASE/validator-$index/network-descriptor-v1.bin \\
  --socket 127.0.0.1:17000 \\
  --tee-dir $tee_dir \\
  --chain-id $chain_id_hex
SCRIPT
    else
        cat > "$stage/validator-$index/run-enclave.sh" <<SCRIPT
#!/usr/bin/env bash
set -euo pipefail
install -d -m 700 $tee_dir
exec outbe-tee-enclave-mock \\
  --socket 127.0.0.1:17000 \\
  --dkg-seed $(printf '%064x' $((index + 1))) \\
  --dev-network-binding $binding \\
  --tee-dir $tee_dir \\
  --chain-id $chain_id_hex
SCRIPT
    fi
    chmod 755 "$stage/validator-$index/run-enclave.sh"
    # The node downloads the Barretenberg CRS from the internet at startup. The
    # VMs have no internet access, so the launch script takes the CRS from the
    # data disk and fetches it once from the bundle server.
    crs_block=$(cat <<SCRIPT
export BB_CRS_PATH=$BASE/crs/bn254_g1.dat
# AMD certificates for SEV-SNP evidence when the host does not supply them.
export OUTBE_SEV_SNP_CERT_DIR=$BASE/sev-snp-certs
# Intel collateral (collateral.json) for TDX evidence.
export OUTBE_TDX_COLLATERAL_DIR=$BASE/tdx-collateral
if [ ! -s "\$BB_CRS_PATH" ]; then
  mkdir -p "\$(dirname "\$BB_CRS_PATH")"
  curl -fsS --max-time 300 -o "\$BB_CRS_PATH.tmp" $CRS_URL
  mv "\$BB_CRS_PATH.tmp" "\$BB_CRS_PATH"
fi
SCRIPT
)
    awk -v block="$crs_block" '{print} /^set -euo pipefail$/ && !done {print block; done=1}' \
        "$stage/validator-$index/run-node.sh" > "$stage/run-node.sh.new"
    grep -q 'BB_CRS_PATH' "$stage/run-node.sh.new"
    # The validators use private addresses on the VM bridge. The default
    # consensus network configuration rejects private addresses.
    sed -i 's|^\( *\)--validator \\$|\1--validator \\\n\1--consensus.use-local-defaults \\|' \
        "$stage/run-node.sh.new"
    grep -q -- '--consensus.use-local-defaults' "$stage/run-node.sh.new"
    mv "$stage/run-node.sh.new" "$stage/validator-$index/run-node.sh"
    chmod 755 "$stage/validator-$index/run-node.sh"
    tar -czf "$net/publish/validator-$index.tgz" -C "$stage" .
    rm -rf "$stage"
done

[ -f "$CRS_FILE" ] && cp "$CRS_FILE" "$net/publish/bn254_g1.dat"
(cd "$net/publish" && sha256sum validator-*.tgz > SHA256SUMS)
echo "bundles: $net/publish"
