use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::AGENT_REWARD_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::dsl::Map;
use outbe_primitives::storage::types::StorageKey;
use outbe_primitives::time::WorldwideDay;

use crate::errors::AgentRewardError;

/// EVM storage layout for the Agent Reward contract.
///
/// Tracks tribute counts per address/reward UTC day, claimable reward balances
/// per address and pool, and per-day address lists for the WAA (wallet) and SRA
/// (signer-of-record) participant pools. A reward day is the calendar day when
/// an offer executes, not the Tribute's target WorldwideDay.
///
/// Naming history: Phase 2 of the Cycle epic renamed `wallet_*` to `waa_*`
/// and `sfa_*` to `sra_*`, so the on-chain identifiers match the README and
/// external documentation.
#[storage_schema]
#[contract(addr = AGENT_REWARD_ADDRESS)]
pub struct AgentRewardContract {
    // slot 0: WAA tribute count per day+address (key: B256 = keccak(day, addr))
    #[attribute(order = 0)]
    pub waa_tribute_counts: outbe_primitives::storage::dsl::Map<B256, u64>,

    // slot 1: SRA tribute count per day+address (key: B256 = keccak(day, addr))
    #[attribute(order = 1)]
    pub sra_tribute_counts: outbe_primitives::storage::dsl::Map<B256, u64>,

    // slot 2: claimable WAA reward per address
    #[attribute(order = 2)]
    pub waa_claimable_minor: outbe_primitives::storage::dsl::Map<Address, U256>,

    // slot 3: WAA address count per day
    #[attribute(order = 3)]
    pub waa_address_count: outbe_primitives::storage::dsl::Map<WorldwideDay, u32>,

    // slot 4: WAA address list - key = keccak(day, index), value = address
    #[attribute(order = 4)]
    pub waa_addresses: outbe_primitives::storage::dsl::Map<B256, Address>,

    // slot 5: SRA address count per day
    #[attribute(order = 5)]
    pub sra_address_count: outbe_primitives::storage::dsl::Map<WorldwideDay, u32>,

    // slot 6: SRA address list - key = keccak(day, index), value = address
    #[attribute(order = 6)]
    pub sra_addresses: outbe_primitives::storage::dsl::Map<B256, Address>,

    // slot 7: claimable SRA reward per address
    #[attribute(order = 7)]
    pub sra_claimable_minor: outbe_primitives::storage::dsl::Map<Address, U256>,

    // slot 8: claimable CCA reward per address
    #[attribute(order = 8)]
    pub cca_claimable_minor: outbe_primitives::storage::dsl::Map<Address, U256>,

    // slot 9: claimable validator reward per address
    #[attribute(order = 9)]
    pub validator_claimable_minor: outbe_primitives::storage::dsl::Map<Address, U256>,

    // slot 10: WAA tribute count per day, summed over its addresses
    #[attribute(order = 10)]
    pub waa_total_count: outbe_primitives::storage::dsl::Map<WorldwideDay, u64>,

    // slot 11: SRA tribute count per day, summed over its addresses
    #[attribute(order = 11)]
    pub sra_total_count: outbe_primitives::storage::dsl::Map<WorldwideDay, u64>,

    // slot 12: queued WAA and SRA day pools, by sequence number
    #[attribute(order = 12)]
    pub accrual_jobs: outbe_primitives::storage::dsl::Map<u64, AccrualJob>,

    // slot 13: sequence number of the job credited next
    #[attribute(order = 13)]
    pub accrual_head: outbe_primitives::storage::dsl::Value<u64>,

    // slot 14: sequence number the next queued job takes
    #[attribute(order = 14)]
    pub accrual_tail: outbe_primitives::storage::dsl::Value<u64>,
}

/// One settled WAA or SRA day pool. Blocks credit its addresses in list order,
/// a bounded slice at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = day)]
pub struct AccrualJob {
    #[key]
    pub seq: u64,
    /// UTC day key (YYYYMMDD) whose tribute counts weight the pool.
    #[attribute(order = 0)]
    pub day: u32,
    /// `RewardPool::Waa` or `RewardPool::Sra`.
    #[attribute(order = 1)]
    pub pool: u8,
    #[attribute(order = 2)]
    pub pool_minor: U256,
    /// The day's tribute count over all addresses of the pool.
    #[attribute(order = 3)]
    pub total_weight: u64,
    /// Index of the next address in the day list.
    #[attribute(order = 4)]
    pub next_index: u32,
    #[attribute(order = 5)]
    pub credited_minor: U256,
}

/// Pool a claimable balance belongs to. The pool decides the Gem class a
/// claim issues, so the contract keeps the pool balances apart.
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
}

/// The per-day tribute index of the WAA or the SRA pool.
pub(crate) struct DayIndex<'a, 's> {
    pub(crate) address_count: &'a Map<'s, WorldwideDay, u32>,
    pub(crate) addresses: &'a Map<'s, B256, Address>,
    pub(crate) counts: &'a Map<'s, B256, u64>,
    pub(crate) total: &'a Map<'s, WorldwideDay, u64>,
}

impl<'s> AgentRewardContract<'s> {
    /// The day index of a pool weighted by tribute counts.
    pub(crate) fn day_index(&self, pool: RewardPool) -> Result<DayIndex<'_, 's>> {
        match pool {
            RewardPool::Waa => Ok(DayIndex {
                address_count: &self.waa_address_count,
                addresses: &self.waa_addresses,
                counts: &self.waa_tribute_counts,
                total: &self.waa_total_count,
            }),
            RewardPool::Sra => Ok(DayIndex {
                address_count: &self.sra_address_count,
                addresses: &self.sra_addresses,
                counts: &self.sra_tribute_counts,
                total: &self.sra_total_count,
            }),
            RewardPool::Cca | RewardPool::Validator => {
                Err(AgentRewardError::UnknownRewardPool(pool as u8).into())
            }
        }
    }
}

impl AgentRewardContract<'_> {
    /// Computes the composite key for per-address per-day tribute counts.
    pub fn tribute_count_key(day: WorldwideDay, address: Address) -> B256 {
        use alloy_primitives::keccak256;
        let mut buf = [0u8; 24]; // 4 + 20
        buf[0..4].copy_from_slice(day.key_bytes().as_slice());
        buf[4..24].copy_from_slice(address.as_slice());
        keccak256(buf)
    }

    /// Computes the composite key for per-day address index lists.
    pub fn address_index_key(day: WorldwideDay, index: u32) -> B256 {
        use alloy_primitives::keccak256;
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(day.key_bytes().as_slice());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }
}
