use alloy_consensus::{SignableTransaction as _, Transaction as _, TxLegacy};
use alloy_primitives::{address, Address, Bytes, Signature, TxKind, B256, U256};
use outbe_primitives::system_tx::{
    build_unsigned_system_tx, expected_end_block_selectors, split_system_layout,
    system_tx_visible_gas_limit, validate_active_system_tx_set, SysTxSelector, SystemTx,
    CREDIS_FACTORY_ORDINAL, CREDIS_FACTORY_SELECTOR, GEM_CLEANUP_ORDINAL, GEM_CLEANUP_SELECTOR,
    GEM_FACTORY_ORDINAL, GEM_FACTORY_SELECTOR, INTEX_FACTORY_ORDINAL, INTEX_FACTORY_SELECTOR,
    MAX_SYSTEM_TXS_PER_BLOCK, NOD_FACTORY_ORDINAL, NOD_FACTORY_SELECTOR,
    SYSTEM_TX_ARTIFACT_GAS_LIMIT,
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
        boundary(),
        SystemTx::ValidatorLiveness,
        SystemTx::Vote,
        SystemTx::Update,
        SystemTx::EnclaveUpgradeDeadline,
        SystemTx::Rewards,
        SystemTx::Staking,
        SystemTx::Oracle,
        SystemTx::OracleSlash,
        SystemTx::HyperlaneSlash,
        SystemTx::CycleTick,
    ]
}

fn end_block_inputs() -> [(SystemTx, u8); 5] {
    [
        (SystemTx::NodFactory, NOD_FACTORY_ORDINAL),
        (SystemTx::GemFactory, GEM_FACTORY_ORDINAL),
        (SystemTx::IntexFactory, INTEX_FACTORY_ORDINAL),
        (SystemTx::CredisFactory, CREDIS_FACTORY_ORDINAL),
        (SystemTx::GemCleanup, GEM_CLEANUP_ORDINAL),
    ]
}

fn end_block_txs() -> Vec<TransactionSigned> {
    end_block_inputs()
        .into_iter()
        .map(|(input, ordinal)| system_tx(input, ordinal))
        .collect()
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
    let mut txs: Vec<TransactionSigned> = canonical_inputs()
        .into_iter()
        .enumerate()
        .map(|(ordinal, input)| system_tx(input, u8::try_from(ordinal).unwrap()))
        .collect();
    txs.push(user_tx());
    txs.extend(end_block_txs());

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
        end_block_inputs().map(|(_, ordinal)| ordinal),
        [24, 25, 26, 27, 28]
    );
    assert_eq!(NOD_FACTORY_ORDINAL, 24);
    assert_eq!(GEM_CLEANUP_ORDINAL, 28);
    assert_eq!(
        layout.end_block_selectors().expect("end selectors"),
        vec![
            NOD_FACTORY_SELECTOR,
            GEM_FACTORY_SELECTOR,
            INTEX_FACTORY_SELECTOR,
            CREDIS_FACTORY_SELECTOR,
            GEM_CLEANUP_SELECTOR,
        ]
    );
    assert_eq!(
        layout.end_block_selectors().unwrap(),
        expected_end_block_selectors(BLOCK_NUMBER)
    );
}

#[test]
fn missing_or_extra_receipt_visible_system_txs_are_rejected() {
    use outbe_primitives::system_tx::SystemTxError;

    let canonical_with_boundary = canonical_inputs();
    let missing_oracle_inputs: Vec<SystemTx> = canonical_inputs()
        .into_iter()
        .filter(|input| !matches!(input, SystemTx::OracleSlash))
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

fn assert_zero_gas(tx: &TransactionSigned, ordinal: u8) {
    assert_eq!(tx.gas_limit(), 0);
    assert_eq!(system_tx_visible_gas_limit(tx.input()).unwrap(), 0);
    assert_eq!(
        tx.nonce(),
        BLOCK_NUMBER * u64::from(MAX_SYSTEM_TXS_PER_BLOCK) + u64::from(ordinal)
    );
}

#[test]
fn system_tx_wire_limits_leave_runtime_gas_headroom() {
    assert_eq!(MAX_SYSTEM_TXS_PER_BLOCK, 32);
    assert_eq!(SYSTEM_TX_ARTIFACT_GAS_LIMIT, 10_000_000_000);

    for (ordinal, input) in canonical_inputs().into_iter().enumerate() {
        let ordinal = u8::try_from(ordinal).unwrap();
        assert_zero_gas(&system_tx(input, ordinal), ordinal);
    }
    for (input, ordinal) in end_block_inputs() {
        assert_zero_gas(&system_tx(input, ordinal), ordinal);
    }
}

#[test]
fn gas_12_system_prefix_aggregate_visible_gas_must_fit_block_cap() {
    let inputs = vec![
        SystemTx::CertifiedParentAccounting {
            metadata: large_metadata(),
        },
        SystemTx::LateFinalizeCredits {
            artifact: Default::default(),
        },
        SystemTx::BoundaryOutcome {
            artifact: large_boundary(),
        },
        SystemTx::ValidatorLiveness,
        SystemTx::Vote,
        SystemTx::Update,
        SystemTx::EnclaveUpgradeDeadline,
        SystemTx::Rewards,
        SystemTx::Staking,
        SystemTx::Oracle,
        SystemTx::OracleSlash,
        SystemTx::HyperlaneSlash,
        SystemTx::CycleTick,
    ];
    let txs: Vec<TransactionSigned> = inputs
        .into_iter()
        .enumerate()
        .map(|(ordinal, input)| system_tx(input, u8::try_from(ordinal).unwrap()))
        .collect();
    let mut layout_txs = txs.clone();
    layout_txs.extend(end_block_txs());
    let layout = split_system_layout(&layout_txs).expect("worst-case system prefix layout splits");
    validate_active_system_tx_set(&layout, BLOCK_NUMBER, true, false)
        .expect("worst-case active system tx set validates");
    assert_eq!(txs.len(), canonical_inputs().len());
    assert!(txs.len() <= usize::from(MAX_SYSTEM_TXS_PER_BLOCK));

    let aggregate_envelope_gas: u64 = txs.iter().map(|tx| tx.gas_limit()).sum();
    for tx in &txs {
        assert_eq!(tx.gas_limit(), 0);
        assert_eq!(system_tx_visible_gas_limit(tx.input()).unwrap(), 0);
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

    assert_eq!(cycle.gas_limit(), 0);
    assert_eq!(system_tx_visible_gas_limit(cycle.input()).unwrap(), 0);
    assert!(cycle.gas_limit() <= USER_BLOCK_GAS_LIMIT);
}
