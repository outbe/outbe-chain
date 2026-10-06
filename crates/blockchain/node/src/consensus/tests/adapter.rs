use super::*;
use alloy_consensus::{constants::EMPTY_ROOT_HASH, TxLegacy};
use alloy_primitives::{Bytes, TxKind};
use reth_chainspec::ChainSpecBuilder;

fn empty_header(number: u64) -> OutbeHeader {
    let mut value = header(number, 100, 0, B256::ZERO).header().clone();
    value.inner.transactions_root = EMPTY_ROOT_HASH;
    value.inner.receipts_root = EMPTY_ROOT_HASH;
    value
}

fn empty_block(header: OutbeHeader) -> SealedBlock<OutbeBlock> {
    OutbeBlock {
        header,
        body: OutbeBlockBody::default(),
    }
    .seal_slow()
}

fn empty_recovered(header: OutbeHeader) -> RecoveredBlock<OutbeBlock> {
    RecoveredBlock::new_unhashed(
        OutbeBlock {
            header,
            body: OutbeBlockBody::default(),
        },
        Vec::new(),
    )
}

fn empty_execution() -> BlockExecutionResult<OutbeReceipt> {
    BlockExecutionResult {
        receipts: Vec::new(),
        requests: Default::default(),
        gas_used: 0,
        blob_gas_used: 0,
    }
}

fn check_empty_execution(
    consensus: &OutbeBeaconConsensus<ChainSpec<OutbeHeader>>,
    header: &OutbeHeader,
    roots: Option<ReceiptRootBloom>,
    access_list_hash: Option<B256>,
) -> Result<(), ConsensusError> {
    consensus.validate_block_post_execution(
        &empty_recovered(header.clone()),
        &empty_execution(),
        roots,
        access_list_hash,
    )
}

fn chain_from(activate: fn(ChainSpecBuilder) -> ChainSpecBuilder) -> Arc<ChainSpec<OutbeHeader>> {
    let builder = ChainSpecBuilder::default()
        .chain(1.into())
        .genesis(Default::default());
    activate(builder)
        .build()
        .map_header(OutbeHeader::new)
        .into()
}

fn lifecycle_block(active: bool) -> eyre::Result<SealedBlock<OutbeBlock>> {
    use outbe_evm::system_tx::SystemTxInputV2;
    let signer = outbe_evm::OutbeEvmSigner::from_secret_bytes([7u8; 32])?;
    let mut inputs = vec![
        SystemTxInputV2::CertifiedParentAccounting {
            metadata: phase1_metadata(1, B256::ZERO),
        },
        SystemTxInputV2::LateFinalizeCredits {
            artifact: Default::default(),
        },
    ];
    if active {
        inputs.push(SystemTxInputV2::OcompLifecycleBegin);
    }
    inputs.extend([
        SystemTxInputV2::CycleTick,
        SystemTxInputV2::RewardsGemDelivery,
        SystemTxInputV2::OracleSlashWindow,
        SystemTxInputV2::HookEvents,
    ]);
    if active {
        inputs.push(SystemTxInputV2::OcompTerminalRequest);
    }
    let transactions: Vec<_> = inputs
        .into_iter()
        .enumerate()
        .map(|(ordinal, input)| signed_system_tx(&signer, ordinal as u8, 2, input))
        .collect();
    let mut header = empty_header(2);
    header.inner.transactions_root =
        reth_primitives_traits::proofs::calculate_transaction_root(&transactions);
    Ok(OutbeBlock {
        header,
        body: OutbeBlockBody {
            transactions,
            ..Default::default()
        },
    }
    .seal_slow())
}

#[test]
fn gas_schedule_skip_is_reversible_and_does_not_skip_timestamp_rules() {
    let strict = OutbeBeaconConsensus::new(test_chain_spec());
    let permissive = strict.clone().with_skip_gas_limit_ramp_check(true);
    let parent = header(0, 100, 0, B256::ZERO);
    let child = header_with_beneficiary_and_gas_limit(
        1,
        (101, 0),
        parent.hash(),
        HeaderOptions {
            beneficiary: REWARDS_ADDRESS,
            gas_limit: 30_000_000,
        },
    );
    let error = strict
        .validate_header_against_parent(&child, &parent)
        .unwrap_err();
    assert!(error.to_string().contains("protocol gas limit mismatch"));
    permissive
        .validate_header_against_parent(&child, &parent)
        .unwrap();
    let restored = permissive.with_skip_gas_limit_ramp_check(false);
    assert_eq!(
        restored
            .validate_header_against_parent(&child, &parent)
            .unwrap_err()
            .to_string(),
        error.to_string()
    );
    let frozen = header(1, 100, 0, parent.hash());
    assert!(matches!(
        strict
            .with_skip_gas_limit_ramp_check(true)
            .validate_header_against_parent(&frozen, &parent),
        Err(ConsensusError::TimestampIsInPast { .. })
    ));
}

#[test]
fn header_millis_error_precedes_gas_and_stock_errors() {
    let consensus = OutbeBeaconConsensus::new(test_chain_spec());
    let mut invalid = header(1, 100, 1000, B256::ZERO).header().clone();
    invalid.inner.gas_limit = 1;
    invalid.inner.gas_used = 2;
    let error = consensus
        .validate_header(&SealedHeader::seal_slow(invalid))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "timestamp_millis_part 1000 must be less than 1000"
    );
}

#[test]
fn header_protocol_gas_error_precedes_stock_checks() {
    let consensus = OutbeBeaconConsensus::new(test_chain_spec());
    let mut invalid = empty_header(1);
    invalid.inner.gas_limit = 1;
    invalid.inner.gas_used = 2;
    let error = consensus
        .validate_header(&SealedHeader::seal_slow(invalid))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "block 1 protocol gas limit mismatch: expected 500000000, got 1"
    );
}

#[test]
fn parent_hash_error_precedes_timestamp_and_gas_errors() {
    let consensus = OutbeBeaconConsensus::new(test_chain_spec());
    let parent = header(0, 100, 0, B256::ZERO);
    let child = header(1, 100, 0, B256::repeat_byte(0x21));
    assert!(matches!(
        consensus.validate_header_against_parent(&child, &parent),
        Err(ConsensusError::ParentHashMismatch(_))
    ));
}

#[test]
fn parent_timestamp_error_precedes_protocol_gas_error() {
    let consensus = OutbeBeaconConsensus::new(test_chain_spec());
    let parent = header(0, 100, 0, B256::ZERO);
    let mut child = header(1, 100, 0, parent.hash()).header().clone();
    child.inner.gas_limit = 1;
    assert!(matches!(
        consensus.validate_header_against_parent(&SealedHeader::seal_slow(child), &parent),
        Err(ConsensusError::TimestampIsInPast {
            parent_timestamp: 100_000,
            timestamp: 100_000
        })
    ));
}

#[test]
fn parent_base_fee_error_precedes_blob_checks() {
    let chain = chain_from(ChainSpecBuilder::cancun_activated);
    let consensus = OutbeBeaconConsensus::new(chain);
    let parent = header(0, 100, 0, B256::ZERO);
    let child = header(1, 101, 0, parent.hash());
    assert!(matches!(
        consensus.validate_header_against_parent(&child, &parent),
        Err(ConsensusError::BaseFeeMissing)
    ));
}

#[test]
fn withdrawals_error_precedes_beneficiary_layout_and_root_errors() {
    let consensus = OutbeBeaconConsensus::new(test_chain_spec());
    let invalid = header_with_beneficiary(1, (100, 0), B256::ZERO, Address::ZERO);
    let block = OutbeBlock {
        header: invalid.header().clone(),
        body: body_with_withdrawal(1),
    }
    .seal_slow();
    let expected = "non-empty EIP-4895 withdrawals are unsupported on Outbe";
    let body_error = consensus
        .validate_body_against_header(block.body(), &invalid)
        .unwrap_err();
    assert!(body_error.to_string().contains(expected));
    assert_eq!(
        consensus
            .validate_block_pre_execution(&block)
            .unwrap_err()
            .to_string(),
        body_error.to_string()
    );
    assert_eq!(
        consensus
            .validate_block_pre_execution_with_tx_root(&block, Some(B256::ZERO))
            .unwrap_err()
            .to_string(),
        body_error.to_string()
    );
}

#[test]
fn transport_size_error_precedes_system_layout_for_both_preexecution_paths() {
    let signer = outbe_evm::OutbeEvmSigner::from_secret_bytes([6u8; 32]).unwrap();
    let tx = signer
        .sign_unsigned(TxLegacy {
            chain_id: Some(1),
            gas_limit: 21_000,
            to: TxKind::Call(Address::ZERO),
            input: Bytes::from(vec![0; OUTBE_MAX_BLOCK_SIZE]),
            ..Default::default()
        })
        .unwrap();
    let block = OutbeBlock {
        header: empty_header(1),
        body: OutbeBlockBody {
            transactions: vec![tx],
            ..Default::default()
        },
    }
    .seal_slow();
    let consensus = OutbeBeaconConsensus::new(test_chain_spec());
    let error = consensus.validate_block_pre_execution(&block).unwrap_err();
    assert!(error.to_string().contains("P2P transport cap"));
    assert_eq!(
        consensus
            .validate_block_pre_execution_with_tx_root(&block, None)
            .unwrap_err()
            .to_string(),
        error.to_string()
    );
}

#[test]
fn beneficiary_error_precedes_stock_root_errors() {
    let consensus = OutbeBeaconConsensus::new(test_chain_spec());
    let mut invalid = empty_header(1);
    invalid.inner.beneficiary = Address::ZERO;
    invalid.inner.transactions_root = B256::ZERO;
    let block = empty_block(invalid);
    let error = consensus.validate_block_pre_execution(&block).unwrap_err();
    assert!(error
        .to_string()
        .contains("beneficiary must be REWARDS_ADDRESS"));
    let sealed = SealedHeader::seal_slow(block.header().clone());
    assert_eq!(
        consensus
            .validate_body_against_header(block.body(), &sealed)
            .unwrap_err()
            .to_string(),
        error.to_string()
    );
    assert_eq!(
        consensus
            .validate_block_pre_execution_with_tx_root(&block, Some(EMPTY_ROOT_HASH))
            .unwrap_err()
            .to_string(),
        error.to_string()
    );
}

#[test]
fn stock_transaction_root_check_and_supplied_root_are_preserved() {
    let consensus = OutbeBeaconConsensus::new(test_chain_spec());
    let block = empty_block(empty_header(0));
    let sealed = SealedHeader::seal_slow(block.header().clone());
    consensus
        .validate_body_against_header(block.body(), &sealed)
        .unwrap();
    consensus.validate_block_pre_execution(&block).unwrap();
    consensus
        .validate_block_pre_execution_with_tx_root(&block, Some(EMPTY_ROOT_HASH))
        .unwrap();
    assert!(matches!(
        consensus.validate_block_pre_execution_with_tx_root(&block, Some(B256::ZERO)),
        Err(ConsensusError::BodyTransactionRootDiff(_))
    ));
    let mut wrong_root = block.header().clone();
    wrong_root.inner.transactions_root = B256::ZERO;
    let wrong_block = empty_block(wrong_root);
    assert!(matches!(
        consensus.validate_block_pre_execution(&wrong_block),
        Err(ConsensusError::BodyTransactionRootDiff(_))
    ));
}

#[test]
fn max_extra_data_setting_is_preserved_by_clone_and_reconfiguration() {
    let spec = test_chain_spec();
    let consensus =
        OutbeBeaconConsensus::new(spec.clone()).with_max_extra_data_size(OUTBE_MAX_EXTRA_DATA_SIZE);
    assert!(Arc::ptr_eq(consensus.chain_spec(), &spec));
    let cloned = consensus.clone();
    assert!(Arc::ptr_eq(cloned.chain_spec(), &spec));
    assert_eq!(cloned.max_extra_data_size(), OUTBE_MAX_EXTRA_DATA_SIZE);
    let sealed = header(2, 100, 1, B256::ZERO);
    cloned.validate_header(&sealed).unwrap();
    let restricted = cloned.with_max_extra_data_size(0);
    assert!(matches!(
        restricted.validate_header(&sealed),
        Err(ConsensusError::ExtraDataExceedsMax { .. })
    ));
    consensus.validate_header(&sealed).unwrap();
}

#[test]
fn blob_gas_skip_is_reversible_and_does_not_skip_base_fee_checks() {
    let spec = chain_from(ChainSpecBuilder::cancun_activated);
    let consensus =
        OutbeBeaconConsensus::new(spec).with_max_extra_data_size(OUTBE_MAX_EXTRA_DATA_SIZE);
    let mut value = empty_header(0);
    value.inner.base_fee_per_gas = Some(7);
    value.inner.withdrawals_root = Some(EMPTY_ROOT_HASH);
    let sealed = SealedHeader::seal_slow(value.clone());
    assert!(matches!(
        consensus.validate_header(&sealed),
        Err(ConsensusError::BlobGasUsedMissing)
    ));
    let permissive = consensus.clone().with_skip_blob_gas_used_check(true);
    permissive.validate_header(&sealed).unwrap();
    assert!(matches!(
        permissive
            .clone()
            .with_skip_blob_gas_used_check(false)
            .validate_header(&sealed),
        Err(ConsensusError::BlobGasUsedMissing)
    ));
    value.inner.base_fee_per_gas = None;
    assert!(matches!(
        permissive.validate_header(&SealedHeader::seal_slow(value)),
        Err(ConsensusError::BaseFeeMissing)
    ));
}

#[test]
fn postexecution_checks_gas_before_receipts() {
    let spec = chain_from(ChainSpecBuilder::byzantium_activated);
    let consensus = OutbeBeaconConsensus::new(spec);
    let mut value = empty_header(0);
    value.inner.gas_used = 1;
    value.inner.receipts_root = B256::ZERO;
    assert!(matches!(
        check_empty_execution(&consensus, &value, None, None),
        Err(ConsensusError::BlockGasUsed { .. })
    ));
}

#[test]
fn postexecution_preserves_computed_and_supplied_receipts_and_bloom() {
    let spec = chain_from(ChainSpecBuilder::byzantium_activated);
    let consensus = OutbeBeaconConsensus::new(spec);
    let value = empty_header(0);
    check_empty_execution(&consensus, &value, None, None).unwrap();
    check_empty_execution(
        &consensus,
        &value,
        Some((EMPTY_ROOT_HASH, Bloom::ZERO)),
        None,
    )
    .unwrap();
    assert!(matches!(
        check_empty_execution(&consensus, &value, Some((B256::ZERO, Bloom::ZERO)), None),
        Err(ConsensusError::BodyReceiptRootDiff(_))
    ));
    assert!(matches!(
        check_empty_execution(
            &consensus,
            &value,
            Some((EMPTY_ROOT_HASH, Bloom::repeat_byte(1))),
            None
        ),
        Err(ConsensusError::BodyBloomLogDiff(_))
    ));
}

#[test]
fn requests_hash_skip_is_reversible_and_keeps_receipt_checks() {
    let spec = chain_from(ChainSpecBuilder::prague_activated);
    let consensus = OutbeBeaconConsensus::new(spec);
    let mut value = empty_header(0);
    value.inner.requests_hash = Some(B256::ZERO);
    assert!(matches!(
        check_empty_execution(&consensus, &value, None, None),
        Err(ConsensusError::BodyRequestsHashDiff(_))
    ));
    let permissive = consensus.with_skip_requests_hash_check(true);
    check_empty_execution(&permissive, &value, None, None).unwrap();
    let restored = permissive.clone().with_skip_requests_hash_check(false);
    assert!(matches!(
        check_empty_execution(&restored, &value, None, None),
        Err(ConsensusError::BodyRequestsHashDiff(_))
    ));
    value.inner.receipts_root = B256::ZERO;
    assert!(matches!(
        check_empty_execution(&permissive, &value, None, None),
        Err(ConsensusError::BodyReceiptRootDiff(_))
    ));
}

#[test]
fn postexecution_forwards_access_list_hash_without_skipping_validation() {
    let spec = chain_from(ChainSpecBuilder::amsterdam_activated);
    let consensus = OutbeBeaconConsensus::new(spec);
    let result = empty_execution();
    let mut value = empty_header(0);
    value.inner.requests_hash = Some(result.requests.requests_hash());
    value.inner.block_access_list_hash = Some(B256::repeat_byte(0xAB));
    assert!(matches!(
        check_empty_execution(&consensus, &value, None, None),
        Err(ConsensusError::BlockAccessListHashMissing)
    ));
    assert!(matches!(
        check_empty_execution(&consensus, &value, None, Some(B256::ZERO)),
        Err(ConsensusError::BlockAccessListHashMismatch(_))
    ));
    check_empty_execution(&consensus, &value, None, Some(B256::repeat_byte(0xAB))).unwrap();
}

#[test]
fn lifecycle_activation_is_preserved_by_configuration_and_clone() {
    let original = OutbeBeaconConsensus::new(test_chain_spec());
    let configured = original
        .clone()
        .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(2));
    let active = lifecycle_block(true).unwrap();
    let active_header = SealedHeader::seal_slow(active.header().clone());
    let cloned = configured.clone();
    cloned
        .validate_body_against_header(active.body(), &active_header)
        .unwrap();
    cloned.validate_block_pre_execution(&active).unwrap();
    cloned
        .validate_block_pre_execution_with_tx_root(
            &active,
            Some(active.header().transactions_root()),
        )
        .unwrap();
    assert!(original.validate_block_pre_execution(&active).is_err());
    let before_activation =
        configured.with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(3));
    assert!(before_activation
        .validate_block_pre_execution(&active)
        .is_err());
    before_activation
        .validate_block_pre_execution(&lifecycle_block(false).unwrap())
        .unwrap();
}

#[test]
fn active_lifecycle_rejects_old_layout_before_stock_root_errors() {
    let consensus = OutbeBeaconConsensus::new(test_chain_spec())
        .with_ocomp_lifecycle_activation(OcompLifecycleActivation::at_block(2));
    let mut block = lifecycle_block(false).unwrap();
    let mut value = block.header().clone();
    value.inner.transactions_root = B256::ZERO;
    block = OutbeBlock {
        header: value,
        body: block.body().clone(),
    }
    .seal_slow();
    let error = consensus.validate_block_pre_execution(&block).unwrap_err();
    assert!(matches!(error, ConsensusError::Other(_)));
    let sealed = SealedHeader::seal_slow(block.header().clone());
    assert_eq!(
        consensus
            .validate_body_against_header(block.body(), &sealed)
            .unwrap_err()
            .to_string(),
        error.to_string()
    );
    assert_eq!(
        consensus
            .validate_block_pre_execution_with_tx_root(&block, None)
            .unwrap_err()
            .to_string(),
        error.to_string()
    );
}
