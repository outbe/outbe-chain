//! Cross-module API for the Gratis token.
//!
//! This is the surface other crates use to move Gratis. Callers pass a
//! [`StorageHandle`] and never construct the [`Gratis`] facade themselves; the
//! mutating primitives on that facade are crate-private on purpose so that the
//! only cross-crate entry points are the ones documented here.
//!
//! Current production callers:
//! - `outbe_gratisfactory::runtime` — [`mine`]/[`burn`] the acquisition and
//!   sale paths, and [`pledge`]/[`unpledge`] the credis escrow.
//!
//! State-changing entry points enforce their own validation (positive amount,
//! non-zero address, sufficient balance/pledge) and emit the matching
//! `IGratis` event; on any failure no balance moves.

use alloy_primitives::{Address, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::Gratis;

// --- Reads ---

/// Total circulating supply, including gratis currently held in the credis
/// escrow.
pub fn total_supply(storage: StorageHandle<'_>) -> Result<U256> {
    Gratis::new(storage).total_supply()
}

/// Spendable balance of `account` (excludes any amount pledged into escrow).
pub fn balance_of(storage: StorageHandle<'_>, account: Address) -> Result<U256> {
    Gratis::new(storage).balance_of(account)
}

/// Aggregate amount currently held in the credis escrow across all pledgers.
pub fn pledged_total_supply(storage: StorageHandle<'_>) -> Result<U256> {
    Gratis::new(storage).pledged_total_supply()
}

/// Amount currently pledged by `account` and held in the credis escrow on
/// their behalf.
pub fn pledged_of(storage: StorageHandle<'_>, account: Address) -> Result<U256> {
    Gratis::new(storage).pledged_of(account)
}

// --- Mutations ---

/// Mint `amount` gratis to `account`. Returns the new total supply.
pub fn mine(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<U256> {
    Gratis::new(storage).mine(account, amount)
}

/// Burn `amount` gratis from `account`. Returns the remaining total supply.
pub fn burn(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<U256> {
    Gratis::new(storage).burn(account, amount)
}

/// Lock `amount` gratis from `account` into the credis escrow and credit the
/// per-account pledge ledger. Returns the new aggregate pledged amount.
pub fn pledge(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<U256> {
    Gratis::new(storage).pledge(account, amount)
}

/// Release `amount` gratis from the credis escrow back to `account`, debiting
/// the per-account pledge ledger. Returns the remaining aggregate pledged
/// amount.
pub fn unpledge(storage: StorageHandle<'_>, account: Address, amount: U256) -> Result<U256> {
    Gratis::new(storage).unpledge(account, amount)
}
