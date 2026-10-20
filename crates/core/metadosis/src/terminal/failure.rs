use super::MetadosisFailureReceipt;
use crate::{
    aggregate::{ValidatedWwdAggregate, WwdStatus},
    commit::commit_outer_transition,
    errors::MetadosisStorageCorruptionError,
    precompile::IMetadosis,
    reducer::{reduce_outer_wwd, OuterWwdEvent},
    schema::MetadosisContract,
};
use alloy_primitives::U256;
use outbe_primitives::{error::Result, storage::StorageHandle, time::WorldwideDay};
use outbe_promislimit::PromisLimitContract;
use outbe_tribute::TributeContract;

struct FailureSettlement {
    block_number: u64,
    worldwide_day: WorldwideDay,
    unused_limit: U256,
}

impl MetadosisContract<'_> {
    pub(crate) fn failure_value_routed(&self, _day: WorldwideDay, day_limit: U256) -> Result<U256> {
        Ok(day_limit)
    }
}

/// Closes the existing deterministic day-failure settlement atomically.
pub(crate) fn fail_worldwide_day(
    storage: StorageHandle<'_>,
    block_number: u64,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    let aggregate = ValidatedWwdAggregate::load_and_validate(storage.clone())?;
    let current = aggregate
        .record(worldwide_day)
        .ok_or(MetadosisStorageCorruptionError::FailureMissingPersistedWorldwideDay)?;
    let mut metadosis = MetadosisContract::new(storage.clone());
    let unused_limit =
        metadosis.failure_value_routed(worldwide_day, current.metadosis_limit_minor)?;

    if current.status == WwdStatus::Failed {
        metadosis
            .read_metadosis_failure_receipt(worldwide_day, unused_limit)?
            .ok_or(MetadosisStorageCorruptionError::FailedWwdMissingFailureReceipt)?;
        return Ok(());
    }
    if current.status.is_terminal() {
        return Err(MetadosisStorageCorruptionError::FailureCannotReplaceCompletedWwd.into());
    }

    let settlement = FailureSettlement {
        block_number,
        worldwide_day,
        unused_limit,
    };
    let forfeited_nominal = route_failure(&mut metadosis, &settlement)?;
    let transition = reduce_outer_wwd(Some(current), OuterWwdEvent::EmergencyFail)?;
    commit_outer_transition(&mut metadosis, worldwide_day, &transition, block_number)?;
    metadosis.emit(failed_day_event(&settlement, forfeited_nominal))
}

fn route_failure(
    metadosis: &mut MetadosisContract<'_>,
    settlement: &FailureSettlement,
) -> Result<U256> {
    let FailureSettlement {
        block_number,
        worldwide_day,
        unused_limit,
    } = *settlement;
    let storage = metadosis.storage.clone();
    let credit = PromisLimitContract::new(storage.clone()).checked_credit(unused_limit)?;
    // The Promis Limit credit precedes native Tribute retirement.
    // The failure receipt, outer transition, and event follow retirement.
    // The owning command checkpoint covers every write and event.
    let tribute = TributeContract::new(storage).forfeit_sealed_partition(worldwide_day)?;
    metadosis.write_metadosis_failure_receipt(MetadosisFailureReceipt {
        worldwide_day,
        value_routed: credit.credited,
        promis_limit_before: credit.before,
        promis_limit_after: credit.after,
        retirement: tribute.retirement_outcome,
        block_number,
    })?;
    Ok(tribute.forfeited_nominal)
}

fn failed_day_event(
    settlement: &FailureSettlement,
    forfeited_nominal: U256,
) -> IMetadosis::MetadosisExecuted {
    let FailureSettlement {
        block_number,
        worldwide_day,
        unused_limit,
        ..
    } = *settlement;
    IMetadosis::MetadosisExecuted {
        worldwideDay: worldwide_day.into(),
        tributeNominalTotalMinor: forfeited_nominal,
        gratisDemandMinor: U256::ZERO,
        dayGratisLimitMinor: U256::ZERO,
        lysisLimitMinor: U256::ZERO,
        unusedLysisLimitMinor: U256::ZERO,
        lysisAllocationMinor: U256::ZERO,
        promisLimitReturnedMinor: unused_limit,
        status: "FAILED".into(),
        blockNumber: block_number,
    }
}
