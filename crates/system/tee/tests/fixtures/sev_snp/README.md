# SEV-SNP test fixtures

| File | Source |
|---|---|
| `milan-report.bin`, `milan-vcek.der` | test data of the `sev` crate 6.0.0 (virtee, Apache-2.0), `tests/certs_data` |
| `milan-ark.der`, `milan-ask.der` | AMD Milan root and intermediate certificates from the `sev` crate 6.0.0, `src/certs/snp/builtin/milan`, converted from PEM to DER |
| `simulator-report.bin`, `simulator-cert-table.bin`, `simulator-ark.der` | dstack simulator (`--simulated-tee dstack-amd-sev-snp`), commit `c88f60e781a62d542e0843b333f18a48d1458b88`; the mock certificates use ECDSA P-384, not RSA-PSS |

## Files that are not in the repository

`milan-report-v5.bin` and `milan-vcek-v5.der` hold the chip identifier of one
server, so they are not committed. The tests of `src/sev_snp_v1.rs` include
them at compile time: copy both files into this directory to build and run the
tests of this crate with the `sev-snp` feature. A release build does not need
them.

## milan-report-v5.bin

An attestation report of version 5, taken on 2026-11-11 in a dstack VM on an
AMD EPYC 7413 (Milan) with SEV-SNP firmware API 1.58. Report data is zero. The
launch and current mitigation vectors (offsets 0x1f8 and 0x200) are not zero;
version 6.0.0 of the `sev` crate rejects such a report.

## milan-vcek-v5.der

The VCEK certificate of the processor that signed `milan-report-v5.bin`, from
the AMD key distribution service for the reported TCB (boot loader 4, TEE 0,
SNP 30, microcode 223). The host passes no certificate table to the guest, so
this certificate does not come with the report.
