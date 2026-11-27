# outbe-net-up

`outbe-net-up` builds and operates a multi-host Outbe founding network over SSH. In VM mode, it pins the matching `outbe-up` binary on each VMM host and drives every host through `outbe-up ... --json`.

`outbe-net-up` never edits host network files and never opens SSH connections into guest VMs.

## Host prerequisites

Prepare each VMM host once before running `network deploy`:

- Configure a Linux bridge (default `obr0`) with an IPv4 subnet and outbound NAT egress.
- Run `dstack-vmm` with `<vmm_home>/run/vmm.sock` reachable by the SSH user, and install `qemu-img`.
- Run a working Quote Generation Service (QGS) on Intel TDX hosts.
- Reserve static guest IPv4 addresses on the bridge subnet outside the host DHCP pool (for example `10.77.0.11`, `.12`, and `.13` on `10.77.0.0/24`).
- For multi-host networks, add 1:1 DNAT and forward rules in `/etc/host-firewall.nft`. Forward each validator's public `advertise_ip` port `base + port_offset` to the same port on its `vm.vm_ip`:
  - `30303 + port_offset` (TCP and UDP, execution P2P)
  - `30400 + port_offset` (TCP, consensus P2P)
  - `31303 + port_offset` (UDP, discv5)
  - `8776 + port_offset` (TCP, Radicle)

### What `outbe-up host check` checks

`network deploy`, `network add-validator`, and `network add-full-node` run `outbe-up host check --json` on each target host (read-only):

- `host_network_ok`: the bridge is up, IPv4 forwarding is active, and outbound NAT egress exists.
- `service_ip_ok`: the static guest IP is inside the bridge subnet, outside the DHCP pool, and free of conflicts.
- `published_ports_ok`: live `nftables` DNAT and forward rules match all required 1:1 P2P and Radicle ports for every multi-host validator.

If any check fails, `outbe-net-up` leaves the new VM stopped, prints the recommended rules from `host check`, and exits with code `2`. Apply the printed firewall rules on the host, then rerun the command.

## Configure `network.yaml` (2+2 multi-host example)

Generate a starter template with `outbe-net-up network config-example --output network.yaml`.

Below is a four-founder VM network split across two VMM hosts (two founders per host using `port_offset: 0` and `1`):

```yaml
version: 1
name: testnet-vm
network: testnet
chain_id: 54322345
certified_rpc: "http://198.51.100.10:8545"
vm:
  vmm_host:
    host: 198.51.100.10
    user: ubuntu
    port: 22
  vmm_home: "~/.dstack-vmm"
  bridge: obr0
  prefix: 24
  gateway: 10.77.0.1
  dns: [1.1.1.1, 8.8.8.8]
  vcpu: 8
  memory_gib: 16
  disk_gib: 100
  flavor: dev
founders:
  - name: validator-0
    advertise_ip: 198.51.100.10
    hyperlane_public_url: "http://198.51.100.10:9000"
    vm:
      vm_ip: 10.77.0.11
      port_offset: 0
  - name: validator-1
    advertise_ip: 198.51.100.10
    hyperlane_public_url: "http://198.51.100.10:9001"
    vm:
      vm_ip: 10.77.0.12
      port_offset: 1
  - name: validator-2
    advertise_ip: 203.0.113.20
    hyperlane_public_url: "http://203.0.113.20:9000"
    vm:
      vmm_host:
        host: 203.0.113.20
        user: ubuntu
        port: 22
      vm_ip: 10.77.0.11
      port_offset: 0
  - name: validator-3
    advertise_ip: 203.0.113.20
    hyperlane_public_url: "http://203.0.113.20:9001"
    vm:
      vmm_host:
        host: 203.0.113.20
        user: ubuntu
        port: 22
      vm_ip: 10.77.0.12
      port_offset: 1
hyperlane:
  gasless: true
feeder:
  rpc_endpoints:
    1: "https://ethereum-rpc.example.invalid"
    97: "https://bsc-testnet-rpc.example.invalid"
    11155111: "https://sepolia-rpc.example.invalid"
topology:
  use_local_defaults: false
genesis:
  epoch_length_blocks: 1200
  dkg_prepare_window_blocks: 300
  dkg_activation_grace_blocks: 300
  prefunded_accounts: {}
```

Configuration rules:

- Three distinct per-member addresses:
  - `vm.vm_ip` (`OUTBE_VM_IP`): static private IPv4 configured on the guest VM's `eth0` bridge interface (VM mode only). `outbe-net-up` and `outbe-up` (`vm status service_ip`, `host check --ip`, readiness, join driver) reach the VM at `<vm_ip>:<rpc_port>`.
  - `rpc_bind_ip` (`OUTBE_RPC_BIND_IP`, optional): HTTP JSON-RPC listen address (`--http.addr`). Defaults to `vm.vm_ip` in VM mode and `127.0.0.1` on bare hosts. In VM mode, `outbe-net-up` (`network build`, `deploy`, `start`, `update`, `add-validator`, `add-full-node`) refuses `rpc_bind_ip != vm.vm_ip` because host-side RPC readiness checks require reaching `OUTBE_VM_IP:OUTBE_RPC_PORT` (use standalone `outbe-up` if you need loopback-only RPC without `outbe-net-up` orchestration).
  - `advertise_ip` (`OUTBE_EXTERNAL_IP`): external/public IPv4 advertised for P2P, Radicle, and on-chain consensus registration.
- VM defaults are `8` vCPU, `16` GiB memory, and `100` GiB disk (`vcpu: 8`, `memory_gib: 16`, `disk_gib: 100`); keep `vm.memory_gib` at `16` or higher on active validator VMs to avoid OOM kills.
- Optionally set per-founder `rpc_port` (default `8545`, written to `OUTBE_RPC_PORT`) or pass `--rpc-port` to `add-validator` / `add-full-node`; RPC ports must be in `1..=65535` and distinct from all other configurable ports and fixed service ports (`8551`, `8876`, `9000`, `9002`, `9003`, `9101`).
- Set `topology.use_local_defaults: false` when founders advertise public IPs across hosts.
- For non-devnet VM networks (such as `testnet`), configure `feeder.rpc_endpoints` in `network.yaml` for every external chain ID required by the active feeder profile (`1`, `97`, and `11155111` for `testnet/pairs.toml`), plus `feeder.api_keys.<provider>` if a profile uses an API-key provider. `network build` embeds these endpoints into every validator `feeder-public.toml` (both founders and `joining/operator-config.example.yaml` for `add-validator`) and refuses to build if any required chain ID is missing or any `REPLACE_WITH_` placeholder remains. Devnet (`network: devnet`) uses the self-contained `mock` provider and requires no external feeder RPCs.
- When `genesis.base` is not set, `network build` defaults omitted epoch and DKG fields to `1200 / 300 / 300`.
- `network build` refuses `dkg_prepare_window_blocks < 120` or `dkg_activation_grace_blocks < 120`. Shorter windows risk starving DKG `ACK_COLLECTION_GRACE` (30s) at ~4 blocks/s.

## Build, deploy, start, and inspect

Build immutable network artifacts, founder keys, genesis, the static DNAT/firewall mapping table (`firewall_mappings` in `network-manifest.json`, also printed to stdout for published/multi-host VM networks), and the joining operator bundle:

```bash
outbe-net-up network build \
  --software-bundle ./outbe-software-bundle-v1-<commit>.tar.zst \
  --config ./network.yaml \
  --output ./out/network
```

Deploy stopped founder VMs across all hosts (upload or fetch the guest image, stage disks, create VMs, and run `outbe-up host check`). `deploy` (against an already-running network), `add-validator`, `add-full-node`, and `update` verify that `certified_rpc` answers `eth_chainId` with the network `chain_id` (directly from this machine for public URLs, or via the first VMM host for private addresses); on a brand-new founding network `deploy` skips that check and `start` verifies it once the network is live:

```bash
outbe-net-up network deploy \
  --artifacts ./out/network \
  --image-archive ./outbe-vm-image-v1-dev-<hash16>.tar.zst \
  --ssh-key ~/.ssh/id_ed25519
```

You can pass `--image-url <https-url> --image-sha256 <hex64>` instead of `--image-archive`. Pass `--no-tee` only on non-TEE development hosts.

Start all founder VMs and wait for finalized consensus growth:

```bash
outbe-net-up network start \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519 \
  --timeout 600 \
  --max-lag 20 \
  --min-finalized-blocks 10
```

Check network health (one-shot table, JSON, or continuous watch):

```bash
outbe-net-up network status --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519
outbe-net-up network status --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519 --json
outbe-net-up network status --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519 --watch
```

In one-shot and `--json` modes, `network status` exits non-zero if any member fails health checks. Failure conditions include a stopped or unreachable VM, lag above `--max-lag`, an orphan VM, missing `outbe_consensusStatus`, validator `connectedPeers < n - 1`, or an unhealthy/unreachable validator `outbe-feeder` (`http://<vm_ip>:9002/health`, bound on `0.0.0.0:9002` inside validator VMs). `network start`, `network add-validator`, and `network update` enforce the same validator feeder health check in their readiness gates.

## Add a joining validator or full node

Add a joining validator VM, fund it, bond stake, submit readiness, and wait through DKG activation:

```bash
outbe-net-up network add-validator \
  --artifacts ./out/network \
  --name validator-joiner-0 \
  --host 203.0.113.20 \
  --vm-ip 10.77.0.13 \
  --advertise-ip 203.0.113.20 \
  --port-offset 2 \
  --memory-gib 16 \
  --ssh-key ~/.ssh/id_ed25519 \
  --timeout 900
```

Add a strictly keyless full-node VM and check finalized block tracking:

```bash
outbe-net-up network add-full-node \
  --artifacts ./out/network \
  --name full-node-0 \
  --host 198.51.100.10 \
  --vm-ip 10.77.0.13 \
  --memory-gib 16 \
  --ssh-key ~/.ssh/id_ed25519 \
  --timeout 600
```

## Rolling guest OS image update

Roll all VMs in a live network to a new guest OS image one VM at a time:

```bash
outbe-net-up network update \
  --artifacts ./out/network \
  --image-archive ./outbe-vm-image-v1-dev-<new-hash16>.tar.zst \
  --software-bundle ./outbe-software-bundle-v1-<new-commit>.tar.zst \
  --ssh-key ~/.ssh/id_ed25519 \
  --timeout 900 \
  --stop-timeout 120 \
  --max-lag 20 \
  --min-finalized-blocks 10 \
  --dkg-guard-blocks 30
```

Omit `--software-bundle` when keeping the bundle from `<artifacts>/software-bundle.tar.zst`.

Safety checks built into `network update`:

- **One VM at a time, strictly graceful:** calls `outbe-up vm update` (`ShutdownVm`, `UpdateVm`, `StartVm`) and rejects `--force`. Every VM keeps its UUID, MAC address, static IP, and `hda.img` data disk.
- **Fail-closed peer and DKG guard:** before every stop, checks that all members are healthy, every validator reports `connectedPeers >= n - 1`, and `nextPlannedActivationHeight > 0`. Defers past the next DKG boundary when inside a ceremony or within the time-based guard window.
- **Forward 60-block missed-vote check:** before every stop, queries `IValidatorSet.getActiveValidators()` and `IValidatorSet.validatorByAddress(address)` via `eth_call` to `0x000000000000000000000000000000000000EE00` for all active validators and waits 60 finalized blocks. It requires `missedVotes` delta `0` for every validator. On epoch boundaries or transient missed votes, it retries the window up to `--timeout`. Each retry re-checks the DKG guard. Any RPC error or timeout fails closed.
- **Resumable:** if interrupted, rerun the same `network update` command. It skips VMs already on the target image, resumes at most one interrupted VM, and refuses when two or more members are unhealthy.
- **Post-rollout DKG check:** after the last VM returns in lockstep, waits for all validators to cross the next DKG boundary together. Every member must report matching finalized block hashes.

## Stop and remove a network

Gracefully stop all VMs in the network:

```bash
outbe-net-up network stop \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519 \
  --timeout 120
```

Gracefully remove all tagged VMs, clean staged disks on hosts, and remove `network-vm-state.json`:

```bash
outbe-net-up network remove \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519 \
  --timeout 120
```

`network remove` touches only VMs carrying this network's `owner_tag`. It leaves foreign VMs, guest OS images, and host firewall or bridge configurations untouched. Avoid `--force` on live validators.

## Bare-host founding networks

For bare-host (non-VM) founding networks (`network.yaml` without a top-level `vm:` section), stage founder bundles without starting services using `network prepare`, then start and inspect with `network start` and `network status`:

```bash
outbe-net-up network prepare --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519
outbe-net-up network start --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519
outbe-net-up network status --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519
```

## Diagnostics without VM SSH

Do not SSH into guest VMs. Inspect networks from the operator machine and VMM hosts:

- Run `outbe-net-up network status --artifacts ./out/network --json` for RPC, lag, VRF version, consensus peers, and TEE state.
- Run `outbe-up vm logs --name <network>-<member> --lines 200` on the VMM host to read the guest serial log.
- Query host-local JSON-RPC on `http://<vm-ip>:<rpc-port>` (default `8545`) from the VMM host (for example `outbe_consensusStatus` or `eth_call` to `IValidatorSet`).
- Pass `--ssh-config /dev/null` on any SSH command to ignore local `~/.ssh/config` overrides.
- SSH host fingerprints for VMM and bare-host targets are pinned per network in `$XDG_STATE_HOME/outbe-net-up/known-hosts-<genesis16>` (or `~/.local/state/outbe-net-up/known-hosts-<genesis16>`) using `StrictHostKeyChecking=accept-new`, isolated from `~/.ssh/known_hosts`.
- Do not edit `<artifacts>/network-vm-state.json` by hand.
