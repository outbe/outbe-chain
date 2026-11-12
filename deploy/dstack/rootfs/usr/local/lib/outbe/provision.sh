#!/bin/bash
# First-boot provisioning for an Outbe validator VM.
#
# The VM image is identical for every validator and holds every program and
# launch script. The data disk and the dstack user config are operator input:
# this script reads them as data only. It never executes a file from the data
# disk and never evaluates text from the user config.
#
# User config (JSON), all values are checked before use:
#   validator_index  number of this validator, 0 to 255
#   bundle_url       where to fetch the data bundle (keys, genesis, bootnodes)
#   tee_mode         "dev" or "confidential-vm"
#   crs_url          optional: where to fetch the Barretenberg CRS
#   external_address optional: IPv4 address that the validator advertises
#   upstream_url     optional: RPC of a healthy node. After a VM restart a
#                    validator in the confidential-vm mode joins the network
#                    again through this node and catches up from it
#   port_offset      optional: 0 to 99, added to every peer-to-peer port. Two
#                    validators behind one public address need different
#                    offsets
set -euo pipefail

CONFIG=/dstack/user_config
BASE=/dstack/persistent/outbe
RUN=/run/outbe

fail() { echo "outbe-provision: $*" >&2; exit 1; }
field() { jq -er --arg key "$1" '.[$key] // empty | tostring' "$CONFIG" 2>/dev/null || true; }

index=$(field validator_index)
[[ $index =~ ^[0-9]{1,3}$ ]] && ((10#$index <= 255)) || fail "validator_index must be 0 to 255"
index=$((10#$index))
url=$(field bundle_url)
[[ $url =~ ^https?://[A-Za-z0-9._:/-]+$ ]] || fail "bundle_url is not a plain http(s) URL"
tee_mode=$(field tee_mode)
[[ $tee_mode == dev || $tee_mode == confidential-vm ]] || fail "tee_mode must be dev or confidential-vm"
crs_url=$(field crs_url)
[[ -z $crs_url || $crs_url =~ ^https?://[A-Za-z0-9._:/-]+$ ]] || fail "crs_url is not a plain http(s) URL"
address=$(field external_address)
if [[ -z $address ]]; then
    address=$(ip -4 -o addr show scope global | awk '{print $4}' | cut -d/ -f1 | head -n 1)
fi
[[ $address =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]] || fail "no IPv4 address to advertise"

upstream=$(field upstream_url)
[[ -z $upstream || $upstream =~ ^https?://[A-Za-z0-9._:/-]+$ ]] || fail "upstream_url is not a plain http(s) URL"
offset=$(field port_offset)
offset=${offset:-0}
[[ $offset =~ ^[0-9]{1,2}$ ]] || fail "port_offset must be 0 to 99"
offset=$((10#$offset))

mkdir -p "$RUN"

if [ ! -f "$BASE/.provisioned" ]; then
    mkdir -p "$BASE"
    archive=$(mktemp)
    until curl -fsS --max-time 60 -o "$archive" "$url"; do
        echo "outbe-provision: bundle not available at $url, retrying" >&2
        sleep 5
    done
    # The bundle is data: regular files and directories with relative names.
    # Links, devices and names that leave the target directory are refused.
    listing=$(tar -tvzf "$archive")
    if grep -qv '^[-d]' <<<"$listing"; then
        fail "the bundle contains an entry that is not a file or a directory"
    fi
    names=$(tar -tzf "$archive")
    if grep -qE '(^/|(^|/)\.\.(/|$))' <<<"$names"; then
        fail "the bundle contains an absolute name or a parent directory"
    fi
    tar -xzf "$archive" -C "$BASE" --no-same-owner --no-same-permissions
    rm -f "$archive"
    # Write the files to the disk before the marker: a power loss must not
    # leave the marker next to incomplete files.
    sync
    touch "$BASE/.provisioned"
    sync
fi

crs=$BASE/crs/bn254_g1.dat
if [ ! -s "$crs" ] && [ -n "$crs_url" ]; then
    mkdir -p "$(dirname "$crs")"
    curl -fsS --max-time 300 -o "$crs.tmp" "$crs_url"
    mv "$crs.tmp" "$crs"
fi

# Print the identity of this VM for the operator: the values that an
# attestation policy pins. The image has no shell access, so this is how they
# are read (dstack shows the console log).
if [ "$tee_mode" = confidential-vm ]; then
    outbe-tee-enclave --probe-attestation 2>&1 |
        grep -E 'tsm provider|identity:|certificate table' |
        sed 's/^/outbe-identity: /' >&2 || true
fi

# Checked values for the launch script. This file is on tmpfs and is written
# by this script only.
cat > "$RUN/env" <<ENV
OUTBE_BASE=$BASE
OUTBE_VALIDATOR_INDEX=$index
OUTBE_TEE_MODE=$tee_mode
OUTBE_EXTERNAL_ADDRESS=$address
OUTBE_PORT_OFFSET=$offset
OUTBE_UPSTREAM_URL=$upstream
ENV
