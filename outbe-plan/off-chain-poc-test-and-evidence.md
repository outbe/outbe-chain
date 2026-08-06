# Off-chain PoC: test architecture and completion evidence

Status: **ALIGNED ON 2026-07-26 — FULL-RESULT VOTE, QUORUM APPLY AND
ACCOUNTABILITY EVIDENCE REQUIRED**

Scope: the tests, production boundaries, allowed oracles, evidence records and
CI gates that can prove the bounded Lysis V1 PoC was implemented. This document
does not implement tests or production code and does not promote BoundedMVP
crash/operations work into PoC.

Normative inputs:

- [`off-chain-poc.md`](../off-chain-poc.md), especially sections 13, 14 and 17;
- [`PFS-002`](../docs/flows/002-off-chain-poc-protocol-flow.md);
- [`ADR-S-OCM-001`](../docs/adr/system/ADR-S-OCM-001-ocomp-kernel-and-typed-program-boundary.md)
  through
  [`ADR-S-OCM-004`](../docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md);
- [`ADR-B-TST-001`](../docs/adr/blockchain/ADR-B-TST-001-production-verification-and-evidence-architecture.md);
- the [protocol freeze](off-chain-poc-protocol-freeze.md);
- the [input/export decision](off-chain-poc-finalized-input-export.md);
- the [process/CAS decision](off-chain-poc-process-and-artifact-topology.md);
- the machine-readable
  [planning ledger](off-chain-poc-evidence-ledger.yaml).

## 1. Completion rule

The PoC is proven only when one exact source revision and one exact set of built
artifacts produce a verified evidence bundle in which:

```text
all mandatory ADR invariant records = PASS
and POC-01..POC-26 = PASS
and every required PFS-002 row = PASS
and story steps 1..13 = PASS
and PFS-002-03 = RETIRED with its unreachable-production-state reason
and PFS-002-07/-08 = DEFERRED with their fixed reasons
and skipped/todo/quarantined/missing/retried-away assertions = 0
and closure verifier result = PASS
```

Workflow success, line coverage, a transaction receipt, supervisor state or an
unverified JSON `"passed"` field is not completion. The closure verifier
recomputes coverage and artifact bindings from the repository ledger and the
retained evidence.

The strongest product outcome is:

```text
third matching full-result vote transaction
  -> atomic quorum apply in that transaction
  -> finalized quorum-forming block
  -> canonical terminal job and ActiveGenerationV1
  -> public owner state, receipts and CE proofs
  -> independently checked conservation and replay parity
```

Mongo, CAS and supervisor journals can prove their own local requirements but
never the canonical outcome.

## 2. Evidence layers and allowed oracles

No single layer may claim a boundary it substitutes.

| Layer | What it proves | Required production boundary |
|---|---|---|
| `BYTE` | OCB1/ABI/hash/signature bytes, malformed input and generated caps | production codecs plus independent encoder/decoder |
| `PURE` | Lysis arithmetic, planner/reducer and receipt equations | storage-independent production functions plus independent reference |
| `MODEL` | legal/illegal FSM sequences, indexes, retry and deadline invariants | generated state sequences checked against the frozen transition table |
| `MODULE` | dispatch, authorization, owner methods, checkpoint rollback and events | production module entrypoint; substitutions declared |
| `PROCESS` | public-RPC export, CAS, Axum registration, ZeroMQ workers, voting and restart | real sibling processes and real files/TCP endpoints/databases |
| `EXECUTION` | proposer/import/replay ordering, public transaction and state parity | real executor/storage/system-transaction path |
| `E2E` | consensus/finality/RPC/P2P/process wiring and public outcome | fresh four-validator devnet using released-form binaries |

Allowed oracle types are closed:

| Oracle ID | Authority |
|---|---|
| `CANONICAL_BYTES` | exact bytes recomputed by production and independent implementations |
| `REFERENCE_DIFF` | independent test-only Rust reference result versus native Lysis result |
| `FSM_MODEL` | generated legal/illegal transition and invariant model |
| `FINALITY_PROOF` | verified finalized header/certificate/state proof |
| `FINALIZED_PUBLIC_STATE` | exact-block `eth_call`, `eth_getProof`, OCOMP views and CE proof package |
| `PUBLIC_TX_RECEIPT` | canonical transaction, receipt, log and inclusion/finality references |
| `STATE_ROOT_DIFF` | exact common-height pre/post state roots plus scoped public proofs |
| `MODULE_STATE` | exact typed production storage fields, events and rollback snapshot before/after one focused module dispatch |
| `PROCESS_BOUNDARY` | harness-owned child state, PID/socket identity and exit status |
| `DURABLE_JOURNAL` | exact persisted pin/sign-once bytes verified after process restart |
| `DIGESTED_ARTIFACT` | same-descriptor length/digest/manifest-membership verification |
| `RESOURCE_COUNTER` | decoder/allocation/crypto/work counters at a real bounded interface |
| `STATIC_BOUNDARY` | compile-fail, dependency and compiler/type-system-enforced capability boundary checks; never source-text matching |
| `RUNTIME_BOUNDARY_TRACE` | structured proposer/import/replay boundary trace tied to exact blocks |

The following may not close a claim:

- a direct Lysis, quorum-apply or owner-handler call when the claim is public
  vote dispatch, execution, networking or atomic apply;
- `HashMapStorageProvider`, memory CAS, mock validator domains or a shared
  calculator for persistence/process/distributed claims;
- Mongo count/order, CAS pathname or supervisor journal as chain authority;
- on-chain Lysis as a comparison oracle;
- an unverified log line, screenshot, manual step or source-text assertion;
- a retry that hides the first failure.

Focused module tests may use a declared failpoint or in-memory adapter when the
ledger names it and the assertion is limited to the production module
entrypoint, owner sequence and checkpoint. A higher-layer test must discharge
it only when the claim extends to persistence, process, public execution or
distributed behavior.

## 3. Reuse the existing harness; close only its OCOMP gaps

`testing/e2e-harness` already provides:

- a fresh per-scenario four-validator localnet owned by Rust `ChildGuard`s;
- a harness-owned transaction-capable MongoDB replica set with one logical
  database per validator;
- native Alloy JSON-RPC submission, receipt reads and finality polling;
- validator kill/restart and finalized state-root parity checks;
- public compressed-entity proof retrieval and independent verification;
- isolated ports/directories, signal cleanup and retained failure data;
- per-scenario JSON containing source identity, invocation, result and log audit.

The PoC extends this owner instead of creating another harness. Required gaps
are:

1. an `OcompTopology` handle owning four Supervisor/exporter/CAS domains,
   bounded one-unit Worker processes, Axum registration endpoints and ZeroMQ
   TCP command channels;
2. Supervisor-only stop/restart, message-drop, CAS/Mongo corruption, Worker
   schedule and bundle-mismatch controls;
3. public OCOMP transaction/view/proof helpers and exact-block state snapshots;
4. a deterministic persisted-finality/orphan fixture driving the production
   pin coordinator, durable journal, restart recovery and attestation gate;
5. structured forbidden-call traces for request and quorum-forming vote blocks;
6. evidence schema V1 for OCOMP assertions and one run-level manifest;
7. an independent evidence verifier and exact test-discovery check;
8. a clean-source check that includes tracked changes and untracked files (the
   current generic evidence dirty bit intentionally ignores untracked files).

Current generic scenario evidence remains compatible. OCOMP adds nested records;
it does not replace unrelated features or make existing `@todo` scenarios part
of PoC closure.

## 4. Stable test catalog

The planning ledger is the machine authority for mappings. Every test below is
`planned`, not claimed to exist.

### 4.1 Fast byte, pure and model gates

| Test ID | Production subject | Assertions and primary evidence |
|---|---|---|
| `OCM-EVD-001` | independent evidence/coverage verifier | complete synthetic bundle passes; missing, mixed-revision, skipped, retried-away, stale and hash-corrupt bundles fail closed |
| `OCM-BYT-001` | OCB1 registry and every top-level type | production versus independent encode/decode; positive, truncation, trailing, enum/order/cap mutations; exact golden files |
| `OCM-BYT-002` | SystemTx kinds, ABI selectors, errors, events and hash-domain registry | Alloy/Foundry-independent selector/topic parity, collision scan, request/end-zone envelope bytes |
| `OCM-CAP-001` | generated `CapacityProfileV1` and maximum fixtures | no total Tribute cap; reproducible per-shard/per-chunk/concurrency caps; shard-cap+1 is accepted and covered by two shards; 10,000/1,000,000,000 derive exact unit counts without proportional plan allocation; full-result vote cap-1/cap/cap+1 for both non-quorum and quorum-forming paths; checked worst-case per-interface work bill; exact machine facts and five cold runs leave at least 20% headroom; generated constants match genesis/Rust |
| `OCM-FIN-001` | `FinalizedIntentProofV1` | valid proof and wrong hash/root/height/committee/bitmap/certificate mutations |
| `OCM-SEM-001` | storage-independent Lysis V1 | native/reference JSONL differential including zero, rounding, overflow, league/currency/exclusion and first-error corpus |
| `OCM-SEM-002` | planner, units, coverage and reducers | every phase/range/padded tree vector, including both prefix-scan directions and bounded shuffle merges; `S-1/S/S+1` partitions with `S+1` in shard 2; 10,000/1,000,000,000 count derivation; exact no-gap/no-overlap coverage; producer membership; deterministic roots and no self-reference |
| `OCM-FSM-001` | request/expiry/conflict/completion model | arbitrary legal/illegal sequences, exact indexes/budget split, nonce/attempt, exclusive deadline, retry backoff and terminal cap |
| `OCM-APL-001` | result/receipt/conservation verifier | result-chunk catalog/root/count/total/event/receipt mutations, old-root-to-new-root binding and GREEN/RED equations |
| `OCM-BND-001` | private quorum-apply capability and owner boundary | compile-fail external constructors, module-private factory, behavioral four-call cursor and denial by every non-OCOMP provider; no raw/generic write bypass |
| `OCM-BND-002` | post-fork no-compute dependency boundary | request/vote/apply crates cannot import or call mutating Lysis/Fidelity/Oracle calculation modules |
| `OCM-BND-003` | bounded decoders and checked work | cap+1 rejection precedes proportional allocation, hashing and signature verification; arithmetic cannot wrap |

Planned source owners:

```text
crates/system/ocomp-protocol/tests/
crates/core/lysis/tests/
crates/core/metadosis/tests/ocomp_fsm_model.rs
crates/core/lysis/tests/compile_fail/
testing/e2e-harness/tests/ocomp_evidence_verifier.rs
```

### 4.2 Production-seam integration gates

| Test ID | Real seam | Required evidence |
|---|---|---|
| `OCM-REQ-001` | payload builder -> begin/user/CE-seal/end executor -> Metadosis | split, exact GREEN Desis or RED carry-over, intent/index/event commit together; final CE root is bound; no Nod/contributor/Tribute-consume effect; retry does not repeat the early effect |
| `OCM-PIN-001` | deterministic consensus boundary -> production node pin coordinator/journal/attestation gate | tentative record durable before positive vote; finalize/export transition; exact competing finality releases the orphan; restart preserves refusal; the real gate returns typed `NotExported` without creating a sign-once record |
| `OCM-DIS-001` | finalized cursor -> public RPC discovery | dropped event still discovers one job; duplicate/restart remains exactly once |
| `OCM-EXP-001` | retained CE MDBX + real Mongo + historical openings -> exporter | complete fold/root/count/nominal; parent-job retention and cursor GC across at least `S+1` Tribute; deterministic left-first opening-proof bisection preserves owner order/completeness under the control cap and a one-owner oversize abstains; body omission/change, opening mutation, source-ahead/behind and exporter restart yield exact export or abstention |
| `OCM-CAS-001` | exporter/supervisor/worker filesystem CAS | atomic publish, same-descriptor verify, membership/order/length; truncate/change/reorder/TOCTOU/quota faults never reach signing |
| `OCM-CTL-001` | Supervisor Axum registration plus ZeroMQ/TCP Worker transport | registration binding, stale generation, bounded 1..4 registry, asynchronous dispatch, cancellation/redelivery and stale completion rejection; blocks continue while OCOMP is unavailable |
| `OCM-DET-001` | real worker processes and reducer | one `S+1` parent job produces two primary shards; 1/2/4 workers, randomized completion, kill/retry and cache hit/miss produce byte-identical plan commitment/result roots/digest; removing either shard prevents reduction/signing |
| `OCM-SIG-001` | node attestation gate + real sign-once filesystem | file/directory fsync-before-release, fault at each persistence boundary, exact retry, different digest refusal before/after restart |
| `OCM-VOT-001` | public full-result vote dispatch -> four compact slots -> quorum | each vote carries canonical `LysisResultV1`; exactly three matching eligible indexes form q; duplicate, unknown, wrong epoch/key, malformed result, minority and equivocation cases are bounded and deterministic; only one canonical result is retained at q |
| `OCM-APL-002` | quorum-forming vote dispatch -> one outer checkpoint -> four private owner APIs plus the stored request split receipt | table-driven failure after slot/quorum and after each owner, plus mutation of each result/request receipt; unexpected failure rolls back the third slot and all owner effects; exact retry succeeds; expected stale owner precondition terminates `CONFLICTED` without owner effects |
| `OCM-TIM-001` | request logical context -> quorum-applied owner effects | different valid quorum-forming heights preserve semantic projection; only declared apply metadata differs |

Module failpoints are named, test-only and selectable only in the focused
integration binary. They run after real preceding writes and cannot construct a
success capability, result or receipt.

### 4.3 Public fork/execution gates

| Test ID | Real path | Required evidence |
|---|---|---|
| `OCM-PUB-001` | RPC -> txpool -> P2P -> proposal -> import -> replay | cap-1 and cap accepted; cap+1 rejected consistently; same receipt/state/CE/header result on all nodes |
| `OCM-PUB-002` | public `submitLysisResult(bytes)` changed-binding rejection and recovery | one representative changed binding rejects with exact scoped pre/post equality; restarting the stopped supervisors forms the valid quorum through RPC/txpool/P2P/import |
| `OCM-PUB-003` | begin-zone expiry versus public full-result votes | height `< deadline` may fill a slot and q may apply; height `= deadline` first expires a non-quorum job and rejects a new slot; no proposer-order race; a terminal job still accepts the fourth timely accountability vote |
| `OCM-PUB-004` | completed full-result vote replay | duplicate same-validator vote is idempotent with no new owner effects/events; changed binding or equivocation follows the frozen rejection/evidence rule and cannot change the terminal result |

These tests use block production and import/replay APIs, not a direct executor
injection. The positive apply and completed replay assertions (`OCM-PUB-001`
and `OCM-PUB-004`) intentionally share one completed-job scenario. Expiry and
pre-quorum mutation remain separate because they require incompatible terminal
states.

### 4.4 Four-domain gates

| Test ID | Scenario |
|---|---|
| `OCM-E2E-001` | one tracer public Tribute -> finalized JobIntent -> four independent compute domains -> q=3 -> certified Nod, with exact request/quorum replay trace |
| `OCM-E2E-007` | one incompatible supervisor refuses OCOMP while its node continues finality |
| `OCM-E2E-008` | completed nodes and compute processes restart/replay and select the same public active generation without CAS authority |
| `OCM-TRC-001` | second assertion over the retained `OCM-E2E-001` tracer scenario: proposer/import/replay traces for exact request and q-forming vote blocks contain zero calls to mutating Lysis, Fidelity league and Oracle calculation boundaries |

Stable IDs whose names historically contain `E2E` remain valid but no longer
start four nodes:

| Test ID | New lane | Focused proof |
|---|---|---|
| `OCM-E2E-002` | `OCM-INT` | production Metadosis empty-day branch, Desis brief, carry-over and zero Job/Nod state |
| `OCM-E2E-004` | `OCM-INT` | production Tribute changed-body duplicate rejection with unchanged supply, totals, pre-admission and owner/day indexes; `PFS-002-05` separately also requires `OCM-EXP-001` exact export completeness |
| `OCM-E2E-006` | `OCM-INT` | production q-forming dispatch and outer checkpoint rollback for every owner failure, followed by exact retry on the same fixture |

`OCM-E2E-003` and `OCM-E2E-005` are retired tombstones and cannot be reused.
The former depended on an unreachable zero-limit premise; the latter attempted
to prove a validator-local retention invariant by timing a live consensus
proposal micro-window. `PFS-002-10` is instead closed by `OCM-PIN-001` in the
required `OCM-INT` lane.

### 4.5 Existing Tribute harness reuse contract

OCOMP is an extension of the existing
`testing/e2e-harness`, not a parallel runner. The executable baseline is
`features/tribute_projection.feature` backed by
`src/features/tribute_projection.rs`, `World`, `Rpc`, `MongoDb`, `Localnet` and
the existing CE point-read verifier. `OCM-24` adds one registered
`src/features/ocomp.rs` module and one `world::ocomp` handle; it does not add a
second Tribute sender, Mongo client, CE verifier or process owner.

The `OCM-E2E-001` scenario must complete this prefix before it may claim that a
job exists:

```text
public encrypted Tribute transaction
  -> successful receipt + inclusion + finalized reference
  -> identical primary/owner/day projections on four validators
  -> independently verified finalized CE point read
  -> CE body bytes equal projected Mongo body bytes
  -> correlated production snapshot/manifest
  -> production Metadosis JobIntent
```

The correlation record carries transaction/block/header, owner/day, entity ID,
projection digest and CE proof/root through the later manifest, job, result and
vote, quorum-apply and terminal records. The harness observes the `JobIntent` and roots produced by
the real exporter/Metadosis path; no step may inject a root, manifest, job,
result or consensus state. The existing Tribute projection feature stays
independently runnable and does not depend on OCOMP. Shared behavior uses typed
`World` handles rather than one Gherkin step invoking another.

Duplicate public admission remains independently covered by the existing
Tribute projection feature. The OCOMP closure does not repeat a complete
Lysis generation for that rejection: `OCM-E2E-004` now composes the focused
production Tribute admission test with `OCM-EXP-001`, which proves exact export
membership from retained accepted identities.

## 5. Exact four-validator demonstration

### 5.1 Fresh topology

One closure run builds binaries once and creates a fresh generated devnet:

```text
validator domain 0..3
  outbe-chain node               owns consensus, pin and OCOMP key
  outbe-ocomp supervisor         owns scheduling journal
  outbe-ocomp snapshot-exporter  read-only checkpoint + Mongo access
  validator-local CAS directory  never chain authority
  Supervisor-launched workers    1..4 bounded one-unit child processes

validator node vote submitter
  receives one bounded signed result from its local supervisor
  submits through the restricted validator ZeroFee transaction seam
```

Each domain has a distinct node data directory, Mongo logical database, CAS,
pin/sign journal, loopback TCP namespace and OCOMP key/index. Operating-system
service-manager hardening remains outside the PoC: it is an MVP deployment
concern, not protocol evidence.

There is no relay or public activation transaction. Each validator domain submits
its own full-result vote. The transaction that records the third matching slot
also applies the result under one outer checkpoint. Nodes remain running when
supervisors, exporters, workers or CAS access are stopped.

### 5.2 Thirteen-step proof map

The thirteen entries are acceptance properties, not thirteen operations that
must be repeated inside one expensive devnet scenario. `OCM-E2E-001` is the
single tracer story; deterministic mutations, timing matrices and rare
failures are proved at their narrowest production seam and correlated by the
evidence ledger.

| Story step | Test IDs | Required retained oracle |
|---:|---|---|
| 1 | `OCM-E2E-001`, `OCM-E2E-004` | finalized public Tribute receipts, deterministic fixture bytes and CE/Mongo commitment parity for leagues/currencies/exclusion |
| 2 | `OCM-E2E-001`, `OCM-REQ-001` | request block/finality refs, split receipt, intent OCB1, voting/apply preconditions, expiry and event |
| 3 | `OCM-E2E-001` | exact request-height public proofs showing one early effect, zero new Nod/contributor/Tribute-consume effect and no duplicate on retry |
| 4 | `OCM-E2E-001` | supervisor-3 exit status while node-3 and committee finality advance |
| 5 | `OCM-E2E-001`, `OCM-EXP-001`, `OCM-DET-001` | three independent manifest roots, plan hashes and identical result digests; domain-local process/artifact identities |
| 6 | `OCM-E2E-001`, `OCM-VOT-001`, `OCM-PUB-001` | three separately signed full-result vote transactions, compact slots, identical derived digest and txpool/gossip/inclusion refs |
| 7 | `OCM-E2E-001`, `OCM-APL-002` | finalized q-forming vote receipt, the one stored canonical result, terminal job and all public owner/generation reads and proofs |
| 8 | `OCM-E2E-001`, `OCM-SEM-001` | independent reference output and field-by-field semantic comparison |
| 9 | `OCM-E2E-001`, `OCM-DET-001` | 1/2/4-worker schedules, seeds/retries and byte-identical artifacts |
| 10 | `OCM-E2E-001`, `OCM-SIG-001`, `OCM-PUB-002` | durable first sign record, typed refusal and failed public mutation receipts with unchanged live state |
| 11 | `OCM-E2E-001`, `OCM-TIM-001` | correlated delay=0/delay=N fresh fixtures and normalized semantic equality |
| 12 | `OCM-E2E-001`, `OCM-PUB-003` | two supervisors stopped, finality advancing, finalized expiry/release/requeue and zero Nod/fallback |
| 13 | `OCM-E2E-001`, `OCM-TRC-001` | exact-block traces from proposer/import/replay plus static boundary result; forbidden counters all zero |

For step 11, `OCM-TIM-001` uses the same production request and quorum-apply
entrypoints at controlled heights and compares canonical normalized results.
Mongo/opening/CAS mutation matrices remain in `OCM-EXP-001`, `OCM-CAS-001` and
`OCM-SIG-001`; the tracer scenario proves that those already-tested components
are wired together, not every internal fault permutation again.

### 5.3 No-on-chain-compute trace

Runtime evidence uses three stable, Lysis-specific calculation-boundary markers:

```text
legacy synchronous Lysis entry
Fidelity league calculation entry
Oracle calculation entry
```

The production functions expose those markers at every allowlisted caller.
`OCM-BND-002` checks compiler dependency closure and runtime call traces. During the request and
q-forming vote blocks, proposer, each importer and historical replay emit a
structured block-scoped boundary trace. `OCM-TRC-001` requires zero entries for
all three markers and proves the trace covered the exact transaction/system
phase IDs. Merely grepping logs for an absent string is not evidence.

## 6. OCOMP evidence bundle V1

### 6.1 Directory and publication

```text
evidence/ocomp-poc/v1/<source-sha>/<run-id>/
  run-manifest.json
  discovery.json
  build/
  topology/
  scenarios/
  chain/
  protocol/
  artifacts/
  traces/
  assertions.jsonl
  closure-report.json
  closure-report.md
  closure-report.sha256
```

Large synthetic input/result bytes may be stored once under `artifacts/`; other
records refer to exact relative path, byte length and SHA-256. Consensus
semantic/transport digests are recorded separately and recomputed with the
protocol rules.

The harness publishes each file through a temporary file plus fsync/rename and
publishes `run-manifest.json` last after hashing every member. A crash, timeout
or missing child result cannot publish PASS. The verifier writes the closure
report separately; neither harness nor scenario code can self-declare closure.

### 6.2 Required identities

The run manifest binds:

- source SHA, dirty bit, `Cargo.lock`, Rust toolchain and dependency metadata;
- exact executable hashes for node, `outbe-ocomp`, CLI and evidence
  verifier;
- exact hashes and retained bytes for every node/OCOMP/Mongo/network
  configuration and environment file, with secrets represented only by public
  identity/hash;
- chain ID, genesis/block-0 hash, fork height/ID, protocol bundle, correctness
  profile, capacity profile, object registry and generated limit manifests;
- static OCOMP committee indexes, public keys, PoPs and key epoch;
- command line, test discovery set, seed, machine/kernel/filesystem facts and
  Mongo image digest;
- per-domain PIDs, sockets, peer credentials, databases, CAS identity and
  process exit history;
- request/q-forming-vote/expiry block height/hash/state root/finality proof;
- transaction/calldata/receipt/log identities and public query/proof responses;
- input/plan/unit/result/vote/quorum/receipt/active-generation digests;
- every assertion, its requirement IDs, oracle, expected/actual artifact refs
  and status;
- all skip/todo/quarantine/retry/timeout/infrastructure records;
- retention/archive location and the hash of every evidence member.

Private keys, nondeterministic user data, production secrets and unrestricted
database/log dumps are forbidden. All inputs are deterministic synthetic
fixtures; bounded redacted logs are retained only where the assertion needs
them.

### 6.3 Assertion statuses

The closed set is:

```text
PASS | FAIL | INFRA_ERROR | TIMEOUT | SKIPPED | MISSING | DEFERRED
```

Only `PASS` proves a mandatory mapping. `DEFERRED` is legal only for
`PFS-002-07` and `PFS-002-08` with their exact frozen reasons. An infrastructure
rerun creates a new run ID and retains the failed run; it never edits or
supersedes an assertion inside the closure candidate.

### 6.4 Incremental task CI versus closure

The ledger intentionally names tests that do not exist at the start of
implementation. This must not make every early tracer PR permanently red or
allow a partial run to look like PoC success.

`mise run ocomp-poc-task -- OCM-NN` is task-progress mode. It requires the
card's task-local tests, every stable closing ID owned by that task and every
already-discovered OCOMP test to pass with no skip/todo/quarantine/retry.
Future planned IDs remain visibly `MISSING`; the report can claim only that
named task, never PoC closure.

`mise run ocomp-poc-closure -- --evidence-dir <dir>` is the only closure mode.
It runs all four lanes against one exact artifact set and fails on any required
missing/non-PASS ID. Retired and deferred planning rows never become runtime
assertions. CI job names, JSON status and Markdown reports carry the mode, so
task progress cannot be mistaken for full success.

For `gramine-direct`, the exact artifact set includes the canonical Docker
`sha256:` image ID resolved before the run. Enclave and signing-key containers
are launched by that immutable ID rather than the mutable local test tag;
scenario aggregation rejects a missing or different ID, and capacity hashing
binds the ID together with the exact Rust binaries.

### 6.5 Independent closure verifier

A small test-only `outbe-e2e-evidence` binary in the existing harness package:

1. validates the repository planning ledger and run schema;
2. requires exact source/binary/config/launch/image/profile identity and a
   clean source tree;
3. recomputes every file hash and protocol digest it can derive;
4. verifies finality proofs, vote signatures, compact quorum evidence, receipts,
   public proofs, conservation,
   reference output and normalized logical-time comparison;
5. requires exact test discovery and rejects zero-test/filtered/duplicate IDs;
6. resolves ADR/POC/PFS/story coverage from the repository ledger rather than
   trusting scenario-declared coverage;
7. rejects forbidden oracle/substitution combinations;
8. requires all mandatory statuses PASS and only the two declared deferred rows;
9. emits deterministic JSON and human-readable closure reports.

The verifier may use independent codec/reference modules but never invokes
on-chain Lysis or reads validator supervisor/CAS state as canonical outcome.

## 7. CI lanes and exact command surfaces

These `mise.toml` tasks are planned stable entrypoints. “Mandatory” in this
table means mandatory for PoC closure; task-progress mode invokes the relevant
tests with the narrower claim defined in section 6.4:

| Lane | Planned command | Platform/frequency | Timeout | Completion use |
|---|---|---|---:|---|
| `OCM-FAST` | `mise run ocomp-poc-fast` | Linux, every relevant PR | 15 min | mandatory |
| `OCM-INT` | `mise run ocomp-poc-integration` | Linux + Mongo, every relevant PR | 30 min | mandatory |
| `OCM-PUBLIC` | `mise run ocomp-poc-public-path` | fresh four-node process localnet, every relevant PR | 45 min | mandatory |
| `OCM-E2E` | `mise run ocomp-poc-e2e -- --evidence-dir <dir>` | privileged Linux with the production enclave binary under `gramine-direct`, every closure candidate and scheduled on main | 120 min | mandatory |
| `OCM-VERIFY` | `mise run ocomp-poc-evidence-verify -- <manifest>` | clean Linux, every evidence bundle | 15 min | mandatory final gate |
| `OCM-CLOSURE` | `mise run ocomp-poc-closure -- --evidence-dir <dir>` | closure runner, exact revision/artifacts | 240 min | aggregates all above |

The E2E task resolves to the existing harness entrypoint:

```text
cargo run --locked -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee gramine-direct --validators 4 --all \
  --enclave-bin target/release/outbe-tee-enclave \
  --input testing/e2e-harness/features/ocomp_poc.feature \
  --evidence-dir <dir>
```

`gramine-direct` exercises the existing encrypted public Tribute path with the
production `outbe-tee-enclave` binary but makes no SGX hardware/attestation
claim. All four nodes, consensus instances and OCOMP domains remain separate
real processes.

Rules:

- no test/scenario automatic retry;
- waits poll an observable condition with a recorded deadline; fixed long
  sleeps are forbidden;
- random schedules use a recorded seed and operation list;
- a manual infrastructure rerun retains both runs and does not turn the first
  failure green;
- mandatory tests cannot be `#[ignore]`, `@todo`, quarantined or filtered out;
- `--all` is necessary but not sufficient: discovery/closure verification
  detects missing scenarios even if the runner exits zero;
- PR failure blocks merge; scheduled failure alerts the named owner and blocks
  PoC closure until a later complete clean run;
- passing runs retain compact manifests/proofs/digests; failing runs additionally
  retain bounded logs and the exact reproduction command.

No performance/SLO claim is made. `OCM-CAP-001` and `OCM-PUBLIC` prove only that
the generated per-interface PoC bounds are enforced and executable with the
frozen PoC headroom on the declared test machine.

## 8. PFS and compatibility closure

Every PFS row has its stable historical ID. The full mapping is in the planning
ledger. In summary:

- `PFS-002-01` is the aggregate thirteen-step report;
- `PFS-002-02` covers the reachable empty compatibility branch;
- `PFS-002-03` and its former `OCM-E2E-003` test ID are `RETIRED` tombstones:
  the zero-limit READY premise is not produced by the production lifecycle;
- `OCM-E2E-005` is also a `RETIRED` tombstone; `PFS-002-10` is closed by the
  deterministic production-boundary `OCM-PIN-001` integration test;
- `PFS-002-04..06` cover source mutation, duplicate identity and certified
  owner rollback;
- `PFS-002-07` and `PFS-002-08` remain explicitly `DEFERRED`;
- `PFS-002-09..15` cover cursor, orphan, CAS, schedules, q=3/q<3 and sign-once;
- `PFS-002-16..21` cover full-result votes/quorum/result/receipt/time/deadline/caps;
- `PFS-002-22..24` cover version refusal, generation replay and forbidden-call
  trace.

Pre-fork, fork activation and post-fork blocks are all executed. The reachable
empty compatibility branch uses its pinned direct behavior and does not create
an OCOMP job. The populated active-fork branch has no synchronous fallback.

## 9. Implementation ownership for the later task graph

The task graph may split work but not move authority:

```text
outbe-plan/off-chain-poc-evidence-ledger.yaml
  stable requirement/test/lane mapping

crates/system/ocomp-protocol/tests/
  bytes, hashes, finality, vote/quorum and generated-cap vectors

crates/core/lysis/tests/
  independent semantics, planner/reducer and receipt equations

crates/core/metadosis/tests/
  FSM model and request/expiry invariants

bin/outbe-ocomp/tests/
  real public-RPC/export/CAS/ZeroMQ-worker/reducer process seams

crates/blockchain/node/src/ocomp/tests/
  pin, cursor, attestation and sign-once persistence

crates/blockchain/evm/tests/
  system phase, public full-result vote, quorum apply, checkpoint and replay parity

testing/e2e-harness/
  existing Tribute projection/RPC/Mongo/CE fixture path
  + one registered OCOMP World handle and step module
  + OCOMP Gherkin features, evidence writer and verifier

mise.toml + .github/workflows/
  stable command surfaces and fail-closed lanes
```

No new general test framework, OCOMP mock chain, central calculator or
production deployment controller is introduced.

## 10. Explicitly outside PoC evidence

The following remain BoundedMVP/TargetLarge:

- `PFS-002-07` CE persistence crash matrix and `PFS-002-08` backlog policy;
- exhaustive kill-before/after every Reth/CE/Mongo persistence boundary;
- production SLO, RPO/RTO, supported-network minimum-machine/headroom and
  release evidence (the fixed disposable-PoC machine/headroom gate remains
  required);
- mixed-version rolling upgrades, key compromise/revocation operations,
  long-horizon GC and destructive restore;
- billion-record throughput, proof-carrying execution and external DA;
- second-program/Gem qualification and a generic registry/adapter.

Normal node/compute restart and finalized generation replay remain PoC
requirements; they are not the deferred CE crash-consistency matrix.

## 11. Decision result

Ticket #9 is resolved without grilling. The normative matrices and current
harness determine the answer:

- keep the existing Rust/Cucumber harness and add only OCOMP-specific handles;
- use layered tests, but close them through one machine-readable ledger;
- require a real four-domain public path and real independent process
  boundaries without claiming host isolation;
- retain exact artifacts and let an independent verifier compute closure;
- treat skips, todo, retries and trusted local storage as non-evidence;
- retire exactly PFS-002-03 and its invalid E2E test identity;
- defer exactly PFS-002-07/-08 and no other PoC requirement.
