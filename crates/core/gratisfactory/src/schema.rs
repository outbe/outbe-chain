//! Storage schema for the gratisfactory precompile.
//!
//! The gratis itself lives in `outbe_gratis`, and the pledge terms are sealed
//! inside the enclave's `PledgeLockTicket`. What is kept here is the part the
//! ticket cannot carry without a TEE wire change: when the quote was struck, and
//! the order the quotes were struck in.

use alloy_primitives::B256;
use outbe_macros::contract;
use outbe_primitives::addresses::GRATIS_FACTORY_ADDRESS;

/// EVM storage layout for the gratisfactory precompile.
///
/// Layout:
///   slot 0:    pledge handle -> unix timestamp the quote was struck at.
///   slots 1-2: pledge handles with a vault reservation still to unwind.
#[contract(addr = GRATIS_FACTORY_ADDRESS)]
pub struct GratisFactoryContract {
    /// Cleared when the ticket is spent or unpledged, so a non-zero entry means
    /// "live quote". The same timestamp is the reservation's deadline.
    #[attribute(order = 0)]
    pub pledge_quoted_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// Handles with a vault reservation to unwind, in the order they were
    /// pledged. [`crate::constants::PLEDGE_QUOTE_TTL_SECS`] is a constant, so
    /// insertion order **is** expiry order and the queue needs no timestamp of
    /// its own: the head's deadline is `pledge_quoted_at[head] + TTL`. A head
    /// whose quote reads zero was already spent or unpledged and is popped as a
    /// tombstone.
    #[attribute(order = 1)]
    pub pledge_queue: outbe_primitives::storage::dsl::Deque<B256>,
}
