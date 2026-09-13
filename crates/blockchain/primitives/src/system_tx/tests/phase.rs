use super::*;

#[test]
fn advance_after_commit_interleaves_optional_phase3b() {
    // CycleTick -> RewardsGemDelivery -> BoundaryOutcome -> TeeBootstrap -> OracleSlashWindow.
    let cycle = SystemTxPhase::CycleTick { body_index: 1 };
    let rewards = cycle.advance_after_commit(true, true);
    assert_eq!(rewards, SystemTxPhase::RewardsGemDelivery { body_index: 2 });
    let bo = rewards.advance_after_commit(true, true);
    assert_eq!(bo, SystemTxPhase::BoundaryOutcomeOptional { body_index: 3 });
    let tee = bo.advance_after_commit(true, true);
    assert_eq!(tee, SystemTxPhase::TeeBootstrapOptional { body_index: 4 });
    let oracle = tee.advance_after_commit(true, true);
    assert_eq!(oracle, SystemTxPhase::OracleSlashWindow { body_index: 5 });
    let hook_events = oracle.advance_after_commit(true, true);
    assert_eq!(hook_events, SystemTxPhase::HookEvents { body_index: 6 });
    assert_eq!(
        hook_events.advance_after_commit(true, true),
        SystemTxPhase::UserTxs
    );

    // No boundary, bootstrap present: CycleTick -> RewardsGemDelivery -> TeeBootstrap.
    assert_eq!(
        SystemTxPhase::CycleTick { body_index: 1 }.advance_after_commit(false, true),
        SystemTxPhase::RewardsGemDelivery { body_index: 2 }
    );
    assert_eq!(
        SystemTxPhase::RewardsGemDelivery { body_index: 2 }.advance_after_commit(false, true),
        SystemTxPhase::TeeBootstrapOptional { body_index: 3 }
    );

    // Neither: CycleTick -> RewardsGemDelivery -> OracleSlashWindow.
    assert_eq!(
        SystemTxPhase::CycleTick { body_index: 1 }.advance_after_commit(false, false),
        SystemTxPhase::RewardsGemDelivery { body_index: 2 }
    );
    assert_eq!(
        SystemTxPhase::RewardsGemDelivery { body_index: 2 }.advance_after_commit(false, false),
        SystemTxPhase::OracleSlashWindow { body_index: 3 }
    );
    assert_eq!(
        SystemTxPhase::OracleSlashWindow { body_index: 3 }.advance_after_commit(false, false),
        SystemTxPhase::HookEvents { body_index: 4 }
    );
}

// ---------- : SystemTxPhase cursor tests ----------

#[test]
fn initial_for_block_block_1_is_cycletick() {
    // block 1 (genesis bootstrap) skips Phase 1 and starts at CycleTick.
    let cursor = SystemTxPhase::initial_for_block(1, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert_eq!(cursor, SystemTxPhase::CycleTick { body_index: 0 });
    assert_eq!(cursor.expected_kind(), Some(SystemTxKind::CycleTick));
    assert_eq!(cursor.body_index(), Some(0));
}

#[test]
fn initial_for_block_block_2_is_phase1_preexecuted() {
    // Block 2 (first post-bootstrap block) starts at Phase1Preexecuted with
    // a zero placeholder tx_hash that the executor overwrites after the
    // Phase 1 preflight commits.
    let cursor = SystemTxPhase::initial_for_block(2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert!(matches!(
        cursor,
        SystemTxPhase::Phase1Preexecuted {
            body_index: 0,
            receipt_index: 0,
            ..
        }
    ));
    if let SystemTxPhase::Phase1Preexecuted { tx_hash, .. } = cursor {
        assert_eq!(tx_hash, B256::ZERO);
    }
    assert_eq!(
        cursor.expected_kind(),
        Some(SystemTxKind::CertifiedParentAccounting)
    );
}

#[test]
fn initial_for_block_block_0_is_cycletick_placeholder() {
    // Block 0 is genesis; it has no begin-zone txs at all, but the cursor
    // initialisation must not panic and must not pick the Phase 1 branch.
    let cursor = SystemTxPhase::initial_for_block(0, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert_eq!(cursor, SystemTxPhase::CycleTick { body_index: 0 });
}

#[test]
fn expected_kind_returns_phase_for_each_variant() {
    let cases = [
        (
            SystemTxPhase::Phase1Preexecuted {
                body_index: 0,
                tx_hash: B256::ZERO,
                receipt_index: 0,
            },
            Some(SystemTxKind::CertifiedParentAccounting),
        ),
        (
            SystemTxPhase::CycleTick { body_index: 1 },
            Some(SystemTxKind::CycleTick),
        ),
        (
            SystemTxPhase::RewardsGemDelivery { body_index: 2 },
            Some(SystemTxKind::RewardsGemDelivery),
        ),
        (
            SystemTxPhase::BoundaryOutcomeOptional { body_index: 3 },
            Some(SystemTxKind::BoundaryOutcome),
        ),
        (
            SystemTxPhase::TeeBootstrapOptional { body_index: 4 },
            Some(SystemTxKind::TeeBootstrap),
        ),
        (
            SystemTxPhase::OracleSlashWindow { body_index: 5 },
            Some(SystemTxKind::OracleSlashWindow),
        ),
        (
            SystemTxPhase::HookEvents { body_index: 6 },
            Some(SystemTxKind::HookEvents),
        ),
        (SystemTxPhase::UserTxs, None),
    ];
    for (phase, expected) in cases {
        assert_eq!(phase.expected_kind(), expected, "phase={phase:?}");
    }
}

#[test]
fn body_index_matches_for_every_begin_zone_variant() {
    for (phase, expected) in [
        (
            SystemTxPhase::Phase1Preexecuted {
                body_index: 0,
                tx_hash: B256::ZERO,
                receipt_index: 0,
            },
            Some(0),
        ),
        (SystemTxPhase::CycleTick { body_index: 1 }, Some(1)),
        (SystemTxPhase::RewardsGemDelivery { body_index: 2 }, Some(2)),
        (
            SystemTxPhase::BoundaryOutcomeOptional { body_index: 3 },
            Some(3),
        ),
        (
            SystemTxPhase::TeeBootstrapOptional { body_index: 4 },
            Some(4),
        ),
        (SystemTxPhase::OracleSlashWindow { body_index: 5 }, Some(5)),
        (SystemTxPhase::HookEvents { body_index: 6 }, Some(6)),
        (SystemTxPhase::UserTxs, None),
    ] {
        assert_eq!(phase.body_index(), expected, "phase={phase:?}");
    }
}
