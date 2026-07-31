# I1 checkpoint — enclave-resident native QVL feasibility

Date: 2026-07-30

Audited implementation commits:

- `c679649dc438f39788abd79210faf3f8ded567f4`;
- `bc96db2b2e1a6c3968c7d2cf87c1c9b04e6d5a8e` (review hardening);
- `d5ab89eca7db39350f8b2d38fc1e576a00996af8` (review closure);
- `fd1598c1eb59c07eb4fe049659f9cf0c17626875` (stateful syscall isolation);
- `8dcebae477a63493d99753ab177b0e24d3cb873d` (QVL-scoped syscall trace).

Status: `PASS` for the narrow feasibility gate. Full I1 remains incomplete,
but I2–I8 are not stopped behind an unavailable accepted Processor capture.
The 2026-07-31 testing-gate amendment in the authoritative decision map,
engineering evidence and implementation plan supersedes that earlier
sequencing: I1 uses immutable real-corpus replay, while accepted live hardware
is a fail-not-skip I9 production-activation gate.

## Scope proved

The current Rust/Gramine enclave can call exact-pinned Intel native QVL
in-process without migrating to Intel SGX SDK:

- target is explicitly limited to `x86_64-unknown-linux-gnu`;
- inputs are quote bytes, all seven submitted collateral components and an
  explicit consensus timestamp;
- the C adapter constructs collateral ABI 3.1 and calls
  `sgx_qv_verify_quote` with `p_qve_report_info = NULL`;
- no host verdict, QvE, TVL, QPL/PCCS lookup or wall clock is in the interface;
- the supplemental ABI is checked against the exact compiled Intel structure
  size and returned as version 3.0;
- raw PPID remains inside the private FFI result and is not exposed by the
  public Rust verdict;
- unsupported target, ABI, result, malformed supplemental data, empty input
  and native verification failure all fail closed.

This proves the accepted trust-boundary composition. It does not yet prove the
complete Outbe grammar or admission policy.

## Pinned artifacts

The inactive contract `release/dcap-native-qvl-v1.json` records:

| Role | Package version | SHA-256 |
|---|---|---|
| QVL | `libsgx-dcap-quote-verify 1.26.100.1-noble1` | `4745bc5b46cbdc17a78119ae2db08f54b86ff9077c5ab480f378741396365aef` |
| C++ runtime | `libstdc++6 14.2.0-4ubuntu2~24.04.1` | `1fd75fe70354a416d75aef22bcae68c47bd25d20e2d0568c30b1a9838cf62f11` |
| GCC runtime | `libgcc-s1 14.2.0-4ubuntu2~24.04.1` | `d93224d2b0dab4247598be683adca02f5cf00586f99c187579cd7e92058fb7cb` |

`scripts/release/verify_dcap_native_qvl.py` verified the installed artifacts
and exact installed package/header versions. Its five tests prove exact
success, changed-artifact rejection, status rejection, package/build-ID
rejection and QvE/QPL boundary rejection.

## Executed evidence

Real upstream Processor-CA fixture:

- quote: 4,600 bytes, SHA-256
  `f8b81014b6e443609746822194910f5dc1c92c322fa0584298d1e33e505ca3b5`;
- collateral wrapper: SHA-256
  `bdd694bbe50f3a2a1cfe12f9e2bd83125921107a368edcf10780a5523b8501ce`.

The same five public-interface cases passed natively and in Gramine Direct:

1. real quote verifies cryptographically as
   `ConfigurationAndSWHardeningNeeded`, with QE `UpToDate`;
2. a tampered real quote returns the stable verification error;
3. explicit time controls the native expiration flag;
4. empty submitted collateral rejects before QVL;
5. negative consensus time rejects before QVL.

Commands:

```text
cargo test -p outbe-tee --features native-dcap --test native_qvl --offline
5 passed; 0 failed

python3 scripts/release/test_dcap_native_qvl_gramine.py
5 passed; 0 failed
native-QVL Gramine feasibility harness passed

cargo check -p outbe-tee --offline
passed

cargo check -p outbe-tee-enclave --features native-dcap --offline
passed

cargo clippy -p outbe-tee --features native-dcap --test native_qvl --offline -- -D warnings
passed

python3 scripts/release/tests/test_dcap_native_qvl.py
5 passed; 0 failed

python3 scripts/release/verify_dcap_native_qvl.py
native-QVL artifact contract verified
```

The checked-in harness traces Gramine with test-only markers immediately
around each native QVL call. Gramine itself creates internal AF_UNIX channels,
resolves `localhost` and reads time during LibOS initialization; those calls
are outside the QVL markers. Between each of four matched QVL BEGIN/END pairs,
the harness fails on every network, wall-clock or additional write syscall and
observed none. The test bundle exposes only the executable, pinned runtime
libraries and `TZ=UTC`.

## Semantic finding carried into full I1

Native QVL 1.26 reported collateral valid at `1752919277` and expired at the
exact earliest expiration `1752919278`. It did not enforce the lower
signed-document issue boundary on its own. Full I1 must therefore enforce both
authenticated signed-document lower bounds and the exclusive upper bound in
the stable Outbe wrapper; relying only on QVL's expiration flag is forbidden.

## Review closure

Independent Standards and Spec reviews initially found incomplete exact
pinning, a non-reproducible Gramine proof, raw identifier exposure and gaps in
the syscall evidence. Commits `bc96db2`, `d5ab89e`, `fd1598c` and `8dcebae`
closed those findings. The final re-review returned no remaining actionable
finding for this feasibility scope.

## Remaining I1 gates

- canonical DER, signed-JSON and exact SGX Quote v3 grammar;
- complete outer/inner consumption and type-5 chain equality;
- pinned Intel root, FMSPC/PCE ID and evaluation-number checks;
- separate Platform and QE status matrix with authenticated advisories;
- stable consensus verdict/reject ordering and gas precharge;
- a real intent-bound Processor-CA corpus replayed through pinned QVL with its
  authentic strict-policy result, current large-CRL coverage and synthetic
  Intel Platform-CA parser/policy vectors;
- byte-stable vectors and bounded performance evidence.

Fresh accepted Processor-CA execution and a real registered multi-package
Platform-CA fixture are intentionally retained as fail-not-skip I9 release
gates rather than misrepresented by Gramine Direct or synthetic evidence.
