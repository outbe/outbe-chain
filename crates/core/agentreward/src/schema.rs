use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::error::Result;
use outbe_primitives::storage::dsl::Map;
use outbe_system_common::addresses::AGENT_REWARD_ADDRESS;

use crate::errors::AgentRewardError;

/// EVM storage layout for the Agent Reward contract.
///
/// Keeps the WAA and SRA weights of each emission day: the offer count of each address for the
/// Nods of that day, the number of addresses and the sum of the counts. Keeps one rate record for
/// each pool and day whose rate is set and that still has a receiver with a weight.
#[storage_schema]
#[contract(addr = AGENT_REWARD_ADDRESS)]
pub struct AgentRewardContract {
    /// WAA offer count per emission day and address (key: `offer_count_key`).
    #[attribute(order = 0)]
    pub waa_offer_counts: Map<B256, u64>,

    /// SRA offer count per emission day and address (key: `offer_count_key`).
    #[attribute(order = 1)]
    pub sra_offer_counts: Map<B256, u64>,

    /// The number of the WAA addresses with an offer, per emission day.
    #[attribute(order = 2)]
    pub waa_address_count: Map<u32, u32>,

    /// The number of the SRA addresses with an offer, per emission day.
    #[attribute(order = 3)]
    pub sra_address_count: Map<u32, u32>,

    /// The sum of the WAA offer counts, per emission day.
    #[attribute(order = 4)]
    pub waa_total_count: Map<u32, u64>,

    /// The sum of the SRA offer counts, per emission day.
    #[attribute(order = 5)]
    pub sra_total_count: Map<u32, u64>,

    /// The rate record of each pool and day (key: `pool_day_key`).
    #[attribute(order = 6)]
    pub pool_days: Map<u64, PoolDay>,
}

/// The reward of one pool and emission day after its rate is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[storage_record(exists_field = deadline)]
pub struct PoolDay {
    #[key]
    pub key: u64,
    /// The reward for one unit of weight, with 18 decimal places.
    #[attribute(order = 0)]
    pub rate: U256,
    /// The part of the allocated amount that no claim or expiry took yet.
    #[attribute(order = 1)]
    pub unclaimed_minor: U256,
    /// One deadline for all receivers: the time the rate was set plus 365 days.
    #[attribute(order = 2)]
    pub deadline: u64,
    /// The receivers whose weight is not claimed or expired yet.
    #[attribute(order = 3)]
    pub receivers: u32,
}

/// The reward pool of an agent type. The values are the ABI pool numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RewardPool {
    Waa = 0,
    Sra = 1,
    Cca = 2,
    Validator = 3,
}

impl RewardPool {
    pub fn from_abi(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Waa),
            1 => Ok(Self::Sra),
            2 => Ok(Self::Cca),
            3 => Ok(Self::Validator),
            _ => Err(AgentRewardError::UnknownRewardPool(value).into()),
        }
    }

    pub fn emission_pool(self) -> outbe_emission::api::RewardPool {
        match self {
            Self::Waa => outbe_emission::api::RewardPool::Waa,
            Self::Sra => outbe_emission::api::RewardPool::Sra,
            Self::Cca => outbe_emission::api::RewardPool::Cca,
            Self::Validator => outbe_emission::api::RewardPool::Validator,
        }
    }
}

/// The offer index of the WAA or the SRA pool.
pub(crate) struct OfferIndex<'a, 's> {
    pub(crate) counts: &'a Map<'s, B256, u64>,
    pub(crate) address_count: &'a Map<'s, u32, u32>,
    pub(crate) total: &'a Map<'s, u32, u64>,
}

impl<'s> AgentRewardContract<'s> {
    pub(crate) fn offer_index(&self, pool: RewardPool) -> Result<OfferIndex<'_, 's>> {
        match pool {
            RewardPool::Waa => Ok(OfferIndex {
                counts: &self.waa_offer_counts,
                address_count: &self.waa_address_count,
                total: &self.waa_total_count,
            }),
            RewardPool::Sra => Ok(OfferIndex {
                counts: &self.sra_offer_counts,
                address_count: &self.sra_address_count,
                total: &self.sra_total_count,
            }),
            RewardPool::Cca | RewardPool::Validator => {
                Err(AgentRewardError::UnknownRewardPool(pool as u8).into())
            }
        }
    }

    /// The key of the offer count of `address` for the Nods of emission day `day`.
    pub fn offer_count_key(day: u32, address: Address) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..4].copy_from_slice(&day.to_be_bytes());
        buf[4..24].copy_from_slice(address.as_slice());
        alloy_primitives::keccak256(buf)
    }

    /// The key of the rate record of `pool` and emission day `day`.
    pub fn pool_day_key(pool: RewardPool, day: u32) -> u64 {
        (u64::from(day) << 8) | u64::from(pool as u8)
    }
}
