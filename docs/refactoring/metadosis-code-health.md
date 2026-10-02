# Metadosis code health refactor

Branch: `refactor/metadosis-code-health`, based on `main` at `b1149eb6`.
Fresh-chain storage changes are intentional; no migrations or legacy decoder.
Public commands, read APIs, Solidity ABI and protocol economics stay unchanged.

## Execution checklist

- [x] Unify terminal receipt storage and proof/genesis bindings.
- [x] Normalize the live job index to identity keys.
- [ ] Decompose aggregate validation by index invariant.
- [ ] Separate request, finality and voting.
- [ ] Isolate completion and expiry.
- [ ] Separate worldwide-day advancement and effects.
- [ ] Decompose activation fixtures by domain responsibility.
- [ ] Extract precompile queries; run final verification and static analysis.

## Baseline

`cargo test -p outbe-metadosis --all-targets --features test-utils --locked --offline`:
240 library tests, 2 compile-fail harness tests, 5 FSM model tests and 5 semantic
facade tests passed; one exhaustive capacity fault matrix is intentionally ignored
in the default run and must run separately.

LLVM line coverage of production `crates/core/metadosis/src` (excluding tests and
`test_support`): 7,077 / 8,669 lines, **81.64%**. Measured with cargo-llvm-cov 0.8.5.
Baseline SARIF and LLVM JSON are retained in `/tmp/metadosis-before.sarif` and
`/tmp/metadosis-coverage-before.json` during this work.

## Decisions and evidence boundaries

The analyzer's untested-hotspot claim does not describe the existing test suite:
there are behavioral command/API/ABI, replay and fault-injection tests. Missing
coverage ingestion and paired test-file detection are separate metadata issues.
New codec tests pin independent wire vectors and reject malformed input; tests
do not inspect implementation source text.

Terminal receipt common fields have one authority. Capacity evidence is a tagged
variant in the same byte record. Live scheduler keys identify the authoritative
per-day FSM rather than duplicating its snapshot. Layout changes require updating
the Fidelity opening codec registry from its TSV authority as well as genesis pins.

History-based churn, defect and coupling findings are background signals and are
not targets of this refactor. Test fixture `unwrap`s intentionally fail incomplete
setup; accessor/factory cohesion findings require behavioral justification before
changing interfaces.

Step 1: 245 library tests and all 12 integration/model/facade harness tests passed.
The fixed OMTR wire vectors first failed against the empty encoder, then passed
with canonical encoding. Registry and measurement shape were regenerated from
their authorities; the intermediate live codec remains OMLI1 until step 2.

Step 2: 249 library tests passed, one separately gated fault matrix ignored.
OMLI2 stores 36-byte identity keys instead of 148-byte FSM copies. Snapshot and
carrier admission byte caps follow the new key size; per-day FSM validation remains
authoritative. The fresh-chain layout hash now commits OMLI2.
