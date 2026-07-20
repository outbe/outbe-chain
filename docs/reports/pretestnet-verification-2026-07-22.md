# Pre-testnet verification for 22 July 2026

Status: final; no testnet mutation was performed.

## Scope and identity

- Verification date: 18 July 2026 (UTC).
- Baseline `origin/main`: `f2303c163fd8cc889db4ca4daa13ec0ba4bd6c33` (`Feat/offchain data (#131)`).
- Verified code candidate: `e7c6a6840670222d6e4f145c3ebdc028285e897e` (`perf(ci): keep container recipe out of source layer`).
- Branch: `test/pretestnet-verification-2026-07-22`.
- Testnet was not deployed, updated, or otherwise mutated.
- Nothing from this branch was pushed during verification.
- Local networks, MongoDB databases, ports, data directories, and systemd units used dedicated pre-testnet names.

The candidate includes small, independently committed fixes discovered by this verification. Therefore it is intentionally not byte-identical to `origin/main`. A testnet release must select and review these commits before deployment.

## Verdict

**NO-GO for updating testnet from the candidate as verified.**

The functional Rust, contract, localnet, MongoDB, consensus, follower, restart, fuzz, amd64-container, and hardware-SGX evidence is strong. The NO-GO is caused by unresolved release/security gates, not by a known consensus-liveness failure: runtime binary audits contain three RustSec vulnerabilities; the amd64 image scan reports 4 Critical and 17 High OS-package findings; `cargo vet` cannot validate twelve git dependencies; Aderyn reports six untriaged High categories; the principal release binary exceeds the binary auditor's size limit; Smart Account lint is broken; DCAP quote generation is unavailable on this host; and the arm64 image could not be built or scanned in the available environment.

Release conditions:

1. Review and accept or replace every candidate commit listed below.
2. Resolve or formally risk-accept the RustSec findings with dependency-path and runtime-reachability evidence; make the main `outbe-chain` binary auditable despite its 191 MB size.
3. Triage every Slither/Aderyn finding, add regression tests for confirmed issues, and record false-positive reasoning for the rest.
4. Repair `cargo vet`, Smart Account lint, and the remaining prerelease container gates; run amd64 and arm64 Trivy scans on the selected release SHA.
5. If remote attestation is a testnet requirement, provision AESM/PCK/PCCS and pass a DCAP quote-verification smoke test before deployment.
6. Do not treat unimplemented flow rows in the coverage matrix as tested merely because component unit tests pass.

## Candidate commits

| Commit | Reason and confirming evidence |
|---|---|
| `36a1d1c` | Restores the main-branch Clippy CI gate. Final strict Clippy passes. |
| `8ac65ed` | Aligns compressed-entity fixtures with schema v3. Covered by the full Rust suite. |
| `7776718` | Restores the consensus dependency-boundary audit script. |
| `01cf334` | Restores the missing mock-USDT deployment guard. It permits BSC testnet (`97`) and local Anvil (`31337`); it does not replace BSC with Anvil. |
| `78f03e3` | Updates vulnerable runtime dependencies where compatible fixes exist. The remaining findings are disclosed below. |
| `26f78bd` | Prevents healthy DKG-share disclosure in logs. Hardware-SGX audit found zero share-reveal alarms. |
| `301b0e8` | Cancels stale compressed-tree payload jobs. Regression and lifecycle E2E pass. |
| `d267f65` | Preserves stale-parent readiness instead of losing the lifecycle condition. |
| `0be40c6` | Preserves typed readiness errors across the EVM hook boundary. |
| `f787618` | Retries stale payload attempts without publishing an invalid attempt. |
| `da03d06` | Persists sealed TEE state across full localnet restart. Four-node restart and hardware-SGX restart pass. |
| `5e61927` | Builds payloads on canonical ancestors after forkchoice changes. Focused hardware-SGX lifecycle passes. |
| `ed7bd37` | Makes Tribute E2E wait for a pending offer to be mined before asserting its projection. |
| `f53b6da` | Applies repository-wide formatting required by the final format gate. |
| `0b62a3b` | Advances a late verifier through a missed DKG freeze notification. Exact-HEAD hardware-SGX promotion passes. |
| `8b9bfc4` | Adds the live late warm-promotion regression scenario. |
| `8d6751c` | Reconciles the ADR coverage ledger with observed tests and gaps. |
| `47c99cb` | Makes the isolated participation fuzz target runnable and pins its compatible lockfile. |
| `06994a9` | Returns a normal CLI error for malformed private-key lengths instead of panicking. Unit and real CLI paths pass. |
| `61feeb1` | Keeps governance votes within the proposal's actual voting window. The focused live scenario passes twice after the fix. |
| `3d29b72` | Requires the lifecycle E2E to observe a successful Tribute receipt instead of accepting transaction submission alone. The 9-step lifecycle rerun passes. |
| `bb86666` | Restores the current-nightly `cargo udeps` gate after an obsolete test import became a denied warning. |
| `afbf3df`, `fa09bb2`, `f933901`, `8a632b8` | Replace future-incompatible tail-position `bail!` expressions without semantic changes. Affected strict Clippy and targeted suites pass. |
| `c974eb7` | Updates the offchain reader to the current atomic API; strict Clippy and 26/26 tests pass. |
| `b02326a` | Refreshes stale Intent ABI exports discovered by the exact CI freshness check. |
| `282c292` | Adds a bounded Docker context and the missing libc++ builder dependencies. |
| `80f56e4` | Retains the Solidity precompile interfaces required by Rust `sol!` expansion while excluding unrelated contract projects from the image context. |
| `72e1370` | Installs libc++ in the runtime image after the first successful build failed its smoke launch on `libc++.so.1`. |
| `e7c6a68` | Keeps Docker recipe metadata out of the Rust source layer, reducing unnecessary rebuild invalidation. |

## Final exact-candidate gates

| Check | Result | Duration | Evidence |
|---|---:|---:|---|
| `cargo fmt --all -- --check` | PASS | final exact-candidate run | `/tmp/outbe-pretestnet-f2303c1/final-e7c6a68-cargo-fmt.log` |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS | 11.60 s | `/tmp/outbe-pretestnet-f2303c1/final-e7c6a68-cargo-clippy.log` |
| `cargo nextest run --workspace --all-features` | PASS: 2706/2706; 21 skipped | 103.34 s | final exact-candidate terminal record |
| `cargo test --doc --workspace` | PASS | 18.21 s | `/tmp/outbe-pretestnet-f2303c1/final-e7c6a68-doctests.log` |
| `cargo audit` against the local 1166-advisory DB | FAIL: 4 vulnerabilities; 9 warnings | 0.05 s | `/tmp/outbe-pretestnet-f2303c1/final-cargo-audit-local-db.log` |
| `cargo deny check` | FAIL: advisories; bans/licenses/sources pass | 2.62 s | `/tmp/outbe-pretestnet-f2303c1/final-cargo-deny-escalated.log` |
| `cargo +nightly udeps --workspace --lib --examples --tests --benches --all-features` | PASS: all dependencies used | 15.69 s | `/tmp/outbe-pretestnet-f2303c1/ci/cargo-udeps-final.log` |
| `cargo llvm-cov --workspace --cobertura` | PASS: 76.314% lines (74,734/97,929) | 480.56 s | `/tmp/outbe-pretestnet-f2303c1/ci/cargo-llvm-cov.log`; `ci/cobertura.xml` |
| `cargo machete --with-metadata` | FAIL/advisory: 22 declarations in 8 packages | recorded | `/tmp/outbe-pretestnet-f2303c1/ci/cargo-machete-with-metadata.log` |
| `cargo vet` | FAIL: missing policy for 12 git packages | recorded | `/tmp/outbe-pretestnet-f2303c1/ci/cargo-vet.log` |
| `mise run build-release` | PASS: auditable release workspace | 242.00 s | `/tmp/outbe-pretestnet-f2303c1/ci/build-release.log` |
| release binary audit | FAIL: 3 vulnerabilities in each scanned runtime binary; main binary too large to scan | recorded | `/tmp/outbe-pretestnet-f2303c1/ci/cargo-audit-release-binaries-valid.log` |

The first exact-HEAD nextest attempt stopped after 15 CLI RPC tests could not bind loopback listeners under the sandbox (`EPERM`). The complete rerun with loopback access passed all 2706 tests. This is classified as an execution-environment failure, not hidden or counted as a product pass.

## Contract suites

| Suite | Result | Evidence |
|---|---:|---|
| Crosschain Foundry | PASS: 60 | `contracts/crosschain-rerun.log` |
| Intent Foundry | PASS: 98 | `contracts/intent-rerun.log` |
| Intex Foundry | PASS: 623 | `contracts/intex-rerun.log` |
| Smart account Foundry | PASS: 42 | `contracts/smart-account-rerun.log` |
| Tokens Foundry | PASS: 44 | `contracts/tokens-fixed.log` |
| Precompiles | BUILD PASS, 0 tests | `contracts/precompiles.log` |
| Smart-account lint | FAIL: unresolved `kernel-7579-plugins` import | `contracts/smart-account-lint.log` |
| Contract format checks | PASS: crosschain, intent, smart-account, tokens, Intex | `/tmp/outbe-pretestnet-f2303c1/ci/contracts-format-lint.log`; `intex-lint-format-compile.log` |
| Contract high-severity lint | PASS: crosschain, intent, tokens; FAIL: smart-account import resolution | `/tmp/outbe-pretestnet-f2303c1/ci/contracts-format-lint.log` |
| Intent ABI freshness | initially FAIL, fixed in `b02326a`; Smart Account fresh | generated diff and clean rerun |
| Intex Solhint/format/compile | PASS, warnings disclosed | `/tmp/outbe-pretestnet-f2303c1/ci/intex-lint-format-compile.log` |
| Intex Slither 0.11.5 | command PASS with 6 findings (2 reentrancy, 4 unused returns) | `/tmp/outbe-pretestnet-f2303c1/ci/intex-slither.log` |
| Intex Aderyn 0.6.8 | command PASS; report contains 6 High categories and 10 Low categories | `/tmp/outbe-pretestnet-f2303c1/ci/intex-aderyn.log`; ignored generated `contracts/intex/report.md` |

The smart-account compiler/tests pass with Foundry remappings, while its separate lint tool cannot resolve the same import. This is a toolchain/remapping gap, not evidence that the linted source is clean.

The security analyzers completing with exit code zero does not clear their findings. Slither's six instances and Aderyn's category-level report remain untriaged release blockers. Aderyn also reports `0` nSLOC for all files, so its issue counts are retained as leads rather than treated as calibrated severity evidence.

## Fuzz, property, ignored, and unavailable checks

The repository contains one libFuzzer target, `fuzz_participation_decode`. The original nested manifest was not independently runnable and an unconstrained isolated resolution selected an incompatible Alloy parser. Commit `47c99cb` makes it an isolated workspace and pins the compatible dependency graph.

Final command:

```text
cargo +nightly fuzz run fuzz_participation_decode -- -max_total_time=60 -print_final_stats=1
```

Result: PASS, 9,767,285 executions in 61 seconds, average 160,119 executions/s, coverage 127, feature count 195, corpus 41, peak RSS 408 MB, no panic/crash/ASan report. Total command time was 328.67 seconds because it included the ASan build. Evidence: `/tmp/outbe-pretestnet-f2303c1/fuzz-participation-decode-final.log`.

The exact test inventory contains 21 ignored tests:

- 18 MongoDB integration tests: explicitly executed; 18/18 pass.
- Emission pin-print helper: explicitly executed; 1/1 pass. It prints deterministic reference values and is not a correctness assertion.
- TEE throughput benchmark: PASS with native debug transport (20,000 offers, 1,155 offers/s) and hardware-SGX release transport (20,000 offers, 12,364 offers/s). The rates are not directly comparable because profiles differ.
- EVM call-trampoline skeleton: not counted as a test. Both builders return the same zero `PostState`, and the source states that it is dormant until a real sub-call implementation and committed trampoline bytecode exist. Running it would create false confidence.

Evidence: `nextest-list-exact-head.json`, `ignored-emission-print-pins.log`, `ignored-tee-throughput-native.log`, and `ignored-tee-throughput-real-sgx.log`.

### Miri applicability

The exact CI command, `cargo +nightly miri test --lib`, was executed rather than inferred from compilation. It made substantial progress through the workspace but did not complete:

- `outbe-agentreward::gas_06_agentreward_dense_daily_distribution_completes_and_clears_indexes` did not finish in the extended interpreter window; the same test passes natively. The run was stopped and preserved in `/tmp/outbe-pretestnet-f2303c1/ci/cargo-miri.log`.
- Continuing without `outbe-agentreward` stopped in `outbe-common` because `ring 0.17.14` calls the unsupported foreign function `OPENSSL_cpuid_setup`. Miri explicitly says this does not indicate a program bug. Evidence: `ci/cargo-miri-without-agentreward.log`.
- Continuing without both known blockers reached `outbe-compressed-entities` but its first cryptographic golden-vector test also did not finish in a practical interpreter window. Evidence: `ci/cargo-miri-without-known-blockers-1.log`.
- Proc-macro unit tests execute outside Miri by design.

No Miri `Undefined Behavior` report was observed. This is a **partial/blocked**, not a PASS. The gate needs a Miri-specific test profile that excludes performance/FFI tests while retaining unsafe-memory coverage.

## Release and container gates

- `cargo auditable build --release` passes for the complete workspace.
- Seven `outbe-*` release executables were found. Six auditable binaries below 100 MB each report the same three vulnerabilities: `hickory-proto 0.25.2` (RUSTSEC-2026-0118 and -0119) and `tracing-subscriber 0.2.25` (RUSTSEC-2025-0055). `outbe-tee-enclave-mock` was not built with auditable metadata.
- `outbe-chain` is 191,481,992 bytes and exceeds cargo-audit's fixed 104,857,600-byte binary limit, so the workflow cannot audit its principal artifact even though the workspace lockfile audit identifies the vulnerable dependency paths.
- The first amd64 Docker attempt failed before the Dockerfile because the missing `.dockerignore` included a 263 GB local `target/` tree. After bounding the context it fell from 765 MB to about 30 MB.
- The next attempt exposed missing `libc++`; the builder now installs `libc++-dev` and `libc++abi-dev`, matching the native prerelease job.
- Excluding all contracts then exposed compile-time `sol!` dependencies on `contracts/precompiles/src`; `80f56e4` retains only that required subtree.
- Exact-candidate amd64 image build and smoke launch PASS. Image `outbe-chain:rc-amd64` is `sha256:7e5c09c956a75adde1a798c6bb8daab23ce8344170840b55b1f10046c584aa8b`, 91,891,388 bytes, and reports the exact candidate commit from `--version`. Evidence: `/tmp/outbe-pretestnet-f2303c1/ci/docker-build-amd64-final-e7c6a68.log`.
- Trivy scan of that exported image reports 4 Critical and 17 High Debian-package vulnerabilities, with no fixed version supplied by the current vendor feed. Evidence: `/tmp/outbe-pretestnet-f2303c1/ci/trivy-amd64.json`.
- ARM64 is blocked on this host: Docker buildx is absent and no arm64 binfmt handler is registered. Installing a privileged third-party binfmt helper would mutate host kernel execution settings and was rejected without explicit authorization. This was not bypassed or reported as a pass.

## Live localnet and MongoDB evidence

An isolated four-validator network used RPC ports 11545-11548, MongoDB port 37027, container `outbe-pretestnet-stack-f2303c1-mongodb`, and database prefix `outbe_pretestnet_f2303c1`. The user's regular localnet container was not modified.

Observed happy-path invariants:

- all four heads, finalized heights, and compressed roots converged;
- encrypted Tribute transaction `0xd52b6...65d3` produced successful receipts on all nodes;
- `eth_call` returned identical state on all nodes;
- all four MongoDB projections contained one Tribute plus matching owner/day indexes;
- a full stop/start preserved TEE state and moved heads from 3 to 5;
- each validator log showed the sealed-state unseal path after restart.

Observed MongoDB outage/recovery invariants:

- all four validators were at height 59 before the outage;
- while MongoDB was paused, consensus advanced to height 60;
- encrypted Tribute transaction `0x25fc877c7e10380dc0dc3c09cdb781d125800eba4ec814a60a3b2eac234c0ad4` was submitted through the public CLI;
- after MongoDB resumed, the receipt was successful in block 61 with gas used `0x4ea74`;
- all validators converged to height 98;
- every projection contained exactly one transaction Tribute, Tribute, owner index, and day index.

Evidence: `mongo-outage-live-with-tx.log`, `mongo-recovery-projections.log`, and `localnet-restart-fix/`.

## Hardware-SGX evidence

- SGX devices: `/dev/sgx_enclave`, `/dev/sgx_provision`, and `/dev/sgx_vepc` were present.
- Hardware smoke passed enclave execution and EGETKEY/sealing.
- DCAP quote generation did not pass: AESM returned error 12 because PCK/PCCS provisioning is unavailable on this host.
- The broad hardware-SGX run executed 16 scenarios: 15 passed and one exposed the late warm-promotion defect fixed by `0b62a3b`.
- The affected lifecycle was rerun on hardware SGX and passed 9/9 steps after the canonical-ancestor fix.
- The exact-candidate late follower/recovery/warm-promotion regression passed 10/10 steps.
- The exact regression observed validator 4 record DKG freeze at height 105 and activate epoch 2 at height 120.
- Audit counts for that regression: zero fatal messages, zero VRF alarms, zero DKG-share reveal alarms, zero SGX resource/EAGAIN errors, and zero panics.
- Two duplicated `vrf_verified=false` records refer to the same first epoch-2 view-1 block during degraded seed bootstrap. They are warnings, not VRF alarm events, and are disclosed as a transition risk.

Evidence: `e2e/sgx-smoke-final-2.log`, `e2e/e2e-sgx-full-final-4.log`, `e2e/sgx-lifecycle-fcu-fix.log`, and `e2e/sgx-follower-late-promotion-exact-head.log`.

## Live E2E scenarios

### PFS-005–PFS-008 branch addendum (pre-review)

> Historical checkpoint only. The review, focused repetitions, canonical mock
> and hardware-SGX suites, workspace regression and ignored-test audit have now
> completed. The authoritative outcome is
> [PFS-005–PFS-008 completion verification](pfs-005-008-completion-2026-07-20.md).
> Statements below about deferred final suites describe the earlier checkpoint
> and are retained only for chronology.

This addendum records the current `test/pfs-005-008-live-e2e` branch state. It
does **not** supersede the final-suite evidence above and must not be read as a
release PASS. At the operator's request, the final complete mock, hardware-SGX
and workspace regressions are deliberately deferred until a separate review of
the branch fixes and assertions is complete.

| Flow | Reproducible scenarios now present | Evidence status before review |
|---|---|---|
| PFS-005 | voting-window validator restart; full-committee schedule and activation-boundary restarts; duplicate/unauthorized/conflicting/expired paths; unsupported activation, unchanged-binary restart and real replacement-binary recovery | self-contained unsupported-version operator recovery focused PASS: 20/20 steps, `/tmp/outbe-e2e-harness-3104132/run-1784490228-3104132`; final feature/suite deferred |
| PFS-006 | join/exit/claim accounting; stale join; stalled reshare; actual downtime slash and duplicate-punishment guard; registration, in-flight DKG, completed-DKG, active-share, full-committee sealed-state and active-validator-during-reshare restarts | stalled-reshare focused PASS: 6/6 steps, `/tmp/outbe-e2e-harness-2903979/run-1784485353-2903979`; remaining historical runs and product-fix evidence require review before being promoted to final evidence |
| PFS-007 | quota/fallback; exact raw replay before and after validator/full-committee restart; invalid/wrong-target/conflicting authorizations; worldwide-day lazy reset | scenarios implemented; final live rerun deliberately deferred until review |
| PFS-008 | cold/chained sync; upstream loss, lag and switch; durable follower restart; warm-promotion boundary, duplicate readiness, promoted node/enclave restart and active-validator restart | scenarios implemented; final hardware-SGX rerun deliberately deferred until review |

Product-fix commits on this branch are candidates for review, not automatically
accepted findings. The review must reconstruct each red reproduction, verify the
root cause from code/log evidence, and confirm that the regression would fail
without the fix. Any item lacking that evidence must be reclassified rather than
reported as a confirmed defect.

The initial complete mock run passed 15 scenarios and failed the governance timing scenario. That failure was deterministic: the test cast votes after its own configured voting window. After `61feeb1`, the focused governance feature passed twice, 16/16 steps each time. The lifecycle feature was separately strengthened by `3d29b72` and passed 9/9 steps; restart coverage passed 7/7 focused steps. The broad hardware-SGX run passed every scenario except the late promotion case that produced the confirmed defect; the focused exact-candidate hardware regression then passed that corrected case. All runtime-affecting nightly-compatibility edits also passed their targeted Rust suites, and the final full workspace regression is recorded in the exact-candidate table.

| Live feature | Primary observable invariants |
|---|---|
| Follower upstream | cold and chained followers track finality; killed validator catches up; warm follower joins, activates, and remains in lockstep |
| Validator lifecycle | cold sync, promotion, in-flight offer, DKG/reshare, exit, continuing liveness |
| Active restart | individual and full-committee sealed restart recover without a fresh initial ceremony |
| DKG failure | missing dealer does not halt the old committee; restored dealer permits recovery |
| Downtime | chain remains live after one validator is killed |
| Stale join | unconfirmed joiner remains pending until confirm-ready |
| Tribute projection | successful encrypted offer, inclusion/absence proofs, duplicate logical-offer rejection, four Mongo projections |
| Update | vote, approval, schedule, activation, oversized RPC pagination, intentionally unsupported version produces the expected fatal stall |
| Governance OIP/GIP | proposal, approval, and materialized Approved state |
| ZeroFee | EIP-7702 delegation, free quota consumption, quota rejection, and paid fallback |

## Flow coverage matrix

“Component” means in-process or contract evidence. “Live” means exercised through running nodes and public/operator interfaces. A component PASS does not promote a missing live composition to covered.

| Flow | Components | Existing evidence | Executed live evidence | Material gaps |
|---|---|---|---|---|
| PFS-001 encrypted Tribute materialization | TEE, Tribute, compressed entities, RPC, ExEx/Mongo | unit/property, Mongo integration, four Gherkin scenarios | 001-01/02/03/05; manual Mongo outage/recovery covers the operational substance of 001-06 | malformed ciphertext 001-04; projection crash failpoint 001-07; enclave outage 001-08; exact envelope replay 001-09 |
| PFS-002 worldwide-day Tribute to Nod | Cycle, Metadosis, Lysis, Tribute/Nod factories | component tests for 002-01/06 and partial admission | no composed live scenario | 002-02/03/04/07/08 and full day-boundary composition |
| PFS-003 Gratis pledge to Credis repayment | Gratis, Gratisfactory, Credis, factories, TEE | in-process coverage for 003-01/03/09/10/11/12; fragments of 003-02 | no composed live scenario | 003-04 through 003-08 and public-interface multi-step composition |
| PFS-004 Intex settlement to Promis | Intex, Desis, Promis, bridges, auction contracts | extensive Foundry and runtime fragments | no composed two-chain live scenario | end-to-end auction/bridge/settlement, failure/retry, ordering and replay across two live chains |
| PFS-005 governance update | Governance, Vote, Update, RPC, node lifecycle | unit coverage for 005-03/04/07/10/11/12 | live scenarios now include restart/rejection paths and focused 005-09 real binary replacement recovery | stateful 005-02, membership 005-05 and deliberately failing migration 005-06; final branch suite awaits review |
| PFS-006 validator lifecycle | ValidatorSet, Staking, DKG, consensus, TEE | broad unit/simulation plus restart tests | live scenarios now include exact exit/claim accounting, actual slash/idempotency and six restart checkpoints | 006-05/07/08/10 remain outside the current goal's requested composition; final branch suite awaits review |
| PFS-007 ZeroFee | EIP-7702 detection, admission, quota, accounting, txpool | unit/property plus native Alloy E2E | live scenarios implemented for 007-01 through 007-12, including replay/restarts/errors/day reset | final branch suite awaits review |
| PFS-008 follower recovery/promotion | follower resolver, finality, DKG, validator activation | unit/simulation plus composite features | live scenarios implemented for 008-01 through 008-08, including upstream loss and boundary restarts | final hardware-SGX branch suite awaits review |

## Confirmed defects and disposition

| Severity | Defect | Reproduction/impact | Cause | Disposition |
|---|---|---|---|---|
| High | Late warm-promoted verifier never activates | Broad hardware-SGX run stalled the promoted verifier at activation | DKG freeze notification was missed when the verifier joined after the transition point | Fixed `0b62a3b`; exact hardware regression passes |
| High | Payload built on non-canonical parent after forkchoice change | Hardware lifecycle emitted payload/forkchoice failures | payload arguments retained a stale prefinalization parent | Fixed `5e61927`; lifecycle passes 9/9 |
| High | Localnet full restart lost enclave state | all-enclave restart could not resume the active committee | localnet script did not persist/seal TEE state | Fixed `da03d06`; mock and hardware restart pass |
| High | Stale compressed payload attempts could outlive canonical work | lifecycle tests exposed stale parent/readiness and retry races | job cancellation and typed readiness propagation were incomplete | Fixed by `301b0e8`, `d267f65`, `0be40c6`, and `f787618`; regression suite passes |
| Medium | DKG share disclosure in healthy logs | log audit found share material on a non-error path | diagnostic path logged more than operational state | Fixed `26f78bd`; hardware audit count is zero |
| Medium | Pending Tribute asserted before mining | reruns could report missing projection for a valid pending transaction | E2E assumed immediate inclusion | Fixed `ed7bd37`; receipt/projection wait is explicit |
| Medium | CLI panics for empty/short/long private key | `outbe-cli ... --private-key ''` reached GenericArray length assertion | decoded `Vec` was converted through a slice without validating length | Fixed `06994a9`; unit and binary invocation return error, exit 1 |
| Medium | Fuzz target cannot run from its manifest | Cargo rejects the nested unlisted package; latest isolated parser is incompatible | missing isolated workspace marker and unpinned isolated graph | Fixed `47c99cb`; 9.7M iterations pass |
| Medium | Governance live test votes outside its configured window | Full mock E2E deterministically failed the governance scenario | scenario timing was inconsistent with proposal parameters | Fixed `61feeb1`; focused feature passes twice |
| Medium | Lifecycle E2E accepted submission without proving execution | a rejected/reverted Tribute could leave the scenario green | assertion stopped at transaction submission | Fixed `3d29b72`; successful receipt is mandatory and lifecycle passes 9/9 |
| Medium | Intent ABI exports stale | exact CI export changed Router and SolverEscrow ABIs | committed exports were not refreshed with source changes | Fixed `b02326a`; regeneration is clean |
| High | Prerelease Docker image did not build or launch | bounded amd64 build failed on missing builder `libc++` and Solidity interfaces; first built image then lacked runtime `libc++.so.1` | Dockerfile dependencies and context contract were incomplete | Fixed by `282c292`, `80f56e4`, and `72e1370`; exact-candidate build and smoke pass |

## Residual dependency and tooling risks

`cargo audit` reports:

- `hickory-proto 0.25.2`: RUSTSEC-2026-0119 (upgrade available) and RUSTSEC-2026-0118 (no fixed 0.25 release);
- `rsa 0.9.10`: RUSTSEC-2023-0071, timing side channel, no fixed release;
- `tracing-subscriber 0.2.25`: RUSTSEC-2025-0055, ANSI log injection, upgrade available.

Warnings include unmaintained `atomic-polyfill`, `bincode`, `derivative`, `paste`, and `proc-macro-error2`, plus unsound advisories for `anyhow 1.0.102`, `git2 0.20.4` (two), and `memmap2 0.9.10`. `cargo deny` additionally reports yanked transitive versions. Ownership and reachability must be assigned before release rather than suppressed globally.

Other explicit limitations:

- `cargo vet` is red because twelve git dependencies that match published versions lack an explicit `audit-as-crates-io` policy; granting that policy requires source-equivalence review, not a mechanical config edit;
- `cargo machete --with-metadata` reports 22 possibly-unused declarations across eight packages, while all-features `cargo udeps` passes; each finding therefore needs package/feature-specific validation before removal;
- current nightly reports future incompatibilities in external `discv5`, `proc-macro-error2`, and several Reth crates;
- precompiles compile but have zero Foundry tests;
- the dormant EVM trampoline has no meaningful implementation to verify;
- smart-account lint has a remapping-resolution failure;
- hardware enclave execution and sealing pass, but DCAP quote provisioning does not;
- several business flows have component coverage but no live composed E2E;
- the first block after DKG activation can use degraded VRF seed bootstrap and emit `vrf_verified=false`, although no VRF alarm was raised.

## Proposed next tests

Priority 0 before an unconditional GO:

1. DCAP quote generation and verification against the deployment's actual PCCS/AESM configuration.
2. Stateful PFS-002 day-boundary E2E: offer, close day, Metadosis, Lysis, Nod materialization, all receipts/events/RPC/Mongo checks, restart at every phase boundary.
3. Two-live-chain PFS-004 auction-to-Promis E2E with duplicate delivery, out-of-order delivery, bridge outage, retry and replay assertions.
4. Projection failpoint test that terminates the projector between transaction-body and index/checkpoint commits, then proves atomic recovery.

Priority 1:

1. Invalid Tribute ciphertext and exact signed-envelope replay through public RPC.
2. ZeroFee quota persistence across validator/full-committee restart and exact transaction replay.
3. Follower upstream partition and restart during warm promotion.
4. Governance restart/recovery around schedule and activation, including rollback/operator procedure after an unsupported version.
5. Complete validator exit/slash accounting, including stake/value effects rather than liveness only.
6. Fix smart-account lint remappings and require it in CI.
7. Add real tests to the precompiles suite or remove the misleading zero-test gate.

## Operator release checklist

1. Record the reviewed release SHA; do not deploy directly from an unreviewed verification branch.
2. Re-run format, strict Clippy, full nextest, doctests, Foundry suites, audit/deny and exact release binary builds.
3. Run the canonical full mock E2E and hardware-SGX E2E with `--all` on the release SHA.
4. Require zero unexplained fatal, panic, DKG-share, VRF-alarm, EPC/resource and projection-fatal records.
5. Verify four-node head/finality/root equality, public CLI transaction receipt, RPC read and all four Mongo projections.
6. Stop/start the entire committee and repeat the state checks.
7. Resolve or formally accept every residual risk above before changing testnet.
