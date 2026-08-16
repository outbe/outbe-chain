//! Storage schema for IntexFactory: settlement bookkeeping and the
//! unqualified-series bin index. Canonical series state lives in Intex.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_intex::{SeriesId, SERIES_ID_LEN};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;

/// Issuance inputs captured on Outbe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuanceParams {
    pub series_id: SeriesId,
    pub worldwide_day: WorldwideDay,
    pub issued_intex_count: u32,
    pub promis_load_minor: u128,
    /// Entry price (per-unit, reference currency, 1e18 oracle scale); cost/floor/call derive from it.
    pub entry_price_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    /// Auction winners: per-address mint recipients for ISSUANCE_INSTRUCTIONS.
    pub recipients: Vec<Address>,
    pub quantities: Vec<U256>,
    /// Source chain of each winner (parallel to `recipients`); routes each mint to its chain.
    pub recipient_chains: Vec<u32>,
    /// Every target chain of the day's snapshot; each gets an ISSUANCE (empty recipients = create only).
    pub snapshot_chains: Vec<u32>,
}

/// EVM storage layout: settlement bookkeeping (authorized_settler, settle_count,
/// mine_seq) and the unqualified-series floor-bin index.
#[storage_schema]
#[contract(addr = INTEX_FACTORY_ADDRESS)]
pub struct IntexFactoryContract {
    /// `keccak256(holder ++ series_id)` -> authorized settler address.
    #[attribute(order = 0)]
    pub authorized_settler: outbe_primitives::storage::dsl::Map<B256, Address>,

    /// series_id -> cumulative settle count.
    #[attribute(order = 1)]
    pub settle_count: outbe_primitives::storage::dsl::Map<SeriesId, U256>,

    /// `keccak256(series_id ++ holder)` -> monotonic minePromis sequence.
    #[attribute(order = 2)]
    pub mine_seq: outbe_primitives::storage::dsl::Map<B256, u32>,

    // Unqualified-series bin index (by floor_price_minor) for begin_block qualify.
    // A floor is only comparable to the rate of its own reference currency, so
    // every column is namespaced by ISO code and each currency walks its own trie.
    #[attribute(order = 3)]
    pub bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 4)]
    pub bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 5)]
    pub bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    /// `scoped(iso, bin_id)` -> count of series in the bin.
    #[attribute(order = 6)]
    pub unqualified_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    /// `keccak256(iso_be16 ++ bin_id_be32 ++ index_be32)` -> series_id.
    #[attribute(order = 7)]
    pub unqualified_bin_series: outbe_primitives::storage::dsl::Map<B256, U256>,

    // Qualified-series bin index (by call_price_minor) for the daily
    // Called scan. A series moves here from the unqualified index on qualify.
    #[attribute(order = 8)]
    pub qualified_bin_tree_root: outbe_primitives::storage::dsl::Map<u16, U256>,
    #[attribute(order = 9)]
    pub qualified_bin_tree_mid: outbe_primitives::storage::dsl::Map<u64, U256>,
    #[attribute(order = 10)]
    pub qualified_bin_tree_leaf: outbe_primitives::storage::dsl::Map<u64, U256>,
    /// `scoped(iso, bin_id)` -> count of series in the bin.
    #[attribute(order = 11)]
    pub qualified_bin_count: outbe_primitives::storage::dsl::Map<u64, u32>,
    /// `keccak256(iso_be16 ++ bin_id_be32 ++ index_be32)` -> series_id.
    #[attribute(order = 12)]
    pub qualified_bin_series: outbe_primitives::storage::dsl::Map<B256, U256>,

    // Genesis parameter-profile selector (0 = prod, 1 = dev); see crate::config.
    #[attribute(order = 13)]
    pub config_profile: outbe_primitives::storage::dsl::Value<u8>,

    // Bin each currency's sweep resumes from, so per-block work stays capped. 0 = fresh sweep.
    #[attribute(order = 14)]
    pub qualify_scan_cursor: outbe_primitives::storage::dsl::Map<u16, u32>,

    // Registry index each scan resumes at, so a currency that exhausts the shared
    // budget cannot starve the ones behind it.
    #[attribute(order = 15)]
    pub qualify_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 16)]
    pub call_currency_cursor: outbe_primitives::storage::dsl::Value<u32>,

    // Called-scan twin of the qualify cursor: without it a budgeted run re-walks the
    // lowest bins every day and never reaches the series above them.
    #[attribute(order = 17)]
    pub call_scan_cursor: outbe_primitives::storage::dsl::Map<u16, u32>,

    // Qualified notices waiting for the `intex_qualify_notify` trigger to send
    // them: the sweep that qualifies runs in a block hook, which cannot call
    // contracts. Head and tail reset to 0 whenever the queue drains empty.
    #[attribute(order = 18)]
    pub qualify_notify_head: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 19)]
    pub qualify_notify_tail: outbe_primitives::storage::dsl::Value<u32>,
    /// Queue index -> series awaiting its Qualified notice.
    #[attribute(order = 20)]
    pub qualify_notify_at: outbe_primitives::storage::dsl::Map<u32, SeriesId>,
}

impl IntexFactoryContract<'_> {
    /// Composite key for `authorized_settler`: `keccak256(holder ++ series_id)`.
    pub fn authorized_settler_key(holder: Address, series_id: SeriesId) -> B256 {
        let mut buf = [0u8; 20 + SERIES_ID_LEN];
        buf[0..20].copy_from_slice(holder.as_slice());
        buf[20..].copy_from_slice(series_id.as_bytes());
        keccak256(buf)
    }

    /// Namespace a bin-index column by the reference currency its prices are in.
    pub(crate) const fn scoped(reference_currency: u16, key: u32) -> u64 {
        ((reference_currency as u64) << 32) | key as u64
    }

    /// Composite key for `mine_seq`: `keccak256(series_id ++ holder)`.
    pub fn mine_seq_key(series_id: SeriesId, holder: Address) -> B256 {
        let mut buf = [0u8; SERIES_ID_LEN + 20];
        buf[..SERIES_ID_LEN].copy_from_slice(series_id.as_bytes());
        buf[SERIES_ID_LEN..].copy_from_slice(holder.as_slice());
        keccak256(buf)
    }
}
