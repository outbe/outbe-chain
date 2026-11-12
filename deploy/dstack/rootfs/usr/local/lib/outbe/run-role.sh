#!/bin/bash
# Launch one role of the validator: enclave, radicle or node.
#
# This script is part of the VM image, so the platform measures it. The data
# disk supplies data only: keys, the genesis, the bootnode list, the network
# descriptor and attestation collateral. Nothing on the data disk is executed.
set -euo pipefail
role=${1:?role required}
# Written by provision.sh from checked values.
# shellcheck disable=SC1091
. /run/outbe/env

BASE=$OUTBE_BASE
INDEX=$OUTBE_VALIDATOR_INDEX
HOME_DIR=$BASE/validator-$INDEX
KEYS=$BASE/keys/validator-$INDEX
GENESIS=$BASE/genesis.json
DESCRIPTOR=$HOME_DIR/network-descriptor-v1.bin
# Copy of the application manifest that dstack makes in the guest. The service
# checks the file against the launch identity of the VM.
APP_MANIFEST=/dstack/.host-shared/app-compose.json
# State of the hardware service: on tmpfs, so in the memory of the VM only.
TEE_MEMORY_DIR=/run/outbe/tee
ENCLAVE_SOCKET=127.0.0.1:17000
# Copy of the reth p2p key without white space, in the memory of the VM.
P2P_KEY=/run/outbe/reth-p2p-secret.hex
# Peer-to-peer ports. The offset keeps two validators behind one public address
# apart. The RPC port is the same in every VM: the host maps it.
OFFSET=${OUTBE_PORT_OFFSET:-0}
CONSENSUS_PORT=$((30400 + OFFSET))
RETH_PORT=$((30303 + OFFSET))
DISCV5_PORT=$((31303 + OFFSET))
RADICLE_PORT=$((8776 + OFFSET))

chain_id_hex() {
    local id
    id=$(jq -er '.config.chainId' "$GENESIS")
    [[ $id =~ ^[0-9]+$ ]] || { echo "genesis chainId is not a number" >&2; exit 1; }
    printf '0x%064x' "$id"
}

run_enclave() {
    local tee_dir
    if [ "$OUTBE_TEE_MODE" = confidential-vm ]; then
        # The offer key stays in the memory of the VM. The service seals its
        # state files with a key of its own process, so a new process starts
        # with an empty directory and the validator joins the network again.
        rm -rf -- "$TEE_MEMORY_DIR"
        install -d -m 700 "$TEE_MEMORY_DIR"
        rm -f /run/outbe/enclave-current /run/outbe/rejoin-request
        # The service detects SEV-SNP or TDX from the kernel.
        exec outbe-tee-enclave \
            --platform auto \
            --network-descriptor "$DESCRIPTOR" \
            --app-manifest "$APP_MANIFEST" \
            --socket "$ENCLAVE_SOCKET" \
            --tee-dir "$TEE_MEMORY_DIR" \
            --chain-id "$(chain_id_hex)"
    fi
    tee_dir=$HOME_DIR/tee
    install -d -m 700 "$tee_dir"
    # Development enclave: no hardware attestation. NetworkBindingV1 is the
    # first 66 bytes of the canonical descriptor.
    local binding
    binding=$(head -c 66 "$DESCRIPTOR" | od -An -v -tx1 | tr -d ' \n')
    exec outbe-tee-enclave-mock \
        --socket "$ENCLAVE_SOCKET" \
        --dkg-seed "$(printf '%064x' $((INDEX + 1)))" \
        --dev-network-binding "$binding" \
        --tee-dir "$tee_dir" \
        --chain-id "$(chain_id_hex)"
}

run_radicle() {
    local home=$KEYS/radicle directory socket
    # `outbe-keygen validator` creates only keys/. The sidecar needs its
    # working directories, owner-only, before it starts.
    for directory in storage node cobs; do
        [ -d "$home/$directory" ] || mkdir -m 700 -- "$home/$directory"
    done
    # A hard restart leaves the control socket behind. Remove it when no
    # sidecar runs.
    socket=$home/node/outbe-control.sock
    if [ -S "$socket" ] && ! pgrep -x outbe-radicle >/dev/null 2>&1; then
        rm -f "$socket"
    fi
    exec outbe-radicle \
        --home "$home" \
        --control-socket "$socket" \
        --listen "0.0.0.0:$RADICLE_PORT" \
        --status-listen 127.0.0.1:8876 \
        --max-validators 128 \
        --external-inbound-reserve 16 \
        --advertise "$OUTBE_EXTERNAL_ADDRESS:$RADICLE_PORT"
}

# Lease that a validator requests when it joins again: three days.
REJOIN_LEASE_SECONDS=259200

# Join the network again when the enclave lost its state.
#
# The hardware service keeps its state in memory, so a restart of the VM or of
# the service gives it a fresh identity, while the registry still holds the
# binding of the previous one.
# The validator then registers again with fresh hardware evidence and receives
# the offer key from the network. The image has no shell access, so this
# script does what an operator would do with `outbe-cli tee join`.
#
# The marker file is on tmpfs: it shows that the enclave of this boot is the
# one that the node data directory knows.
#
# The request file is on tmpfs too: it belongs to one service process. The
# node data directory can hold the records of an attempt that an earlier
# service process did not finish, for example when the VM restarted during
# the attempt or the network rejected it. That process does not exist, so its
# attempt cannot continue, and its records stop a new attempt: remove them
# before the first attempt of this service process.
rejoin_if_needed() {
    local data=$1 marker=/run/outbe/enclave-current request=/run/outbe/rejoin-request
    [ ! -f "$marker" ] || return 0
    if [ "$OUTBE_TEE_MODE" = confidential-vm ] &&
        [ -f "$data/tee-node-host-v1/initialization-manifest.bin" ]; then
        if [ -z "${OUTBE_UPSTREAM_URL:-}" ]; then
            echo "outbe-node: the enclave lost its state; set upstream_url in the user config to join again" >&2
            exit 1
        fi
        # Keep the request stable while one registration is in progress.
        if [ ! -s "$request" ]; then
            find "$data/tee-node-host-v1" -maxdepth 1 -type f -name 'replacement-*' -delete
            printf '0x%s %s\n' "$(head -c 32 /dev/urandom | od -An -v -tx1 | tr -d ' \n')" \
                "$(($(date +%s) + REJOIN_LEASE_SECONDS))" > "$request"
        fi
        local binding valid
        read -r binding valid < "$request"
        echo "outbe-node: joining the network again through $OUTBE_UPSTREAM_URL" >&2
        outbe-cli tee join \
            --enclave-socket "$ENCLAVE_SOCKET" \
            --rpc-url "$OUTBE_UPSTREAM_URL" \
            --node-data-dir "$data" \
            --private-key "$(tr -d '[:space:]' < "$KEYS/evm-key.hex")" \
            --reth-p2p-secret-key "$P2P_KEY" \
            --genesis "$GENESIS" \
            --binding-id "$binding" \
            --valid-until "$valid" \
            --timeout-secs 120
        rm -f "$request"
    fi
    : > "$marker"
}

run_node() {
    local data=$HOME_DIR/data bootnodes
    local -a upstream=()
    # A validator that joined again catches up through a healthy node.
    if [ -n "${OUTBE_UPSTREAM_URL:-}" ]; then
        upstream=(--upstream "$OUTBE_UPSTREAM_URL")
    fi
    mkdir -p "$data" "$HOME_DIR/logs"
    export BB_CRS_PATH=$BASE/crs/bn254_g1.dat
    # AMD certificates for SEV-SNP evidence and Intel collateral for TDX
    # evidence, when the host does not supply them.
    export OUTBE_SEV_SNP_CERT_DIR=$BASE/sev-snp-certs
    export OUTBE_TDX_COLLATERAL_DIR=$BASE/tdx-collateral
    # Block 1 initializes the secp256k1 tables in a frame that overflows the
    # default thread stack of reth.
    export RUST_MIN_STACK=16777216
    # reth reads the p2p key from the file verbatim, and the key file of the
    # bundle can end with a newline. Give reth a copy without white space. The
    # copy is on tmpfs: this script writes nothing to a key file on the data
    # disk, so a power loss cannot damage a key.
    (umask 077 && printf '%s' "$(tr -d '[:space:]' < "$KEYS/reth-p2p-secret.hex")" > "$P2P_KEY")
    bootnodes=$(grep -v '^[[:space:]]*#' "$BASE/reth-bootnodes.txt" | paste -sd, -)
    rejoin_if_needed "$data"
    exec outbe-chain node \
        --validator \
        "${upstream[@]}" \
        --consensus.use-local-defaults \
        --chain "$GENESIS" \
        --datadir "$data" \
        --engine.persistence-threshold 0 \
        --engine.memory-block-buffer-target 0 \
        --consensus.signing-key "$KEYS/signing-key.hex" \
        --validator.evm-key "$KEYS/evm-key.hex" \
        --consensus.listen-addr "0.0.0.0:$CONSENSUS_PORT" \
        --consensus.storage-dir "$HOME_DIR/consensus" \
        --radicle.control-socket "$KEYS/radicle/node/outbe-control.sock" \
        --radicle.status-address 127.0.0.1:8876 \
        --tee-enclave-socket "$ENCLAVE_SOCKET" \
        --http --http.addr 0.0.0.0 --http.port 8545 \
        --http.api eth,net,web3,outbe \
        --authrpc.port 8551 \
        --port "$RETH_PORT" \
        --discovery.port "$RETH_PORT" \
        --nat "extip:$OUTBE_EXTERNAL_ADDRESS" \
        --discovery.v5.addr 0.0.0.0 \
        --discovery.v5.port "$DISCV5_PORT" \
        --p2p-secret-key "$P2P_KEY" \
        --bootnodes "$bootnodes" \
        --metrics 127.0.0.1:9101 \
        --ipcpath "$data/reth.ipc" \
        --log.file.directory "$HOME_DIR/logs"
}

case "$role" in
    enclave) run_enclave ;;
    radicle) run_radicle ;;
    node) run_node ;;
    *) echo "unknown role: $role" >&2; exit 2 ;;
esac
