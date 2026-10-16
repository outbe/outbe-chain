use super::*;

pub(super) fn apply(
    contract: &mut TributeContract<'_>,
    day: WorldwideDay,
) -> Result<TributeForfeitureReceipt> {
    let mut totals = contract.get_day_totals(day)?;
    if !totals.initialized || !totals.is_sealed {
        return Err(TributeError::CapacityForfeitureRequiresSealedPartition.into());
    }
    if contract.retired_days.read(&day)? {
        return Err(TributeError::CapacityForfeitureRequiresUnretiredPartition.into());
    }
    let forfeited_count = totals.tribute_count;
    let forfeited_nominal = totals.tribute_nominal_total_minor;
    let supply = contract
        .total_supply
        .read()?
        .checked_sub(u64::from(forfeited_count))
        .ok_or(TributeError::TotalSupplyUnderflowDuringCapacityForfeiture)?;
    contract.total_supply.write(supply)?;
    contract.freeze_day_nominal(&totals)?;
    contract.reset_day_nominal(&totals)?;
    totals.tribute_count = 0;
    totals.tribute_nominal_total_minor = U256::ZERO;
    contract.store_day_totals(&totals)?;
    let retirement_outcome = contract.retire_completed_partition_inner(day)?;
    if retirement_outcome != RetirementOutcome::Requested {
        return Err(TributeError::CapacityForfeitureRequiresUnretiredPartition.into());
    }
    Ok(TributeForfeitureReceipt {
        worldwide_day: day,
        forfeited_count,
        forfeited_nominal,
        source_generation: 0,
        retired_generation: 1,
        retirement_outcome,
    })
}
