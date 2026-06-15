//! Storage schema for the IntexRegistry runtime module.
//!
//! IntexRegistry is the canonical, cross-chain Intex series ledger: it owns the
//! per-series identity parameters captured once at issuance and the lifecycle
//! status updated as the series progresses. It mirrors the identity + lifecycle
//! half of the Origin `IntexNFT1155.SeriesData` struct; the supply/balance half
//! (`totalSupply`, `mintedCount`, `status`) stays in the ERC-1155 ledger.
//!
//! One record per `seriesId`. The Issued/Settled token-id split is an ERC-1155
//! balance-ledger concern and is not represented here.

use alloy_primitives::U256;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::INTEX_REGISTRY_ADDRESS;

use crate::errors::IntexRegistryError;

/// Series lifecycle state. Mirrors `IIntexNFT1155.IntexState`.
///
/// Lifecycle: `Issued -> Qualified -> Called`. Expiration after the call
/// deadline is not a distinct state here (it is signalled on the ERC-1155
/// ledger), matching the Origin contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IntexState {
    Issued = 0,
    Qualified = 1,
    Called = 2,
}

impl IntexState {
    /// Decode a stored `u8` into the typed state, rejecting unknown values.
    pub fn from_u8(value: u8) -> Result<Self, IntexRegistryError> {
        match value {
            0 => Ok(Self::Issued),
            1 => Ok(Self::Qualified),
            2 => Ok(Self::Called),
            other => Err(IntexRegistryError::InvalidStateValue(other)),
        }
    }
}

/// Forced-call trigger parameters for a series. Mirrors
/// `IIntexNFT1155.IntexCallTrigger`. Pure identity, set once at issuance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IntexCallTrigger {
    /// Observation window length in days.
    pub window_days: u16,
    /// Number of days within the window that breach the trigger.
    pub threshold_days: u16,
    /// COEN price level that arms the forced call (1e18, oracle scale).
    pub coen_price_call_trigger: U256,
}

/// Identity parameters captured once at series creation. Mirrors the
/// non-supply, non-lifecycle inputs of `IntexNFT1155.createSeries`.
///
/// `promis_load_minor` is kept as `u128` here to mirror the Origin `uint128` ABI
/// exactly; storage holds it as `U256` (the storage DSL has no `u128` slot
/// codec), and the `u128 -> U256` widening is always lossless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSeriesParams {
    pub series_id: u32,
    /// Auction-cleared cap on cumulative Issued mint quantity. Identity only;
    /// the live mint counter lives on the ERC-1155 ledger.
    pub issued_intex_count: u32,
    /// Promis tokens per Intex unit (18 decimals); bounded by source `uint128`.
    pub promis_load_minor: u128,
    /// Intex strike price (payment-token decimals).
    pub cost_amount_minor: u64,
    /// COEN price floor (1e18, oracle scale).
    pub floor_price_minor: U256,
    /// Duration in seconds between `called_at` and the settlement deadline.
    pub intex_call_period: u32,
    /// Forced-call trigger parameters.
    pub call_trigger: IntexCallTrigger,
    /// Creation timestamp (UNIX seconds), supplied by the caller from
    /// `BlockContext`. Must be non-zero — it doubles as the existence sentinel.
    pub issued_at: u32,
}

/// Per-series identity + lifecycle record. Keyed by `series_id`.
///
/// `issued_at` is the existence sentinel (`issued_at == 0` means "no series"),
/// matching the Origin contract's `issuedAt != 0` check.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = issued_at)]
pub struct SeriesRecord {
    #[key]
    pub series_id: u32,

    // --- identity (immutable after creation) ---
    /// Promis tokens per Intex unit; bounded by source `uint128`.
    #[attribute(order = 0)]
    pub promis_load_minor: U256,

    #[attribute(order = 1)]
    pub cost_amount_minor: u64,

    #[attribute(order = 2)]
    pub floor_price_minor: U256,

    #[attribute(order = 3)]
    pub issued_intex_count: u32,

    #[attribute(order = 4)]
    pub call_window_days: u16,

    #[attribute(order = 5)]
    pub call_threshold_days: u16,

    #[attribute(order = 6)]
    pub coen_price_call_trigger: U256,

    // --- lifecycle (mutated as the series progresses) ---
    /// Lifecycle state as `u8`; decode via [`IntexState::from_u8`].
    #[attribute(order = 7)]
    pub state: u8,

    /// Creation timestamp + existence sentinel (`!= 0`).
    #[attribute(order = 8)]
    pub issued_at: u32,

    /// Timestamp the series entered `Called` (0 until called).
    #[attribute(order = 9, default = 0)]
    pub called_at: u32,

    #[attribute(order = 10)]
    pub intex_call_period: u32,
}

impl SeriesRecord {
    /// Typed lifecycle state.
    pub fn lifecycle_state(&self) -> Result<IntexState, IntexRegistryError> {
        IntexState::from_u8(self.state)
    }

    /// Forced-call trigger as a grouped value.
    pub fn call_trigger(&self) -> IntexCallTrigger {
        IntexCallTrigger {
            window_days: self.call_window_days,
            threshold_days: self.call_threshold_days,
            coen_price_call_trigger: self.coen_price_call_trigger,
        }
    }
}

/// EVM storage layout for the IntexRegistry module.
///
/// - `series` holds one identity + lifecycle record per `seriesId`.
/// - `total_series` / `series_id_at_index` provide dense, no-`Vec` enumeration
///   in the same shape as the credis/nod owner index.
#[storage_schema]
#[contract(addr = INTEX_REGISTRY_ADDRESS)]
pub struct IntexRegistryContract {
    /// Per-series identity + lifecycle record keyed by `series_id`.
    #[attribute(order = 0)]
    pub series: outbe_primitives::storage::dsl::Map<u32, SeriesRecord>,

    /// Total series ever created (for dense enumeration).
    #[attribute(order = 1)]
    pub total_series: outbe_primitives::storage::dsl::Value<u64>,

    /// Dense index — index -> series_id.
    #[attribute(order = 2)]
    pub series_id_at_index: outbe_primitives::storage::dsl::Map<u64, u32>,
}
