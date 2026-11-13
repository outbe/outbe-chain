//! `OutbeRuntime` implements the infrastructure seam traits.

use std::sync::Arc;

use alloy_primitives::{Address, Bytes, Log, B256, U256};
use outbe_consensus::protocol_state::{
    EngineProtocolError, EngineProtocolState, EngineValidatorQuery, EngineValidatorRow,
};
use outbe_evm::precompiles::PrecompileMutationCall;
use outbe_evm::{EvmRuntimeHost, UserTxFeePolicy};
use outbe_execution::tee_remote_session::TeeSessionStateReader;
use outbe_primitives::code_registry::{PatchApprovalRecord, PublicationReceipt, RepoId};
use outbe_primitives::committee_snapshot::CommitteeSnapshot;
use outbe_primitives::consensus::{DkgBoundaryArtifact, GenesisValidators};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::enclave_upgrade::EnclaveUpgradeV1;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::radicle_genesis::GovernmentsDescriptor;
use outbe_primitives::radicle_publication::PublicationBody;
use outbe_primitives::reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit};
use outbe_primitives::storage::{MutationEntitlements, StorageHandle};
use outbe_primitives::system_tx_context::SystemTxExecutionContext;
use outbe_primitives::tee_attestation_v1::{NodeIdV1, TeePolicyV1};
use outbe_primitives::tee_registry_abi_v1::NodeEnclaveBindingV1;
use outbe_primitives::validator_dto::{EpochSnapshot, ValidatorRecord};
use outbe_primitives::zero_fee_types::{
    BootstrapAccountView, BootstrapCandidate, BootstrapTransactionView, SponsorshipAuthorization,
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeePolicyError, ZeroFeeTransaction,
};
use outbe_radicle::protocol_state::{RadicleProtocolError, RadicleProtocolState};
use outbe_rpc::state_reader::OutbeRpcStateReader;
use outbe_rpc::{
    EpochInfo, ParticipationInfo, SlashConfig, SlashInfo, ValidatorDetailInfo, ValidatorInfo,
};
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;
use outbe_tee::tee_attestation_activation::TeeAttestationChainSpecStateV1;
use outbe_txpool::TxPoolZeroFeePolicy;
use {
    outbe_evm::system_tx::BlockOrder,
    outbe_primitives::block::{BlockHeightRegime, BlockRuntimeContext},
};

/// Composition root. One value implements every infrastructure seam.
#[derive(Debug, Default)]
pub struct OutbeRuntime;

impl OutbeRuntime {
    /// Returns the runtime host handle.
    #[must_use]
    pub fn new() -> Self {
        debug_assert!(!crate::precompiles::outbe_exact_precompile_addresses().is_empty());
        Self
    }

    /// Host handle for the EVM factory and the block executor.
    #[must_use]
    pub fn host(self: &Arc<Self>) -> Arc<dyn EvmRuntimeHost> {
        Arc::clone(self) as Arc<dyn EvmRuntimeHost>
    }
}

pub(crate) fn sorted_active_consensus_validators(
    storage: StorageHandle<'_>,
) -> Result<Vec<Address>> {
    let mut validators: Vec<Address> = outbe_validatorset::contract::ValidatorSet::new(storage)
        .get_active_consensus_set()?
        .into_iter()
        .map(|record| record.validator_address)
        .collect();
    validators.sort();
    Ok(validators)
}

impl UserTxFeePolicy for OutbeRuntime {
    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        outbe_zerofee::registry().classify(tx)
    }

    fn classify_bootstrap(&self, tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate> {
        outbe_zerofee::classify_bootstrap(tx)
    }

    fn authorize_bootstrap(
        &self,
        candidate: BootstrapCandidate,
        account: BootstrapAccountView,
    ) -> bool {
        outbe_zerofee::authorize_bootstrap(candidate, account)
    }

    fn authorize_sponsorship(
        &self,
        storage: StorageHandle<'_>,
        signer: Address,
        block_timestamp_secs: u64,
    ) -> std::result::Result<SponsorshipAuthorization, ZeroFeePolicyError> {
        outbe_zerofee::authorize_sponsorship(storage, signer, block_timestamp_secs)
    }

    fn record_sponsorship_use(
        &self,
        storage: StorageHandle<'_>,
        authorization: SponsorshipAuthorization,
    ) -> std::result::Result<u32, ZeroFeePolicyError> {
        outbe_zerofee::record_sponsorship_use(storage, authorization)
    }

    fn sponsored_gas_exceeds_block_budget(&self, used: u64, gas_limit: u64) -> bool {
        outbe_zerofee::sponsored_gas_exceeds_block_budget(used, gas_limit)
    }

    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle<'_>,
        candidate: ZeroFeeCandidate,
    ) -> std::result::Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        outbe_zerofee::registry().authorize_fee_waiver(storage, candidate)
    }

    fn classify_sponsorship(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<(), ZeroFeePolicyError> {
        outbe_zerofee::classify_sponsorship(tx)
    }
}

impl TxPoolZeroFeePolicy for OutbeRuntime {
    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        outbe_zerofee::registry().classify(tx)
    }

    fn authorize(
        &self,
        storage: StorageHandle<'_>,
        candidate: ZeroFeeCandidate,
    ) -> std::result::Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        outbe_zerofee::registry().authorize_fee_waiver(storage, candidate)
    }

    fn classify_bootstrap(&self, tx: &BootstrapTransactionView<'_>) -> Option<BootstrapCandidate> {
        outbe_zerofee::classify_bootstrap(tx)
    }

    fn authorize_bootstrap(
        &self,
        candidate: BootstrapCandidate,
        account: BootstrapAccountView,
    ) -> bool {
        outbe_zerofee::authorize_bootstrap(candidate, account)
    }

    fn classify_sponsorship(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> std::result::Result<(), ZeroFeePolicyError> {
        outbe_zerofee::classify_sponsorship(tx)
    }

    fn precheck_sponsorship(&self, signer: Address) -> std::result::Result<(), ZeroFeePolicyError> {
        outbe_zerofee::precheck_sponsorship(signer)
    }
}

impl EvmRuntimeHost for OutbeRuntime {
    fn block_order(&self) -> BlockOrder {
        crate::system_tx::BLOCK_ORDER
    }

    fn marker_addresses(&self) -> &'static [Address] {
        &crate::marker_addresses::OUTBE_RUNTIME_MARKER_ADDRESSES
    }

    fn is_registered_precompile(&self, address: &Address) -> bool {
        crate::precompiles::is_registered(address)
    }

    fn precompile_is_payable(&self, address: &Address) -> bool {
        crate::precompiles::is_payable(address)
    }

    fn precompile_base_gas(&self, address: &Address, input: &[u8]) -> u64 {
        crate::precompiles::base_gas(address, input)
    }

    fn dispatch_precompile(
        &self,
        storage: StorageHandle<'_>,
        address: &Address,
        data: &[u8],
        caller: Address,
        value: U256,
    ) -> Result<Bytes> {
        crate::precompiles::dispatch(storage, address, data, caller, value)
    }

    fn dispatch_system_tx(
        &self,
        storage: StorageHandle<'_>,
        data: &[u8],
        caller: Address,
        value: U256,
        tee: &TeeAttestationChainSpecStateV1,
    ) -> Result<Bytes> {
        crate::system_tx::dispatch(storage, data, caller, value, tee)
    }

    fn validate_finalized_metadata(
        &self,
        storage: StorageHandle<'_>,
        metadata: &CertifiedParentAccountingMetadata,
    ) -> Result<()> {
        outbe_rewards::finalization::validate_finalized_metadata(storage, metadata)
    }

    fn run_finalization_and_slashing(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        metadata: &CertifiedParentAccountingMetadata,
        execution: &SystemTxExecutionContext,
    ) -> Result<()> {
        outbe_rewards::finalization::run_finalization_and_slashing(ctx, metadata, execution)
    }

    fn authenticate_late_credit(
        &self,
        storage: &StorageHandle<'_>,
        credit: &PerBlockCredit,
    ) -> Result<()> {
        outbe_rewards::finalization::authenticate_late_credit(storage, credit)
    }

    fn run_late_finalize_credits(
        &self,
        ctx: &BlockRuntimeContext<'_>,
        artifact: &LateFinalizeCreditsArtifact,
        execution: Option<&SystemTxExecutionContext>,
    ) -> Result<()> {
        outbe_rewards::finalization::run_late_finalize_credits(ctx, artifact, execution)
    }

    fn apply_boundary_outcome(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()> {
        outbe_validatorset::boundary::apply_boundary_outcome(
            storage,
            boundary,
            block_number,
            timestamp,
        )
    }

    fn prepare_boundary(
        &self,
        storage: StorageHandle<'_>,
        boundary: &DkgBoundaryArtifact,
        block_number: u64,
    ) -> Result<()> {
        outbe_slashindicator::prepare_boundary_epoch_counters(storage, boundary, block_number)
    }

    fn consensus_validators(&self, storage: StorageHandle<'_>) -> Result<Vec<Address>> {
        sorted_active_consensus_validators(storage)
    }

    fn is_consensus_participant(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<bool> {
        outbe_validatorset::is_consensus_participant(storage, proposer)
    }

    fn is_validator(&self, storage: StorageHandle<'_>, proposer: Address) -> Result<bool> {
        outbe_validatorset::is_validator(storage, proposer)
    }

    fn read_committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        key: B256,
    ) -> Result<Option<CommitteeSnapshot>> {
        outbe_validatorset::state::read_committee_snapshot(storage, key)
    }

    fn validate_genesis_state_for_regime(
        &self,
        regime: BlockHeightRegime,
        storage: StorageHandle<'_>,
        genesis_validators: Option<&GenesisValidators>,
    ) -> Result<()> {
        outbe_staking::genesis::validate_genesis_state_for_regime(
            regime,
            storage,
            genesis_validators,
        )
    }

    fn validate_genesis_state(
        &self,
        storage: StorageHandle<'_>,
        genesis: &GenesisValidators,
    ) -> Result<()> {
        outbe_staking::genesis::validate_genesis_state(storage, genesis)
    }

    fn proposer_attestation(
        &self,
        storage: StorageHandle<'_>,
        proposer: Address,
    ) -> Result<Option<[u8; 32]>> {
        Ok(outbe_teeregistry::TeeRegistry::new(storage)
            .validator_enclave_binding_v1(proposer)?
            .map(|binding| binding.attestation_ed25519.0))
    }

    fn mutation_entitlements(&self, call: PrecompileMutationCall<'_>) -> MutationEntitlements {
        crate::system_tx::precompile_mutation_entitlements(call)
    }

    fn is_protocol_cycle_call(&self, address: Address, caller: Address, data: &[u8]) -> bool {
        crate::system_tx::is_protocol_cycle_call(address, caller, data)
    }

    fn derive_active_utc_day(
        &self,
        storage: StorageHandle<'_>,
        genesis_timestamp: u64,
        block_timestamp: u64,
    ) -> Result<u32> {
        outbe_cycle::call_authority::cycle_call_active_utc_day(
            storage,
            genesis_timestamp,
            block_timestamp,
        )
    }

    fn settle_block_burned_fees(&self, mut storage: StorageHandle<'_>, amount: U256) -> Result<()> {
        outbe_promislimit::settle_block_burned_fees(&mut storage, amount)
    }

    fn user_fee_policy(&self) -> &dyn UserTxFeePolicy {
        self
    }
}

impl TeeSessionStateReader for OutbeRuntime {
    fn node_binding(
        &self,
        storage: StorageHandle<'_>,
        node_id: &NodeIdV1,
    ) -> std::result::Result<Option<NodeEnclaveBindingV1>, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).node_enclave_binding_for_identity_v1(node_id)
    }

    fn binding_admitted(
        &self,
        storage: StorageHandle<'_>,
        binding: &NodeEnclaveBindingV1,
        height: u64,
    ) -> std::result::Result<bool, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).binding_code_admitted_at_v1(binding, height)
    }

    fn binding_slots(
        &self,
        storage: StorageHandle<'_>,
        node_id: &NodeIdV1,
    ) -> std::result::Result<Vec<B256>, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).node_enclave_binding_storage_slots_v1(node_id)
    }

    fn retirement_height(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<u64, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).last_enclave_retirement_height_v1()
    }

    fn successor(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<Option<(U256, TeePolicyV1)>, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).staged_successor_policy_v1()
    }

    fn enclave_upgrade(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<EnclaveUpgradeV1, PrecompileError> {
        outbe_teeregistry::TeeRegistry::new(storage).enclave_upgrade_v1()
    }
}

impl OutbeRpcStateReader for OutbeRuntime {
    fn validators(&self, storage: StorageHandle<'_>) -> Result<Vec<ValidatorInfo>> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let records = vs.get_active_validators()?;
        let staking = outbe_staking::contract::Staking::new(storage);
        let mut result = Vec::with_capacity(records.len());
        for record in &records {
            let stake = staking
                .get_stake(record.validator_address)
                .unwrap_or(U256::ZERO);
            result.push(ValidatorInfo {
                address: record.validator_address,
                consensus_pubkey: hex::encode(record.consensus_pubkey),
                status: record.status,
                stake,
            });
        }
        Ok(result)
    }

    fn validator(
        &self,
        storage: StorageHandle<'_>,
        address: Address,
    ) -> Result<Option<ValidatorDetailInfo>> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let state = vs.validator_state(address)?;
        if !state.is_registered() {
            return Ok(None);
        }
        let consensus_pubkey = state
            .consensus_pubkey()
            .ok_or(outbe_rpc::RpcValidatorStateError::MissingConsensusPubkey)?;
        let history = state
            .history()
            .ok_or(outbe_rpc::RpcValidatorStateError::MissingHistory)?;
        Ok(Some(ValidatorDetailInfo {
            address,
            consensus_pubkey: hex::encode(consensus_pubkey),
            status: state
                .lifecycle()
                .stored_status()
                .ok_or(outbe_rpc::RpcValidatorStateError::MissingPersistedStatus)?,
            stake: state.bonded_stake(),
            slash_count: history.slash_count(),
            missed_blocks: history.missed_blocks(),
            missed_votes: history.missed_votes(),
            blocks_proposed: history.blocks_proposed(),
            joined_at_height: history.joined_at_height(),
            deactivated_at_height: history.last_deactivated_at_height().unwrap_or(0),
            unbonding_end: state.unbonding_end_hint().unwrap_or(0),
            has_bls_share: state.has_bls_share(),
        }))
    }

    fn epoch(&self, storage: StorageHandle<'_>) -> Result<EpochInfo> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        let epoch = vs.epoch_snapshot()?;
        let active_count = vs.active_validator_count()?;
        let total_staked = outbe_staking::contract::Staking::new(storage).get_total_staked()?;
        Ok(EpochInfo {
            epoch_number: epoch.number,
            epoch_start_timestamp: epoch.start_timestamp,
            epoch_start_block: epoch.start_block,
            epoch_length_blocks: epoch.length_blocks,
            active_validator_count: active_count,
            total_staked,
        })
    }

    fn epoch_snapshot(&self, storage: StorageHandle<'_>) -> Result<EpochSnapshot> {
        outbe_validatorset::contract::ValidatorSet::new(storage).epoch_snapshot()
    }

    fn stake(&self, storage: StorageHandle<'_>, address: Address) -> Result<U256> {
        outbe_staking::contract::Staking::new(storage).get_stake(address)
    }

    fn slash_info(&self, storage: StorageHandle<'_>, address: Address) -> Result<SlashInfo> {
        let indicator = outbe_slashindicator::contract::SlashIndicator::new(storage);
        Ok(SlashInfo {
            proposer_miss_count: indicator.proposer_miss_count.read(&address)?,
            voter_miss_count: indicator.voter_miss_count.read(&address)?,
            felony_count: indicator.felony_count.read(&address)?,
        })
    }

    fn slash_config(&self, storage: StorageHandle<'_>) -> Result<SlashConfig> {
        let indicator = outbe_slashindicator::contract::SlashIndicator::new(storage);
        Ok(SlashConfig {
            proposer_misdemeanor_threshold: indicator.proposer_misdemeanor_threshold()?,
            proposer_felony_threshold: indicator.proposer_felony_threshold()?,
            voter_misdemeanor_threshold: indicator.voter_misdemeanor_threshold()?,
            voter_felony_threshold: indicator.voter_felony_threshold()?,
            slash_amount_percent: indicator.slash_amount_percent()?,
            evidence_reward_percent: indicator.evidence_reward_percent()?,
        })
    }

    fn participation(
        &self,
        storage: StorageHandle<'_>,
        address: Address,
    ) -> Result<ParticipationInfo> {
        let participation =
            outbe_validatorset::contract::ValidatorSet::new(storage).participation(address)?;
        Ok(ParticipationInfo {
            address,
            blocks_proposed: participation.blocks_proposed,
            missed_blocks: participation.missed_blocks,
            missed_votes: participation.missed_votes,
        })
    }

    fn live_upgrade_candidate(
        &self,
        storage: StorageHandle<'_>,
        context: &DcapOnboardingContextV1,
        timestamp: u64,
        legacy_direct_dev_source: bool,
    ) -> Result<bool> {
        outbe_teeregistry::TeeRegistry::new(storage).live_upgrade_candidate_v1(
            context,
            timestamp,
            legacy_direct_dev_source,
        )
    }
}

impl EngineProtocolState for OutbeRuntime {
    fn system_tx_leader_policy(
        &self,
    ) -> outbe_primitives::system_tx::binding::SystemTxLeaderPolicy {
        outbe_primitives::system_tx::binding::SystemTxLeaderPolicy {
            validate_layout: |body, header| {
                outbe_evm::system_tx::validate_system_layout(
                    body,
                    header,
                    &crate::system_tx::BLOCK_ORDER,
                )
            },
            step_name_or_stage: |tx| crate::system_tx::BLOCK_ORDER.step_name_or_stage(tx),
        }
    }

    fn validator_rows(
        &self,
        storage: StorageHandle<'_>,
        query: EngineValidatorQuery,
    ) -> Result<Vec<EngineValidatorRow>> {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let records = match query {
            EngineValidatorQuery::ActiveValidators => vs.get_active_validators()?,
            EngineValidatorQuery::ConsensusParticipants => vs.get_active_consensus_set()?,
            EngineValidatorQuery::ReshareTarget => vs.get_reshare_target_set()?,
            EngineValidatorQuery::PendingValidators => vs.get_pending_validators()?,
            EngineValidatorQuery::AdmittedNonConsensus => {
                vs.get_admitted_non_consensus_validators()?
            }
        };
        let mut rows = Vec::with_capacity(records.len());
        for record in records {
            let p2p = match vs.get_p2p_address(record.validator_address) {
                Ok(Some((version, encoded))) => {
                    outbe_consensus::protocol_state::EngineP2pRead::Present { version, encoded }
                }
                Ok(None) => outbe_consensus::protocol_state::EngineP2pRead::Missing,
                Err(error) => outbe_consensus::protocol_state::EngineP2pRead::Failed { error },
            };
            rows.push(EngineValidatorRow { record, p2p });
        }
        Ok(rows)
    }

    fn committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        epoch: u64,
    ) -> Result<Option<CommitteeSnapshot>> {
        outbe_validatorset::state::read_committee_snapshot_for_epoch(storage, epoch)
    }

    fn has_pending_set_change(&self, storage: StorageHandle<'_>) -> Result<bool> {
        outbe_validatorset::contract::ValidatorSet::new(storage).has_pending_set_change()
    }

    fn tee_offer_public(&self, storage: StorageHandle<'_>) -> Result<B256> {
        outbe_teeregistry::TeeRegistry::new(storage).offer_public_key()
    }

    fn tee_offer_epoch(&self, storage: StorageHandle<'_>) -> Result<u64> {
        outbe_teeregistry::TeeRegistry::new(storage).tribute_offer_epoch()
    }

    fn tee_recipient(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> Result<Option<B256>> {
        Ok(outbe_teeregistry::TeeRegistry::new(storage)
            .validator_enclave_binding_v1(validator)?
            .map(|binding| binding.recipient_x25519))
    }

    fn node_host_binding(
        &self,
        storage: StorageHandle<'_>,
        reth_p2p_public: [u8; 33],
    ) -> Result<Option<NodeEnclaveBindingV1>> {
        outbe_teeregistry::TeeRegistry::new(storage).node_host_enclave_binding_v1(reth_p2p_public)
    }

    fn binding_code_admitted(
        &self,
        storage: StorageHandle<'_>,
        binding: &NodeEnclaveBindingV1,
        block_number: u64,
    ) -> Result<bool> {
        outbe_teeregistry::TeeRegistry::new(storage)
            .binding_code_admitted_at_v1(binding, block_number)
    }

    fn validator_enclave_binding(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> Result<Option<NodeEnclaveBindingV1>> {
        outbe_teeregistry::TeeRegistry::new(storage).validator_enclave_binding_v1(validator)
    }

    fn validator_record(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> Result<Option<ValidatorRecord>> {
        outbe_validatorset::contract::ValidatorSet::new(storage).get_validator(validator)
    }

    fn check_binary_compatibility(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<(), EngineProtocolError> {
        let update = outbe_update::schema::Update::new(storage);
        let active_version = update.get_active_version()?;
        outbe_update::startup::assert_binary_protocol_compatible(active_version).map_err(
            |outbe_update::errors::StartupCompatibilityError::BinaryOlderThanActive {
                 binary,
                 active,
             }| EngineProtocolError::BinaryOlderThanActive { binary, active },
        )?;
        let mut waiting = Vec::new();
        for proposal_id in update.list_waiting_for_activation_proposal_ids()? {
            if let Some(record) = update.read_scheduled_update(proposal_id)? {
                waiting.push(record);
            }
        }
        outbe_update::startup::warn_missing_handlers_for_waiting_updates(
            &waiting,
            outbe_update::handlers::registry(),
        );
        Ok(())
    }
}

impl RadicleProtocolState for OutbeRuntime {
    fn publication_receipt(
        &self,
        storage: StorageHandle<'_>,
        sequence: u64,
    ) -> std::result::Result<Option<PublicationReceipt>, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).publication_receipt(sequence)?)
    }

    fn pending_publication_attesters(
        &self,
        storage: StorageHandle<'_>,
        body: &PublicationBody,
    ) -> std::result::Result<Vec<u32>, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).pending_publication_attesters(body)?)
    }

    fn network_repository(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<B256, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .network_repository
            .read()?)
    }

    fn publication_sequence(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<u64, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .publication_sequence
            .read()?)
    }

    fn publication_material_sequence(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<u64, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .publication_material_sequence
            .read()?)
    }

    fn publication_receipt_hash(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<B256, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .publication_receipt_hash
            .read()?)
    }

    fn governments_descriptor(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<Option<GovernmentsDescriptor>, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).governments_descriptor()?)
    }

    fn patch_approval(
        &self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
    ) -> std::result::Result<Option<PatchApprovalRecord>, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).patch_approval(proposal_id)?)
    }

    fn config_descriptor_hash(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<B256, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .config_descriptor_hash
            .read()?)
    }

    fn patch_execution_stage(
        &self,
        storage: StorageHandle<'_>,
        proposal_id: U256,
    ) -> std::result::Result<u8, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .patch_execution_stage
            .read(&proposal_id)?)
    }

    fn repository_count(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<u32, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .repository_count
            .read()?)
    }

    fn registry_generation(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<u64, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage)
            .registry_generation
            .read()?)
    }

    fn repository_at(
        &self,
        storage: StorageHandle<'_>,
        index: u32,
    ) -> std::result::Result<RepoId, RadicleProtocolError> {
        Ok(outbe_coderegistry::CodeRegistry::new(storage).repository_at(index)?)
    }

    fn current_epoch(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<u64, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage).current_epoch_u64()?)
    }

    fn active_consensus_set(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<Vec<ValidatorRecord>, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage).get_active_consensus_set()?)
    }

    fn epoch_snapshot(
        &self,
        storage: StorageHandle<'_>,
    ) -> std::result::Result<EpochSnapshot, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage).epoch_snapshot()?)
    }

    fn radicle_node_id(
        &self,
        storage: StorageHandle<'_>,
        validator: Address,
    ) -> std::result::Result<B256, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage)
            .get_radicle_node_id(validator)?)
    }

    fn validator_by_radicle_node_id(
        &self,
        storage: StorageHandle<'_>,
        node_id: B256,
    ) -> std::result::Result<Address, RadicleProtocolError> {
        Ok(outbe_validatorset::contract::ValidatorSet::new(storage)
            .validator_by_radicle_node_id(node_id)?)
    }

    fn committee_snapshot(
        &self,
        storage: StorageHandle<'_>,
        epoch: u64,
    ) -> std::result::Result<Option<CommitteeSnapshot>, RadicleProtocolError> {
        Ok(outbe_validatorset::state::read_committee_snapshot_for_epoch(storage, epoch)?)
    }

    fn approved_proposal_id(
        &self,
        log: &Log,
    ) -> std::result::Result<Option<U256>, RadicleProtocolError> {
        use alloy_sol_types::SolEvent as _;
        use outbe_vote::precompile::IVote;
        if log.address == outbe_system_common::addresses::VOTE_ADDRESS
            && log.data.topics().first() == Some(&IVote::ProposalApproved::SIGNATURE_HASH)
        {
            let event = IVote::ProposalApproved::decode_log_validate(log)?;
            Ok(Some(event.data.proposalId))
        } else {
            Ok(None)
        }
    }
}
