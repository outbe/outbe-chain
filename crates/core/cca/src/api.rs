//! Shared registry queries and trusted position-accounting entrypoints.
pub use crate::precompile::ICca;
pub use crate::runtime::{position_opened, position_voided};
use crate::{schema::CcaContract, state::decode_state};
use alloy_primitives::{Address, U256};
use outbe_primitives::{error::Result, storage::StorageHandle, time::WorldwideDay};

pub fn cca_state(storage: &StorageHandle<'_>, cca: Address) -> Result<ICca::State> {
    match CcaContract::new(storage.clone()).records.get(cca)? {
        Some(record) => decode_state(record.state),
        None => Ok(ICca::State::Unknown),
    }
}

pub fn is_active(storage: &StorageHandle<'_>, cca: Address) -> Result<bool> {
    Ok(cca_state(storage, cca)? as u8 == ICca::State::Active as u8)
}

/// Raw GRATIS in one activity-day bucket; normalized only during distribution.
pub fn reward_weight(storage: &StorageHandle<'_>, cca: Address, day: WorldwideDay) -> Result<U256> {
    CcaContract::new(storage.clone())
        .reward_weights
        .read(&CcaContract::reward_weight_key(cca, day))
}

pub fn get_cca(storage: &StorageHandle<'_>, cca: Address) -> Result<ICca::Cca> {
    let record = CcaContract::new(storage.clone()).records.get(cca)?;
    match record {
        Some(r) => Ok(ICca::Cca {
            cca: r.cca,
            state: decode_state(r.state)?,
            bondedAmount: r.bonded_amount,
            unbondUnlockAfter: r.unbond_unlock_after,
            rewardAmount: r.reward_amount,
        }),
        None => Ok(ICca::Cca {
            cca,
            state: ICca::State::Unknown,
            bondedAmount: U256::ZERO,
            unbondUnlockAfter: 0,
            rewardAmount: U256::ZERO,
        }),
    }
}
