#!/usr/bin/env bash
#
# Standalone OCOMP lifecycle for a single validator host.
#
# Direct OCOMP launcher. The chain process must already expose its standard
# JSON-RPC endpoint; OCOMP has no private node control port. This script owns
# role identities, directories, process groups, status and shutdown directly.
#
# Usage:
#   sudo /opt/outbe-chain/ocomp.sh install
#   sudo /opt/outbe-chain/ocomp.sh start
#   sudo /opt/outbe-chain/ocomp.sh status
#   sudo /opt/outbe-chain/ocomp.sh logs
#   sudo /opt/outbe-chain/ocomp.sh stop

set -euo pipefail

readonly DEPLOY_ROOT="${OUTBE_OCOMP_BASE_PATH:-/opt/outbe-chain}"
readonly OCOMP_BINARY="$DEPLOY_ROOT/outbe-ocomp"
readonly OCOMP_ENV_FILE="$DEPLOY_ROOT/ocomp.env"
readonly OCOMP_EXPORT_ENV_FILE="$DEPLOY_ROOT/ocomp-export.env"
readonly PROTOCOL_BUNDLE="$DEPLOY_ROOT/protocol-bundle-v1.ocb1"
readonly RESULT_COMMITTEE="$DEPLOY_ROOT/result-committee-v1.ocb1"

# Role identities are fixed.
readonly SUPERVISOR_USER="outbe-ocomp-supervisor"
readonly EXPORTER_USER="outbe-ocomp-export"
readonly WORKER_USER="outbe-ocomp-worker"
readonly ARTIFACT_GROUP="outbe-ocomp-artifacts"

readonly OCOMP_ROOT="$DEPLOY_ROOT/ocomp"
readonly RUNTIME_ROOT="$OCOMP_ROOT/run"
readonly STATE_ROOT="$OCOMP_ROOT/data"
readonly KEY_ROOT="$STATE_ROOT/keys"
readonly OCOMP_EVM_KEY="$KEY_ROOT/ocomp-evm-key.hex"
readonly OCOMP_RESULT_KEY="$KEY_ROOT/ocomp-key-v1.hex"
readonly LOG_ROOT="$OCOMP_ROOT/logs"

readonly PID_ROOT="$RUNTIME_ROOT/pids"
readonly SUPERVISOR_PID="$PID_ROOT/supervisor/pid"
readonly EXPORTER_PID="$PID_ROOT/snapshot-exporter/pid"
readonly LIFECYCLE_LOCK="$RUNTIME_ROOT/lifecycle.lock"

readonly SUPERVISOR_LOG="$LOG_ROOT/supervisor.log"
readonly EXPORTER_LOG="$LOG_ROOT/snapshot-exporter.log"

worker_pid_file() {
  local ordinal=$1
  printf '%s/worker-%s/pid\n' "$PID_ROOT" "$ordinal"
}

worker_log_file() {
  local ordinal=$1
  printf '%s/worker-%s.log\n' "$LOG_ROOT" "$ordinal"
}

die() {
  echo "ocomp: $*" >&2
  exit 1
}

require_root() {
  [[ ${EUID:-$(id -u)} -eq 0 ]] || die "run as root (sudo $0 ...)"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is missing: $1"
}

ensure_group() {
  local group=$1
  getent group "$group" >/dev/null 2>&1 || groupadd --system "$group"
}

ensure_user() {
  local user=$1
  if ! id -u "$user" >/dev/null 2>&1; then
    ensure_group "$user"
    useradd \
      --system \
      --gid "$user" \
      --home-dir /nonexistent \
      --no-create-home \
      --shell /usr/sbin/nologin \
      "$user"
  fi
}

add_user_to_group() {
  local user=$1
  local group=$2
  id -nG "$user" | tr ' ' '\n' | grep -Fxq "$group" ||
    usermod --append --groups "$group" "$user"
}

prepare_identities() {
  ensure_group "$ARTIFACT_GROUP"
  ensure_user "$SUPERVISOR_USER"
  ensure_user "$EXPORTER_USER"
  ensure_user "$WORKER_USER"

  add_user_to_group "$SUPERVISOR_USER" "$ARTIFACT_GROUP"
  add_user_to_group "$EXPORTER_USER" "$ARTIFACT_GROUP"
  add_user_to_group "$WORKER_USER" "$ARTIFACT_GROUP"
}

prepare_directories() {
  install -d -m 0755 -o root -g root "$OCOMP_ROOT"
  install -d -m 0755 -o root -g root "$RUNTIME_ROOT"
  install -d -m 0700 -o root -g root "$PID_ROOT"
  install -d -m 0700 -o root -g root \
    "$PID_ROOT/supervisor"
  install -d -m 0700 -o root -g root \
    "$PID_ROOT/snapshot-exporter"
  local ordinal
  for ordinal in 0 1 2 3; do
    install -d -m 0700 -o root -g root "$PID_ROOT/worker-$ordinal"
  done
  install -d -m 0750 -o "$SUPERVISOR_USER" -g "$ARTIFACT_GROUP" "$STATE_ROOT"
  install -d -m 0700 -o "$SUPERVISOR_USER" -g "$SUPERVISOR_USER" "$KEY_ROOT"
  install -d -m 0770 -o "$SUPERVISOR_USER" -g "$ARTIFACT_GROUP" \
    "$STATE_ROOT/cas-v1" \
    "$STATE_ROOT/cas-v1/objects" \
    "$STATE_ROOT/cas-v1/refs" \
    "$STATE_ROOT/cas-v1/staging" \
    "$STATE_ROOT/worker-inbox-v1"
  install -d -m 0770 -o "$EXPORTER_USER" -g "$ARTIFACT_GROUP" \
    "$STATE_ROOT/cas-v1/staging/exporter"
  install -d -m 0770 -o "$SUPERVISOR_USER" -g "$ARTIFACT_GROUP" \
    "$STATE_ROOT/cas-v1/staging/supervisor"
  install -d -m 0750 -o "$EXPORTER_USER" -g "$ARTIFACT_GROUP" \
    "$STATE_ROOT/exporter-v1"
  install -d -m 0700 -o "$SUPERVISOR_USER" -g "$ARTIFACT_GROUP" \
    "$STATE_ROOT/supervisor-v1"
  install -d -m 0750 -o root -g root "$LOG_ROOT"
  local log_file
  for log_file in \
    "$SUPERVISOR_LOG" \
    "$EXPORTER_LOG" \
    "$(worker_log_file 0)" \
    "$(worker_log_file 1)" \
    "$(worker_log_file 2)" \
    "$(worker_log_file 3)"; do
    [[ ! -L "$log_file" ]] || die "refusing symlink log path: $log_file"
    if [[ ! -e "$log_file" ]]; then
      : >"$log_file"
    fi
    [[ -f "$log_file" ]] || die "log path is not a regular file: $log_file"
    chown root:root "$log_file"
    chmod 0640 "$log_file"
  done
}

prepare_ocomp_evm_key() {
  local created=0
  if [[ ! -e "$OCOMP_EVM_KEY" && ! -L "$OCOMP_EVM_KEY" ]]; then
    local temporary_key="$KEY_ROOT/.ocomp-evm-key.hex.$$"
    [[ ! -e "$temporary_key" && ! -L "$temporary_key" ]] ||
      die "refusing existing temporary key path: $temporary_key"
    (
      umask 077
      openssl rand -hex -out "$temporary_key" 32
    )
    chown "$SUPERVISOR_USER":"$SUPERVISOR_USER" "$temporary_key"
    chmod 0600 "$temporary_key"
    if ln -- "$temporary_key" "$OCOMP_EVM_KEY"; then
      rm -f -- "$temporary_key"
      created=1
    else
      rm -f -- "$temporary_key"
      [[ -e "$OCOMP_EVM_KEY" || -L "$OCOMP_EVM_KEY" ]] ||
        die "failed to install OCOMP EVM key at $OCOMP_EVM_KEY"
    fi
  fi

  [[ -f "$OCOMP_EVM_KEY" && ! -L "$OCOMP_EVM_KEY" ]] ||
    die "OCOMP EVM key is not a regular non-symlink file: $OCOMP_EVM_KEY"

  local expected_uid actual_uid mode link_count
  expected_uid=$(id -u "$SUPERVISOR_USER")
  actual_uid=$(stat -c '%u' "$OCOMP_EVM_KEY")
  mode=$(stat -c '%a' "$OCOMP_EVM_KEY")
  link_count=$(stat -c '%h' "$OCOMP_EVM_KEY")
  [[ "$actual_uid" == "$expected_uid" ]] ||
    die "$OCOMP_EVM_KEY belongs to uid $actual_uid, expected $expected_uid"
  [[ "$mode" == 600 ]] ||
    die "$OCOMP_EVM_KEY has mode $mode, expected 600"
  [[ "$link_count" == 1 ]] ||
    die "$OCOMP_EVM_KEY has $link_count hard links, expected exactly one"
  [[ $(wc -c <"$OCOMP_EVM_KEY") -eq 65 &&
    $(tr -d '\n' <"$OCOMP_EVM_KEY") =~ ^[0-9a-f]{64}$ ]] ||
    die "$OCOMP_EVM_KEY is not canonical lowercase 32-byte hex plus LF"

  local signer_address
  if ! signer_address=$(
    runuser --user "$SUPERVISOR_USER" -- \
      env \
        OUTBE_OCOMP_BASE_PATH="${RUNTIME_ENV[OUTBE_OCOMP_BASE_PATH]}" \
        "$OCOMP_BINARY" signer-address
  ); then
    if ((created == 1)); then
      rm -f -- "$OCOMP_EVM_KEY"
    fi
    die "OCOMP EVM key is not a valid secp256k1 signer"
  fi
  echo "ocomp: OCOMP delegate signer address $signer_address"
}

prepare() {
  require_root
  require_command getent
  require_command groupadd
  require_command useradd
  require_command usermod
  require_command runuser
  require_command setsid
  require_command setpriv
  require_command prlimit
  require_command flock
  require_command timeout
  require_command curl
  require_command openssl
  [[ -x "$OCOMP_BINARY" ]] ||
    die "OCOMP binary is missing or not executable: $OCOMP_BINARY"
  prepare_identities
  prepare_directories
  prepare_ocomp_evm_key
}

declare -A RUNTIME_ENV=()

load_env_file() {
  local path=$1
  local allowed_names=$2
  [[ -r "$path" ]] || die "environment file is missing or unreadable: $path"

  local line name value
  while IFS= read -r line || [[ -n "$line" ]]; do
    line=${line%$'\r'}
    [[ "$line" =~ ^[[:space:]]*$ ]] && continue
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]] ||
      die "unsupported line in $path: $line"
    name=${BASH_REMATCH[1]}
    value=${BASH_REMATCH[2]}
    [[ ",$allowed_names," == *",$name,"* ]] ||
      die "variable $name is not allowed in $path"
    [[ ! -v "RUNTIME_ENV[$name]" ]] ||
      die "duplicate variable $name in OCOMP environment files"
    if [[ ${#value} -ge 2 ]] &&
      { [[ "$value" == \"*\" ]] || [[ "$value" == \'*\' ]]; }; then
      value=${value:1:${#value}-2}
    fi
    RUNTIME_ENV["$name"]=$value
  done <"$path"
}

require_env() {
  local name=$1
  [[ -n ${RUNTIME_ENV[$name]:-} ]] ||
    die "required environment variable is missing: $name"
}

load_runtime_environment() {
  RUNTIME_ENV=()
  load_env_file \
    "$OCOMP_ENV_FILE" \
    "OCOMP_CHAIN_ID,OCOMP_GENESIS_HASH,OCOMP_BOOT_NONCE,OCOMP_PROTOCOL_BUNDLE_HASH,OCOMP_REGISTRY_GENERATION,OCOMP_VALIDATOR_INDEX,OCOMP_SUPERVISOR_ADDRESS,OCOMP_WORKER_COUNT,OUTBE_OCOMP_BASE_PATH,OUTBE_OCOMP_RPC_URL"
  load_env_file \
    "$OCOMP_EXPORT_ENV_FILE" \
    "OUTBE_OCOMP_PROJECTION_MONGODB_URI,OUTBE_OCOMP_PROJECTION_MONGODB_DATABASE"

  local name
  for name in \
    OCOMP_CHAIN_ID \
    OCOMP_GENESIS_HASH \
    OCOMP_BOOT_NONCE \
    OCOMP_PROTOCOL_BUNDLE_HASH \
    OCOMP_REGISTRY_GENERATION \
    OCOMP_VALIDATOR_INDEX \
    OCOMP_SUPERVISOR_ADDRESS \
    OCOMP_WORKER_COUNT \
    OUTBE_OCOMP_BASE_PATH \
    OUTBE_OCOMP_RPC_URL \
    OUTBE_OCOMP_PROJECTION_MONGODB_URI \
    OUTBE_OCOMP_PROJECTION_MONGODB_DATABASE; do
    require_env "$name"
  done

  [[ "${RUNTIME_ENV[OUTBE_OCOMP_BASE_PATH]}" == "$DEPLOY_ROOT" ]] ||
    die "OUTBE_OCOMP_BASE_PATH must match launcher deployment root $DEPLOY_ROOT"
  [[ "${RUNTIME_ENV[OCOMP_REGISTRY_GENERATION]}" != 0 ]] ||
    die "OCOMP_REGISTRY_GENERATION must be greater than zero"
  [[ "${RUNTIME_ENV[OCOMP_VALIDATOR_INDEX]}" =~ ^[0-3]$ ]] ||
    die "OCOMP_VALIDATOR_INDEX must be between 0 and 3"
  [[ "${RUNTIME_ENV[OCOMP_SUPERVISOR_ADDRESS]}" =~ ^127\.0\.0\.1:([1-9][0-9]*)$ ]] ||
    die "OCOMP_SUPERVISOR_ADDRESS must be an explicit nonzero 127.0.0.1 HTTP address"
  [[ "${RUNTIME_ENV[OCOMP_WORKER_COUNT]}" =~ ^[1-4]$ ]] ||
    die "OCOMP_WORKER_COUNT must be between 1 and 4"
}

proc_start_ticks() {
  local pid=$1 stat_line stat_rest
  local -a stat_fields
  [[ -r "/proc/$pid/stat" ]] || return 1
  IFS= read -r stat_line <"/proc/$pid/stat"
  stat_rest=${stat_line##*) }
  read -r -a stat_fields <<<"$stat_rest"
  [[ ${#stat_fields[@]} -gt 19 ]] || return 1
  printf '%s\n' "${stat_fields[19]}"
}

pid_is_running() {
  local pid_file=$1
  [[ -r "$pid_file" ]] || return 1
  local pid recorded_ticks actual_ticks
  read -r pid recorded_ticks <"$pid_file"
  [[ "$pid" =~ ^[0-9]+$ && "$recorded_ticks" =~ ^[0-9]+$ ]] || return 1
  kill -0 "$pid" 2>/dev/null || return 1
  actual_ticks=$(proc_start_ticks "$pid") || return 1
  [[ "$actual_ticks" == "$recorded_ticks" ]]
}

START_ROLE_LAUNCHED=0
declare -a STARTED_WORKER_ORDINALS_THIS_RUN=()
STARTED_EXPORTER_THIS_RUN=0
STARTED_SUPERVISOR_THIS_RUN=0

terminate_process_group() {
  local pid=$1
  kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
}

start_role() {
  local name=$1
  local user=$2
  local group=$3
  local pid_file=$4
  local log_file=$5
  local expected_command=$6
  shift 6

  if pid_is_running "$pid_file"; then
    echo "ocomp: $name already running (pid $(cut -d ' ' -f 1 "$pid_file"))"
    START_ROLE_LAUNCHED=0
    return
  fi
  rm -f "$pid_file"
  [[ -f "$log_file" && ! -L "$log_file" ]] ||
    die "unsafe log path: $log_file"

  (
    umask 0027
    exec setsid \
      setpriv \
        --reuid="$user" \
        --regid="$group" \
        --init-groups \
        --no-new-privs \
      env -i \
        PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        HOME=/nonexistent \
        "$@"
  ) 9>&- >>"$log_file" 2>&1 &
  local pid=$!
  local start_ticks
  start_ticks=$(proc_start_ticks "$pid") || {
    terminate_process_group "$pid"
    die "$name exited before its process identity could be recorded"
  }
  printf '%s %s\n' "$pid" "$start_ticks" >"$pid_file"
  chmod 0600 "$pid_file"

  local attempt
  for attempt in {1..50}; do
    if pid_is_running "$pid_file"; then
      local actual_uid command_line
      actual_uid=$(stat -c '%u' "/proc/$pid")
      command_line=$(tr '\0' ' ' <"/proc/$pid/cmdline")
      if [[ "$actual_uid" == "$(id -u "$user")" &&
        "$command_line" == *"$expected_command"* ]]; then
        echo "ocomp: started $name (pid $pid)"
        START_ROLE_LAUNCHED=1
        return
      fi
    fi
    sleep 0.1
  done
  terminate_process_group "$pid"
  rm -f "$pid_file"
  die "$name failed to start; inspect $log_file"
}

verify_prerequisites() {
  [[ -x "$OCOMP_BINARY" ]] || die "OCOMP binary is missing or not executable: $OCOMP_BINARY"
  [[ -r "$PROTOCOL_BUNDLE" ]] || die "protocol bundle is missing: $PROTOCOL_BUNDLE"
  [[ -r "$RESULT_COMMITTEE" ]] || die "result committee is missing: $RESULT_COMMITTEE"
  local role_user
  for role_user in "$SUPERVISOR_USER" "$EXPORTER_USER" "$WORKER_USER"; do
    runuser --user "$role_user" -- test -r "$PROTOCOL_BUNDLE" ||
      die "$role_user cannot read protocol bundle $PROTOCOL_BUNDLE"
  done
  runuser --user "$SUPERVISOR_USER" -- test -r "$RESULT_COMMITTEE" ||
    die "$SUPERVISOR_USER cannot read result committee $RESULT_COMMITTEE"
  runuser --user "$SUPERVISOR_USER" -- test -r "$OCOMP_EVM_KEY" ||
    die "$SUPERVISOR_USER cannot read OCOMP EVM key $OCOMP_EVM_KEY"
  runuser --user "$SUPERVISOR_USER" -- test -r "$OCOMP_RESULT_KEY" ||
    die "$SUPERVISOR_USER cannot read pre-provisioned OCOMP result key $OCOMP_RESULT_KEY"
}

start_worker() {
  local ordinal=$1
  start_role \
    "worker $ordinal" \
    "$WORKER_USER" \
    "$ARTIFACT_GROUP" \
    "$(worker_pid_file "$ordinal")" \
    "$(worker_log_file "$ordinal")" \
    "$OCOMP_BINARY worker" \
    env \
      OUTBE_OCOMP_BASE_PATH="${RUNTIME_ENV[OUTBE_OCOMP_BASE_PATH]}" \
      prlimit --nproc=16:16 -- \
      "$OCOMP_BINARY" worker \
        --chain-id "${RUNTIME_ENV[OCOMP_CHAIN_ID]}" \
        --genesis-hash "${RUNTIME_ENV[OCOMP_GENESIS_HASH]}" \
        --boot-nonce "${RUNTIME_ENV[OCOMP_BOOT_NONCE]}" \
        --worker-ordinal "$ordinal" \
        --protocol-bundle-hash "${RUNTIME_ENV[OCOMP_PROTOCOL_BUNDLE_HASH]}" \
        --supervisor-address "${RUNTIME_ENV[OCOMP_SUPERVISOR_ADDRESS]}"
  if ((START_ROLE_LAUNCHED == 1)); then
    STARTED_WORKER_ORDINALS_THIS_RUN+=("$ordinal")
  fi
}

start_supervisor() {
  start_role \
    "supervisor" \
    "$SUPERVISOR_USER" \
    "$ARTIFACT_GROUP" \
    "$SUPERVISOR_PID" \
    "$SUPERVISOR_LOG" \
    "$OCOMP_BINARY supervisor" \
    env \
      OCOMP_CHAIN_ID="${RUNTIME_ENV[OCOMP_CHAIN_ID]}" \
      OCOMP_GENESIS_HASH="${RUNTIME_ENV[OCOMP_GENESIS_HASH]}" \
      OCOMP_BOOT_NONCE="${RUNTIME_ENV[OCOMP_BOOT_NONCE]}" \
      OCOMP_PROTOCOL_BUNDLE_HASH="${RUNTIME_ENV[OCOMP_PROTOCOL_BUNDLE_HASH]}" \
      OCOMP_REGISTRY_GENERATION="${RUNTIME_ENV[OCOMP_REGISTRY_GENERATION]}" \
      OCOMP_VALIDATOR_INDEX="${RUNTIME_ENV[OCOMP_VALIDATOR_INDEX]}" \
      OUTBE_OCOMP_BASE_PATH="${RUNTIME_ENV[OUTBE_OCOMP_BASE_PATH]}" \
      OUTBE_OCOMP_RPC_URL="${RUNTIME_ENV[OUTBE_OCOMP_RPC_URL]}" \
      "$OCOMP_BINARY" supervisor \
        --supervisor-address "${RUNTIME_ENV[OCOMP_SUPERVISOR_ADDRESS]}"
  STARTED_SUPERVISOR_THIS_RUN=$START_ROLE_LAUNCHED
}

start_exporter() {
  start_role \
    "snapshot exporter" \
    "$EXPORTER_USER" \
    "$ARTIFACT_GROUP" \
    "$EXPORTER_PID" \
    "$EXPORTER_LOG" \
    "$OCOMP_BINARY snapshot-exporter" \
    env \
      OCOMP_CHAIN_ID="${RUNTIME_ENV[OCOMP_CHAIN_ID]}" \
      OCOMP_GENESIS_HASH="${RUNTIME_ENV[OCOMP_GENESIS_HASH]}" \
      OCOMP_BOOT_NONCE="${RUNTIME_ENV[OCOMP_BOOT_NONCE]}" \
      OCOMP_PROTOCOL_BUNDLE_HASH="${RUNTIME_ENV[OCOMP_PROTOCOL_BUNDLE_HASH]}" \
      OCOMP_REGISTRY_GENERATION="${RUNTIME_ENV[OCOMP_REGISTRY_GENERATION]}" \
      OUTBE_OCOMP_BASE_PATH="${RUNTIME_ENV[OUTBE_OCOMP_BASE_PATH]}" \
      OUTBE_OCOMP_RPC_URL="${RUNTIME_ENV[OUTBE_OCOMP_RPC_URL]}" \
      OUTBE_OCOMP_PROJECTION_MONGODB_URI="${RUNTIME_ENV[OUTBE_OCOMP_PROJECTION_MONGODB_URI]}" \
      OUTBE_OCOMP_PROJECTION_MONGODB_DATABASE="${RUNTIME_ENV[OUTBE_OCOMP_PROJECTION_MONGODB_DATABASE]}" \
      "$OCOMP_BINARY" snapshot-exporter
  STARTED_EXPORTER_THIS_RUN=$START_ROLE_LAUNCHED
}

start() {
  load_runtime_environment
  prepare
  acquire_lifecycle_lock
  verify_prerequisites

  STARTED_WORKER_ORDINALS_THIS_RUN=()
  STARTED_EXPORTER_THIS_RUN=0
  STARTED_SUPERVISOR_THIS_RUN=0
  rollback_start() {
    local exit_code=$?
    trap - EXIT
    if [[ $exit_code -ne 0 ]]; then
      set +e
      ((STARTED_SUPERVISOR_THIS_RUN == 0)) ||
        stop_pid "supervisor" "$SUPERVISOR_USER" "$OCOMP_BINARY supervisor" "$SUPERVISOR_PID"
      ((STARTED_EXPORTER_THIS_RUN == 0)) ||
        stop_pid "snapshot exporter" "$EXPORTER_USER" "$OCOMP_BINARY snapshot-exporter" "$EXPORTER_PID"
      local ordinal
      for ordinal in "${STARTED_WORKER_ORDINALS_THIS_RUN[@]}"; do
        stop_pid \
          "worker $ordinal" \
          "$WORKER_USER" \
          "$OCOMP_BINARY worker" \
          "$(worker_pid_file "$ordinal")"
      done
    fi
    exit "$exit_code"
  }
  trap rollback_start EXIT

  start_supervisor
  start_exporter
  local ordinal=0
  local worker_count=${RUNTIME_ENV[OCOMP_WORKER_COUNT]}
  while ((ordinal < worker_count)); do
    start_worker "$ordinal"
    ((ordinal += 1))
  done
  sleep 1
  status
  trap - EXIT
}

acquire_lifecycle_lock() {
  exec 9>"$LIFECYCLE_LOCK"
  flock -n 9 || die "another ocomp.sh lifecycle command is running"
}

stop_pid() {
  local name=$1
  local expected_user=$2
  local expected_command=$3
  local pid_file=$4
  if ! pid_is_running "$pid_file"; then
    rm -f "$pid_file"
    echo "ocomp: $name is not running"
    return
  fi

  local pid recorded_ticks
  read -r pid recorded_ticks <"$pid_file"
  local expected_uid actual_uid command_line
  expected_uid=$(id -u "$expected_user")
  actual_uid=$(stat -c '%u' "/proc/$pid")
  command_line=$(tr '\0' ' ' <"/proc/$pid/cmdline")
  [[ "$actual_uid" == "$expected_uid" ]] ||
    die "refusing to stop $name: pid $pid belongs to uid $actual_uid, expected $expected_uid"
  [[ "$command_line" == *"$expected_command"* ]] ||
    die "refusing to stop $name: pid $pid command does not contain $expected_command"
  terminate_process_group "$pid"
  local attempt
  for attempt in {1..50}; do
    if ! kill -0 "$pid" 2>/dev/null; then
      rm -f "$pid_file"
      echo "ocomp: stopped $name"
      return
    fi
    sleep 0.1
  done
  die "$name did not stop after SIGTERM (pid $pid)"
}

stop() {
  require_root
  require_command flock
  acquire_lifecycle_lock
  stop_pid "supervisor" "$SUPERVISOR_USER" "$OCOMP_BINARY supervisor" "$SUPERVISOR_PID"
  stop_pid "snapshot exporter" "$EXPORTER_USER" "$OCOMP_BINARY snapshot-exporter" "$EXPORTER_PID"
  local ordinal
  for ordinal in 0 1 2 3; do
    stop_pid \
      "worker $ordinal" \
      "$WORKER_USER" \
      "$OCOMP_BINARY worker" \
      "$(worker_pid_file "$ordinal")"
  done
}

role_status() {
  local name=$1
  local pid_file=$2
  if pid_is_running "$pid_file"; then
    echo "active   $name (pid $(cut -d ' ' -f 1 "$pid_file"))"
    return 0
  fi
  echo "inactive $name"
  return 1
}

status() {
  if [[ ${#RUNTIME_ENV[@]} -eq 0 ]]; then
    load_runtime_environment
  fi
  local failed=0
  local ordinal=0
  local worker_count=${RUNTIME_ENV[OCOMP_WORKER_COUNT]}
  while ((ordinal < worker_count)); do
    role_status "worker $ordinal" "$(worker_pid_file "$ordinal")" || failed=1
    ((ordinal += 1))
  done
  role_status "snapshot exporter" "$EXPORTER_PID" || failed=1
  role_status "supervisor" "$SUPERVISOR_PID" || failed=1
  local status_json
  if status_json=$(curl -fsS --max-time 2 "http://${RUNTIME_ENV[OCOMP_SUPERVISOR_ADDRESS]}/v1/status") &&
    [[ "$status_json" =~ \"connected_workers\":([0-9]+) ]]; then
    local connected=${BASH_REMATCH[1]}
    echo "connected $connected/${RUNTIME_ENV[OCOMP_WORKER_COUNT]} workers via TCP ZeroMQ"
    [[ "$connected" == "${RUNTIME_ENV[OCOMP_WORKER_COUNT]}" ]] || failed=1
  else
    echo "unavailable Supervisor registration status"
    failed=1
  fi
  return "$failed"
}

logs() {
  require_root
  tail -n 100 -F \
    "$SUPERVISOR_LOG" \
    "$EXPORTER_LOG" \
    "$(worker_log_file 0)" \
    "$(worker_log_file 1)" \
    "$(worker_log_file 2)" \
    "$(worker_log_file 3)"
}

usage() {
  cat <<EOF
Usage: sudo $0 <command>

Commands:
  install   Create the fixed OCOMP users, groups and data directories.
  start     Start worker, snapshot exporter and supervisor.
  stop      Stop all standalone OCOMP processes.
  restart   Stop and start all standalone OCOMP processes.
  status    Show standalone OCOMP process status.
  logs      Follow supervisor, exporter and worker logs.

This launcher does not start outbe-chain. Start a node with JSON-RPC enabled and
configure its URL in $OCOMP_ENV_FILE. Configure the exporter projection in:
  $OCOMP_EXPORT_ENV_FILE
EOF
}

case "${1:-}" in
  install)
    load_runtime_environment
    prepare
    ;;
  start)
    start
    ;;
  stop)
    stop
    ;;
  restart)
    stop
    start
    ;;
  status)
    status
    ;;
  logs)
    logs
    ;;
  help | --help | -h)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
