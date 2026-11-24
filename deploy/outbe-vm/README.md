# Outbe VM Deployment

This directory holds the dstack patches and manifest used to build reproducible Outbe validator VM images.
The guest image contains the measured binaries, runtime libraries, and static systemd units.
Each validator's keys, genesis, configuration, CRS, and chain state live on an unmeasured ext4 data disk mounted at `/opt/outbe-chain` with `noexec,nosuid,nodev`.

## Prerequisites

- Clean git checkout on a Linux `x86_64` build machine.
- Docker with BuildKit (`docker buildx`) for reproducible software and guest image builds.
- `sfdisk` (`util-linux >= 2.26`), `mke2fs`, and `debugfs` (`e2fsprogs >= 1.47`) in `PATH` for `outbe-up vm disk`.
- Target VMM host(s) running `dstack-vmm` (with `<vmm_home>/run/vmm.sock`), `qemu-img`, and `dnsmasq` on the configured bridge (default `obr0`).
- Intel TDX VMM hosts must run a working Quote Generation Service (QGS). Even with `key_provider: none`, dstack guest boot requests an attestation quote.

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
- Add `--check` to run two isolated builds and verify byte-for-byte reproducibility across all requested flavors (see [Reproducibility](#reproducibility)).
- Pass `--dstack-repo <path>` to clone from a local dstack mirror instead of GitHub.
- Pass `--runtime-libs-dir <dir>` for offline builds; by default `xtask` builds the `toolchain` target of `Dockerfile.project-toolchain` with `--load`, extracts the three pinned LLVM 18 runtime libraries (`libc++.so.1`, `libc++abi.so.1`, `libunwind.so.1`), and verifies their SHA-256 digests.
- Pass `--toolchain-image <tag>`, `--mkosi-image <tag>`, and `--cpuset-cpus <set>` (e.g. `0-3`) when sharing a Docker daemon or isolated `BUILDX_BUILDER` worker.

This writes the built flavor directories under `./out/vm-image/out/` and `./out/vm-image/vm-image-manifest.json` (recording `source_commit`, `software_bundle_sha256`, `dstack_rev`, `overlay_commit`, `reproducible`, patch hashes, runtime library hashes, and per-flavor `os_image_hash`).

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

With `--check`, `xtask` clones the assembled dstack overlay commit into two separate `--no-hardlinks` working trees and runs two independent cold (`--no-cache`) container builds into `./out/vm-image/a` and `./out/vm-image/b` (each with its own `.workspace` and output tree). It then compares `digest.txt` and the SHA-256 of every file and symlink target inside each requested flavor's image directory byte for byte. On a mismatch, it fails and lists every differing flavor and file without creating `./out/vm-image/out`. On a match, it promotes `./out/vm-image/a/out` to `./out/vm-image/out` and sets `"reproducible": true` in `vm-image-manifest.json`.

## 3. Extract the Matching `outbe-up` CLI

The software bundle, the guest VM image, and the `outbe-up` CLI must come from the same commit. Extract `bin/outbe-up` from the software bundle you built in Step 1 and verify its SHA-256 against `software-manifest.json` inside the bundle:

```bash
mkdir -p ./out/cli
tar --zstd -xf ./out/software/outbe-software-bundle-v1-<commit>.tar.zst \
  -C ./out/cli bin/outbe-up software-manifest.json
sha256sum ./out/cli/bin/outbe-up
grep -A2 '"bin/outbe-up"' ./out/cli/software-manifest.json
```

Use `./out/cli/bin/outbe-up` for every deployment command below.

## 4. Build Network Artifacts and VM Data Trees

Create a `network.yaml` for `devnet` or `testnet` with a top-level `vm:` section (`vmm_host`, optional `vmm_home`, `bridge`, `dhcp_hosts_file`, `dhcp_leases_file`, `dhcp_pid_file`) and per-founder `vm.bridge_ip` entries (see `bin/outbe-up/templates/network/network.example.yaml`).
VM deployment is rejected on `mainnet`, and `prod` flavor rejects `authorized_keys`.

Generate genesis, validator keys, and per-founder VM data trees (`vm/validator-0..3`):

```bash
./out/cli/bin/outbe-up network build \
  --software-bundle ./out/software/outbe-software-bundle-v1-<commit>.tar.zst \
  --config ./network.yaml \
  --output ./out/network
```

## 5. Build Founder Data Disks (Optional Pre-Build)

`outbe-up vm create` can build founder disks on demand, or you can pre-build deterministic sparse GPT + ext4 data disks (no root privileges or loop mounts required) and pass `--disks-dir ./out/disks`:

```bash
for i in 0 1 2 3; do
  ./out/cli/bin/outbe-up vm disk \
    --artifacts ./out/network \
    --founder "$i" \
    --output "./out/disks/validator-${i}.img"
done
```

Each image contains one GPT partition labelled `dstack-data` with an ext4 filesystem holding `outbe-chain/` (owned by `0:0`, no executable files).

## 6. Create VMs and Apply DHCP Reservations

Create stopped founder VMs on the configured VMM host(s), upload the digest-qualified guest image (`outbe-vm-<flavor>-<hash16>`) atomically, stream each sparse disk compressed over SSH with raw SHA-256 verification before `qemu-img convert` + `resize` into `<vmm_home>/vm/<VM-ID>/hda.img`, and record VM IDs and assigned MACs in `./out/network/vm-state.json`:

```bash
./out/cli/bin/outbe-up vm create \
  --artifacts ./out/network \
  --image-dir ./out/vm-image/out/dev/dstack-dev-0.6.3 \
  --disks-dir ./out/disks \
  --ssh-key ~/.ssh/id_ed25519
```

`vm create` never mutates host network configuration silently. It checks the host's `dhcp_hosts_file` and `dhcp_leases_file` read-only, refuses any occupied IP or active lease conflict before creating VMs or uploading disks, and writes `./out/network/dnsmasq-reservations.txt`.

Apply only this network's managed `mac,ip,10m` block (preserving foreign entries and refusing occupied bridge IPs) and reload dnsmasq:

```bash
./out/cli/bin/outbe-up vm host-reserve \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519
```

Pass `--create` if the configured `dhcp_hosts_file` does not exist yet.

### `--ssh-config` Scope

Pass `--ssh-config /dev/null` (forwarded to `ssh -F`) when the local system SSH config overrides port or proxy settings for VMM hosts. `--ssh-config` is supported only on commands that open SSH connections:
- `outbe-up vm create`
- `outbe-up vm host-reserve`
- `outbe-up vm host-release`
- `outbe-up vm start`
- `outbe-up vm stop`
- `outbe-up vm remove`
- `outbe-up vm status`
- `outbe-up network prepare`
- `outbe-up network start`
- `outbe-up network status`

Local commands (`outbe-up network build`, `outbe-up vm disk`, and `outbe-up validator *`) do not accept `--ssh-config`.

## 7. Start, Inspect, Stop, and Remove VMs

Start all recorded founder VMs concurrently, wait for finalized consensus via SSH-exec JSON-RPC (`ssh <vmm_host> curl http://<bridge_ip>:8545`, no published ports or SSH port forwarding required), run Hyperlane on-chain setup, and verify consensus health:

```bash
./out/cli/bin/outbe-up vm start \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519 \
  --timeout 600 \
  --max-lag 20
```

`vm start` succeeds only when every founder is running on the expected chain ID with all required peers, reports `hyperlane_registered == true`, trails the highest finalized block by at most `--max-lag` blocks (default `20`), and matches the majority `vrfMaterialVersion`.

Inspect VMM status, finalized height and lag, execution peers, Hyperlane registration, and consensus telemetry (`vrf=<vrfMaterialVersion>`, peers, shares, TEE readiness):

```bash
# One-shot check for scripts and automation (exits 0 when all founders are healthy, non-zero otherwise):
./out/cli/bin/outbe-up vm status \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519 \
  --max-lag 20

# Continuous watch view:
./out/cli/bin/outbe-up vm status \
  --artifacts ./out/network \
  --ssh-key ~/.ssh/id_ed25519 \
  --max-lag 20 \
  --watch
```

In one-shot mode (without `--watch`), `vm status` exits `0` only if every founder is running, reachable with a finalized block, within `--max-lag` (default `20`) of the highest founder, and on the majority `vrfMaterialVersion`. Any stopped, unreachable, or `[LAGGING: ...]` founder causes a non-zero exit code.

### Stopping, Single-Founder Restarts, and Removal

`vm stop` and `vm start` act on all founders recorded in `./out/network/vm-state.json`; there is no single-founder selector yet. Do not edit `vm-state.json` to target one VM. To gracefully restart a single founder, run the VMM CLI on that founder's VMM host with its VM ID from `vm-state.json`:

```bash
dstack stop <vm-id>
# Confirm `dstack info <vm-id>` shows Status: stopped before starting again.
dstack start <vm-id>
```

To stop or remove all VMs in the network (guest OS images stay intact; `vm remove` polls until asynchronous VMM removal finishes within `--timeout`, default `120` seconds, before deleting `vm-state.json`):

```bash
./out/cli/bin/outbe-up vm stop --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519
./out/cli/bin/outbe-up vm remove --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519 --release-reservations
# Or release reservations separately after removal:
./out/cli/bin/outbe-up vm host-release --artifacts ./out/network --ssh-key ~/.ssh/id_ed25519
```

`vm remove --release-reservations` and `vm host-release` strip only this network's managed block from `dhcp_hosts_file` and reload dnsmasq. They do not delete active leases from `dhcp_leases_file`. Existing leases stay active until expiry (`10m` for networks created by this version; leases created by earlier versions without a duration may be infinite). Re-creating a network on the same IPs before those leases expire fails `vm create`'s preflight before any mutation. Wait for lease expiry or choose other free bridge IPs; do not hand-edit `dhcp_leases_file`.

## 8. Dev-Flavor Guest SSH Access

- The `dev` flavor enables `sshd` with default `StrictModes yes` and `PermitRootLogin prohibit-password`. The `prod` flavor contains no `sshd` configuration or SSH service preset.
- Guest VMs are reachable only from the VMM host over their bridge IPs (no published host SSH ports).
- The public key configured in `vm.authorized_keys` is the guest login key (separate from the VMM host `--ssh-key`) and is placed at `/opt/outbe-chain/dev/authorized_keys` on the data disk.
- Some VMM hosts reject SSH TCP forwarding (`ProxyJump`). Run diagnostic SSH directly from the VMM host. If you copy a temporary diagnostic private key to the VMM host, keep it mode `0600` in a `0700` directory and delete it when done.
- The `dev` guest generates a new SSH host key on every boot. After an intentional reboot, remove only that restarted VM's old entry from your `known_hosts` file instead of disabling host key checking globally.

## Known Limitations

- **Hyperlane validator on devnet**: `outbe-hyperlane-validator` runs and registers on chain, but remains idle on devnet because genesis has no external controller and VMs have no outbound internet route.
- **Force-stop during pending DKG**: force-stopping or power-cutting a validator can lose its pending DKG snapshot (tracked as a node defect; use graceful `dstack stop` and wait for `stopped` before restarting).
- **TEE resident key location**: the validator's TEE resident key is stored on the unmeasured ext4 data disk under `/opt/outbe-chain`.
- **`/dstack/persistent` mount flags**: `/opt/outbe-chain` is bind-mounted `noexec,nosuid,nodev`, while `/dstack/persistent` itself remains mounted `exec` (and `noexec` does not block script interpreters).
- **`prod` flavor validation**: the `prod` flavor image is built and verified byte-for-byte reproducible, but has not been booted on hardware with production attestation policy or encrypted storage.
