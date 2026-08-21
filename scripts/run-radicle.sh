#!/usr/bin/env bash
# Start one operator-owned Outbe Radicle sidecar in the foreground.

set -euo pipefail
umask 077

usage() {
    echo "Usage: $0 <rad-home> <listen> <status-listen> <max-validators> <advertise> [advertise ...]" >&2
    exit 2
}

[ "$#" -ge 5 ] || usage

RADICLE_HOME=$1
RADICLE_LISTEN=$2
RADICLE_STATUS_LISTEN=$3
RADICLE_MAX_VALIDATORS=$4
shift 4

RADICLE_BINARY=${OUTBE_RADICLE_BINARY:-./target/release/outbe-radicle}
RADICLE_CONTROL_SOCKET=${OUTBE_RADICLE_CONTROL_SOCKET:-$RADICLE_HOME/node/outbe-control.sock}
RADICLE_EXTERNAL_INBOUND_RESERVE=${OUTBE_RADICLE_EXTERNAL_INBOUND_RESERVE:-16}

if [ ! -x "$RADICLE_BINARY" ]; then
    echo "Error: release outbe-radicle binary is not executable: $RADICLE_BINARY" >&2
    exit 1
fi
if [ ! -d "$RADICLE_HOME" ] || [ -L "$RADICLE_HOME" ]; then
    echo "Error: Radicle home must be an existing non-symlink directory: $RADICLE_HOME" >&2
    exit 1
fi
if [ ! -d "$RADICLE_HOME/keys" ] || [ -L "$RADICLE_HOME/keys" ]; then
    echo "Error: Radicle keys must be an existing non-symlink directory: $RADICLE_HOME/keys" >&2
    exit 1
fi
if [ ! -f "$RADICLE_HOME/keys/radicle" ] || [ -L "$RADICLE_HOME/keys/radicle" ]; then
    echo "Error: generate the Radicle key first with outbe-keygen radicle --output-dir $RADICLE_HOME" >&2
    exit 1
fi
if [ ! -f "$RADICLE_HOME/keys/radicle.pub" ] || [ -L "$RADICLE_HOME/keys/radicle.pub" ]; then
    echo "Error: missing Heartwood public key: $RADICLE_HOME/keys/radicle.pub" >&2
    exit 1
fi

# Lock the existing home inode before changing anything under it. Locking the
# directory itself creates no file and prevents a losing launcher from touching
# a live sidecar's profile.
exec 9<"$RADICLE_HOME"
if ! flock -n 9; then
    echo "Error: another outbe-radicle instance owns $RADICLE_HOME" >&2
    exit 1
fi
if [ ! "$RADICLE_HOME" -ef "/proc/$$/fd/9" ]; then
    echo "Error: Radicle home changed while acquiring ownership: $RADICLE_HOME" >&2
    exit 1
fi

RADICLE_OWNER_UID=$(id -u)

require_owned_directory() {
    local path=$1
    local mode=$2
    if [ ! -d "$path" ] || [ -L "$path" ]; then
        echo "Error: expected non-symlink directory: $path" >&2
        exit 1
    fi
    if [ "$(stat -c '%u' -- "$path")" != "$RADICLE_OWNER_UID" ] || \
       [ "$(stat -c '%a' -- "$path")" != "$mode" ]; then
        echo "Error: $path must be owned by uid $RADICLE_OWNER_UID with mode $mode" >&2
        exit 1
    fi
}

require_owned_file() {
    local path=$1
    local mode=$2
    if [ ! -f "$path" ] || [ -L "$path" ]; then
        echo "Error: expected non-symlink regular file: $path" >&2
        exit 1
    fi
    if [ "$(stat -c '%u' -- "$path")" != "$RADICLE_OWNER_UID" ] || \
       [ "$(stat -c '%a' -- "$path")" != "$mode" ]; then
        echo "Error: $path must be owned by uid $RADICLE_OWNER_UID with mode $mode" >&2
        exit 1
    fi
}

require_owned_directory "$RADICLE_HOME" 700
require_owned_directory "$RADICLE_HOME/keys" 700
require_owned_file "$RADICLE_HOME/keys/radicle" 600
if [ "$(stat -c '%u' -- "$RADICLE_HOME/keys/radicle.pub")" != "$RADICLE_OWNER_UID" ]; then
    echo "Error: $RADICLE_HOME/keys/radicle.pub must be owned by uid $RADICLE_OWNER_UID" >&2
    exit 1
fi
# The public key is non-secret and older keygen versions inherited a group-write
# bit. Normalize it only after exclusive ownership; the private key remains
# strict and is never rewritten.
chmod 644 "$RADICLE_HOME/keys/radicle.pub"

for directory in storage node cobs; do
    path=$RADICLE_HOME/$directory
    if [ -e "$path" ] || [ -L "$path" ]; then
        require_owned_directory "$path" 700
    else
        mkdir -m 700 -- "$path"
    fi
done

RADICLE_CONFIG=$RADICLE_HOME/config.json
if [ -L "$RADICLE_CONFIG" ]; then
    echo "Error: Radicle config must not be a symlink: $RADICLE_CONFIG" >&2
    exit 1
fi
if [ -e "$RADICLE_CONFIG" ]; then
    require_owned_file "$RADICLE_CONFIG" 600
fi

# Native `rad` and `git-remote-rad` load config.json even though the sidecar
# receives its authoritative runtime config on the command line. Keep both
# views aligned without ever introducing public Radicle seeds.
python3 - "$RADICLE_CONFIG" "$RADICLE_LISTEN" "$RADICLE_MAX_VALIDATORS" \
    "$RADICLE_EXTERNAL_INBOUND_RESERVE" "$@" <<'PY'
import json
import os
import pathlib
import sys
import tempfile

path = pathlib.Path(sys.argv[1])
listen = sys.argv[2]
max_validators = int(sys.argv[3])
external_inbound_reserve = int(sys.argv[4])
advertise = sys.argv[5:]

if path.exists():
    with path.open("r", encoding="utf-8") as source:
        config = json.load(source)
    if not isinstance(config, dict):
        raise SystemExit(f"Radicle config root must be an object: {path}")
else:
    config = {}

node = config.setdefault("node", {})
if not isinstance(node, dict):
    raise SystemExit(f"Radicle node config must be an object: {path}")

managed_peers = max_validators - 1
node.update({
    "alias": "outbe",
    "listen": [listen],
    "peers": {"type": "static"},
    "connect": [],
    "externalAddresses": advertise,
    "network": "outbe",
    "relay": "always",
    "seedingPolicy": {"default": "block"},
})
node.pop("policy", None)
node.pop("scope", None)
limits = node.setdefault("limits", {})
if not isinstance(limits, dict):
    raise SystemExit(f"Radicle limits config must be an object: {path}")
connection = limits.setdefault("connection", {})
if not isinstance(connection, dict):
    raise SystemExit(f"Radicle connection limits must be an object: {path}")
connection.update({
    "outbound": managed_peers,
    "inbound": managed_peers + external_inbound_reserve,
})
config["preferredSeeds"] = []

temporary = None
try:
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=path.parent,
        prefix=".config.json.",
        delete=False,
    ) as output:
        temporary = pathlib.Path(output.name)
        os.chmod(temporary, 0o600)
        json.dump(config, output, indent=2, sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
finally:
    if temporary is not None and temporary.exists():
        temporary.unlink()
PY

if [ -e "$RADICLE_CONTROL_SOCKET" ]; then
    if [ ! -S "$RADICLE_CONTROL_SOCKET" ]; then
        echo "Error: control socket path exists and is not a Unix socket: $RADICLE_CONTROL_SOCKET" >&2
        exit 1
    fi
    if RADICLE_CONTROL_SOCKET="$RADICLE_CONTROL_SOCKET" python3 -c '
import os
import socket

client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.settimeout(1.0)
client.connect(os.environ["RADICLE_CONTROL_SOCKET"])
' >/dev/null 2>&1; then
        echo "Error: a live sidecar already owns $RADICLE_CONTROL_SOCKET" >&2
        exit 1
    fi
    rm -f "$RADICLE_CONTROL_SOCKET"
fi

args=(
    --home "$RADICLE_HOME"
    --control-socket "$RADICLE_CONTROL_SOCKET"
    --listen "$RADICLE_LISTEN"
    --status-listen "$RADICLE_STATUS_LISTEN"
    --max-validators "$RADICLE_MAX_VALIDATORS"
    --external-inbound-reserve "$RADICLE_EXTERNAL_INBOUND_RESERVE"
)
for address in "$@"; do
    args+=(--advertise "$address")
done

exec "$RADICLE_BINARY" "${args[@]}"
