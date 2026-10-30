//! READY EmissionDay classification and local settlement paths.

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{block::BlockRuntimeContext, error::Result};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

use crate::{
    constants::SYMBOLIC_RATE,
    lifecycle::{complete_emission_day, fail_emission_day},
    precompile::IMetadosis,
    schema::{emission_day_state, EmissionDay, MetadosisContract},
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
    pub(crate) fn calculate_metadosis(
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
    let wwd = emission_day.wwd;
    let limit_amount = emission_day.metadosis_limit_minor();
    if limit_amount.is_zero() {
        return fail_emission_day(
            metadosis,
            ctx,
            emission_day.day,
            wwd,
            "emission limit is zero",
        );
    }

    if wwd.value() == 0 {
        return settle_empty_day(metadosis, ctx, emission_day);
    }

    let mut tribute = TributeContract::new(metadosis.storage.clone());
    let mut tribute_totals = tribute.get_day_totals(wwd)?;
    if !tribute_totals.is_sealed {
        tribute.seal_day(wwd)?;
        tribute_totals = tribute.get_day_totals(wwd)?;
    }

    if tribute_totals.tribute_count == 0 {
        return settle_empty_day(metadosis, ctx, emission_day);
    }

    let calculation =
        metadosis.calculate_metadosis(tribute_totals.tribute_nominal_total_minor, limit_amount)?;
    if calculation.lysis_limit_minor.is_zero() {
        return settle_zero_allocation(
            metadosis,
            ctx,
            emission_day,
            tribute_totals.tribute_nominal_total_minor,
            calculation,
        );
    }

    crate::lysis::execute(metadosis, ctx, emission_day, calculation)
}

fn settle_empty_day(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    emission_day: &EmissionDay,
) -> Result<()> {
    let wwd = emission_day.wwd;
    let day_limit = emission_day.metadosis_limit_minor();
    let mut promis_limit = PromisLimitContract::new(ctx.storage.clone());
    let to_promis = dispatch_brief(ctx, wwd, U256::ZERO)?;
    let returned = to_promis
        .checked_add(day_limit)
        .ok_or(crate::errors::MetadosisStorageCorruptionError::DayLimitReturnOverflow)?;
    let updated = outbe_emission::api::record_empty_day_metadosis_completed(
        metadosis.storage.clone(),
        emission_day.day,
    )?;
    complete_emission_day(metadosis, &updated, emission_day_state::SUCCEEDED)?;
    if wwd.value() != 0 {
        TributeContract::new(metadosis.storage.clone()).retire_completed_partition(wwd)?;
    }
    metadosis.emit(IMetadosis::MetadosisWorldwideDayProcessed {
        worldwideDay: wwd.into(),
        metadosisLimitMinor: day_limit,
        promisLimitReturnedMinor: returned,
        status: "COMPLETED".into(),
        action: "no tributes".into(),
    })?;
    promis_limit.checked_credit(returned).map(|_| ())
}

fn settle_zero_allocation(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    emission_day: &EmissionDay,
    tribute_nominal_total: U256,
    calculation: MetadosisCalculation,
) -> Result<()> {
    let wwd = emission_day.wwd;
    let day_limit = emission_day.metadosis_limit_minor();
    let mut promis_limit = PromisLimitContract::new(ctx.storage.clone());
    let desis_limit_minor = desis_limit(
        tribute_nominal_total,
        calculation.lysis_limit_minor,
        day_limit,
        U256::ZERO,
    )
    .ok_or(crate::errors::MetadosisStorageCorruptionError::LysisLimitExceedsDayNominal)?;
    let to_promis = dispatch_brief(ctx, wwd, emission_day.day, desis_limit_minor)?;
    let returned = day_limit
        .checked_sub(calculation.lysis_limit_minor)
        .and_then(|rest| rest.checked_sub(desis_limit_minor))
        .and_then(|headroom| headroom.checked_add(to_promis))
        .ok_or(crate::errors::MetadosisStorageCorruptionError::SplitExceedsDayLimit)?;
    promis_limit.checked_credit(returned)?;
    let updated = outbe_emission::api::record_metadosis_completed(
        metadosis.storage.clone(),
        emission_day.day,
        outbe_emission::api::MetadosisCompletedAmounts {
            lysis_limit: U256::ZERO,
            desis_limit: desis_limit_minor,
            nods: U256::ZERO,
            promis_limit_added: day_limit,
        },
    )?;
    complete_emission_day(metadosis, &updated, emission_day_state::SUCCEEDED)?;
    TributeContract::new(metadosis.storage.clone()).forfeit_sealed_partition(wwd)?;
    metadosis.emit(IMetadosis::MetadosisExecuted {
        worldwideDay: wwd.into(),
        tributeNominalTotalMinor: tribute_nominal_total,
        gratisDemandMinor: calculation.gratis_demand,
        dayGratisLimitMinor: calculation.day_gratis_limit_minor,
        lysisLimitMinor: U256::ZERO,
        unusedLysisLimitMinor: U256::ZERO,
        lysisAllocationMinor: U256::ZERO,
        promisLimitReturnedMinor: returned,
        status: "COMPLETED".into(),
        blockNumber: ctx.block.block_number,
    })
}

fn dispatch_brief(
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
    emission_day: u32,
    desis_limit_minor: U256,
) -> Result<U256> {
    let is_active = !desis_limit_minor.is_zero();
    let briefed_desis_limit_minor = if is_active {
        desis_limit_minor
    } else {
        U256::ZERO
    };
    let receipt = outbe_desis::api::dispatch_auction_brief(
        ctx.storage.clone(),
        outbe_desis::api::AuctionBrief {
            worldwide_day: wwd,
            emission_day,
            desis_limit_minor: briefed_desis_limit_minor,
            has_limit: is_active,
        },
        ctx.block.timestamp,
        outbe_desis::api::BriefOverflowPolicy::ReturnToPromisLimit,
    )?;
    match receipt {
        outbe_desis::api::AuctionBriefReceipt::Accepted => desis_limit_minor
            .checked_sub(briefed_desis_limit_minor)
            .ok_or_else(|| {
                crate::errors::MetadosisStorageCorruptionError::AcceptedDesisBriefExceedsRoutingLimit
                    .into()
            }),
        outbe_desis::api::AuctionBriefReceipt::ReturnedToPromisLimit {
            reason: outbe_desis::api::AuctionBriefRejectionReason::DesisLimitExceedsAuctionDomain,
            desis_limit_minor: rejected_desis_limit_minor,
            max_accepted,
        } => {
            if rejected_desis_limit_minor != briefed_desis_limit_minor
                || rejected_desis_limit_minor <= max_accepted
            {
                return Err(
                    crate::errors::MetadosisStorageCorruptionError::DesisRejectionReceiptMismatchDispatchedLimit
                        .into(),
                );
            }
            Ok(rejected_desis_limit_minor)
        }
    }
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
                let calc = contract.calculate_metadosis(nominal, cap).unwrap();
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
