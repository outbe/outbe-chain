//! Shared registry queries and trusted Credis-accounting entrypoints.
pub use crate::precompile::ICcaRegistry;
pub use crate::runtime::{credis_forfeited, credis_issued};
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
    let record = CcaContract::new(storage.clone()).load(cca)?;
    if record.state == ICcaRegistry::State::Active {
        Ok(())
    } else {
        Err(CcaError::CcaNotActive {
            cca,
            state: record.state,
        }
        .into())
    }
}

/// Raw GRATIS in one UTC day bucket (YYYYMMDD). Distribution is the only step that normalizes it.
pub fn reward_weight(storage: &StorageHandle<'_>, cca: Address, day: u32) -> Result<U256> {
    CcaContract::new(storage.clone())
        .gratis_minor_per_utc_day
        .read(&address_day_key(cca, day))
}

pub fn get_cca(storage: &StorageHandle<'_>, cca: Address) -> Result<ICcaRegistry::Cca> {
    let contract = CcaContract::new(storage.clone());
    let record = contract.load(cca)?;
    Ok(ICcaRegistry::Cca {
        cca: record.cca,
        name: record.name,
        state: record.state,
        bondedMinor: record.bonded_minor,
        unbondUnlocksAfter: record.unbond_unlocks_after,
    })
}

/// Native COEN bonded across all records and not yet claimed back.
pub fn total_bonded(storage: &StorageHandle<'_>) -> Result<U256> {
    CcaContract::new(storage.clone()).total_bonded_minor.read()
}

fn require_sealed(contract: &CcaContract<'_>, day: u32) -> Result<()> {
    if day == 0 || day > outbe_rewards::api::get_last_sealed_day(&contract.storage)? {
        return Err(CcaError::RewardDayNotSealed.into());
    }
    Ok(())
}

/// The number of the CCAs with a weight in the sealed `day` and the sum of their weights.
pub fn reward_day_inputs(storage: &StorageHandle<'_>, day: u32) -> Result<(u32, U256)> {
    let contract = CcaContract::new(storage.clone());
    require_sealed(&contract, day)?;
    Ok((
        contract.reward_day_count.read(&day)?,
        contract.reward_day_total.read(&day)?,
    ))
}

/// Reads and deletes the weight of `cca` in the sealed `day`, once its reward is claimed or expired.
pub fn take_reward_weight(storage: &StorageHandle<'_>, cca: Address, day: u32) -> Result<U256> {
    let contract = CcaContract::new(storage.clone());
    require_sealed(&contract, day)?;
    let key = address_day_key(cca, day);
    let weight = contract.gratis_minor_per_utc_day.read(&key)?;
    contract.gratis_minor_per_utc_day.clear(&key)?;
    contract.gratis_deficit_minor_per_utc_day.clear(&key)?;
    Ok(weight)
}

/// Deletes the count and the weight sum of the sealed `day` with its last weight.
pub fn clear_reward_day(storage: &StorageHandle<'_>, day: u32) -> Result<()> {
    let contract = CcaContract::new(storage.clone());
    require_sealed(&contract, day)?;
    contract.reward_day_count.clear(&day)?;
    contract.reward_day_total.clear(&day)
}
