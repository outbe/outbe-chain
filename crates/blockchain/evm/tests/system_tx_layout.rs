//! V3 stage-framed system transaction layout invariants.

#[path = "system_tx_helpers/end_block_inputs.rs"]
mod end_block_inputs;

use end_block_inputs::end_block_inputs;

use alloy_consensus::{SignableTransaction as _, TxLegacy};
use alloy_primitives::{address, Bytes, Signature, TxKind, U256};
use outbe_evm::system_tx::{BlockOrder, TEST_BLOCK_ORDER, *};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use reth_ethereum::TransactionSigned;

/// Empty layout - used by the block-1 / block-0 layout tests that only need to
/// drive the membership check, not the structural splitter.
fn empty_layout() -> SystemTxLayout<'static> {
    SystemTxLayout {
        begin: Vec::new(),
        user: Vec::new(),
        end: Vec::new(),
    }
}

const TEST_BLOCK: u64 = 32;
const TEST_CHAIN_ID: u64 = 2026;

fn system_inputs() -> Vec<SystemTx> {
    vec![
        SystemTx::Consensus {
            step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
            payload: CertifiedParentAccountingMetadata::default()
                .encode()
                .unwrap(),
        },
        SystemTx::Consensus {
            step_index: CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX,
            payload: outbe_primitives::reshare_artifact::encode_late_finalize_credits_artifact(
                &Default::default(),
            )
            .unwrap(),
        },
        SystemTx::Consensus {
            step_index: CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
            payload: Bytes::new(),
        },
        SystemTx::BeginBlock { step_index: 0 },
        SystemTx::BeginBlock { step_index: 1 },
        SystemTx::BeginBlock { step_index: 2 },
        SystemTx::BeginBlock { step_index: 3 },
        SystemTx::BeginBlock { step_index: 4 },
        SystemTx::BeginBlock { step_index: 5 },
        SystemTx::BeginBlock { step_index: 6 },
        SystemTx::BeginBlock { step_index: 7 },
        SystemTx::BeginBlock { step_index: 8 },
    ]
}

fn signed_system_tx(input: SystemTx, ordinal: u8) -> TransactionSigned {
    build_unsigned_system_tx(
        ordinal,
        TEST_BLOCK,
        TEST_CHAIN_ID,
        input.encode().expect("system input encodes"),
    )
    .expect("system tx builds")
    .into_signed(Signature::test_signature())
    .into()
}

fn user_tx() -> TransactionSigned {
    TxLegacy {
        chain_id: Some(TEST_CHAIN_ID),
        nonce: 0,
        gas_price: 0,
        gas_limit: 21_000,
        to: TxKind::Call(address!("4444444444444444444444444444444444444444")),
        value: U256::ZERO,
        input: Bytes::new(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

#[test]
fn legacy_selectors_and_versions_are_rejected() {
    for selector in [
        *b"OSF1", *b"OSC1", *b"OSB1", *b"OSO1", *b"OSA3", *b"OSL2", *b"OSB2", *b"OST3", *b"OSC2",
    ] {
        let mut bytes = selector.to_vec();
        bytes.extend([2, 0]);
        assert_eq!(
            SystemTx::decode(&bytes),
            Err(SystemTxError::UnknownSelector(selector))
        );
    }
    for version in [1, 2] {
        let mut bytes = BEGIN_BLOCK_STAGE_SELECTOR.to_vec();
        bytes.extend([version, 8]);
        assert_eq!(
            SystemTx::decode(&bytes),
            Err(SystemTxError::UnsupportedVersion(version))
        );
    }
}

#[test]
fn block_1_layout_requires_boundary_outcome_and_tee_bootstrap() {
    assert_eq!(
        validate_system_tx_set_for_publication(
            &TEST_BLOCK_ORDER,
            &empty_layout(),
            1,
            BlockSystemFlags::default()
        ),
        Err(SystemTxError::V2Block1MissingBoundaryOutcome)
    );
    let flags = BlockSystemFlags {
        has_boundary_outcome: true,
        has_publication: false,
    };
    assert_eq!(
        validate_system_tx_set_for_publication(&TEST_BLOCK_ORDER, &empty_layout(), 1, flags),
        Err(SystemTxError::V2Block1MissingTeeBootstrap)
    );
    let plan = BlockSystemPlan::for_block(1, flags, &TEST_BLOCK_ORDER);
    let mut expected = vec![
        SystemTxStepId::consensus(CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX),
        SystemTxStepId::consensus(CONSENSUS_TEE_BOOTSTRAP_STEP_INDEX),
        SystemTxStepId::consensus(CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX),
    ];
    expected.extend((0..9).map(SystemTxStepId::begin_block));
    assert_eq!(plan.expected_prefix_steps(), expected);
    assert!(plan.expected_end_block_steps().is_empty());
}

#[test]
fn empty_block_order_fails_closed_at_block_1() {
    let flags = BlockSystemFlags {
        has_boundary_outcome: true,
        has_publication: false,
    };
    let layout = empty_layout();
    assert_eq!(
        validate_system_tx_set_for_publication(&BlockOrder::default(), &layout, 1, flags),
        Err(SystemTxError::EmptyBlockOrder { block_number: 1 })
    );
}

#[test]
fn block_0_has_no_system_transactions() {
    for has_boundary_outcome in [false, true] {
        let flags = BlockSystemFlags {
            has_boundary_outcome,
            has_publication: false,
        };
        validate_system_tx_set_for_publication(&TEST_BLOCK_ORDER, &empty_layout(), 0, flags)
            .unwrap();
        let plan = BlockSystemPlan::for_block(0, flags, &TEST_BLOCK_ORDER);
        assert!(plan.expected_prefix_steps().is_empty());
        assert!(plan.expected_end_block_steps().is_empty());
    }
}

fn canonical_transactions() -> Vec<TransactionSigned> {
    system_inputs()
        .into_iter()
        .enumerate()
        .map(|(index, kind)| signed_system_tx(kind, index as u8))
        .chain(std::iter::once(user_tx()))
        .chain(
            end_block_inputs()
                .into_iter()
                .map(|(input, ordinal)| signed_system_tx(input, ordinal)),
        )
        .collect()
}

#[test]
fn native_layout_separates_begin_prefix_and_user_transactions() {
    let txs = canonical_transactions();
    let layout = split_system_layout(&txs).unwrap();
    validate_system_tx_set_for_publication(
        &TEST_BLOCK_ORDER,
        &layout,
        TEST_BLOCK,
        BlockSystemFlags::default(),
    )
    .unwrap();
    assert_eq!(
        layout.prefix_steps().unwrap(),
        BlockSystemPlan::for_block(TEST_BLOCK, BlockSystemFlags::default(), &TEST_BLOCK_ORDER)
            .expected_prefix_steps()
    );
    assert_eq!(layout.user.len(), 1);
    assert_eq!(layout.end.len(), 5);
    assert_eq!(
        layout.end_block_steps().unwrap(),
        BlockSystemPlan::for_block(TEST_BLOCK, BlockSystemFlags::default(), &TEST_BLOCK_ORDER)
            .expected_end_block_steps()
    );
}

#[test]
fn native_layout_accepts_an_empty_user_zone() {
    let mut txs = canonical_transactions();
    let user_index = txs
        .iter()
        .position(|tx| !is_reserved_system_tx(tx))
        .expect("user tx");
    txs.remove(user_index);
    let layout = split_system_layout(&txs).unwrap();
    validate_system_tx_set_for_publication(
        &TEST_BLOCK_ORDER,
        &layout,
        TEST_BLOCK,
        BlockSystemFlags::default(),
    )
    .unwrap();
    assert!(layout.user.is_empty());
    assert_eq!(layout.end.len(), 5);
}

/// Filtering retains static step indices; prefix nonces still use body positions.
#[test]
fn stage_ordering_is_canonical() {
    for has_boundary_outcome in [false, true] {
        let flags = BlockSystemFlags {
            has_boundary_outcome,
            has_publication: false,
        };
        let mut expected = vec![
            SystemTxStepId::consensus(CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX),
            SystemTxStepId::consensus(CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX),
        ];
        if has_boundary_outcome {
            expected.push(SystemTxStepId::consensus(
                CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
            ));
        }
        expected.push(SystemTxStepId::consensus(
            CONSENSUS_VALIDATOR_LIVENESS_STEP_INDEX,
        ));
        expected.extend((0..9).map(SystemTxStepId::begin_block));
        let plan = BlockSystemPlan::for_block(42, flags, &TEST_BLOCK_ORDER);
        assert_eq!(plan.expected_prefix_steps(), expected);
        assert_eq!(
            plan.expected_end_block_steps(),
            (0..5).map(SystemTxStepId::end_block).collect::<Vec<_>>()
        );
    }
    assert_eq!(
        end_block_inputs().map(|(_, ordinal)| ordinal),
        [24, 25, 26, 27, 28]
    );
    for input in system_inputs() {
        assert_eq!(input.body_zone(), BodyZone::BeginBlock);
        assert!(input.nonce_ordinal().is_none());
    }
}
