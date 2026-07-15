use alloy_primitives::{Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::NOD_ADDRESS;
use outbe_primitives::storage::types::StorageKey;
use serde::{Deserialize, Serialize};

/// Input for `NodContract::issue`. `nod_id` is derived inside the contract via
/// `NodContract::nod_id(owner, worldwide_day)`; `cost_amount_minor` is computed
/// from `cost_of_gratis_minor * gratis_load_minor / SCALE_1E18`. `issued_at` is
/// stamped inside `issue` from the current block timestamp and is not part of
/// caller inputs.
#[derive(Debug, Clone, PartialEq)]
pub struct NodIssueParams {
    pub owner: Address,
    pub worldwide_day: WorldwideDay,
    pub league_id: u16,
    pub floor_price_minor: U256,
    pub gratis_load_minor: U256,
    pub entry_price_minor: U256,
    pub cost_amount_minor: U256,
    pub issuance_currency: u16,
    /// Reference currency (ISO 4217 numeric) propagated from the originating
    /// Tribute. Used for off-chain pricing references on the Nod.
    pub reference_currency: u16,
}

#[derive(Serialize, Deserialize)]
#[storage_record(exists_field = owner)]
pub struct NodItemState {
    #[key]
    pub nod_id: U256,

    #[attribute(order = 0)]
    pub owner: Address,

    #[attribute(order = 1)]
    pub gratis_load_minor: U256,

    #[attribute(order = 2)]
    pub worldwide_day: WorldwideDay,

    #[attribute(order = 3)]
    pub league_id: u16,

    #[attribute(order = 4)]
    pub floor_price_minor: U256,

    #[attribute(order = 5)]
    pub bucket_key: B256,

    #[attribute(order = 6)]
    pub cost_amount_minor: U256,

    #[attribute(order = 7)]
    pub issuance_currency: u16,

    #[attribute(order = 8)]
    pub reference_currency: u16,

    #[attribute(order = 9)]
    pub issued_at: u64,
}

/// Bucket record exists while `total_nods > 0`; when the last NOD in the bucket is mined,
/// `nod_buckets.delete(bucket_key)` drops the entry.
#[derive(Serialize, Deserialize)]
#[storage_record(exists_field = total_nods)]
pub struct NodBucketState {
    #[key]
    pub bucket_key: B256,

    #[attribute(order = 0)]
    pub worldwide_day: WorldwideDay,

    #[attribute(order = 1)]
    pub floor_price_minor: U256,

    #[attribute(order = 2)]
    pub is_qualified: bool,

    #[attribute(order = 3)]
    pub total_nods: u64,

    #[attribute(order = 4)]
    pub entry_price_minor: U256,
}

/// EVM storage layout for the Nod NFT contract.
///
/// NodItem fields keyed by nod_id (U256).
/// NodBucketState keyed by bucket_key (B256).
/// Bucket key = keccak256(abi.encode(worldwide_day, floor_price_minor)).
///
/// Unqualified buckets are tracked by a PancakeSwap-Liquidity-Book-style
/// 3-level radix-256 bitmap trie indexed by `floor_price_minor`. Each set
/// leaf bit identifies a non-empty price bin; per-bin bucket lists live in
/// `unqualified_bin_count` + `unqualified_bin_buckets`. See
/// `crates/core/nod/src/bin_tree.rs` for the LB-port traversal helpers.
#[storage_schema]
#[contract(addr = NOD_ADDRESS)]
pub struct NodContract {
    // slot 0: total nod items
    #[attribute(order = 0)]
    pub total_supply: outbe_primitives::storage::dsl::Value<u64>,

    // slots 1-10: item state record keyed by nod_id
    #[attribute(order = 1)]
    pub(crate) nod_items: outbe_primitives::storage::dsl::Map<U256, NodItemState>,

    // slots 11-15: bucket state record keyed by bucket_key
    #[attribute(order = 2)]
    pub(crate) nod_buckets: outbe_primitives::storage::dsl::Map<B256, NodBucketState>,

    // --- Enumeration indexes ---
    // slot 16: owner → count of nods ever issued
    #[attribute(order = 4)]
    pub owner_nod_counts: outbe_primitives::storage::dsl::Map<Address, u32>,

    // slot 17: per-owner nod index — keccak(owner ++ index) → nod_id
    #[attribute(order = 5)]
    pub owner_nod_ids: outbe_primitives::storage::dsl::Map<B256, U256>,

    // --- Unqualified-bucket bin index (PancakeSwap LB-style trie) ---

    // slot 18: top-level 256-bit bitmap. Bit `i` is set iff `bin_tree_mid[i]`
    // is non-zero. Indexed by bits [16:24] of bin_id.
    #[attribute(order = 10)]
    pub bin_tree_root: outbe_primitives::storage::dsl::Value<U256>,

    // slot 19: mid-level bitmaps. Key = bits [16:24] of bin_id (kept as u32
    // because StorageKey is only impl'd for u32/u64/U256 in this workspace).
    // Bit `j` is set iff `bin_tree_leaf[(key << 8) | j]` is non-zero.
    #[attribute(order = 11)]
    pub bin_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,

    // slot 20: leaf-level bitmaps. Key = bits [8:24] of bin_id (u16 worth of
    // address space, encoded as u32 for StorageKey). Bit `k` is set iff bin
    // `(key << 8) | k` currently holds at least one bucket_key.
    #[attribute(order = 12)]
    pub bin_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,

    // slot 21: per-bin count of bucket_keys parked in the bin.
    #[attribute(order = 13)]
    pub unqualified_bin_count: outbe_primitives::storage::dsl::Map<u32, u32>,

    // slot 22: per-bin bucket index — keccak(bin_id ++ index) → bucket_key.
    // Insertion-ordered; on qualification, the bin is either drained
    // wholesale (count := 0, bit cleared) or compacted (survivors moved up).
    #[attribute(order = 14)]
    pub unqualified_bin_buckets: outbe_primitives::storage::dsl::Map<B256, B256>,

    // --- Global enumeration (ERC-721 Enumerable) ---

    // slot 23: dense global list — global_nod_ids[i] = nod_id for i in
    // [0, total_supply). Kept gap-free via swap-on-delete in mine_gratis.
    #[attribute(order = 15)]
    pub global_nod_ids: outbe_primitives::storage::dsl::List<U256>,

    // slot 24: reverse lookup nod_id → its current index in global_nod_ids.
    #[attribute(order = 16)]
    pub global_nod_index: outbe_primitives::storage::dsl::Map<U256, u32>,
}

impl NodContract<'_> {
    /// Computes the bucket key from (worldwide_day, floor_price_minor).
    pub fn bucket_key(worldwide_day: WorldwideDay, floor_price_minor: U256) -> B256 {
        use alloy_primitives::keccak256;
        let mut buf = [0u8; 36];
        buf[0..4].copy_from_slice(worldwide_day.key_bytes().as_slice());
        buf[4..36].copy_from_slice(&floor_price_minor.to_be_bytes::<32>());
        keccak256(buf)
    }

    /// Domain-separated deterministic NOD id derived from (owner, worldwide_day).
    /// The `b"nod"` prefix prevents collisions with other keccak-based ids in
    /// the chain (e.g., bucket keys, tribute token ids).
    pub fn generate_nod_id(owner: Address, worldwide_day: WorldwideDay) -> U256 {
        use alloy_primitives::keccak256;
        let mut buf = [0u8; 3 + 20 + 4];
        buf[0..3].copy_from_slice(b"nod");
        buf[3..23].copy_from_slice(owner.as_slice());
        buf[23..27].copy_from_slice(worldwide_day.key_bytes().as_slice());
        let hash = keccak256(buf);
        U256::from_be_bytes(hash.0)
    }
}
