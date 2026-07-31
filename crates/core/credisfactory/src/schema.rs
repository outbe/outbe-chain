//! Storage schema for the credisfactory precompile.
//!
//! The credis schedule itself (positions, anadosis installments, the pledger EOA)
//! lives in the `outbe_credis` crate. Collateral stays in the pledger's own
//! confidential Gratis `pledged_ct` for the whole term (no escrow account), so this
//! schema only needs the begin-block expiry-sweep cursor.

use outbe_macros::contract;
use outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS;
use outbe_primitives::storage::types::Slot;

/// EVM storage layout for the credisfactory precompile.
///
/// Storage slots:
///   0: u64 — begin-block expiry-scan cursor (index into the credis dense position
///      index to resume from next block).
#[contract(addr = CREDIS_FACTORY_ADDRESS)]
pub struct CredisFactoryContract {
    pub expiry_scan_cursor: Slot<u64>,
}
