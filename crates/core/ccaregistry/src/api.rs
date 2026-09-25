//! Shared registry queries and trusted position-accounting entrypoints.
pub use crate::precompile::ICcaRegistry;
pub use crate::runtime::{position_opened, position_voided};
use crate::{
    errors::CcaError,
    schema::{address_day_key, CcaContract},
    state::validate_state,
};
use alloy_primitives::{Address, U256};
use outbe_primitives::{error::Result, storage::StorageHandle};

pub fn cca_state(storage: &StorageHandle<'_>, cca: Address) -> Result<ICcaRegistry::State> {
    Ok(CcaContract::new(storage.clone()).load(cca)?.state)
}

pub fn is_active(storage: &StorageHandle<'_>, cca: Address) -> Result<bool> {
    match CcaContract::new(storage.clone()).records.get(cca)? {
        Some(record) => Ok(validate_state(record.state)? == ICcaRegistry::State::Active),
        None => Ok(false),
    }
}

/// Reverts unless `cca` is recorded in `Active` standing.
pub fn require_active_cca(storage: &StorageHandle<'_>, cca: Address) -> Result<()> {
    if is_active(storage, cca)? {
        Ok(())
    } else {
        Err(CcaError::CcaNotActive(cca).into())
    }
}

/// Raw GRATIS in one UTC day bucket (YYYYMMDD); normalized only during distribution.
pub fn reward_weight(storage: &StorageHandle<'_>, cca: Address, day: u32) -> Result<U256> {
    CcaContract::new(storage.clone())
        .gratis_sum_per_utc_day
        .read(&address_day_key(cca, day))
}

pub fn get_cca(storage: &StorageHandle<'_>, cca: Address) -> Result<ICcaRegistry::Cca> {
    let contract = CcaContract::new(storage.clone());
    let record = contract.load(cca)?;
    Ok(ICcaRegistry::Cca {
        cca: record.cca,
        name: record.name,
        state: record.state,
        bondedAmount: record.bonded_amount,
        unbondUnlocksAfter: record.unbond_unlocks_after,
    })
}

/// Positive net Gratis weights for CCAs active at the time of settlement.
/// Historical day buckets are read without mutation.
pub fn active_reward_weights(
    storage: &StorageHandle<'_>,
    day: u32,
) -> Result<Vec<(Address, U256)>> {
    let contract = CcaContract::new(storage.clone());
    let mut weights = Vec::new();
    for cca in contract.active.read_all()? {
        let weight = contract
            .gratis_sum_per_utc_day
            .read(&address_day_key(cca, day))?;
        if !weight.is_zero() {
            weights.push((cca, weight));
        }
    }
    Ok(weights)
}
