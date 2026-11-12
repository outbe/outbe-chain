# First run on real hardware

Checklist for bringing the Outbe validator image up on a server with AMD SEV-SNP
or Intel TDX. Nothing here has been run on real hardware yet: the steps come
from the dstack documentation and from the run in the dstack simulator. Record
the output of every check.

The guest image is the same for both platforms.

## AMD SEV-SNP host

Supported processor generations in the dstack verifier: Milan (EPYC 7003),
Genoa (9004), Turin (9005), Bergamo, Siena.

1. BIOS settings (the menu names depend on the board vendor):

   | Setting | Value |
   |---|---|
   | SMEE | Enable |
   | SEV Control | Enable |
   | SEV-ES ASID Space Limit | 99 |
   | SNP Memory (RMP Table) Coverage | Enabled |
   | SEV-SNP Support | Enable |

2. Host kernel, QEMU and OVMF with SEV-SNP support. dstack refers to the
   [AMDSEV](https://github.com/AMDESE/AMDSEV) project for this.

3. Checks on the host:

   ```sh
   test -e /dev/sev && echo "sev device: ok"
   sudo dmesg | grep -e SEV-SNP -e RMP
   cat /sys/module/kvm_amd/parameters/sev_snp     # must print Y
   ```

   `SEV-SNP: Memory for the RMP table has not been reserved by BIOS` in `dmesg`
   means the RMP setting is off or the board does not expose it.

## Intel TDX host

TDX needs a 5th generation Xeon Scalable or a Xeon 6 processor.

1. BIOS: enable TDX, the TDX SEAM loader, and SGX. The host needs SGX because
   the service that signs TDX quotes runs in an SGX enclave.

2. Host OS, kernel and QEMU from the
   [Canonical TDX guide](https://github.com/canonical/tdx). Do not replace its
   QEMU with a generic package.

3. Checks on the host:

   ```sh
   sudo dmesg | grep -i tdx                        # module initialized, its version
   test -e /dev/sgx_enclave && test -e /dev/sgx_provision && echo "sgx devices: ok"
   systemctl is-active qgsd pccs                   # quote generation and collateral cache
   cat /sys/module/kvm_intel/parameters/tdx        # must print Y
   ```

## On both hosts

1. Prepare the host and the VM network:

   ```sh
   ./deploy/dstack/host-setup.sh packages
   ./deploy/dstack/host-setup.sh tools
   # edit ~/.dstack-vmm/vmm.toml as README.md describes; omit [cvm.tee_simulator]
   ./deploy/dstack/host-setup.sh network-up
   ./deploy/dstack/host-setup.sh vmm-start
   ./deploy/dstack/host-setup.sh status
   ```

2. Copy the image built on the build machine and install it under its own name.
   Use the `dev` flavor for the first run: it has an SSH server. Its
   `os_image_hash` differs from the `prod` flavor.

3. Deploy one VM without `--simulated-tee` and with `no_instance_id: true` in
   the application manifest. Wait for `Boot Progress: done`.

4. In the guest, read the platform evidence:

   ```sh
   outbe-tee-enclave-mock --probe-attestation
   ```

   Expected on SEV-SNP: `tsm provider: sev_guest`, an evidence length of 1184,
   and a `sev-snp identity` line with the launch measurement and the host data.
   Expected on TDX: `tsm provider: tdx_guest`, a quote of a few kilobytes, and
   five `tdx identity` lines: `mrtd`, `rtmr0`, `rtmr1`, `rtmr2`, `mrconfigid`.

   On TDX record also:

   | Line | What it decides |
   |---|---|
   | `tdx quote: version=… tee_type=…` | the verifier accepts version 4 and TEE type 0x81 |
   | `tdx report: td_attributes=… debug=…` | `debug` must be `false` |
   | `tdx quote: canonical_len=… bytes_after_signature_data=…` | the node cuts the bytes after the signature data |
   | `tdx quote: pck certification data type=…` | must be 5, a PCK certificate chain |

   On SEV-SNP the probe also prints the facts that the protocol settings
   depend on. Record all of them:

   | Line | What it decides |
   |---|---|
   | `sev-snp report: version=… vmpl=…` | the verifier accepts versions 2, 3 and 5 and privilege level 0 only |
   | `sev-snp report: guest policy=… debug_allowed=… migration_agent_allowed=…` | both flags must be `false` |
   | `sev-snp report: signing_key=…` | must be 0 (VCEK); 1 (VLEK) is not supported |
   | `sev-snp report: tcb current=… reported=… committed=… launch=…` | the four values must be equal |
   | `sev-snp report: cpuid family=… model=…` (report version 3) | the processor generation, which selects the AMD root certificate |
   | `sev-snp certificate ASK/VCEK/ARK …` or `certificate table: EMPTY` | where the node takes the AMD certificates from: the host, or the AMD key distribution service |

   The probe needs a shell, so run it in the `dev` image. For the `prod` image
   read the image measurements from outside, as README.md describes in
   "Attestation evidence source".

5. Compare the measurement with the value that dstack computes for the image
   (`dstack-mr`, or `measurement.snp.cbor` / `measurement.tdx.cbor` in the image
   directory) for the same VM shape. This is the check that the simulator
   cannot do.

6. Deploy a second VM with the same manifest. Its host data (SEV-SNP) or
   `MRCONFIGID` (TDX) must equal that of the first VM.

7. The service keeps no state on a disk, so the platform needs no sealing key.

8. Run the four-validator network from README.md on the host with the
   development enclave first. This checks the host, the network and the image
   without attestation.

9. SEV-SNP: run the network in the attestation mode from the `prod` image.

   - Take `SNP_MEASUREMENT` from a VM of the `prod` image with the shape of the
     validators. Any application manifest gives the same value.
   - Take `SNP_ARK_SHA256` for the processor generation. For Milan it is
     `69d063b45344d26a2e94e1f4210de49ef555308287d4c174445c95639a540bcd`.
   - When the host passes no VCEK, download the VCEK certificate from the AMD
     key distribution service and pass it to `make-bundles.sh` as
     `SNP_VCEK_FILE`. Each processor has its own VCEK.
   - Generate the bundles with `TEE_MODE=sev-snp`, then deploy every VM with
     the manifest `<net>/app-compose.json` that the script wrote.

   Expected: block 1 registers four enclaves, every validator reports
   `mode: signer`, `enclave.state: ready` and `offerKeyReady: true`. Then set
   `upstream_url` for one VM and restart it: the validator must register again
   and report the same state, with no access to the VM.

10. TDX: run the network in the attestation mode from the `prod` image.

   - Take `TDX_MRTD`, `TDX_RTMR0`, `TDX_RTMR1` and `TDX_RTMR2` from a VM of the
     `prod` image with the shape of the validators.
   - Download the Intel collateral for the processor and pass it to
     `make-bundles.sh` as `TDX_COLLATERAL_FILE`.
   - Generate the bundles with `TEE_MODE=tdx`, then deploy every VM with the
     manifest that the script wrote.

   Expected: as in step 9.

11. Both hosts: run the mixed network. `TEE_MODE=mixed` takes the values of
    both platforms. Give `make-bundles.sh` the public address of the host of
    each validator and one `PORT_OFFSETS` number for each, and publish the VMs
    with `host-setup.sh expose` on each host.

12. Negative checks, on each platform:

    - no VM of the `prod` image accepts a connection on port 22;
    - a validator VM with a changed manifest (`dstack update-app-compose`)
      does not become ready, and the registry emits no new event;
    - a VM of the `dev` image with the manifest of the network does not start
      the service: `the image of this VM is not in the release list of its
      application manifest`;
    - a VM of the `dev` image with a manifest that lists the `dev` image starts
      the service, and `outbe-cli tee join` from it ends with a reverted
      registration;
    - the data disk of a stopped validator VM has no directory
      `validator-<i>/tee` and no executable file.

## Notes from the first AMD host (EPYC 7413, Ubuntu 26.04, kernel 7.0)

- The kernel parameter `iommu=pt` disables SEV-SNP on the host:
  `AMD-Vi: SNP: IOMMU disabled or configured in passthrough mode`. Remove it.
- `/dev/sev` belongs to root only. Give it to the `kvm` group with a udev rule:
  `KERNEL=="sev", GROUP="kvm", MODE="0660"`.
- The kernel has the `provider` attribute of the TSM report interface in every
  request directory, not in the interface directory as the dstack simulator.
- The host passes no certificate table to the guest (`auxblob` is empty).
  Download the VCEK from the AMD key distribution service one time:
  `https://kdsintf.amd.com/vcek/v1/Milan/<chip id, 128 hex digits>?blSPL=…&teeSPL=…&snpSPL=…&ucodeSPL=…`.
  The chip id is at offset 0x1A0 of the report. The four numbers are the bytes
  0, 1, 6 and 7 of the reported TCB at offset 0x180.
- Firmware with SEV-SNP API 1.58 returns reports of version 5. Version 6.0.0 of
  the `sev` crate does not parse them; the guest then stops in `dstack-prepare`
  with `Skipped bytes were expected to be zeroed`.

## Results on the two hosts (2026-11-11)

| Point | Result |
|---|---|
| AMD certificate chain from the host | the host passes none; the VCEK comes from the AMD key distribution service |
| Processor generation | Milan (EPYC 7413); report version 5 |
| Intel quote verification library and TDX quotes | the pinned library accepts the TDX quotes of the Xeon 6527P host |
| Dependence of the measurements on the manifest | none: the image measurements are equal for two manifests; `HOST_DATA` and `MRCONFIGID` follow the manifest hash as `make-bundles.sh` computes them |
| Dependence of the measurements on the VM shape | not measured; every run used 3 processors and 5 GiB |
