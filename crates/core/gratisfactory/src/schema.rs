//! Storage schema for the gratisfactory precompile.
//!
//! The pledge tickets themselves are confidential and live inside the enclave-
//! sealed Gratis state, so nothing about their contents can be stored here. The
//! one thing this module does need on-chain is *when* each quote was struck: the
//! quote fixes `P₀` and therefore the whole geometry of the Credis position that
//! spends it, and §7 requires a stale quote to be unexercisable.
//!
//! The timestamp is kept beside the ticket rather than inside it on purpose.
//! Adding a field to `PledgeTerms` would be a TEE wire change — a new
//! `RECORD_PLAINTEXT_LEN`, a new `inputs_canonical_hash`, and a new MRENCLAVE
//! rolled out in lockstep across host and enclave. The pledge handle is already
//! public (it is calldata at `requestCredis` and is emitted in `GratisPledged`),
//! so recording its age on-chain leaks nothing that was not already visible.

use alloy_primitives::B256;
use outbe_macros::{contract, storage_schema};
use outbe_primitives::addresses::GRATIS_FACTORY_ADDRESS;

/// EVM storage layout for the gratisfactory precompile.
#[storage_schema]
#[contract(addr = GRATIS_FACTORY_ADDRESS)]
pub struct GratisFactoryContract {
    /// pledge handle → unix timestamp the quote was struck at. Cleared when the
    /// ticket is spent or unpledged, so a non-zero entry means "live quote".
    #[attribute(order = 0)]
    pub pledge_quoted_at: outbe_primitives::storage::dsl::Map<B256, u64>,

    /// Handles with a vault reservation to unwind, in the order they were
    /// pledged. The TTL is a constant, so insertion order **is** expiry order and
    /// the queue needs no timestamp of its own: the head's deadline is
    /// `pledge_quoted_at[head] + PLEDGE_QUOTE_TTL_SECS`. A head whose quote reads
    /// zero was already spent or unpledged and is popped as a tombstone.
    #[attribute(order = 1)]
    pub pledge_queue: outbe_primitives::storage::dsl::Deque<B256>,
}
