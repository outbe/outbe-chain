use std::sync::Arc;

use super::fixtures::{
    dummy_pubkey, test_committee_snapshot, test_evm_signer, CHAIN_ID, TEST_BLOCK_TIMESTAMP_BASE,
};
use super::*;

pub(super) fn execution_ctx<'a>(
    tx_count_hint: Option<usize>,
    extra_data: Bytes,
) -> OutbeBlockExecutionCtx<'a> {
    OutbeBlockExecutionCtx {
        inner: EthBlockExecutionCtx {
            parent_hash: B256::ZERO,
            parent_beacon_block_root: None,
            ommers: &[],
            withdrawals: None,
            extra_data,
            tx_count_hint,
            slot_number: None,
        },
        timestamp_millis_part: 0,
        block_hash: None,
        block_state_root: None,
        expected_begin_system_txs: Vec::new(),
        system_layout_error: None,
        parent_consensus_metadata: None,
        proposer_evm_address: None,
        execute_outbe_block_hooks: true,
        prebuilt_phase1_tx: None,
        parent_artifact_hint: None,
        pending_tee_bootstrap: None,
        execution_read_budget: None,
    }
}

pub fn block_one_execution_ctx<'a>(
    tx_count_hint: Option<usize>,
    extra_data: Bytes,
) -> OutbeBlockExecutionCtx<'a> {
    execution_ctx_with_tee_bootstrap(tx_count_hint, extra_data, sample_tee_bootstrap_payload(1))
}

pub fn block_two_execution_ctx<'a>(
    tx_count_hint: Option<usize>,
    extra_data: Bytes,
    proposer: Address,
    parent_hash: B256,
) -> (
    OutbeBlockExecutionCtx<'a>,
    CertifiedParentAccountingMetadata,
) {
    let mut metadata = metadata_with(vec![proposer], vec![1], Vec::new());
    metadata.finalized_block_number = 1;
    metadata.finalized_block_hash = parent_hash;
    let mut ctx = execution_ctx(tx_count_hint, extra_data);
    ctx.inner.parent_hash = parent_hash;
    ctx.parent_consensus_metadata = Some(metadata.clone());
    ctx.parent_artifact_hint = Some(AccountedParentArtifact {
        summary: ExecutionSummaryArtifact {
            validator_fee_sum: U256::ZERO,
        },
        timestamp: 0,
        state_root: Some(B256::repeat_byte(0x91)),
    });
    ctx.proposer_evm_address = Some(proposer);
    (ctx, metadata)
}

pub(super) fn execution_ctx_with_tee_bootstrap<'a>(
    tx_count_hint: Option<usize>,
    extra_data: Bytes,
    tee_bootstrap: outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2,
) -> OutbeBlockExecutionCtx<'a> {
    let mut ctx = execution_ctx(tx_count_hint, block_one_artifacts(extra_data));
    ctx.pending_tee_bootstrap = Some(tee_bootstrap);
    ctx
}

pub enum BootstrapFixture {
    StandardForBlock,
    Payload(Box<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>),
    Absent,
}

impl BootstrapFixture {
    pub(super) fn explicit(
        payload: Option<outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2>,
    ) -> Self {
        match payload {
            Some(payload) => Self::Payload(Box::new(payload)),
            None => Self::Absent,
        }
    }
}

pub struct BeginBlockFixture<'a> {
    pub block_number: u64,
    pub parent_hash: B256,
    pub extra_data: &'a Bytes,
    pub parent_consensus_metadata: Option<CertifiedParentAccountingMetadata>,
    pub proposer: Address,
    pub bootstrap: BootstrapFixture,
}

pub fn begin_system_txs_for_test(
    config: &OutbeEvmConfig,
    input: BeginBlockFixture<'_>,
) -> Vec<reth_primitives_traits::Recovered<reth_ethereum::TransactionSigned>> {
    let BeginBlockFixture {
        block_number,
        parent_hash,
        extra_data,
        parent_consensus_metadata,
        proposer,
        bootstrap,
    } = input;
    let pending_tee_bootstrap = match bootstrap {
        BootstrapFixture::StandardForBlock => {
            (block_number == 1).then(|| sample_tee_bootstrap_payload(block_number))
        }
        BootstrapFixture::Payload(payload) => Some(*payload),
        BootstrapFixture::Absent => None,
    };
    let extra_data = if block_number == 1 {
        block_one_artifacts(extra_data.clone())
    } else {
        extra_data.clone()
    };
    config
        .build_begin_system_txs(
            block_number,
            CHAIN_ID,
            outbe_primitives::system_tx::protocol_block_gas_limit(block_number),
            parent_hash,
            &extra_data,
            parent_consensus_metadata,
            Some(proposer),
            None,
            pending_tee_bootstrap,
        )
        .expect("begin-zone system txs should build")
}

pub(super) fn sample_tee_bootstrap_payload(
    block_number: u64,
) -> outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2 {
    sample_tee_bootstrap_payload_at(
        block_number,
        TEST_BLOCK_TIMESTAMP_BASE.saturating_add(block_number),
    )
}

pub(super) fn sample_tee_bootstrap_payload_at(
    block_number: u64,
    consensus_timestamp: u64,
) -> outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2 {
    use outbe_primitives::tee_test_utils::DevValidatorV1;

    let consensus_public = dummy_pubkey(0xA2);
    let snapshot = test_committee_snapshot(&[(test_evm_signer().address(), consensus_public)]);
    let committee_snapshot_hash = outbe_validatorset::committee_set_hash_v2(0, &snapshot);
    let requested_valid_until = consensus_timestamp
        .checked_add(3_600)
        .expect("test lease timestamp fits u64");
    sample_tee_bootstrap_payload_for(
        block_number,
        committee_snapshot_hash,
        requested_valid_until,
        &[DevValidatorV1 {
            evm_secret: [1; 32],
            bls_minpk_public: consensus_public,
        }],
    )
}

pub(super) fn sample_tee_bootstrap_payload_for(
    block_number: u64,
    committee_snapshot_hash: B256,
    requested_valid_until: u64,
    validators: &[outbe_primitives::tee_test_utils::DevValidatorV1],
) -> outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2 {
    use outbe_primitives::tee_test_utils::{gramine_direct_bootstrap_v2, gramine_direct_policy_v1};

    let policy = gramine_direct_policy_v1(CHAIN_ID, MAINNET.genesis_hash())
        .expect("test GramineDirectDev policy is canonical");
    gramine_direct_bootstrap_v2(
        policy,
        committee_snapshot_hash,
        block_number,
        requested_valid_until,
        validators,
    )
    .expect("test GramineDirectDev OST3 payload is canonical")
}

pub(super) fn test_regular_tx() -> reth_ethereum::TransactionSigned {
    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit: 21_000,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(Address::ZERO),
        value: U256::ZERO,
        input: Bytes::new(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

pub(super) fn test_oracle_submit_vote_tx() -> reth_ethereum::TransactionSigned {
    test_oracle_submit_vote_tx_with_gas_limit(1_000_000)
}

pub(super) fn test_oracle_submit_vote_tx_with_gas_limit(
    gas_limit: u64,
) -> reth_ethereum::TransactionSigned {
    let input = outbe_oracle::precompile::IOracle::submitVoteCall {
        tuples: vec![outbe_oracle::precompile::IOracle::ExchangeRateTuple {
            base: outbe_oracle::api::COEN_ASSET,
            quote: outbe_oracle::api::currency_address(840),
            exchangeRate: U256::from(1_000_000u64),
            volume: U256::from(10_000_000_000u64),
        }],
        gas: Vec::new(),
    }
    .abi_encode();

    TxEip1559 {
        chain_id: CHAIN_ID,
        nonce: 0,
        gas_limit,
        max_fee_per_gas: MIN_PROTOCOL_BASE_FEE as u128,
        max_priority_fee_per_gas: 0,
        to: TxKind::Call(ORACLE_ADDRESS),
        value: U256::ZERO,
        input: input.into(),
        access_list: Default::default(),
    }
    .into_signed(Signature::test_signature())
    .into()
}

pub(super) fn test_metadata() -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata::default()
}

pub(crate) fn metadata_with(
    committee: Vec<Address>,
    signer_bitmap: Vec<u8>,
    missed_proposers: Vec<Address>,
) -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        ordered_committee: committee,
        signer_bitmap,
        // convert V1-shape `Vec<Address>` test fixture into V2
        // `Vec<MissedProposerEvent>`. The view defaults to 0. The V2 contract
        // is an empty list. This fixture exercises the validation path only.
        missed_proposers: missed_proposers
            .into_iter()
            .map(
                |validator| outbe_primitives::consensus_metadata::MissedProposerEvent {
                    view: 0,
                    validator,
                },
            )
            .collect(),
        ..CertifiedParentAccountingMetadata::default()
    }
}

pub(crate) fn boundary_with(
    is_validator_set_change: bool,
    committee: Vec<(Address, [u8; 48])>,
) -> outbe_primitives::consensus::DkgBoundaryArtifact {
    boundary_with_epoch(0, is_validator_set_change, committee)
}

pub(crate) fn boundary_with_epoch(
    epoch: u64,
    is_validator_set_change: bool,
    committee: Vec<(Address, [u8; 48])>,
) -> outbe_primitives::consensus::DkgBoundaryArtifact {
    let new_active_set: Vec<Address> = committee.iter().map(|(address, _)| *address).collect();
    let vrf_group_public_key_bytes = vec![0x42u8; 96];
    let snapshot = outbe_validatorset::CommitteeSnapshot {
        committee: committee
            .into_iter()
            .map(
                |(address, consensus_pubkey)| outbe_validatorset::CommitteeEntry {
                    address,
                    consensus_pubkey,
                },
            )
            .collect(),
        vrf_material_version: 0,
        vrf_group_public_key_bytes: vrf_group_public_key_bytes.clone(),
        vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
    };
    let active_set_hash = super::hash_boundary_active_set(&new_active_set);
    let committee_set_hash = outbe_validatorset::committee_set_hash_v2(epoch, &snapshot);
    let vrf_group_public_key = keccak256(&vrf_group_public_key_bytes);
    outbe_primitives::consensus::DkgBoundaryArtifact {
        epoch,
        dkg_cycle: 0,
        freeze_height: 0,
        planned_activation_height: 0,
        target_set_hash: B256::ZERO,
        vrf_material_version: 0,
        vrf_group_public_key,
        vrf_group_public_key_bytes: Bytes::from(vrf_group_public_key_bytes),
        committee_set_hash,
        is_validator_set_change,
        outcome: Bytes::new(),
        is_full_dkg: false,
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: outbe_primitives::consensus::ReshareResult {
            new_active_set,
            active_set_hash,
        },
    }
}

pub(crate) fn empty_executor_inputs(parent_hash: B256) -> BlockExecutorInputs {
    BlockExecutorInputs {
        identity: BlockExecutionIdentity {
            block_extra_data: Bytes::new(),
            validate_execution_summary: false,
            block_hash: None,
            block_state_root: None,
            parent_hash,
        },
        system_plan: BlockSystemPlan {
            expected_begin_system_txs: Vec::new(),
            system_layout_error: None,
            proposer_evm_address: None,
            execute_outbe_block_hooks: true,
            prebuilt_phase1_tx: None,
            pending_tee_bootstrap: None,
        },
        parent_accounting: ParentAccountingInputs {
            accounted_parent_artifact_provider: None,
            parent_consensus_metadata: None,
            parent_artifact_hint: None,
        },
        dependencies: BlockExecutionDependencies {
            bridge: None,
            evm_signer: None,
            runtime_host: Arc::new(outbe_runtime::OutbeRuntime::new()),
        },
    }
}

pub(crate) fn executor_inputs_from_ctx(
    ctx: &OutbeBlockExecutionCtx<'_>,
    evm_signer: Option<SharedOutbeEvmSigner>,
    validate_execution_summary: bool,
) -> BlockExecutorInputs {
    let expected_begin_system_txs = ctx.expected_begin_system_txs.clone();
    let system_layout_error = ctx.system_layout_error.clone();
    let parent_consensus_metadata = ctx.parent_consensus_metadata.clone();
    let proposer_evm_address = ctx.proposer_evm_address;
    let execute_outbe_block_hooks = ctx.execute_outbe_block_hooks;
    let prebuilt_phase1_tx = ctx.prebuilt_phase1_tx.clone();
    let parent_artifact_hint = ctx.parent_artifact_hint;
    BlockExecutorInputs {
        identity: BlockExecutionIdentity {
            block_extra_data: ctx.inner.extra_data.clone(),
            validate_execution_summary,
            block_hash: None,
            block_state_root: None,
            parent_hash: ctx.inner.parent_hash,
        },
        system_plan: BlockSystemPlan {
            expected_begin_system_txs,
            system_layout_error,
            proposer_evm_address,
            execute_outbe_block_hooks,
            prebuilt_phase1_tx,
            pending_tee_bootstrap: None,
        },
        parent_accounting: ParentAccountingInputs {
            accounted_parent_artifact_provider: None,
            parent_consensus_metadata,
            parent_artifact_hint,
        },
        dependencies: BlockExecutionDependencies {
            bridge: None,
            evm_signer,
            runtime_host: Arc::new(outbe_runtime::OutbeRuntime::new()),
        },
    }
}

pub(crate) fn executor_inputs_with_bootstrap(
    ctx: &OutbeBlockExecutionCtx<'_>,
    evm_signer: Option<SharedOutbeEvmSigner>,
    validate_execution_summary: bool,
) -> BlockExecutorInputs {
    let mut inputs = executor_inputs_from_ctx(ctx, evm_signer, validate_execution_summary);
    inputs.system_plan.pending_tee_bootstrap = ctx.pending_tee_bootstrap.clone();
    inputs
}

/// Observations from the executed prefix, without deriving any expected result.
pub(crate) struct CycleObservation {
    pub(crate) visible_gas: u64,
    pub(crate) internal_gas: u64,
    pub(crate) receipt_index: usize,
}

pub(crate) fn observe_cycle_tick(
    system_txs: Vec<Recovered<TransactionSigned>>,
    decode_message: &str,
    mut execute: impl FnMut(Recovered<TransactionSigned>) -> CycleObservation,
) -> Option<CycleObservation> {
    for tx in system_txs {
        let input = SystemTx::decode(tx.tx().input().as_ref()).expect(decode_message);
        let observation = execute(tx);
        if matches!(input, SystemTx::BeginBlock { step_index: 8 }) {
            return Some(observation);
        }
    }
    None
}

pub(crate) struct ParentAccountingFixture {
    pub(crate) parent_hash: B256,
    pub(crate) metadata: CertifiedParentAccountingMetadata,
    pub(crate) artifact: AccountedParentArtifact,
    pub(crate) proposer: Address,
}

pub(crate) fn parent_accounting_context(
    input: ParentAccountingFixture,
) -> OutbeBlockExecutionCtx<'static> {
    let mut execution = execution_ctx(Some(0), Bytes::new());
    execution.inner.parent_hash = input.parent_hash;
    execution.parent_consensus_metadata = Some(input.metadata);
    execution.parent_artifact_hint = Some(input.artifact);
    execution.proposer_evm_address = Some(input.proposer);
    execution
}

pub(crate) struct BeginPrefixCheck<'a> {
    pub(crate) execution_message: &'a str,
    pub(crate) cumulative_receipts: bool,
}

pub(crate) fn assert_begin_prefix_gas<E>(
    executor: &mut E,
    system_txs: Vec<Recovered<TransactionSigned>>,
    check: BeginPrefixCheck<'_>,
) -> u64
where
    E: alloy_evm::block::BlockExecutor<Transaction = TransactionSigned, Receipt = Receipt>,
    E::Evm: reth_ethereum::evm::primitives::Evm<Tx = revm::context::TxEnv>,
{
    let mut visible_system_gas_used = 0u64;
    for tx in system_txs {
        let signed_gas_limit = tx.tx().gas_limit();
        let gas_output = executor
            .execute_transaction(tx)
            .expect(check.execution_message);
        assert!(gas_output.tx_gas_used() <= signed_gas_limit);
        visible_system_gas_used += gas_output.tx_gas_used();
        if check.cumulative_receipts {
            assert_eq!(
                executor
                    .receipts()
                    .last()
                    .expect("system receipt should be present")
                    .cumulative_gas_used,
                visible_system_gas_used,
            );
        }
    }
    visible_system_gas_used
}

/// Successful block-one fixtures include the mandatory boundary as well as OST3.
pub(crate) fn block_one_artifacts(extra_data: Bytes) -> Bytes {
    let mut artifacts = decode_outbe_block_artifacts(&extra_data).unwrap();
    if artifacts.consensus_header_artifact.is_none() {
        artifacts.consensus_header_artifact =
            Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary_with(
                true,
                vec![(test_evm_signer().address(), dummy_pubkey(0xA2))],
            )));
    }
    encode_outbe_block_artifacts(&artifacts).unwrap()
}
