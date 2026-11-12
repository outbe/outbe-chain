#!/bin/bash
# Generate genesis and per-validator data bundles for a four-validator network
# whose validators run inside dstack VMs. TEE_MODE selects the attestation
# policy of the genesis: the development enclave, AMD SEV-SNP reports or Intel
# TDX quotes.
#
# A bundle holds data only: keys, the genesis, the bootnode list, the network
# descriptor and attestation collateral. The launch scripts are part of the VM
# image. The dstack user config of a VM selects the enclave to run:
# `"tee_mode": "dev"` for TEE_MODE=dev, `"tee_mode": "confidential-vm"` for
# the hardware modes.
#
# In a hardware mode the script also writes <network-dir>/app-compose.json, the
# application manifest of every VM of this network. The manifest names the
# network descriptor and lists the images that can receive the offer key. The
# processor measures the manifest, and the service in the VM takes its release
# rules from it. Deploy every VM of the network with exactly this file.
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
#             (the VMs fetch it from `crs_url` of their user config)
#   TEE_MODE  `dev` (default), `sev-snp`, `tdx` or `mixed` (validators on both
#             platforms; needs the values of both). The hardware modes need
#             the image measurements of the VM. A VM that runs the same image
#             with the same number of processors and the same memory size
#             prints them at boot in its `outbe-identity:` lines. They do not
#             depend on the application manifest.
#             With `sev-snp`:
#   SNP_MEASUREMENT  launch measurement of the VM, 96 hex digits
#   SNP_ARK_SHA256   SHA-256 of the AMD root certificate of the processor
#                    generation, 64 hex digits
#             With `tdx`:
#   TDX_MRTD         build-time measurement of the trust domain, 96 hex digits
#   TDX_RTMR0, TDX_RTMR1, TDX_RTMR2
#                    runtime measurement registers, 96 hex digits each
#   APP_COMPOSE_TEMPLATE
#                    application manifest without the release section
#                    (default: app-compose.json next to this script)
#   TDX_MIN_TCB_EVALUATION_DATA_NUMBER
#                    lowest accepted TCB evaluation data number of the Intel
#                    collateral (default: 1)
#   PORT_OFFSETS     optional: four numbers, one for each validator, added to
#                    its peer-to-peer ports (default: "0 0 0 0"). Give the VM
#                    the same number as `port_offset` of its user config. Two
#                    validators behind one public address need different
#                    numbers.
#   SNP_VCEK_FILE    optional: VCEK certificate of the processor (DER), for a
#                    host that passes no certificate table to its guests
#   TDX_COLLATERAL_FILE
#                    optional: Intel collateral file (collateral.json)
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
TEE_MODE=${TEE_MODE:-dev}
case "$TEE_MODE" in
    dev) ;;
    sev-snp)
        : "${SNP_MEASUREMENT:?SNP_MEASUREMENT required with TEE_MODE=sev-snp}"
        : "${SNP_ARK_SHA256:?SNP_ARK_SHA256 required with TEE_MODE=sev-snp}"
        ;;
    tdx)
        : "${TDX_MRTD:?TDX_MRTD required with TEE_MODE=tdx}"
        : "${TDX_RTMR0:?TDX_RTMR0 required with TEE_MODE=tdx}"
        : "${TDX_RTMR1:?TDX_RTMR1 required with TEE_MODE=tdx}"
        : "${TDX_RTMR2:?TDX_RTMR2 required with TEE_MODE=tdx}"
        TDX_MIN_TCB_EVALUATION_DATA_NUMBER=${TDX_MIN_TCB_EVALUATION_DATA_NUMBER:-1}
        ;;
    mixed)
        # Validators on both platforms: the identity of each one is required.
        : "${SNP_MEASUREMENT:?SNP_MEASUREMENT required with TEE_MODE=mixed}"
        : "${SNP_ARK_SHA256:?SNP_ARK_SHA256 required with TEE_MODE=mixed}"
        : "${TDX_MRTD:?TDX_MRTD required with TEE_MODE=mixed}"
        : "${TDX_RTMR0:?TDX_RTMR0 required with TEE_MODE=mixed}"
        : "${TDX_RTMR1:?TDX_RTMR1 required with TEE_MODE=mixed}"
        : "${TDX_RTMR2:?TDX_RTMR2 required with TEE_MODE=mixed}"
        TDX_MIN_TCB_EVALUATION_DATA_NUMBER=${TDX_MIN_TCB_EVALUATION_DATA_NUMBER:-1}
        ;;
    *) echo "TEE_MODE must be dev, sev-snp, tdx or mixed" >&2; exit 1 ;;
esac

addresses=("$@")
read -r -a offsets <<<"${PORT_OFFSETS:-0 0 0 0}"
[ "${#offsets[@]}" -eq 4 ] || { echo "PORT_OFFSETS needs four numbers" >&2; exit 1; }
for offset in "${offsets[@]}"; do
    [[ $offset =~ ^[0-9]{1,2}$ ]] || { echo "a port offset must be 0 to 99" >&2; exit 1; }
done

gen=$net/gen
rm -rf "$gen" "$net/publish" "$net/app-compose.json"
mkdir -p "$gen" "$net/publish"
# One genesis time for both generator runs below.
GENESIS_TIMESTAMP=$(date +%s)

# Write network.yaml. The two arguments are the configuration identities of
# the VMs: the SEV-SNP host data and the TDX MRCONFIGID.
write_network_yaml() {
    local snp_host_data=$1 tdx_mrconfigid=$2
    echo "chain_id: $CHAIN_ID"
    echo "timestamp: $GENESIS_TIMESTAMP"
    echo "validators:"
    for index in 0 1 2 3; do
        echo "  - ${addresses[$index]}:$((30400 + offsets[index]))"
    done
    echo "keys_dir: $net/keys"
    echo "tee:"
    case "$TEE_MODE" in
        sev-snp) echo "  mode: sev-snp-required" ;;
        tdx) echo "  mode: tdx-required" ;;
        mixed) echo "  mode: confidential-vm-required" ;;
        dev) echo "  mode: gramine-direct-dev" ;;
    esac
    if [ "$TEE_MODE" = sev-snp ] || [ "$TEE_MODE" = mixed ]; then
        echo "  snp_measurement: \"$SNP_MEASUREMENT\""
        echo "  snp_host_data: \"$snp_host_data\""
        echo "  snp_ark_sha256: \"$SNP_ARK_SHA256\""
    fi
    if [ "$TEE_MODE" = tdx ] || [ "$TEE_MODE" = mixed ]; then
        echo "  tdx_mrtd: \"$TDX_MRTD\""
        echo "  tdx_rtmr0: \"$TDX_RTMR0\""
        echo "  tdx_rtmr1: \"$TDX_RTMR1\""
        echo "  tdx_rtmr2: \"$TDX_RTMR2\""
        echo "  tdx_mrconfigid: \"$tdx_mrconfigid\""
        echo "  tdx_minimum_tcb_evaluation_data_number: $TDX_MIN_TCB_EVALUATION_DATA_NUMBER"
    fi
    echo "enclave_sgx: false"
    echo "remote_base_dir: $BASE"
    echo "remote_keys_dir: $BASE/keys"
    echo "chain_binary: $BIN_DIR/outbe-chain"
    echo "keygen_binary: $BIN_DIR/outbe-keygen"
    echo "contracts_dir: $REPO/contracts/predeploys"
}

# Generate the genesis and the network descriptor in a new directory.
generate() {
    local dir=$1
    rm -rf "${dir:?}"
    mkdir -p "$dir"
    write_network_yaml "$2" "$3" > "$dir/network.yaml"
    (cd "$dir" && "$BIN_DIR/outbe-genesis-cli" create network.yaml)
    "$BIN_DIR/outbe-chain" tee network-descriptor \
        --genesis "$dir/genesis.json" --output "$dir/network-descriptor-v1.bin"
}

if [ "$TEE_MODE" = dev ]; then
    generate "$gen" "" ""
else
    # The manifest names the network descriptor, and the attestation policy of
    # the genesis names the manifest. The descriptor does not depend on the
    # policy, so the first run uses placeholder identities and gives the
    # descriptor, and the second run gives the genesis with the real policy.
    draft=$net/gen-draft
    generate "$draft" "$(printf '11%.0s' {1..32})" "01$(printf '11%.0s' {1..32})$(printf '00%.0s' {1..15})"
    descriptor_hash=$(sha256sum "$draft/network-descriptor-v1.bin" | cut -d' ' -f1)
    snp_list='[]'
    tdx_list='[]'
    if [ "$TEE_MODE" = sev-snp ] || [ "$TEE_MODE" = mixed ]; then
        snp_list=$(jq -cn --arg m "$SNP_MEASUREMENT" '[$m]')
    fi
    if [ "$TEE_MODE" = tdx ] || [ "$TEE_MODE" = mixed ]; then
        tdx_list=$(jq -cn --arg mrtd "$TDX_MRTD" --arg r0 "$TDX_RTMR0" --arg r1 "$TDX_RTMR1" \
            --arg r2 "$TDX_RTMR2" '[{mrtd: $mrtd, rtmr0: $r0, rtmr1: $r1, rtmr2: $r2}]')
    fi
    jq -c --arg descriptor "$descriptor_hash" --argjson snp "$snp_list" --argjson tdx "$tdx_list" \
        '. + {outbe_release: {network_descriptor_sha256: $descriptor,
                              sev_snp_measurements: $snp, tdx_images: $tdx}}' \
        "${APP_COMPOSE_TEMPLATE:-$HERE/app-compose.json}" > "$net/app-compose.json"
    # dstack binds the SHA-256 hash of the manifest file to the launch identity
    # of the VM: directly on TDX, through a small JSON document on SEV-SNP.
    manifest_hash=$(sha256sum "$net/app-compose.json" | cut -d' ' -f1)
    tdx_mrconfigid="01${manifest_hash}$(printf '00%.0s' {1..15})"
    document="{\"app_id\":\"${manifest_hash:0:40}\",\"compose_hash\":\"$manifest_hash\",\"key_provider\":\"none\",\"version\":3}"
    snp_host_data=$(printf 'dstack-mr-config-v3:\0%s' "$document" | sha256sum | cut -d' ' -f1)
    generate "$gen" "$snp_host_data" "$tdx_mrconfigid"
    cmp -s "$draft/network-descriptor-v1.bin" "$gen/network-descriptor-v1.bin" || {
        echo "the network descriptor depends on the attestation policy" >&2
        exit 1
    }
    rm -rf "${draft:?}"
    {
        echo "manifest_sha256=$manifest_hash"
        echo "network_descriptor_sha256=$descriptor_hash"
        echo "snp_host_data=$snp_host_data"
        echo "tdx_mrconfigid=$tdx_mrconfigid"
    } > "$net/identity.txt"
fi

# The generator writes one reth port for every bootnode. Give each validator
# the port of its offset. The file lists the validators in order.
bootnodes=$gen/reth-bootnodes.offsets.txt
index=0
: > "$bootnodes"
while IFS= read -r line; do
    case "$line" in
        enode://*)
            echo "${line%:*}:$((30303 + offsets[index]))" >> "$bootnodes"
            index=$((index + 1))
            ;;
        *) echo "$line" >> "$bootnodes" ;;
    esac
done < "$gen/reth-bootnodes.txt"
[ "$index" -eq 4 ] || { echo "expected four bootnodes, found $index" >&2; exit 1; }

# A bundle holds data only. The launch scripts are part of the VM image, and the
# VM refuses a bundle with anything but regular files and directories.
for index in 0 1 2 3; do
    full=$(mktemp -d)
    stage=$(mktemp -d)
    tar -xzf "$gen/dist/validator-$index.tgz" -C "$full"
    install -d "$stage/keys" "$stage/validator-$index"
    cp -r "$full/keys/validator-$index" "$stage/keys/"
    cp "$full/genesis.json" "$stage/"
    cp "$bootnodes" "$stage/reth-bootnodes.txt"
    # The service reads the network descriptor from the data disk and checks
    # it against the hash in the measured application manifest.
    cp "$gen/network-descriptor-v1.bin" "$stage/validator-$index/"
    # Attestation collateral that the node adds to its evidence.
    if [ -n "${SNP_VCEK_FILE:-}" ]; then
        install -D -m 0644 "$SNP_VCEK_FILE" "$stage/sev-snp-certs/vcek.der"
    fi
    if [ -n "${TDX_COLLATERAL_FILE:-}" ]; then
        install -D -m 0644 "$TDX_COLLATERAL_FILE" "$stage/tdx-collateral/collateral.json"
    fi
    if find "$stage" ! -type f ! -type d | grep -q .; then
        echo "bundle $index contains an entry that is not a file or a directory" >&2
        exit 1
    fi
    tar -czf "$net/publish/validator-$index.tgz" -C "$stage" .
    rm -rf "$stage" "$full"
done

[ -f "$CRS_FILE" ] && cp "$CRS_FILE" "$net/publish/bn254_g1.dat"
(cd "$net/publish" && sha256sum validator-*.tgz > SHA256SUMS)
echo "bundles: $net/publish"
if [ -f "$net/app-compose.json" ]; then
    echo "application manifest of every VM: $net/app-compose.json"
    cat "$net/identity.txt"
fi
