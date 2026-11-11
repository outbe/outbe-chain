#!/bin/bash
# Prepare a host to run Outbe validator VMs under dstack.
#
# Usage: host-setup.sh <command>
#
#   packages     install the host packages and load the vsock module
#   tools        clone dstack at the pinned revision, build and install the VMM tools
#   network-up   create the VM bridge, the firewall rule, DHCP and the VMM network helper
#   network-down remove what network-up created
#   vmm-start    start the VMM in the background
#   status       print the state of every part
#
# Environment:
#   WORK      working directory (default: $HOME/outbe-tdx-mvp)
#   BRIDGE    bridge name (default: obr0)
#   SUBNET    first three octets of the VM subnet (default: 10.77.0)
#   JOBS      build parallelism for `tools` (default: 4)
#
# network-up does not survive a host reboot. Run it again after a reboot.
set -euo pipefail

DSTACK_COMMIT=c88f60e781a62d542e0843b333f18a48d1458b88
WORK=${WORK:-$HOME/outbe-tdx-mvp}
BRIDGE=${BRIDGE:-obr0}
SUBNET=${SUBNET:-10.77.0}
JOBS=${JOBS:-4}
VMM_HOME=$HOME/.dstack-vmm
DHCP_PID=/run/outbe-mvp-dnsmasq.pid
DHCP_HOSTS=/var/lib/misc/dnsmasq.outbe-mvp.hosts
DHCP_LEASES=/var/lib/misc/dnsmasq.outbe-mvp.leases

packages() {
    sudo apt-get update
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        qemu-system-x86 qemu-utils swtpm swtpm-tools libvirt-clients dnsmasq-base jq git
    sudo usermod -aG kvm "$USER"
    sudo modprobe vhost_vsock
    echo "packages installed. Log in again, or run KVM commands through: sg kvm -c '...'"
}

tools() {
    mkdir -p "$WORK" "$HOME/.local/bin"
    [ -d "$WORK/dstack/.git" ] ||
        git clone --branch next https://github.com/Dstack-TEE/dstack.git "$WORK/dstack"
    git -C "$WORK/dstack" checkout --quiet "$DSTACK_COMMIT"
    (cd "$WORK/dstack" && nice -n 19 cargo build --manifest-path dstack/Cargo.toml --release \
        -j "$JOBS" -p dstack-vmm -p supervisor -p mock-attestation -p dstack-verifier)
    local release=$WORK/dstack/dstack/target/release
    install -Dm755 "$release/dstack-vmm" "$HOME/.local/bin/dstack-vmm"
    install -Dm755 "$release/supervisor" "$HOME/.local/bin/dstack-supervisor"
    install -Dm755 "$release/dstack-mock-attestation" "$HOME/.local/bin/dstack-mock-attestation"
    install -Dm755 "$release/dstack-verifier" "$HOME/.local/bin/dstack-verifier"
    install -Dm755 "$WORK/dstack/dstack/vmm/src/vmm-cli.py" "$HOME/.local/bin/dstack"
    mkdir -p "$VMM_HOME/image" "$VMM_HOME/run"
    if [ ! -f "$VMM_HOME/vmm.toml" ]; then
        cp "$WORK/dstack/dstack/vmm/vmm.toml" "$VMM_HOME/vmm.toml"
        echo "created $VMM_HOME/vmm.toml from the dstack template. Edit it as README.md describes."
    fi
}

network_up() {
    if ! ip link show "$BRIDGE" >/dev/null 2>&1; then
        sudo ip link add "$BRIDGE" type bridge
        sudo ip addr add "$SUBNET.1/24" dev "$BRIDGE"
    fi
    sudo ip link set "$BRIDGE" up
    # Read the chain into a variable first: `grep -q` in a pipeline closes the
    # pipe early and fails the pipeline under `pipefail`.
    local input_chain
    if input_chain=$(sudo nft list chain ip filter INPUT 2>/dev/null) &&
        [[ $input_chain != *"iifname \"$BRIDGE\""* ]]; then
        sudo nft insert rule ip filter INPUT iifname "$BRIDGE" counter accept
    fi
    sudo touch "$DHCP_HOSTS"
    if ! { [ -f "$DHCP_PID" ] && sudo kill -0 "$(cat "$DHCP_PID")" 2>/dev/null; }; then
        sudo dnsmasq --user=root --conf-file=/dev/null --interface="$BRIDGE" --bind-interfaces \
            --except-interface=lo --port=0 --dhcp-ignore-clid \
            --dhcp-range="$SUBNET.10,$SUBNET.50,255.255.255.0,infinite" \
            --dhcp-hostsfile="$DHCP_HOSTS" \
            --dhcp-option=option:router,"$SUBNET.1" \
            --dhcp-leasefile="$DHCP_LEASES" --pid-file="$DHCP_PID"
    fi
    sudo mkdir -p /run/dstack
    if ! pgrep -f 'dstack-vmm .* netd' >/dev/null; then
        sudo sh -c "nohup setsid sg kvm -c '$HOME/.local/bin/dstack-vmm --config $VMM_HOME/vmm.toml netd' \
            > $WORK/netd.log 2>&1 &"
    fi
    echo "network is up: bridge $BRIDGE, host address $SUBNET.1"
}

network_down() {
    sudo pkill -f 'dstack-vmm .* netd' || true
    [ -f "$DHCP_PID" ] && sudo kill "$(cat "$DHCP_PID")" 2>/dev/null || true
    local handle
    handle=$(sudo nft -a list chain ip filter INPUT 2>/dev/null |
        awk -v bridge="\"$BRIDGE\"" '$0 ~ "iifname " bridge { print $NF }' | head -1)
    [ -n "$handle" ] && sudo nft delete rule ip filter INPUT handle "$handle"
    sudo ip link del "$BRIDGE" 2>/dev/null || true
    echo "network is down"
}

vmm_start() {
    mkdir -p "$VMM_HOME/run"
    if pgrep -xf "$HOME/.local/bin/dstack-vmm -c ../vmm.toml" >/dev/null; then
        echo "VMM is already running"
        return
    fi
    (cd "$VMM_HOME/run" &&
        nohup setsid sg kvm -c "$HOME/.local/bin/dstack-vmm -c ../vmm.toml" > "$WORK/vmm.log" 2>&1 &)
    echo "VMM started; log: $WORK/vmm.log"
}

status() {
    printf 'kvm device:        %s\n' "$([ -e /dev/kvm ] && echo present || echo ABSENT)"
    printf 'vhost_vsock:       %s\n' "$([ -d /sys/module/vhost_vsock ] && echo loaded || echo NOT LOADED)"
    printf 'sev-snp host:      %s\n' "$(cat /sys/module/kvm_amd/parameters/sev_snp 2>/dev/null || echo n/a)"
    printf 'tdx host:          %s\n' "$(cat /sys/module/kvm_intel/parameters/tdx 2>/dev/null || echo n/a)"
    printf 'bridge %s:       %s\n' "$BRIDGE" "$(ip -4 -br addr show dev "$BRIDGE" 2>/dev/null | awk '{print $3}' || true)"
    printf 'dhcp:              %s\n' "$([ -f "$DHCP_PID" ] && sudo kill -0 "$(cat "$DHCP_PID")" 2>/dev/null && echo running || echo stopped)"
    printf 'netd:              %s\n' "$(pgrep -f 'dstack-vmm .* netd' >/dev/null && echo running || echo stopped)"
    printf 'vmm:               %s\n' "$(pgrep -xf "$HOME/.local/bin/dstack-vmm -c ../vmm.toml" >/dev/null && echo running || echo stopped)"
}

case "${1:-}" in
    packages) packages ;;
    tools) tools ;;
    network-up) network_up ;;
    network-down) network_down ;;
    vmm-start) vmm_start ;;
    status) status ;;
    *) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
