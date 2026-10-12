//! Shared registry queries and trusted Credis-accounting entrypoints.
pub use crate::precompile::ICcaRegistry;
pub use crate::runtime::{credis_forfeited, credis_issued};
use crate::{
    constants::MAX_ACTIVE_CCAS,
    errors::CcaError,
    schema::{address_day_key, CcaContract},
    state::validate_state,
};
use alloy_primitives::{Address, U256};
use outbe_primitives::{
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

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

/// Raw GRATIS in one UTC day bucket (YYYYMMDD). Distribution is the only step that normalizes it.
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
/// This function reads historical day buckets and does not mutate them.
///
/// Enumeration is limited to [`MAX_ACTIVE_CCAS`]. A longer index fails settlement.
pub fn active_reward_weights(
    storage: &StorageHandle<'_>,
    day: u32,
) -> Result<Vec<(Address, U256)>> {
    let contract = CcaContract::new(storage.clone());
    let len = contract.active.len()?;
    if len > MAX_ACTIVE_CCAS {
        return Err(CcaError::ActiveSetFull.into());
    }
    let mut weights = Vec::new();
    for index in 0..len {
        let cca = contract
            .active
            .at(index)?
            .ok_or_else(|| PrecompileError::Fatal("CCA active index entry missing".into()))?;
        let weight = contract
            .gratis_sum_per_utc_day
            .read(&address_day_key(cca, day))?;
        if !weight.is_zero() {
            weights.push((cca, weight));
        }
    }
    Ok(weights)
}
