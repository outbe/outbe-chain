//! Shared registry queries and trusted position-accounting entrypoints.
pub use crate::precompile::ICca;
pub use crate::runtime::{position_opened, position_voided};
use crate::{schema::CcaContract, state::decode_state};
use alloy_primitives::{Address, U256};
use outbe_primitives::{error::Result, storage::StorageHandle};

pub fn cca_state(storage: &StorageHandle<'_>, cca: Address) -> Result<ICca::State> {
    match CcaContract::new(storage.clone()).records.get(cca)? {
        Some(record) => decode_state(record.state),
        None => Ok(ICca::State::Unknown),
    }
}

pub fn is_active(storage: &StorageHandle<'_>, cca: Address) -> Result<bool> {
    Ok(cca_state(storage, cca)? as u8 == ICca::State::Active as u8)
}

pub fn get_cca(storage: &StorageHandle<'_>, cca: Address) -> Result<ICca::Cca> {
    let record = CcaContract::new(storage.clone()).records.get(cca)?;
    match record {
        Some(r) => Ok(ICca::Cca {
            state: decode_state(r.state)?,
            selfBond: r.self_bond,
            unbondAmount: r.unbond_amount,
            unbondCompleteTime: r.unbond_complete_time,
            rewardWeight: r.reward_weight,
            claimableRewards: r.claimable_rewards,
        }),
        None => Ok(ICca::Cca {
            state: ICca::State::Unknown,
            selfBond: U256::ZERO,
            unbondAmount: U256::ZERO,
            unbondCompleteTime: 0,
            rewardWeight: U256::ZERO,
            claimableRewards: U256::ZERO,
        }),
    }
}
