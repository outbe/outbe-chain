//! Production chain readers for the RPC, engine, and radicle seams.
//!
//! These bodies used to live behind `cfg(test)` in the infrastructure crates.
//! The composition root is now the production implementation.

use alloy_primitives::{Address, Log, B256, U256};
use outbe_engine::protocol_state::{
    EngineProtocolError, EngineProtocolState, EngineValidatorQuery, EngineValidatorRow,
};
use outbe_primitives::code_registry::{PatchApprovalRecord, PublicationReceipt, RepoId};
use outbe_primitives::committee_snapshot::CommitteeSnapshot;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::radicle_genesis::GovernmentsDescriptor;
use outbe_primitives::radicle_publication::PublicationBody;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::tee_registry_abi_v1::NodeEnclaveBindingV1;
use outbe_primitives::validator_dto::{EpochSnapshot, ValidatorRecord};
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_radicle::protocol_state::{RadicleProtocolError, RadicleProtocolState};
use outbe_rpc::state_reader::{OutbeRpcStateReader, TributeWords};
use outbe_rpc::{
    EpochInfo, ParticipationInfo, SlashConfig, SlashInfo, ValidatorDetailInfo, ValidatorInfo,
};
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;
use outbe_tee::tribute_source::TributeSourceSubject;

use crate::wiring::OutbeRuntime;

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

    fn tribute_words(
        &self,
        storage: StorageHandle<'_>,
        subject: &TributeSourceSubject,
        tribute_id: WwdEntityId,
    ) -> Result<TributeWords> {
        let record = outbe_tribute::state_api::get_retained_record(&storage, tribute_id)?
            .ok_or_else(|| PrecompileError::storage("Tribute source is missing"))?;
        let tribute = record
            .encrypted()
            .ok_or_else(|| PrecompileError::storage("Tribute source is not encrypted"))?
            .clone();
        let contract = outbe_tribute::TributeContract::new(storage);
        let position = contract.day_id_position.read(&tribute_id)?;
        if position == 0 || position > subject.tribute_count {
            return Err(PrecompileError::storage("Tribute source position mismatch"));
        }
        let day_index = position - 1;
        let node_subject = outbe_tribute::state_proof::TributeStateProofSubject::from(*subject);
        let words = outbe_tribute::state_proof::expected_record_words(
            &contract,
            &node_subject,
            day_index,
            &tribute,
        )?;
        Ok((tribute, day_index, words))
    }

    fn live_upgrade_candidate(
        &self,
        storage: StorageHandle<'_>,
        context: &DcapOnboardingContextV1,
        timestamp: u64,
        legacy_direct_dev_source: bool,
    ) -> Result<bool> {
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        let policy = registry.active_policy_v1()?;
        if !candidate_binding_matches(&registry, context, timestamp)? {
            return Ok(false);
        }
        if !candidate_offer_matches(&registry, context)? {
            return Ok(false);
        }
        let chain_matches =
            context.chain_id == policy.chain_id && context.genesis_hash == policy.genesis_hash;
        let mode_allowed = !legacy_direct_dev_source
            || policy.attestation_mode
                == outbe_primitives::tee_attestation_v1::AttestationMode::GramineDirectDev;
        Ok(chain_matches && mode_allowed)
    }
}

fn candidate_binding_matches(
    registry: &outbe_teeregistry::TeeRegistry<'_>,
    context: &DcapOnboardingContextV1,
    timestamp: u64,
) -> Result<bool> {
    let node = context.node_id_hash;
    let live_context = registry.upgrade_candidate_context.read(&node)? == context.context_hash()
        && registry.upgrade_candidate_expiry.read(&node)? > timestamp;
    Ok(live_context
        && registry.upgrade_candidate_source.read(&node)?
            == registry.v1_node_binding_id.read(&node)?
        && !registry.v1_node_binding_id.read(&node)?.is_zero())
}

fn candidate_offer_matches(
    registry: &outbe_teeregistry::TeeRegistry<'_>,
    context: &DcapOnboardingContextV1,
) -> Result<bool> {
    Ok(
        registry.strict_upgrade_successor.read()? == context.policy_hash
            && registry.offer_public_key()?.0 == context.tribute_offer_public
            && registry.key_epoch()? == context.key_epoch
            && registry.tribute_offer_epoch()? == context.tribute_offer_epoch,
    )
}

impl EngineProtocolState for OutbeRuntime {
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
                    outbe_engine::protocol_state::EngineP2pRead::Present { version, encoded }
                }
                Ok(None) => outbe_engine::protocol_state::EngineP2pRead::Missing,
                Err(error) => outbe_engine::protocol_state::EngineP2pRead::Failed {
                    error: error.to_string(),
                },
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
        let active_version = update
            .get_active_version()
            .map_err(|error| EngineProtocolError::Compatibility(error.to_string()))?;
        outbe_update::startup::assert_binary_protocol_compatible(active_version)
            .map_err(|error| EngineProtocolError::Compatibility(error.to_string()))?;
        let mut waiting = Vec::new();
        for proposal_id in update
            .list_waiting_for_activation_proposal_ids()
            .map_err(|error| EngineProtocolError::Compatibility(error.to_string()))?
        {
            if let Some(record) = update
                .read_scheduled_update(proposal_id)
                .map_err(|error| EngineProtocolError::Compatibility(error.to_string()))?
            {
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
        if log.address == outbe_primitives::addresses::VOTE_ADDRESS
            && log.data.topics().first() == Some(&IVote::ProposalApproved::SIGNATURE_HASH)
        {
            let event = IVote::ProposalApproved::decode_log_validate(log)
                .map_err(|error| RadicleProtocolError::VoteLog(error.to_string()))?;
            Ok(Some(event.data.proposalId))
        } else {
            Ok(None)
        }
    }
}
