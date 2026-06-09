use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::GEM_ADDRESS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GemState {
    Issued = 0,
    Qualified = 1,
    Settled = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GemAddParams {
    pub owner: Address,
    pub gem_type: u8,
    pub gem_load: U256,
    pub entry_price: U256,
    pub cost_amount: U256,
    pub floor_price: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub initial_state: GemState,
    pub issued_at: u64,
}

#[storage_record(exists_field = owner)]
pub struct GemData {
    #[key]
    pub gem_id: U256,

    #[attribute(order = 0)]
    pub owner: Address,

    #[attribute(order = 1)]
    pub gem_type: u8,

    #[attribute(order = 2)]
    pub gem_load: U256,

    #[attribute(order = 3)]
    pub entry_price: U256,

    #[attribute(order = 4)]
    pub cost_amount: U256,

    #[attribute(order = 5)]
    pub floor_price: U256,

    #[attribute(order = 6)]
    pub issuance_currency: u16,

    #[attribute(order = 7)]
    pub reference_currency: u16,

    #[attribute(order = 8)]
    pub state: u8,

    #[attribute(order = 9)]
    pub issued_at: u64,
}

#[storage_schema]
#[contract(addr = GEM_ADDRESS)]
pub struct GemContract {
    #[attribute(order = 0)]
    pub total_supply: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 1)]
    pub gem_items: outbe_primitives::storage::dsl::Map<U256, GemData>,

    #[attribute(order = 2)]
    pub owner_gem_counts: outbe_primitives::storage::dsl::Map<Address, u32>,

    #[attribute(order = 3)]
    pub owner_gem_ids: outbe_primitives::storage::dsl::Map<B256, U256>,

    #[attribute(order = 4)]
    pub all_gem_ids: outbe_primitives::storage::dsl::List<U256>,

    #[attribute(order = 5)]
    pub gem_index: outbe_primitives::storage::dsl::Map<U256, u32>,

    // --- Unqualified-gem bin index (PancakeSwap LB-style 3-level radix-256 trie) ---
    #[attribute(order = 6)]
    pub bin_tree_root: outbe_primitives::storage::dsl::Value<U256>,

    #[attribute(order = 7)]
    pub bin_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,

    #[attribute(order = 8)]
    pub bin_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,

    #[attribute(order = 9)]
    pub unqualified_bin_count: outbe_primitives::storage::dsl::Map<u32, u32>,

    #[attribute(order = 10)]
    pub unqualified_bin_gems: outbe_primitives::storage::dsl::Map<B256, U256>,
}

impl GemContract<'_> {
    /// `gem_id = keccak256("gem" ‖ owner ‖ amount_be ‖ block_number_be)`.
    /// `amount` is the gem's `gem_load` (reward principal).
    pub fn generate_gem_id(owner: Address, amount: U256, block_number: u64) -> U256 {
        use alloy_primitives::keccak256;
        let mut buf = [0u8; 3 + 20 + 32 + 8];
        buf[0..3].copy_from_slice(b"gem");
        buf[3..23].copy_from_slice(owner.as_slice());
        buf[23..55].copy_from_slice(&amount.to_be_bytes::<32>());
        buf[55..63].copy_from_slice(&block_number.to_be_bytes());
        U256::from_be_bytes(keccak256(buf).0)
    }
}
