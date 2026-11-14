//! Chain reads behind the `outbe_*` RPC methods.
//!
//! The handler keeps the RPC wire types. This trait supplies the chain facts.
//! Callers pass the installed reader.
//! Emission percent is a constant shared with the emission crate and does not
//! read chain state.

use alloy_primitives::{Address, U256};
#[cfg(test)]
use outbe_primitives::error::PrecompileError;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::validator_dto::EpochSnapshot;
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;

use crate::api::{
    EmissionInfo, EpochInfo, ParticipationInfo, SlashConfig, SlashInfo, ValidatorDetailInfo,
    ValidatorInfo,
};

/// Validator reward percent. Locked to the emission allocation constant by a test.
const VALIDATOR_REWARD_PERCENT: u64 = 4;

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

    /// Whether `context` is the live finalized upgrade candidate.
    fn live_upgrade_candidate(
        &self,
        storage: StorageHandle<'_>,
        context: &DcapOnboardingContextV1,
        timestamp: u64,
        legacy_direct_dev_source: bool,
    ) -> Result<bool>;
}

#[cfg(test)]
pub(crate) struct ClosedRpcReader;

#[cfg(test)]
fn closed<T>() -> Result<T> {
    Err(PrecompileError::storage(
        "rpc state reader is not installed",
    ))
}

#[cfg(test)]
impl OutbeRpcStateReader for ClosedRpcReader {
    fn validators(&self, _: StorageHandle<'_>) -> Result<Vec<ValidatorInfo>> {
        closed()
    }

    fn validator(&self, _: StorageHandle<'_>, _: Address) -> Result<Option<ValidatorDetailInfo>> {
        closed()
    }

    fn epoch(&self, _: StorageHandle<'_>) -> Result<EpochInfo> {
        closed()
    }

    fn epoch_snapshot(&self, _: StorageHandle<'_>) -> Result<EpochSnapshot> {
        closed()
    }

    fn stake(&self, _: StorageHandle<'_>, _: Address) -> Result<U256> {
        closed()
    }

    fn slash_info(&self, _: StorageHandle<'_>, _: Address) -> Result<SlashInfo> {
        closed()
    }

    fn slash_config(&self, _: StorageHandle<'_>) -> Result<SlashConfig> {
        closed()
    }

    fn participation(&self, _: StorageHandle<'_>, _: Address) -> Result<ParticipationInfo> {
        closed()
    }

    fn live_upgrade_candidate(
        &self,
        _: StorageHandle<'_>,
        _: &DcapOnboardingContextV1,
        _: u64,
        _: bool,
    ) -> Result<bool> {
        closed()
    }
}
