//! Storage schema for IntexFactory: settlement bookkeeping and the
//! unqualified-series bin index. Canonical series state lives in Intex.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;

/// Issuance inputs captured on Outbe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuanceParams {
    pub series_id: u32,
    pub issued_intex_count: u32,
    pub promis_load_minor: u128,
    /// Entry price (per-unit, reference currency, 1e18 oracle scale); cost/floor/call derive from it.
    pub entry_price_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    /// Auction winners: per-address mint recipients for ISSUANCE_INSTRUCTIONS.
    pub recipients: Vec<Address>,
    pub quantities: Vec<U256>,
}

/// EVM storage layout: settlement bookkeeping (authorized_settler, settle_count,
/// mine_seq) and the unqualified-series floor-bin index.
#[storage_schema]
#[contract(addr = INTEX_FACTORY_ADDRESS)]
pub struct IntexFactoryContract {
    /// `keccak256(holder ++ series_id_be32)` -> authorized settler address.
    #[attribute(order = 0)]
    pub authorized_settler: outbe_primitives::storage::dsl::Map<B256, Address>,

    /// series_id -> cumulative settle count.
    #[attribute(order = 1)]
    pub settle_count: outbe_primitives::storage::dsl::Map<u32, U256>,

    /// `keccak256(series_id_be32 ++ holder)` -> monotonic minePromis sequence.
    #[attribute(order = 2)]
    pub mine_seq: outbe_primitives::storage::dsl::Map<B256, u32>,

    // Unqualified-series bin index (by floor_price_minor) for begin_block qualify.
    #[attribute(order = 3)]
    pub bin_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 4)]
    pub bin_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 5)]
    pub bin_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    /// bin_id -> count of series in the bin.
    #[attribute(order = 6)]
    pub unqualified_bin_count: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// `keccak256(bin_id_be32 ++ index_be32)` -> series_id.
    #[attribute(order = 7)]
    pub unqualified_bin_series: outbe_primitives::storage::dsl::Map<B256, u32>,

    // Qualified-series bin index (by call_price_minor) for the daily
    // Called scan. A series moves here from the unqualified index on qualify.
    #[attribute(order = 8)]
    pub qualified_bin_tree_root: outbe_primitives::storage::dsl::Value<U256>,
    #[attribute(order = 9)]
    pub qualified_bin_tree_mid: outbe_primitives::storage::dsl::Map<u32, U256>,
    #[attribute(order = 10)]
    pub qualified_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u32, U256>,
    /// bin_id -> count of series in the bin.
    #[attribute(order = 11)]
    pub qualified_bin_count: outbe_primitives::storage::dsl::Map<u32, u32>,
    /// `keccak256(bin_id_be32 ++ index_be32)` -> series_id.
    #[attribute(order = 12)]
    pub qualified_bin_series: outbe_primitives::storage::dsl::Map<B256, u32>,

    // Genesis parameter-profile selector (0 = prod, 1 = dev); see crate::config.
    #[attribute(order = 13)]
    pub config_profile: outbe_primitives::storage::dsl::Value<u8>,

    // Begin-block qualify-scan cursor: unqualified bin to resume from next block so per-block
    // work is capped (OIP-00151). 0 = start a fresh sweep.
    #[attribute(order = 14)]
    pub qualify_scan_cursor: outbe_primitives::storage::dsl::Value<u32>,
}

impl IntexFactoryContract<'_> {
    /// Composite key for `authorized_settler`: `keccak256(holder ++ series_id_be32)`.
    pub fn authorized_settler_key(holder: Address, series_id: u32) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..20].copy_from_slice(holder.as_slice());
        buf[20..24].copy_from_slice(&series_id.to_be_bytes());
        keccak256(buf)
    }

    /// Composite key for `mine_seq`: `keccak256(series_id_be32 ++ holder)`.
    pub fn mine_seq_key(series_id: u32, holder: Address) -> B256 {
        let mut buf = [0u8; 24];
        buf[0..4].copy_from_slice(&series_id.to_be_bytes());
        buf[4..24].copy_from_slice(holder.as_slice());
        keccak256(buf)
    }
}
