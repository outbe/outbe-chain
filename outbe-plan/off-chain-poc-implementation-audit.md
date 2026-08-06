# Off-chain Computation PoC implementation-plan audit

Status: **STALE AFTER 2026-07-26 PROTOCOL CORRECTION — REAUDIT REQUIRED**

The audit predates removal of the relay/`ExecutionCertificateV1`. Its closure
claim must not be used until the revised vote/quorum/accountability plan and
evidence ledger are reaudited.

Date: 2026-07-23

Audited artifact:
[`off-chain-poc-implementation-plan.md`](off-chain-poc-implementation-plan.md)

This audit proves that the plan is complete enough to start implementation. It
does not claim that OCOMP production code, tests, a devnet run or PoC closure
evidence already exists.

## 1. Verdict

The plan is **READY_FOR_IMPLEMENTATION** for the fresh-devnet Lysis V1 PoC over
bounded work units and constant-size commitments, with no total Tribute cap.

The 2026-07-24 amendment review confirmed that this remains true after removing
the mistaken complete-job ceiling: the task graph now treats shard capacity as
a per-unit bound, requires population-independent lazy planning and keeps
activation constant-size.

- The complete public Tribute -> Metadosis -> JobIntent -> finalized export ->
  independent Lysis -> q=3/4 -> public activation -> Nod path has task owners.
- All normative ADR/POC/PFS/story requirements reach planned tests, allowed
  oracles, retained evidence, CI lanes and one closing task owner.
- The 28 task cards and their 72 declared dependencies form one acyclic graph.
- Fork-critical bytes are blocked by `G1`; measured capacity and the canonical
  fresh-devnet identity are blocked by `G7`.
- Exactly two new workspace packages are planned. No generic program framework,
  second program, ZeroFee change, TargetLarge path or supported-network rollout
  enters a task Definition of Done.
- `PFS-002-07` and `PFS-002-08` remain the only deferred PFS rows; the closure
  DoD names them only as exclusions, not implementation work.

No product decision remains open and no grilling is required before `OCM-00`.

## 2. Reverse requirement coverage

| Source obligation | Current evidence | Result |
|---|---|---|
| current production seams and gaps | [current-code map](off-chain-poc-current-code-map.md), rechecked against current CodeGraph source | covered |
| four OCOMP ADRs | 34 source-derived invariant IDs in the [ledger](off-chain-poc-evidence-ledger.yaml) | covered |
| `POC-01..POC-26` | all 26 exact IDs map to non-empty planned test sets | covered |
| `PFS-002-01..25` | 22 required rows map to tests; 03 is the exact `RETIRED` tombstone and only 07/08 are `DEFERRED` | covered |
| distributed thirteen-property proof map | steps `1..13` each map to tests and an allowed oracle; `OCM-E2E-001` is the one correlated tracer story while matrices remain at focused seams | covered |
| existing Tribute E2E integration | `OCM-24/27` reuse the current public transaction, four-validator Mongo projection and independently verified CE path before observing `JobIntent`; `OCM-E2E-001` retains the tracer correlation, while stable `OCM-E2E-004` is integration-owned duplicate/export containment | covered |
| section 17 PoC deliverables | protocol/runtime/semantic/evidence owners listed below | covered |
| section 22 planning decisions | all 15 resolved before their dependent work, including the split final-cap gate | covered |
| PoC -> BoundedMVP evolution | section 9 of the canonical plan preserves the core and names replaceable operational shells | covered |

### 2.1 Source section 22 closure

| # | Required decision | Frozen asset | First implementation owner |
|---:|---|---|---|
| 1 | fork/version/bundle/genesis | [protocol freeze](off-chain-poc-protocol-freeze.md), two-stage final identity | `OCM-04`, final `OCM-26` |
| 2 | generated per-shard/per-chunk/evidence/activation/block caps; no total Tribute cap | protocol freeze, public measurement gate | `OCM-25`, `OCM-26` |
| 3 | admission/split/precondition/result-chunk/result/generation/receipt fields | protocol freeze and [activation decision](off-chain-poc-activation-and-atomic-apply.md) | `OCM-03..05`, `OCM-17..23` |
| 4 | codec/hash/signature/golden format | protocol freeze, OCB1 registry | `OCM-02..04` |
| 5 | legacy Lysis semantics/corpus | [semantic baseline](off-chain-poc-lysis-v1-semantics.md) | `OCM-01` |
| 6 | finalized-intent proof/history source | [finalized export decision](off-chain-poc-finalized-input-export.md) | `OCM-09` |
| 7 | Reth/CE checkpoint API | finalized export decision | `OCM-10` |
| 8 | CAS layout/quota/cleanup | [process/CAS decision](off-chain-poc-process-and-artifact-topology.md) | `OCM-11`, `OCM-13` |
| 9 | harness-owned process topology, public RPC, ZeroMQ/TCP and bounded CAS | process/CAS decision | `OCM-11`, proved by `OCM-24/27` |
| 10 | key format/sign-once durability | [deterministic/quorum decision](off-chain-poc-deterministic-execution-and-quorum.md) | `OCM-15` |
| 11 | full-result vote/public transaction bytes and q-forming apply | deterministic/quorum and activation decisions | `OCM-16`, `OCM-23` |
| 12 | logical retirement/GC boundary | activation decision | `OCM-20`, `OCM-23` |
| 13 | independent reference technology | semantic baseline: isolated test-only Rust crate with arbitrary-precision arithmetic | `OCM-01` |
| 14 | no-on-chain-calculation trace | [test/evidence decision](off-chain-poc-test-and-evidence.md) | `OCM-17`, `OCM-24`, `OCM-27` |
| 15 | minimum machine/headroom rule | protocol freeze: exact four-CPU/16-GiB class, five cold runs, 20% minimum headroom | `OCM-04`, measured by `OCM-25/26` |

Final capacity is intentionally not guessed at `G1`. `G1` freezes codecs,
semantics, candidate ceilings, the exact machine class and measurement rule.
`G7` uses the completed real public path to select the lower/equal final cap
and is the sole owner allowed to arm the canonical fresh-devnet fork.

### 2.2 PoC deliverable ownership

| Deliverable class | Closing tasks |
|---|---|
| canonical bundle/hash, codecs/vectors, FSM/finality/activation state, committee, public schemas and immutable manifests | `OCM-02..09`, `OCM-23`, `OCM-26` |
| node control/attestation, supervisor, exporter, worker, CAS, relay and four-domain deployment | `OCM-11..16`, `OCM-24`, `OCM-27` |
| pure Lysis/reference, typed result, certified apply, four activation receipts, request budget-split receipt and logical Tribute retirement | `OCM-01`, `OCM-14`, `OCM-17..23` |
| POC/PFS/story tests, cap gate, forbidden-call trace and reproducible report | `OCM-00`, `OCM-25..27` |

## 3. Authority and dependency audit

Authority remains singular:

| Authority | Owner |
|---|---|
| canonical OCB1 bytes/hashes/pure proof verifiers | `outbe-ocomp-protocol` |
| JobIntent/FSM/expiry/terminal/active generation | Metadosis OCOMP module |
| finalized proof/pin/control/attestation/sign-once | node |
| Lysis input completeness/plan/result/apply meaning | Lysis V1 modules |
| Nod/Intex/Tribute/Promis activation mutations and Desis request dispatch | each existing state-owner crate |
| local scheduling/export/artifacts/relay | fixed modes of `outbe-ocomp` |
| public transaction/checkpoint/import/replay | existing EVM/Reth path |
| orchestration and evidence capture | existing E2E harness |

Repeated file ownership is ordered, not competing: protocol foundation
`02 -> 03 -> 04`; Metadosis request/apply `08 -> 23`; budget/effect
`05 -> 18..22`; harness evidence/topology/public/closure
`00 -> 24 -> 25 -> 27`.

The runtime capability no longer lives in the shared wire crate. Its opaque
token and one-shot closure live in the existing `outbe-primitives` storage
capability seam; `CtxStorageProvider` grants the lease only for the exact
Metadosis activation frame. This avoids a public constructor, an owner ->
Lysis/Metadosis dependency cycle and a third package.

## 4. Minimality and scope audit

The only new packages are:

1. `crates/system/ocomp-protocol`;
2. `bin/outbe-ocomp`, with four fixed modes.

The plan reuses current finality, CE, Mongo, typed storage, state owners,
precompile dispatch, keygen, public transaction path and E2E process harness.
It adds no CAS daemon, launch broker, custom activation RPC, transaction type,
generic state-write executor, `ProgramRegistry`, `TaskAdapter`, second
program, proof/DA system or production deployment controller.

Normal restart, sign-once restart and finalized-generation replay remain in
PoC. Exhaustive CE crash recovery, backlog policy, supported-network rollout,
production operations and billion-record mechanisms remain outside it.

## 5. Findings corrected by this audit

| Finding | Why it mattered | Resolution |
|---|---|---|
| capability placed in shared protocol crate | private construction across existing crates was not implementable without a public factory or dependency cycle | moved to the existing primitives storage-frame seam; protocol crate carries bytes only |
| early full-ledger CI was ambiguous | sequential PRs could be permanently red or partial work could look fully green | explicit `task_progress` versus fail-closed `poc_closure` modes |
| evidence identity omitted exact config/launch hashes | identical binaries with different process/network configuration were not the same run | manifest and verifier now require exact config/launch/image/environment hashes |
| minimum machine/headroom was not frozen | capacity task could choose its benchmark target after seeing results | exact machine class, five cold runs and 20% headroom frozen at `G1` |
| private OCOMP key file format was only called an “envelope” | implementers could invent incompatible storage and publication rules | exact lowercase-hex file, path, permissions, no-clobber/fsync and load checks frozen |
| six direct dependencies existed in cards but not Mermaid | graph visualization understated review/merge prerequisites | added `04 -> 09` and `05 -> 18..22`; graph now has 72 matching edges |
| PoC/production headroom wording overlapped | required PoC capacity evidence could be mistaken for deferred production work | only supported-network/production headroom remains deferred |
| existing Tribute harness reuse was implicit | implementers could add a second sender/reader or inject a ready root/job and still appear to satisfy the OCOMP Gherkin story | `OCM-24/27` now name the exact harness wiring and require receipt/finality, four-validator Mongo parity, independent CE verification and end-to-end fixture correlation before `JobIntent` |

## 6. Mechanical audit results

The current files passed:

- duplicate-key YAML parsing;
- exact source-derived cardinality:
  `ADR={8,8,8,10}`, `POC=26`, `PFS=24`, story `=13`;
- 37 unique stable tests, all referenced by requirements and each owned by
  exactly one closing task;
- all 28 tasks present in machine ownership, including tasks with only local
  tests at their own merge point;
- required test fields, registered lanes, allowed oracles and declared
  substitutions/discharge;
- 28 detailed cards with every required field;
- exact equality between card dependencies and the 72 Mermaid edges;
- acyclic topological traversal from `OCM-00` to `OCM-27`;
- local Markdown link existence, balanced fences, no trailing whitespace and
  clean `git diff --check`.

The task-progress verifier may prove only a named task. Only the exact-artifact
closure run may prove the PoC. Until `OCM-27` executes, every ledger test
remains `planned`, not implemented evidence.
