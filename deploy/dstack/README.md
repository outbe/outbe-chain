# Outbe validator on dstack

This directory builds a [dstack](https://github.com/Dstack-TEE/dstack) guest
image that runs an Outbe validator, and prepares a four-validator development
network in which every validator runs in its own dstack VM.

Status: MVP. The running network uses the `gramine-direct-dev` attestation mode
and the hardware-free development enclave. The protocol also has attestation
modes for AMD SEV-SNP and Intel TDX; no network has run in them yet.

dstack is used as a substrate only: VMM, guest OS and attestation report
delivery. Its KMS, gateway and contracts are not used.

## Contents

| File | Purpose |
|---|---|
| `build-binaries.sh` | reproducible build of the Outbe binaries in the pinned toolchain image |
| `build-binaries-in-container.sh` | the part of the build that runs inside the image |
| `build-image.sh` | builds the guest image from a pinned dstack revision plus the Outbe overlay |
| `patches/` | changes to the dstack tree |
| `rootfs/` | files added to the guest root file system |
| `app-compose.json` | dstack application manifest of a validator VM |
| `make-bundles.sh` | generates genesis and one bundle per validator |

## What is in the image

| Path in the guest | Content |
|---|---|
| `/usr/local/bin/` | `outbe-chain`, `outbe-radicle`, `outbe-cli`, `outbe-feeder`, `outbe-tee-enclave-mock`, `outbe-tee-enclave-local-e2e` |
| `/usr/local/lib/outbe/lib/` | shared libraries the binaries need and Debian does not ship |
| `/usr/local/lib/outbe/provision.sh` | first-boot fetch of the validator bundle |
| `/usr/local/lib/outbe/run-role.sh` | runs one launch script of the bundle |
| `outbe-{provision,enclave,radicle,node}.service` | systemd units, started by the application manifest |

The image is the same for every validator. Validator-specific material
(genesis, keys, launch scripts) arrives in a bundle at first boot.

Changes to the dstack tree:

- `patches/0001-unencrypted-ext4-data-disk.patch` adds
  `dstack.storage_encrypted=false dstack.storage_fs=ext4` to the kernel command
  line. The command line is fixed at image build time and is measured.
- `rootfs/` is copied over `os/mkosi/mkosi.skeleton/`.

The pinned dstack revision is `DSTACK_COMMIT` in `build-image.sh`.

## Guest layout

| Path | Storage | Content |
|---|---|---|
| `/dstack/persistent/outbe/` | data disk, not encrypted, survives restarts | unpacked bundle, chain data, Radicle state, enclave state, logs |
| `/dstack/persistent/outbe/validator-<i>/tee/` | data disk | sealed enclave state |
| `/dstack/persistent/outbe/crs/` | data disk | Barretenberg CRS file |
| `/dstack/user_config` | host share | `{"validator_index": N, "bundle_url": "..."}` |

Chain data needs no protection: a host that changes it makes its own validator
faulty, and consensus handles that.

The enclave state must survive a VM restart. The registry does not admit a
validator again while its enclave lease is live (up to 14 days), so a validator
that loses the offer key cannot return with `outbe-cli tee join`.

## 1. Prepare the host

Requirements: x86-64 Linux with KVM, Docker that can run `--privileged`, about
60 GB of free disk space.

```sh
sudo apt-get install -y --no-install-recommends \
    qemu-system-x86 qemu-utils swtpm swtpm-tools libvirt-clients dnsmasq-base jq
sudo usermod -aG kvm "$USER"          # log in again, or use `sg kvm -c '...'`
sudo modprobe vhost_vsock
```

`qemu-utils` supplies `qemu-img`, which the VMM calls. `libvirt-clients`
supplies `virsh`; the VMM network helper refuses to start without it. No libvirt
daemon is needed.

Clone dstack at the pinned revision and build the VMM tools:

```sh
mkdir -p ~/outbe-tdx-mvp && cd ~/outbe-tdx-mvp
git clone --branch next https://github.com/Dstack-TEE/dstack.git
git -C dstack checkout c88f60e781a62d542e0843b333f18a48d1458b88
cd dstack
nice -n 19 cargo build --manifest-path dstack/Cargo.toml --release -j 4 \
    -p dstack-vmm -p supervisor -p mock-attestation -p dstack-verifier
install -Dm755 dstack/target/release/dstack-vmm              ~/.local/bin/dstack-vmm
install -Dm755 dstack/target/release/supervisor              ~/.local/bin/dstack-supervisor
install -Dm755 dstack/target/release/dstack-mock-attestation ~/.local/bin/dstack-mock-attestation
install -Dm755 dstack/target/release/dstack-verifier         ~/.local/bin/dstack-verifier
install -Dm755 dstack/vmm/src/vmm-cli.py                     ~/.local/bin/dstack
```

Configure the VMM in `~/.dstack-vmm/vmm.toml` (start from
`dstack/vmm/vmm.toml`):

```toml
[image]
path = "/home/USER/.dstack-vmm/image"

[cvm]
qemu_path = "/usr/bin/qemu-system-x86_64"
kms_urls = []
gateway_urls = []
allowed_bridges = ["obr0"]

[cvm.networking]
mode = "user"
bridge = "obr0"

[cvm.port_mapping]
enabled = true

[supervisor]
exe = "/home/USER/.local/bin/dstack-supervisor"

# Development without TEE hardware only. Use your own random 32-byte seed.
[cvm.tee_simulator]
mock_attestation_seed = "<64 hex characters>"
collateral_base_url = "http://10.0.2.2:18088"
```

Start the VMM from a fixed directory. Its API socket path is relative to it:

```sh
mkdir -p ~/.dstack-vmm/run && cd ~/.dstack-vmm/run
sg kvm -c "$HOME/.local/bin/dstack-vmm -c ../vmm.toml"
```

All `dstack ...` commands below run from `~/.dstack-vmm/run`.

## 2. Network between the VMs

Each VM needs its own stable IP address. The consensus transport accepts an
inbound connection only when the source IP equals the registered address of the
peer. QEMU user-mode networking with port forwarding does not satisfy this, so
the VMs use dstack bridge networking.

```sh
# Bridge with the host as 10.77.0.1.
sudo ip link add obr0 type bridge
sudo ip addr add 10.77.0.1/24 dev obr0
sudo ip link set obr0 up

# Let the VMs reach the host (DHCP, bundle server). Needed when the INPUT
# policy of the host firewall is `drop`.
sudo nft insert rule ip filter INPUT iifname "obr0" counter accept

# DHCP on the bridge only. `--dhcp-ignore-clid` identifies a VM by its MAC
# address: a guest presents a new client id on every boot.
sudo dnsmasq --user=root --conf-file=/dev/null --interface=obr0 --bind-interfaces \
    --except-interface=lo --port=0 --dhcp-ignore-clid \
    --dhcp-range=10.77.0.10,10.77.0.50,255.255.255.0,infinite \
    --dhcp-hostsfile=/var/lib/misc/dnsmasq.outbe-mvp.hosts \
    --dhcp-option=option:router,10.77.0.1 \
    --dhcp-leasefile=/var/lib/misc/dnsmasq.outbe-mvp.leases \
    --pid-file=/run/outbe-mvp-dnsmasq.pid

# Privileged VMM helper that attaches VMs to the bridge.
sudo mkdir -p /run/dstack
sudo sg kvm -c "$HOME/.local/bin/dstack-vmm --config $HOME/.dstack-vmm/vmm.toml netd"
```

`/var/lib/misc/dnsmasq.outbe-mvp.hosts` pins one address per VM, one line per
VM: `<mac>,id:*,<ip>`. The MAC address of a VM is derived from its VM id and is
stable. Fill the file after the VMs are created (step 5).

These settings do not survive a host reboot. The VMs have no internet access
with this setup.

## 3. Build the binaries

```sh
JOBS=4 ./deploy/dstack/build-binaries.sh --output ~/outbe-tdx-mvp/bin-repro
```

The build runs in the pinned project toolchain image and writes `bin/`, `lib/`,
`metadata/` and `SHA256SUMS`. To prove that the build is reproducible, build
twice from scratch and compare:

```sh
JOBS=4 ./deploy/dstack/build-binaries.sh --output ~/outbe-tdx-mvp/bin-check --check
```

Feature flags used:

| Package | Features | Reason |
|---|---|---|
| `outbe-chain` | `production-enclave`, `test-protocol-overrides` | a genesis from the testnet profile carries `config.outbeProtocol`; a build without `test-protocol-overrides` rejects it |
| `outbe-tee-enclave` | `local-e2e` | builds the hardware-free variants `outbe-tee-enclave-mock` and `outbe-tee-enclave-local-e2e` |
| others | none | |

## 4. Build the guest image

```sh
BIN_DIR=~/outbe-tdx-mvp/bin-repro JOBS=4 ./deploy/dstack/build-image.sh
```

The output is `~/outbe-tdx-mvp/build-outbe/out/dev/`. The script prints the
`os_image_hash` of the image. Install the image for the VMM under its own name:

```sh
tmp=$(mktemp -d -p ~/outbe-tdx-mvp)
tar -xzf ~/outbe-tdx-mvp/build-outbe/out/dev/dstack-dev-*.tar.gz -C "$tmp"
mv "$tmp"/dstack-dev-* ~/.dstack-vmm/image/outbe-validator-dev
```

The `dev` flavor contains the dstack TEE simulator and an SSH server. Build the
`prod` flavor (`FLAVORS=prod`) for real hardware.

## 5. Create the VMs

```sh
for i in 0 1 2 3; do
    echo "{\"validator_index\": $i, \"bundle_url\": \"http://10.77.0.1:18090/validator-$i.tgz\"}" \
        > user-config-$i.json
    dstack deploy --name outbe-v$i --image outbe-validator-dev \
        --compose deploy/dstack/app-compose.json --user-config user-config-$i.json \
        --vcpu 3 --memory 5G --disk 40G --net bridge \
        --simulated-tee dstack-amd-sev-snp
done
```

Drop `--simulated-tee` on a host with real SEV-SNP or TDX.

Each VM boots and waits for its bundle. Read the addresses the VMs received from
`/var/lib/misc/dnsmasq.outbe-mvp.leases`, pin them in
`/var/lib/misc/dnsmasq.outbe-mvp.hosts`, and restart `dnsmasq`.

The stock manifest gives no shell access. For a development VM add the lines
that install an SSH key to `bash_script` before deploying; `/root` is read-only
in the guest, so the key goes to `/etc/ssh/` with an `AuthorizedKeysFile`
drop-in.

## 6. Generate the network and start it

```sh
for i in 0 1 2 3; do
    outbe-keygen validator --output-dir <net>/keys/validator-$i --chain-id 424242
done
./deploy/dstack/make-bundles.sh <net> <ip-0> <ip-1> <ip-2> <ip-3>
(cd <net>/publish && python3 -m http.server 18090 --bind 10.77.0.1)
```

`make-bundles.sh` runs `outbe-genesis-cli create` with `tee.mode:
gramine-direct-dev` and then adjusts the generated launch scripts:

- `run-enclave.sh` starts `outbe-tee-enclave-mock` as a plain process with its
  state on the data disk (`TEE_STATE=disk`, the default);
- `run-node.sh` takes the Barretenberg CRS from the data disk, fetching it once
  from the bundle server, because the node otherwise downloads it from the
  internet at startup;
- `run-node.sh` gets `--consensus.use-local-defaults`, because the default
  consensus network configuration rejects private addresses.

Each VM fetches its bundle, unpacks it to `/dstack/persistent/outbe` and starts
the enclave, the Radicle sidecar and the node. The founders run the DKG ceremony
together: all four nodes must be up within the same 120 seconds.

## 7. Check the network

Inside a VM:

```sh
systemctl is-active outbe-provision outbe-enclave outbe-radicle outbe-node
curl -s -X POST -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"outbe_consensusStatus","params":[]}' \
    http://127.0.0.1:8545 | jq '{mode: .result.mode, enclave: .result.enclave.state}'
```

A healthy validator reports `mode: signer` and `enclave: ready`. The TEE
registry (`0x…EE0A`) answers `isBootstrapped() = 1` and
`isValidatorEnclaveReady(<validator>) = 1`.

## 8. Restart a validator

```sh
dstack stop <vm-id>
dstack start <vm-id>
```

The enclave restores its sealed state from the data disk, and the node rejoins
without operator action. The other three validators keep producing blocks while
one is down.

## Attestation evidence source

`outbe-tee-enclave` reads platform attestation evidence through one abstraction
(`platform_attestation::AttestationSource`):

| Platform | Interface | Evidence |
|---|---|---|
| Intel SGX | Gramine `/dev/attestation` | DCAP quote |
| AMD SEV-SNP | kernel `configfs-tsm` | attestation report and certificate table |
| Intel TDX | kernel `configfs-tsm` | TDX quote |

`outbe-tee-enclave --probe-attestation` prints what the machine provides. In an
SEV-SNP guest it prints the launch measurement and the host data, the two values
that the attestation policy pins. It also prints the report fields that the
verifier checks, the certificates that the host supplies, and whether the
processor derives a sealing key. In a TDX guest it prints `MRTD`, `RTMR0` to
`RTMR2` and `MRCONFIGID`, the quote layout facts, and whether the dstack guest
agent returns a key. `HARDWARE.md` lists the lines and their meaning.

Set `no_instance_id: true` in the application manifest of a network that uses
SEV-SNP or TDX attestation. Without it the host data (SEV-SNP) or `MRCONFIGID`
(TDX) contains a per-VM instance id and differs between validators.

## SEV-SNP attestation mode

The protocol has an attestation mode for AMD SEV-SNP,
`AttestationMode::SevSnpRequired`. The policy pins three values:

| Value | Policy field | Source |
|---|---|---|
| launch measurement of the VM image | `mrenclave` of a rule, as a keccak projection | `--probe-attestation`, or `dstack-mr` for the image and the VM shape |
| host data | `mrsigner` of a rule | `--probe-attestation`; equal for all VMs with `no_instance_id: true` |
| SHA-256 of the AMD root certificate (ARK, DER) | `intel_root_der_hash` | the ARK of the processor generation |

Generate the genesis policy:

```sh
outbe-chain tee genesis --input seeded.json --output genesis.json \
    --mode sev-snp-required \
    --snp-measurement <96 hex digits> \
    --snp-host-data <64 hex digits> \
    --snp-ark-sha256 <64 hex digits>
```

`outbe-genesis-cli` takes the same values as `tee.mode: sev-snp-required` with
the keys `snp_measurement`, `snp_host_data` and `snp_ark_sha256`. The mode is
allowed on devnet and testnet.

Status of the mode:

| Part | State |
|---|---|
| Evidence format, policy projection, gas, genesis and CLI | done |
| Verification of a report inside the enclave service (feature `sev-snp`) | done; tested with a report of a real Milan processor |
| Registry admission and the offer key hand-over to a new node | done |
| Evidence assembly in the node at network start and in `outbe-cli tee join` | done; unit tests only |
| Service runtime in a confidential VM: report source, own code identity, sealing with a processor key | done; unit tests only |
| Renewal, binding replacement, measurement transition | outside the first scope; explicit errors |

The verifier accepts report versions 2, 3 and 5. Version 5 comes from firmware
with SEV-SNP API 1.58; it needs version 8.0.0 of the `sev` crate, in this
repository and in the dstack guest tools (patch
`patches/0002-sev-crate-8-report-version-5.patch`).

No network has run in this mode yet. The verifier accepts the AMD certificate
chain only (RSA-PSS). It rejects the mock certificates of the dstack simulator,
which use ECDSA, and the simulator guest has no `/dev/sev-guest` device for the
sealing key. An end-to-end run therefore needs real hardware.

### Service in a confidential VM

The image carries `outbe-tee-enclave`: the production entry point of the
service without development code. It is built with the features `sev-snp` (the
confidential VM backend for both platforms) and `native-dcap` (the Intel quote
verification library, which verifies TDX quotes). Start it with:

```sh
outbe-tee-enclave --platform sev-snp \
    --network-descriptor <dir>/network-descriptor-v1.bin \
    --socket 127.0.0.1:17000 --tee-dir <dir>/tee --chain-id <hex32>
```

With `--platform sev-snp` the service takes from the platform what an SGX
enclave takes from Gramine:

| Need | Source in the VM |
|---|---|
| own code identity | launch measurement and host data of an attestation report read at startup |
| registration evidence | attestation report from the kernel `configfs-tsm` interface |
| sealing key | key that the processor derives for the launch measurement (`/dev/sev-guest`) |

The service does not start when one of the three is unavailable.

`make-bundles.sh` with `TEE_MODE=sev-snp` writes this command into the
validator bundles and puts the SEV-SNP policy into the genesis.

### AMD certificates

The node adds three AMD certificates to the report. It looks for them in this
order:

1. the certificate table that the host passes to the guest;
2. files in the directory `OUTBE_SEV_SNP_CERT_DIR` (the bundles set it to
   `/dstack/persistent/outbe/sev-snp-certs`): `vcek.der` or `vcek.pem`,
   `cert_chain.pem`, or `ask.*` and `ark.*`;
3. for the root key and the signing key: the AMD certificates built into the
   `sev` crate (Milan, Genoa, Turin).

The node verifies the report with the collected certificates before it submits
the evidence. When the host table has no VCEK, download it one time from the AMD
key distribution service and put it into the directory. The probe line
`sev-snp report: ...` gives the chip id and the TCB version for the request.

### Known gaps of this mode

- **The network descriptor is not measured.** The service reads it from the
  data disk. An operator can give the service a descriptor with the consensus
  keys of a private chain and obtain the offer key. The planned fix puts the
  hash of the descriptor into the application manifest, which the host data
  binds.
- **The sealing key does not depend on the application manifest.** The
  processor derives the key from the launch measurement. A VM that starts the
  same image with another application manifest receives the same key and can
  open the sealed state. The `dev` image flavor also has an SSH server.
- For both reasons this mode does not protect the offer key from the operator
  yet. The mode is allowed on devnet and testnet only.

## TDX attestation mode

The protocol has an attestation mode for Intel TDX,
`AttestationMode::TdxRequired`. TDX evidence has the layout of SGX DCAP
evidence: a quote of version 4 and eight Intel collateral components. The
verifier is the DCAP pipeline with the TDX profile and the TDX entry point of
the pinned Intel quote verification library. The policy pins:

| Value | Policy field | Source |
|---|---|---|
| `MRTD`, `RTMR0`, `RTMR1`, `RTMR2` of the trust domain | `mrenclave` of a rule, as one keccak projection | `--probe-attestation` |
| `MRCONFIGID` | `mrsigner` of a rule, as a keccak projection | `--probe-attestation`; equal for all VMs with `no_instance_id: true` |
| lowest TCB evaluation data number | `minimum_tcb_evaluation_data_number` | the Intel TCB info of the processor |

`RTMR3` is not part of the identity. `RTMR0` depends on the VM shape, so every
shape needs its own rule. The Intel root is the SGX root that DCAP uses.

```sh
outbe-chain tee genesis --input seeded.json --output genesis.json \
    --mode tdx-required \
    --tdx-mrtd <96 hex digits> --tdx-rtmr0 <96> --tdx-rtmr1 <96> --tdx-rtmr2 <96> \
    --tdx-mrconfigid <96 hex digits> \
    --tdx-minimum-tcb-evaluation-data-number <number>
```

`outbe-genesis-cli` takes the same values as `tee.mode: tdx-required` with the
keys `tdx_mrtd`, `tdx_rtmr0`, `tdx_rtmr1`, `tdx_rtmr2`, `tdx_mrconfigid` and
`tdx_minimum_tcb_evaluation_data_number`. `make-bundles.sh` takes them with
`TEE_MODE=tdx`. The mode is allowed on devnet and testnet.

Start the service with `--platform tdx`; the other arguments are those of
`--platform sev-snp`.

### Intel collateral

The node adds the Intel collateral to the quote. The PCK certificate chain
comes from the quote. The node reads the other seven components from
`collateral.json` in the directory `OUTBE_TDX_COLLATERAL_DIR` (the bundles set
it to `/dstack/persistent/outbe/tdx-collateral`). An operator downloads the
collateral for the processor from the Intel provisioning certification service
and refreshes it before it expires. `crates/system/tee/src/tdx_host.rs`
describes the fields of the file.

### Status and known gaps of the TDX mode

| Part | State |
|---|---|
| Evidence format, policy projection, genesis and CLI | done |
| Verification of a quote (feature `native-dcap`) | done; tested with a real Intel TDX quote and its collateral |
| Evidence assembly in the node and in `outbe-cli tee join` | done; unit tests only |
| Service runtime: quote source, own code identity, sealing | done; unit tests only |
| Renewal, binding replacement, measurement transition | outside the first scope; explicit errors |

- No network has run in this mode. No TDX host has run this image.
- **Sealing key.** The service takes its sealing key from the dstack guest
  agent. With `key_provider: none` in the application manifest the agent makes
  new keys at every boot: the sealed state does not survive a VM restart, and
  the key is not bound to the measurements. A key that survives a restart needs
  the dstack local key provider on the host, or a key of the TDX module if the
  guest kernel exposes one.
- The network descriptor is not measured, as in the SEV-SNP mode.
- Quotes of version 5 and the TDX-only verification results
  (`TD_RELAUNCH_ADVISED`) are rejected.

## Limits of this MVP

- SEV-SNP and TDX verification exist in the protocol; see the tables above for
  what is missing around them.
- In the dstack simulator the launch measurement is a placeholder. Image
  measurement can be checked on real hardware only.
- The mock enclave seals its state with a fixed development key. The SEV-SNP
  service seals with a processor key; see its known gaps above.
- Bundle delivery over plain HTTP is a development mechanism, not a production
  key delivery design.
- All services run as root inside the guest.
- Docker stays in the guest image. The validator does not use it.
- The Barretenberg CRS is fetched at first start. A production image must ship
  it.

## Reproducibility

Both build steps have a check mode that builds twice from scratch and compares
the results byte for byte:

```sh
JOBS=4 ./deploy/dstack/build-binaries.sh --output <dir> --check
BIN_DIR=<dir>/a JOBS=4 ./deploy/dstack/build-image.sh --check
```

What fixes the inputs:

| Input | How it is fixed |
|---|---|
| Rust toolchain and system packages | `Dockerfile.project-toolchain`: base images by digest, Ubuntu snapshot archive |
| Source | git tree hash of the working tree; `SOURCE_DATE_EPOCH` is the commit time of `HEAD` |
| Paths, locale, time zone | fixed `/workspace`, `--remap-path-prefix`, `LANG=C`, `TZ=UTC` |
| Shared libraries in the image | copied from the toolchain image, not from the build host |
| dstack | pinned revision; Debian snapshot and component versions pinned by dstack |
| Overlay commit in the dstack clone | fixed author, date of the pinned dstack revision |

`build-image.sh --check` builds the `prod` flavor: the dstack reproducibility
check covers that flavor only. Build the `dev` flavor with `FLAVORS=dev` and
without `--check`.

The checks run both builds on one machine. They show that the build does not
depend on the build time or on the build directory. A build on a second machine
is the stronger evidence and has not been done.

The identity that a verifier compares is the `os_image_hash` printed by the
image build (`digest.txt` in the image directory).
