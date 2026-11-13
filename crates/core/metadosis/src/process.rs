//! The Metadosis of a READY EmissionDay.

use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::{
    constants::METADOSIS_CANCELED_NO_LIMIT,
    lifecycle::cancel_emission_day,
    schema::{EmissionDay, MetadosisContract},
};

/// Desis limit of a day, in six-decimal protocol COEN minor units.
/// Success is the minimum of nominal minus lysis and the unused day limit plus `promis_limit_draw`.
/// Returns `None` when lysis exceeds the day limit, when that remainder plus the draw overflows, or when lysis exceeds nominal.
pub(crate) fn desis_limit(
    nominal_total: U256,
    lysis_limit_minor: U256,
    day_limit: U256,
    promis_limit_draw: U256,
) -> Option<U256> {
    let available = day_limit
        .checked_sub(lysis_limit_minor)?
        .checked_add(promis_limit_draw)?;
    Some(nominal_total.checked_sub(lysis_limit_minor)?.min(available))
}

pub(crate) fn process_ready_candidate(
    metadosis: &mut MetadosisContract<'_>,
    ctx: &BlockRuntimeContext<'_>,
    emission_day: &EmissionDay,
) -> Result<()> {
    let limit_amount = emission_day.metadosis_limit_minor();
    if limit_amount.is_zero() {
        return cancel_emission_day(metadosis, emission_day.day, METADOSIS_CANCELED_NO_LIMIT);
    }
    crate::lysis::execute(metadosis, ctx, emission_day)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desis_limit_refuses_each_checked_arm_and_returns_the_minimum() {
        let cases = [
            (
                U256::from(1u64),
                U256::from(1u64),
                U256::ZERO,
                U256::ZERO,
                None,
            ),
            (U256::MAX, U256::ZERO, U256::MAX, U256::from(1u64), None),
            (
                U256::from(4u64),
                U256::from(5u64),
                U256::from(10u64),
                U256::ZERO,
                None,
            ),
            (
                U256::from(50u64),
                U256::from(10u64),
                U256::from(40u64),
                U256::from(5u64),
                Some(U256::from(35u64)),
            ),
            (
                U256::from(30u64),
                U256::from(10u64),
                U256::from(100u64),
                U256::ZERO,
                Some(U256::from(20u64)),
            ),
            (
                U256::MAX,
                U256::ZERO,
                U256::MAX,
                U256::ZERO,
                Some(U256::MAX),
            ),
        ];
        for (nominal, lysis, day_limit, draw, expected) in cases {
            assert_eq!(
                desis_limit(nominal, lysis, day_limit, draw),
                expected,
                "nominal {nominal} lysis {lysis} day_limit {day_limit} draw {draw}"
            );
        }
    }
}
