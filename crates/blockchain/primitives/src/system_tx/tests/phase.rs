use super::*;

#[test]
fn block_execution_fsm_walks_genesis_bootstrap_and_steady_state() {
    // 1. Block 0 (Genesis): [*] -> Completed
    let mut fsm0 = BlockExecutionFsm::for_block(0, false, false, false);
    assert_eq!(fsm0.stage(), BlockExecutionStage::Completed);
    assert_eq!(fsm0.expected_system_tx_kind(), None);
    assert_eq!(fsm0.expected_ordinal(), None);
    assert_eq!(fsm0.finish(), Ok(()));
    assert_eq!(
        fsm0.on_user_tx(),
        Err(SystemTxError::UnexpectedUserTx {
            stage: BlockExecutionStage::Completed
        })
    );
    assert_eq!(
        fsm0.on_system_tx(SystemTxKind::CycleTick),
        Err(SystemTxError::UnexpectedSystemTx {
            stage: BlockExecutionStage::Completed,
            expected: None,
            actual: "CycleTick",
        })
    );

    // 2. Block 1 (GenesisBootstrap): ConsensusStage (OSB2 -> OST3 -> OSVL) -> BeginBlock (9 txs) -> Completed
    let mut fsm1 = BlockExecutionFsm::for_block(1, true, true, false);
    assert_eq!(fsm1.stage(), BlockExecutionStage::ConsensusStage);
    assert_eq!(
        fsm1.on_system_tx(SystemTxKind::BoundaryOutcome),
        Ok(BlockExecutionStage::ConsensusStage)
    );
    assert_eq!(
        fsm1.on_system_tx(SystemTxKind::TeeBootstrap),
        Ok(BlockExecutionStage::ConsensusStage)
    );
    assert_eq!(
        fsm1.on_system_tx(SystemTxKind::ValidatorLiveness),
        Ok(BlockExecutionStage::BeginBlock)
    );
    for (idx, kind) in BEGIN_BLOCK_ORDER.into_iter().enumerate() {
        let next_stage = fsm1.on_system_tx(kind).expect("begin block tx accepted");
        if idx + 1 < BEGIN_BLOCK_ORDER.len() {
            assert_eq!(next_stage, BlockExecutionStage::BeginBlock);
        } else {
            assert_eq!(next_stage, BlockExecutionStage::Completed);
        }
    }
    assert_eq!(fsm1.finish(), Ok(()));

    // 3. Block 2 (SteadyState): ConsensusStage -> BeginBlock -> MidBlock -> EndBlock -> Completed
    let mut fsm2 = BlockExecutionFsm::for_block(2, true, false, true);
    assert_eq!(fsm2.stage(), BlockExecutionStage::ConsensusStage);
    for kind in [
        SystemTxKind::CertifiedParentAccounting,
        SystemTxKind::LateFinalizeCredits,
        SystemTxKind::BoundaryOutcome,
        SystemTxKind::ValidatorLiveness,
        SystemTxKind::RadiclePublication,
    ] {
        fsm2.on_system_tx(kind).expect("consensus tx accepted");
    }
    assert_eq!(fsm2.stage(), BlockExecutionStage::BeginBlock);
    for kind in BEGIN_BLOCK_ORDER {
        fsm2.on_system_tx(kind).expect("begin tx accepted");
    }
    assert_eq!(fsm2.stage(), BlockExecutionStage::MidBlock);
    assert_eq!(fsm2.on_user_tx(), Ok(BlockExecutionStage::MidBlock));
    assert_eq!(fsm2.on_user_tx(), Ok(BlockExecutionStage::MidBlock));
    assert_eq!(fsm2.user_txs_executed(), 2);
    for (idx, kind) in END_BLOCK_ORDER.into_iter().enumerate() {
        assert_eq!(fsm2.expected_ordinal(), kind.nonce_ordinal());
        let next = fsm2.on_system_tx(kind).expect("end tx accepted");
        if idx + 1 < END_BLOCK_ORDER.len() {
            assert_eq!(next, BlockExecutionStage::EndBlock);
        } else {
            assert_eq!(next, BlockExecutionStage::Completed);
        }
    }
    assert_eq!(fsm2.finish(), Ok(()));
}

#[test]
fn block_execution_fsm_rejects_out_of_order_missing_and_duplicate_txs() {
    let mut fsm = BlockExecutionFsm::for_block(2, false, false, false);
    assert_eq!(
        fsm.on_system_tx(SystemTxKind::LateFinalizeCredits),
        Err(SystemTxError::UnexpectedSystemTx {
            stage: BlockExecutionStage::ConsensusStage,
            expected: Some("CertifiedParentAccounting"),
            actual: "LateFinalizeCredits",
        })
    );
    assert_eq!(
        fsm.finish(),
        Err(SystemTxError::IncompleteBlockExecution {
            stage: BlockExecutionStage::ConsensusStage,
            expected_next: Some("CertifiedParentAccounting"),
        })
    );
    assert_eq!(
        fsm.on_user_tx(),
        Err(SystemTxError::UnexpectedUserTx {
            stage: BlockExecutionStage::ConsensusStage,
        })
    );
    fsm.on_system_tx(SystemTxKind::CertifiedParentAccounting)
        .unwrap();
    // Duplicate CertifiedParentAccounting rejected:
    assert_eq!(
        fsm.on_system_tx(SystemTxKind::CertifiedParentAccounting),
        Err(SystemTxError::UnexpectedSystemTx {
            stage: BlockExecutionStage::ConsensusStage,
            expected: Some("LateFinalizeCredits"),
            actual: "CertifiedParentAccounting",
        })
    );
}

#[test]
fn advance_after_commit_interleaves_optional_consensus_and_begin_phases() {
    let late = SystemTxPhase::LateFinalizeCredits { body_index: 1 };
    let bo = late.advance_after_commit_with_publication(true, true, true);
    assert_eq!(bo, SystemTxPhase::BoundaryOutcomeOptional { body_index: 2 });
    let tee = bo.advance_after_commit_with_publication(true, true, true);
    assert_eq!(tee, SystemTxPhase::TeeBootstrapOptional { body_index: 3 });
    let liveness = tee.advance_after_commit_with_publication(true, true, true);
    assert_eq!(liveness, SystemTxPhase::ValidatorLiveness { body_index: 4 });
    let pub_phase = liveness.advance_after_commit_with_publication(true, true, true);
    assert_eq!(
        pub_phase,
        SystemTxPhase::RadiclePublication { body_index: 5 }
    );
    let vote = pub_phase.advance_after_commit_with_publication(true, true, true);
    assert_eq!(vote, SystemTxPhase::Vote { body_index: 6 });
    let update = vote.advance_after_commit_with_publication(true, true, true);
    assert_eq!(update, SystemTxPhase::Update { body_index: 7 });
    let eud = update.advance_after_commit_with_publication(true, true, true);
    assert_eq!(eud, SystemTxPhase::EnclaveUpgradeDeadline { body_index: 8 });
    let rewards = eud.advance_after_commit_with_publication(true, true, true);
    assert_eq!(rewards, SystemTxPhase::Rewards { body_index: 9 });
    let staking = rewards.advance_after_commit_with_publication(true, true, true);
    assert_eq!(staking, SystemTxPhase::Staking { body_index: 10 });
    let oracle = staking.advance_after_commit_with_publication(true, true, true);
    assert_eq!(oracle, SystemTxPhase::Oracle { body_index: 11 });
    let oracle_slash = oracle.advance_after_commit_with_publication(true, true, true);
    assert_eq!(oracle_slash, SystemTxPhase::OracleSlash { body_index: 12 });
    let hyperlane_slash = oracle_slash.advance_after_commit_with_publication(true, true, true);
    assert_eq!(
        hyperlane_slash,
        SystemTxPhase::HyperlaneSlash { body_index: 13 }
    );
    let cycle = hyperlane_slash.advance_after_commit_with_publication(true, true, true);
    assert_eq!(cycle, SystemTxPhase::CycleTick { body_index: 14 });
    assert_eq!(
        cycle.advance_after_commit_with_publication(true, true, true),
        SystemTxPhase::UserTxs
    );
}

#[test]
fn initial_for_block_matches_block_height_regimes() {
    let block0 = SystemTxPhase::initial_for_block(0, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert_eq!(block0, SystemTxPhase::UserTxs);
    assert_eq!(block0.expected_selector(), None);
    assert_eq!(block0.body_index(), None);

    let block1 = SystemTxPhase::initial_for_block(1, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert_eq!(
        block1,
        SystemTxPhase::BoundaryOutcomeOptional { body_index: 0 }
    );
    assert_eq!(block1.expected_selector(), Some(BOUNDARY_OUTCOME_SELECTOR));
    assert_eq!(block1.body_index(), Some(0));

    let block2 = SystemTxPhase::initial_for_block(2, GENESIS_BOOTSTRAP_BLOCK_NUMBER);
    assert!(matches!(
        block2,
        SystemTxPhase::Phase1Preexecuted {
            body_index: 0,
            receipt_index: 0,
            ..
        }
    ));
    assert_eq!(
        block2.expected_selector(),
        Some(CERTIFIED_PARENT_ACCOUNTING_SELECTOR)
    );
}

#[test]
fn expected_selector_and_body_index_match_every_phase_variant() {
    let cases = [
        (
            SystemTxPhase::Phase1Preexecuted {
                body_index: 0,
                tx_hash: B256::ZERO,
                receipt_index: 0,
            },
            Some(CERTIFIED_PARENT_ACCOUNTING_SELECTOR),
            Some(0),
        ),
        (
            SystemTxPhase::LateFinalizeCredits { body_index: 1 },
            Some(LATE_FINALIZE_CREDITS_SELECTOR),
            Some(1),
        ),
        (
            SystemTxPhase::BoundaryOutcomeOptional { body_index: 2 },
            Some(BOUNDARY_OUTCOME_SELECTOR),
            Some(2),
        ),
        (
            SystemTxPhase::TeeBootstrapOptional { body_index: 3 },
            Some(TEE_BOOTSTRAP_SELECTOR),
            Some(3),
        ),
        (
            SystemTxPhase::ValidatorLiveness { body_index: 4 },
            Some(VALIDATOR_LIVENESS_SELECTOR),
            Some(4),
        ),
        (
            SystemTxPhase::RadiclePublication { body_index: 5 },
            Some(RADICLE_PUBLICATION_SELECTOR),
            Some(5),
        ),
        (
            SystemTxPhase::Vote { body_index: 6 },
            Some(VOTE_SELECTOR),
            Some(6),
        ),
        (
            SystemTxPhase::Update { body_index: 7 },
            Some(UPDATE_SELECTOR),
            Some(7),
        ),
        (
            SystemTxPhase::EnclaveUpgradeDeadline { body_index: 8 },
            Some(ENCLAVE_UPGRADE_DEADLINE_SELECTOR),
            Some(8),
        ),
        (
            SystemTxPhase::Rewards { body_index: 9 },
            Some(REWARDS_SELECTOR),
            Some(9),
        ),
        (
            SystemTxPhase::Staking { body_index: 10 },
            Some(STAKING_SELECTOR),
            Some(10),
        ),
        (
            SystemTxPhase::Oracle { body_index: 11 },
            Some(ORACLE_SELECTOR),
            Some(11),
        ),
        (
            SystemTxPhase::OracleSlash { body_index: 12 },
            Some(ORACLE_SLASH_SELECTOR),
            Some(12),
        ),
        (
            SystemTxPhase::HyperlaneSlash { body_index: 13 },
            Some(HYPERLANE_SLASH_SELECTOR),
            Some(13),
        ),
        (
            SystemTxPhase::CycleTick { body_index: 14 },
            Some(CYCLE_TICK_SELECTOR),
            Some(14),
        ),
        (SystemTxPhase::UserTxs, None, None),
        (
            SystemTxPhase::EndBlockNodFactory,
            Some(NOD_FACTORY_SELECTOR),
            Some(NOD_FACTORY_ORDINAL),
        ),
        (
            SystemTxPhase::EndBlockGemFactory,
            Some(GEM_FACTORY_SELECTOR),
            Some(GEM_FACTORY_ORDINAL),
        ),
        (
            SystemTxPhase::EndBlockIntexFactory,
            Some(INTEX_FACTORY_SELECTOR),
            Some(INTEX_FACTORY_ORDINAL),
        ),
        (
            SystemTxPhase::EndBlockCredisFactory,
            Some(CREDIS_FACTORY_SELECTOR),
            Some(CREDIS_FACTORY_ORDINAL),
        ),
        (
            SystemTxPhase::EndBlockGemCleanup,
            Some(GEM_CLEANUP_SELECTOR),
            Some(GEM_CLEANUP_ORDINAL),
        ),
        (SystemTxPhase::EndBlock, None, None),
    ];
    for (phase, expected_sel, expected_idx) in cases {
        assert_eq!(phase.expected_selector(), expected_sel, "phase={phase:?}");
        assert_eq!(phase.body_index(), expected_idx, "phase={phase:?}");
        for input in inputs() {
            assert_eq!(
                phase.accepts(&input),
                expected_sel == Some(input.selector())
            );
        }
    }
}
