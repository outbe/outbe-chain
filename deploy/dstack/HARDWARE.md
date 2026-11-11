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
   | `tdx guest agent key: AVAILABLE fingerprint=…` | whether the service can seal its state; compare the fingerprint after a VM restart |

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
   | `sev-snp derived key: AVAILABLE … fingerprint=…` | whether the processor can seal the state of the service |

   Run the probe again after a VM restart. The `derived key` fingerprint must
   not change. Then deploy a VM from a different image: its fingerprint must
   differ.

5. Compare the measurement with the value that dstack computes for the image
   (`dstack-mr`, or `measurement.snp.cbor` / `measurement.tdx.cbor` in the image
   directory) for the same VM shape. This is the check that the simulator
   cannot do.

6. Deploy a second VM with the same manifest. Its host data (SEV-SNP) or
   `MRCONFIGID` (TDX) must equal that of the first VM.

7. Sealing key. Check that the guest can obtain a key bound to the VM
   measurement:

   - SEV-SNP: the derived key request of the `sev-guest` driver
     (`SNP_GET_DERIVED_KEY` on `/dev/sev-guest`);
   - TDX: the sealing key of the TDX module (`TDG.MR.KEY.GET`, modules 1.5.34
     and 2.0.18 or later). If the guest kernel does not expose it, the fallback
     is the dstack local key provider on the host.

8. Run the four-validator network from README.md on the host with the
   development enclave first. This checks the host, the network and the image
   without attestation.

9. SEV-SNP only: run the network in the attestation mode.

   - Take `SNP_MEASUREMENT` and `SNP_HOST_DATA` from the `sev-snp identity`
     line of the probe. Use a VM with the application manifest of the
     validators: another manifest gives another host data.
   - Take `SNP_ARK_SHA256` for the processor generation. For Milan it is
     `69d063b45344d26a2e94e1f4210de49ef555308287d4c174445c95639a540bcd`.
   - When the probe prints `certificate table: EMPTY` or no VCEK line, download
     the VCEK certificate from the AMD key distribution service and put it as
     `vcek.der` into `/dstack/persistent/outbe/sev-snp-certs` of every VM. Each
     processor has its own VCEK.
   - Generate the bundles with `TEE_MODE=sev-snp` and start the network as
     README.md describes.

   Expected: every service logs `MODE = confidential VM`, block 1 registers
   four enclaves, the network produces blocks. Then restart one VM: its service
   must restore the sealed state and the validator must take part again without
   `tee join`.

10. TDX only: run the network in the attestation mode.

   - Take `TDX_MRTD`, `TDX_RTMR0`, `TDX_RTMR1`, `TDX_RTMR2` and
     `TDX_MRCONFIGID` from the `tdx identity` lines of the probe. Use a VM with
     the application manifest and the shape of the validators.
   - Download the Intel collateral for the processor and put it as
     `collateral.json` into `/dstack/persistent/outbe/tdx-collateral` of every
     VM.
   - Generate the bundles with `TEE_MODE=tdx` and start the network.

   Expected: every service logs `MODE = confidential VM`, block 1 registers
   four enclaves, the network produces blocks. A VM restart keeps the sealed
   state only when the guest agent key fingerprint is stable across restarts.

## Notes from the first AMD host (EPYC 7413, Ubuntu 26.04, kernel 7.0)

- The kernel parameter `iommu=pt` disables SEV-SNP on the host:
  `AMD-Vi: SNP: IOMMU disabled or configured in passthrough mode`. Remove it.
- `/dev/sev` belongs to root only. Give it to the `kvm` group with a udev rule:
  `KERNEL=="sev", GROUP="kvm", MODE="0660"`.
- Firmware with SEV-SNP API 1.58 returns reports of version 5. Version 6.0.0 of
  the `sev` crate does not parse them; the guest then stops in `dstack-prepare`
  with `Skipped bytes were expected to be zeroed`.

## Open points to settle on hardware

- Whether the host passes the AMD certificate chain to the guest (`auxblob` is
  not empty). If it does not, the node must fetch the VCEK certificate from the
  AMD key distribution service before it registers.
- Which processor generation the server has. The attestation policy pins one
  AMD root certificate, and each generation has its own.
- Whether the pinned Intel quote verification library in this repository
  accepts TDX quotes.
- How the launch measurement depends on the VM shape (vCPU count, memory).
  Every shape needs its own policy rule.
