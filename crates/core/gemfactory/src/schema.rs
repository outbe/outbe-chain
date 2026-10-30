use alloy_primitives::{Address, B256, U256};
use outbe_intex::SeriesId;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::GEM_FACTORY_ADDRESS;

/// The terms a reward gem is issued on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GemIssueParams {
    pub owner: Address,
    pub promis_load_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub entry_price_minor: U256,
    /// The price the call rate marks up. The entry may sit below it.
    pub call_anchor_price_minor: U256,
}

/// One merchant issuance, authorized by the position owner.
#[derive(Debug, Clone, Copy)]
pub struct MerchantGemIssueParams {
    pub caller: Address,
    pub position_id: U256,
    pub owner: Address,
    pub promis_load_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
}

/// The proof of work submitted for a settled Gem.
#[derive(Debug, Clone, Copy)]
pub struct GemMiningProof {
    pub gem_id: U256,
    pub nonce: u64,
}

/// A merchant's Gem Factory position: the pool of Promis capacity from which the
/// Gem Factory issues Merchant gems. The position is a single-owner, non-transferable
/// NFT (owner = `merchant`), keyed by `position_id`. `merchant == 0` means "no
/// position".
#[storage_record(exists_field = merchant)]
pub struct GemPosition {
    #[key]
    pub position_id: U256,

    #[attribute(order = 0)]
    pub merchant: Address,

    #[attribute(order = 1)]
    pub source_series_id: SeriesId,

    /// Remaining Promis capacity. Each issue drains it by `promis_load_minor`.
    #[attribute(order = 2)]
    pub remaining_capacity_minor: U256,

    /// When the position was issued.
    #[attribute(order = 3)]
    pub issued_at: u64,

    /// Deadline snapshotted from the profile when the position opened, so a later profile
    /// change cannot retroactively expire positions already in the queue. Positions
    /// expire in groups by its hour.
    #[attribute(order = 4)]
    pub expires_at: u64,
}

impl GemPosition {
    /// Hourly position membership is independent of daily queue scheduling.
    pub const GROUP_HOUR_SECONDS: u64 = 3_600;

    /// The expiry group: the hour `expires_at` falls in.
    #[must_use]
    pub fn expiry_hour(&self) -> u64 {
        self.expires_at / Self::GROUP_HOUR_SECONDS
    }
}

#[storage_schema]
#[contract(addr = GEM_FACTORY_ADDRESS)]
pub struct GemFactoryContract {
    #[attribute(order = 0)]
    pub total_gems_issued: outbe_primitives::storage::dsl::Value<U256>,

    #[attribute(order = 1)]
    pub total_capacity_minor: outbe_primitives::storage::dsl::Value<U256>,

    #[attribute(order = 2)]
    pub positions: outbe_primitives::storage::dsl::Map<U256, GemPosition>,

    /// Each merchant's positions.
    #[attribute(order = 3)]
    pub owner_position_ids:
        outbe_primitives::storage::dsl::Map<Address, outbe_primitives::storage::dsl::List<U256>>,
    #[attribute(order = 4)]
    pub owner_position_index: outbe_primitives::storage::dsl::Map<U256, u32>,

    /// The positions of each expiry hour, and the capacity they have left to issue.
    #[attribute(order = 5)]
    pub hour_positions:
        outbe_primitives::storage::dsl::Map<u64, outbe_primitives::storage::dsl::List<U256>>,
    #[attribute(order = 6)]
    pub hour_position_index: outbe_primitives::storage::dsl::Map<U256, u32>,
    #[attribute(order = 7)]
    pub hour_capacity_minor: outbe_primitives::storage::dsl::Map<u64, U256>,

    // The queue stores hour groups as `hour + 1` and schedules them by UTC deadline day.
    #[attribute(order = 8)]
    pub expiry_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 9)]
    pub expiry_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 10)]
    pub expiry_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 11)]
    pub expiry_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 12)]
    pub expiry_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 13)]
    pub expiry_day_at: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 14)]
    pub expiry_entry_slot: outbe_primitives::storage::dsl::Map<u64, u64>,
    #[attribute(order = 15)]
    pub expiry_deadline: outbe_primitives::storage::dsl::Map<u64, u64>,
    #[attribute(order = 16)]
    pub expiry_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 17)]
    pub expiry_cursor: outbe_primitives::storage::dsl::Value<u64>,

    // This queue stores encoded expired hour groups whose positions await deletion.
    #[attribute(order = 18)]
    pub cleanup_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 19)]
    pub cleanup_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 20)]
    pub cleanup_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 21)]
    pub cleanup_day_len: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 22)]
    pub cleanup_day_live: outbe_primitives::storage::dsl::Map<u32, u32>,
    #[attribute(order = 23)]
    pub cleanup_day_at: outbe_primitives::storage::dsl::Map<B256, u64>,
    #[attribute(order = 24)]
    pub cleanup_entry_slot: outbe_primitives::storage::dsl::Map<u64, u64>,
    #[attribute(order = 25)]
    pub cleanup_deadline: outbe_primitives::storage::dsl::Map<u64, u64>,
    #[attribute(order = 26)]
    pub cleanup_sweep_day: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 27)]
    pub cleanup_cursor: outbe_primitives::storage::dsl::Value<u64>,
    #[attribute(order = 28)]
    pub cleanup_budget: outbe_primitives::storage::dsl::Value<U256>,
}

impl GemFactoryContract<'_> {
    /// `keccak256(source_series_id || merchant || block_number_be)`. A series is held by many
    /// owners and any may send units, so the merchant tells two positions of one block apart.
    pub fn generate_position_id(
        source_series_id: SeriesId,
        merchant: Address,
        block_number: u64,
    ) -> U256 {
        source_series_id.right_id(&[merchant.as_slice(), &block_number.to_be_bytes()])
    }
}
