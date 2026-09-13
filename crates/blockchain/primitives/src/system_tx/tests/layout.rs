use super::*;

fn system_tx(kind: SystemTxKind, ordinal: u8, block_number: u64) -> TransactionSigned {
    let input = input_for(kind).encode().expect("system input encodes");
    build_unsigned_system_tx(kind, ordinal, block_number, CHAIN_ID, input)
        .expect("system tx builds")
        .into_signed(Signature::test_signature())
        .into()
}

fn user_tx() -> TransactionSigned {
    TxLegacy {
        chain_id: Some(CHAIN_ID),
        nonce: 0,
        gas_price: 0,
        gas_limit: 21_000,
        to: TxKind::Call(address!("0x4444444444444444444444444444444444444444")),
        value: U256::ZERO,
        input: Bytes::new(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

#[test]
fn split_accepts_empty_and_user_only_layouts() {
    let empty = split_system_layout(&[]).expect("empty splits");
    assert!(empty.is_empty());

    let txs = vec![user_tx(), user_tx()];
    let layout = split_system_layout(&txs).expect("user-only splits");
    assert_eq!(layout.begin.len(), 0);
    assert_eq!(layout.user.len(), 2);
    assert_eq!(layout.end.len(), 0);
}

#[test]
fn split_accepts_block1_cycle_tick_prefix() {
    let txs = vec![
        system_tx(SystemTxKind::CycleTick, 0, 1),
        system_tx(SystemTxKind::RewardsGemDelivery, 1, 1),
        user_tx(),
    ];
    let layout = split_system_layout(&txs).expect("layout splits");
    assert_eq!(
        layout.begin_block_kinds().expect("kinds"),
        vec![SystemTxKind::CycleTick, SystemTxKind::RewardsGemDelivery,]
    );
    assert_eq!(layout.user.len(), 1);
    assert!(layout.end.is_empty());
}

#[test]
fn split_accepts_block_with_optional_boundary_prefix() {
    let txs = vec![
        system_tx(SystemTxKind::CertifiedParentAccounting, 0, 42),
        system_tx(SystemTxKind::CycleTick, 1, 42),
        system_tx(SystemTxKind::RewardsGemDelivery, 2, 42),
        system_tx(SystemTxKind::BoundaryOutcome, 3, 42),
        user_tx(),
    ];
    let layout = split_system_layout(&txs).expect("layout splits");
    assert_eq!(
        layout.begin_block_kinds().expect("kinds"),
        vec![
            SystemTxKind::CertifiedParentAccounting,
            SystemTxKind::CycleTick,
            SystemTxKind::RewardsGemDelivery,
            SystemTxKind::BoundaryOutcome,
        ]
    );
    assert_eq!(layout.user.len(), 1);
}

#[test]
fn split_rejects_out_of_order_prefix() {
    let txs = vec![
        system_tx(SystemTxKind::CycleTick, 0, 42),
        system_tx(SystemTxKind::CertifiedParentAccounting, 1, 42),
    ];
    assert!(matches!(
        split_system_layout(&txs),
        Err(SystemTxError::OutOfOrder { .. })
    ));
}

#[test]
fn split_rejects_reserved_tx_in_wrong_suffix_zone() {
    let txs = vec![user_tx(), system_tx(SystemTxKind::CycleTick, 0, 42)];
    assert!(matches!(
        split_system_layout(&txs),
        Err(SystemTxError::SystemTxInWrongZone {
            actual: BodyZone::EndBlock,
            ..
        })
    ));
}

#[test]
fn split_rejects_reserved_tx_in_middle_zone() {
    let txs = vec![
        system_tx(SystemTxKind::CycleTick, 0, 1),
        system_tx(SystemTxKind::RewardsGemDelivery, 1, 1),
        user_tx(),
        system_tx(SystemTxKind::BoundaryOutcome, 2, 1),
        user_tx(),
    ];
    assert!(matches!(
        split_system_layout(&txs),
        Err(SystemTxError::MidBlockSystemTx { index: 3 })
    ));
}

#[test]
fn validate_active_system_tx_set_accepts_expected_membership() {
    let block0 = split_system_layout(&[]).expect("layout");
    validate_active_system_tx_set(&block0, 0, false, false).expect("genesis ok");

    // / V2: block 1 mandatorily carries a BoundaryOutcome for
    // the genesis bootstrap.
    let block1_txs = vec![
        system_tx(SystemTxKind::CycleTick, 0, 1),
        system_tx(SystemTxKind::RewardsGemDelivery, 1, 1),
        system_tx(SystemTxKind::BoundaryOutcome, 2, 1),
        system_tx(SystemTxKind::TeeBootstrap, 3, 1),
        system_tx(SystemTxKind::OracleSlashWindow, 4, 1),
        system_tx(SystemTxKind::HookEvents, 5, 1),
    ];
    let block1 = split_system_layout(&block1_txs).expect("layout");
    validate_active_system_tx_set(&block1, 1, true, true).expect("block 1 V2 ok");

    let block2_txs = vec![
        system_tx(SystemTxKind::CertifiedParentAccounting, 0, 2),
        system_tx(SystemTxKind::LateFinalizeCredits, 1, 2),
        system_tx(SystemTxKind::CycleTick, 2, 2),
        system_tx(SystemTxKind::RewardsGemDelivery, 3, 2),
        system_tx(SystemTxKind::OracleSlashWindow, 4, 2),
        system_tx(SystemTxKind::HookEvents, 5, 2),
    ];
    let block2 = split_system_layout(&block2_txs).expect("layout");
    validate_active_system_tx_set(&block2, 2, false, false).expect("block 2 ok");

    let block2_with_boundary_txs = vec![
        system_tx(SystemTxKind::CertifiedParentAccounting, 0, 2),
        system_tx(SystemTxKind::LateFinalizeCredits, 1, 2),
        system_tx(SystemTxKind::CycleTick, 2, 2),
        system_tx(SystemTxKind::RewardsGemDelivery, 3, 2),
        system_tx(SystemTxKind::BoundaryOutcome, 4, 2),
        system_tx(SystemTxKind::OracleSlashWindow, 5, 2),
        system_tx(SystemTxKind::HookEvents, 6, 2),
    ];
    let block2_with_boundary = split_system_layout(&block2_with_boundary_txs).expect("layout");
    validate_active_system_tx_set(&block2_with_boundary, 2, true, false)
        .expect("block 2 boundary ok");
}

#[test]
fn validate_active_system_tx_set_requires_mandatory_and_conditional_kinds() {
    let missing_finalization_txs = vec![
        system_tx(SystemTxKind::CycleTick, 0, 2),
        system_tx(SystemTxKind::RewardsGemDelivery, 1, 2),
        system_tx(SystemTxKind::OracleSlashWindow, 2, 2),
    ];
    let missing_finalization = split_system_layout(&missing_finalization_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&missing_finalization, 2, false, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));

    let missing_cycle_tick_txs = vec![
        system_tx(SystemTxKind::CertifiedParentAccounting, 0, 2),
        system_tx(SystemTxKind::RewardsGemDelivery, 1, 2),
        system_tx(SystemTxKind::OracleSlashWindow, 2, 2),
    ];
    let missing_cycle_tick = split_system_layout(&missing_cycle_tick_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&missing_cycle_tick, 2, false, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));

    // / V2: block 1 must include CycleTick, BoundaryOutcome and TeeBootstrap.
    // Missing CycleTick (with the other mandatory phases present)
    // still yields ActiveSystemTxSetMismatch.
    let block1_missing_cycle_tick_txs = vec![
        system_tx(SystemTxKind::RewardsGemDelivery, 0, 1),
        system_tx(SystemTxKind::BoundaryOutcome, 1, 1),
        system_tx(SystemTxKind::TeeBootstrap, 2, 1),
        system_tx(SystemTxKind::OracleSlashWindow, 3, 1),
        system_tx(SystemTxKind::HookEvents, 4, 1),
    ];
    let block1_missing_cycle_tick =
        split_system_layout(&block1_missing_cycle_tick_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&block1_missing_cycle_tick, 1, true, true),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));

    let block1_missing_tee_txs = vec![
        system_tx(SystemTxKind::CycleTick, 0, 1),
        system_tx(SystemTxKind::RewardsGemDelivery, 1, 1),
        system_tx(SystemTxKind::BoundaryOutcome, 2, 1),
        system_tx(SystemTxKind::OracleSlashWindow, 3, 1),
        system_tx(SystemTxKind::HookEvents, 4, 1),
    ];
    let block1_missing_tee = split_system_layout(&block1_missing_tee_txs).expect("layout");
    assert!(
        validate_active_system_tx_set(&block1_missing_tee, 1, true, false).is_err(),
        "block 1 must not commit without TeeBootstrap"
    );

    let block1_with_user_txs = vec![
        system_tx(SystemTxKind::CycleTick, 0, 1),
        system_tx(SystemTxKind::RewardsGemDelivery, 1, 1),
        system_tx(SystemTxKind::BoundaryOutcome, 2, 1),
        system_tx(SystemTxKind::TeeBootstrap, 3, 1),
        system_tx(SystemTxKind::OracleSlashWindow, 4, 1),
        system_tx(SystemTxKind::HookEvents, 5, 1),
        user_tx(),
    ];
    let block1_with_user = split_system_layout(&block1_with_user_txs).expect("layout");
    assert!(
        validate_active_system_tx_set(&block1_with_user, 1, true, true).is_err(),
        "block 1 must contain exactly the six mandatory system transactions"
    );

    // / V2: block 1 without BoundaryOutcome is rejected with
    // the V2-specific genesis bootstrap error before structural checks.
    let block1_no_boundary_txs = vec![
        system_tx(SystemTxKind::CycleTick, 0, 1),
        system_tx(SystemTxKind::RewardsGemDelivery, 1, 1),
        system_tx(SystemTxKind::OracleSlashWindow, 2, 1),
        system_tx(SystemTxKind::HookEvents, 3, 1),
    ];
    let block1_no_boundary = split_system_layout(&block1_no_boundary_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&block1_no_boundary, 1, false, false),
        Err(SystemTxError::V2Block1MissingBoundaryOutcome)
    ));

    let missing_oracle_slash_window_txs = vec![
        system_tx(SystemTxKind::CertifiedParentAccounting, 0, 2),
        system_tx(SystemTxKind::CycleTick, 1, 2),
        system_tx(SystemTxKind::RewardsGemDelivery, 2, 2),
        system_tx(SystemTxKind::HookEvents, 3, 2),
    ];
    let missing_oracle_slash_window =
        split_system_layout(&missing_oracle_slash_window_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&missing_oracle_slash_window, 2, false, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));

    let missing_boundary_txs = vec![
        system_tx(SystemTxKind::CertifiedParentAccounting, 0, 2),
        system_tx(SystemTxKind::CycleTick, 1, 2),
        system_tx(SystemTxKind::RewardsGemDelivery, 2, 2),
        system_tx(SystemTxKind::OracleSlashWindow, 3, 2),
        system_tx(SystemTxKind::HookEvents, 4, 2),
    ];
    let missing_boundary = split_system_layout(&missing_boundary_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&missing_boundary, 2, true, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));

    let unexpected_boundary_txs = vec![
        system_tx(SystemTxKind::CertifiedParentAccounting, 0, 2),
        system_tx(SystemTxKind::CycleTick, 1, 2),
        system_tx(SystemTxKind::RewardsGemDelivery, 2, 2),
        system_tx(SystemTxKind::BoundaryOutcome, 3, 2),
        system_tx(SystemTxKind::OracleSlashWindow, 4, 2),
        system_tx(SystemTxKind::HookEvents, 5, 2),
    ];
    let unexpected_boundary = split_system_layout(&unexpected_boundary_txs).expect("layout");
    assert!(matches!(
        validate_active_system_tx_set(&unexpected_boundary, 2, false, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));
}

#[test]
fn revert_fails_block_classifies_critical_begin_zone_phases() {
    // consensus- and economic-critical phases fail the block on
    // a revert/halt; non-critical phases keep the soft-receipt skip. Pin the
    // full classification so a new phase is forced to make this choice.
    for kind in [
        SystemTxKind::CertifiedParentAccounting,
        SystemTxKind::LateFinalizeCredits,
        SystemTxKind::CycleTick,
        SystemTxKind::BoundaryOutcome,
        SystemTxKind::TeeBootstrap,
    ] {
        assert!(
            kind.revert_fails_block(),
            "{kind:?} must fail the block on revert"
        );
    }
    for kind in [
        SystemTxKind::RewardsGemDelivery,
        SystemTxKind::OracleSlashWindow,
        SystemTxKind::HookEvents,
    ] {
        assert!(
            !kind.revert_fails_block(),
            "{kind:?} must keep the soft-receipt skip"
        );
    }
}

#[test]
fn validate_active_system_tx_set_rejects_phase3b_outside_block1() {
    // TeeBootstrap is the mandatory block-1 Phase 3b and cannot be replayed
    // at a later height even when the body-derived flag says it is present.
    let txs = vec![
        system_tx(SystemTxKind::CertifiedParentAccounting, 0, 2),
        system_tx(SystemTxKind::LateFinalizeCredits, 1, 2),
        system_tx(SystemTxKind::CycleTick, 2, 2),
        system_tx(SystemTxKind::RewardsGemDelivery, 3, 2),
        system_tx(SystemTxKind::BoundaryOutcome, 4, 2),
        system_tx(SystemTxKind::TeeBootstrap, 5, 2),
        system_tx(SystemTxKind::OracleSlashWindow, 6, 2),
        system_tx(SystemTxKind::HookEvents, 7, 2),
    ];
    let layout = split_system_layout(&txs).expect("layout");
    assert!(validate_active_system_tx_set(&layout, 2, true, true).is_err());

    // Same bytes, but the flag says no bootstrap expected -> mismatch.
    assert!(matches!(
        validate_active_system_tx_set(&layout, 2, true, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch { .. })
    ));

    // A later bootstrap without a boundary outcome is equally invalid.
    let txs_no_bo = vec![
        system_tx(SystemTxKind::CertifiedParentAccounting, 0, 2),
        system_tx(SystemTxKind::LateFinalizeCredits, 1, 2),
        system_tx(SystemTxKind::CycleTick, 2, 2),
        system_tx(SystemTxKind::RewardsGemDelivery, 3, 2),
        system_tx(SystemTxKind::TeeBootstrap, 4, 2),
        system_tx(SystemTxKind::OracleSlashWindow, 5, 2),
        system_tx(SystemTxKind::HookEvents, 6, 2),
    ];
    let layout_no_bo = split_system_layout(&txs_no_bo).expect("layout");
    assert!(validate_active_system_tx_set(&layout_no_bo, 2, false, true).is_err());
}

#[test]
fn reserved_address_does_not_collide_with_system_precompiles() {
    let addr_bytes = OUTBE_SYSTEM_TX_ADDRESS.0;
    assert_eq!(addr_bytes[0], 0xff);
    assert_ne!(addr_bytes[19], 0x00);
}
