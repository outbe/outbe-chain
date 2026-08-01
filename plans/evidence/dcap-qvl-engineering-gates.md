# DCAP QVL engineering-gate evidence

Date: 2026-07-31

Repository base: `d44bf85` (`main`)

Native amendment base after rebasing onto `777624e`: `5fb89ce` (`main`)

Host: `x86_64-unknown-linux-gnu`, Rust `1.96.0`

This note records the throwaway prototype used to investigate decision-map
questions #17 and #18, the later native Intel amendment, and the accepted
Gramine-native verifier boundary. It is engineering evidence, not a substitute
for the x86_64 real-hardware release gates listed below.

## Selected production verifier boundary

V1 runs Intel native QVL inside the existing Outbe Gramine enclave because
every production validator and full node is required to run on x86_64 Intel
SGX:

- Intel native QVL runs in-process over canonical, self-contained evidence and
  an explicit consensus block timestamp;
- QVL is invoked with explicit non-null collateral and
  `p_qve_report_info = NULL`; QvE and TVL are not V1 dependencies;
- the host cannot supply a `qv_result`; local PCCS responses, cache entries and
  clocks are never consensus authority;
- the Outbe wrapper enforces exact grammar, TCB Info schema v3, separate
  Platform/QE policy, measurement policy and stable verdict codes over the same
  authenticated bytes;
- missing or mismatched QVL or native dependencies fail closed.

The narrow I1 feasibility gate passed in commits `c679649`, `bc96db2`,
`d5ab89e`, `fd1598c` and `8dcebae`:

- QVL runtime and development package:
  `libsgx-dcap-quote-verify 1.26.100.1-noble1`;
- Intel headers: `libsgx-headers 2.29.100.1-noble1`;
- target: `x86_64-unknown-linux-gnu`;
- Gramine: `1.9`;
- collateral ABI: `sgx_ql_qve_collateral_t` 3.1;
- returned supplemental-data ABI: 3.0, with an exact structure-size check;
- QVL:
  `/usr/lib/x86_64-linux-gnu/libsgx_dcap_quoteverify.so.1.13.103.0`,
  SHA-256
  `4745bc5b46cbdc17a78119ae2db08f54b86ff9077c5ab480f378741396365aef`,
  ELF build ID `663c0acf2b4673c22c66f01112c5f38d856fd5a5`;
- C++ runtime:
  `libstdc++.so.6.0.33`, SHA-256
  `1fd75fe70354a416d75aef22bcae68c47bd25d20e2d0568c30b1a9838cf62f11`;
- GCC runtime:
  `libgcc_s.so.1`, SHA-256
  `d93224d2b0dab4247598be683adca02f5cf00586f99c187579cd7e92058fb7cb`.

The compiler-generated dependency closure for `native/qvl_wrapper.c` contains
11 Intel headers from exact-pinned `libsgx-dcap-quote-verify-dev` and
`libsgx-headers`. `release/dcap-native-qvl-v1.json` records every path, package
version, byte size and SHA-256. The build reads and verifies those bytes, stages
them into an isolated `OUT_DIR` include tree and places that tree before system
include paths. The pinned headers cannot introduce an unverified transitive
Intel include. The C adapter `_Static_assert`s all nine SGX QVL result enum
values against the staged `sgx_qve_header.h`; Rust tests then convert
independent raw literals through the production mapping.

`release/dcap-native-qvl-v1.json` records the inactive exact artifact and build
input contract, and `scripts/release/verify_dcap_native_qvl.py` fails closed on
a missing, changed or boundary-incompatible artifact or include input.

The real Processor-CA fixture passed five public-interface tests both natively
and under Gramine Direct: valid cryptographic verification, tampered quote,
explicit expiration boundary, empty collateral and negative time. The
checked-in Gramine harness separates Gramine initialization from the public
verifier phase; test-only markers enclose exactly the native QVL calls, and no
network, wall-clock or other write syscall occurred between them. QVL received
non-null submitted collateral and
`p_qve_report_info = NULL`; neither QPL/PCCS nor a host verdict is in the
interface. The Gramine manifest exposed only pinned runtime libraries and
fixtures, set `TZ=UTC`, and did not expose host timezone or OpenSSL
configuration files. Real SGX execution remains a fail-not-skip I9 release
gate.

The stable result must not contain addresses, native error strings or other
run-specific data. Only the canonical Outbe verdict affects consensus.

The accepted composition is supported by Gramine's own DCAP verifier:

- `ra_tls_verify_dcap.so` calls `sgx_qv_verify_quote` in-process;
- its call supplies `p_qve_report_info = NULL`;
- `ra_tls_verify_dcap_gramine.c` supplies dummy URTS enclave-management
  functions because no nested Intel SDK enclave is created.

Outbe reuses this raw QVL integration pattern only. Gramine's stock RA-TLS
wrapper is not consensus-safe as-is because it uses `time(NULL)`, supplies
`collateral = NULL`, and is coupled to X.509/RA-TLS.

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
`SW_HARDENING_NEEDED`. Outbe preserves its important trust property—an
untrusted host result is never authority—but uses Gramine's supported
enclave-resident QVL boundary with mandatory block time, canonical evidence,
exact native artifact pins and explicit Platform/QE policy.

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

The saved quote also carries `REPORT_DATA = "Hello, world!" || zeroes`; those
signed bytes cannot be changed without invalidating the quote. It therefore
cannot serve as the Outbe intent-binding vector. The 2026-07-31 corpus below
supplies that I1 binding evidence; an accepted Processor-CA capture remains a
separate fail-not-skip I9 release requirement.

On 2026-07-31 the I1 capture path produced a second real Processor-CA corpus,
this time bound to the checked-in canonical `RegistrationIntentV1` and its
complete 64-byte `report_data`. It is stored at
`crates/system/tee/tests/fixtures/intel-dcap-1.26-intent-bound-processor-negative/`
with capture provenance and per-file SHA-256 checksums. The quote is 4,600
bytes with SHA-256
`f7626ab64e0c2d182984390e9bab26d20f7b96af10dab20b43e6559edc118b1d`.
Pinned QVL 1.26 returns:

- platform and aggregate: `ConfigurationAndSWHardeningNeeded`;
- QE: `UpToDate`;
- combined TCB evaluation reference: `19`;
- QE supplemental evaluation reference: `0` (unavailable in this QVL output);
- earliest all-collateral expiration: `1787808799`.

The fixture proves real intent binding and is permanently classified as a
negative admission vector. The strict Platform policy remains
`UpToDate | SWHardeningNeeded`; it is not weakened to admit the rented capture
host. An accepted intent-bound Processor-CA capture from an SGX host that
already returns an allowed Platform status is still required before I9 may
activate production DCAP, but it does not block hardware-free I1 development.

This capture also fixed the wrapper interpretation of QVL 1.26 supplemental
data. The combined TCB evaluation reference is the minimum of the signed
Platform and QE references; a QE-specific supplemental reference of zero is
treated as unavailable, while a non-zero value must equal the signed QE
reference. `latest_issue_date` and `earliest_expiration_date` are the effective
window across all supplied collateral, so that native window must be no wider
than the windows parsed from the two signed JSON documents. The explicit block
timestamp is checked against the native all-collateral window, and the exact
expiration instant rejects.

Quote and collateral acquisition remain test tooling, not consensus inputs.
The SGX quote is generated by a separate required-feature capture executable;
the production `outbe-tee-enclave` binary does not route capture commands.
QPL/PCCS is used only by the host-side corpus capture script and is absent from
`verify_dcap_evidence`.

## Test and release evidence classes

V1 keeps implementation regression evidence separate from production-release
hardware evidence:

- the mandatory `.github/workflows/ci.yml` `dcap-replay` job installs exact
  versions from `release/project-toolchain-v1.json`, verifies the pinned Intel
  apt key and native artifact/header digests, and does not install QPL/PCCS;
- after an explicit Cargo dependency fetch, that job invokes
  `scripts/release/test_dcap_replay_ci.sh` with Cargo offline: parser/policy
  vectors and immutable real quote/collateral bytes run through the public
  verifier and pinned native QVL at a fixed historical consensus timestamp;
- the saved timestamp proves deterministic historical verification only;
  production still supplies its current consensus timestamp, so expired
  collateral rejects normally;
- the current rented SGX host remains useful for periodic quote-generation and
  capture-path smoke tests whose expected admission result is the authentic
  `ConfigurationAndSWHardeningNeeded` rejection;
- a new live capture is required when the canonical `report_data` binding,
  evidence grammar, QVL ABI/version, Intel trust root or release measurement
  changes, not once per test case;
- accepted downstream I3–I8 state-machine transitions use a private
  `#[cfg(test)]` capability strictly after the verifier boundary; it accepts a
  typed pre-verified `DcapVerdictV1`, cannot parse evidence or replace QVL, is
  absent from non-test compilation and never counts as DCAP E2E evidence;
- I5 candidate-lifecycle tests use real NodeHost/enclave keys, real UDS Noise
  sessions, canonical evidence bytes and real proofs of possession. Their
  accepted Registry mutation still begins strictly after that private typed
  verifier capability, so these are reachable lifecycle tests rather than
  Intel-hardware acceptance evidence;
- I9 alone requires fresh accepted Processor-CA evidence for the exact release
  enclave/policy, including the A-to-B replacement path, and accepted registered
  multi-package Platform-CA evidence.

For I9, `fresh` means the release job freezes the candidate enclave ELF/SGX
manifest and measurement, chain/genesis and exact active policy bytes before
capture; chooses a new one-use non-zero `binding_id` committed by the canonical
intent and quote `REPORT_DATA`; acquires collateral after that release run
starts; and verifies at the release-gate consensus timestamp while every
collateral component is current. Historical replay cannot satisfy that gate.

Synthetic Intel vectors may cover Platform-CA parsing and pure status-policy
branches but never count as real hardware evidence. No fake native verdict,
runtime verifier injection or Cargo test feature may produce an accepted result
from the public `verify_dcap_evidence` boundary. The post-verifier test
capability does not change this rule.

Production packaging must select exactly package `outbe-tee-enclave`, binary
`outbe-tee-enclave` and application feature `native-dcap`, without
`--all-features` or `--all-targets`. It excludes the capture executable,
fixture tooling, trace hooks and every mock/test-only surface. I9 updates
`release/reproducible-elf-build-v1.json` from its staged empty feature list to
exactly `["native-dcap"]`; until then that manifest is not production DCAP
activation evidence.

No ready Intel-rooted type-5 Platform-CA quote plus complete collateral bundle
exists in the inspected Secret Network or official Intel fixture corpora.
Intel publishes a real Platform-CA PCK parser vector. Its v1.26 verification
tests dynamically generate an ephemeral self-signed Platform-CA chain and
quote, but do not publish reusable Intel-rooted hardware evidence for a
positive native-QVL test. Outbe therefore uses the parser vector for CA
classification and exact raw SGX values from the pinned Intel 1.26 QVL ABI for
private status-policy tests. Because Platform CA applies to registered
multi-package SGX platforms, the current single-package capture host cannot
produce real Platform-CA evidence.

The earlier pure-Rust prototype treated both endpoints as inclusive. The
selected native QVL 1.26 behavior is different: it returned
`collateral_expiration_status = 0` at `1752919277` and `1` at the exact
earliest expiration `1752919278`. It also did not enforce the lower signed
document issue boundary by itself. Therefore the native flag is evidence, not
the complete Outbe time policy: the wrapper must parse both authenticated
signed documents and enforce their lower bounds plus the exclusive
`block_timestamp < earliest_expiration_date` upper bound before admission.
Outbe leases still end at least one hour before that authenticated deadline.

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
stricter policy. The wrapper parses the same QVL-authenticated signed documents
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
gas is charged for every participant's logical evidence dimensions. I1 proves
the deterministic caps, allocation ordering, checked arithmetic and exact gas
formula; hardware-free Gramine Direct timing is not production SGX capacity
evidence. I9 must benchmark the exact-release enclave-resident native QVL and
full block path on the published minimum supported validator profile. If that
path cannot satisfy the documented block budgets, production activation stops
instead of silently changing the schedule or weakening verification.

## Implementation evidence split

The following ownership is fixed rather than left as an open product decision.

I1 deterministic correctness evidence:

- the real intent-bound Processor-CA corpus reaches its authentic QVL and
  strict-policy result under offline replay;
- the official Intel v1.26 Platform-CA parser vector and exact Intel QVL 1.26
  raw SGX status ABI vectors are used only by private Outbe policy tests;
- cap-minus-one/cap/cap-plus-one, allocation-before-decode and checked gas
  arithmetic tests pass; synthetic cap vectors are never hardware evidence;
- byte-stable verdicts, host-verdict rejection, evidence tamper and
  missing-native-stack rejection pass on supported x86_64 CI builds.

I9 empirical production-release evidence:

- a fresh real accepted Processor-CA fixture for the exact release
  enclave/policy and a real accepted Platform-CA SGX fixture captured on a
  registered multi-package platform; either absence is a release failure;
- fresh actual Processor, Platform and root CRLs are recorded with issuer/type,
  validity dates, byte size and SHA-256 and checked against the protocol caps;
  the release benchmark uses the largest actual matching collateral bundle;
- exact-release `gramine-sgx` valid, invalid-early, invalid-late and dense
  32-validator benchmarks run on the published minimum supported x86_64
  validator profile and keep full-block execution inside its timing budget;
- the signed Gramine release manifest pins the selected QVL/native dependency
  artifacts, and SGX/DCAP end-to-end release CI is fail-not-skip.

The 2026-07-31 observed Intel CRLs were 1,002 bytes for Processor, 3,354 bytes
for Platform and 294 bytes for the root. Intel controls the signed revocation
population, so V1 has no fabricated or undefined "large real CRL" threshold.
If Intel later publishes a materially larger valid CRL, it is retained as a
regression fixture without changing the evidence classification above.

ARM TEE and aarch64 are not production targets for V1 and therefore do not
carry fixture, verdict or release-gate requirements.
