//! Cross-module API for the CCA program.
//!
//! This is the surface `credisfactory` calls: it gates origination on
//! [`can_originate`] and reports each of the three events that move an agent's
//! standing — an origination it should be paid for, a settlement that restores
//! it, and a void that penalizes it.

use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::schema::CcaContract;

/// Whether `cca` may open new positions. False — never an error — for an
/// address that never registered, so a caller can gate on it directly.
pub fn can_originate(storage: StorageHandle<'_>, cca: Address) -> Result<bool> {
    CcaContract::new(storage).can_originate(cca)
}

/// Credits the agent with the reward units earned by an origination, returning
/// the units credited (zero when the §8.3 dust guard or the one-unit-per-owner-
/// per-day rule applies). Reverts when `cca` is not registered.
pub fn record_origination(
    storage: StorageHandle<'_>,
    cca: Address,
    smart_account: Address,
    principal: U256,
    day: WorldwideDay,
) -> Result<U256> {
    CcaContract::new(storage).record_origination(cca, smart_account, principal, day)
}

/// Restores part of the agent's multiplier in proportion to principal repaid on
/// a position it originated. A no-op for an unregistered or unpenalized agent.
pub fn record_settlement(
    storage: StorageHandle<'_>,
    cca: Address,
    settled_value: U256,
) -> Result<()> {
    CcaContract::new(storage).record_settlement(cca, settled_value)
}

/// Cuts the agent's multiplier for a void, scaled by the share of principal left
/// unpaid (1e18). A no-op for an unregistered agent.
pub fn record_void(storage: StorageHandle<'_>, cca: Address, unpaid_share: U256) -> Result<()> {
    CcaContract::new(storage).record_void(cca, unpaid_share)
}

/// The agent's multiplier, 1e18 scaled; 1e18 for an address that never
/// registered. Used by the daily distribution to weight origination units.
pub fn multiplier_of(storage: StorageHandle<'_>, cca: Address) -> Result<U256> {
    CcaContract::new(storage).multiplier_of(cca)
}

/// Every agent that earned origination units on `day`, paired with those units
/// before the multiplier is applied.
pub fn day_originators(
    storage: StorageHandle<'_>,
    day: WorldwideDay,
) -> Result<Vec<(Address, U256)>> {
    CcaContract::new(storage).day_originators(day)
}
