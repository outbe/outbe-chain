# Outbe VM Deployment

This directory holds the dstack patches and manifest used to build reproducible Outbe validator VM images.
The guest image contains the measured binaries, runtime libraries, and static systemd units.
Each validator's keys, genesis, configuration, CRS, and chain state live on an unmeasured ext4 data disk mounted at `/opt/outbe-chain` with `noexec,nosuid,nodev`.

## Prerequisites

- Clean git checkout on a Linux `x86_64` build machine.
- Docker with BuildKit (`docker buildx`) for reproducible software and guest image builds.
- `sfdisk` (`util-linux >= 2.26`), `mke2fs`, and `debugfs` (`e2fsprogs >= 1.47`) in `PATH` for `outbe-up vm disk`.
- Target VMM host(s) running `dstack-vmm` (with `<vmm_home>/run/vmm.sock`), `qemu-img`, and `dnsmasq` on the configured bridge (default `obr0`).

## 1. Build the Software Bundle

Build the deterministic software bundle from the current clean commit (uses the vendored CRS in `release/crs/bn254_g1.dat`; no network CRS download):

```bash
nice -n 19 ionice -c3 cargo xtask release build --output ./out/software
```

This writes `outbe-software-bundle-v1-<commit>.tar.zst` into `./out/software`.

## 2. Build the Guest VM Images

Build the `dev` and `prod` dstack guest images from the verified software bundle:

```bash
nice -n 19 ionice -c3 cargo xtask release vm-image \
  --software-bundle ./out/software/outbe-software-bundle-v1-<commit>.tar.zst \
  --output ./out/vm-image \
  --flavors dev,prod \
  --jobs 4
```

Options:
- Add `--check` to build twice and verify byte-for-byte reproducibility (`repro-check`).
- Pass `--dstack-repo <path>` to clone from a local dstack mirror instead of GitHub.
- Pass `--runtime-libs-dir <dir>` for offline builds; by default `xtask` builds the `toolchain` target of `Dockerfile.project-toolchain` with `--load`, extracts the three pinned LLVM 18 runtime libraries (`libc++.so.1`, `libc++abi.so.1`, `libunwind.so.1`), and verifies their SHA-256 digests.
- Pass `--toolchain-image <tag>`, `--mkosi-image <tag>`, and `--cpuset-cpus <set>` (e.g. `0-3`) when sharing a Docker daemon or isolated `BUILDX_BUILDER` worker.

This writes the built flavor directories and `./out/vm-image/vm-image-manifest.json` (recording `source_commit`, `software_bundle_sha256`, `dstack_rev`, patch hashes, and per-flavor `os_image_hash`).

## 3. Build Network Artifacts and VM Data Trees

Create a `network.yaml` for `devnet` or `testnet` with a top-level `vm:` section (`vmm_host`, optional `vmm_home`, `bridge`, `dhcp_hosts_file`, `dhcp_leases_file`, `dhcp_pid_file`) and per-founder `vm.bridge_ip` entries (see `bin/outbe-up/templates/network/network.example.yaml`).
VM deployment is rejected on `mainnet`, and `prod` flavor rejects `authorized_keys`.

Generate genesis, validator keys, and per-founder VM data trees (`vm/validator-0..3`):

```bash
outbe-up network build \
  --software-bundle ./out/software/outbe-software-bundle-v1-<commit>.tar.zst \
  --config ./network.yaml \
  --output ./out/network
```

## 4. Build Founder Data Disks (Optional Pre-Build)

`outbe-up vm create` can build founder disks on demand, or you can pre-build deterministic sparse GPT + ext4 data disks (no root privileges or loop mounts required) and pass `--disks-dir ./out/disks`:

```bash
for i in 0 1 2 3; do
  outbe-up vm disk \
    --artifacts ./out/network \
    --founder "$i" \
    --output "./out/disks/validator-${i}.img"
done
```

Each image contains one GPT partition labelled `dstack-data` with an ext4 filesystem holding `outbe-chain/` (owned by `0:0`, no executable files).

## 5. Create VMs and Apply DHCP Reservations

Create stopped founder VMs on the configured VMM host(s), upload the digest-qualified guest image (`outbe-vm-<flavor>-<hash16>`) atomically, stream each sparse disk compressed over SSH with raw SHA-256 verification before `qemu-img convert` + `resize` into `<vmm_home>/vm/<VM-ID>/hda.img`, and record VM IDs and assigned MACs in `./out/network/vm-state.json`:

```bash
outbe-up vm create \
  --artifacts ./out/network \
  --image-dir ./out/vm-image/out/dev/dstack-dev-0.6.3 \
  --disks-dir ./out/disks \
  --ssh-key ~/.ssh/id_ed25519
```

Pass `--ssh-config /dev/null` on any `outbe-up network` or `outbe-up vm` command if the local system SSH config overrides port/proxy settings for VMM hosts.

`vm create` never mutates host network configuration silently; it verifies the host's `dhcp_hosts_file` read-only and writes `./out/network/dnsmasq-reservations.txt`. Apply only this network's managed `mac,ip,10m` block (preserving foreign entries and refusing occupied bridge IPs) and reload dnsmasq:

```bash
outbe-up vm host-reserve \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519
```

Pass `--create` if the configured `dhcp_hosts_file` does not exist yet.

## 6. Start, Inspect, Stop, and Remove VMs

Start all recorded founder VMs concurrently, wait for finalized consensus via SSH-exec JSON-RPC (`ssh <vmm_host> curl http://<bridge_ip>:8545`, no published ports or SSH port forwarding required), and run Hyperlane on-chain setup:

```bash
outbe-up vm start \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519 \
  --timeout 600
```

Inspect VMM status, finalized height, execution peers, Hyperlane registration, and consensus telemetry (`vrf=<vrfMaterialVersion>`, peers, shares, TEE readiness):

```bash
outbe-up vm status \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519 \
  --watch
```

Stop or remove only the VM IDs recorded in `./out/network/vm-state.json` (guest OS images are left intact; `vm remove` waits for asynchronous VMM removal to complete before clearing `vm-state.json`). Pass `--release-reservations` on `vm remove` or run `outbe-up vm host-release` afterwards to strip only this network's managed dnsmasq block and reload dnsmasq:

```bash
outbe-up vm stop --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519
outbe-up vm remove --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519 --release-reservations
# Or separately after removal:
outbe-up vm host-release --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519
```
