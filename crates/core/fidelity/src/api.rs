//! Cross-module API for the Fidelity module.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::FidelityContract;

/// ACQUISITION hook: record a new active gratis cohort for `account` at block
/// time `now` (seconds). No-op on a zero amount. See
/// [`FidelityContract::on_gratis_mined`].
pub fn on_gratis_mined(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    now: u64,
) -> Result<()> {
    FidelityContract::new(storage).on_gratis_mined(account, amount, now)
}

/// SALE hook: destroy `account`'s active cohorts LIFO at block time `now`
/// (seconds), logging the sold slices. No-op on a zero amount; excess over the
/// recorded cohorts is clamped. See [`FidelityContract::on_coen_mined`].
pub fn on_coen_mined(
    storage: StorageHandle<'_>,
    account: Address,
    amount: U256,
    now: u64,
) -> Result<()> {
    FidelityContract::new(storage).on_coen_mined(account, amount, now)
}

/// RCFI for `account` at the current block time, in decayed days (0..L). See
/// [`FidelityContract::get_rcfi`].
pub fn get_rcfi(storage: StorageHandle<'_>, account: Address) -> Result<u64> {
    FidelityContract::new(storage).get_rcfi(account)
}

/// RCFI for `account` at an explicit block time `now` (seconds), in decayed
/// days (0..L). See [`FidelityContract::compute_rcfi`].
pub fn compute_rcfi(storage: StorageHandle<'_>, account: Address, now: u64) -> Result<u64> {
    FidelityContract::new(storage).compute_rcfi(account, now)
}
