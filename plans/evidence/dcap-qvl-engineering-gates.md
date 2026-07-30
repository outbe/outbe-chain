# DCAP QVL engineering-gate evidence

Date: 2026-07-30

Repository base: `d44bf85` (`main`)

Native amendment base after rebasing onto `777624e`: `5fb89ce` (`main`)

Host: `x86_64-unknown-linux-gnu`, Rust `1.96.0`

This note records the throwaway prototype used to investigate decision-map
questions #17 and #18 and the later native Intel amendment. It is engineering
evidence, not a substitute for the x86_64 real-hardware release gates listed
below.

## Selected production verifier boundary

V1 follows the Secret Network-style native Intel boundary because every
production validator and full node is required to run on x86_64 Intel SGX:

- Intel native QVL runs in QvE mode over canonical, self-contained evidence
  and an explicit consensus block timestamp;
- the matching pinned TVL verifies the QvE report and identity inside the Outbe
  attestation enclave;
- an untrusted host `qv_result`, local PCCS response, cache entry or clock is
  never consensus authority;
- the Outbe wrapper enforces exact grammar, TCB Info schema v3, separate
  Platform/QE policy, measurement policy and stable verdict codes over the same
  authenticated bytes;
- missing or mismatched QVL, QvE, TVL or native dependencies fail closed.

I1 starts with a narrow native feasibility gate: select one supported Intel
DCAP release, record exact package/source provenance and binary digests, and
prove QvE-mode plus TVL verification in the current Outbe/Gramine runtime with
one fixed real SGX vector. The documents intentionally do not invent a native
version before that executable gate.

The stable result must not contain QvE nonce/report bytes, addresses, native
error strings or other run-specific data. Those are local verification
artifacts; only the canonical Outbe verdict affects consensus.

## Superseded pure-Rust prototype profile

- Crate: exact `dcap-qvl = 0.5.2`
- crates.io checksum:
  `92a14fb8954c867d6855e44d98eab18e769816357738406691ebe60d8fdd005d`
- upstream source commit:
  `31a32a44de4cf68cb50c079e5bfd5348e4e6f4d5`
- prototype features:
  `default-features = false`, `std`, `ring`, `default-x509`
- excluded from the prototype: `report`, HTTP/PCCS client, `rustcrypto`,
  `danger-allow-tcb-override`, language bindings and every fail-open path

The local differential run showed that Ring produced the same complete reports
as RustCrypto for the bundled SGX and TDX fixtures and was approximately twice
as fast. This profile is now retained only as comparative test evidence. It is
not shipped as a second V1 verifier and does not define production behavior.

## Secret Network and current Intel collateral evidence

The inspected Secret Network `master` at
`95d87aef4164cb3d056c3a364802552467ba394a` uses host Intel QVL plus
enclave-side QvE report verification and admits only `OK` and
`SW_HARDENING_NEEDED`. Outbe adopts that boundary with mandatory block time,
canonical evidence, exact native artifact pins and explicit Platform/QE
policy.

Secret Network's saved 2021 fixture has SGX Quote v3 but TCB Info schema v2.
That is historical collateral, not a hardware-version constraint: on
2026-07-30 Intel PCS v4 returned signed TCB Info schema v3, evaluation number
19, for the same FMSPC `00906ED50000`. Schema v3 therefore does not upgrade or
repair hardware; it is Outbe's canonical collateral grammar. Actual admission
still depends on the verified CPU SVN/PCE SVN match and resulting status.

Reference:
`https://api.trustedservices.intel.com/sgx/certification/v4/tcb?fmspc=00906ED50000`.

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

The superseded pure-Rust prototype accepted both:

- one byte appended after an otherwise complete quote;
- one byte appended inside the declared quote authentication-data region after
  increasing that region's length.

The prototype's exact SGX-v3 length grammar rejected both. The native
production adapter must perform the same strict check before QVL and must
require:

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

The native QVL aggregate result is necessary but not sufficient for Outbe's
stricter policy. The wrapper parses the same QvE-authenticated signed documents
to enforce schema v3 and separate Platform/QE status. It does not implement a
second certificate/signature verifier.

## Non-normative pure-Rust timing and memory

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

The normative verification-charge formula remains:

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

Batch-local collateral deduplication reduces encoded bytes only. Verification
gas is charged for every participant's logical evidence dimensions. I1 and I9
must benchmark the pinned native QVL/QvE path; if it cannot satisfy the
documented block budgets, implementation stops on that evidence instead of
silently changing the schedule or weakening verification.

## Release evidence still required by implementation

These are acceptance criteria of the implementation tasks, not open product
decisions:

- a real Platform-CA SGX fixture in addition to the Processor-CA fixture;
- current PCS fixtures with large real CRLs;
- cap-minus-one/cap/cap-plus-one and allocation-before-decode tests;
- byte-stable verdict vectors across supported x86_64 validator builds;
- exact QVL/QvE/TVL/native dependency version and digest evidence;
- forged-result, QvE-report tamper, nonce mismatch and missing-native-stack
  rejection vectors;
- valid, invalid-early, invalid-late and dense 32-validator benchmarks on the
  slowest supported x86_64 validator class;
- fail-not-skip SGX/DCAP end-to-end release CI.

ARM TEE and aarch64 are not production targets for V1 and therefore do not
carry fixture, verdict or release-gate requirements.
