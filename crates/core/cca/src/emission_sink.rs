//! Daily CCA rewards, sampled when Cycle settles the emission day.
use crate::{errors::CcaError, precompile::ICca, schema::CcaContract};
use alloy_primitives::{U256, U512};
use outbe_primitives::{
    addresses::CCA_ADDRESS, block::BlockRuntimeContext, error::Result,
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
        let mut total = U256::ZERO;
        for cca in contract.active.read_all()? {
            let weight = contract
                .gratis_sum_per_utc_day
                .read(&CcaContract::reward_weight_key(cca, day))?;
            total = total.checked_add(weight).ok_or(CcaError::Arithmetic)?;
            weights.push((cca, weight));
        }
        if total.is_zero() {
            return Ok(amount);
        }
        let mut distributed = U256::ZERO;
        for (cca, weight) in weights {
            // The product of two U256 values fits U512. Since weight <= total,
            // the quotient is <= amount and fits U256; still check the conversion.
            let wide_share = U512::from(amount) * U512::from(weight) / U512::from(total);
            if wide_share > U512::from(U256::MAX) {
                return Err(CcaError::Arithmetic.into());
            }
            let share = wide_share.wrapping_to::<U256>();
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
            ctx.storage.increase_balance(CCA_ADDRESS, native)?;
            contract.emit(ICca::RewardAccrued {
                cca,
                utcDay: day,
                amount: native,
            })?;
            distributed = distributed.checked_add(share).ok_or(CcaError::Arithmetic)?;
        }
        amount
            .checked_sub(distributed)
            .ok_or_else(|| CcaError::Arithmetic.into())
    })
}
