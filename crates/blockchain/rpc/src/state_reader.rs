//! Chain reads behind the `outbe_*` RPC methods.
//!
//! The handler keeps the RPC wire types. This trait supplies the chain facts.
//! Callers pass the installed reader. [`UninstalledRpcStateReader`] fails closed.
//! Emission percent is a constant shared with the emission crate and does not
//! read chain state.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::tribute_encryption::EncryptedTributeV2;
use outbe_primitives::validator_dto::EpochSnapshot;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;
use outbe_tee::tribute_source::TributeSourceSubject;

use crate::api::{
    EmissionInfo, EpochInfo, ParticipationInfo, SlashConfig, SlashInfo, ValidatorDetailInfo,
    ValidatorInfo,
};

/// Validator reward percent. Locked to the emission allocation constant by a test.
const VALIDATOR_REWARD_PERCENT: u64 = 4;

/// Encrypted tribute, its day index, and the storage words a proof must cover.
pub type TributeWords = (EncryptedTributeV2, u32, Vec<(B256, U256)>);

/// Object-safe chain reads for the RPC handler.
pub trait OutbeRpcStateReader: Send + Sync {
    /// Active validators with their current stake.
    fn validators(&self, storage: StorageHandle<'_>) -> Result<Vec<ValidatorInfo>>;

    /// One validator, or `Ok(None)` when the address is not registered.
    fn validator(
        &self,
        storage: StorageHandle<'_>,
        address: Address,
    ) -> Result<Option<ValidatorDetailInfo>>;

    /// Current epoch, active count, and total stake.
    fn epoch(&self, storage: StorageHandle<'_>) -> Result<EpochInfo>;

    /// Epoch metadata used by the TEE renewal schedule.
    fn epoch_snapshot(&self, storage: StorageHandle<'_>) -> Result<EpochSnapshot>;

    /// Bonded stake for `address`.
    fn stake(&self, storage: StorageHandle<'_>, address: Address) -> Result<U256>;

    /// Miss and felony counters for `address`.
    fn slash_info(&self, storage: StorageHandle<'_>, address: Address) -> Result<SlashInfo>;

    /// Effective slash thresholds and percents.
    fn slash_config(&self, storage: StorageHandle<'_>) -> Result<SlashConfig>;

    /// Proposal and miss counters for `address`.
    fn participation(
        &self,
        storage: StorageHandle<'_>,
        address: Address,
    ) -> Result<ParticipationInfo>;

    /// Reward split advertised by `outbe_getEmissionInfo`.
    fn emission_info(&self) -> EmissionInfo {
        EmissionInfo {
            validator_reward_percent: VALIDATOR_REWARD_PERCENT,
            fee_escrow_address: outbe_primitives::addresses::REWARDS_ADDRESS,
        }
    }

    /// Encrypted tribute, day index, and the storage words a proof must cover.
    fn tribute_words(
        &self,
        storage: StorageHandle<'_>,
        subject: &TributeSourceSubject,
        tribute_id: WwdEntityId,
    ) -> Result<TributeWords>;

    /// Whether `context` is the live finalized upgrade candidate.
    fn live_upgrade_candidate(
        &self,
        storage: StorageHandle<'_>,
        context: &DcapOnboardingContextV1,
        timestamp: u64,
        legacy_direct_dev_source: bool,
    ) -> Result<bool>;
}

/// Fail-closed reader used when no composition root installed chain reads.
pub struct UninstalledRpcStateReader;

fn not_installed<T>() -> Result<T> {
    Err(PrecompileError::storage(
        "rpc state reader is not installed",
    ))
}

impl OutbeRpcStateReader for UninstalledRpcStateReader {
    fn validators(&self, _storage: StorageHandle<'_>) -> Result<Vec<ValidatorInfo>> {
        not_installed()
    }

    fn validator(
        &self,
        _storage: StorageHandle<'_>,
        _address: Address,
    ) -> Result<Option<ValidatorDetailInfo>> {
        not_installed()
    }

    fn epoch(&self, _storage: StorageHandle<'_>) -> Result<EpochInfo> {
        not_installed()
    }

    fn epoch_snapshot(&self, _storage: StorageHandle<'_>) -> Result<EpochSnapshot> {
        not_installed()
    }

    fn stake(&self, _storage: StorageHandle<'_>, _address: Address) -> Result<U256> {
        not_installed()
    }

    fn slash_info(&self, _storage: StorageHandle<'_>, _address: Address) -> Result<SlashInfo> {
        not_installed()
    }

    fn slash_config(&self, _storage: StorageHandle<'_>) -> Result<SlashConfig> {
        not_installed()
    }

    fn participation(
        &self,
        _storage: StorageHandle<'_>,
        _address: Address,
    ) -> Result<ParticipationInfo> {
        not_installed()
    }

    fn tribute_words(
        &self,
        _storage: StorageHandle<'_>,
        _subject: &TributeSourceSubject,
        _tribute_id: WwdEntityId,
    ) -> Result<TributeWords> {
        not_installed()
    }

    fn live_upgrade_candidate(
        &self,
        _storage: StorageHandle<'_>,
        _context: &DcapOnboardingContextV1,
        _timestamp: u64,
        _legacy_direct_dev_source: bool,
    ) -> Result<bool> {
        not_installed()
    }
}

#[cfg(test)]
pub(crate) struct StorageRpcStateReader;

#[cfg(test)]
impl OutbeRpcStateReader for StorageRpcStateReader {
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
            .ok_or(crate::errors::RpcValidatorStateError::MissingConsensusPubkey)?;
        let history = state
            .history()
            .ok_or(crate::errors::RpcValidatorStateError::MissingHistory)?;
        Ok(Some(ValidatorDetailInfo {
            address,
            consensus_pubkey: hex::encode(consensus_pubkey),
            status: state
                .lifecycle()
                .stored_status()
                .ok_or(crate::errors::RpcValidatorStateError::MissingPersistedStatus)?,
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
        // This function reads the policy even when the first candidate condition rejects.
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    #[test]
    fn validator_reward_percent_matches_emission() {
        assert_eq!(
            super::VALIDATOR_REWARD_PERCENT,
            outbe_emission::allocation::VALIDATOR_REWARD_PCT
        );
    }
}
