use alloy_primitives::{Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::AGENT_REWARD_ADDRESS;
use outbe_primitives::storage::types::StorageKey;

/// EVM storage layout for the Agent Reward contract.
///
/// Tracks tribute counts per address/day, claimable reward balances per
/// address, and per-day address lists for the WAA (wallet) and SRA
/// (signer-of-record) participant pools.
///
/// Naming history: `wallet_*` was renamed to `waa_*` and `sfa_*` to
/// `sra_*` as part (Phase 2 of the Cycle epic) so the on-chain
/// identifiers match the README and external documentation.
#[storage_schema]
#[contract(addr = AGENT_REWARD_ADDRESS)]
pub struct AgentRewardContract {
    // slot 0: WAA tribute count per day+address (key: B256 = keccak(day, addr))
    #[attribute(order = 0)]
    pub waa_tribute_counts: outbe_primitives::storage::dsl::Map<B256, u64>,

    // slot 1: SRA tribute count per day+address (key: B256 = keccak(day, addr))
    #[attribute(order = 1)]
    pub sra_tribute_counts: outbe_primitives::storage::dsl::Map<B256, u64>,

    // slot 2: claimable reward per address
    #[attribute(order = 2)]
    pub claimable_rewards: outbe_primitives::storage::dsl::Map<Address, U256>,

    // slot 3: WAA address count per day
    #[attribute(order = 3)]
    pub waa_address_count: outbe_primitives::storage::dsl::Map<WorldwideDay, u32>,

    // slot 4: WAA address list — key = keccak(day, index), value = address
    #[attribute(order = 4)]
    pub waa_addresses: outbe_primitives::storage::dsl::Map<B256, Address>,

    // slot 5: SRA address count per day
    #[attribute(order = 5)]
    pub sra_address_count: outbe_primitives::storage::dsl::Map<WorldwideDay, u32>,

    // slot 6: SRA address list — key = keccak(day, index), value = address
    #[attribute(order = 6)]
    pub sra_addresses: outbe_primitives::storage::dsl::Map<B256, Address>,
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
