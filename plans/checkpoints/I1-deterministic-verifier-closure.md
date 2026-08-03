# I1 checkpoint — deterministic DCAP verifier closure

Date: 2026-07-31

Status: `PASS` for I1. This checkpoint does not claim an accepted hardware
verdict, production activation, Docker delivery or real registered
multi-package Platform-CA coverage; those remain fail-not-skip I9 gates.

## Outcome

The canonical public `verify_dcap_evidence` path replays a real
Processor-CA quote and caller-supplied collateral through exact-pinned Intel
QVL at an explicit historical consensus timestamp. Cryptography succeeds and
the authentic `ConfigurationAndSWHardeningNeeded` Platform result reaches the
strict policy, which returns stable `PlatformTcbRejected` bytes `05 01`.
Policy was not weakened and no test seam can turn that corpus into a positive
public-verifier result.

The same hardware-free replay is now a mandatory ordinary x86_64 CI job. It
does not install QPL/PCCS, does not need SGX hardware and performs no live
collateral fetch. Cargo dependencies are populated before the replay phase;
all replay commands themselves use `--offline`.

## Acceptance audit

| I1 criterion | Authoritative evidence | Result |
|---|---|---|
| Intent-bound real Processor corpus reaches its authentic strict-policy result | `intent_bound_real_processor_quote_reaches_strict_platform_status_policy`; checked-in quote, collateral, intent and policy hashes | `PASS` |
| Replay, tamper and time boundaries execute offline on every supported x86_64 CI build | `.github/workflows/ci.yml` job `dcap-replay`; `scripts/release/test_dcap_replay_ci.sh`; 25 public and 5 native integration tests | `PASS` |
| Platform grammar and exact SGX result ABI are pinned without synthetic hardware claims | Intel v1.26 `PlatformPEM`; independent Rust raw literals; nine C `_Static_assert`s against digest-pinned `sgx_qve_header.h` | `PASS` |
| Synthetic statuses cannot produce a public-verifier positive | Status constructors remain private pure tests; no fake QVL/runtime injection or production-selectable fake feature exists | `PASS` |
| Trailing and non-canonical evidence rejects | Public integration tests cover outer/declared trailing bytes, PEM/JSON canonicality, empty components and policy canonicality | `PASS` |
| Time and strict Platform/QE matrices pass | Signed-document issue/expiration tests, native collateral window tests, Platform allowlist `UpToDate | SWHardeningNeeded`, QE only `UpToDate` | `PASS` |
| Deterministic capacity and gas behavior is bounded before cryptographic work | cap-minus-one/cap/cap-plus-one, allocation ordering, overflow checks and `consensus_qvl_precharge_matches_the_normative_formula_exactly` | `PASS` |
| Consensus is independent of environment, filesystem, network, wall clock and native error strings | Public API accepts only evidence, policy and timestamp; Gramine syscall harness; stable owned reject codes | `PASS` |
| Host verdict is absent and tampering rejects | Native wrapper returns only parsed QVL data; real quote tamper and intent-rebind tests reject | `PASS` |
| Fixture verdict bytes are stable | `strict-policy-reject-code-v1.hex` SHA-256 `46698a793e5cae7c0a6da3cf4361e810b4155a1e173cbac9993576b2c9ed891e`; public byte comparison | `PASS` |
| Native versions, artifact/header digests and missing/mismatch behavior fail closed | `release/project-toolchain-v1.json`; `release/dcap-native-qvl-v1.json`; verifier behavioral tests; Rust build contract | `PASS` |
| No second verifier or live collateral fetch enters consensus | Native-QVL dependency/build audit; QPL/PCCS exists only in the separate host capture tool | `PASS` |

## Independent ABI proof

A compiler depfile for `native/qvl_wrapper.c` identifies 11 Intel headers:
`sgx_dcap_quoteverify.h`, `sgx_qve_header.h`, `sgx_key.h`, `sgx_attributes.h`,
`sgx_ql_quote.h`, `sgx_ql_lib_common.h`, `sgx_quote.h`, `sgx_report.h`,
`sgx_defs.h`, `sgx_quote_3.h` and `sgx_pce.h`. They come from exact-pinned
`libsgx-dcap-quote-verify-dev 1.26.100.1-noble1` and
`libsgx-headers 2.29.100.1-noble1`.

`release/dcap-native-qvl-v1.json` pins every include by path, package version,
size and SHA-256. The build verifies the bytes and stages them into an isolated
`OUT_DIR` include tree before compiling the C adapter. A dependency check of
that compilation resolves every Intel include from the staged tree, not
`/usr/include`.

The staged `sgx_qve_header.h` is 10,590 bytes with SHA-256
`f8994fcb1b56ed938adbf923146b5fed8c3e8d5d7d6827f45342db0a23e56677`.
The C translation unit `_Static_assert`s values `0x0000` and `0xA001..0xA008`
against Intel's nine SGX result enum constants. Rust tests use independent raw
literals and pass them through the production `from_raw` and output conversion
paths for both Platform and QE. A coordinated error in a Rust constant and its
Rust test vector can therefore no longer mask header drift, while a changed API
prototype or collateral layout fails the include digest contract.

## Reproducible gate

CI installs exact package versions read from the single project pin on
Ubuntu 24.04 x86_64 and verifies the Intel apt signing-key SHA-256 before
adding the repository. `--no-install-recommends` and the selected QVL
runtime/development/header packages do not install QPL/PCCS.

Local and CI entry point:

```bash
bash scripts/release/test_dcap_replay_ci.sh
```

The entry point runs:

- 10 native-manifest behavioral tests and installed digest verification;
- 14 consensus-primitives tests for cap boundaries, allocation ordering,
  checked gas arithmetic and canonical protocol vectors;
- 39 `outbe-tee` unit tests;
- 25 public DCAP integration tests;
- 5 native QVL integration tests;
- 2 intent-bound fixture-tool tests;
- all checked-in corpus SHA-256 checks.

The checked-in Gramine Direct harness separately proves that exactly four
marked native-QVL calls perform no network, wall-clock or other write syscall
inside the verifier boundary.

## Deferred, not waived

I9 still requires all of the following to fail rather than skip:

- a fresh accepted Processor-CA capture bound to the exact release
  enclave/policy and a one-use nonzero `binding_id`;
- a real accepted Intel-rooted Platform-CA capture from registered
  multi-package SGX hardware;
- real accepted validator, full-node and 32-validator block-1
  `DcapRequired` execution;
- fresh actual Processor, Platform and root CRLs recorded with issuer/type,
  validity, byte size and SHA-256 and checked against the caps; the benchmark
  uses the largest actual matching PCS bundle, not a fabricated "large" CRL;
- empirical exact-release `gramine-sgx` valid, invalid-early, invalid-late and
  dense 32-validator benchmarks on the published minimum supported x86_64
  validator profile, including the full-block consensus timing budget;
- exact Docker delivery and production activation with only
  `outbe-tee-enclave` plus feature `native-dcap`.
