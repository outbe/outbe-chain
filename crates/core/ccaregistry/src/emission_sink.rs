//! Daily CCA rewards, sampled when Cycle settles the emission day.
use crate::{
    errors::CcaError,
    precompile::ICcaRegistry,
    schema::{address_day_key, CcaContract},
};
use alloy_primitives::U256;
use outbe_common::distribution::calculate_distribution_with_cap;
use outbe_primitives::{
    addresses::CCA_REGISTRY_ADDRESS, block::BlockRuntimeContext, error::Result,
    units::checked_protocol_to_native,
};

/// Returns undistributed six-decimal emission units for terminal Metadosis.
/// Cycle owns the exactly-once day guard and its enclosing transaction.
pub fn distribute_daily(ctx: &BlockRuntimeContext, day: u32, amount: U256) -> Result<U256> {
    ctx.storage.with_checkpoint(|| {
        if amount.is_zero() {
            return Ok(U256::ZERO);
        }
        let mut contract = CcaContract::new(ctx.storage.clone());
        // TODO: O(active CCAs) per daily settlement; fine because of a few active CCAs.
        let mut weights = Vec::new();
        for cca in contract.active.read_all()? {
            let weight = contract
                .gratis_sum_per_utc_day
                .read(&address_day_key(cca, day))?;
            if !weight.is_zero() {
                weights.push((cca, weight));
            }
        }
        let (rewards, excess) =
            calculate_distribution_with_cap(amount, &weights).map_err(|_| CcaError::Arithmetic)?;
        for reward in rewards {
            let cca = reward.address;
            let share = reward.reward_amount;
            if share.is_zero() {
                continue;
            }
            let native = checked_protocol_to_native(share).ok_or(CcaError::Arithmetic)?;
            let reward = contract
                .reward_amounts
                .read(&cca)?
                .checked_add(native)
                .ok_or(CcaError::Arithmetic)?;
            contract.reward_amounts.write(&cca, reward)?;
            ctx.storage.increase_balance(CCA_REGISTRY_ADDRESS, native)?;
            contract.emit(ICcaRegistry::RewardAccrued {
                cca,
                utcDay: day,
                amount: native,
            })?;
        }
        Ok(excess)
    })
}
