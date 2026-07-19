#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT_DIR"

ACTION=${1:-start}
STACK_DIR=$(realpath -m "${LOCALNET_STACK_DIR:-/tmp/outbe-localnet-stack}")
MONGO_NAME=${LOCALNET_STACK_MONGO_NAME:-outbe-localnet-stack-mongodb}
MONGO_VOLUME="${MONGO_NAME}-data"
MONGO_PORT=${LOCALNET_STACK_MONGO_PORT:-27027}
PORT_OFFSET=${LOCALNET_STACK_PORT_OFFSET:-1000}
DATABASE_PREFIX=${LOCALNET_STACK_DATABASE_PREFIX:-outbe_localnet_stack}
RPC_PORT=$((8545 + PORT_OFFSET))
RPC_URL="http://127.0.0.1:${RPC_PORT}"
MONGO_URI="mongodb://127.0.0.1:${MONGO_PORT}/?replicaSet=rs0&directConnection=true"

case "$STACK_DIR" in
  /tmp/outbe-?*) ;;
  *)
    echo "LOCALNET_STACK_DIR must resolve to a dedicated /tmp/outbe-* path (got: $STACK_DIR)" >&2
    exit 2
    ;;
esac

docker_cmd() {
  if docker info >/dev/null 2>&1; then
    docker "$@"
  else
    sudo docker "$@"
  fi
}

network_env() {
  export PORT_OFFSET
  export OUTBE_CHAIN_BINARY="$ROOT_DIR/target/debug/outbe-chain"
  export OUTBE_PROJECTION_MONGODB_URI="$MONGO_URI"
  export OUTBE_PROJECTION_MONGODB_DATABASE_PREFIX="$DATABASE_PREFIX"
  export OUTBE_TEE_ENCLAVE=1
  export OUTBE_TEE_ENCLAVE_MOCK=1
  # A stack stop tears down every enclave together. Persist each validator's
  # DKG-derived offer key so the next start can recover without depending on a
  # still-running committee member for TEE key handoff.
  export OUTBE_TEE_SEAL=1
  export OUTBE_TEE_ENCLAVE_BINARY="$ROOT_DIR/target/release/outbe-tee-enclave-mock"
}

stop_stack() {
  network_env
  if [[ -d "$STACK_DIR" ]]; then
    ./scripts/run-testnet.sh stop "$STACK_DIR" || true
  fi
  docker_cmd stop "$MONGO_NAME" >/dev/null 2>&1 || true
}

remove_stack() {
  stop_stack
  docker_cmd rm -f "$MONGO_NAME" >/dev/null 2>&1 || true
  docker_cmd volume rm -f "$MONGO_VOLUME" >/dev/null 2>&1 || true
}

print_connection_info() {
  cat <<EOF

Localnet stack is ready.

Primary RPC:    $RPC_URL
Validator RPCs: $RPC_URL, http://127.0.0.1:$((RPC_PORT + 1)), http://127.0.0.1:$((RPC_PORT + 2)), http://127.0.0.1:$((RPC_PORT + 3))
MongoDB URI:    $MONGO_URI
MongoDB name:   $MONGO_NAME
DB prefix:      $DATABASE_PREFIX
Data dir:       $STACK_DIR
Mongo volume:   $MONGO_VOLUME

Useful environment for manual flows:

  export OUT_DIR="$STACK_DIR"
  export RPC_PORT="$RPC_PORT"
  export RPC_URL="$RPC_URL"
  export OUTBE_PROJECTION_MONGODB_URI="$MONGO_URI"
  export OUTBE_PROJECTION_MONGODB_DATABASE_PREFIX="$DATABASE_PREFIX"

Stop services but keep chain data:

  mise run localnet-stack-stop

Stop services and delete chain data:

  mise run localnet-stack-clean
EOF
}

case "$ACTION" in
  start)
    command -v docker >/dev/null || { echo "docker is required" >&2; exit 1; }
    command -v cast >/dev/null || { echo "cast is required (mise install)" >&2; exit 1; }

    start_complete=0
    trap 'if [[ $start_complete -ne 1 ]]; then remove_stack; fi' EXIT

    remove_stack
    if ! rm -rf "$STACK_DIR" 2>/dev/null; then
      sudo rm -rf "$STACK_DIR"
    fi

    cargo build -p outbe-chain --bin outbe-chain -p outbe-cli
    cargo build --release -p outbe-tee-enclave --features mock --bin outbe-tee-enclave-mock

    docker_cmd volume create "$MONGO_VOLUME" >/dev/null
    docker_cmd run -d --name "$MONGO_NAME" --network host \
      --mount "source=${MONGO_VOLUME},target=/data/db" mongo:7.0 \
      --replSet rs0 --bind_ip 127.0.0.1 --port "$MONGO_PORT" >/dev/null

    mongo_listening=0
    for _ in $(seq 1 80); do
      if docker_cmd exec "$MONGO_NAME" mongosh --quiet --port "$MONGO_PORT" \
        --eval 'db.runCommand({ping:1}).ok' >/dev/null 2>&1; then
        mongo_listening=1
        break
      fi
      sleep 0.25
    done
    [[ $mongo_listening -eq 1 ]] || { echo "MongoDB did not become reachable" >&2; exit 1; }

    docker_cmd exec "$MONGO_NAME" mongosh --quiet --port "$MONGO_PORT" --eval \
      "rs.initiate({_id:'rs0',members:[{_id:0,host:'127.0.0.1:${MONGO_PORT}'}]})" >/dev/null
    mongo_primary=0
    for _ in $(seq 1 80); do
      if docker_cmd exec "$MONGO_NAME" mongosh --quiet --port "$MONGO_PORT" \
        --eval 'if (!db.hello().isWritablePrimary) quit(1)' >/dev/null 2>&1; then
        mongo_primary=1
        break
      fi
      sleep 0.25
    done
    [[ $mongo_primary -eq 1 ]] || { echo "MongoDB replica set did not elect a primary" >&2; exit 1; }

    docker_cmd exec "$MONGO_NAME" mongosh --quiet --port "$MONGO_PORT" --eval '
      const s = db.getMongo().startSession();
      const probe = s.getDatabase("localnet_stack_transaction_probe");
      s.startTransaction();
      probe.probe.insertOne({ready:true});
      s.abortTransaction();
      s.endSession();
      if (db.getSiblingDB("localnet_stack_transaction_probe").probe.countDocuments({}) !== 0) quit(1);
    ' >/dev/null

    network_env
    ./scripts/bootstrap-testnet.sh 4 "$STACK_DIR"
    ./scripts/run-testnet.sh start "$STACK_DIR"

    rpc_ready=0
    for _ in $(seq 1 120); do
      if height=$(cast block-number --rpc-url "$RPC_URL" 2>/dev/null) && [[ ${height:-0} -ge 1 ]]; then
        rpc_ready=1
        break
      fi
      sleep 0.5
    done
    if [[ $rpc_ready -ne 1 ]]; then
      echo "Localnet RPC did not reach block 1 at $RPC_URL" >&2
      ./scripts/run-testnet.sh status "$STACK_DIR" >&2 || true
      exit 1
    fi

    for i in 0 1 2 3; do
      pid_file="$STACK_DIR/pids/validator-${i}.pid"
      pid=$(cat "$pid_file" 2>/dev/null || true)
      if [[ -z "$pid" ]] || ! kill -0 "$pid" 2>/dev/null; then
        echo "validator-$i is not running after RPC readiness" >&2
        exit 1
      fi
    done

    projection_ready=0
    for _ in $(seq 1 40); do
      count=$(docker_cmd exec "$MONGO_NAME" mongosh --quiet --port "$MONGO_PORT" --eval \
        "print(db.adminCommand({listDatabases:1,nameOnly:true}).databases.filter(x => x.name.startsWith('${DATABASE_PREFIX}_validator_')).length)" 2>/dev/null || echo 0)
      if [[ "$count" == "4" ]]; then
        projection_ready=1
        break
      fi
      sleep 0.25
    done
    [[ $projection_ready -eq 1 ]] || { echo "four validator projection databases were not initialized" >&2; exit 1; }

    ./scripts/run-testnet.sh status "$STACK_DIR"
    echo "Localnet reached block $height"
    print_connection_info
    start_complete=1
    trap - EXIT
    ;;
  stop)
    stop_stack
    echo "Localnet stack stopped; chain data kept at $STACK_DIR"
    ;;
  clean)
    remove_stack
    if ! rm -rf "$STACK_DIR" 2>/dev/null; then
      sudo rm -rf "$STACK_DIR"
    fi
    echo "Localnet stack stopped and removed: $STACK_DIR"
    ;;
  *)
    echo "usage: $0 {start|stop|clean}" >&2
    exit 2
    ;;
esac
