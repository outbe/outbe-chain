//! Storage schema for the Intex runtime module: the canonical per-series
//! identity + lifecycle ledger. One record per `seriesId`.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::INTEX_ADDRESS;

use crate::errors::IntexError;

/// Series lifecycle state. `Issued -> Qualified -> Called`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IntexState {
    Issued = 0,
    Qualified = 1,
    Called = 2,
}

impl IntexState {
    pub fn from_u8(value: u8) -> Result<Self, IntexError> {
        match value {
            0 => Ok(Self::Issued),
            1 => Ok(Self::Qualified),
            2 => Ok(Self::Called),
            other => Err(IntexError::InvalidStateValue(other)),
        }
    }
}

/// Forced-call trigger parameters for a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IntexCallTrigger {
    pub window_days: u16,
    pub threshold_days: u16,
    /// Seconds between `called_at` and the settlement deadline.
    pub intex_call_period: u32,
}

/// Identity parameters captured once at series creation.
///
/// `promis_load_minor` is `u128` to mirror the Origin `uint128` ABI; storage
/// widens it to `U256` (the storage DSL has no `u128` codec), always lossless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSeriesParams {
    pub series_id: u32,
    pub worldwide_day: u32,
    pub issued_intex_count: u32,
    /// Promis tokens per Intex unit (18 decimals); bounded by source `uint128`.
    pub promis_load_minor: u128,
    /// Entry price (per-unit, reference currency, 1e18 oracle scale). Primary
    /// anchor; cost/floor/call derive from it.
    pub entry_price_minor: U256,
    /// Price floor (1e18, oracle scale).
    pub floor_price_minor: U256,
    /// Call price level that arms the forced call (1e18, oracle scale).
    pub call_price_minor: U256,
    pub call_trigger: IntexCallTrigger,
    /// Creation timestamp (UNIX seconds); non-zero, doubles as existence sentinel.
    pub issued_at: u32,
    pub issuance_currency: u16,
    pub reference_currency: u16,
}

/// Per-series identity + lifecycle record. Keyed by `series_id`.
/// `issued_at == 0` means "no series".
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = issued_at)]
pub struct SeriesRecord {
    #[key]
    pub series_id: u32,

    #[attribute(order = 0)]
    pub issuance_currency: u16,

    #[attribute(order = 1)]
    pub reference_currency: u16,

    #[attribute(order = 2)]
    pub issued_intex_count: u32,

    #[attribute(order = 3)]
    pub promis_load_minor: U256,

    #[attribute(order = 4)]
    pub entry_price_minor: U256,

    #[attribute(order = 5)]
    pub floor_price_minor: U256,

    #[attribute(order = 6)]
    pub call_price_minor: U256,

    // call_trigger group — stored flat (the storage DSL has no nested-struct codec),
    // exposed nested via `call_trigger()`.
    #[attribute(order = 7)]
    pub call_window_days: u16,

    #[attribute(order = 8)]
    pub call_threshold_days: u16,

    #[attribute(order = 9)]
    pub intex_call_period: u32,

    #[attribute(order = 10)]
    pub issued_at: u32,

    #[attribute(order = 11, default = 0)]
    pub called_at: u32,

    /// Lifecycle state as `u8`; decode via [`IntexState::from_u8`].
    #[attribute(order = 12)]
    pub state: u8,

    /// Worldwide day whose tributes fed this series (== series_id until multi-currency).
    #[attribute(order = 13, default = 0)]
    pub worldwide_day: u32,
}

impl SeriesRecord {
    pub fn lifecycle_state(&self) -> Result<IntexState, IntexError> {
        IntexState::from_u8(self.state)
    }

    pub fn call_trigger(&self) -> IntexCallTrigger {
        IntexCallTrigger {
            window_days: self.call_window_days,
            threshold_days: self.call_threshold_days,
            intex_call_period: self.intex_call_period,
        }
    }
}

/// Paginated creator-reward distribution progress for a series. Exists while a
/// distribution is in flight; `active != 0` is the existence sentinel.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = active)]
pub struct DistProgress {
    #[key]
    pub series_id: u32,

    /// Total native COEN received for this series' distribution.
    #[attribute(order = 0)]
    pub amount: U256,

    /// Σ of contributor nominals (the proportionality denominator).
    #[attribute(order = 1)]
    pub total_nominal: U256,

    /// Native COEN already paid out across chunks so far.
    #[attribute(order = 2)]
    pub paid_so_far: U256,

    /// Index of the next contributor to pay.
    #[attribute(order = 3)]
    pub cursor: u32,

    /// Existence sentinel: 1 while in flight.
    #[attribute(order = 4)]
    pub active: u8,
}

/// EVM storage layout for the Intex module.
#[storage_schema]
#[contract(addr = INTEX_ADDRESS)]
pub struct IntexContract {
    #[attribute(order = 0)]
    pub series: outbe_primitives::storage::dsl::Map<u32, SeriesRecord>,

    #[attribute(order = 1)]
    pub total_series: outbe_primitives::storage::dsl::Value<u64>,

    #[attribute(order = 2)]
    pub series_id_at_index: outbe_primitives::storage::dsl::Map<u64, u32>,

    // --- Creator-reward: per-series contributors (owner → nominal share) ---
    /// series_id -> number of contributors.
    #[attribute(order = 3)]
    pub contributor_count: outbe_primitives::storage::dsl::Map<u32, u32>,

    /// keccak256(series_id_be32 ++ index_be32) -> contributor owner.
    #[attribute(order = 4)]
    pub contributor_owner_at: outbe_primitives::storage::dsl::Map<B256, Address>,

    /// keccak256(series_id_be32 ++ index_be32) -> contributor nominal share.
    #[attribute(order = 5)]
    pub contributor_nominal_at: outbe_primitives::storage::dsl::Map<B256, U256>,

    /// series_id -> Σ nominal across all contributors.
    #[attribute(order = 6)]
    pub contributor_total: outbe_primitives::storage::dsl::Map<u32, U256>,

    // --- Creator-reward: paginated distribution progress + active set ---
    /// series_id -> in-flight distribution progress.
    #[attribute(order = 7)]
    pub dist_progress: outbe_primitives::storage::dsl::Map<u32, DistProgress>,

    /// Number of in-flight distributions (dense active set for the begin-block drain).
    #[attribute(order = 8)]
    pub active_dist_count: outbe_primitives::storage::dsl::Value<u32>,

    /// dense index -> series_id.
    #[attribute(order = 9)]
    pub active_dist_at: outbe_primitives::storage::dsl::Map<u32, u32>,

    /// series_id -> (active index + 1); 0 = not active.
    #[attribute(order = 10)]
    pub active_dist_slot: outbe_primitives::storage::dsl::Map<u32, u32>,
}

impl IntexContract<'_> {
    /// Composite key for per-series contributor index lists:
    /// `keccak256(series_id_be32 ++ index_be32)`.
    pub fn contributor_index_key(series_id: u32, index: u32) -> B256 {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&series_id.to_be_bytes());
        buf[4..8].copy_from_slice(&index.to_be_bytes());
        keccak256(buf)
    }
}
