# Intel DCAP 1.26 intent-bound Processor-CA fixture

This is a real Intel SGX Processor-CA quote captured under Gramine SGX on
2026-08-27. It is not synthetic evidence. Under the testnet policy it is an
accepted public-verifier fixture, but it is not exact-release evidence for a
future testnet bundle.

The enclave decoded `intent.bin`, derived
`RegistrationIntentV1::report_data()`, generated `quote.bin`, and checked the
signed 64-byte quote `REPORT_DATA` before returning it. The canonical inputs
and capture metadata are preserved as `policy.bin`, `intent.bin`,
`report-data.bin`, and `capture-input-v1.json`.

Capture facts:

- capture consensus timestamp: `1787850648`;
- quote: SGX v3, ECDSA-P256, type-5 Processor-CA, 4,600 bytes;
- quote SHA-256:
  `2c6114cf7a3da7a915bf5d510f6be59308deee9e916b146b9be4088d2b7b10a2`;
- `REPORT_DATA`:
  `5bc33573961e5f30a8b746c6894328eef94e34cd544ecff35554eb7894e4ee4d237bebfe208a8439ef0ffb7714d3d91c869905c5204c0ef32a11bf5fd058c4a1`;
- QVL package: `libsgx-dcap-quote-verify 1.26.100.1-noble1`;
- aggregate QVL status: `ConfigurationAndSWHardeningNeeded`;
- QE status: `UpToDate`;
- combined Platform/QE evaluation reference: `20`;
- QE supplemental evaluation reference: `0`;
- earliest expiration across all collateral: `1790380006` (PCK CRL).

The testnet matrix accepts `UpToDate`, `SWHardeningNeeded`, and
`ConfigurationAndSWHardeningNeeded` for Platform while QE remains exactly
`UpToDate`. Verification at the capture timestamp therefore returns an
accepted verdict preserving the exact Platform status and advisory IDs.
Verification at `1790380006` returns `CollateralExpired` (`0x0305`).

`accepted-verdict-v1.hex` stores the canonical accepted verdict bytes. The
public-verifier replay test compares the real result to these checked-in bytes
rather than only comparing Rust enum values.

`capture-provenance.json` records the exact QVL/QPL/QCNL package versions,
artifact hashes, source collateral ABI, and normalized component hashes. No
PCS subscription key, PCCS credential, host verdict, or network response is
stored in this fixture.

`SHA256SUMS` covers every immutable fixture input and the expected stable result.
This corpus closes the authentic intent-bound I1 replay case. I9 still captures
one fresh quote for the exact signed release on the same SGX server and measures
QVL/full-block execution there; this historical replay does not replace that
fresh release run.
