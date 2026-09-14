//! Shared registry queries and trusted position-accounting entrypoints.
pub use crate::precompile::ICca;
pub use crate::runtime::{position_opened, position_voided};
use crate::{schema::CcaContract, state::validate_state};
use alloy_primitives::{Address, U256};
use outbe_primitives::{error::Result, storage::StorageHandle, time::WorldwideDay};

pub fn cca_state(storage: &StorageHandle<'_>, cca: Address) -> Result<ICca::State> {
    Ok(CcaContract::new(storage.clone()).load(cca)?.state)
}

pub fn is_active(storage: &StorageHandle<'_>, cca: Address) -> Result<bool> {
    match CcaContract::new(storage.clone()).records.get(cca)? {
        Some(record) => Ok(validate_state(record.state)? == ICca::State::Active),
        None => Ok(false),
    }
}

/// Raw GRATIS in one activity-day bucket; normalized only during distribution.
pub fn reward_weight(storage: &StorageHandle<'_>, cca: Address, day: WorldwideDay) -> Result<U256> {
    CcaContract::new(storage.clone())
        .reward_weights
        .read(&CcaContract::reward_weight_key(cca, day))
}

pub fn get_cca(storage: &StorageHandle<'_>, cca: Address) -> Result<ICca::Cca> {
    let record = CcaContract::new(storage.clone()).load(cca)?;
    Ok(ICca::Cca {
        cca: record.cca,
        state: record.state,
        bondedAmount: record.bonded_amount,
        unbondUnlocksAfter: record.unbond_unlocks_after,
        rewardAmount: record.reward_amount,
        name: record.name,
    })
}
