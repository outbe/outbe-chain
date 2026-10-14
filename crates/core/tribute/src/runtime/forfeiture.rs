use super::*;

struct ForfeitureState {
    totals: crate::DayTotals,
    admission: crate::DayPreAdmission,
    authenticated_root: Option<B256>,
    sealed_root: B256,
}

pub(super) fn apply(
    contract: &mut TributeContract<'_>,
    scope: &ExecutionScope,
    day: WorldwideDay,
) -> Result<TributeForfeitureReceipt> {
    let ForfeitureState {
        mut totals,
        mut admission,
        authenticated_root,
        sealed_root,
    } = prepare(contract, scope, day)?;
    let forfeited_count = totals.tribute_count;
    let forfeited_nominal = totals.tribute_nominal_total_minor;
    let supply = contract
        .total_supply
        .read()?
        .checked_sub(u64::from(forfeited_count))
        .ok_or(TributeError::TotalSupplyUnderflowDuringCapacityForfeiture)?;
    contract.total_supply.write(supply)?;
    contract.store_day_pre_admission(&admission)?;
    contract.reset_day_nominal(&totals)?;
    totals.tribute_count = 0;
    totals.tribute_nominal_total_minor = U256::ZERO;
    contract.store_day_totals(&totals)?;

    let retirement_outcome = contract.retire_completed_partition_inner(scope, day)?;
    let expected_retirement = if authenticated_root.is_some() {
        RetirementOutcome::Requested
    } else {
        RetirementOutcome::NotPresent
    };
    if retirement_outcome != expected_retirement {
        return Err(TributeError::RetirementOutcomeContradictsAuthenticatedRoot.into());
    }

    let source_generation = admission.source_generation;
    let retired_generation = source_generation
        .checked_add(1)
        .ok_or(TributeError::SourceGenerationOverflowDuringCapacityForfeiture)?;
    admission.source_generation = retired_generation;
    contract.store_day_pre_admission(&admission)?;

    Ok(TributeForfeitureReceipt {
        worldwide_day: day,
        sealed_root,
        forfeited_count,
        forfeited_nominal,
        source_generation,
        retired_generation,
        retirement_outcome,
    })
}

fn prepare(
    contract: &TributeContract<'_>,
    scope: &ExecutionScope,
    day: WorldwideDay,
) -> Result<ForfeitureState> {
    let totals = contract.get_day_totals(day)?;
    if !contract.ocomp_profile_ready.read()? || !totals.initialized || !totals.is_sealed {
        return Err(TributeError::CapacityForfeitureRequiresFreshSealedPartition.into());
    }

    let partition = PartitionRef::TributeWwd(day);
    let authenticated_root = scope.authenticated_partition_root(partition)?;
    if totals.tribute_count != 0 && authenticated_root.is_none() {
        return Err(TributeError::PopulatedAggregateMissingAuthenticatedParent.into());
    }
    let sealed_root = authenticated_root.unwrap_or(B256::ZERO);

    let mut admission = contract.read_day_pre_admission(day)?;
    if admission.source_generation != 0 {
        return Err(TributeError::CapacityForfeitureRequiresUnretiredGenerationZero.into());
    }
    if admission.is_sealed {
        if admission.sealed_collection_root != sealed_root
            || admission.sealed_tribute_count != totals.tribute_count
            || admission.sealed_tribute_nominal_total_minor != totals.tribute_nominal_total_minor
        {
            return Err(TributeError::SealedPreAdmissionDiffersFromAuthenticatedAggregate.into());
        }
    } else {
        admission.initialized = true;
        admission.is_sealed = true;
        admission.sealed_collection_root = sealed_root;
        admission.sealed_tribute_count = totals.tribute_count;
        admission.sealed_tribute_nominal_total_minor = totals.tribute_nominal_total_minor;
    }

    Ok(ForfeitureState {
        totals,
        admission,
        authenticated_root,
        sealed_root,
    })
}
