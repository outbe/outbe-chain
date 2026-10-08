//! The factory stores the call sweep's pinned day and cursor. Positions and return serials live
//! in Credis. Encrypted collateral is aggregated at CREDIS_ADDRESS in Gratis.

use outbe_macros::contract;
use outbe_primitives::addresses::CREDIS_FACTORY_ADDRESS;
use outbe_primitives::storage::types::Slot;

/// EVM storage layout for the credisfactory precompile.
///
/// Storage slots:
///   0: u32 - ISO 4217 code of the currency the call sweep resumes at.
///   1: u32 - UTC day the unfinished call sweep is pinned to. 0 = none in flight.
///   2: u32 - UTC day waiting behind it. 0 = none.
#[contract(addr = CREDIS_FACTORY_ADDRESS)]
pub struct CredisFactoryContract {
    pub call_currency_cursor: Slot<u32>,
    pub call_sweep_day: Slot<u32>,
    pub call_pending_day: Slot<u32>,
}
