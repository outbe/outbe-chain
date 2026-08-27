use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_intex::{SeriesId, SERIES_ID_LEN};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::GEM_FACTORY_ADDRESS;

pub use outbe_gem::GemTypes;

/// A merchant's parked-Intex position: the pool of Promis capacity from which
/// Merchant gems are issued. Modeled as a single-owner, non-transferable NFT
/// (owner = `merchant`), keyed by `position_id`. `merchant == 0` means "no
/// position".
#[storage_record(exists_field = merchant)]
pub struct GemPosition {
    #[key]
    pub position_id: U256,

    #[attribute(order = 0)]
    pub merchant: Address,

    #[attribute(order = 1)]
    pub source_intex_id: SeriesId,

    /// Remaining Promis capacity; drains by `gem_load` on each issue.
    #[attribute(order = 2)]
    pub remaining_capacity: U256,

    /// Snapshot of the parked Intex entry/floor, used as issuance lower bounds.
    #[attribute(order = 3)]
    pub source_entry_price: U256,

    #[attribute(order = 4)]
    pub source_floor_price: U256,

    #[attribute(order = 5)]
    pub issuance_currency: u16,

    #[attribute(order = 6)]
    pub reference_currency: u16,

    #[attribute(order = 7)]
    pub parked_at: u64,
}

#[storage_schema]
#[contract(addr = GEM_FACTORY_ADDRESS)]
pub struct GemFactoryContract {
    #[attribute(order = 0)]
    pub total_gems_issued: outbe_primitives::storage::dsl::Value<U256>,

    #[attribute(order = 1)]
    pub total_intex_parked: outbe_primitives::storage::dsl::Value<U256>,

    #[attribute(order = 2)]
    pub positions: outbe_primitives::storage::dsl::Map<U256, GemPosition>,

    // --- Position-NFT owner index (per-merchant enumeration) ---
    #[attribute(order = 3)]
    pub position_owner_counts: outbe_primitives::storage::dsl::Map<Address, u32>,

    #[attribute(order = 4)]
    pub position_owner_ids: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// Dense index of the positions still holding reclaimable capacity.
    /// Membership invariant: a position is listed until the expiry sweep
    /// retires it, so the sweep visits only positions that can still return
    /// capacity instead of the whole book.
    #[attribute(order = 5)]
    pub active_positions: outbe_primitives::storage::dsl::List<U256>,

    /// position_id → its slot in [`Self::active_positions`], for O(1) swap-remove.
    #[attribute(order = 6)]
    pub active_position_index: outbe_primitives::storage::dsl::Map<U256, u32>,

    /// Resume point of the expiry sweep, stored as `index + 1`; `0` means
    /// "start a fresh pass from the top".
    #[attribute(order = 7)]
    pub expiry_scan_cursor: outbe_primitives::storage::dsl::Value<u32>,
}

impl GemFactoryContract<'_> {
    /// `position_id = keccak256("gemposition" ‖ source_intex_id_be ‖ block_number_be)`.
    /// A source Intex is parked once, so `source_intex_id` alone disambiguates.
    pub fn generate_position_id(source_intex_id: SeriesId, block_number: u64) -> U256 {
        let mut buf = [0u8; 11 + SERIES_ID_LEN + 8];
        buf[0..11].copy_from_slice(b"gemposition");
        buf[11..11 + SERIES_ID_LEN].copy_from_slice(source_intex_id.as_bytes());
        buf[11 + SERIES_ID_LEN..].copy_from_slice(&block_number.to_be_bytes());
        U256::from_be_bytes(keccak256(buf).0)
    }
}
