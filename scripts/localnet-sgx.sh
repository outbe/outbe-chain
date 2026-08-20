#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"

ACTION=${1:-status}
BASE_DIR=$(realpath -m "${LOCALNET_SGX_DIR:-${OUT_DIR:-/tmp/outbe-testnet}-sgx}")
VALIDATOR_COUNT=4
CHAIN_ID=54322345
SEED_FILE=${SEED_FILE:-$ROOT_DIR/scripts/seed-testnet-lowstake.json}
CHAIN_BIN=$ROOT_DIR/target/release/outbe-chain
KEYGEN_BIN=$ROOT_DIR/target/release/outbe-keygen
OCOMP_BIN=$ROOT_DIR/target/release/outbe-ocomp
ENCLAVE_BIN=$ROOT_DIR/target/release/outbe-tee-enclave
GRAMINE_IMAGE=${OUTBE_TEE_GRAMINE_IMAGE:-outbe-tee-enclave-gramine-test:latest}

RPC_BASE=${LOCALNET_SGX_RPC_BASE:-18545}
AUTHRPC_BASE=${LOCALNET_SGX_AUTHRPC_BASE:-19551}
RETH_P2P_BASE=${LOCALNET_SGX_RETH_P2P_BASE:-20303}
RETH_DISCOVERY_BASE=${LOCALNET_SGX_RETH_DISCOVERY_BASE:-21303}
METRICS_BASE=${LOCALNET_SGX_METRICS_BASE:-19101}
CONSENSUS_BASE=${LOCALNET_SGX_CONSENSUS_BASE:-22400}
TEE_BASE=${LOCALNET_SGX_TEE_BASE:-27000}
MONGO_NAME=${LOCALNET_SGX_MONGO_NAME:-outbe-localnet-sgx-mongodb}
MONGO_PORT=${LOCALNET_SGX_MONGO_PORT:-27037}
MONGO_VOLUME=${LOCALNET_SGX_MONGO_VOLUME:-outbe-localnet-sgx-mongodb-data}
MONGO_URI="mongodb://127.0.0.1:${MONGO_PORT}/?replicaSet=rs0&directConnection=true"
DATABASE_PREFIX=${LOCALNET_SGX_DATABASE_PREFIX:-outbe_localnet_sgx}
TEST_PROTOCOL_OVERRIDES=${LOCALNET_SGX_TEST_PROTOCOL_OVERRIDES:-0}
METADOSIS_ADVANCE_INTERVAL_SECONDS=${LOCALNET_SGX_METADOSIS_ADVANCE_INTERVAL_SECONDS:-300}

case "$BASE_DIR" in
  /tmp/outbe-?*) ;;
  *)
    echo "LOCALNET_SGX_DIR must be a dedicated /tmp/outbe-* path (got: $BASE_DIR)" >&2
    exit 2
    ;;
esac

export OUTBE_LOCALNET_MONGO_NAME=$MONGO_NAME
export OUTBE_LOCALNET_MONGO_PORT=$MONGO_PORT
export OUTBE_LOCALNET_MONGO_VOLUME=$MONGO_VOLUME

docker_cmd() {
  if docker info >/dev/null 2>&1; then
    docker "$@"
  else
    sudo -n docker "$@"
  fi
}

require_file() {
  [[ -f "$1" ]] || { echo "required file is missing: $1" >&2; exit 1; }
}

require_executable() {
  [[ -x "$1" ]] || { echo "required release binary is missing: $1" >&2; exit 1; }
}

pid_alive() {
  local pid_file=$1
  [[ -f "$pid_file" ]] || return 1
  local pid
  pid=$(tr -cd '0-9' < "$pid_file")
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

ensure_not_running() {
  local pid_file
  for pid_file in "$BASE_DIR"/run/*.pid; do
    [[ -e "$pid_file" ]] || continue
    if pid_alive "$pid_file"; then
      echo "network is still running (PID $(cat "$pid_file"), $pid_file)" >&2
      echo "run 'mise run localnet-sgx-stop' first" >&2
      exit 1
    fi
  done
}

rpc_result() {
  local port=$1 method=$2 params=${3:-'[]'}
  curl --fail --silent --show-error \
    -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$method\",\"params\":$params}" \
    "http://127.0.0.1:$port" | jq -er '.result'
}

protocol_bundle_hash_from_genesis() {
  local canonical_install request_profile protocol_hash
  canonical_install=$(jq -er '.config.ocompForkInstallV1.canonicalBytes' "$BASE_DIR/genesis.json")
  request_profile=${canonical_install#*4f4d52500001}
  protocol_hash=0x${request_profile:144:64}
  [[ "$protocol_hash" =~ ^0x[0-9a-f]{64}$ ]] || {
    echo "cannot extract protocol bundle hash from canonical OCOMP install" >&2
    return 1
  }
  printf '%s\n' "$protocol_hash"
}

wait_tcp() {
  local port=$1 attempts=${2:-300}
  local attempt
  for attempt in $(seq 1 "$attempts"); do
    if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
      exec 3>&- 2>/dev/null || true
      return 0
    fi
    sleep 0.1
  done
  return 1
}

generate_test_signing_key() {
  local key=$BASE_DIR/test-sgx-signing-key.pem
  if [[ -f "$key" ]]; then
    return
  fi
  docker_cmd run --rm \
    --user "$(id -u):$(id -g)" \
    --entrypoint gramine-sgx-gen-private-key \
    -v "$BASE_DIR:/keys" \
    "$GRAMINE_IMAGE" \
    /keys/test-sgx-signing-key.pem
  chmod 600 "$key"
}

bootstrap() {
  command -v jq >/dev/null
  command -v python3 >/dev/null
  command -v docker >/dev/null
  require_executable "$CHAIN_BIN"
  require_executable "$KEYGEN_BIN"
  require_executable "$OCOMP_BIN"
  require_executable "$ENCLAVE_BIN"
  require_file "$SEED_FILE"
  ensure_not_running

  # A fresh genesis must never reuse the dedicated projection replica-set
  # identity from an older localnet generation.
  ./scripts/localnet-mongo.sh clean >/dev/null 2>&1 || true
  rm -rf -- "$BASE_DIR"
  mkdir -p "$BASE_DIR/run"

  "$CHAIN_BIN" dkg bootstrap \
    --output-dir "$BASE_DIR" \
    --validators "$VALIDATOR_COUNT"

  local validators_tmp=$BASE_DIR/validators.json.tmp
  jq --argjson base "$CONSENSUS_BASE" '
    to_entries
    | map(.value + {p2p_address: ("127.0.0.1:" + (($base + (.key * 10)) | tostring))})
  ' "$BASE_DIR/validators.json" > "$validators_tmp"
  mv -f "$validators_tmp" "$BASE_DIR/validators.json"

  local bootnodes_tmp=$BASE_DIR/reth-bootnodes.txt.tmp
  awk -v base="$RETH_P2P_BASE" '
    NF {
      sub(/:[0-9]+$/, ":" (base + count))
      print
      count++
    }
  ' "$BASE_DIR/reth-bootnodes.txt" > "$bootnodes_tmp"
  mv -f "$bootnodes_tmp" "$BASE_DIR/reth-bootnodes.txt"

  local now genesis_time worldwide_day
  now=$(date -u +%s)
  genesis_time=$(date -u -d '1 day ago' '+%Y-%m-%dT%H:%M:%SZ')
  worldwide_day=$(date -u -d '+14 hours' '+%Y%m%d')

  jq -n \
    --argjson chain_id "$CHAIN_ID" \
    --arg genesis_time "$genesis_time" \
    --arg timestamp "0x$(printf '%x' "$now")" \
    --slurpfile validators "$BASE_DIR/validators.json" '
    {
      config: {
        chainId: $chain_id,
        homesteadBlock: 0,
        eip150Block: 0,
        eip155Block: 0,
        eip158Block: 0,
        byzantiumBlock: 0,
        constantinopleBlock: 0,
        petersburgBlock: 0,
        istanbulBlock: 0,
        berlinBlock: 0,
        londonBlock: 0,
        mergeNetsplitBlock: 0,
        terminalTotalDifficulty: 0,
        terminalTotalDifficultyPassed: true,
        shanghaiTime: 0,
        cancunTime: 0,
        pragueTime: 0,
        epochLengthBlocks: 300,
        dkgPrepareWindowBlocks: 30,
        dkgActivationGraceBlocks: 30,
        genesisTime: $genesis_time
      },
      nonce: "0x0",
      timestamp: $timestamp,
      extraData: "0x",
      gasLimit: "0x1c9c380",
      difficulty: "0x0",
      mixHash: "0x0000000000000000000000000000000000000000000000000000000000000000",
      coinbase: "0x0000000000000000000000000000000000000000",
      alloc: (reduce $validators[0][] as $validator ({};
        . + {($validator.address | sub("^0x"; "")): {balance: "0x21E19E0C9BAB2400000"}}
      ))
    }
  ' > "$BASE_DIR/genesis.base.json"

  python3 "$ROOT_DIR/scripts/seed_genesis.py" \
    --genesis "$BASE_DIR/genesis.base.json" \
    --seed "$SEED_FILE" \
    --validators "$BASE_DIR/validators.json" \
    --worldwide-day "$worldwide_day" \
    --fresh-metadosis \
    --output "$BASE_DIR/genesis.seeded.json"

  local genesis_protocol_input
  if [[ "$TEST_PROTOCOL_OVERRIDES" == 1 ]]; then
    [[ "$METADOSIS_ADVANCE_INTERVAL_SECONDS" =~ ^[1-9][0-9]*$ ]] || {
      echo "LOCALNET_SGX_METADOSIS_ADVANCE_INTERVAL_SECONDS must be a positive integer" >&2
      exit 2
    }
    genesis_protocol_input=$BASE_DIR/genesis.seeded.test-overrides.json
    jq --argjson advance_interval "$METADOSIS_ADVANCE_INTERVAL_SECONDS" '
      .config.outbeProtocol = {
        schemaVersion: 1,
        metadosis: {advanceIntervalSeconds: $advance_interval}
      }
    ' "$BASE_DIR/genesis.seeded.json" > "$genesis_protocol_input"
  else
    # Production-feature binaries reject test-only protocol overrides. This is
    # the only post-seeding genesis mutation: all other seeded fields are kept.
    genesis_protocol_input=$BASE_DIR/genesis.seeded.no-overrides.json
    jq 'del(.config.outbeProtocol)' \
      "$BASE_DIR/genesis.seeded.json" > "$genesis_protocol_input"
  fi

  "$CHAIN_BIN" ocomp bindings \
    --input "$genesis_protocol_input" \
    --validators "$BASE_DIR/validators.json" \
    --output "$BASE_DIR/ocomp-bindings-v1.json"

  local genesis_hash validator_address consensus_key index validator_dir
  genesis_hash=$(jq -er '.genesisHash' "$BASE_DIR/ocomp-bindings-v1.json")
  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    validator_dir=$BASE_DIR/validator-$index
    validator_address=$(jq -er ".[$index].address" "$BASE_DIR/validators.json")
    consensus_key=$(jq -er ".[$index].public_key" "$BASE_DIR/validators.json")
    "$KEYGEN_BIN" ocomp \
      --output-dir "$validator_dir" \
      --chain-id "$CHAIN_ID" \
      --genesis-hash "$genesis_hash" \
      --validator-address "$validator_address" \
      --consensus-bls-min-pk "$consensus_key"
  done

  "$CHAIN_BIN" ocomp genesis \
    --input "$genesis_protocol_input" \
    --validators "$BASE_DIR/validators.json" \
    --registrations-dir "$BASE_DIR" \
    --output "$BASE_DIR/genesis.ocomp.json" \
    --protocol-bundle-output "$BASE_DIR/protocol-bundle-v1.ocb1"

  "$CHAIN_BIN" tee genesis \
    --input "$BASE_DIR/genesis.ocomp.json" \
    --output "$BASE_DIR/genesis.json" \
    --mode gramine-direct-dev

  jq -e --argjson chain_id "$CHAIN_ID" \
    --argjson test_protocol_overrides "$TEST_PROTOCOL_OVERRIDES" \
    --argjson advance_interval "$METADOSIS_ADVANCE_INTERVAL_SECONDS" '
    .config.chainId == $chain_id
    and (if $test_protocol_overrides == 1 then
      .config.outbeProtocol.metadosis.advanceIntervalSeconds == $advance_interval
    else
      (.config | has("outbeProtocol") | not)
    end)
    and (.config.ocompForkInstallV1.installHash | type == "string")
    and (.config.teeAttestationV1 | type == "object")
  ' "$BASE_DIR/genesis.json" >/dev/null

  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    validator_dir=$BASE_DIR/validator-$index
    local domain=$validator_dir/ocomp/domain-v1
    mkdir -p "$domain" "$validator_dir/tee" "$validator_dir/data" \
      "$validator_dir/logs" "$validator_dir/consensus"
    cp -f "$BASE_DIR/protocol-bundle-v1.ocb1" "$domain/protocol-bundle-v1.ocb1"
    cp -f "$validator_dir/ocomp-key-v1.hex" "$domain/ocomp-key-v1.hex"
    printf '%s\n' "$(tr -d '\r\n' < "$validator_dir/evm-key.hex" | sed -E 's/^0x//')" \
      > "$domain/ocomp-evm-key.hex"
    chmod 640 "$domain/protocol-bundle-v1.ocb1"
    chmod 600 "$domain/ocomp-key-v1.hex" "$domain/ocomp-evm-key.hex"
  done

  if ! docker_cmd image inspect "$GRAMINE_IMAGE" >/dev/null 2>&1; then
    docker_cmd build \
      -f bin/outbe-tee-enclave/gramine/Dockerfile.test \
      -t "$GRAMINE_IMAGE" \
      bin/outbe-tee-enclave/gramine
  fi
  generate_test_signing_key

  printf '%s\n' "$CHAIN_ID" > "$BASE_DIR/chain-id"
  printf '%s\n' "$genesis_hash" > "$BASE_DIR/genesis-hash"
  jq -er '.config.ocompForkInstallV1.installHash' "$BASE_DIR/genesis.json" > "$BASE_DIR/ocomp-boot-nonce"
  protocol_bundle_hash_from_genesis > "$BASE_DIR/ocomp-protocol-bundle-hash"

  echo "SGX localnet bootstrapped at $BASE_DIR"
  if [[ "$TEST_PROTOCOL_OVERRIDES" == 1 ]]; then
    echo "chainId=$CHAIN_ID; metadosis.advanceIntervalSeconds=$METADOSIS_ADVANCE_INTERVAL_SECONDS"
  else
    echo "chainId=$CHAIN_ID; config.outbeProtocol is absent"
  fi
}

start_enclaves() {
  [[ -e /dev/sgx_enclave ]] || { echo "/dev/sgx_enclave is missing" >&2; exit 1; }
  [[ -e /dev/sgx_provision ]] || { echo "/dev/sgx_provision is missing" >&2; exit 1; }
  local index container tee_port validator_dir
  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    container=outbe-localnet-sgx-enclave-$index
    tee_port=$((TEE_BASE + index))
    validator_dir=$BASE_DIR/validator-$index
    docker_cmd rm -f "$container" >/dev/null 2>&1 || true
    docker_cmd run -d \
      --name "$container" \
      --security-opt seccomp=unconfined \
      --network host \
      --device /dev/sgx_enclave:/dev/sgx_enclave \
      --device /dev/sgx_provision:/dev/sgx_provision \
      -e OUTBE_TEST_REMOTE_ATTESTATION=none \
      -v "$ENCLAVE_BIN:/app/outbe-tee-enclave:ro" \
      -v "$BASE_DIR/test-sgx-signing-key.pem:/run/secrets/outbe-test-sgx-key.pem:ro" \
      -v "$validator_dir/tee:/tee" \
      "$GRAMINE_IMAGE" \
      --socket "127.0.0.1:$tee_port" \
      --tee-dir /tee \
      --chain-id "0x$(printf '%064x' "$CHAIN_ID")" >/dev/null
    printf '%s\n' "$container" > "$BASE_DIR/run/enclave-$index.container"
  done
  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    tee_port=$((TEE_BASE + index))
    if ! wait_tcp "$tee_port" 600; then
      docker_cmd logs "outbe-localnet-sgx-enclave-$index" >&2 || true
      echo "validator-$index SGX enclave did not open 127.0.0.1:$tee_port" >&2
      exit 1
    fi
  done
}

start_nodes() {
  local bootnodes index validator_dir rpc_port p2p_port disc_port auth_port metrics_port consensus_port tee_port
  bootnodes=$(grep -v '^[[:space:]]*#' "$BASE_DIR/reth-bootnodes.txt" | paste -sd, -)
  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    validator_dir=$BASE_DIR/validator-$index
    rpc_port=$((RPC_BASE + index))
    p2p_port=$((RETH_P2P_BASE + index))
    disc_port=$((RETH_DISCOVERY_BASE + index))
    auth_port=$((AUTHRPC_BASE + index))
    metrics_port=$((METRICS_BASE + index))
    consensus_port=$((CONSENSUS_BASE + index * 10))
    tee_port=$((TEE_BASE + index))
    rm -f "$validator_dir/node.exit" "$validator_dir/data/db/lock"
    nohup "$ROOT_DIR/scripts/run-supervised.sh" "$validator_dir/node.exit" \
      env \
        RUST_MIN_STACK=16777216 \
        OUTBE_PROJECTION_MONGODB_URI="$MONGO_URI" \
        OUTBE_PROJECTION_MONGODB_DATABASE="${DATABASE_PREFIX}_validator_${index}" \
      "$CHAIN_BIN" node \
        --validator \
        --chain "$BASE_DIR/genesis.json" \
        --datadir "$validator_dir/data" \
        --http --http.addr 127.0.0.1 --http.port "$rpc_port" \
        --http.api eth,net,web3,outbe \
        --rpc.eth-proof-window 1868 \
        --port "$p2p_port" \
        --discovery.port "$p2p_port" \
        --discovery.v5.addr 127.0.0.1 --discovery.v5.port "$disc_port" \
        --bootnodes "$bootnodes" \
        --p2p-secret-key-hex "$(tr -d '[:space:]' < "$validator_dir/reth-p2p-secret.hex")" \
        --authrpc.port "$auth_port" \
        --ipcpath "$validator_dir/data/reth.ipc" \
        --metrics "127.0.0.1:$metrics_port" \
        --engine.persistence-threshold 0 \
        --engine.memory-block-buffer-target 0 \
        --tee-enclave-socket "127.0.0.1:$tee_port" \
        --tee-session-mode production-node-host \
        --tee-bootstrap-timeout-secs 600 \
        --log.file.directory "$validator_dir/logs" \
        --log.file.max-size 500 \
        --log.file.max-files 40 \
        --color never \
        --consensus.signing-key "$validator_dir/signing-key.hex" \
        --validator.evm-key "$validator_dir/evm-key.hex" \
        --consensus.listen-addr "127.0.0.1:$consensus_port" \
        --consensus.storage-dir "$validator_dir/consensus" \
        --consensus.use-local-defaults \
      > "$validator_dir/node.log" 2>&1 < /dev/null &
    printf '%s\n' "$!" > "$BASE_DIR/run/node-$index.pid"
  done
}

wait_for_chain() {
  local attempt index height all_ready
  for attempt in $(seq 1 300); do
    all_ready=1
    for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
      if ! pid_alive "$BASE_DIR/run/node-$index.pid"; then
        echo "validator-$index exited during startup" >&2
        tail -n 60 "$BASE_DIR/validator-$index/node.log" >&2 || true
        exit 1
      fi
      height=$(rpc_result "$((RPC_BASE + index))" eth_blockNumber 2>/dev/null || true)
      if [[ -z "$height" ]] || (( 16#${height#0x} < 1 )); then
        all_ready=0
        break
      fi
    done
    [[ $all_ready -eq 1 ]] && return
    sleep 2
  done
  echo "four-validator chain did not reach block 1 within 10 minutes" >&2
  exit 1
}

start_ocomp_roles() {
  local genesis_hash boot_nonce protocol_hash index rpc_port supervisor_port validator_dir common_env
  local attempt all_ready
  genesis_hash=$(<"$BASE_DIR/genesis-hash")
  boot_nonce=$(<"$BASE_DIR/ocomp-boot-nonce")
  protocol_hash=$(protocol_bundle_hash_from_genesis)
  for attempt in $(seq 1 12); do
    for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
      rpc_port=$((RPC_BASE + index))
      supervisor_port=$((CONSENSUS_BASE + index * 10 + 1))
      validator_dir=$BASE_DIR/validator-$index
      wait_tcp "$supervisor_port" 300 || {
        echo "embedded validator-$index OCOMP Supervisor endpoint is unavailable" >&2
        exit 1
      }
      if pid_alive "$BASE_DIR/run/snapshot-exporter-$index.pid" \
        && pid_alive "$BASE_DIR/run/worker-$index.pid"; then
        continue
      fi
      stop_pid_file "$BASE_DIR/run/snapshot-exporter-$index.pid"
      stop_pid_file "$BASE_DIR/run/worker-$index.pid"
      common_env=(
        OUTBE_OCOMP_BASE_PATH="$BASE_DIR"
        OCOMP_VALIDATOR_INDEX="$index"
        OCOMP_CHAIN_ID="$CHAIN_ID"
        OCOMP_GENESIS_HASH="$genesis_hash"
        OCOMP_BOOT_NONCE="$boot_nonce"
        OCOMP_PROTOCOL_BUNDLE_HASH="$protocol_hash"
        OUTBE_OCOMP_RPC_URL="http://127.0.0.1:$rpc_port"
        OUTBE_OCOMP_PROJECTION_MONGODB_URI="$MONGO_URI"
        OUTBE_OCOMP_PROJECTION_MONGODB_DATABASE="${DATABASE_PREFIX}_validator_${index}_ocomp"
      )
      nohup env "${common_env[@]}" "$OCOMP_BIN" snapshot-exporter \
        --supervisor-address "127.0.0.1:$supervisor_port" \
        > "$validator_dir/ocomp-snapshot-exporter.log" 2>&1 < /dev/null &
      printf '%s\n' "$!" > "$BASE_DIR/run/snapshot-exporter-$index.pid"
      nohup env "${common_env[@]}" "$OCOMP_BIN" worker \
        --chain-id "$CHAIN_ID" \
        --genesis-hash "$genesis_hash" \
        --boot-nonce "$boot_nonce" \
        --protocol-bundle-hash "$protocol_hash" \
        --supervisor-address "127.0.0.1:$supervisor_port" \
        --worker-ordinal 0 \
        > "$validator_dir/ocomp-worker.log" 2>&1 < /dev/null &
      printf '%s\n' "$!" > "$BASE_DIR/run/worker-$index.pid"
    done

    sleep 5
    all_ready=1
    for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
      pid_alive "$BASE_DIR/run/snapshot-exporter-$index.pid" \
        && pid_alive "$BASE_DIR/run/worker-$index.pid" \
        || all_ready=0
    done
    if [[ $all_ready -eq 1 ]]; then
      sleep 10
      for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
        pid_alive "$BASE_DIR/run/snapshot-exporter-$index.pid" \
          && pid_alive "$BASE_DIR/run/worker-$index.pid" \
          || all_ready=0
      done
      [[ $all_ready -eq 1 ]] && return
    fi
  done
  echo "OCOMP SnapshotExporter/Worker roles did not remain healthy after bounded startup retries" >&2
  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    tail -n 20 "$BASE_DIR/validator-$index/ocomp-snapshot-exporter.log" >&2 || true
    tail -n 20 "$BASE_DIR/validator-$index/ocomp-worker.log" >&2 || true
  done
  exit 1
}

start() {
  require_file "$BASE_DIR/genesis.json"
  require_executable "$CHAIN_BIN"
  require_executable "$OCOMP_BIN"
  require_executable "$ENCLAVE_BIN"
  if pid_alive "$BASE_DIR/run/node-0.pid"; then
    start_ocomp_roles
    status
    return
  fi
  ./scripts/localnet-mongo.sh start
  start_enclaves
  start_nodes
  wait_for_chain
  start_ocomp_roles
  status
}

stop_pid_file() {
  local pid_file=$1
  [[ -f "$pid_file" ]] || return 0
  local pid
  pid=$(tr -cd '0-9' < "$pid_file")
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    kill -TERM "$pid" 2>/dev/null || true
    local attempt
    for attempt in $(seq 1 300); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
      pkill -KILL -P "$pid" 2>/dev/null || true
      kill -KILL "$pid" 2>/dev/null || true
    fi
  fi
  rm -f "$pid_file"
}

stop() {
  local index container
  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    stop_pid_file "$BASE_DIR/run/worker-$index.pid"
    stop_pid_file "$BASE_DIR/run/snapshot-exporter-$index.pid"
  done
  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    stop_pid_file "$BASE_DIR/run/node-$index.pid"
  done
  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    container=outbe-localnet-sgx-enclave-$index
    docker_cmd rm -f "$container" >/dev/null 2>&1 || true
    rm -f "$BASE_DIR/run/enclave-$index.container"
  done
  ./scripts/localnet-mongo.sh stop || true
  echo "SGX localnet stopped; data kept at $BASE_DIR"
}

status() {
  require_file "$BASE_DIR/genesis.json"
  local index chain_hex height node_state worker_state exporter_state enclave_state errors=0
  printf 'chainId=%s outbeProtocol=%s\n' \
    "$(jq -r '.config.chainId' "$BASE_DIR/genesis.json")" \
    "$(jq -r 'if .config | has("outbeProtocol") then "present" else "absent" end' "$BASE_DIR/genesis.json")"
  for index in $(seq 0 $((VALIDATOR_COUNT - 1))); do
    node_state=dead
    worker_state=dead
    exporter_state=dead
    enclave_state=dead
    pid_alive "$BASE_DIR/run/node-$index.pid" && node_state=running
    pid_alive "$BASE_DIR/run/worker-$index.pid" && worker_state=running
    pid_alive "$BASE_DIR/run/snapshot-exporter-$index.pid" && exporter_state=running
    if docker_cmd inspect -f '{{.State.Running}}' "outbe-localnet-sgx-enclave-$index" 2>/dev/null | grep -qx true; then
      enclave_state=running
    fi
    chain_hex=$(rpc_result "$((RPC_BASE + index))" eth_chainId 2>/dev/null || true)
    height=$(rpc_result "$((RPC_BASE + index))" eth_blockNumber 2>/dev/null || true)
    printf 'validator-%s node=%s enclave=%s snapshot-exporter=%s worker=%s chain=%s height=%s\n' \
      "$index" "$node_state" "$enclave_state" "$exporter_state" "$worker_state" \
      "${chain_hex:-unavailable}" "${height:-unavailable}"
    [[ "$node_state" == running \
      && "$enclave_state" == running \
      && "$worker_state" == running \
      && "$exporter_state" == running \
      && "$chain_hex" == "0x$(printf '%x' "$CHAIN_ID")" \
      && -n "$height" ]] || errors=$((errors + 1))
    if [[ -f "$BASE_DIR/validator-$index/node.log" ]] && \
      tail -n 400 "$BASE_DIR/validator-$index/node.log" | rg -i -q '(^|[^a-z])(panic|fatal|error)([^a-z]|$)'; then
      printf 'validator-%s recent_log_errors=yes\n' "$index"
    else
      printf 'validator-%s recent_log_errors=no\n' "$index"
    fi
  done
  [[ $errors -eq 0 ]]
}

logs() {
  local index=${2:-0}
  tail -n 120 "$BASE_DIR/validator-$index/node.log"
  docker_cmd logs --tail 120 "outbe-localnet-sgx-enclave-$index" 2>&1 || true
  tail -n 120 "$BASE_DIR/validator-$index/ocomp-snapshot-exporter.log" 2>/dev/null || true
  tail -n 120 "$BASE_DIR/validator-$index/ocomp-worker.log" 2>/dev/null || true
}

case "$ACTION" in
  bootstrap) bootstrap ;;
  start) start ;;
  status) status ;;
  stop) stop ;;
  logs) logs "$@" ;;
  *)
    echo "usage: $0 {bootstrap|start|status|stop|logs [validator-index]}" >&2
    exit 2
    ;;
esac
