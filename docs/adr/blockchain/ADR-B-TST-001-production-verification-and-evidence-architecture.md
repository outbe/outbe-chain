# ADR-B-TST-001: Production verification and evidence architecture

- **Status:** Proposed; broad tests exist but project-wide evidence closure is incomplete
- **Date:** 2026-07-17
- **Decision owners:** All Architecture Spaces, release engineering and verification maintainers
- **Scope:** verification layers, production-interface evidence, CI/release gates and ADR coverage ledger
- **Depends on:** every indexed module-owner ADR
- **Related:** every System/Core ADR and every PFS

## Context

Outbe has thousands of useful tests across Rust modules, Commonware deterministic
simulation, EVM integration, property/reference models, Mongo conformance, Forge
contracts and a multi-process Cucumber localnet. These tests answer different
questions. A handler test using `HashMapStorageProvider` can prove an FSM invariant
but cannot prove Reth journaling, payload assembly, RPC submission, Mongo atomicity
or supervisor shutdown. A localnet happy path can prove wiring but is too coarse to
exhaust illegal states or crash boundaries.

The ADR catalog is intended to be source of truth and evidence input for
`module-audit tooling`. Therefore “there are tests” is insufficient. Every normative
invariant, mutation boundary, failure class and production path needs an explicit,
reviewable evidence claim with a known test layer and current CI execution status.

This ADR defines that evidence architecture. It does not duplicate module-specific
test cases; each owner ADR lists its required evidence and this ADR defines how those
claims become trustworthy release gates.

## Decision

### Evidence ledger is part of the ADR catalog

Maintain a machine-checkable `VerificationLedger` generated from ADR/PFS evidence
declarations. Each requirement has:

- stable requirement ID, owner ADR/PFS section and risk tier;
- exact invariant/transition/failure it proves;
- production entrypoint and side effects in scope;
- test ID, layer, source path and command;
- real components and substituted seams, with justification;
- positive, negative, replay/retry and fault boundaries covered;
- CI lane, frequency, platform and required/advisory status;
- last verified revision/artifact and result; and
- explicit gap/expiry when evidence is absent, ignored, quarantined or stale.

A source file mention, coverage line or passing neighboring test cannot satisfy a
requirement. One test may satisfy multiple IDs only when each assertion is explicit.
Every ADR's `Production-interface verification evidence` and
`Open questions and technical debt` sections reconcile with the ledger.

### Verification layers

Evidence is classified, never flattened into “unit” versus “e2e”:

| Layer | Proves | Does not prove by itself |
|---|---|---|
| Pure/reference | formulas, codecs, canonical vectors, overflow/bounds | storage, ABI or lifecycle wiring |
| Stateful model/FSM | legal/illegal transitions and invariants over generated sequences | production adapter equivalence |
| Module contract | public ABI/dispatch, authorization, journaling and events using production module code | whole-node ordering/networking |
| Differential/conformance | two implementations/backends obey identical observable contract | either is complete without an independent oracle |
| Execution integration | proposer/validator/import/replay parity through real executor/storage seams | multi-process consensus and external deployment |
| Deterministic distributed simulation | Byzantine timing, partitions, retries, epochs and finality safety/liveness | OS/process/RPC/database behavior |
| Process/localnet e2e | released binaries, genesis, RPC/CLI, consensus, Reth, Mongo/TEE and restart wiring | exhaustive state/fault space |
| Crash/durability | kill/fault at every persistent boundary and exact recovery | protocol correctness outside tested boundary |
| Compatibility/upgrade | historical replay, mixed versions, activation, snapshot/import | new feature semantics alone |
| Capacity/security | maximum-shape benchmark, fuzz, adversarial resource and exploit properties | functional business completeness |

Names such as `e2e`, `integration` or `conformance` do not assign the layer; inspected
components and assertions do.

### architectural evidence minimum per stateful module

Before a module can claim Implemented evidence, its ledger covers:

1. every public mutation entrypoint and caller/role/value/static-call guard;
2. complete legal transition table plus generated illegal transition attempts;
3. persistent invariants and all index/count/list equivalences after arbitrary
   successful and failed sequences;
4. atomic rollback at every storage/event/subcall/external effect boundary;
5. replay, duplicate intent, retry after uncertain result and reorg behavior;
6. arithmetic extrema, collection/capacity limits and gas/work proportionality;
7. strict ABI/codec/schema/version/migration and malformed input behavior;
8. production dispatch/address/registry/storage layout and genesis activation;
9. proposer, validator/import and historical replay parity where consensus-relevant;
10. required cross-module seams through PFS tests without absorbing their ownership;
11. observability/fatality/readiness behavior for local failures; and
12. at least one test through the actual production interface for every critical
    mutation and failure class.

Mock/state-model tests remain valuable evidence but declare the assumptions that a
higher layer must discharge.

### Production-interface rule

A production-interface test enters through the same public boundary and uses the
same registry, codec, executor, storage transaction, task ownership and output path
as production. Test-only constructors/providers/hooks are permitted only if the
requirement explicitly excludes the substituted behavior and a separate equivalence
test proves the seam.

In-memory storage does not prove Reth/MDBX/Mongo atomicity. Direct handler calls do
not prove ABI dispatch, precompile call frames or gas. Mock automata do not prove
execution delivery. Gramine mock proves enclave protocol wiring but not SGX
attestation/sealing hardware. Noop settlement proves orchestration around the hook,
not value movement. These limitations appear in the ledger, not only comments.

### Required project suites

The release verification graph contains:

- module unit/FSM/property/compile-fail suites for all workspace crates;
- generated storage/ABI/address/crypto/genesis manifest conformance;
- proposer-validator-import-replay differential execution and state/receipt/header/
  CE-root parity;
- Commonware deterministic Byzantine simulations across committee sizes, missed
  views, partitions, reordering, duplication, DKG rotation and recovery;
- real Mongo replica-set conformance/projection/restart/lease/crash tests;
- CE independent reference vectors, random mutation model, MDBX crash/recovery and
  proof corruption tests;
- multi-process validator and certified-follower Cucumber scenarios implementing
  every PFS, including negative and restart paths;
- mock-Gramine and real-SGX lanes with clearly different evidence claims;
- snapshot/bootstrap, upgrade/mixed-version and historical replay tests;
- RPC/CLI/feeder/operator automation and exit-code/readiness tests;
- Forge plus Rust cross-language predeploy/ABI/storage conformance; and
- fuzz, Miri/sanitizer, dependency/security, reproducibility and ADR-B-CAP-001 capacity
  suites.

Every production feature has a blocking presubmit subset and a required scheduled
full suite with an owned response SLA. Nightly is evidence only if failures alert an
owner, remain visible, are triaged and block release until resolved.

### Determinism, isolation and artifacts

Tests allocate unique ports, directories, database names, chain IDs and process
groups; cleanup cannot touch operator data. Seeds, virtual-time schedules, binaries,
genesis, manifests and dependency commits are recorded. A failure preserves bounded
redacted logs, supervisor snapshots, block/receipt/checkpoint identities and the
minimal reproduction command.

Flaky behavior is a finding. Retries may measure reproducibility but cannot turn a
failure green. Quarantined/ignored tests have owner, requirement IDs, reason and
deadline; their requirements remain Gap. Environment-unsatisfied scenarios fail in
the lane that claims full coverage.

### Fault and adversary matrix

For every durable/external boundary, deterministic injection fails before and after
the effect, response and acknowledgement. The matrix includes panic/abort/kill,
short/corrupt writes, disk full, clock movement, lost lease, Mongo/TEE/RPC/network
outage, duplicate/reordered messages, Byzantine signatures/proofs, queue overload and
simultaneous failures.

Assertions cover pre-state restoration or exact committed state, restart
classification, no hidden effects, stable failure code, readiness removal and
idempotent retry. Source-text assertions and “function was called” mocks cannot prove
semantic rollback.

### CI and release gates

The verification graph is fail-closed:

- required jobs cannot be skipped by branch/event expressions without an explicit
  policy result;
- coverage generation has risk-weighted requirement thresholds, not only line
  percentages or a report comment;
- ignored/@todo/unsatisfied scenarios are counted and fail a full-coverage lane;
- test discovery manifests detect renamed, filtered-out or zero-test commands;
- exact commands/toolchains/features match the release build;
- release consumes results for the exact commit/artifacts it publishes; and
- advisory security/quality lanes have a deadline to become required or an accepted
  risk record.

CI produces a signed evidence manifest listing every requirement result, skipped/
ignored/quarantined test and artifact digest. `Implemented` status and release gates
consume this manifest rather than assuming a workflow name succeeded.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| Required evidence per behavior | owner ADR/PFS requirement IDs |
| Test-to-requirement mapping | generated `VerificationLedger` |
| Layer/substitution classification | inspected test manifest |
| Required commands/frequency/platform | CI verification graph |
| Release evidence | exact-commit signed evidence manifest |
| Unproven behavior | ADR debt plus ledger Gap/Expired state |

## Invariants

- Every normative ADR/PFS requirement is Proven, Gap, Contradicted or Expired; never
  silently absent.
- A mock/in-memory test cannot claim behavior of a substituted production component.
- Ignored, skipped, todo, quarantined or advisory evidence cannot satisfy a required
  release claim.
- Critical persistence/consensus behavior has negative, replay and fault evidence,
  not only a happy path.
- Test and release artifacts name the exact source/dependency/genesis/profile identity.
- A retry never erases the original failing result.
- Full-suite lanes fail when their required environment or scenario is unavailable.
- Production secrets/user payloads are absent from retained test artifacts.

## Atomicity, replay and failure

The evidence ledger is generated atomically from one repository revision and CI
result set. Partial/stale results cannot be mixed with a newer commit. Cancelled,
timed-out or infrastructure-failed jobs are Not Proven unless a policy explicitly
classifies and reruns them on the identical artifacts.

Tests themselves restore isolated resources through owned handles and bounded cleanup.
A harness crash must not report success; its supervisor records child exit and retains
diagnostics. Replay tests start from durable bytes produced by the production path,
not reconstructed fixtures that bypass the failure under test.

## Compatibility and migration

Requirement/test IDs are append-only or explicitly superseded. Changing a normative
ADR invariant invalidates affected evidence until remapped and rerun. Golden vectors
carry format/profile versions; updates require old-version verification plus new
activation evidence. CI workflow/toolchain changes are reviewed as verification
architecture changes because they can silently stop executing claims.

## Production-interface verification evidence

Inspected workspace tests, main CI/nightly workflows, `mise` commands, the Rust
Cucumber harness/features, `crates/core/e2e`, Commonware deterministic harness,
EVM/CE/Mongo integration and conformance suites, ignored tests and Forge lanes.
There is strong real evidence for multi-validator bootstrap/restart and encrypted
Tribute-to-Mongo/CE proof flow, plus extensive component-level parity/property tests.
However, no machine-checkable ADR requirement ledger exists, several real-backend
tests are ignored/environment-gated, and some “e2e” tests explicitly substitute
storage and settlement. Status remains Proposed.

## Consequences

Test quantity and line coverage stop standing in for architectural proof. Reviewers
can see exactly which production claim is established, mocked or missing; architecture
audits can request the right next layer; release confidence grows without discarding
fast unit/model suites.

## Rejected alternatives

- **Use line coverage as completion:** executed lines do not prove invariants,
  rollback, assertions or production seams.
- **Require every test to be full localnet e2e:** slow coarse tests cannot exhaust FSM
  and fault spaces and are difficult to diagnose.
- **Call any cross-crate test e2e:** naming does not prove process/storage/network
  fidelity.
- **Allow ignored tests as documentation:** an unexecuted assertion is a Gap.
- **Retry flaky tests until green:** it hides nondeterminism and corrupts evidence.

## Open questions and technical debt

1. **Critical:** no machine-checkable mapping exists from every ADR/PFS invariant,
   transition and debt claim to exact tests, production interfaces and CI lanes.
   Build the `VerificationLedger` before claiming complete project coverage.
2. **Critical:** `crates/core/e2e` uses `HashMapStorageProvider`, `MemoryStorage` and
   explicit noop settlement in important flows. Reclassify it as execution/module
   integration and add process-level production tests for the undisproved seams.
3. **Critical:** several Mongo production-backend tests are `#[ignore]` even though CI
   starts a transaction-capable Mongo replica set. Add an explicit required
   `--ignored`/filtered lane and fail if zero expected tests execute.
4. **Critical:** the Cucumber harness treats `@todo` as always skipped even with
   `--all`. A lane claiming all scenarios must fail on any todo requirement and
   report the exact unimplemented scenario count.
5. Per-PR localnet smoke is opt-in and `continue-on-error`; nightly is required only
   after changes have merged. Define which high-risk paths block the originating PR
   and remove timing flakiness rather than weakening the gate.
6. Real SGX and full mock-Gramine e2e depend on a self-hosted `sgx` runner. Prove the
   runner exists, alert on absence/queue starvation and block releases on stale or
   failed evidence.
7. The ordinary CI uploads/comments line coverage but no enforced threshold was found.
   Add risk-weighted requirement thresholds; do not optimize a global line number.
8. `cargo llvm-cov --workspace` does not prove doctest/ignored/external service/
   Cucumber/Forge execution. Publish test-discovery counts per suite and layer.
9. Inventory every ignored test. The full-block call-trampoline differential is an
   explicit skeleton and therefore a real production evidence gap, not coverage.
10. Remove or close stale comments that claim ignored Phase-1 tests if attributes have
    changed; ledger generation should read actual discovery output, not prose.
11. Add production Reth proposer/validator/import/replay differential tests for all
    begin/end system transaction phases, subcalls, receipts, state roots, CE roots and
    failure paths. Many current EVM tests use direct/in-memory providers.
12. Extend deterministic consensus simulation from happy zero-latency fully connected
    defaults to generated partitions, loss, jitter, Byzantine equivocation, delayed
    DKG activation, restart and committee-size boundaries with safety/liveness oracles.
13. The consensus harness uses `MockAutomaton`, `MockRelay` and `MockReporter`; add
    equivalence and process-level tests for application/executor/marshal delivery
    seams before using it as whole-node evidence.
14. Add crash-injection tests at Reth, marshal ACK/archive, CE MDBX marker, Mongo
    document/checkpoint and snapshot activation boundaries required by ADR-B-OCD-007 and ADR-B-OCD-015.
15. Mongo projection e2e covers Tribute well; extend it to every projected Core/System
    event/index and verify reorg/restart/duplicate/lease-loss/corruption behavior.
16. Cucumber features cover validator lifecycle/update/DKG/downtime/restart/follower
    and Tribute, but the PFS catalog contains other cross-module sagas. Add one
    feature/matrix per PFS requirement, including negative compensation paths.
17. Mock enclave tests do not prove SGX measurement, quote verification, EGETKEY
    sealing, anti-rollback or hardware failure. Keep separate ledger claims and add
    repeatable real-hardware negative tests.
18. Add validator/follower mixed-version protocol activation, rollback-limit and
    historical replay tests for ADR-S-GOV-003 and ADR-B-CRY-001 profiles.
19. Add authenticated snapshot export/import/corruption/catch-up e2e after ADR-B-OCD-015
    implementation; current restart smoke is not new-node snapshot evidence.
20. Add node supervisor fault tests for early consensus-thread death, actor panic/
    return/hang, readiness publisher loss and simultaneous fatal causes from
    ADR-B-SUP-001.
21. Add ADR-B-CAP-001 maximum-shape block/transaction/proof/queue benchmarks on declared
    minimum hardware with pass/fail budgets; Criterion reports alone are not gates.
22. Run fuzz targets continuously with a corpus, sanitizer and minimum duration;
    workspace membership of a fuzz target does not mean CI executes it.
23. Miri is opt-in/advisory by commit marker. Define the unsafe surface and make a
    scheduled required result part of release evidence, or record accepted exclusions.
24. `cargo deny` and `cargo machete` are non-blocking. Establish promotion criteria,
    exception expiry and release policy for known vulnerabilities/licenses/supply
    chain findings.
25. Add independent reference/state-machine models for every high-value ledger/FSM;
    existing CE, gas and Fidelity models are good patterns but coverage is uneven.
26. Add mutation testing or equivalent assertion-strength audits for critical guards,
    rollback and authorization. Line execution can remain green after removing an
    essential check.
27. Pin test seeds and preserve minimized failing cases while also varying scheduled
    seeds. Random-only unrecorded failures are not reproducible evidence.
28. Ensure every harness child/container/process exit is supervised and a cleanup
    failure cannot overwrite the original test cause or return success.
29. Produce bounded redacted failure artifacts containing exact binary/genesis/
    profile/dependency identity, supervisor state and checkpoints; current logs are
    distributed across scripts/processes.
30. Make release workflow consume the exact-commit evidence manifest and reject Gap,
    Contradicted, Expired, skipped required lanes or results produced for different
    binaries/configuration.
