# Citadel audit: `crates/core/metadosis`

Audit date: 2026-07-30.
Inspected revision: working tree on `main` before this report; `git status --short --branch` was clean and reported `main...origin/main`.

## Verdict

- Status: **NOT CITADEL**
- Confidence: **high**
- Scope/trust/atomicity assumptions:
  - Scope includes the complete `outbe-metadosis` crate, its Solidity ABI, direct production callers, storage collection behavior, the Cycle and EVM transaction adapters, transitive Desis/Promis/Tribute/Nod/Intex effects, and the Metadosis/OCOMP test layers.
  - The canonical executor, consensus-certified parent metadata, fork manifest loader, and block-scoped `StorageHandle` are trusted inputs. Rust callers that can obtain a `StorageHandle` are still in scope for interface-closure grading.
  - Both release and debug compilations are in scope because the repository contains no release-only node-admission rule. The finding involving `debug_assertions` is inapplicable to a release binary, but is consensus-critical if any debug node participates.
  - The semantic rollback domains are the Cycle per-trigger storage checkpoint, the ordinary EVM transaction journal, and the OCOMP vote path's additional compressed-entity work checkpoint. Finalized CE MDBX and Mongo projection internals are imported contracts, not re-audited here.
- Rationale: OCOMP is substantially deeper than the legacy WWD path: it has a bounded persisted FSM, ordered indexes, immutable request/effect receipts, exact replay behavior, certified finality, a private activation capability, and an outer storage plus CE rollback frame. The module as a whole still fails Citadel, however, because:
  1. a debug-build process environment variable can change q-forming consensus execution;
  2. the crate publicly exports raw schema fields and deep mutators that can bypass its certified adapters;
  3. the WWD status updater can jump over effect-owning phase edges;
  4. the legacy Lysis error path writes `FAILED` and an event, then returns `Err`, so production reverts what direct tests assert;
  5. READY selection borrows swap-remove set order and scans an unenforced-size active set;
  6. legacy Desis and emission seams return `bool`/zero sentinels instead of exact typed receipts;
  7. invariant-bearing arithmetic and persisted discriminants remain unchecked/raw; and
  8. required production-interface and independent-model evidence remains incomplete.

## Sources of truth and scope

Evidence priority used for this audit:

1. The Citadel grading contract in `.agents/skills/citadel-module-audit/CITADEL.md`.
2. Accepted repository contracts, especially:
   - root `README.md:114-120,167-186,266-280`;
   - `docs/adr/system/ADR-S-OCM-004-certified-activation-job-fsm-and-protocol-versioning.md`;
   - `contracts/precompiles/src/IMetadosis.sol`.
3. Current production source and its actual callers.
4. Proposed owner ADRs as target contracts and explicit debt records:
   - `docs/adr/core/ADR-C-MET-001-metadosis-worldwide-day-fsm.md`;
   - `docs/adr/core/ADR-C-LYS-001-lysis-tribute-to-nod-transformation.md`;
   - `docs/adr/system/ADR-S-CYC-001-deterministic-cycle-scheduling.md`;
   - `docs/adr/blockchain/ADR-B-TST-001-production-verification-and-evidence-architecture.md`.
5. Current tests, classified by the real interface and substituted components they exercise.

The old top-level `adr/011-partition-retirement.md` declares itself superseded and historical at lines 1-5. Its current counterparts under `docs/adr/` were used instead.

The complete module inventory is:

- `src/{lib,constants,errors,schema,state,runtime,emission_sink,pre_admission,precompile,ocomp_budget}.rs`
- `src/ocomp/{mod,state,schema,request,expiry,vote,activation,fork,views,test_support}.rs`
- 77 statically discovered Metadosis package tests across `src/tests`, internal OCOMP tests, and `tests/ocomp_fsm_model.rs`
- seven adjacent EVM OCOMP tests across `ocomp_atomic_apply`, `ocomp_logical_time`, `ocomp_request_lifecycle`, and `ocomp_result_votes`

Prior audit history was used only to identify likely drift hotspots. All verdicts and citations below were re-established against the current working tree.

## Mutation interface and call graph

The intended production graph is:

```text
block executor
├─ CycleLifecycle::begin_block
│  ├─ block 1 -> Metadosis::init_genesis_day
│  ├─ 00:00 emission_limit_1
│  │  └─ emission/reward allocation
│  │     ├─ terminal sink -> Metadosis::emission_sink::apply
│  │     └─ Metadosis::start_metadosis
│  └─ 12:00 wwd_advance_noon
│     └─ Metadosis::advance_active_worldwide_days
├─ CertifiedParentAccounting
│  └─ finalization proof -> record_certified_parent_finality
├─ OcompLifecycleBegin
│  ├─ exact-height fork install
│  └─ bounded open/expiry/accountability close
├─ post-CE-seal OcompTerminalRequest
│  └─ bounded request/retry transition
└─ ordinary EVM transaction to IMetadosis.submitLysisResult
   └─ result-vote verification -> q=3 typed activation
```

The Cycle adapter is real and explicit. `CycleLifecycle::begin_block` calls the trigger dispatcher (`crates/system/cycle/src/lifecycle.rs:44-53`), and each due handler plus cursor/event runs under one checkpoint (`crates/system/cycle/src/runtime.rs:15-18,83-123`). The midnight handler invokes terminal emission then `start_metadosis` before marking the UTC day settled (`crates/system/cycle/src/handler.rs:166-190`).

OCOMP production authority is also materially strong:

- only an actual finalization proof reaches the certified-finality wrapper (`crates/blockchain/evm/src/begin_block_precompile.rs:471-483`);
- fork install and bounded lifecycle execute in the reserved begin-zone slot (`crates/blockchain/evm/src/begin_block_precompile.rs:1012-1028`);
- request creation executes in the reserved post-CE-seal slot (`crates/blockchain/evm/src/begin_block_precompile.rs:1031-1037`);
- the public result selector is intercepted with the current execution scope and a same-address reentrancy guard (`crates/blockchain/evm/src/precompiles.rs:219-245,274-303`);
- standard Metadosis dispatch is otherwise view-only (`crates/core/metadosis/src/precompile.rs:13-84`).

The effective Rust interface is much wider than that graph:

- `lib.rs` publicly exports `ocomp`, `runtime`, `schema`, and `state` (`crates/core/metadosis/src/lib.rs:1-12`);
- `MetadosisContract` exposes every record, set, deque, mapping, byte vector, and OCOMP index as a public field (`crates/core/metadosis/src/schema.rs:107-242`);
- the shared `StorageBacked::new` constructor is public (`crates/blockchain/primitives/src/storage/mod.rs:165-177`);
- deep state methods such as create/delete, raw day-type write, raw time-derived status update, limit overwrite, broad failure, and active-index add/remove are public (`crates/core/metadosis/src/state.rs:22-66,92-149,192-205,231-239`);
- `record_ocomp_finality` accepts caller-supplied block hashes, roots, height, and response window (`crates/core/metadosis/src/ocomp/schema.rs:428-485`);
- request-profile and fork-install initializers are public (`crates/core/metadosis/src/ocomp/schema.rs:131-158`; `crates/core/metadosis/src/ocomp/fork.rs:203-247`).

Current production callers use the safer wrappers, but the crate boundary does not make those wrappers authoritative.

## Side-effect ledger

| Effect | Owner | Atomicity domain | Commit/rollback/compensation point | Receipt | Retry/idempotency |
|---|---|---|---|---|---|
| WWD record creation, active membership, initial Tribute seal, `WorldwideDayStarted` | Metadosis + Tribute | Transactionally coupled | Cycle trigger checkpoint in production; no local checkpoint in `create_worldwide_day_for_date` | `Result<()>` plus event | Wrapper is identity-idempotent; raw create is public |
| WWD phase status, Oracle snapshots, day type/VWAP, Tribute seal/unseal, status event | Metadosis + Oracle + Tribute | Transactionally coupled | Cycle trigger checkpoint | No typed transition receipt | Same phase is a no-op, but multi-edge catch-up is incomplete |
| Daily limit plus carried Promis, formation marker, two events | Metadosis + PromisLimit | Journaled-local | `apply_ocomp_day_limit` local checkpoint; Cycle checkpoint widens it | Returns `U256::ZERO` sentinel | Exact same base replays as no-op; different base rejects |
| Legacy daily limit write and accumulation event | Metadosis | Transactionally coupled only by caller | Cycle handler's day-settled guard/checkpoint | Returns `U256::ZERO` sentinel | Raw `apply` replay can overwrite and re-emit |
| Legacy Lysis Nod/CE, contributors, Tribute consumption | Lysis/Nod/Intex/Tribute | Journaled-local plus block CE scope | Lysis storage checkpoint, widened by Cycle checkpoint | `LysisResult` | Success is terminal; corruption error retries the same Cycle slot after rollback |
| Legacy Desis brief | Desis | Compensated fallback inside local journal | Desis inner checkpoint; on error it reverts Desis writes, emits failure outside that inner checkpoint, and returns `false` | `bool` | Metadosis converts rejection to full Promis carry-over and may still complete |
| Legacy terminal status, active/closed movement, cleanup event | Metadosis | Transactionally coupled | Cycle checkpoint | No typed terminal receipt | `FAILED` is idempotent; `COMPLETED` requires READY |
| OCOMP strict budget split, Desis brief or carry-over, immutable receipt | Metadosis/Desis/PromisLimit | Journaled-local | Terminal request's local checkpoint | `RequestBudgetSplitReceiptV1` | Exact retained effect validates without repeating; different effect rejects |
| OCOMP intent, ready/live/deadline indexes, job/FSM records, request/expiry/conflict events | Metadosis OCOMP | Journaled-local | Local request/finality/lifecycle checkpoints plus enclosing system transaction | Canonical typed records and FSM projections | Exact request/finality replay is idempotent; retained retry has a new nonce |
| OCOMP vote slots, quorum, certified Nod/contributor/Tribute/carry-over owners, Metadosis completion, terminal receipt | OCOMP plus domain owners | Journaled-local plus CE work | One outer storage checkpoint and explicit `ExecutionScope` CE checkpoint/restore (`ocomp/vote.rs:133-245`) | Owner receipts plus aggregate terminal receipt | Exact terminal retry has no effects; different result/equivocation follows bounded rules |
| EVM/product events | Owning module | Journaled-local | Same EVM/storage checkpoint as state | Event itself, not a command receipt | Reverted events are not durable |
| Gas/work and tracing | Executor/local diagnostics | Diagnostic/metering | Ordinary EVM gas rules; process logs are outside consensus state | Meter/log | The `result_vote_committed` trace is emitted before the outer transaction is actually committed (`precompiles.rs:341-345`) |

The important positive result is that no irreversible network or filesystem effect occurs inside the audited state transition. The important negative result is that several public seams rely on a caller's checkpoint rather than requiring a capability that proves the checkpoint exists.

## Observed FSM

### WorldwideDay and legacy settlement

| Current | Event | Guard | Effects | Next/error | Rollback owner |
|---|---|---|---|---|---|
| absent | Create(day) | `forming_start == 0` in wrapper | record, active membership, Tribute seal, start event | FORMING | Cycle trigger |
| FORMING | Tick(time) | time before forming end | none | FORMING | Cycle trigger |
| FORMING | Tick(time) | time at/after forming end | status is first derived directly from time; snapshot VWAP and resolve type | LOOKBACK_DELAY, OFFERING, WAITING, or READY | Cycle trigger |
| LOOKBACK_DELAY | Tick(time) | derived target differs | status write; unseal only when target is exactly OFFERING | OFFERING, WAITING, or READY | Cycle trigger |
| OFFERING | Tick(time) | derived target differs | seal Tribute partition; status event | WAITING or READY | Cycle trigger |
| WAITING | Tick(time) | time reaches scheduled process | status/event | READY | Cycle trigger |
| READY | Settle | zero limit | mark failed, retire active member, skipped event | FAILED | Cycle trigger |
| READY | Settle | UNKNOWN type | mark failed, failure event, credit full limit | FAILED | Cycle trigger |
| READY | Settle | no Tribute or zero allocation | optional Desis brief/fallback, Promis credit, complete, retire partition, event | COMPLETED | Cycle trigger |
| READY | Settle | populated, pre-OCOMP profile | synchronous Lysis, Desis/Promis, complete, retire | COMPLETED | Cycle trigger |
| READY | Settle | synchronous Lysis error | write FAILED/event, then return original error | `Err`; production restores pre-command READY, direct test retains FAILED | Cycle trigger in production; absent in direct test |
| READY | Prepare OCOMP | active profile and eligible population | pre-admission init, Fidelity league snapshot, ordered READY enqueue | READY plus OCOMP Ready substate | Cycle trigger |
| any non-COMPLETED | Raw `mark_wwd_failed` | only COMPLETED rejects | FAILED, active removal, closed enqueue | FAILED | Whatever caller provides |

The reducer is not explicit: `update_wwd_status` writes the timestamp-derived target first (`state.rs:109-140`), while the runtime later infers edge effects from `(old, new)` (`runtime.rs:381-423`). A jump from before OFFERING to after OFFERING never unseals the partition and can still reach READY.

### OCOMP attempt FSM

| Current | Event | Guard | Effects | Next/error | Rollback owner |
|---|---|---|---|---|---|
| Ready | Defer | due, next height strictly later | replace one ordered due key | Ready | Terminal request checkpoint |
| Ready | Request | due, admission eligible, capacity available | exact split receipt, intent/job, live index, status OFFCHAIN_PENDING | AwaitingFinality | Terminal request checkpoint |
| AwaitingFinality | CertifiedFinality | matching finalized request block | immutable JobId/open/deadline binding | AwaitingFinality(finalized) | Finality checkpoint |
| AwaitingFinality(finalized) | Begin at exact open height | exact height; skipped height is fatal | response index, vote slots, VotingOpen | VotingOpen | Lifecycle checkpoint |
| VotingOpen | First valid vote | eligible signer, `open <= h < deadline` | one fixed vote slot | VotingOpen | Vote storage + CE checkpoint |
| VotingOpen | Third matching vote | verified result and live targets | quorum plus all certified owner effects and terminal receipt | COMPLETED | Vote storage + CE checkpoint |
| VotingOpen | Third vote with expected stale target | typed conflict outcome | no owner effects; retained retry transition | READY(next attempt) / CONFLICTED job | Vote storage + CE checkpoint |
| VotingOpen | Deadline without quorum | exact due close | terminal expiry record, retained Lysis effect, requeue one block later | READY(next attempt), or FAILED at cap | Lifecycle checkpoint |
| Completed/Conflicted | Exact retry/fourth vote | immutable terminal identity; window still open for accountability | no terminal/economic replay; bounded accountability only | same terminal result | Vote storage + CE checkpoint |
| any | Invalid/corrupt input | decode, binding, capacity, signature, index, receipt, or invariant failure | none | reject/fatal | Local/outer checkpoint |

`JobFsmState` is a validated in-memory aggregate and the stored OCOMP maps check several record/index equivalences. That is the best Citadel-shaped part of this module.

## Citadel gates

| Gate | Status | Evidence | Gap/closure |
|---|---|---|---|
| G1 — Deep, closed interface | **FAIL** | Production adapters are narrow, but `schema`, raw fields, state helpers, finality, profile, and fork installers are public and constructible from any `StorageHandle`. | Make raw schema and deep mutators `pub(crate)` or private. Expose one command interface whose authority variants cannot be constructed outside the canonical adapters; keep a query-only public projection. |
| G2 — Valid state model | **FAIL** | Status/day type persist as raw `u8`; public methods accept arbitrary type/status inputs; raw fields can create record/index drift; `delete_worldwide_day` does not remove active/closed membership. | Typed decoding at every load, local aggregate validation, and atomic record/index commits. Unknown tags and impossible cross-field combinations must be fatal. |
| G3 — Explicit FSM | **FAIL** | WWD status is derived and written from time before edge effects; multi-edge jumps skip OFFERING effects; `mark_wwd_failed` accepts every non-COMPLETED state; legacy corruption retry has no reachable recovery. OCOMP's attempt FSM is explicit but does not repair the outer WWD FSM. | Add a pure WWD reducer with a complete event/state table and named catch-up/forfeiture policy. Preserve broad emergency failure only as a private command outcome. |
| G4 — Atomicity domains and error guarantees | **FAIL** | Cycle and OCOMP checkpoints are real. The immediate-fail pattern remains at `runtime.rs:605-615`: write FAILED/event then return `Err`; the direct test observes writes production reverts (`tests/lifecycle.rs:1337-1356`). Some public commands depend on an undocumented caller checkpoint. | Every exported mutation owns a checkpoint or requires an unforgeable transaction capability. A committed failure returns a typed successful domain outcome; `Err` leaves semantic pre-state. Test the same transaction seam. |
| G5 — Explicit effects and receipts | **FAIL** | OCOMP uses typed receipts. Legacy Desis returns `bool` after swallowing an inner error (`desis/src/api.rs:18-63`); Metadosis emission returns a zero sentinel (`emission_sink.rs:13-38,60-104`). | Replace legacy `bool`/sentinel APIs with exhaustive receipts consumed by the reducer; propagate any outcome not explicitly selected as a business fallback. |
| G6 — Deterministic, bounded execution | **FAIL** | A debug environment variable mutates q-forming receipts; READY selection uses the first value in an O(N) swap-remove set; no active-set cap is enforced; allocation/window arithmetic is unchecked. | Remove all process-local consensus inputs, use a bounded ordered due index/cursor, define backlog policy, and use checked domain arithmetic with cap boundary tests. |
| G7 — Single-source invariants | **FAIL** | Public record and index mutations can be committed independently; `active_wwd_count` duplicates set state but is unused; active/closed/record equivalences are not checked at the WWD seam. OCOMP checks more of its own equivalences. | One aggregate commit owns record plus active/closed/due indexes. Retain slot 2 as reserved storage, but remove it as a semantic counter or verify it everywhere. |
| G8 — Replay, retry, reentrancy, concurrency | **FAIL** | Same-address EVM reentrancy is denied and OCOMP exact/different replay is strong. Legacy emission `apply` can overwrite/re-emit on direct replay, and synchronous corruption makes Cycle retry a permanently failing slot without a named recovery transition. | Give every command a canonical intent/replay key and typed recorded result. Decide whether legacy corruption is a fatal chain stop, committed FAILED outcome, or governance recovery; do not silently retry forever. Serialized EVM execution makes concurrent mutation otherwise N/A. |
| G9 — Production-interface evidence | **FAIL** | A 512-operation independent in-memory OCOMP FSM comparison exists, and request builder/replay uses the real executor. It does not drive the persisted aggregate through the production adapter or report distribution. Owner-failure tests mostly call a fixture directly. Legacy tests lack T-1/T/T+1, backward/multi-edge, ordering/starvation, corruption, and production rollback parity. | Add an independent persisted-state model through the production command seam, full fault matrix, cap/order/starvation properties, and proposer/import/replay parity for every critical failure class. |
| G10 — Migration and project contract | **FAIL** | Append-only OCOMP slots and exact-height manifest install are positive. The process-global debug failpoint contradicts root `README.md:184-186` and Accepted ADR-S-OCM-004:156-161. Proposed Metadosis ADR debt and its evidence statements are stale relative to current OCOMP code. | Remove the contract violation; choose activation/backfill for new indexes; preserve reserved slots; update README/ABI/ADRs/evidence ledger on the same revision. |

## Project-contract axis

| Rule/source | Status | Evidence | Required action |
|---|---|---|---|
| Critical begin-zone failure fails the block, not silently skips (`README.md:114-120`) | **PASS** | Cycle returns a failed handler and reverts its trigger checkpoint (`cycle/src/runtime.rs:83-123`). | Retain this, but distinguish fatal invariant failure from committed business FAILED. |
| Block-boundary work uses `BlockLifecycle`, explicit order, scoped storage (`README.md:167-186`) | **PASS** | `CycleLifecycle` is the executor-facing marker; Metadosis is an imported domain command. Storage is explicit. | Keep Metadosis behind Cycle/OCOMP adapters rather than adding another lifecycle. |
| Persistent behavior cannot depend on process globals (`README.md:184-186`) | **FAIL** | Debug q-forming apply reads `OUTBE_E2E_OCOMP_OWNER_FAILPOINT`. | Delete the branch from product code; test injection must be compile-time test-only and adapter-owned. |
| WWD phases advance at midnight/noon (`README.md:275-280`) | **PARTIAL** | Registry contains both ticks and the Cycle checkpoint. WWD transition code does not explicitly replay/reject crossed edges. | Freeze a catch-up/forfeiture rule and test canonical time gaps. |
| Accepted OCOMP q-forming typed atomic apply (ADR-S-OCM-004:234-309,378-406) | **PARTIAL** | Private in-frame capability, owner receipts, bounded votes, storage/CE rollback, exact retry, and ordered request/deadline indexes are implemented. Debug environment injection violates deterministic apply, and raw public initializers/finality remain bypass seams. | Remove local input and close raw Rust mutators; refresh final evidence. |
| Proposed Metadosis owner contract (ADR-C-MET-001:122-178,212-245) | **FAIL** | The ADR itself lists current READY order, multi-edge, raw tag, arithmetic, best-effort, rollback diagnostic, and model gaps; current source still exhibits them. | Resolve and implement the listed decisions rather than carrying them as permanent architecture. |
| Proposed deterministic Cycle contract (ADR-S-CYC-001:46-73,102-129) | **PARTIAL** | Stable registry/cursor/checkpoint are present; catch-up replays one old slot against current block time and remains explicitly unaccepted policy. | Pass a typed scheduled slot or define how stale slots map to domain logical time. |
| Public ABI is views plus one bounded result-vote selector (`IMetadosis.sol:96-137`) | **PASS** | Normal dispatch is view-only; active selector is intercepted with execution scope and reentrancy guard. | Add invalid status-input rejection and retain route/ABI conformance tests. |
| Stateful module evidence minimum (ADR-B-TST-001:71-105) | **FAIL** | Strong component and one real builder/replay test exist, but critical mutation/failure classes do not all cross production interfaces. | Build the requirement ledger and execute all mandatory layers on supported CI platforms. |
| OCOMP storage evolution is append-only and fork-bound | **PASS** | Schema comments and fixed slot tests pin append-only fields; manifest loading validates chain/genesis before executor installation. | Any new WWD due index must also append and activate with an explicit migration/backfill. |

## Findings

### CITADEL-001 — Debug process environment changes q-forming consensus execution

- Severity: **Critical**
- Gate: **G6, G10**
- Evidence: `apply_certified_result` performs all four owner mutations, then calls `inject_test_receipt_fault` before receipt verification (`crates/core/metadosis/src/ocomp/activation.rs:378-399`). In non-`test-utils` debug builds, that helper reads `OUTBE_E2E_OCOMP_OWNER_FAILPOINT` and mutates either the Nod receipt root or request logical anchor (`activation.rs:424-446`).
- Reachable failure mode: for the same pre-state and q-forming `submitLysisResult` transaction, a debug validator without the variable verifies receipts and commits; a debug validator with it rejects and restores the transaction. They compute different receipt/state outcomes and cannot agree on the block. Release builds compile the branch out, but no structural rule prevents a debug node from joining.
- Structural closure: delete the `debug_assertions` environment branch. Keep fault injection only behind `cfg(test)`/the explicit `test-utils` fixture adapter, with no process-global read in any library object linked into a node.
- Closure test: build debug and release node artifacts without `test-utils`, execute the same maximum-shape q-forming block under varied process environments, and assert byte-identical receipts, state root, CE root, and import result. Add a source-independent link/behavior test proving the environment name has no effect.

### CITADEL-002 — Public raw storage and deep mutators bypass the authoritative adapters

- Severity: **High**
- Gate: **G1, G2, G7, G8**
- Evidence: public module re-exports (`lib.rs:1-12`), public raw storage fields (`schema.rs:107-242`), public facade construction (`storage/mod.rs:165-177`), and public state methods (`state.rs:22-66,92-149,192-205,231-239`) let another crate write arbitrary tags, overwrite an unformed limit, delete a record without index cleanup, or alter active membership. Public `record_ocomp_finality` accepts caller-provided authority facts (`ocomp/schema.rs:428-485`). Public fork install validates its genesis argument against the install's own genesis field (`ocomp/fork.rs:203-216`); the production loader does the real selected-chain check earlier (`blockchain/node/src/ocomp/fork.rs:23-50`).
- Reachable failure mode: any in-process module with a `StorageHandle` can bypass certified parent finality, construct record/index drift, create an impossible WWD status/type, or install/initialize state outside the intended owner path. This is not reachable from current user calldata, but it is reachable through the crate's advertised Rust API and makes future caller mistakes compile cleanly.
- Structural closure: make storage schema/facade and all invariant-bearing mutators private to a module aggregate. Export:
  - read-only typed projections;
  - one `apply(Command, Authority)` seam;
  - unforgeable authority variants constructed only by Cycle, certified-finality, fork-install, and result-vote adapters.
  Keep the broad emergency failure policy as an internal `CommandOutcome::Failed`, not a public state setter.
- Closure test: compile-fail tests must prove an external crate cannot construct the raw facade, write schema fields, record finality, install authority, or call state helpers. Behavioral tests must drive every mutation through the canonical adapter and check record/index equivalence after each command.

### CITADEL-003 — Timestamp-derived WWD updates can skip effect-owning edges

- Severity: **High**
- Gate: **G2, G3, G4**
- Evidence: `update_wwd_status` maps a timestamp directly to any later status and writes it (`state.rs:109-140`). The runtime unseals Tribute only when the derived target is exactly OFFERING, and seals only when the prior state was OFFERING (`runtime.rs:381-423`). The public status test covers exact edges in sequence, not a jump (`tests/state.rs:50-86`). Proposed ADR-C-MET-001 records this debt at lines 214-218.
- Reachable failure mode: a caller can advance FORMING or LOOKBACK_DELAY directly to WAITING/READY. The WWD then appears ready even though Tribute was never opened, and settlement can proceed without the OFFERING transition's effect. Canonical consensus timestamp drift and twice-daily ticks reduce ordinary production reachability, but the public command accepts the invalid jump and neither reducer nor persisted state records a named forfeiture.
- Structural closure: replace direct target assignment with a pure reducer that returns an ordered `TransitionPlan`. For each crossed edge, either:
  - replay the exact edge and receipt, or
  - invoke an explicit missed-window policy. Prior user-design history preferred fail-closed, but that memory may be stale and is not a current repository contract: no Desis/Lysis, preserve/retire Tribute according to the selected rule, and route the exact limit to carry-over once. Record the final choice in ADR-C-MET-001.
- Closure test: through the production Cycle/command adapter, cover every state with T-1/T/T+1, backward time, one-edge and multi-edge jumps, failed Oracle/Tribute writes between effects, reorg/replay, and assert no READY state exists without the required phase receipts or named forfeiture receipt.

### CITADEL-004 — Legacy Lysis writes a terminal outcome that production always rolls back

- Severity: **High**
- Gate: **G3, G4, G9**
- Evidence: on Lysis error, `process_metadosis` marks the WWD failed, emits the failure event, then returns the original error (`runtime.rs:605-615`). The Cycle trigger checkpoint reverts the handler and retries the same slot (`cycle/src/runtime.rs:83-123`). The direct test calls `start_metadosis` without that checkpoint and asserts the otherwise impossible durable `FAILED` state (`tests/lifecycle.rs:1231-1356`).
- Reachable failure mode: on a legacy/no-OCOMP profile, a persistent invariant error such as an existing Nod identity causes every attempt to abort the critical Cycle system transaction. The chain cannot commit the advertised `FAILED` state or event, while tests report it as the outcome. Operators see a retrying block failure with no canonical failure state.
- Structural closure: choose exactly one semantic class:
  - **fatal corruption:** return `Err` with no consensus writes and emit only non-authoritative diagnostics; or
  - **committed domain failure:** apply a typed `Failed` plan, route/preserve all value exactly once, and return `Ok(CommittedFailureReceipt)`.
  If OCOMP is now mandatory, remove the synchronous populated-day Lysis branch instead of retaining an untested fallback.
- Closure test: inject every Lysis/Nod/contributor/Tribute/Promis/event failure through the real Cycle system transaction. For `Err`, compare full semantic pre-state, events, CE work, cursor, and receipt. For a committed failure, assert the exact terminal receipt and successful block import. Direct and production tests must agree.

### CITADEL-005 — READY choice and phase advancement use incidental, unenforced-size set traversal

- Severity: **Medium — Decision required**
- Gate: **G6, G9**
- Evidence: `start_metadosis` reads all active WWDs and processes the first READY entry (`runtime.rs:129-153`); phase advancement also scans all active entries (`runtime.rs:258-280`). `StorageSet` is O(N), and removal replaces the deleted position with the last element (`storage/types/set.rs:8-16,71-107`). `MAX_RECORDS_KEPT = 365` caps the closed queue, not the active set (`constants.rs:28-29`). OCOMP's separate ready index is bounded and ordered, but the outer WWD scan still selects which day reaches it.
- Reachable failure mode: after removals and a multi-day backlog, the next READY day is selected by storage insertion/removal history rather than a domain rule. Raw public insertion can also make each midnight/noon command unbounded. All honest nodes remain deterministic for identical storage, but economic ordering/fairness is accidental and future refactors can change it.
- Structural closure: decide the protocol order, recommended `(scheduled_process_time, WWD)` oldest-first. Maintain one append-only, bounded lifecycle due index keyed by next edge/due time, or prove a fixed maximum active population and sort a bounded snapshot. Define cap-1/cap/cap+1 behavior and a monotonic catch-up cursor.
- Closure test: generated multi-record histories with insert/remove/requeue/reorg must compare to an independent ordered model, record distribution, prove oldest-first (or selected rule), bounded gas/work, no skip/repeat, and no starvation under continuous arrivals.

### CITADEL-006 — Legacy effect seams encode outcomes as `bool` and zero sentinels

- Severity: **Medium**
- Gate: **G5, G8**
- Evidence: Desis explicitly catches a failed brief, emits `AuctionDispatchFailed`, and returns `Ok(false)` (`crates/core/desis/src/api.rs:18-63`). Metadosis converts `false` to full Promis carry-over and can continue to COMPLETED (`metadosis/src/runtime.rs:620-646`). Both legacy and OCOMP limit sinks return `U256::ZERO` regardless of the actual formation receipt (`emission_sink.rs:13-38,60-104`). Legacy `apply` has no local replay marker and directly overwrites the amount.
- Reachable failure mode: caller code can ignore a zero sentinel or misinterpret a false brief as a technical success. Direct replay of legacy `apply` can overwrite the day limit and emit another accumulation event; only the wider Cycle/Rewards marker normally prevents it.
- Structural closure: introduce exhaustive receipts such as `LimitFormationReceipt::{Formed, ExactReplay}` and `AuctionBriefReceipt::{Accepted{hash}, RejectedToCarryOver{reason, credit_receipt}}`. The reducer must consume the receipt before terminal commit. No `bool`, zero sentinel, or undocumented caller idempotency guard remains.
- Closure test: fail before/after each Desis/Promis/Metadosis write and event; assert exact receipt, full rollback or selected committed fallback, same-intent replay equality, different-intent rejection, and real Cycle/EVM transaction parity.

### CITADEL-007 — Consensus arithmetic and stored discriminants are not closed

- Severity: **Medium**
- Gate: **G2, G6, G9**
- Evidence: WWD window construction and bootstrap use unchecked `u64` additions (`state.rs:30-33`; `runtime.rs:308-317`). `calculate_metadosis` multiplies the Tribute nominal total by 32 and subtracts allocation with unchecked `U256` operators (`runtime.rs:31-64`). Status and day type are raw `u8` (`schema.rs:15-33`), `set_wwd_day_type` writes any byte (`state.rs:147-149`), and `getWorldwideDaysByStatus` accepts any ABI byte without typed validation (`precompile.rs:48-50`). Tests cover ordinary examples, not calculation extrema.
- Reachable failure mode: extreme or corrupt state can wrap/panic/mis-account depending the operator semantics at an invariant-bearing boundary, while unknown query/status values are treated as ordinary bytes. The effect can be wrong budget conservation, deterministic block failure, or silent empty query output.
- Structural closure: use checked helpers/newtypes for window time, nominal, demand, supply, allocation, and remainder; validate maximum domain bounds before arithmetic. Decode persisted tags through closed enums and fail fatal on corrupt storage; reject invalid ABI query tags.
- Closure test: min/max, max-1/max/max+1 domain bounds, overflow at every intermediate, corrupt tag bytes, and cross-build parity. Add conservation properties for `day_limit = lysis_budget + auction_base` and `lysis_budget = used + unused`.

### CITADEL-008 — Evidence and ADR status lag the current implementation

- Severity: **Low**
- Gate: **G9, G10**
- Evidence: ADR-S-OCM-004 is Accepted but still says implementation is on `feat/ocomp-poc` and that no complete path exists (`ADR-S-OCM-004:1-5,408-419`), while current `main` contains the request/finality/vote/apply code and a real payload-builder/replay test (`crates/blockchain/evm/tests/ocomp_request_lifecycle.rs:362-530`). ADR-C-MET-001 still states the current implementation is synchronous and the selected OCOMP states are not implemented end to end (`ADR-C-MET-001:1-4,188-193`). No machine-checkable verification ledger maps the new code to the accepted claims.
- Reachable failure mode: reviewers and release gates cannot tell whether an accepted invariant is proven, merely implemented, contradicted, or stale. Direct fixture tests may be over-credited as production evidence.
- Structural closure: update ADR status/evidence sections from the exact current revision; create the VerificationLedger required by ADR-B-TST-001; classify every Metadosis/OCOMP test by real interface and substitution.
- Closure test: ledger generation must fail when a required ID has no discovered test, when a test is filtered/ignored, or when results are for a different revision/profile. Release consumes the exact-commit evidence manifest.

## Target architecture

Preserve and deepen the current OCOMP aggregate rather than replacing it. The target should be:

```text
Cycle / Emission / CertifiedFinality / ForkInstall / EVM vote adapters
                              |
                              v
          Metadosis::{query, apply(Command, Authority)}
                              |
                    module-owned checkpoint
                              |
       decode + validate typed WWD/OCOMP aggregate
                              |
            pure reduce(state, command, policy)
                              |
           ordered TransitionPlan with no writes
                              |
     typed effect adapters -> mandatory typed receipts
                              |
  validate receipts + atomically commit record/index/event
```

Concrete boundaries:

- `pub`:
  - typed read-only projections;
  - ABI dispatch;
  - adapter-specific entrypoints that carry unforgeable authority.
- `pub(crate)` or private:
  - `MetadosisContract` and all schema fields;
  - status/type setters, index operations, finality/profile/fork deep writes;
  - reducer and effect executor internals.
- Commands:
  - `CreateDay { scheduled_slot, wwd }`
  - `AdvanceDue { scheduled_slot, max_steps }`
  - `FormLimit { intent, base_limit }`
  - `ProcessReady { intent }`
  - `RecordCertifiedFinality { certificate_binding }`
  - `RunOcompLifecycle { height }`
  - `SubmitVerifiedResultVote { verified_vote }`
  - private `EmergencyFail { cause }`
- Persisted aggregate:
  - typed WWD state;
  - one exact membership (`active XOR closed`);
  - ordered lifecycle due key;
  - OCOMP FSM and ordered request/deadline indexes;
  - immutable effect/replay receipts.
- Error classes:
  - `Committed(OutcomeReceipt)` for a business terminal/fallback;
  - `RetryableErr` with full semantic rollback;
  - `FatalInvariant` with full rollback and non-consensus diagnostics.
- Test seam: the same `apply` function and transaction capability as production. Test-only varying dependencies are typed effect adapters; raw storage mutation is reserved for explicit corrupt-storage tests.

## Verification plan

1. **Determinism stop-ship**
   - Remove the debug environment branch.
   - Run debug/release differential execution of the same q-forming block with varied environments.
2. **Interface closure**
   - Privatize raw schema/deep mutators.
   - Add compile-fail external-crate tests and caller inventory checks.
3. **WWD reducer**
   - Freeze missed-window and READY-order policies.
   - Implement a pure transition plan and typed receipts.
   - Generate all legal/illegal state-event pairs, T-1/T/T+1, backward time, and multi-edge histories.
4. **Atomicity and effects**
   - Inject failure before/after each storage write, index change, event, Oracle/Tribute/Desis/Promis/Nod/Intex effect, and CE checkpoint boundary.
   - Compare semantic pre-state for every `Err`.
5. **Replay and capacity**
   - Exercise same intent/different intent, terminal retry, reorg, active/closed/due invariants, and cap-1/cap/cap+1.
   - Prove no starvation with continuous arrivals.
6. **Production interface**
   - Run the WWD model through real Cycle system transactions.
   - Run request/finality/open/vote/q-apply/expiry through proposer, validator/import, and historical replay.
   - Add a production-path owner-failure q-forming case, not only direct fixture calls.
7. **Project contract**
   - Update ABI/README/ADRs, schema activation/backfill, and the exact-commit verification ledger.

Suggested Linux verification commands after closure:

```bash
cargo test -p outbe-metadosis --features test-utils
cargo test -p outbe-cycle
cargo test -p outbe-evm \
  --test ocomp_request_lifecycle \
  --test ocomp_result_votes \
  --test ocomp_atomic_apply \
  --test ocomp_logical_time
```

These commands are only discovery anchors. The release ledger must pin the exact features, profile, toolchain, test count, and required CI lane.

## Verification log

| Date | Command/check | Result |
|---|---|---|
| 2026-07-30 | `git status --short --branch` before report | PASS: clean `main...origin/main` |
| 2026-07-30 | Static test discovery with `rg` | 77 Metadosis package tests and 7 adjacent EVM OCOMP tests discovered; this proves discovery only, not execution |
| 2026-07-30 | `cargo test -p outbe-metadosis` on `aarch64-apple-darwin` | **BLOCKED before tests**: `outbe-ocomp-protocol/src/local_control.rs` unconditionally references Linux-only `libc::ucred` and `SO_PEERCRED` at lines 747-768 |
| 2026-07-30 | `cargo check -p outbe-metadosis --tests --target x86_64-unknown-linux-gnu` | **BLOCKED before module check**: installed Rust target lacks `x86_64-linux-gnu-gcc`, required by native dependencies including `blst` and `zstd-sys` |
| 2026-07-30 | Current production request evidence inspection | `ocomp_request_lifecycle` builds the real payload, asserts begin/end system layout, replays through the validator executor, and compares execution/state-root outputs (`lines 447-530`) |
| 2026-07-30 | Current q-forming rollback evidence inspection | owner/receipt fault tests compare rollback snapshots, but representative failures call `ActivationFixture::apply` directly; only completed exact retry crosses the real public selector (`ocomp_atomic_apply.rs:61-157`) |
| 2026-07-30 | Current independent model inspection | 512 deterministic operations compare `JobFsmState` to a separate in-memory model (`tests/ocomp_fsm_model.rs:282-429`); persisted indexes, production adapter, shrinking, and case distribution are absent |

No test is reported as passing in this audit. The two attempted build commands failed before test execution for platform/toolchain reasons unrelated to a Metadosis assertion.

## Decisions required

1. **Missed OFFERING:** adopt and record the fail-closed policy, or specify another named forfeiture. Silent timestamp jump is not an option.
2. **READY ordering:** select oldest scheduled WWD/WWD key, another explicit priority, or a bounded batch rule. Storage-set order is not a policy.
3. **Legacy corruption:** decide whether synchronous Lysis corruption is a fatal chain stop, committed FAILED outcome, or removable legacy path.
4. **Legacy profile support:** if every supported network activates OCOMP before a populated WWD can become READY, delete the synchronous populated-day fallback and document the activation proof.
5. **Emergency failure sink:** prior user-design history preferred broad emergency semantics, but that memory may be stale. Confirm it, then keep the sink private and invoke it only through a reducer outcome with a complete value-routing rule.
6. **Build support:** decide whether macOS is a supported development/test platform. If yes, gate or implement peer credentials portably; if no, make Linux execution available as the required verification lane.
7. **Compatibility point:** state whether any deployed chain carries the current Metadosis layout. That decides whether a new due index needs a fork/backfill or can be genesis-only.

## Migration and documentation impact

- Raw Rust visibility can be closed without changing EVM layout or ABI.
- Status/day-type can become typed in memory while preserving the stored `u8` discriminants. Unknown stored values must change from permissive behavior to fatal corruption handling.
- `active_wwd_count` occupies a historical slot before all OCOMP fields. Do not remove or shift it physically; rename/document it as reserved, or keep a verified compatibility wrapper.
- A lifecycle due index must be append-only and needs:
  - an exact activation height;
  - an empty-state/genesis rule or deterministic backfill from bounded existing records;
  - mixed pre/post-fork replay tests;
  - fixed slot/layout assertions.
- Removing the environment failpoint requires no state migration.
- Invalid `getWorldwideDaysByStatus` bytes changing from empty-result behavior to revert is an ABI behavior change and should be recorded, even though the Solidity signature is unchanged.
- If legacy synchronous Lysis is removed, update README emission behavior, ADR-C-MET-001, ADR-C-LYS-001, PFS-002/PFS-009, and any operator expectations for pre-OCOMP profiles.
- Update ADR-S-OCM-004 and ADR-C-MET-001 implementation/evidence statements to match `main`.
- Add exact requirement/test mappings and platform requirements to the VerificationLedger before changing an ADR status to Implemented.

## Definition of done

- [ ] **G1:** no external crate can construct raw Metadosis storage or call an invariant-bearing mutator; every mutation enters one command seam with structural authority.
- [ ] **G2:** all persisted tags decode through closed types; record/active/closed/due/OCOMP equivalences are validated or preserved by construction.
- [ ] **G3:** one exhaustive reducer defines all WWD and OCOMP state/event pairs, including backward time, catch-up, emergency failure, terminal absorption, and progress.
- [ ] **G4:** every command owns its complete storage/CE atomicity domain; every `Err` restores semantic pre-state; no test observes production-reverted state.
- [ ] **G5:** Desis, limit, Promis, Tribute, Oracle, Nod, contributor, and terminal effects return exhaustive typed receipts that the reducer consumes.
- [ ] **G6:** no process-local input changes consensus; all arithmetic is checked; selection/order/work is bounded with explicit cap and cursor policy.
- [ ] **G7:** `active XOR closed`, terminal/index, record/index, and OCOMP due/live equivalences pass after arbitrary accepted/rejected histories; duplicate counter state is removed or verified.
- [ ] **G8:** same-intent replay returns the same recorded result without effects, different intent rejects, terminal retry/reorg is explicit, and persistent failure has a named recovery or fatal policy.
- [ ] **G9:** the independent persisted-state model and fault matrix run through production interfaces; proposer/import/replay parity and case distribution are recorded; all required commands pass on the supported platform.
- [ ] **G10:** schema activation/backfill, reserved slots, ABI behavior, README/ADR/PFS changes, operator impact, and exact-commit evidence ledger are accepted together.
- [ ] The Critical environment-dependent execution finding is closed before any debug artifact can participate in a network.
- [ ] Every finding above has its stated closure test in a required CI/release lane.
