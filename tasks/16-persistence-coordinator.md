# T16 — Finalized persistence coordinator: durable-Reth barrier → SMT commit → Marshal ACK

Status: todo
Source: `compressed_entities_concept_v6_proposed_10-07-2026.md` §13.2 (Q9, Q14, Q16)
Depends on: T12, T15, T32 (persistence spike: concrete APIs, retention floor, fault map)
Blocks: T14 (block-1 checks), T17, T24

## Summary

Implement the finalized persistence coordinator enforcing the complete order: Marshal durable
block+finalization → Reth execution + finalized FCU → forced durable Reth checkpoint → atomic SMT commit →
Marshal ACK, with the startup-enforced Reth configuration.

## Context

A canonical notification / `new_payload` / FCU success is not a durability barrier. With CE active, Reth
must start with `persistence_threshold = 0` and `memory_block_buffer_target = 0` — incompatible config fails
startup. After finalized FCU for H the coordinator waits for `PersistedBlockSubscriptions`, then verifies
via a DB-only provider: `persisted_tip >= H`, exact canonical hash, durable block/receipts/EVM state, and
`last_sealed_root(H)`. Only then SMT commit begins; only after commit does the executor ACK Marshal.
`MAX_PENDING_ACKS = 1` is a protocol-required startup invariant (already the repo default,
`consensus/src/config.rs:158`) — marker lag is bounded at one in-flight block in live ACK-gated operation (amended §13.2; bootstrap
catch-up legitimately exceeds it). Current ACK site:
`crates/blockchain/consensus/src/executor/actor.rs` (`ack.acknowledge()` after finalized FCU) moves behind
the barrier + commit.

## Scope

- Startup validation: TreeConfig values and `MAX_PENDING_ACKS = 1` asserted with CE active (gated by
  T14's `ces_active` chain-spec predicate — postfix PF-H07); structured
  startup failure otherwise.
- Coordinator: subscribe to persisted-block notifications; DB-only provider verification step; invoke T15
  `apply_finalized` with the staged batch (from T12 speculative cache) and marker; then ACK.
- Invariant enforcement: `persistent_tree_height <= min(durable_evm_height, consensus_finalized_height)`.
- Batch-miss path (audit-final B-05): if the speculative batch for H is absent (restart), the coordinator
  returns a typed `RebuildRequired {height, expected_hash}` outcome while HOLDING the ACK/pruning posture
  (no ACK sent, retention lease kept) — T16 closes with a FAKE rebuild-handler test; the real rebuild
  orchestration and its integration test are T17's, and rebuild never synthesizes an ACK.
- Commit-time binding (audit-final H-05): before `apply_finalized`, the coordinator validates that the
  staged-batch envelope (T12), the durable `last_sealed_root(H)` slot, the header artifact root, and the
  marker-to-be all describe the same `{block_hash, parent, root}`; a wrong-fork/stale/corrupt batch is
  rejected fail-closed.
- Local storage-failure posture (audit-final H-13): on a typed T15 capacity/I/O failure the coordinator
  sends NO ACK and releases NO retention lease; bounded local retry; unrecoverable ⇒ the T17 halt/resync
  path.
- Transient-vs-corruption classification (audit-final M-10; concrete inputs from the T32 spike):
  channel/subscription drops and provider-busy conditions are TRANSIENT — bounded retry with posture
  held; a proven contradiction (durable state conflicting with consensus finality) is corruption ⇒ the
  fail-closed T17 row; the classifier is typed and tested for both classes.
- Metrics/logging: barrier wait time, commit time, ACK latency (feeds T24 benchmark and ops).
- Dynamic pruning hold (in addition to the static config guard): canonical events/receipts for heights
  above the CE tree marker are never pruned while a catch-up is in progress — a retention lease from
  `persistent_tree_height + 1` up to the catch-up target, released as the marker advances (mirrors how
  `FinishedHeight` already holds Reth pruning for the projector). This protects an in-flight bootstrap or
  crash-recovery replay from losing its own tail mid-run; the static startup floor alone cannot.
  Ordering (audit-final M-06): the lease is ACQUIRED AND VERIFIED BEFORE a snapshot/bootstrap activation
  commits (the T22 importer calls this API pre-activation), is held while head advances, and is
  RE-ACQUIRED on restart before pruning resumes; bootstrap leases are keyed by the T22 `import_id` and
  re-verified on restart before the activation state machine resumes (postfix PF-M02).
- Retention policy config + pruning guard (§14.5, §12.1): node config for canonical event/receipt retention
  with CE-safe defaults. The HARD startup floor covers only what consensus-critical recovery mandates: the
  T17 rebuild window (one in-flight block by MAX_PENDING_ACKS = 1, plus a safety margin). Longer retention
  (event tail back to an advertised snapshot H, per-key replay ranges for T21) is an OPERATIONAL choice, not
  a startup predicate: insufficient retention automatically disables snapshot bootstrap-capability
  advertisement (T22 flag) and degrades per-key event recovery to the remaining sources (peers/snapshots) —
  never blocks node startup. README documents both tiers.

## Out of scope

- Restart matrix logic (T17); Marshal-side changes (none needed — ACK gating is existing Commonware behavior).

## Acceptance criteria

1. Ordering test with fault injection at every arrow of the §13.2 chain: no ordering violation observable
   after restart (§19.11 subset: before/after Reth FCU, Reth durable persistence, SMT tx, Marshal ACK).
2. Startup fails when `persistence_threshold != 0`, `memory_block_buffer_target != 0`, or
   `MAX_PENDING_ACKS != 1` with CE active.
3. Chain pause at tip: block H still commits and ACKs (no tip-stall) — regression test for the r1 finding.
4. Localnet: 4-validator network seals, finalizes, persists, and survives node restarts (`localnet-smoke`).
5. Retention guard: a config pruning receipts inside the T17 crash-recovery window fails startup with a
   structured error naming the violated floor; retention below the operational tier does NOT block startup
   but disables bootstrap advertisement and logs the degradation.
6. Pruning hold: a pruning attempt targeting heights above the tree marker during an in-progress catch-up
   is held until the marker passes them; restart after a partial replay resumes with the tail intact
   (fault-injection test).
7. `RebuildRequired` path (audit-final B-05): batch-miss yields the typed outcome with posture held; the
   fake-handler test is green; no ACK before commit is ever observed.
8. Envelope binding (audit-final H-05): a wrong-fork, stale, or corrupt staged batch is rejected at commit
   time (red fixtures for each).
9. Storage-failure posture (audit-final H-13): induced MDBX capacity/I/O failures produce no ACK and no
   lease release; recovery by retry after the fault clears.
10. Transient matrix (audit-final M-10): transient injections recover by bounded retry with posture held;
   contradiction fixtures fail closed into the T17 row.

## Invariants

- The tree never moves ahead of durable EVM state or consensus finality; derived-state lag only.
- ACK is sent exactly once per finalized block, after the SMT marker commit.

## Tests

- Crash-injection harness around the coordinator; localnet restart scenario; invariant property checks.

## Files

- `crates/blockchain/consensus/src/executor/actor.rs` (ACK relocation)
- `crates/core/compressed_entities/src/persist/coordinator.rs`
- `bin/outbe-chain/src/main.rs` (startup validation)
