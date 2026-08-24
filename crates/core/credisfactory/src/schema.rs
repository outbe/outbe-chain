//! Storage schema for the credisfactory precompile.
//!
//! The positions themselves (terms, state, the pledger EOA) live in the
//! `outbe_credis` crate. The pledger's own collateral stays in its confidential Gratis
//! `pledged_ct` for the whole life of the position (no escrow account); what is held
//! here is the originating CCA's matching COEN stake, plus the daily price-path scan's
//! cursor.

use alloy_primitives::U256;
use outbe_macros::contract;
use outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS;
use outbe_primitives::storage::types::Slot;

/// EVM storage layout for the credisfactory precompile.
///
/// Storage slots:
///   0: u32 — daily price-path scan cursor, stored as `index + 1` into the credis
///      active-position index. 0 means the last pass completed and the next run
///      starts a fresh one from the top.
///   1: map positionId -> U256 — the originating CCA's escrowed COEN stake, taken at
///      `requestCredis` and equal to the pledged collateral. Returned to the CCA when
///      the position closes, burned when it voids. The native COEN itself sits in this
///      precompile's own balance; this map is the per-position claim on it.
#[contract(addr = CREDIS_FACTORY_ADDRESS)]
pub struct CredisFactoryContract {
    pub call_scan_cursor: Slot<u32>,
    pub cca_stake: outbe_primitives::storage::dsl::Map<U256, U256>,
}
