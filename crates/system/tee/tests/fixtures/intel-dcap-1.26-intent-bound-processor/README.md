# Intel DCAP 1.26 intent-bound Processor-CA fixture

This is a real Intel SGX Processor-CA quote captured under Gramine SGX on
2026-07-31. It is not synthetic evidence. Under the testnet policy it is an
accepted public-verifier fixture, but it is not exact-release evidence for a
future testnet bundle.

The enclave decoded `intent.bin`, derived
`RegistrationIntentV1::report_data()`, generated `quote.bin`, and checked the
signed 64-byte quote `REPORT_DATA` before returning it. The canonical inputs
and capture metadata are preserved as `policy.bin`, `intent.bin`,
`report-data.bin`, and `capture-input-v1.json`.

Capture facts:

- capture consensus timestamp: `1785491440`;
- quote: SGX v3, ECDSA-P256, type-5 Processor-CA, 4,600 bytes;
- quote SHA-256:
  `f7626ab64e0c2d182984390e9bab26d20f7b96af10dab20b43e6559edc118b1d`;
- `REPORT_DATA`:
  `9255eaef4a7f3c555965344821b231e72361d18ce2acd23c91388fa7c32f021469d0f3a33012fd8bc466ad156204b2512a4500c63c89a5490a726a7f0387a1d5`;
- QVL package: `libsgx-dcap-quote-verify 1.26.100.1-noble1`;
- aggregate QVL status: `ConfigurationAndSWHardeningNeeded`;
- QE status: `UpToDate`;
- combined Platform/QE evaluation reference: `19`;
- QE supplemental evaluation reference: `0`;
- earliest expiration across all collateral: `1787808799` (PCK CRL).

The testnet matrix accepts `UpToDate`, `SWHardeningNeeded`, and
`ConfigurationAndSWHardeningNeeded` for Platform while QE remains exactly
`UpToDate`. Verification at the capture timestamp therefore returns an
accepted verdict preserving the exact Platform status and advisory IDs.
Verification at `1787808799` returns `CollateralExpired` (`0x0305`).

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
