//! READY EmissionDay classification and the Metadosis calculation.

use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};
use outbe_tribute::TributeContract;

use crate::{
    constants::{METADOSIS_CANCELED_NO_LIMIT, SYMBOLIC_RATE},
    lifecycle::cancel_emission_day,
    schema::{EmissionDay, MetadosisContract},
};

/// Split of one EmissionDay in six-decimal protocol COEN minor units.
/// 1_000_000 minor units equal 1 COEN. These amounts are not native wei.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct MetadosisCalculation {
    /// `floor(nominal * SYMBOLIC_RATE / 100)`.
    pub(crate) gratis_demand: U256,
    /// Emission cap for the day, in the same minor units.
    pub(crate) day_gratis_limit_minor: U256,
    /// Minimum of `gratis_demand` and `day_gratis_limit_minor`.
    pub(crate) lysis_limit_minor: U256,
}

impl MetadosisContract<'_> {
    /// Demand is `floor(nominal * SYMBOLIC_RATE / 100)` through the quotient and remainder split.
    /// Lysis is the minimum of that demand and `emission_limit_minor`.
    /// Both inputs are six-decimal protocol COEN minor units, not native wei.
    /// At rate 32 every `U256` nominal succeeds, including `U256::MAX`.
    /// `FullPrecisionDemandOverflow` stays as a defensive guard.
    /// This comment records the source contract. It is not a runtime certificate.
    pub(crate) fn lysis_limit_of_day(
        &self,
        tribute_nominal_total: U256,
        emission_limit_minor: U256,
    ) -> Result<MetadosisCalculation> {
        let denominator = U256::from(100u64);
        let rate = U256::from(SYMBOLIC_RATE);
        let quotient = tribute_nominal_total / denominator;
        let remainder = tribute_nominal_total % denominator;
        let demand = quotient
            .checked_mul(rate)
            .and_then(|scaled| {
                remainder
                    .checked_mul(rate)
                    .and_then(|tail| scaled.checked_add(tail / denominator))
            })
            .ok_or(crate::errors::MetadosisStorageCorruptionError::FullPrecisionDemandOverflow)?;
        let day_gratis_limit_minor = emission_limit_minor;
        let lysis_limit_minor = demand.min(day_gratis_limit_minor);
        Ok(MetadosisCalculation {
            gratis_demand: demand,
            day_gratis_limit_minor,
            lysis_limit_minor,
        })
    }
}

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
    let wwd = emission_day.wwd;
    let mut nominal_total = U256::ZERO;
    if wwd.value() != 0 {
        nominal_total = TributeContract::new(metadosis.storage.clone())
            .get_day_totals(wwd)?
            .tribute_nominal_total_minor;
    }
    let calculation = metadosis.lysis_limit_of_day(nominal_total, limit_amount)?;
    crate::lysis::execute(metadosis, ctx, emission_day, calculation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigUint;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    fn wide_floor_demand(nominal: U256) -> U256 {
        let wide = BigUint::from_bytes_be(&nominal.to_be_bytes::<32>());
        let demand = wide * 32u32 / 100u32;
        let bytes = demand.to_bytes_be();
        assert!(bytes.len() <= 32, "wide demand exceeds U256");
        let mut padded = [0u8; 32];
        padded[32 - bytes.len()..].copy_from_slice(&bytes);
        U256::from_be_bytes(padded)
    }

    #[test]
    fn demand_matches_the_wide_oracle_and_lysis_respects_the_cap() {
        let mut provider = HashMapStorageProvider::new(1);
        let contract = MetadosisContract::new(StorageHandle::new(&mut provider));
        let nominals = [
            U256::ZERO,
            U256::from(99u64),
            U256::from(100u64),
            U256::from(101u64),
            U256::from(1u128) << 128,
            U256::MAX - U256::ONE,
            U256::MAX,
        ];
        let mut previous_demand = None;
        for nominal in nominals {
            let expected = wide_floor_demand(nominal);
            let mut seen = None;
            for cap in [U256::ZERO, U256::ONE, expected, U256::MAX] {
                let calc = contract.lysis_limit_of_day(nominal, cap).unwrap();
                assert_eq!(calc.gratis_demand, expected, "nominal {nominal}");
                assert!(calc.gratis_demand <= nominal, "nominal {nominal}");
                assert_eq!(calc.day_gratis_limit_minor, cap, "nominal {nominal}");
                assert_eq!(
                    calc.lysis_limit_minor,
                    expected.min(cap),
                    "nominal {nominal} cap {cap}"
                );
                seen = Some(calc.gratis_demand);
            }
            let demand = seen.unwrap();
            if let Some(earlier) = previous_demand {
                assert!(
                    demand >= earlier,
                    "demand decreased from {earlier} to {demand}"
                );
            }
            previous_demand = Some(demand);
        }
    }

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
