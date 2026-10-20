//! Storage schema for IntexFactory: settlement bookkeeping and the
//! call-price bin index. Canonical series state lives in Intex.

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_intex::{IntexCallTrigger, SeriesId, SERIES_ID_LEN};
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::time::WorldwideDay;

/// The call terms of the live profile. `api::call_terms` reads them once, and the
/// auction start fixes them for the day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntexCallTerms {
    /// Percentage points over the entry price.
    pub call_rate: u16,
    pub call_trigger: IntexCallTrigger,
}

impl IntexCallTerms {
    /// Per-unit call price (reference ISO stable-units, 1e6): the entry marked up by the call rate.
    pub fn call_price(&self, entry_price_minor: U256) -> Result<U256> {
        crate::runtime::marked_up(entry_price_minor, self.call_rate)
    }
}

/// Issuance inputs captured on Outbe: every condition the auction fixed at its start.
/// Issuance reads no profile value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuanceParams {
    pub series_id: SeriesId,
    pub worldwide_day: WorldwideDay,
    pub issued_units: u32,
    pub promis_load_minor: u128,
    /// Entry price (per-unit, reference ISO stable-units, 1e6). Cost and floor derive from it.
    pub entry_price_minor: U256,
    /// Call price (per-unit, reference ISO stable-units, 1e6), fixed at the auction start.
    pub call_price_minor: U256,
    /// Call window, threshold and notice, fixed at the auction start.
    pub call_trigger: IntexCallTrigger,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    /// Every target chain of the day's snapshot. Each one creates the series.
    pub snapshot_chains: Vec<u32>,
}

/// EVM storage layout: settlement bookkeeping (mine_seq), the Called-notice outbox and
/// the cursors of the cross-chain sends. The series lifecycle lives in Intex.
#[storage_schema]
#[contract(addr = INTEX_FACTORY_ADDRESS)]
pub struct IntexFactoryContract {
    /// `keccak256(series_id ++ owner)` -> monotonic minePromis sequence.
    #[attribute(order = 1)]
    pub mine_seq: outbe_primitives::storage::dsl::Map<B256, u32>,

    // Called notices waiting to be sent after the call slice, in order. Head and tail
    // reset to 0 whenever the queue drains empty.
    #[attribute(order = 24)]
    pub notify_head: outbe_primitives::storage::dsl::Value<u32>,
    #[attribute(order = 25)]
    pub notify_tail: outbe_primitives::storage::dsl::Value<u32>,
    /// Queue index -> a Called series' word packed with its call time, the router calls
    /// it was refused, and the time it may go out again.
    #[attribute(order = 26)]
    pub notify_at: outbe_primitives::storage::dsl::Map<u32, U256>,
    /// Where the parked-message sweep resumes: every index below it is sent or empty.
    #[attribute(order = 45)]
    pub parked_message_cursor: outbe_primitives::storage::dsl::Value<u64>,
    /// As `parked_message_cursor`, for the proceeds the factory refused.
    #[attribute(order = 46)]
    pub parked_proceeds_cursor: outbe_primitives::storage::dsl::Value<u64>,
}

impl IntexFactoryContract<'_> {
    /// Composite key for `mine_seq`: `keccak256(series_id ++ owner)`.
    pub fn mine_seq_key(series_id: SeriesId, owner: Address) -> B256 {
        let mut buf = [0u8; SERIES_ID_LEN + 20];
        buf[..SERIES_ID_LEN].copy_from_slice(series_id.as_bytes());
        buf[SERIES_ID_LEN..].copy_from_slice(owner.as_slice());
        keccak256(buf)
    }
}
