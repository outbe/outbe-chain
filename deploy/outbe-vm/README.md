# Outbe VM Deployment

This directory holds the dstack patches and manifest used to build reproducible Outbe guest OS images.

The measured guest image contains the node binaries (`outbe-chain`, `outbe-radicle`, `outbe-feeder`, `outbe-hyperlane-agent`), the minimal in-guest runtime agent (`outbe-vm-agent`), pinned runtime libraries, and static systemd units. It does not ship `outbe-up`, SSH clients, or VMM orchestration code.

Each VM stores its keys (validators only), genesis, CRS, `validator.env`, configuration files, and chain state on an unmeasured ext4 data disk mounted at `/opt/outbe-chain` with `noexec,nosuid,nodev`.

## Prerequisites

- Clean git checkout on a Linux `x86_64` build machine.
- Docker with BuildKit (`docker buildx`) for reproducible software and guest image builds.
- `sfdisk` (`util-linux >= 2.26`), `mke2fs`, and `debugfs` (`e2fsprogs >= 1.47`) in `PATH` for `outbe-up disk prepare`.
- Target VMM hosts running `dstack-vmm` (with `<vmm_home>/run/vmm.sock`), `qemu-img`, and a configured bridge (default `obr0`) with outbound NAT egress.
- Intel TDX VMM hosts must run a working Quote Generation Service (QGS). Even with `key_provider: none`, dstack guest boot requests an attestation quote.

## 1. Build the software bundle

Build the deterministic software bundle from a clean commit (uses the vendored CRS in `release/crs/bn254_g1.dat`):

```bash
nice -n 19 ionice -c3 cargo xtask release build --output ./out/software
```

This writes `outbe-software-bundle-v1-<commit>.tar.zst` into `./out/software`.

## 2. Build the guest VM images

Build the `dev` and `prod` dstack guest images from the verified software bundle:

```bash
nice -n 19 ionice -c3 cargo xtask release vm-image \
  --software-bundle ./out/software/outbe-software-bundle-v1-<commit>.tar.zst \
  --output ./out/vm-image \
  --flavors dev,prod \
  --jobs 4
```

You can also run `outbe-up image build` with the same inputs to build and optionally `--publish` into `<vmm_home>/image/`.

Options:

- Add `--check` to run two isolated builds and verify byte-for-byte reproducibility across all requested flavors.
- Pass `--dstack-repo <path>` to clone from a local dstack mirror instead of GitHub.
- Pass `--runtime-libs-dir <dir>` for offline builds. By default, `xtask` extracts the three pinned LLVM 18 runtime libraries (`libc++.so.1`, `libc++abi.so.1`, `libunwind.so.1`) from `Dockerfile.project-toolchain` and verifies their SHA-256 digests.
- Pass `--toolchain-image <tag>`, `--mkosi-image <tag>`, and `--cpuset-cpus <set>` when sharing a Docker daemon.

This writes the flavor image trees under `./out/vm-image/out/`, the deterministic archives `outbe-vm-image-v1-<flavor>-<hash16>.tar.zst`, and `./out/vm-image/vm-image-manifest.json`.

## Reproducibility

To produce and verify reproducible release artifacts from the same clean commit, run:

```bash
nice -n 19 ionice -c3 cargo xtask release build --output ./out/software

nice -n 19 ionice -c3 cargo xtask release vm-image \
  --software-bundle ./out/software/outbe-software-bundle-v1-<commit>.tar.zst \
  --output ./out/vm-image \
  --flavors dev,prod \
  --jobs 4 \
  --check
```

With `--check`, `xtask` runs two cold (`--no-cache`) isolated container builds into `./out/vm-image/a` and `./out/vm-image/b`. It compares `digest.txt`, every file SHA-256, and every symlink target byte for byte. On mismatch, it fails without creating `./out/vm-image/out`. On match, it promotes `./out/vm-image/a/out` to `./out/vm-image/out` and sets `"reproducible": true` in `vm-image-manifest.json`.

## 3. Extract matching CLIs

The software bundle, the guest VM image, `outbe-up`, and `outbe-net-up` should come from the same commit. Extract `bin/outbe-up` from the software bundle and verify its SHA-256 against `software-manifest.json`:

```bash
mkdir -p ./out/cli
tar --zstd -xf ./out/software/outbe-software-bundle-v1-<commit>.tar.zst \
  -C ./out/cli bin/outbe-up software-manifest.json
sha256sum ./out/cli/bin/outbe-up
grep -A2 '"bin/outbe-up"' ./out/cli/software-manifest.json
```

## 4. Guest boot and static networking

Every guest runs `outbe-vm-agent vm-boot` before starting node services:

1. Verifies `/opt/outbe-chain` mount flags (`noexec,nosuid,nodev`), ownership, and `OUTBE_NODE_ROLE` in `validator.env`.
2. Reads static IPv4 settings (`OUTBE_VM_IP`, `OUTBE_VM_PREFIX`, `OUTBE_VM_GATEWAY`, `OUTBE_VM_DNS`) from `validator.env`.
3. Writes `/run/systemd/network/20-wired.network.d/80-outbe-static.conf` and reloads `systemd-networkd`.
4. Starts the role's systemd units (`founder`, `joining-validator`, or `full-node`).

Guests do not depend on DHCP or `dnsmasq` reservations for their service IPs. Pick static bridge IPs outside the host DHCP pool and verify them with `outbe-up host check`.

## 5. Deploy and operate VMs

- **Single-host VM operator:** follow [bin/outbe-up/README.md](../../bin/outbe-up/README.md) (`host check`, `image fetch/import`, `disk prepare`, `vm create/start/stop/update/status/logs/remove`).
- **Multi-host founding network (dev team):** follow [bin/outbe-net-up/README.md](../../bin/outbe-net-up/README.md) (`network build/deploy/start/status/add-validator/add-full-node/update/stop/remove`).

## Dev-flavor guest SSH

- The `dev` flavor includes `sshd` for emergency host-side debugging. The `prod` flavor omits `sshd`.
- Normal operation and diagnostics never require SSH into guest VMs. Use `outbe-up vm status`, `outbe-up vm logs`, and host-local JSON-RPC at `http://<vm_ip>:<rpc_port>` (`OUTBE_VM_IP:OUTBE_RPC_PORT`, default `8545`).
- If you set `vm.authorized_keys` on a `dev` VM, `outbe-net-up` places it at `/opt/outbe-chain/dev/authorized_keys`. The `prod` flavor rejects `authorized_keys`.

## Known limitations

- **Hyperlane controller deployment:** VM validators start Hyperlane automatically; checkpoint
  submission remains in `waiting_for_controller` until the network's Hyperlane controller is
  deployed and initialized.
- **Forced stop during pending DKG:** force-stopping or power-cutting a validator can lose its pending DKG snapshot. Always use graceful `outbe-up vm stop`, `outbe-up vm update`, or `outbe-net-up network update` (never `--force`).
- **Guest memory:** allocate at least `8` GiB of RAM per validator VM (`memory_gib: 8`) to avoid guest OOM kills.
- **TEE resident key:** in devnet/testnet VMs without sealed storage, the resident TEE key lives in memory and re-registers on reboot. DKG shares persist under `/opt/outbe-chain`.
- **`/dstack/persistent` mount flags:** `/opt/outbe-chain` is bind-mounted `noexec,nosuid,nodev`, while `/dstack/persistent` remains mounted `exec`.
- **`prod` flavor validation:** the `prod` flavor image builds reproducibly, but has not booted on hardware with production attestation policy or encrypted storage.
