# Outbe validator on dstack

This directory builds a [dstack](https://github.com/Dstack-TEE/dstack) guest
image that runs an Outbe validator, and prepares a four-validator development
network in which every validator runs in its own dstack VM.

Status: MVP. One image runs a validator on Intel TDX and on AMD SEV-SNP. A
four-validator network ran from the `prod` image on each platform and with two
validators on each platform in one network (2026-11-11). The development
network uses the `gramine-direct-dev` attestation mode and the hardware-free
development enclave.

What the hardware modes protect: the offer key of the network stays in the
memory of the VMs. The operator of a validator cannot read it from a running
network. See [Protection of the offer key](#protection-of-the-offer-key) for
the rules and for what is not covered.

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
| `/usr/local/bin/` | `outbe-chain`, `outbe-radicle`, `outbe-cli`, `outbe-feeder`, `outbe-tee-enclave` (hardware modes), `outbe-tee-enclave-mock`, `outbe-tee-enclave-local-e2e` |
| `/usr/local/lib/outbe/lib/` | shared libraries the binaries need and Debian does not ship |
| `/usr/local/lib/outbe/provision.sh` | first-boot fetch of the validator data bundle |
| `/usr/local/lib/outbe/run-role.sh` | launches the enclave service, the Radicle sidecar and the node |
| `outbe-{provision,enclave,radicle,node}.service` | systemd units, started by the application manifest |

The image is the same for every validator and holds every program and launch
script. Validator-specific data (genesis, keys, bootnodes) arrives in a bundle
at first boot.

Changes to the dstack tree:

- `patches/0001-unencrypted-ext4-data-disk.patch` adds
  `dstack.storage_encrypted=false dstack.storage_fs=ext4` to the kernel command
  line. The command line is fixed at image build time and is measured.
- `rootfs/` is copied over `os/mkosi/mkosi.skeleton/`.

The pinned dstack revision is `DSTACK_COMMIT` in `build-image.sh`.

## Guest layout

| Path | Storage | Content |
|---|---|---|
| `/dstack/persistent/outbe/` | data disk, not encrypted, survives restarts | unpacked bundle, chain data, Radicle state, node logs |
| `/dstack/persistent/outbe/validator-<i>/tee/` | data disk | state of the development enclave (`tee_mode: dev`) only |
| `/run/outbe/tee/` | tmpfs, memory of the VM | state of the hardware service; empty after every start of the service |
| `/dstack/persistent/outbe/crs/` | data disk | Barretenberg CRS file |
| `/dstack/user_config` | host share | user config, see section 6 |
| `/dstack/.host-shared/app-compose.json` | host share | application manifest; the hardware service checks it against the launch identity of the VM |

The data disk is operator input. A host can read and change it. Chain data
needs no protection: a host that changes it makes its own validator faulty,
and consensus handles that. Nothing on the data disk is executed, and the
hardware service keeps nothing on it.

In the hardware modes the offer key is never written to a disk. After a restart
of the VM, or of the service, the validator registers again and receives the
key from the network (see [Rejoin after a VM restart](#rejoin-after-a-vm-restart)).

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
`prod` flavor (`FLAVORS=prod`, or `FLAVORS="dev prod"` for both) for real
hardware: it has no SSH server and no shell access. The two flavors have
different measurements.

## 5. Create the VMs

```sh
for i in 0 1 2 3; do
    cat > user-config-$i.json <<JSON
{"validator_index": $i,
 "bundle_url": "http://10.77.0.1:18090/validator-$i.tgz",
 "crs_url": "http://10.77.0.1:18090/bn254_g1.dat",
 "tee_mode": "dev"}
JSON
    dstack deploy --name outbe-v$i --image outbe-validator-dev \
        --compose deploy/dstack/app-compose.json --user-config user-config-$i.json \
        --vcpu 3 --memory 5G --disk 40G --net bridge \
        --simulated-tee dstack-amd-sev-snp
done
```

Drop `--simulated-tee` on a host with real SEV-SNP or TDX. For a network in a
hardware mode do section 6 first and deploy with the `prod` image, the manifest
`<net>/app-compose.json` and `"tee_mode": "confidential-vm"`.

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

`make-bundles.sh` runs `outbe-genesis-cli create` and writes one data bundle
per validator: the keys, the genesis, the bootnode list, the network descriptor
and, when given, attestation collateral. A bundle holds no program and no
script.

In a hardware mode (`TEE_MODE=sev-snp`, `tdx` or `mixed`) the script also
writes `<net>/app-compose.json`: the application manifest of every VM of the
network, with the section `outbe_release` (see
[Protection of the offer key](#protection-of-the-offer-key)). Deploy every VM
with exactly this file, so generate the network before you create the VMs. The
genesis names the addresses of the validators: give `make-bundles.sh` addresses
that are known before the VMs exist, for example the public address of the host
with one `port_offset` for each validator (see
[Validators on different hosts](#validators-on-different-hosts)).

The launch scripts are part of the VM image
(`rootfs/usr/local/lib/outbe/provision.sh` and `run-role.sh`), so the platform
measures them. They read the data disk and the dstack user config as data only:
they check every value, execute nothing from the data disk, and refuse a bundle
that contains anything but regular files and directories. The user config
selects:

| Field | Meaning |
|---|---|
| `validator_index` | number of the validator, 0 to 255 |
| `bundle_url` | where the VM fetches its data bundle |
| `tee_mode` | `dev`: the development enclave. `confidential-vm`: the service that attests with the hardware; it detects SEV-SNP or TDX itself |
| `crs_url` | optional: where the VM fetches the Barretenberg CRS, which the node otherwise downloads from the internet |
| `external_address` | optional: IPv4 address that the validator advertises; default is the address of the VM |
| `upstream_url` | optional: RPC of a healthy node. A validator in the `confidential-vm` mode that starts again joins the network through this node and catches up from it |
| `port_offset` | optional: 0 to 99, added to every peer-to-peer port |

The user config is not measured. A wrong value cannot run other code: it selects
between the two enclave programs of the image, and a network in a hardware
attestation mode does not admit the development enclave.

Each VM fetches its bundle, unpacks it to `/dstack/persistent/outbe` and starts
the enclave, the Radicle sidecar and the node. The founders run the DKG ceremony
together: all four nodes must be up within the same 120 seconds.

## 7. Check the network

The `prod` image gives no shell access. Read the state from outside, through
the RPC port of a VM:

```sh
curl -s -X POST -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"outbe_consensusStatus","params":[]}' \
    http://<vm-ip>:8545 | jq '{mode: .result.mode, enclave: .result.enclave}'
```

A healthy validator reports `mode: signer`, `enclave.state: ready` and
`offerKeyReady: true`. The TEE registry (`0x…EE0A`) emits its events in block 1
(one registration for each founder) and in the block of every later
registration (`eth_getLogs`).

What an operator can see without a shell:

| Channel | Content |
|---|---|
| RPC, port 8545 | chain data, consensus mode, enclave state |
| `dstack logs <vm-id>` | console of the VM: boot and unit start messages. The output of the Outbe services is not on the console of the `prod` image |
| port 8090 of the VM | dstack page with the image hash, the manifest hash and, on TDX, the measurements |
| the data disk, attached read-only on the host | node log files in `validator-<i>/logs` |

To read the data disk of a stopped VM on the host:

```sh
sudo modprobe nbd max_part=8
sudo qemu-nbd -r -c /dev/nbd0 ~/.dstack-vmm/vm/<vm-id>/hda.img
sudo mount -o ro,noload /dev/nbd0p1 /mnt
```

## 8. Restart a validator

```sh
dstack stop <vm-id>
dstack start <vm-id>
```

With `tee_mode: dev` the development enclave restores its state from the data
disk, and the node continues without operator action.

With `tee_mode: confidential-vm` the service starts without the offer key. Set
`upstream_url` in the user config before the restart
(`dstack update-user-config`). The launch script then registers the validator
again and the node catches up; see
[Rejoin after a VM restart](#rejoin-after-a-vm-restart). The other three
validators keep producing blocks while one is down.

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
verifier checks and the certificates that the host supplies. In a TDX guest it
prints `MRTD`, `RTMR0` to `RTMR2` and `MRCONFIGID` and the quote layout facts.
`HARDWARE.md` lists the lines and their meaning.

The probe needs a shell, so it serves the `dev` image. For the `prod` image
read the image measurements from outside: on TDX from the page on port 8090 of
a VM (`/prpc/Info`, field `tcb_info`), on SEV-SNP from the attestation report
inside the certificate that the same page returns (launch measurement at
offset `0x90` of the report). The image measurements depend on the image and on
the VM shape, not on the application manifest: a VM with any manifest gives
them.

Set `no_instance_id: true` in the application manifest of a network that uses
SEV-SNP or TDX attestation. Without it the host data (SEV-SNP) or `MRCONFIGID`
(TDX) contains a per-VM instance id and differs between validators.

## SEV-SNP attestation mode

The protocol has an attestation mode for AMD SEV-SNP,
`AttestationMode::SevSnpRequired`. The policy pins three values:

| Value | Policy field | Source |
|---|---|---|
| launch measurement of the VM image | `mrenclave` of a rule, as a keccak projection | a VM of the image and the VM shape, see "Attestation evidence source" |
| host data | `mrsigner` of a rule | `make-bundles.sh` computes it from the application manifest; equal for all VMs with `no_instance_id: true` |
| SHA-256 of the AMD root certificate (ARK, DER) | `intel_root_der_hash` | the ARK of the processor generation. The verifier accepts the AMD roots of Milan, Genoa and Turin only, whatever the policy names |

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
| Evidence assembly in the node at network start | done; ran on hardware |
| Evidence assembly in `outbe-cli tee join` | done; ran on hardware |
| Service runtime in a confidential VM: report source, own code identity, release rules from the measured manifest | done; ran on hardware |
| Renewal, binding replacement, measurement transition | outside the first scope; explicit errors |

The verifier accepts report versions 2, 3 and 5. Version 5 comes from firmware
with SEV-SNP API 1.58; it needs version 8.0.0 of the `sev` crate, in this
repository and in the dstack guest tools (patch
`patches/0002-sev-crate-8-report-version-5.patch`).

A four-validator network ran in this mode from the `prod` image on one AMD
EPYC 7413 host (2026-11-11): block 1 registered four enclaves with reports of
the processor, the validators produced blocks, and a restarted VM registered
again and received the offer key without operator access to the VM.

The verifier accepts the AMD certificate chain only (RSA-PSS, AMD roots). It
rejects the mock certificates of the dstack simulator. An end-to-end run
therefore needs real hardware.

### Service in a confidential VM

The image carries `outbe-tee-enclave`: the production entry point of the
service without development code. It is built with the features `sev-snp` (the
confidential VM backend for both platforms) and `native-dcap` (the Intel quote
verification library, which verifies TDX quotes). Start it with:

```sh
outbe-tee-enclave --platform auto \
    --network-descriptor <dir>/network-descriptor-v1.bin \
    --app-manifest /dstack/.host-shared/app-compose.json \
    --socket 127.0.0.1:17000 --tee-dir /run/outbe/tee --chain-id <hex32>
```

`--platform auto` takes the platform that the kernel reports; `sev-snp` and
`tdx` name it. The service takes from the platform what an SGX enclave takes
from Gramine:

| Need | Source in the VM |
|---|---|
| own code identity | launch identity of an attestation report read at startup |
| registration evidence | report or quote from the kernel `configfs-tsm` interface |
| trusted network descriptor and release rules | the application manifest, checked against the launch identity |
| sealing key | random for each process; the state files do not open after a restart |

The service does not start when the manifest file is not the one that the
launch identity binds, when the manifest does not list the image of this VM, or
when the network descriptor file is not the one that the manifest names.

The launch script of the image starts the service this way when the user
config has `"tee_mode": "confidential-vm"`. Pass the VCEK file to
`make-bundles.sh` as `SNP_VCEK_FILE` to add it to the bundles.

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

## TDX attestation mode

The protocol has an attestation mode for Intel TDX,
`AttestationMode::TdxRequired`. TDX evidence has the layout of SGX DCAP
evidence: a quote of version 4 and eight Intel collateral components. The
verifier is the DCAP pipeline with the TDX profile and the TDX entry point of
the pinned Intel quote verification library. The policy pins:

| Value | Policy field | Source |
|---|---|---|
| `MRTD`, `RTMR0`, `RTMR1`, `RTMR2` of the trust domain | `mrenclave` of a rule, as one keccak projection | `--probe-attestation` |
| `MRCONFIGID` | `mrsigner` of a rule, as a keccak projection | `make-bundles.sh` computes it from the application manifest; equal for all VMs with `no_instance_id: true` |
| lowest TCB evaluation data number | `minimum_tcb_evaluation_data_number` | the Intel TCB info of the processor |

`RTMR3` is not part of the identity. `RTMR0` depends on the VM shape, so every
shape needs its own rule. The Intel root is the SGX root that DCAP uses; the
verifier accepts this root only, whatever the policy names. A debug trust
domain is rejected.

```sh
outbe-chain tee genesis --input seeded.json --output genesis.json \
    --mode tdx-required \
    --tdx-mrtd <96 hex digits> --tdx-rtmr0 <96> --tdx-rtmr1 <96> --tdx-rtmr2 <96> \
    --tdx-mrconfigid <96 hex digits> \
    --tdx-minimum-tcb-evaluation-data-number <number>
```

`outbe-genesis-cli` takes the same values as `tee.mode: tdx-required` with the
keys `tdx_mrtd`, `tdx_rtmr0`, `tdx_rtmr1`, `tdx_rtmr2`, `tdx_mrconfigid` and
`tdx_minimum_tcb_evaluation_data_number`. `make-bundles.sh` takes the image
values with `TEE_MODE=tdx` and computes `MRCONFIGID`. The mode is allowed on
devnet and testnet.

The same image and the same launch script serve both platforms: the service
starts with `--platform auto` and takes the platform that the kernel reports.
Pass the collateral file to `make-bundles.sh` as `TDX_COLLATERAL_FILE`.

### Intel collateral

The node adds the Intel collateral to the quote. The PCK certificate chain
comes from the quote. The node reads the other seven components from
`collateral.json` in the directory `OUTBE_TDX_COLLATERAL_DIR` (the bundles set
it to `/dstack/persistent/outbe/tdx-collateral`). An operator downloads the
collateral for the processor from the Intel provisioning certification service
and refreshes it before it expires. `crates/system/tee/src/tdx_host.rs`
describes the fields of the file.

### Status of the TDX mode

| Part | State |
|---|---|
| Evidence format, policy projection, genesis and CLI | done |
| Verification of a quote (feature `native-dcap`) | done; tested with a real Intel TDX quote and its collateral |
| Evidence assembly in the node and in `outbe-cli tee join` | done; ran on hardware |
| Service runtime: quote source, own code identity, release rules from the measured manifest | done; ran on hardware |
| Renewal, binding replacement, measurement transition | outside the first scope; explicit errors |

A four-validator network ran in this mode from the `prod` image on one Intel
Xeon 6527P host (2026-11-11), with the restart of a VM as in the SEV-SNP mode.

Quotes of version 5 and the TDX-only verification results
(`TD_RELAUNCH_ADVISED`) are rejected.

## Mixed network: SEV-SNP and TDX validators together

`AttestationMode::ConfidentialVmRequired` admits the evidence of both
platforms in one network. Its policy pins two roots, the Intel root and the AMD
root (`amd_root_der_hash`, present in this mode only), and holds one
measurement rule for each platform and VM shape. Each piece of evidence is
verified by its kind.

```sh
outbe-chain tee genesis --input seeded.json --output genesis.json \
    --mode confidential-vm-required \
    --snp-measurement … --snp-host-data … --snp-ark-sha256 … \
    --tdx-mrtd … --tdx-rtmr0 … --tdx-rtmr1 … --tdx-rtmr2 … --tdx-mrconfigid … \
    --tdx-minimum-tcb-evaluation-data-number …
```

`make-bundles.sh` takes the values of both platforms with `TEE_MODE=mixed`.

A network of two validators on an Intel host and two on an AMD host ran in
this mode from the `prod` image (2026-11-11), with the restart of one VM on
each platform.

### Rejoin after a VM restart

A service that starts again has no offer key and a fresh identity. In the
confidential VM modes its validator registers again while the old lease is
live: the registry requires the same NodeHost key, a fresh enclave id and
binding id, and fresh hardware evidence, and it retires the previous enclave.
A jailed validator cannot rejoin. The SGX mode keeps its rule: a live binding
is renewed.

The image has no shell access, so the launch script does the rejoin. When the
service of this boot is new and the node data directory holds an earlier
registration, the script runs `outbe-cli tee join` through `upstream_url` of
the user config and then starts the node with that upstream. A restart of the
service alone restarts the node, with the same result.

A request to join belongs to one service process. When the VM or the service
restarts during an attempt, or the network rejects an attempt, the node data
directory keeps the records of that attempt (`replacement-*`). The script
removes them before the first attempt of a new service process: the old
process does not exist, so its attempt cannot continue.

The launch scripts write nothing to a key file on the data disk. A forced stop
of the VM, or a power loss, therefore cannot damage a key.

### Validators on different hosts

Two validators behind one public address need different peer-to-peer ports.
Set `port_offset` in the user config of a VM and the same number in
`PORT_OFFSETS` of `make-bundles.sh`, and give `make-bundles.sh` the public
address of the host of each validator. Then publish the VMs on each host:

```sh
./deploy/dstack/host-setup.sh expose <public-ip> <peer-ip>[,<peer-ip>…] \
    <vm-ip>:<offset> [<vm-ip>:<offset> …]
```

| Port of a VM with offset N | Open to |
|---|---|
| RPC, public port 8545+N | everyone |
| Radicle 8776+N | everyone |
| consensus 30400+N, reth 30303+N, discv5 31303+N | the listed peer addresses |

`host-setup.sh unexpose` removes the rules. They do not survive a host reboot.

`host-setup.sh firewall` installs an inbound firewall for the host itself: SSH
and the VM bridge only. It stays after a reboot. The published ports of the
validators are forwarded to the VMs and stay reachable. `firewall-try` loads
the same rules for two minutes, to check that SSH still works.

## Protection of the offer key

The offer key exists in the memory of the validator VMs only. The processor
keeps the host out of that memory. The remaining way to the key is the service
itself: it hands the key to a joining node. The service must not take the rules
for that from its node, because the operator controls the node, the genesis
that the node reads and the attestation policy in the chain state.

The service takes the rules from the application manifest of its VM. dstack
binds the SHA-256 hash of `app-compose.json` to the launch identity: on TDX
`MRCONFIGID` holds the hash, on SEV-SNP `HOST_DATA` is the hash of a small
document that names it. The service checks the manifest file against its own
launch identity and reads the section that `make-bundles.sh` writes:

```json
"outbe_release": {
  "network_descriptor_sha256": "<64 hex digits>",
  "sev_snp_measurements": ["<96 hex digits>"],
  "tdx_images": [{"mrtd": "…", "rtmr0": "…", "rtmr1": "…", "rtmr2": "…"}]
}
```

| Rule | Effect |
|---|---|
| A VM receives the key only if its evidence chains to the root of AMD or of Intel that is compiled into the verifier | an operator cannot sign evidence with a key hierarchy of his own |
| and its image is in the list of the manifest of the releasing VM | a VM with another image, for example one with a shell, receives nothing |
| and its manifest is the manifest of the releasing VM | a VM that starts other programs, or names other rules, receives nothing |
| The service accepts the network descriptor that the manifest names | the service follows the finality of the real network only |
| The service keeps no state on a disk | a VM with another manifest finds nothing to open |
| The service in a VM does not export the key for an image upgrade | a new image is a new network |

The lists hold the images of both platforms and of every VM shape, so a
service on one platform admits a VM on the other. The image measurements do
not depend on the manifest, and the manifest does not hold its own hash.

Checked on hardware with the `prod` image (2026-11-11): a VM has no open SSH
port; a VM with a changed manifest receives no key; a VM of the `dev` image
with a manifest that does not list it does not start the service; the same VM
with a manifest that lists it has its registration reverted by the network; a
stopped VM has no service state and no executable file on its data disk; a
validator whose VM was stopped by force after a rejected attempt returns to
the network when its manifest is restored.

Not covered:

- **Network founding.** The founders create the offer key in a ceremony before
  block 1. The ceremony authenticates the keys of a participant by a signature
  of that participant only, not by hardware evidence. The operator of a founder
  can take part with plain software and learn the key. The same holds in the
  SGX mode. The fix is known: each participant attaches a hardware report that
  binds its ceremony keys, and each service checks the reports against its
  manifest before it deals.
- **Offer contents.** A node executes offers in the service. A node on a
  private chain can ask its service to decrypt an offer.
- **Guest kernel hardening.** The root file system is read-only with
  dm-verity, and its hash is in the measured kernel command line. The data disk
  and the tmpfs mounts have no `noexec`; kernel lockdown is not active; module
  loading is not disabled; there is no IMA policy. Nothing in the image runs a
  file from the data disk, and the `prod` image has no way in.
- Periodic attestation after registration, a minimum firmware version in the
  SEV-SNP policy, and the recomputation of the expected measurements from the
  build artifacts.
- Attacks on the hardware and side channels.

The hardware modes are allowed on devnet and testnet chain ids only.

## Limits of this MVP

- SEV-SNP and TDX verification exist in the protocol; see the tables above for
  what is missing around them.
- In the dstack simulator the launch measurement is a placeholder. Image
  measurement can be checked on real hardware only.
- The mock enclave seals its state with a fixed development key.
- The `prod` image sends the output of the Outbe services to the journal inside
  the VM only. There is no metrics port and no log shipping to the host.
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

Result: two builds from scratch of one source gave the same `os_image_hash` for
both flavors and byte-identical binaries (checked for the images of
2026-11-11 and 2026-11-12).

The checks run both builds on one machine. They show that the build does not
depend on the build time or on the build directory. A build on a second machine
is the stronger evidence and has not been done.

The identity that a verifier compares is the `os_image_hash` printed by the
image build (`digest.txt` in the image directory).
