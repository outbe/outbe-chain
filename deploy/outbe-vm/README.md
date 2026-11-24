# Outbe VM Deployment

This directory holds the dstack patches and manifest used to build reproducible Outbe validator VM images.
The guest image contains the measured binaries, runtime libraries, and static systemd units.
Each validator's keys, genesis, configuration, CRS, and chain state live on an unmeasured ext4 data disk mounted at `/opt/outbe-chain` with `noexec,nosuid,nodev`.

## Prerequisites

- Clean git checkout on a Linux `x86_64` build machine.
- Docker with BuildKit (`docker buildx`) for reproducible software and guest image builds.
- `sfdisk` (`util-linux >= 2.26`), `mke2fs`, and `debugfs` (`e2fsprogs >= 1.47`) in `PATH` for `outbe-up vm disk`.

## 1. Build the Software Bundle

Build the deterministic software bundle from the current clean commit:

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
- Pass `--runtime-libs-dir <dir>` for offline builds; by default `xtask` extracts the three pinned LLVM 18 runtime libraries (`libc++.so.1`, `libc++abi.so.1`, `libunwind.so.1`) from the `toolchain` target of `Dockerfile.project-toolchain` and checks their SHA-256 digests.

This writes the built flavor directories and `./out/vm-image/vm-image-manifest.json` (recording `source_commit`, `software_bundle_sha256`, `dstack_rev`, patch hashes, and per-flavor `os_image_hash`).

## 3. Build Network Artifacts and VM Data Trees

Create a `network.yaml` for `devnet` or `testnet` with a top-level `vm:` section and per-founder `vm.bridge_ip` entries (see `bin/outbe-up/templates/network/network.example.yaml`).
VM deployment is rejected on `mainnet`.

Generate genesis, validator keys, and per-founder VM data trees (`vm/validator-0..3`):

```bash
outbe-up network build \
  --software-bundle ./out/software/outbe-software-bundle-v1-<commit>.tar.zst \
  --config ./network.yaml \
  --output ./out/network
```

## 4. Build Founder Data Disks

Build a deterministic sparse GPT + ext4 data disk for each founder (no root privileges or loop mounts required):

```bash
for i in 0 1 2 3; do
  outbe-up vm disk \
    --artifacts ./out/network \
    --founder "$i" \
    --output "./out/disks/validator-${i}.img"
done
```

Each image contains one GPT partition labelled `dstack-data` with an ext4 filesystem holding `outbe-chain/` (owned by `0:0`, no executable files).

## 5. Create, Start, and Inspect VMs (TODO — Task D1)

> **TODO (Task D1, not implemented yet):** `outbe-up vm create`, `outbe-up vm start`, `outbe-up vm stop`, and `outbe-up vm status` are planned to deliver the image and founder disks to the VMM host(s), create stopped VMs with substituted data disks, start all founders together, run Hyperlane on-chain setup, and wait for finalized blocks.
