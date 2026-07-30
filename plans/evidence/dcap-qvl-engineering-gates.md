# DCAP QVL engineering-gate evidence

Date: 2026-07-30

Repository base: `d44bf85` (`main`)

Host: `x86_64-unknown-linux-gnu`, Rust `1.96.0`

This note records the throwaway prototype used to close decision-map questions
#17 and #18. It is evidence for the selected design, not a substitute for the
x86_64 real-hardware release gates listed below.

## Selected QVL profile

- Crate: exact `dcap-qvl = 0.5.2`
- crates.io checksum:
  `92a14fb8954c867d6855e44d98eab18e769816357738406691ebe60d8fdd005d`
- upstream source commit:
  `31a32a44de4cf68cb50c079e5bfd5348e4e6f4d5`
- production features:
  `default-features = false`, `std`, `ring`, `default-x509`
- excluded in production: `report`, HTTP/PCCS client, `rustcrypto`,
  `danger-allow-tcb-override`, language bindings and every fail-open path

Ring is selected as the sole production backend. In the local differential
run it produced the same complete reports as RustCrypto for the bundled SGX
and TDX fixtures and was approximately twice as fast. Shipping only one
backend removes feature-selected behavior from the consensus binary.

`dcap-qvl` remains a cryptographic core, not the protocol authority. An Outbe
wrapper must decode the canonical evidence, construct the in-memory QVL
collateral deterministically, call the pinned verifier with the consensus block
timestamp and map the outcome into stable Outbe verdict codes.

## Fixture results

Bundled upstream fixture hashes:

| Fixture | Bytes | SHA-256 |
|---|---:|---|
| SGX quote | 4,600 | `f8b81014b6e443609746822194910f5dc1c92c322fa0584298d1e33e505ca3b5` |
| SGX collateral JSON wrapper | 14,050 | `bdd694bbe50f3a2a1cfe12f9e2bd83125921107a368edcf10780a5523b8501ce` |
| TDX quote | 5,006 | `c42f9164325024bca2757bc8819b11879a0a369132ea4e2b7c85df4805ea72db` |
| TDX collateral JSON wrapper | 16,072 | `b0a5f5fd620a8881b1eda45261fdf30dd930b49aff93231556645c81fcb4c0bc` |
| Intel root DER | 659 | `44a0196b2b99f889b8e149e95b807a350e7424964399e885a7cbb8ccfab674d3` |

The SGX fixture is a real Processor-CA quote with FMSPC `00a067110000`,
PCE ID `0000` and a 16-byte PPID. QVL returns:

- platform: `ConfigurationAndSWHardeningNeeded`;
- QE: `UpToDate`;
- aggregate: `ConfigurationAndSWHardeningNeeded`.

It is therefore a valid cryptographic fixture and a negative Outbe admission
fixture. The TDX fixture returns `UpToDate`, but V1 rejects it before QVL policy
admission because production accepts SGX quote v3 only.

The QVL time window is inclusive at both endpoints. The tested SGX collateral
accepted `not_before = 1750330571` and `not_after = 1752919278`, and rejected
one second outside either boundary. Outbe leases still end at least one hour
before the authenticated collateral deadline, so this inclusive QVL boundary
does not weaken the lease margin.

## Required wrapper checks demonstrated by the prototype

Pinned upstream QVL accepted both:

- one byte appended after an otherwise complete quote;
- one byte appended inside the declared quote authentication-data region after
  increasing that region's length.

The prototype's exact SGX-v3 length grammar rejected both. Production must
perform that strict check before QVL and must require:

- exact quote v3/SGX/P-256/Intel QE vendor/type-5 certification data;
- complete consumption of both the outer quote and inner authentication data;
- exact canonical DER components and exact signed-JSON bytes;
- TCB Info schema exactly v3, regardless of the reported Platform status;
- Platform status `UpToDate` or `SWHardeningNeeded`, with authenticated
  advisory IDs preserved in the stable verdict;
- QE status exactly `UpToDate`;
- rejection of configuration-needed, out-of-date and revoked Platform or QE
  results, and of `SWHardeningNeeded` for QE;
- both signed documents' minimum TCB evaluation data number;
- verified PCK FMSPC and PCE ID equal the signed TCB Info values;
- exact policy measurement and pinned Intel root;
- stable Outbe reject codes, never consensus-visible upstream error strings.

The public `dcap-qvl` parsing APIs are sufficient for the adapter and the
additional PCK-extension checks. A fork is not the default plan. If
implementation exposes an unavoidable API gap, a minimal vendored patch needs
a separate source-diff and conformance review.

## Local timing and memory

Release build, 40 iterations:

| Case | Mean |
|---|---:|
| SGX Ring valid cryptographic verification | 0.67–0.69 ms |
| SGX RustCrypto valid cryptographic verification | 1.35–1.37 ms |
| SGX Ring invalid at header policy | 0.046–0.047 ms |
| SGX Ring invalid at late signature verification | 0.66–0.70 ms |

The Ring-only prototype process peaked at approximately 3.5 MiB RSS after
build. Compiler RSS is intentionally excluded from this runtime figure.

The current project also passed:

```text
cargo test -p outbe-tee --features dcap --offline
27 passed; 0 failed
```

That test confirms current compatibility but not consensus readiness: the
current dependency still enables upstream defaults, and
`verify_dcap_signature` still reads environment, filesystem and wall clock.

## Normative caps and gas

The prototype found that the proposed 1-MiB aggregate evidence cap can produce
a transaction whose worst-case calldata intrinsic gas plus protocol precharge
exceeds the 30-million steady block limit. The normative V1 values are
therefore:

- quote: 16 KiB;
- each of the eight canonical collateral components: 896 KiB;
- complete canonical `AttestationEvidenceV1`: 896 KiB;
- non-evidence framing of an evidence-bearing registry call: 16 KiB;
- active measurement rules: 64;
- `TeeBootstrapV2`: 1,310,720 bytes;
- block 1: 500,000,000 gas;
- every block from height 2: 30,000,000 gas.

Caps are checked from declared lengths before allocation, decoding, state
access or cryptographic work. All sums and products use checked `u64`.

The normative QVL formula remains:

```text
QVL_DCAP =
    1,500,000
  + 6 * evidence_len
  + 120,000 * 9 certificates
  + 180,000 * 2 CRLs
  + 160,000 * 2 signed JSON documents
  + 10,000 * active_rule_count
```

At the 896-KiB evidence cap and 64 rules:

- `QVL_DCAP = 9,405,024`;
- worst-case `registerEnclave`, with 16-KiB framing and all non-zero calldata,
  costs `28,768,784`, leaving `1,231,216` gas;
- the more expensive `replaceEnclaveBinding` costs `29,133,784`, leaving
  `866,216` gas;
- the 32-participant dense `OST3` precharge is `309,931,488`;
- maximum all-non-zero `OST3` intrinsic gas is `20,992,520`;
- combined `OST3` is `330,924,008`, leaving `169,075,992` of the bootstrap
  block for the other four mandatory system transactions.

Batch-local collateral deduplication reduces encoded bytes only. QVL gas is
charged for every participant's logical evidence dimensions.

## Release evidence still required by implementation

These are acceptance criteria of the implementation tasks, not open product
decisions:

- a real Platform-CA SGX fixture in addition to the Processor-CA fixture;
- current PCS fixtures with large real CRLs;
- cap-minus-one/cap/cap-plus-one and allocation-before-decode tests;
- byte-stable verdict vectors across supported x86_64 validator builds;
- valid, invalid-early, invalid-late and dense 32-validator benchmarks on the
  slowest supported x86_64 validator class;
- fail-not-skip SGX/DCAP end-to-end release CI.

ARM TEE and aarch64 are not production targets for V1 and therefore do not
carry fixture, verdict or release-gate requirements.
