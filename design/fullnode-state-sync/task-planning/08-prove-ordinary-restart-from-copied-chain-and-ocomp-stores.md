# 08. Prove ordinary restart from copied chain and OCOMP stores

Beads: `outbe-chain-g14b.19`. Baseline: `e5d545cedc872c0e579fd3535e7638d883d5b1f5`.

## Outcome

Demonstrate native copied-store recovery and subsequent restart through the existing code. Add test coverage; do not introduce a new recovery implementation by assumption.

## Dependencies and delivery

Depends on: 03.

Native recovery test PR; engine/launch production code is not preemptively rewritten.

## Exact file changes

| File | Action | Responsibility |
|---|---|---|
| `crates/blockchain/engine/src/stack/tests/harness.rs` | modify | Extend start_recovery_marshal fixture to preserve/copy real disk partitions and processed metadata under a different root. Accept explicit stable partition prefix for paired copy/reopen fixtures instead of fresh atomic test_id each invocation. |
| `crates/blockchain/engine/src/stack/tests/restart_recovery.rs` | modify | Copied nonzero archive/processed/forkchoice/CE recovery cases with actual data files. |
| `crates/blockchain/engine/src/stack/tests/recovery.rs` | modify | Extend existing anchor inequalities and missing-archive tests; no synthetic snapshot-floor authority. |
| `bin/outbe-chain/src/ocomp_exex/tests/recovery.rs` | modify | Separate-root native closure/spool/retention/job/CAS/result fixtures; C=H, C<H, frame cursor and second restart assertions. |
| `bin/outbe-chain/src/ocomp_exex/tests/materialization.rs` | modify | Pending NOD/public dependencies after terminal runtime job removal; retained body and lawful release behavior. |
| `bin/outbe-chain/src/launch/tests/state_sync.rs` | new | Test only: normal configured root/identity mapping after file placement; no special startup command/receipt or start_block=H. |
| `bin/outbe-chain/src/launch/tests/mod.rs` | administrative | Register native copied-layout tests. |

## Implementation

1. Use existing start_recovery_marshal / start_recovery_marshal_with_reporter and CE recovery fixtures. Stop normally, copy the real public partitions and metadata to a separate root, and reopen them with the ordinary harness. Parameterize the existing harness partition prefix: current start_recovery_marshal generates a new atomic test_id prefix each call; donor and reopened receiver must use the same explicit prefix. Prove data exists before reopen to avoid accidentally testing a fresh empty store.
2. Retain existing recovered_canonical_block_is_acknowledged_without_reexecution, conflicting recovered hash and CE commit-barrier tests. The ordinary actor already handles exact recovered H; no new actor or callback is needed.
3. With C=H and independently established reusable completed local results/required public outputs, assert those jobs are reused without recomputation using actual worker invocation counts. C=H and canonical Completed alone do not prove local completion. With C<H, copy the actual required canonical frames/receipts from C+1; observe ordinary scanning/result reuse and closure advancement across multiple batches. Missing required replay data retains native failure; never manufacture cursors or replay EVM just to satisfy a test.
4. A Marshal processed cursor H-1 with archive/execution/CE H follows existing anchor and ACK rules. Test missing archives, wrong hashes and incomplete projection using ordinary errors; never repair the fixture by stamping processed/C=H. Unequal CE/projection and partial execution fixtures retain their actual markers and native behavior; no blanket promise that every such image starts successfully.
5. Preserve recipient own keys/sign-once/signed journals and fatal evidence. Deliberately foreign sender journals remain rejected by existing checks; preserving public work does not grant donor authority.
6. Advance through K>H, then reopen the current stores with no archive/manifest/validation receipt. Configuration-only tests must not be described as a real process launch; task09 provides process acceptance.

## Tests first

1. Real disk Marshal copy, matched and lagging processed metadata, exact-H ACK without EVM, H+1 successor, conflicting-hash and CE barrier controls.
2. Native OCOMP C=H/C<H, real retained bodies and CAS/admissions/results, first frame C+1, more than one replay batch, missing required receipt/body, worker-invocation count and no fabricated completion.
3. Pending NOD after terminal job pruning; old-body lawful GC then K restart; fatal evidence remains fatal; protected own identity unchanged.
4. Real native recovery prerequisites: present and deliberately missing genesis-state trust anchor, plus required validator DKG freeze-state/header using existing readers. Preserve ordinary failure for missing history; do not seed it from manifest or weaken recovery. Upstream epoch history availability is a separate ordinary network prerequisite.
5. Reopen actual nested job/ordinal NOD records, stale/released refs, retired discovery spool and pruned Released pins; include partial/empty GcPending plus a shared still-live lease. Preserve unequal Q/P and exercise native recovery/error behavior without promising every unequal or partial image starts. No deleted-history reconstruction or cursor stamping.
6. Use a real Reth-backed CE checkpoint reader for copied A/A-1 header/root access, including equal CE markers; lagging CE also needs actual suffix receipts/state. MemorySource/MemoryTree tests alone do not establish copied-node readiness. Missing-history controls preserve ordinary failure and do not trigger snapshot reconstruction.
7. For a completed-Lysis C=H copy with pending public NOD/recent payout, distinguish successful quiet startup from action submission: with no new finalized frame there is no immediate submission requirement; after the next eligible finalized frame verify ordinary recipient-authorized processing. FullNode remains non-submitting.

## Definition of done

1. Tests exercise actual persisted native markers and requested frame heights, not only scalar mocks.
2. Main launch/consensus/DKG/recovery production stays unchanged unless a concrete failing test establishes a separately recorded minimal defect.
3. No checkpoint_startup, snapshot root selector, snapshot lifecycle or mandatory offline-validation gate is introduced. Applicable tests/fmt/Clippy pass.
4. Ordinary ExEx cursor handling and payout scheduling/submission behavior stay unchanged. Tests verify reuse of copied data through existing behavior; they do not introduce a historical payout requirement.

## Execution discipline

Write the listed failing behavioral unit/fixture tests first, implement the complete slice, then run its integration checks. Task 09 owns real process E2E. Do not count source review or tests from the discarded branch as execution evidence. Keep live progress in Beads.
