#!/bin/bash
# First-boot provisioning for an Outbe validator VM (MVP mechanism).
#
# The VM image is identical for every validator. The per-validator bundle
# (genesis, keys, launch scripts) is fetched once from the URL in the dstack
# user config and unpacked onto the persistent data disk.
set -euo pipefail

CONFIG=/dstack/user_config
BASE=/dstack/persistent/outbe
RUN=/run/outbe

index=$(jq -er '.validator_index' "$CONFIG")
url=$(jq -er '.bundle_url' "$CONFIG")

mkdir -p "$RUN"
# The enclave state directory is on tmpfs. It is empty after every VM restart.
install -d -m 700 "$RUN/tee"

if [ ! -f "$BASE/.provisioned" ]; then
    mkdir -p "$BASE"
    archive=$(mktemp)
    until curl -fsS --max-time 60 -o "$archive" "$url"; do
        echo "outbe-provision: bundle not available at $url, retrying" >&2
        sleep 5
    done
    tar -xzf "$archive" -C "$BASE"
    rm -f "$archive"
    touch "$BASE/.provisioned"
fi

printf 'OUTBE_BASE=%s\nOUTBE_VALIDATOR_INDEX=%s\n' "$BASE" "$index" > "$RUN/env"
