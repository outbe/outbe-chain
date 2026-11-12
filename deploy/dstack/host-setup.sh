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
#   expose       publish the validator VMs on the public address of the host:
#                expose <public-ip> <peer-ip,...> <vm-ip>:<port-offset> ...
#                RPC and Radicle are open to everyone, the consensus and reth
#                ports only to the listed peer addresses
#   unexpose     remove what expose created
#   firewall-try load the inbound firewall for two minutes, to check SSH access
#   firewall     install the inbound firewall: SSH and the VM bridge only; it
#                stays after a reboot. The ports that expose publishes are
#                forwarded to the VMs and stay reachable
#   firewall-off remove the inbound firewall
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

# Publish validator VMs on the public address of the host.
#
# Port plan of a VM with offset N: RPC 8545 in the VM, public port 8545+N;
# Radicle 8776+N; consensus 30400+N; reth 30303+N (TCP and UDP); discv5
# 31303+N (UDP). The VM listens on the offset ports itself, so only the RPC
# port is translated.
expose() {
    local public=${1:?public address required} peers=${2:?peer addresses required}
    shift 2
    [ "$#" -gt 0 ] || { echo "at least one <vm-ip>:<offset> required" >&2; exit 1; }
    local uplink
    uplink=$(ip -4 -o route get 1.1.1.1 | awk '{for (i = 1; i < NF; i++) if ($i == "dev") print $(i + 1)}')
    sudo sysctl -q -w net.ipv4.ip_forward=1
    unexpose >/dev/null 2>&1 || true
    {
        echo "table ip outbe_mvp {"
        echo "  set peers { type ipv4_addr; flags interval; elements = { $peers, $SUBNET.0/24 } }"
        echo "  chain prerouting { type nat hook prerouting priority dstnat; policy accept;"
        local entry vm offset
        for entry in "$@"; do
            vm=${entry%%:*}; offset=${entry##*:}
            echo "    ip daddr $public tcp dport $((8545 + offset)) dnat to $vm:8545"
            echo "    ip daddr $public tcp dport $((8776 + offset)) dnat to $vm"
            echo "    ip daddr $public ip saddr @peers tcp dport { $((30400 + offset)), $((30303 + offset)) } dnat to $vm"
            echo "    ip daddr $public ip saddr @peers udp dport { $((30303 + offset)), $((31303 + offset)) } dnat to $vm"
        done
        echo "  }"
        echo "  chain postrouting { type nat hook postrouting priority srcnat; policy accept;"
        # A VM reaches another VM of this host through the public address, and
        # the peer checks the source address: use the public address.
        echo "    ip saddr $SUBNET.0/24 ip daddr $SUBNET.0/24 ct status dnat snat to $public"
        echo "    ip saddr $SUBNET.0/24 oifname \"$uplink\" snat to $public"
        echo "  }"
        echo "  chain forward { type filter hook forward priority filter; policy accept;"
        echo "    ct state established,related accept"
        echo "    iifname \"$BRIDGE\" accept"
        echo "    oifname \"$BRIDGE\" ct status dnat accept"
        echo "    oifname \"$BRIDGE\" drop"
        echo "  }"
        echo "}"
    } | sudo nft -f -
    echo "exposed on $public (uplink $uplink): $*"
}

# Basic inbound firewall of the host: SSH, and the VM bridge. Everything else
# that arrives for the host itself is dropped. The ports of the validators are
# not host ports: `expose` forwards them to the VMs, and forwarded packets do
# not pass the input chain.
#
# The rules are in their own table, so they do not replace rules of other
# tools. A systemd unit loads them again after a reboot.
FIREWALL_RULES=/etc/outbe-host-firewall.nft
FIREWALL_UNIT=/etc/systemd/system/outbe-host-firewall.service
firewall_rules() {
    cat <<NFT
table inet outbe_host_fw {
  chain input { type filter hook input priority filter; policy drop;
    ct state established,related accept
    iifname "lo" accept
    ct state invalid drop
    meta l4proto { icmp, ipv6-icmp } accept
    udp dport { 68, 546 } accept
    tcp dport 22 accept
    iifname "$BRIDGE" accept
  }
}
NFT
}
# firewall-try loads the rules for two minutes only. Use it to check that a
# new SSH session still opens, then run `firewall` to keep the rules.
firewall_try() {
    sudo nft delete table inet outbe_host_fw 2>/dev/null || true
    sudo systemctl stop outbe-host-firewall-rollback.timer 2>/dev/null || true
    sudo systemctl reset-failed outbe-host-firewall-rollback.service 2>/dev/null || true
    sudo systemd-run --quiet --on-active=120 --unit outbe-host-firewall-rollback \
        nft delete table inet outbe_host_fw
    firewall_rules | sudo nft -f -
    echo "firewall loaded for 120 seconds"
}
firewall() {
    sudo systemctl stop outbe-host-firewall-rollback.timer 2>/dev/null || true
    firewall_rules | sudo tee "$FIREWALL_RULES" >/dev/null
    sudo tee "$FIREWALL_UNIT" >/dev/null <<UNIT
[Unit]
Description=Outbe MVP host firewall
Before=network-pre.target
Wants=network-pre.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStartPre=-/usr/sbin/nft delete table inet outbe_host_fw
ExecStart=/usr/sbin/nft -f $FIREWALL_RULES
ExecStop=/usr/sbin/nft delete table inet outbe_host_fw

[Install]
WantedBy=multi-user.target
UNIT
    sudo systemctl daemon-reload
    sudo systemctl enable --quiet outbe-host-firewall.service
    sudo systemctl restart outbe-host-firewall.service
    echo "firewall installed"
}
firewall_off() {
    sudo systemctl disable --quiet --now outbe-host-firewall.service 2>/dev/null || true
    sudo nft delete table inet outbe_host_fw 2>/dev/null || true
    sudo rm -f "$FIREWALL_RULES" "$FIREWALL_UNIT"
    sudo systemctl daemon-reload
    echo "firewall removed"
}

unexpose() {
    sudo nft delete table ip outbe_mvp 2>/dev/null || true
    echo "exposure removed"
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
    expose) shift; expose "$@" ;;
    unexpose) unexpose ;;
    firewall-try) firewall_try ;;
    firewall) firewall ;;
    firewall-off) firewall_off ;;
    status) status ;;
    *) sed -n '2,25p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
