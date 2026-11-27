# outbe-up

`outbe-up` manages one Outbe VM on the local VMM host. It inspects host networking, manages guest OS images, prepares per-role data disks, and controls local VM lifecycle. It also keeps the bare-host `validator` workflow.

For multi-host VM networks, use `outbe-net-up` (see [bin/outbe-net-up/README.md](../outbe-net-up/README.md)).

## One-time host check and setup

`outbe-up host` commands inspect the host in read-only mode. They never edit host network files or reload host services.

Check local bridge, VMM socket, NAT, and TEE state:

```bash
outbe-up host status
```

After you create a VM, check host prerequisites for that VM:

```bash
outbe-up host check \
  --vm my-validator-0 \
  --ip 10.77.0.11 \
  --bridge obr0 \
  --public-ip 198.51.100.10 \
  --port 30303/tcp --port 30303/udp \
  --port 30400/tcp --port 31303/udp --port 8776/tcp \
  --source 203.0.113.20/32
```

`host check` checks three items:

- `host_network_ok`: the bridge exists, forwarding is active, and outbound NAT egress works.
- `service_ip_ok`: the static guest IP lies in the bridge subnet, outside the DHCP pool, and has no conflict.
- `published_ports_ok`: live `nftables` rules forward every requested port to the guest IP.

When a check fails, `host check` exits non-zero and prints the exact one-time host firewall and NAT rules. The host administrator applies those rules once and reruns `host check`.

To inspect outbound NAT or plan port rules separately:

```bash
outbe-up host nat --bridge obr0
outbe-up host publish --vm my-validator-0 --port 30400/tcp --source 203.0.113.20/32
```

## Guest OS images

Every VM boots a measured dstack guest image stored under `<vmm_home>/image/` (default `~/.dstack-vmm/image/`).

Build reproducible images locally from a verified software bundle:

```bash
outbe-up image build \
  --software-bundle ./outbe-software-bundle-v1-<commit>.tar.zst \
  --output ./out/vm-image \
  --flavors dev,prod \
  --jobs 4 \
  --check \
  --publish
```

Fetch a published image archive over HTTPS and check its SHA-256 before extraction:

```bash
outbe-up image fetch \
  --url https://example.invalid/outbe-vm-image-v1-dev-<hash16>.tar.zst \
  --sha256 <archive-sha256>
```

Import a local image archive and check its SHA-256 before extraction:

```bash
outbe-up image import \
  --archive ./outbe-vm-image-v1-dev-<hash16>.tar.zst \
  --sha256 <archive-sha256>
```

Check, list, or remove unused local images:

```bash
outbe-up image verify --dir ~/.dstack-vmm/image/outbe-vm-dev-<hash16>
outbe-up image list
outbe-up image remove --name outbe-vm-dev-<hash16>
```

## Prepare a VM data disk

`outbe-up disk prepare` builds a GPT+ext4 data disk for one node role without root privileges or loop mounts. Use `--staging-name <name>` to stage the disk under `~/.local/state/outbe-up/staging/<name>/data.img`, or use `--output <path>` to write a standalone file.

Prepare a `founder` disk from a built founder tree or tar archive:

```bash
outbe-up disk prepare \
  --role founder \
  --source ./network-artifacts/vm/validator-0 \
  --bundle ./network-artifacts/joining/outbe-validator-operator-bundle.tar.zst \
  --staging-name my-validator-0 \
  --size-gib 100
```

Prepare a `joining-validator` disk from an operator bundle and config:

```bash
outbe-up disk prepare \
  --role joining-validator \
  --bundle ./outbe-validator-operator-bundle.tar.zst \
  --config ./operator-config.yaml \
  --staging-name my-joiner-0 \
  --size-gib 100
```

Pass `--keys-from <dir>` to reuse an existing joining-validator key directory.

Prepare a keyless `full-node` disk (no validator keys):

```bash
outbe-up disk prepare \
  --role full-node \
  --bundle ./outbe-validator-operator-bundle.tar.zst \
  --config ./full-node-config.yaml \
  --staging-name my-full-node-0 \
  --size-gib 100
```

List or clean staged disks:

```bash
outbe-up disk clean --list
outbe-up disk clean --stale
outbe-up disk clean --staging-name my-validator-0
```

## Manage one local VM

Create a stopped VM from a staged disk and a published guest image:

```bash
outbe-up vm create \
  --name my-validator-0 \
  --image ~/.dstack-vmm/image/outbe-vm-dev-<hash16> \
  --disk-staging my-validator-0 \
  --consume-staging \
  --bridge obr0 \
  --ip 10.77.0.11 \
  --vcpu 8 \
  --memory-gib 16 \
  --disk-gib 100
```

Pass `--disk <path>` instead of `--disk-staging` when using a standalone raw disk file. Defaults are `8` vCPU, `16` GiB of memory, and `100` GiB of disk; allocate at least `16` GiB of memory per validator VM to avoid guest OOM kills.

Start, inspect, read serial logs, and stop one VM:

```bash
outbe-up vm start --name my-validator-0
outbe-up vm status --name my-validator-0
outbe-up vm logs --name my-validator-0 --lines 100
outbe-up vm stop --name my-validator-0 --timeout 120
```

Switch a running VM to a new guest OS image while keeping its VM UUID, MAC address, static IP, and `hda.img` data disk:

```bash
outbe-up vm update \
  --name my-validator-0 \
  --image ~/.dstack-vmm/image/outbe-vm-dev-<new-hash16> \
  --stop-timeout 120
```

`vm update` stops the guest gracefully via `ShutdownVm`, updates the image via `UpdateVm`, and starts the VM again. It never issues a forced stop.

Remove a VM after graceful shutdown (guest images remain intact):

```bash
outbe-up vm remove --name my-validator-0 --timeout 120
```

Pass `--expect-vm-id <uuid>` on `start`, `stop`, `update`, `status`, `logs`, and `remove` to check VM identity before any VMM call. Add `--json` to any local `host`, `image`, `disk`, or `vm` command for machine-readable output.

## Static address and ports in `validator.env`

The measured guest image runs `outbe-vm-agent vm-boot` on startup. It reads `/opt/outbe-chain/validator.env`, configures static IPv4, and starts the systemd units for the node role. Guests do not use DHCP for service IPs.

Static address and network keys in `validator.env` (three distinct IPv4 addresses):

- `OUTBE_NODE_ROLE`: `founder`, `joining-validator`, or `full-node`
- `OUTBE_VM_IP`: static private IPv4 address assigned to the guest VM's `eth0` interface on the host bridge (VM roles only, for example `10.77.0.11`)
- `OUTBE_RPC_BIND_IP`: HTTP JSON-RPC listen address (`--http.addr`); defaults to `OUTBE_VM_IP` in VM configs (may be set to `127.0.0.1` for loopback-only RPC) and to `127.0.0.1` on bare hosts
- `OUTBE_EXTERNAL_IP`: advertised public/external IPv4 for P2P (`--nat extip:...`), Radicle (`--advertise`), and on-chain consensus registration
- `OUTBE_VM_PREFIX`: IPv4 prefix length (for example `24`)
- `OUTBE_VM_GATEWAY`: bridge gateway IPv4 (for example `10.77.0.1`)
- `OUTBE_VM_DNS`: comma-separated DNS servers (for example `1.1.1.1,8.8.8.8`)

`outbe-vm-agent` configures `eth0` from `OUTBE_VM_IP` (writing `/run/systemd/network/20-wired.network.d/80-outbe-static.conf` and reloading `systemd-networkd`) and rejects any `OUTBE_RPC_BIND_IP` other than `127.0.0.1` or `OUTBE_VM_IP`.

Configurable ports in `validator.env` (P2P and Radicle ports use 1:1 host DNAT with `base + port_offset`):

- `OUTBE_RPC_PORT`: HTTP JSON-RPC TCP (default `8545`, bound to `OUTBE_RPC_BIND_IP`)
- `OUTBE_RETH_P2P_PORT`: execution P2P TCP and UDP (base `30303`)
- `OUTBE_CONSENSUS_PORT`: consensus P2P TCP (base `30400`)
- `OUTBE_RETH_DISCOVERY_PORT`: execution discovery UDP (base `31303`)
- `OUTBE_RADICLE_PORT`: Radicle TCP (base `8776`)

Fixed guest service ports are `8551`, `8876`, `9000` (Hyperlane), `9002`, `9003`, and `9101`. Do not assign any configurable port to a fixed port or to another configurable port.

Role services started by `outbe-vm-agent`:

- `founder`: `outbe-radicle.service`, `outbe-validator.service`, `outbe-feeder.service`
- `joining-validator`: `outbe-radicle.service`, `outbe-joining-validator.service`, `outbe-feeder.service`
- `full-node`: `outbe-full-node.service`

## Operational rules: what NOT to do

- **Do not force-stop a validator.** Never pass `--force` to `vm stop` or `vm remove` on an active validator. A forced stop can lose a pending DKG snapshot.
- **Do not stop two validators at once.** Stop or update one validator at a time. Wait until it returns to consensus in lockstep with zero missed votes before stopping the next validator.
- **Do not stop a validator near or during a DKG ceremony.** Check `outbe_consensusStatus` first. Check that `randomnessStatus` is `"healthy"` and leave ample blocks before `nextPlannedActivationHeight`.
- **Do not use short DKG windows.** Keep `dkg_prepare_window_blocks >= 120` and `dkg_activation_grace_blocks >= 120` (default `1200/300/300`). Shorter windows can starve the 30-second DKG `ACK_COLLECTION_GRACE` at ~4 blocks/s.
- **Do not SSH into guest VMs during normal operation.** Query `outbe-up vm status`, `outbe-up vm logs`, and host-local JSON-RPC at `http://<vm-ip>:<rpc-port>` (default `8545`).

## Bare-host joining validator

For non-VM hosts, `outbe-up validator` still prepares, synchronizes, promotes, and monitors a host-installed validator:

```bash
outbe-up validator config-example --output operator-config.yaml
sudo outbe-up validator prepare --bundle <operator-bundle.tar.zst> --config operator-config.yaml
sudo outbe-up validator sync [--snapshot <snapshot.tar.zst>]
sudo outbe-up validator promote
sudo outbe-up validator status [--watch]
```
