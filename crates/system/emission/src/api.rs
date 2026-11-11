use alloy_primitives::U256;
use outbe_primitives::{
    error::Result,
    storage::StorageHandle,
    time::{date_key_to_utc_timestamp, next_date_key, WorldwideDay},
};

use crate::{
    errors::EmissionError,
    schema::{emission_day_state, metadosis_state, EmissionContract, EmissionDay},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmissionDayProjection {
    pub day: u32,
    pub wwd: WorldwideDay,
    pub metadosis_start: u64,
    pub state: u8,
    pub metadosis_state: u8,
    pub emission_limit: u128,
    pub emission_limit_minor: U256,
    pub metadosis_limit: u128,
    pub lysis_limit: u128,
    pub desis_limit: u128,
    pub validators: u128,
    pub waa: u128,
    pub sra: u128,
    pub cca: u128,
    pub nods: u128,
    pub promis_limit: u128,
}

impl From<EmissionDay> for EmissionDayProjection {
    fn from(record: EmissionDay) -> Self {
        Self {
            day: record.day,
            wwd: record.wwd,
            metadosis_start: record.metadosis_start,
            state: record.state,
            metadosis_state: record.metadosis_state,
            emission_limit: record.emission_limit,
            emission_limit_minor: U256::from(record.emission_limit),
            metadosis_limit: record.metadosis_limit,
            lysis_limit: record.lysis_limit,
            desis_limit: record.desis_limit,
            validators: record.validators,
            waa: record.waa,
            sra: record.sra,
            cca: record.cca,
            nods: record.nods,
            promis_limit: record.promis_limit,
        }
    }
}

/// Amounts produced by daily `EmissionDay(E)` limit allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DayAllocationAmounts {
    pub emission_limit: U256,
    pub metadosis_limit: U256,
    pub validators: U256,
    pub waa: U256,
    pub sra: U256,
    pub cca: U256,
    pub promis_limit: U256,
}

/// Reward buckets in the Emission ledger, independent of reward-job storage or ABI pool IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RewardPool {
    Validator,
    Waa,
    Sra,
    Cca,
}

/// Converts a `U256` protocol minor-unit amount to `u128`, returning `EmissionError::AmountOverflow` on overflow.
#[inline]
pub fn to_u128_checked(amount: U256) -> Result<u128> {
    u128::try_from(amount).map_err(|_| EmissionError::AmountOverflow.into())
}

/// Computes `metadosis_start = 00:00:01 UTC` of `next_date_key(day)`.
pub fn compute_metadosis_start(day: u32) -> Result<u64> {
    if !WorldwideDay::new(day).is_valid() {
        return Err(EmissionError::InvalidEmissionDay { day }.into());
    }
    let next_day = next_date_key(day).map_err(EmissionError::from)?;
    date_key_to_utc_timestamp(next_day)
        .map_err(EmissionError::from)?
        .checked_add(1)
        .ok_or_else(|| EmissionError::WaitingWindowOverflow.into())
}

/// Reads one typed `EmissionDayProjection` by its `YYYYMMDD` UTC day key.
pub fn emission_day(storage: StorageHandle<'_>, day: u32) -> Result<Option<EmissionDayProjection>> {
    Ok(EmissionContract::new(storage)
        .emission_days
        .get(day)?
        .map(EmissionDayProjection::from))
}

/// Reads the raw `EmissionDay` record by its `YYYYMMDD` UTC day key.
pub fn get_emission_day_record(
    storage: StorageHandle<'_>,
    day: u32,
) -> Result<Option<EmissionDay>> {
    EmissionContract::new(storage).emission_days.get(day)
}

/// Writes or creates an `EmissionDay` record directly.
pub fn put_emission_day_record(storage: StorageHandle<'_>, record: &EmissionDay) -> Result<()> {
    let contract = EmissionContract::new(storage);
    if contract.emission_days.get(record.day)?.is_some() {
        contract.emission_days.update(record)
    } else {
        contract.emission_days.create(record)
    }
}

/// Links `WorldwideDay` `wwd` (`D`) to `EmissionDay` `day` (`E`).
///
/// - Creates a `Pending` shell with `metadosis_start = 00:00:01 UTC` of `E+1` if `day` does not exist.
/// - Idempotent no-op if `day` is already linked to the same `wwd`.
/// - Reverts with `DayAlreadyAllocated` if `day` has already left `Pending` with a different `wwd`.
/// - Reverts with `ConflictingWwdLink` if `day` is already linked to a different non-zero `wwd`.
pub fn link_wwd(storage: StorageHandle<'_>, day: u32, wwd: WorldwideDay) -> Result<()> {
    if !WorldwideDay::new(day).is_valid() || (wwd.value() != 0 && !wwd.is_valid()) {
        return Err(EmissionError::InvalidEmissionDay { day }.into());
    }
    let contract = EmissionContract::new(storage);
    if let Some(mut existing) = contract.emission_days.get(day)? {
        if existing.wwd == wwd {
            return Ok(());
        }
        if existing.state != emission_day_state::PENDING {
            return Err(EmissionError::DayAlreadyAllocated {
                day,
                state: existing.state,
            }
            .into());
        }
        if existing.wwd.value() != 0 {
            return Err(EmissionError::ConflictingWwdLink {
                day,
                existing_wwd: existing.wwd.value(),
                attempted_wwd: wwd.value(),
            }
            .into());
        }
        existing.wwd = wwd;
        contract.emission_days.update(&existing)?;
        return Ok(());
    }

    let metadosis_start = compute_metadosis_start(day)?;
    let record = EmissionDay::new_pending(day, wwd, metadosis_start);
    contract.emission_days.create(&record)
}

/// Ensures an `EmissionDay` record exists for `day` (creating an empty-day `wwd = 0` shell if absent).
pub fn ensure_emission_day_shell(storage: StorageHandle<'_>, day: u32) -> Result<EmissionDay> {
    let contract = EmissionContract::new(storage);
    if let Some(existing) = contract.emission_days.get(day)? {
        return Ok(existing);
    }
    let metadosis_start = compute_metadosis_start(day)?;
    let record = EmissionDay::new_pending(day, WorldwideDay::new(0), metadosis_start);
    contract.emission_days.create(&record)?;
    Ok(record)
}

/// Validates the conservation rule `emission_limit == validators + waa + sra + cca + nods + promis_limit`.
pub fn check_conservation(record: &EmissionDay) -> Result<()> {
    let landed_sum = record.landed_sum().ok_or(EmissionError::AmountOverflow)?;
    if landed_sum != record.emission_limit {
        return Err(EmissionError::ConservationInvariantViolated {
            day: record.day,
            expected_limit: record.emission_limit,
            landed_sum,
        }
        .into());
    }
    Ok(())
}

/// Records the daily emission allocation for `day` and moves `state` from `Pending` to `Processing`.
pub fn record_day_allocation(
    storage: StorageHandle<'_>,
    day: u32,
    amounts: DayAllocationAmounts,
) -> Result<EmissionDay> {
    let mut record = ensure_emission_day_shell(storage.clone(), day)?;
    if record.state != emission_day_state::PENDING {
        return Err(EmissionError::DayAlreadyAllocated {
            day,
            state: record.state,
        }
        .into());
    }
    record.emission_limit = to_u128_checked(amounts.emission_limit)?;
    record.metadosis_limit = to_u128_checked(amounts.metadosis_limit)?;
    record.validators = to_u128_checked(amounts.validators)?;
    record.waa = to_u128_checked(amounts.waa)?;
    record.sra = to_u128_checked(amounts.sra)?;
    record.cca = to_u128_checked(amounts.cca)?;
    record.promis_limit = to_u128_checked(amounts.promis_limit)?;
    record.state = emission_day_state::PROCESSING;
    EmissionContract::new(storage)
        .emission_days
        .update(&record)?;
    Ok(record)
}

/// Records a failed daily allocation: all of `emission_limit` lands in `promis_limit`,
/// `metadosis_state` becomes `Cancelled`, and `state` becomes `Completed`.
pub fn record_failed_day_allocation(
    storage: StorageHandle<'_>,
    day: u32,
    emission_limit: U256,
) -> Result<EmissionDay> {
    let mut record = ensure_emission_day_shell(storage.clone(), day)?;
    if record.state != emission_day_state::PENDING {
        return Err(EmissionError::DayAlreadyAllocated {
            day,
            state: record.state,
        }
        .into());
    }
    let limit_u128 = to_u128_checked(emission_limit)?;
    record.emission_limit = limit_u128;
    record.metadosis_limit = 0;
    record.lysis_limit = 0;
    record.desis_limit = 0;
    record.validators = 0;
    record.waa = 0;
    record.sra = 0;
    record.cca = 0;
    record.nods = 0;
    record.promis_limit = limit_u128;
    record.metadosis_state = metadosis_state::CANCELLED;
    check_conservation(&record)?;
    record.state = emission_day_state::COMPLETED;
    EmissionContract::new(storage)
        .emission_days
        .update(&record)?;
    Ok(record)
}

/// Marks Metadosis of `day` as `Completed` for an empty day (`wwd == 0` or `tribute_count == 0`),
/// routing `metadosis_limit` into `promis_limit`.
pub fn record_empty_day_metadosis_completed(
    storage: StorageHandle<'_>,
    day: u32,
) -> Result<EmissionDay> {
    let contract = EmissionContract::new(storage);
    let mut record = contract
        .emission_days
        .get(day)?
        .ok_or(EmissionError::EmissionDayNotFound)?;
    if record.metadosis_state != metadosis_state::PENDING {
        return Err(EmissionError::InvalidMetadosisStateTransition {
            day,
            from_state: record.metadosis_state,
            to_state: metadosis_state::COMPLETED,
        }
        .into());
    }
    record.lysis_limit = 0;
    record.desis_limit = 0;
    record.nods = 0;
    record.promis_limit = record
        .promis_limit
        .checked_add(record.metadosis_limit)
        .ok_or(EmissionError::AmountOverflow)?;
    record.metadosis_state = metadosis_state::COMPLETED;
    contract.emission_days.update(&record)?;
    Ok(record)
}

/// Settlement amounts produced when Metadosis completes Lysis and Desis for an `EmissionDay`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadosisCompletedAmounts {
    pub lysis_limit: U256,
    pub desis_limit: U256,
    pub nods: U256,
    pub promis_limit_added: U256,
}

/// Marks Metadosis of `day` as `Completed` after Lysis and Desis briefing, recording
/// `lysis_limit`, `desis_limit`, `nods`, and the unused Metadosis / Lysis portion credited to `PromisLimit`.
pub fn record_metadosis_completed(
    storage: StorageHandle<'_>,
    day: u32,
    amounts: MetadosisCompletedAmounts,
) -> Result<EmissionDay> {
    let contract = EmissionContract::new(storage);
    let mut record = contract
        .emission_days
        .get(day)?
        .ok_or(EmissionError::EmissionDayNotFound)?;
    if record.metadosis_state != metadosis_state::PENDING {
        return Err(EmissionError::InvalidMetadosisStateTransition {
            day,
            from_state: record.metadosis_state,
            to_state: metadosis_state::COMPLETED,
        }
        .into());
    }
    record.lysis_limit = to_u128_checked(amounts.lysis_limit)?;
    record.desis_limit = to_u128_checked(amounts.desis_limit)?;
    record.nods = to_u128_checked(amounts.nods)?;
    let added = to_u128_checked(amounts.promis_limit_added)?;
    record.promis_limit = record
        .promis_limit
        .checked_add(added)
        .ok_or(EmissionError::AmountOverflow)?;
    record.metadosis_state = metadosis_state::COMPLETED;
    contract.emission_days.update(&record)?;
    Ok(record)
}

/// Marks Metadosis of `day` as `Cancelled` when `metadosis_limit == 0`.
pub fn record_metadosis_canceled(storage: StorageHandle<'_>, day: u32) -> Result<EmissionDay> {
    let contract = EmissionContract::new(storage);
    let mut record = contract
        .emission_days
        .get(day)?
        .ok_or(EmissionError::EmissionDayNotFound)?;
    if record.metadosis_state != metadosis_state::PENDING || record.metadosis_limit != 0 {
        return Err(EmissionError::InvalidMetadosisStateTransition {
            day,
            from_state: record.metadosis_state,
            to_state: metadosis_state::CANCELLED,
        }
        .into());
    }
    record.lysis_limit = 0;
    record.desis_limit = 0;
    record.nods = 0;
    record.metadosis_state = metadosis_state::CANCELLED;
    contract.emission_days.update(&record)?;
    Ok(record)
}

/// Marks Metadosis of `day` as `Failed` and routes `metadosis_limit` to `promis_limit`.
pub fn record_metadosis_failed(
    storage: StorageHandle<'_>,
    day: u32,
) -> Result<(EmissionDay, U256)> {
    let contract = EmissionContract::new(storage);
    let mut record = contract
        .emission_days
        .get(day)?
        .ok_or(EmissionError::EmissionDayNotFound)?;
    if record.metadosis_state != metadosis_state::PENDING {
        return Err(EmissionError::InvalidMetadosisStateTransition {
            day,
            from_state: record.metadosis_state,
            to_state: metadosis_state::FAILED,
        }
        .into());
    }
    let returned = U256::from(record.metadosis_limit);
    record.lysis_limit = 0;
    record.desis_limit = 0;
    record.nods = 0;
    record.promis_limit = record
        .promis_limit
        .checked_add(record.metadosis_limit)
        .ok_or(EmissionError::AmountOverflow)?;
    record.metadosis_state = metadosis_state::FAILED;
    contract.emission_days.update(&record)?;
    Ok((record, returned))
}

/// Moves reward-pool rounding residue into the day's `promis_limit` ledger bucket.
///
/// Requires an existing `Processing` day and preserves conservation, including the unsettled
/// Metadosis allocation while it is pending. The caller must atomically credit the actual
/// Promis limit and finalize its reward job; this function only updates the Emission ledger.
pub fn record_reward_pool_residue(
    storage: StorageHandle<'_>,
    day: u32,
    pool: RewardPool,
    residue: U256,
) -> Result<EmissionDay> {
    let contract = EmissionContract::new(storage);
    let mut record = contract
        .emission_days
        .get(day)?
        .ok_or(EmissionError::EmissionDayNotFound)?;
    if record.state != emission_day_state::PROCESSING {
        return Err(EmissionError::InvalidStateTransition {
            day,
            from_state: record.state,
            to_state: emission_day_state::PROCESSING,
        }
        .into());
    }
    // EmissionDay stores protocol-unit buckets in u128; reject values outside that bound.
    let residue_u128 = to_u128_checked(residue)?;
    let bucket = match pool {
        RewardPool::Validator => &mut record.validators,
        RewardPool::Waa => &mut record.waa,
        RewardPool::Sra => &mut record.sra,
        RewardPool::Cca => &mut record.cca,
    };
    *bucket = bucket
        .checked_sub(residue_u128)
        .ok_or(EmissionError::AmountOverflow)?;
    record.promis_limit = record
        .promis_limit
        .checked_add(residue_u128)
        .ok_or(EmissionError::AmountOverflow)?;
    let unsettled = match record.metadosis_state {
        metadosis_state::PENDING => record.metadosis_limit,
        metadosis_state::COMPLETED | metadosis_state::CANCELLED | metadosis_state::FAILED => 0,
        from_state => {
            return Err(EmissionError::InvalidMetadosisStateTransition {
                day,
                from_state,
                to_state: metadosis_state::COMPLETED,
            }
            .into());
        }
    };
    let landed_sum = record
        .landed_sum()
        .and_then(|sum| sum.checked_add(unsettled))
        .ok_or(EmissionError::AmountOverflow)?;
    if landed_sum != record.emission_limit {
        return Err(EmissionError::ConservationInvariantViolated {
            day,
            expected_limit: record.emission_limit,
            landed_sum,
        }
        .into());
    }
    contract.emission_days.update(&record)?;
    Ok(record)
}

/// Finalizes an `EmissionDay` into `Completed` after verifying the conservation invariant.
pub fn complete_emission_day(storage: StorageHandle<'_>, day: u32) -> Result<EmissionDay> {
    let contract = EmissionContract::new(storage);
    let mut record = contract
        .emission_days
        .get(day)?
        .ok_or(EmissionError::EmissionDayNotFound)?;
    if record.state == emission_day_state::COMPLETED {
        return Ok(record);
    }
    if record.state != emission_day_state::PROCESSING {
        return Err(EmissionError::InvalidStateTransition {
            day,
            from_state: record.state,
            to_state: emission_day_state::COMPLETED,
        }
        .into());
    }
    if record.metadosis_state == metadosis_state::PENDING {
        return Err(EmissionError::InvalidMetadosisStateTransition {
            day,
            from_state: record.metadosis_state,
            to_state: metadosis_state::COMPLETED,
        }
        .into());
    }
    check_conservation(&record)?;
    record.state = emission_day_state::COMPLETED;
    contract.emission_days.update(&record)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::error::PrecompileError;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    fn assert_emission_error(actual: PrecompileError, expected: EmissionError) {
        match (actual, PrecompileError::from(expected)) {
            (PrecompileError::RevertBytes(actual), PrecompileError::RevertBytes(expected)) => {
                assert_eq!(actual, expected);
            }
            (PrecompileError::Fatal(actual), PrecompileError::Fatal(expected)) => {
                assert_eq!(actual, expected);
            }
            (actual, expected) => panic!("expected {expected:?}, got {actual:?}"),
        }
    }

    #[test]
    fn link_wwd_is_idempotent_and_rejects_conflicts_or_allocated_days() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);

        let day = 20261022;
        let wwd = WorldwideDay::new(20261001);
        link_wwd(storage.clone(), day, wwd).unwrap();
        // Idempotent repeat:
        link_wwd(storage.clone(), day, wwd).unwrap();

        let proj = emission_day(storage.clone(), day).unwrap().unwrap();
        assert_eq!(proj.wwd, wwd);
        assert_eq!(proj.state, emission_day_state::PENDING);
        assert_eq!(proj.metadosis_start, compute_metadosis_start(day).unwrap());

        // Conflicting WWD is rejected:
        let err = link_wwd(storage.clone(), day, WorldwideDay::new(20261002)).unwrap_err();
        assert!(matches!(
            err,
            outbe_primitives::error::PrecompileError::RevertBytes(_)
        ));

        // Empty day shell (`wwd = 0`) can be linked while Pending, then rejected once allocated:
        let empty_day = 20261023;
        ensure_emission_day_shell(storage.clone(), empty_day).unwrap();
        link_wwd(storage.clone(), empty_day, WorldwideDay::new(20261002)).unwrap();

        record_day_allocation(
            storage.clone(),
            empty_day,
            DayAllocationAmounts {
                emission_limit: U256::from(100u64),
                metadosis_limit: U256::from(84u64),
                validators: U256::from(4u64),
                waa: U256::from(4u64),
                sra: U256::from(4u64),
                cca: U256::from(4u64),
                promis_limit: U256::ZERO,
            },
        )
        .unwrap();
        assert!(link_wwd(storage, empty_day, WorldwideDay::new(20261003)).is_err());
    }

    #[test]
    fn conservation_invariant_checked_before_completed() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let day = 20261001;

        record_day_allocation(
            storage.clone(),
            day,
            DayAllocationAmounts {
                emission_limit: U256::from(1000u64),
                metadosis_limit: U256::from(840u64),
                validators: U256::from(40u64),
                waa: U256::from(40u64),
                sra: U256::from(40u64),
                cca: U256::from(40u64),
                promis_limit: U256::ZERO,
            },
        )
        .unwrap();

        // Cannot complete while metadosis_state is still Pending:
        assert!(complete_emission_day(storage.clone(), day).is_err());

        // Complete empty-day metadosis (routes 840 -> promis_limit):
        record_empty_day_metadosis_completed(storage.clone(), day).unwrap();

        // Conservation holds: 40+40+40+40+0+840 == 1000
        let completed = complete_emission_day(storage.clone(), day).unwrap();
        assert_eq!(completed.state, emission_day_state::COMPLETED);
        assert_eq!(completed.promis_limit, 840);
    }

    #[test]
    fn gap_ed10_day_emission_limit_states() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let amounts = DayAllocationAmounts {
            emission_limit: U256::from(100u64),
            metadosis_limit: U256::from(84u64),
            validators: U256::from(4u64),
            waa: U256::from(4u64),
            sra: U256::from(4u64),
            cca: U256::from(4u64),
            promis_limit: U256::ZERO,
        };

        let day = 20261001;
        let shell = ensure_emission_day_shell(storage.clone(), day).unwrap();
        assert_eq!(shell.state, emission_day_state::PENDING);
        let allocated = record_day_allocation(storage.clone(), day, amounts).unwrap();
        assert_eq!(allocated.state, emission_day_state::PROCESSING);
        record_metadosis_failed(storage.clone(), day).unwrap();
        let completed = complete_emission_day(storage.clone(), day).unwrap();
        assert_eq!(completed.state, emission_day_state::COMPLETED);

        let failed_day = 20261002;
        let failed =
            record_failed_day_allocation(storage.clone(), failed_day, U256::from(100u64)).unwrap();
        assert_eq!(failed.state, emission_day_state::COMPLETED);
        assert_eq!(failed.promis_limit, 100);

        let unknown_state = 20261003;
        let mut record = EmissionDay::new_pending(unknown_state, WorldwideDay::new(0), 1);
        record.state = 1;
        put_emission_day_record(storage.clone(), &record).unwrap();
        let expected = EmissionError::DayAlreadyAllocated {
            day: unknown_state,
            state: 1,
        };
        assert_emission_error(
            record_day_allocation(storage.clone(), unknown_state, amounts).unwrap_err(),
            expected,
        );
        assert_emission_error(
            record_failed_day_allocation(storage, unknown_state, U256::from(100u64)).unwrap_err(),
            expected,
        );
    }

    #[test]
    fn gap_ed09_day_emission_limit_rule_holds() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let amounts = DayAllocationAmounts {
            emission_limit: U256::from(100u64),
            metadosis_limit: U256::from(84u64),
            validators: U256::from(4u64),
            waa: U256::from(4u64),
            sra: U256::from(4u64),
            cca: U256::from(4u64),
            promis_limit: U256::ZERO,
        };

        let day = 20261001;
        record_day_allocation(storage.clone(), day, amounts).unwrap();
        record_metadosis_failed(storage.clone(), day).unwrap();
        let completed = complete_emission_day(storage.clone(), day).unwrap();
        assert_eq!(completed.landed_sum(), Some(completed.emission_limit));

        let broken = 20261002;
        record_day_allocation(storage.clone(), broken, amounts).unwrap();
        record_metadosis_failed(storage.clone(), broken).unwrap();
        let mut record = get_emission_day_record(storage.clone(), broken)
            .unwrap()
            .unwrap();
        record.validators += 1;
        put_emission_day_record(storage.clone(), &record).unwrap();
        let mismatch = EmissionError::ConservationInvariantViolated {
            day: broken,
            expected_limit: 100,
            landed_sum: 101,
        };

        let error = complete_emission_day(storage.clone(), broken).unwrap_err();
        assert!(matches!(error, PrecompileError::Fatal(_)));
        assert_emission_error(error, mismatch);
        let error =
            record_reward_pool_residue(storage.clone(), broken, RewardPool::Waa, U256::from(1))
                .unwrap_err();
        assert_emission_error(error, mismatch);
        assert_eq!(
            get_emission_day_record(storage, broken).unwrap(),
            Some(record)
        );
    }

    #[test]
    fn reward_residue_moves_each_pool_before_or_after_metadosis() {
        for settle_metadosis_first in [false, true] {
            let mut provider = HashMapStorageProvider::new(1);
            let storage = StorageHandle::new(&mut provider);
            let day = 20261001;
            record_day_allocation(
                storage.clone(),
                day,
                DayAllocationAmounts {
                    emission_limit: U256::from(100),
                    metadosis_limit: U256::from(60),
                    validators: U256::from(10),
                    waa: U256::from(10),
                    sra: U256::from(10),
                    cca: U256::from(10),
                    promis_limit: U256::ZERO,
                },
            )
            .unwrap();
            if settle_metadosis_first {
                record_empty_day_metadosis_completed(storage.clone(), day).unwrap();
            }
            for (index, pool) in [
                RewardPool::Validator,
                RewardPool::Waa,
                RewardPool::Sra,
                RewardPool::Cca,
            ]
            .into_iter()
            .enumerate()
            {
                let before = get_emission_day_record(storage.clone(), day)
                    .unwrap()
                    .unwrap();
                let after =
                    record_reward_pool_residue(storage.clone(), day, pool, U256::from(3)).unwrap();
                let mut expected = before;
                match pool {
                    RewardPool::Validator => expected.validators -= 3,
                    RewardPool::Waa => expected.waa -= 3,
                    RewardPool::Sra => expected.sra -= 3,
                    RewardPool::Cca => expected.cca -= 3,
                }
                expected.promis_limit += 3;
                assert_eq!(after, expected);
                assert_eq!(
                    after.promis_limit,
                    if settle_metadosis_first { 60 } else { 0 } + 3 * (index as u128 + 1)
                );
                assert_eq!(
                    get_emission_day_record(storage.clone(), day).unwrap(),
                    Some(after)
                );
            }
            if !settle_metadosis_first {
                record_metadosis_failed(storage.clone(), day).unwrap();
            }
            let completed = complete_emission_day(storage, day).unwrap();
            assert_eq!(completed.promis_limit, 72);
            assert_eq!(completed.landed_sum(), Some(100));
        }
    }

    #[test]
    fn reward_residue_requires_existing_processing_day_even_for_zero() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let day = 20261001;
        let error = record_reward_pool_residue(storage.clone(), day, RewardPool::Waa, U256::ZERO)
            .unwrap_err();
        assert_emission_error(error, EmissionError::EmissionDayNotFound);
        assert_eq!(get_emission_day_record(storage.clone(), day).unwrap(), None);

        for state in [
            emission_day_state::PENDING,
            emission_day_state::COMPLETED,
            u8::MAX,
        ] {
            let mut record = EmissionDay::new_pending(day, WorldwideDay::new(0), 1);
            record.state = state;
            put_emission_day_record(storage.clone(), &record).unwrap();
            let error =
                record_reward_pool_residue(storage.clone(), day, RewardPool::Waa, U256::ZERO)
                    .unwrap_err();
            assert_emission_error(
                error,
                EmissionError::InvalidStateTransition {
                    day,
                    from_state: state,
                    to_state: emission_day_state::PROCESSING,
                },
            );
            assert_eq!(
                get_emission_day_record(storage.clone(), day).unwrap(),
                Some(record)
            );
        }
    }

    #[test]
    fn reward_residue_checks_narrowing_underflow_and_promis_overflow_before_writing() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let day = 20261001;
        let mut record = EmissionDay::new_pending(day, WorldwideDay::new(0), 1);
        record.state = emission_day_state::PROCESSING;
        record.metadosis_state = metadosis_state::COMPLETED;
        record.emission_limit = u128::MAX;
        record.validators = u128::MAX;
        put_emission_day_record(storage.clone(), &record).unwrap();
        for (pool, residue) in [
            (RewardPool::Validator, U256::from(u128::MAX) + U256::from(1)),
            (RewardPool::Waa, U256::from(1)),
            (RewardPool::Sra, U256::from(1)),
            (RewardPool::Cca, U256::from(1)),
        ] {
            let error =
                record_reward_pool_residue(storage.clone(), day, pool, residue).unwrap_err();
            assert_emission_error(error, EmissionError::AmountOverflow);
            assert_eq!(
                get_emission_day_record(storage.clone(), day).unwrap(),
                Some(record)
            );
        }
        let after = record_reward_pool_residue(
            storage.clone(),
            day,
            RewardPool::Validator,
            U256::from(u128::MAX),
        )
        .unwrap();
        assert_eq!(after.validators, 0);
        assert_eq!(after.promis_limit, u128::MAX);
        check_conservation(&after).unwrap();

        record.promis_limit = u128::MAX;
        put_emission_day_record(storage.clone(), &record).unwrap();
        let error =
            record_reward_pool_residue(storage.clone(), day, RewardPool::Validator, U256::from(1))
                .unwrap_err();
        assert_emission_error(error, EmissionError::AmountOverflow);
        assert_eq!(get_emission_day_record(storage, day).unwrap(), Some(record));
    }

    #[test]
    fn reward_residue_rejects_corrupt_conservation_and_metadosis_state() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let day = 20261001;
        let mut record = EmissionDay::new_pending(day, WorldwideDay::new(0), 1);
        record.state = emission_day_state::PROCESSING;
        record.emission_limit = 100;
        record.metadosis_limit = 80;
        record.waa = 10;
        for metadosis_state in [metadosis_state::PENDING, metadosis_state::COMPLETED] {
            record.metadosis_state = metadosis_state;
            put_emission_day_record(storage.clone(), &record).unwrap();
            let error =
                record_reward_pool_residue(storage.clone(), day, RewardPool::Waa, U256::from(1))
                    .unwrap_err();
            assert_emission_error(
                error,
                EmissionError::ConservationInvariantViolated {
                    day,
                    expected_limit: 100,
                    landed_sum: if metadosis_state == metadosis_state::PENDING {
                        90
                    } else {
                        10
                    },
                },
            );
            assert_eq!(
                get_emission_day_record(storage.clone(), day).unwrap(),
                Some(record)
            );
        }
        record.metadosis_state = u8::MAX;
        put_emission_day_record(storage.clone(), &record).unwrap();
        assert!(
            record_reward_pool_residue(storage.clone(), day, RewardPool::Waa, U256::ZERO).is_err()
        );
        assert_eq!(get_emission_day_record(storage, day).unwrap(), Some(record));
    }

    #[test]
    fn record_metadosis_canceled_requires_pending_zero_limit_and_sets_cancelled() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        let day = 20261001;

        record_day_allocation(
            storage.clone(),
            day,
            DayAllocationAmounts {
                emission_limit: U256::from(100u64),
                metadosis_limit: U256::from(20u64),
                validators: U256::from(20u64),
                waa: U256::from(20u64),
                sra: U256::from(20u64),
                cca: U256::from(20u64),
                promis_limit: U256::ZERO,
            },
        )
        .unwrap();

        let err = record_metadosis_canceled(storage.clone(), day).unwrap_err();
        assert_emission_error(
            err,
            EmissionError::InvalidMetadosisStateTransition {
                day,
                from_state: metadosis_state::PENDING,
                to_state: metadosis_state::CANCELLED,
            },
        );

        let mut zero_limit_record = get_emission_day_record(storage.clone(), day)
            .unwrap()
            .unwrap();
        zero_limit_record.metadosis_limit = 0;
        zero_limit_record.promis_limit = 20;
        put_emission_day_record(storage.clone(), &zero_limit_record).unwrap();

        let canceled = record_metadosis_canceled(storage.clone(), day).unwrap();
        assert_eq!(canceled.metadosis_state, metadosis_state::CANCELLED);
        let completed = complete_emission_day(storage.clone(), day).unwrap();
        assert_eq!(completed.state, emission_day_state::COMPLETED);
        assert_eq!(completed.metadosis_state, metadosis_state::CANCELLED);

        let repeat_err = record_metadosis_canceled(storage, day).unwrap_err();
        assert_emission_error(
            repeat_err,
            EmissionError::InvalidMetadosisStateTransition {
                day,
                from_state: metadosis_state::CANCELLED,
                to_state: metadosis_state::CANCELLED,
            },
        );
    }
}
