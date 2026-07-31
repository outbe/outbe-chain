# ADR-B-EVM-001: Protocol block work executes in one deterministic begin/user/seal order

- **Status:** Proposed (documents the observed current implementation)
- **Date:** 2026-07-22
- **Scope:** `crates/blockchain/evm`, `BlockLifecycle` implementations, reserved system transactions and header artifacts
- **Depends on:** ADR-B-CNS-001, ADR-B-CNS-002, ADR-B-EVM-004
- **Related:** ADR-B-EVM-002 through ADR-B-EVM-005, ADR-B-OCD-001, all System/Core module-owner ADRs,
  ADR-B-OCD-007 through ADR-B-OCD-013 compressed entities

## Context

Outbe stateful precompiles need work at block boundaries: finalized-parent
accounting, late credits, validator-set activation, daily/economic ticks, update
activation, unbonding, oracle maintenance, receipt-visible hook events and
compressed-entity sealing. If proposer and validator execute this work through
different paths or incidental module order, they can derive different state,
receipts or header roots.

## Decision

`OutbeBlockExecutor` is the sole production ordering authority. There is no runtime
plugin registration. The high-level block sequence is:

```text
validate header/system layout
-> standard Ethereum pre-execution
-> preserve every runtime-precompile account under EIP-161
-> open block-scoped compressed-entity overlay
-> validate proposer and pure cryptographic preflights
-> pre-execute/commit Phase 1 and cache its body witness
-> atomically run non-receipt lifecycle hooks in explicit order
-> execute reserved begin-zone body transactions in cursor order
-> execute user transactions
-> close/seal compressed entities
-> validate execution summary and CE header artifacts
-> finish Ethereum execution / compute roots / assemble block
-> publish proposer CE candidate only under the final block hash
```

Block 0 has no begin-zone system transactions. Block 1 follows the genesis
bootstrap layout. Blocks `>= 2` begin with CertifiedParentAccounting followed by
LateFinalizeCredits; their calldata and ordinal are re-derived and compared on
validator execution (`evm/src/executor.rs:1725+`). BoundaryOutcome, TeeBootstrap,
CycleTick, OracleSlashWindow and HookEvents occupy their versioned conditional
positions as defined by `SystemTxPhase` and the system-tx codec.

### Lifecycle interface

Stateful boundary modules implement `BlockLifecycle` on zero-sized marker types.
The associated `Context` carries `BlockRuntimeContext` and any additional typed
least-authority capability; direct ad-hoc timestamp/block-number hook entrypoints
are not the canonical seam (`primitives/src/block.rs:67-89`). Persistent access is
through the scoped `StorageHandle`.

### Non-receipt hook order

After Phase 1 precommit, the current explicit pre-execution order is:

1. genesis state validation where applicable;
2. Vote tally/approved-handler dispatch;
3. scheduled Update activation;
4. Rewards genesis initialization;
5. epoch-boundary slash counter reset, ValidatorSet transition and capped inactive cleanup;
6. Staking matured-unbonding processing;
7. Oracle period/daily processing excluding slash-window exits;
8. Gem maturity/floor promotion;
9. Intex maturity/floor qualification and proceeds settlement sweep;
10. Desis auction clearing fan-in gate.

Cycle owns UTC-midnight/noon economic orchestration and compressed-body mutations
through receipt-visible system transactions. Oracle slash-window exits execute
after BoundaryOutcome so an incoming target is activated before penalties can mark
members EXITING (`executor.rs:516-606`).

All non-receipt hooks in one invocation run inside `StorageHandle::with_checkpoint`;
only after success does the provider flush and expose committed changes/events to
Reth (`executor.rs:609-659`). A hook error rolls back that hook batch and fails the
block. Whitelisted hook logs are carried by mandatory HookEvents receipt; other
hook events are diagnostic-only.

### Transaction and overlay atomicity

Every transaction opens a compressed-entity work checkpoint before dispatch.
Reserved system transactions must execute with commit and must match the phase
cursor, expected kind, exact calldata, gas envelope and signature/witness rules.
Phase 1 is committed once in pre-execution so later hooks observe it; body[0] is
retained and hash-validated without double execution or double gas.

User transaction EVM journal rollback and CE work rollback must agree. Successful
transactions advance receipts and CE overlay; rejected/reverted work cannot leak
body/index mutations. Final sealing consumes the overlay once and validates the
header-carried scheme/root. Proposer candidates are published only after assembly
provides the exact block hash (`evm/src/builder.rs:141-218`).

## Block FSM

| Current | Event | Guard | Effects | Next/error |
|---|---|---|---|---|
| Created | apply pre-execution | valid beneficiary/layout/artifact scheme | Ethereum setup + markers + CE begin | Preflight |
| Preflight | verify consensus/credit artifacts | pure checks pass | no mutation | Phase 1 commit |
| Phase 1 commit | execute canonical witness | expected signed input | accounting state + receipt, cache witness hash | Lifecycle hooks |
| Lifecycle hooks | ordered hook batch | every hook succeeds | checkpointed state; partition events | Begin-zone loop |
| Begin-zone loop | reserved tx | exact cursor/kind/calldata/signature/gas | commit state/receipt; advance cursor once | Next phase/user zone |
| Begin-zone loop | mismatch/failure | any invariant violated | rollback transaction/block | Invalid block |
| User zone | user tx | normal EVM + ZeroFee policy | receipt/state/CE work | User zone |
| User zone | soft ZeroFee rejection | within per-block cap | status-0 deterministic receipt, no EVM state | User zone |
| User zone | soft-failure cap exceeded | more than 64 | proposer excludes / validator rejects over-cap block | Continue or invalid block |
| End | finish | all mandatory phases consumed | CE seal + artifact validation + Ethereum finish | Built/validated |
| Built proposer | final hash known | immutable seal output | publish CE candidate + execution summary | Candidate ready |

## Persistent invariants

- Every runtime marker address is preserved as a non-empty account before state-root computation.
- Proposer and validator use the same system phase order and exact system inputs.
- Phase cursor advances exactly once per consumed reserved transaction.
- Critical system phase failure invalidates the block; it cannot become an ignored receipt.
- Non-receipt hook batch is all-or-nothing within its storage checkpoint.
- CE scope opens before any body read/mutation and closes/seals exactly once.
- Receipt ordering matches body transaction ordering, including system witnesses.
- Header execution summary and CE root equal locally derived post-state outputs.
- `BlockContext.validators` is sorted canonical active consensus membership.

## Side-effect ledger

| Effect | Owner | Atomicity domain | Receipt/error | Replay |
|---|---|---|---|---|
| Ethereum pre-block changes | inner executor | Reth block execution journal | block result | deterministic re-execution |
| Marker injection | Outbe executor | block state diff | state-root hook notification | idempotent when code exists |
| Lifecycle hook mutations | explicit hook batch | `StorageHandle` checkpoint + DB flush | block error or committed diff | full block re-execution |
| Reserved system tx | phase router/precompile | transaction journal + receipt | typed phase outcome | exact body witness |
| User tx | EVM/precompile dispatcher | transaction journal + receipt | normal/soft-failure/error | tx hash/nonce semantics |
| CE overlay/seal | compressed-entities lifecycle | block overlay then staged tree batch | `SealOutput` | exact parent/block identity |
| Header artifacts | builder/executor | block assembly/validation | block hash/root | byte-identical re-execution |
| Hook tracing | logging | diagnostic | none | may repeat |

## Determinism and bounds

- Hook order is source-defined and fork-governed.
- Validator lists are sorted before entering context.
- Reserved phase ordinals and codecs are versioned protocol data.
- Inactive validator cleanup is capped at 16 per epoch; its selection/starvation
  policy belongs to ADR-S-VAL-001.
- ZeroFee synthetic failures are capped at 64 per block with deterministic 21k
  visible gas and one canonical failure log.
- Internal system execution has a separate bounded lane; visible receipt gas and
  header gas semantics must remain proposer/validator-identical.
- CE work/gas/capacity closure remains ADR-B-OCD-009 debt.

## Replay, reentrancy and nested calls

Top-level and nested precompile dispatch share the same `ExecutionScope`; a nested
call cannot open a second independent CE lifecycle. Storage facades are short-lived
and scoped. Reserved system addresses are rejected from ordinary uncommitted
execution paths. Re-execution validates the exact system body rather than trusting
locally regenerated intent alone.

## Verification evidence

Executor/builder tests cover marker preservation, system layout/calldata, Phase 1
preexecution witness retention, proposer/validator state-root parity, hook rollback,
Cycle/Oracle receipts, soft-failure caps, CE sealing and header artifacts. Core e2e
and Rust localnet scenarios exercise real blocks and state-root agreement.

No independent model currently enumerates every conditional phase combination and
failure injection point.

## Consequences

- Ordering is centralized and reviewable but changes to the executor are protocol changes.
- Receipt-visible critical phases are auditable from canonical block bodies.
- Pure preflight before mutation reduces partial side-effect risk.
- Some lifecycle work remains non-receipt state change, requiring HookEvents or
  other explicit evidence when external observability is required.

## Rejected alternatives

### Runtime plugin registration for hooks

Rejected because registration/container order would become consensus-visible and
make the complete execution sequence difficult to audit.

### Execute Phase 1 only when body[0] reaches the loop

Rejected because subsequent pre-execution hooks require the finalized-parent
accounting state while the canonical witness must remain in the body.

### Emit every hook event only through tracing

Rejected for protocol-relevant outcomes because logs are not canonical receipts.

## Open questions and technical debt

- `run_outbe_pre_execution_hooks_inner` is a long manual ordering list. Add an
  executable ordering manifest/golden test that fails when a lifecycle is added,
  removed or reordered without updating this ADR.
- Some modules expose legacy/ad-hoc hook functions alongside `BlockLifecycle`.
  Audit all callers and delete bypasses or mark them internal implementation seams.
- Phase 1 is state-committed before its body witness is encountered. Fault tests
  must prove any later witness mismatch rolls back the entire block, including
  state-root background notifications, not merely the transaction loop.
- Hook batch changes are committed to the temporary DB before later system/user
  transactions can fail. Reth block-level rollback is imported behavior and needs
  explicit production-interface evidence for each later failure boundary.
- Non-whitelisted hook events are tracing-only. Classify every current event and
  prove no business receipt is accidentally dropped from canonical observability.
- Inactive cleanup cap 16 needs deterministic ordering, cursor/progress and
  starvation tests in ADR-S-VAL-001.
- Internal system gas versus Ethereum-visible gas is complex and has previously
  produced fee-history assertion drift; ADR-B-OCD-009 must close limits and RPC semantics.
- `execute_outbe_block_hooks == false` creates a pending-RPC path that opens no CE
  overlay. ADR-B-TXP-001 and ADR-B-OCD-001 must state exactly which calls may use it and prevent consensus
  or `eth_call` compressed-body reads from entering an invalid lifecycle.
- Test-only Phase 1 verification opt-out must remain unreachable from production;
  compile-time gating alone should be covered by build/config audit.
- `expected_end_system_txs` is retained but marked dead-code in the executor.
  Remove the dormant interface or specify the end-zone protocol before use.
- No generated matrix covers every combination of BoundaryOutcome, TeeBootstrap,
  Cycle/Oracle triggers, HookEvents, user reverts, CE mutations and artifact errors.
- This ADR requires human acceptance before its `Proposed` status changes.
