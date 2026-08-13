use alloy_primitives::{Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::GEM_ADDRESS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GemState {
    Issued = 0,
    Qualified = 1,
    Called = 2,
    Settled = 3,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GemAddParams {
    pub owner: Address,
    pub gem_type: u8,
    pub gem_load_minor: U256,
    pub entry_price_minor: U256,
    pub cost_amount_minor: U256,
    pub floor_price_minor: U256,
    pub call_price_minor: U256,
    pub call_rate: u16,
    pub call_window: u32,
    pub call_threshold: u32,
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
    pub gem_load_minor: U256,

    #[attribute(order = 3)]
    pub entry_price_minor: U256,

    #[attribute(order = 4)]
    pub cost_amount_minor: U256,

    #[attribute(order = 5)]
    pub floor_price_minor: U256,

    #[attribute(order = 6)]
    pub issuance_currency: u16,

    #[attribute(order = 7)]
    pub reference_currency: u16,

    #[attribute(order = 8)]
    pub state: u8,

    #[attribute(order = 9)]
    pub issued_at: u64,

    /// Coen price level (Reference Currency) whose breach arms a Call Event.
    /// `entry_price_minor * (1 + call_rate)`; call rate is 128% for agent gems.
    #[attribute(order = 10)]
    pub call_price_minor: U256,

    /// Block timestamp when the gem was force-called; `0` until Called.
    #[attribute(order = 11, default = 0)]
    pub called_at: u64,

    /// Call Notice Period in seconds: after a Called gem passes
    /// `called_at + call_notice_period` it is forfeit-burned. Snapshot of the
    /// protocol constant at issuance.
    #[attribute(order = 12, default = 0)]
    pub call_notice_period: u32,

    /// Call-price markup percent (snapshot of `CALL_RATE` at issuance);
    /// `call_price_minor = entry_price_minor * (100 + call_rate) / 100`
    /// (128 => 2.28x).
    #[attribute(order = 13, default = 0)]
    pub call_rate: u16,

    /// Call-trigger evaluation window in seconds (snapshot of `CALL_WINDOW` at
    /// issuance); the trailing span scanned for Call Price breaches.
    #[attribute(order = 14, default = 0)]
    pub call_window: u32,

    /// Breach threshold in seconds (snapshot of `CALL_THRESHOLD` at issuance);
    /// divided by 86400 to get the required breach-day count.
    #[attribute(order = 15, default = 0)]
    pub call_threshold: u32,

    /// Block timestamp when the gem became Qualified; `0` until Qualified.
    #[attribute(order = 16, default = 0)]
    pub qualified_at: u64,

    /// Block timestamp when the gem was Settled; `0` until Settled.
    #[attribute(order = 17, default = 0)]
    pub settled_at: u64,
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
    //
    // A floor price is only comparable to the COEN rate of its own reference
    // currency, so every column here is namespaced by ISO code and each currency
    // walks an independent trie. See `state::CurrencyBins`.
    #[attribute(order = 6)]
    pub bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,

    // Keyed by `state::scoped(iso, trie key)`.
    #[attribute(order = 7)]
    pub bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,

    #[attribute(order = 8)]
    pub bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,

    #[attribute(order = 9)]
    pub unqualified_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,

    #[attribute(order = 10)]
    pub unqualified_bin_gems: outbe_primitives::storage::dsl::Map<B256, U256>,

    // --- Callable-gem index: dense list of gems in Qualified or Called state,
    // the only gems the daily Called scan needs to visit. Membership invariant:
    // a gem is listed iff its state is Qualified or Called. Maintained by
    // add_gem / set_state / burn. `callable_gem_index` maps gem_id -> position
    // for O(1) swap-remove.
    #[attribute(order = 11)]
    pub callable_gems: outbe_primitives::storage::dsl::List<U256>,

    #[attribute(order = 12)]
    pub callable_gem_index: outbe_primitives::storage::dsl::Map<U256, u32>,

    /// Next bin the qualify scan visits, per reference currency. Non-zero only
    /// while a sweep was cut short by the per-block budget.
    #[attribute(order = 13)]
    pub qualify_scan_cursor: outbe_primitives::storage::dsl::Map<u16, u32>,
}

impl GemContract<'_> {
    /// `gem_id = keccak256("gem" ‖ owner ‖ amount_be ‖ block_number_be)`.
    /// `amount` is the gem's `gem_load_minor` (reward principal).
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
