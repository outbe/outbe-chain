//! The factory stores the daily scan cursor. Positions and return serials live
//! in Credis. Encrypted collateral is aggregated at CREDIS_ADDRESS in Gratis.

use outbe_macros::contract;
use outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS;
use outbe_primitives::storage::types::Slot;

/// EVM storage layout for the credisfactory precompile.
///
/// Storage slots:
///   0: u32 - daily price-path scan cursor, stored as `index + 1` into the credis
///      active-position index. 0 means the last pass completed and the next run
///      starts a fresh one from the top.
#[contract(addr = CREDIS_FACTORY_ADDRESS)]
pub struct CredisFactoryContract {
    pub call_scan_cursor: Slot<u32>,
}
