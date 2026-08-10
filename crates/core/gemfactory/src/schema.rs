use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_intex::SeriesId;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::GEM_FACTORY_ADDRESS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GemTypes {
    Genesis = 0,
    Validator = 1,
    Sra = 2,
    Wallet = 3,
    Cca = 4,
    Merchant = 5,
}

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
}

impl GemFactoryContract<'_> {
    /// `position_id = keccak256("gemposition" ‖ source_intex_id_be ‖ block_number_be)`.
    /// A source Intex is parked once, so `source_intex_id` alone disambiguates.
    pub fn generate_position_id(source_intex_id: SeriesId, block_number: u64) -> U256 {
        let mut buf = [0u8; 11 + 8 + 8];
        buf[0..11].copy_from_slice(b"gemposition");
        buf[11..19].copy_from_slice(&source_intex_id.value().to_be_bytes());
        buf[19..27].copy_from_slice(&block_number.to_be_bytes());
        U256::from_be_bytes(keccak256(buf).0)
    }
}
