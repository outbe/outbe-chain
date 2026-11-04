use alloy_consensus::{SignableTransaction as _, Transaction as _, TxLegacy};
use alloy_primitives::{address, Address, Bytes, Signature, TxKind, B256, U256};
use outbe_primitives::system_tx::{
    build_unsigned_system_tx, expected_end_block_selectors, split_system_layout,
    system_tx_visible_gas_limit, validate_active_system_tx_set, SysTxSelector, SystemTx,
    CALL_EVENT_PROCESSING_ORDINAL, CALL_EVENT_PROCESSING_SELECTOR, CLEANUP_PROCESSING_ORDINAL,
    CLEANUP_PROCESSING_SELECTOR, MAX_SYSTEM_TXS_PER_BLOCK, SYSTEM_TX_ARTIFACT_GAS_LIMIT,
    SYSTEM_TX_VISIBLE_GAS_FLOOR,
};
use outbe_primitives::{
    consensus::{DkgBoundaryArtifact, OUTBE_MAX_EXTRA_DATA_SIZE},
    consensus_metadata::{CertifiedParentAccountingMetadata, ParentParticipationProof},
};
use reth_ethereum::TransactionSigned;

const CHAIN_ID: u64 = 2026;
const BLOCK_NUMBER: u64 = 42;
const USER_BLOCK_GAS_LIMIT: u64 = 30_000_000;

fn numbered_address(n: u64) -> Address {
    let mut bytes = [0u8; 20];
    bytes[12..].copy_from_slice(&n.to_be_bytes());
    Address::from(bytes)
}

fn sample_metadata() -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        finalized_block_number: BLOCK_NUMBER - 1,
        finalized_block_hash: B256::repeat_byte(0x41),
        finalized_epoch: 7,
        finalized_view: 42,
        parent_view: 41,
        ordered_committee: vec![address!("0x1111111111111111111111111111111111111111")],
        signer_bitmap: vec![1],
        proof: Bytes::from_static(b"cert"),
        committee_set_hash: B256::repeat_byte(0x77),
        vrf_material_version: 3,
        vrf_group_public_key_hash: B256::repeat_byte(0x88),
        proof_kind: ParentParticipationProof::Finalization,
        // V2 contract requires `missed_proposers` to be empty.
        missed_proposers: Vec::new(),
    }
}

fn large_metadata() -> CertifiedParentAccountingMetadata {
    const COMMITTEE_SIZE: usize = 128;
    let ordered_committee = (0..COMMITTEE_SIZE)
        .map(|idx| numbered_address(10_000 + idx as u64))
        .collect::<Vec<_>>();
    CertifiedParentAccountingMetadata {
        ordered_committee,
        signer_bitmap: vec![1; COMMITTEE_SIZE],
        proof: Bytes::from(vec![0xAB; OUTBE_MAX_EXTRA_DATA_SIZE - 1_024]),
        ..sample_metadata()
    }
}

fn sample_boundary() -> DkgBoundaryArtifact {
    outbe_primitives::test_utils::sample_system_tx_boundary(BLOCK_NUMBER)
}

fn large_boundary() -> DkgBoundaryArtifact {
    const ACTIVE_SET_SIZE: usize = 128;
    let mut artifact = sample_boundary();
    artifact.reshare.new_active_set = (0..ACTIVE_SET_SIZE)
        .map(|idx| numbered_address(20_000 + idx as u64))
        .collect();
    artifact.outcome = Bytes::from(vec![0xCD; OUTBE_MAX_EXTRA_DATA_SIZE - 4_000]);
    artifact.vrf_group_public_key_bytes = Bytes::from(vec![0x44; 96]);
    artifact
}

fn accounting() -> SystemTx {
    SystemTx::CertifiedParentAccounting {
        metadata: sample_metadata(),
    }
}

fn late_credits() -> SystemTx {
    SystemTx::LateFinalizeCredits {
        artifact: Default::default(),
    }
}

fn boundary() -> SystemTx {
    SystemTx::BoundaryOutcome {
        artifact: sample_boundary(),
    }
}

fn canonical_inputs() -> Vec<SystemTx> {
    vec![
        accounting(),
        late_credits(),
        SystemTx::CycleTick,
        boundary(),
        SystemTx::OracleSlashWindow,
        SystemTx::HookEvents,
    ]
}

fn system_tx(input: SystemTx, ordinal: u8) -> TransactionSigned {
    let input = input.encode().expect("system input encodes");
    build_unsigned_system_tx(ordinal, BLOCK_NUMBER, CHAIN_ID, input)
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
fn full_begin_system_prefix_validates_before_user_transactions() {
    let txs = vec![
        system_tx(accounting(), 0),
        system_tx(late_credits(), 1),
        system_tx(SystemTx::CycleTick, 2),
        system_tx(boundary(), 3),
        system_tx(SystemTx::OracleSlashWindow, 4),
        system_tx(SystemTx::HookEvents, 5),
        user_tx(),
        system_tx(SystemTx::CallEventProcessing, CALL_EVENT_PROCESSING_ORDINAL),
        system_tx(SystemTx::CleanupProcessing, CLEANUP_PROCESSING_ORDINAL),
    ];

    let layout = split_system_layout(&txs).expect("layout splits");
    validate_active_system_tx_set(&layout, BLOCK_NUMBER, true, false)
        .expect("full system tx set validates");

    assert_eq!(
        layout
            .begin_block_selectors()
            .expect("begin_block kinds decode"),
        canonical_inputs()
            .iter()
            .map(SystemTx::selector)
            .collect::<Vec<_>>()
    );
    assert_eq!(layout.user.len(), 1);
    assert_eq!(
        layout.end_block_selectors().expect("end selectors"),
        vec![
            CALL_EVENT_PROCESSING_SELECTOR,
            CLEANUP_PROCESSING_SELECTOR
        ]
    );
}

#[test]
fn missing_or_extra_receipt_visible_system_txs_are_rejected() {
    use outbe_primitives::system_tx::SystemTxError;

    let canonical_with_boundary = canonical_inputs();
    let missing_oracle_inputs: Vec<SystemTx> = canonical_inputs()
        .into_iter()
        .filter(|input| !matches!(input, SystemTx::OracleSlashWindow))
        .collect();
    let missing_oracle: Vec<TransactionSigned> = missing_oracle_inputs
        .iter()
        .cloned()
        .enumerate()
        .map(|(ordinal, input)| system_tx(input, ordinal as u8))
        .collect();
    let layout = split_system_layout(&missing_oracle).expect("layout splits");
    assert_eq!(
        validate_active_system_tx_set(&layout, BLOCK_NUMBER, true, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch {
            expected_begin: canonical_with_boundary
                .iter()
                .map(SystemTx::selector)
                .collect(),
            expected_end: expected_end_block_selectors(BLOCK_NUMBER),
            actual_begin: missing_oracle_inputs
                .iter()
                .map(SystemTx::selector)
                .collect(),
            actual_end: Vec::new(),
        })
    );

    let canonical_without_boundary: Vec<SysTxSelector> = canonical_with_boundary
        .iter()
        .filter(|input| !matches!(input, SystemTx::BoundaryOutcome { .. }))
        .map(SystemTx::selector)
        .collect();
    let unexpected_boundary: Vec<TransactionSigned> = canonical_with_boundary
        .iter()
        .cloned()
        .enumerate()
        .map(|(ordinal, input)| system_tx(input, ordinal as u8))
        .collect();
    let layout = split_system_layout(&unexpected_boundary).expect("layout splits");
    assert_eq!(
        validate_active_system_tx_set(&layout, BLOCK_NUMBER, false, false),
        Err(SystemTxError::ActiveSystemTxSetMismatch {
            expected_begin: canonical_without_boundary,
            expected_end: expected_end_block_selectors(BLOCK_NUMBER),
            actual_begin: canonical_with_boundary
                .iter()
                .map(SystemTx::selector)
                .collect(),
            actual_end: Vec::new(),
        })
    );
}

#[test]
fn system_tx_wire_limits_leave_runtime_gas_headroom() {
    assert_eq!(MAX_SYSTEM_TXS_PER_BLOCK, 16);
    assert_eq!(SYSTEM_TX_ARTIFACT_GAS_LIMIT, 10_000_000_000);

    for (ordinal, input) in canonical_inputs().into_iter().enumerate() {
        let tx = system_tx(input, ordinal.try_into().expect("ordinal fits"));
        assert_eq!(
            tx.gas_limit(),
            system_tx_visible_gas_limit(tx.input()).expect("visible system gas computes")
        );
        assert!(tx.gas_limit() >= SYSTEM_TX_VISIBLE_GAS_FLOOR);
        assert!(tx.gas_limit() < USER_BLOCK_GAS_LIMIT);
        assert!(tx.gas_limit() < SYSTEM_TX_ARTIFACT_GAS_LIMIT);
        assert_eq!(
            tx.nonce(),
            BLOCK_NUMBER * u64::from(MAX_SYSTEM_TXS_PER_BLOCK) + ordinal as u64
        );
    }
}

#[test]
fn gas_12_system_prefix_aggregate_visible_gas_must_fit_block_cap() {
    let txs = vec![
        system_tx(
            SystemTx::CertifiedParentAccounting {
                metadata: large_metadata(),
            },
            0,
        ),
        // mandatory phase. The realistic per-block late-credit count is
        // <= K (a few in-window targets), so an empty/small artifact here keeps
        // the worst case dominated by CPA + BoundaryOutcome.
        system_tx(
            SystemTx::LateFinalizeCredits {
                artifact: Default::default(),
            },
            1,
        ),
        system_tx(SystemTx::CycleTick, 2),
        system_tx(
            SystemTx::BoundaryOutcome {
                artifact: large_boundary(),
            },
            3,
        ),
        system_tx(SystemTx::OracleSlashWindow, 4),
        system_tx(SystemTx::HookEvents, 5),
    ];
    let mut layout_txs = txs.clone();
    layout_txs.extend([
        system_tx(SystemTx::CallEventProcessing, CALL_EVENT_PROCESSING_ORDINAL),
        system_tx(SystemTx::CleanupProcessing, CLEANUP_PROCESSING_ORDINAL),
    ]);
    let layout = split_system_layout(&layout_txs).expect("worst-case system prefix layout splits");
    validate_active_system_tx_set(&layout, BLOCK_NUMBER, true, false)
        .expect("worst-case active system tx set validates");
    assert_eq!(txs.len(), 6);
    assert!(txs.len() <= usize::from(MAX_SYSTEM_TXS_PER_BLOCK));

    let aggregate_envelope_gas: u64 = txs.iter().map(|tx| tx.gas_limit()).sum();
    for tx in &txs {
        assert_eq!(
            tx.gas_limit(),
            system_tx_visible_gas_limit(tx.input()).expect("visible system gas computes"),
            "GAS-12: each worst-case system tx must carry deterministic visible gas"
        );
        assert!(tx.gas_limit() >= SYSTEM_TX_VISIBLE_GAS_FLOOR);
        assert!(tx.gas_limit() < SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    }

    assert!(
        aggregate_envelope_gas <= USER_BLOCK_GAS_LIMIT,
        "GAS-12: worst-case visible begin-zone system envelope gas {aggregate_envelope_gas} \
         exceeds the block gas limit {USER_BLOCK_GAS_LIMIT}"
    );
}

#[test]
fn gas_15_visible_system_tx_gas_must_not_exceed_visible_block_gas_limit_for_generic_replay() {
    let cycle = system_tx(SystemTx::CycleTick, 1);

    assert!(
        cycle.gas_limit() <= USER_BLOCK_GAS_LIMIT,
        "GAS-15: visible system tx gas {} exceeds visible block gas limit {USER_BLOCK_GAS_LIMIT}; \
         stock Ethereum replay/import will not treat this as a normal user tx",
        cycle.gas_limit()
    );
    assert_eq!(
        cycle.gas_limit(),
        system_tx_visible_gas_limit(cycle.input()).expect("visible gas computes")
    );
    assert!(cycle.gas_limit() >= SYSTEM_TX_VISIBLE_GAS_FLOOR);
    assert!(cycle.gas_limit() < SYSTEM_TX_ARTIFACT_GAS_LIMIT);
}
