# ADR-S-ACC-001: Accounting owns the certified-parent Phase-1 progress marker

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Execution and accounting maintainers
- **Scope:** `crates/system/accounting`
- **Depends on:** ADR-B-CNS-002, ADR-B-CNS-003, ADR-B-EVM-004
- **Related:** ADR-S-CYC-001, ADR-S-RWD-001
- **Supersedes:** None

## Context

Cycle and Rewards must know whether certified-parent accounting for a block has
committed before running dependent settlement. Accounting stores the canonical
on-chain progress marker. It does not calculate fees or rewards; it certifies the
highest exact-parent Phase-1 height completed inside EVM state.

## Decision

Accounting owns one field at `ACCOUNTING_PROGRESS_ADDRESS`:

```text
last_accounted_block_number: u64
```

Slots 1 through 15 are reserved and must remain zero until an activated schema
revision assigns them.

The sole writer is the executor's receipt-visible Phase-1 system transaction after
all Phase-1 accounting effects for exact parent `N` have succeeded. The marker is
written last in the same checkpoint. Readers in Cycle/Rewards use it only as a gate,
not as proof of arbitrary off-chain work.

The semantic transition is:

```text
current N --successful exact-parent accounting for N+1?--> N+1
```

Genesis zero means no post-genesis Phase 1 has committed. Re-execution under EVM
rollback restores the prior marker.

## Authority and invariants

- Only the executor Phase-1 path writes slot zero.
- Marker is monotonic and, after genesis, advances by exactly one expected parent
  height per successful accounting phase; it never skips or regresses.
- Marker changes only after all effects it certifies have committed.
- A failed phase changes neither marker nor any certified effects.
- Reserved slots remain zero and are checked at genesis/startup/migration.
- Readers compare against the exact expected parent, not merely `>=` a convenient
  value that could hide skips.

`BlockRuntimeContext` binds reads/writes to the executing block, but caller closure
still requires structural proof.

## Atomicity, replay and failure

The marker and Phase-1 side effects share the system-transaction checkpoint. An
attempted regression, duplicate outside explicitly idempotent replay, gap, reserved
slot violation or context mismatch is an invariant error. It must not be logged and
continued because downstream settlement would consume incomplete data.

Canonical reorg rolls marker back with EVM state. Node restart reads it from state;
no local journal is authority.

## Compatibility and evidence

Address, slot zero type/meaning, reserved range and phase ordering are consensus
schema. Future fields require explicit schema version/activation and genesis
migration.

Inspected schema, state helpers, runtime monotonic check and executor/Cycle/Rewards
usage documented by the crate. Current runtime rejects only regression while relying
on the outer precompile/executor for strict exact-parent sequence; direct unit-level
coverage is insufficient for sole-writer proof.

## Consequences

A tiny state module provides a durable cross-phase handshake without making Cycle or
Rewards infer completion from events. Its narrowness increases, rather than reduces,
the need for exact transition and caller tests.

## Rejected alternatives

- **Use an in-memory executor flag:** it is not replay/restart/canonical-state safe.
- **Infer completion from reward records:** multiple consumers would duplicate
  partial logic.
- **Allow arbitrary monotonic jumps:** missing accounting becomes permanently hidden.
- **Reuse reserved slots casually:** future migrations lose a clean compatibility
  envelope.

## Open questions and technical debt

1. `record_phase1_progress` currently permits equality and jumps, rejecting only
   regression. Enforce the exact expected transition in the deepest sanctioned
   writer or prove it cannot be called outside the stricter wrapper.
2. Add a structural compile/test assertion that executor Phase 1 is the only writer
   of `last_accounted_block_number` and raw slot helpers remain inaccessible.
3. Prove marker write is last after every certified effect and event; inject failure
   immediately before/after it and compare semantic state.
4. Define genesis/block-one semantics precisely: zero is both a block number and
   “nothing accounted,” which can be ambiguous for parent zero.
5. Add startup invariant checks for all 15 reserved slots in production and in
   tests/genesis construction.
6. Specify behavior for snapshot import at nonzero height and validate imported
   marker against Rewards/fee/participation state.
7. Readers must distinguish exact match, behind and impossible ahead. Audit every
   comparison in Cycle and Rewards.
8. Decide whether protocol version/schema version should be stored in a reserved
   slot before mainnet migrations are possible.
9. Add generated phase sequences covering success, duplicate, gap, regression,
   reorg, restart and downstream-gate failures.
10. Clarify fatality: a marker inconsistency should halt block execution/startup,
    not appear as an ordinary user revert.
