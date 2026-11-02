//! - Phase 1 pre-execution reorder invariants.
//!
//! Tests in this file pin the public-API surface of `SystemTxPhase` and
//! `expected_begin_block_selectors` that drive the begin-zone phase routing.
//! Some tests assert the *cursor-level* invariant that lays the foundation
//! for moving Phase 1 commit into pre-execution. Tests whose verification
//! requires the actual commit-timing move are marked `#[ignore]`. They point
//! to (Phase 1 commit-into-pre-execution + state-root ordering).

use outbe_primitives::system_tx::*;

const BLOCK_1: u64 = 1;
const BLOCK_2: u64 = 2;
const BLOCK_42: u64 = 42;

/// block 1 (genesis bootstrap) must skip Phase 1
/// and start the cursor at CycleTick. No Phase1Preexecuted variant must be
/// produced for the bootstrap block under any condition.
#[test]
fn apply_pre_execution_changes_skips_phase1_for_block_1() {
    let cursor = SystemTxPhase::initial_for_block(BLOCK_1, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert_eq!(cursor, SystemTxPhase::CycleTick { body_index: 0 });
    assert_eq!(cursor.expected_selector(), Some(CYCLE_TICK_SELECTOR));
    assert!(matches!(cursor, SystemTxPhase::CycleTick { .. }));
    // Crucially, no Phase 1 variant for block 1.
    assert!(!matches!(cursor, SystemTxPhase::Phase1Preexecuted { .. }));
}

/// Cursor body-index ordering through the first scheduled phase of the V2
/// block-`n>=2` canonical layout.
#[test]
fn phase1_receipt_index_0_before_cycle_tick() {
    let phase1 = SystemTxPhase::initial_for_block(BLOCK_2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert!(matches!(
        phase1,
        SystemTxPhase::Phase1Preexecuted {
            body_index: 0,
            receipt_index: 0,
            ..
        }
    ));
    // Advance past Phase 1: LateFinalizeCredits at body_index=1, then
    // CycleTick at body_index=2.
    let late = phase1.advance_after_commit(false, false);
    assert_eq!(late, SystemTxPhase::LateFinalizeCredits { body_index: 1 });
    let cycle = late.advance_after_commit(false, false);
    assert_eq!(cycle, SystemTxPhase::CycleTick { body_index: 2 });
    // CycleTick body_index is exactly 2 - after Phase 1 (0) and LateFinalize (1).
    assert_eq!(cycle.body_index(), Some(2));
}

/// Block `n>=2` body-index map: Phase 1 -> LateFinalizeCredits -> CycleTick ->
/// (optional BoundaryOutcome) -> OracleSlashWindow ->
/// HookEvents -> UserTxs. Reorder must preserve the ordering relative to the
/// body receipt slots.
#[test]
fn phase1_reordering_preserves_body_receipt_order() {
    let kinds_no_boundary = expected_begin_block_selectors(BLOCK_42, false, false);
    assert_eq!(
        kinds_no_boundary,
        vec![
            CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
            LATE_FINALIZE_CREDITS_SELECTOR,
            CYCLE_TICK_SELECTOR,
            ORACLE_SLASH_WINDOW_SELECTOR,
            HOOK_EVENTS_SELECTOR,
        ]
    );

    let kinds_with_boundary = expected_begin_block_selectors(BLOCK_42, true, false);
    assert_eq!(
        kinds_with_boundary,
        vec![
            CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
            LATE_FINALIZE_CREDITS_SELECTOR,
            CYCLE_TICK_SELECTOR,
            BOUNDARY_OUTCOME_SELECTOR,
            ORACLE_SLASH_WINDOW_SELECTOR,
            HOOK_EVENTS_SELECTOR,
        ]
    );

    // Drive the cursor through the canonical block-42 with-boundary sequence
    // and assert body_index increases monotonically by exactly 1 per advance.
    let phase1 = SystemTxPhase::initial_for_block(BLOCK_42, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    let late = phase1.advance_after_commit(true, false);
    let cycle = late.advance_after_commit(true, false);
    let boundary = cycle.advance_after_commit(true, false);
    let oracle = boundary.advance_after_commit(true, false);
    let hook_events = oracle.advance_after_commit(true, false);
    let after_hook_events = hook_events.advance_after_commit(true, false);
    assert_eq!(phase1.body_index(), Some(0));
    assert_eq!(late.body_index(), Some(1));
    assert_eq!(cycle.body_index(), Some(2));
    assert_eq!(boundary.body_index(), Some(3));
    assert_eq!(oracle.body_index(), Some(4));
    assert_eq!(hook_events.body_index(), Some(5));
    assert_eq!(after_hook_events, SystemTxPhase::UserTxs);
}

/// Cursor invariant: Phase 1 is consumed at most once per block. After
/// advancing past Phase 1, further advances never return a Phase 1 variant.
#[test]
fn phase1_executes_exactly_once() {
    let mut cursor = SystemTxPhase::initial_for_block(BLOCK_2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    let mut saw_phase1 = 0;
    if matches!(cursor, SystemTxPhase::Phase1Preexecuted { .. }) {
        saw_phase1 += 1;
    }
    for _ in 0..10 {
        cursor = cursor.advance_after_commit(true, false);
        if matches!(cursor, SystemTxPhase::Phase1Preexecuted { .. }) {
            saw_phase1 += 1;
        }
    }
    assert_eq!(saw_phase1, 1, "Phase 1 must be encountered exactly once");
    assert_eq!(cursor, SystemTxPhase::UserTxs);
}
