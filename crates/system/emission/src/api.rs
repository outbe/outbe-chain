use alloy_primitives::U256;
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};
use outbe_system_common::time::{
    date_key_to_utc_timestamp, day_number_between, next_date_key, timestamp_to_date_key,
    EmissionDate, WorldwideDay,
};

pub use crate::allocation::{compute_emission_shares, EmissionShares};
use crate::{
    day_emission::emission_limit_of_day,
    errors::EmissionError,
    schema::{desis_state, emission_day_state, metadosis_state, EmissionContract, EmissionDay},
};

/// The reward pools of a day in the Emission ledger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RewardPool {
    Validator,
    Waa,
    Sra,
    Cca,
}

const METADOSIS_START_AFTER_MIDNIGHT_SECONDS: u64 = 12 * 60 * 60;

/// Converts a `U256` protocol minor-unit amount to `u128`, returning `EmissionError::AmountOverflow` on overflow.
#[inline]
pub fn to_u128_checked(amount: U256) -> Result<u128> {
    u128::try_from(amount).map_err(|_| EmissionError::AmountOverflow.into())
}

fn to_i128_checked(amount: u128) -> Result<i128> {
    i128::try_from(amount).map_err(|_| EmissionError::AmountOverflow.into())
}

/// Metadosis of `day` starts at 12:00 UTC of the next day, when the offering window of `day` ends.
pub fn compute_metadosis_start(day: u32) -> Result<u64> {
    if !EmissionDate::new(day).is_valid() {
        return Err(EmissionError::InvalidEmissionDay { day }.into());
    }
    let next_day = next_date_key(day).map_err(EmissionError::from)?;
    date_key_to_utc_timestamp(next_day)
        .map_err(EmissionError::from)?
        .checked_add(METADOSIS_START_AFTER_MIDNIGHT_SECONDS)
        .ok_or_else(|| EmissionError::WaitingWindowOverflow.into())
}

/// Reads the stored `EmissionDay` record by its `YYYYMMDD` UTC day key.
pub fn get_emission_day(storage: StorageHandle<'_>, day: u32) -> Result<Option<EmissionDay>> {
    EmissionContract::new(storage).emission_days.get(day)
}

/// The Nod loads that the Metadosis limits of past days did not cover.
pub fn excess_nod_loads(storage: StorageHandle<'_>) -> Result<U256> {
    EmissionContract::new(storage).excess_nod_loads_minor.read()
}

/// Records the Nod loads that the Metadosis limits so far did not cover.
pub fn record_excess_nod_loads(storage: StorageHandle<'_>, amount: U256) -> Result<()> {
    EmissionContract::new(storage)
        .excess_nod_loads_minor
        .write(amount)
}

/// Creates or replaces an `EmissionDay` record with no checks. Test builds only.
#[cfg(any(test, feature = "test-utils"))]
pub fn put_emission_day(storage: StorageHandle<'_>, record: &EmissionDay) -> Result<()> {
    let contract = EmissionContract::new(storage);
    if contract.emission_days.get(record.day)?.is_some() {
        contract.emission_days.update(record)
    } else {
        contract.emission_days.create(record)
    }
}

/// Deletes the `EmissionDay` record of `day` with no checks. Test builds only.
#[cfg(any(test, feature = "test-utils"))]
pub fn delete_emission_day(storage: StorageHandle<'_>, day: u32) -> Result<()> {
    EmissionContract::new(storage).emission_days.delete(day)
}

/// The Emission Limit of `day`, by its number of days since the genesis day.
pub fn emission_limit_of(ctx: &BlockRuntimeContext<'_>, day: u32) -> Result<U256> {
    let genesis_day = timestamp_to_date_key(ctx.block.genesis_timestamp);
    let day_number = day_number_between(genesis_day, day).map_err(EmissionError::from)?;
    Ok(emission_limit_of_day(day_number))
}

/// Makes the `Pending` record of `day` with its Emission Limit and the limit of each part.
/// `wwd` is the WorldwideDay of the spends of `day`, or 0 when it has none.
pub fn create_emission_day(
    ctx: &BlockRuntimeContext<'_>,
    day: u32,
    wwd: WorldwideDay,
) -> Result<EmissionDay> {
    if !EmissionDate::new(day).is_valid() || (wwd.value() != 0 && !wwd.is_valid()) {
        return Err(EmissionError::InvalidEmissionDay { day }.into());
    }
    let contract = EmissionContract::new(ctx.storage.clone());
    if contract.emission_days.get(day)?.is_some() {
        return Err(EmissionError::EmissionDayExists { day }.into());
    }
    let limit = emission_limit_of(ctx, day)?;
    let shares = compute_emission_shares(limit)?;
    let record = EmissionDay::new(
        day,
        wwd,
        compute_metadosis_start(day)?,
        to_u128_checked(limit)?,
        [
            to_u128_checked(shares.validators)?,
            to_u128_checked(shares.cca)?,
            to_u128_checked(shares.waa)?,
            to_u128_checked(shares.sra)?,
            to_u128_checked(shares.metadosis)?,
        ],
    );
    contract.emission_days.create(&record)?;
    Ok(record)
}

fn load(contract: &EmissionContract<'_>, day: u32) -> Result<EmissionDay> {
    contract
        .emission_days
        .get(day)?
        .ok_or_else(|| EmissionError::EmissionDayNotFound.into())
}

/// Moves the record of `day` from `Pending` to `Processing`: the votes and fees of `day` are
/// sealed and the step of 00:00 after it ran.
pub fn record_day_started(storage: StorageHandle<'_>, day: u32) -> Result<EmissionDay> {
    let contract = EmissionContract::new(storage);
    let mut record = load(&contract, day)?;
    if record.state != emission_day_state::PENDING {
        return Err(EmissionError::InvalidStateTransition {
            day,
            from_state: record.state,
            to_state: emission_day_state::PROCESSING,
        }
        .into());
    }
    record.state = emission_day_state::PROCESSING;
    contract.emission_days.update(&record)?;
    Ok(record)
}

/// Adds `amount` that a part sent to the Promis Limit to the net of the day.
fn add_to_promis_limit(record: &mut EmissionDay, amount: u128) -> Result<()> {
    record.promis_limit = record
        .promis_limit
        .checked_add(to_i128_checked(amount)?)
        .ok_or(EmissionError::AmountOverflow)?;
    Ok(())
}

fn require_open(record: &EmissionDay) -> Result<()> {
    if record.state == emission_day_state::COMPLETED {
        return Err(EmissionError::InvalidStateTransition {
            day: record.day,
            from_state: record.state,
            to_state: record.state,
        }
        .into());
    }
    Ok(())
}

/// Writes what the reward `pool` of `day` gave out. The rest of its limit went to the Promis Limit.
pub fn record_reward_pool(
    storage: StorageHandle<'_>,
    day: u32,
    pool: RewardPool,
    allocated: U256,
) -> Result<EmissionDay> {
    let contract = EmissionContract::new(storage);
    let mut record = load(&contract, day)?;
    require_open(&record)?;
    let allocated = to_u128_checked(allocated)?;
    let bit = 1u8 << pool as u8;
    if record.pools_recorded & bit != 0 {
        return Err(EmissionError::RewardPoolRecorded {
            day,
            pool: pool as u8,
        }
        .into());
    }
    record.pools_recorded |= bit;
    let (limit, bucket) = match pool {
        RewardPool::Validator => (record.validators_limit, &mut record.validators),
        RewardPool::Waa => (record.waa_limit, &mut record.waa),
        RewardPool::Sra => (record.sra_limit, &mut record.sra),
        RewardPool::Cca => (record.cca_limit, &mut record.cca),
    };
    let rest = limit
        .checked_sub(allocated)
        .ok_or(EmissionError::AmountOverflow)?;
    *bucket = allocated;
    add_to_promis_limit(&mut record, rest)?;
    contract.emission_days.update(&record)?;
    Ok(record)
}

/// What Metadosis of a day read and gave out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetadosisAmounts {
    pub nominal_amount: U256,
    pub lysis_limit: U256,
    /// The part of the Metadosis limit that covers Nod loads.
    pub nods: U256,
    pub desis_limit: U256,
}

fn metadosis_end(
    storage: StorageHandle<'_>,
    day: u32,
    amounts: MetadosisAmounts,
    to_state: u8,
) -> Result<(EmissionDay, U256)> {
    let contract = EmissionContract::new(storage);
    let mut record = load(&contract, day)?;
    if record.state != emission_day_state::PROCESSING
        || record.metadosis_state != metadosis_state::PENDING
    {
        return Err(EmissionError::InvalidMetadosisStateTransition {
            day,
            from_state: record.metadosis_state,
            to_state,
        }
        .into());
    }
    let nods = to_u128_checked(amounts.nods.min(U256::from(record.metadosis_limit)))?;
    let rest = record.metadosis_limit - nods;
    record.nominal_amount = to_u128_checked(amounts.nominal_amount)?;
    record.lysis_limit = to_u128_checked(amounts.lysis_limit)?;
    record.nods = nods;
    record.desis_limit = to_u128_checked(amounts.desis_limit)?;
    add_to_promis_limit(&mut record, rest)?;
    record.metadosis_state = to_state;
    complete_if_ended(&mut record)?;
    contract.emission_days.update(&record)?;
    Ok((record, U256::from(rest)))
}

/// Marks Metadosis of `day` `Completed`. The part of its limit that the loads do not take went
/// to the Promis Limit.
pub fn record_metadosis_completed(
    storage: StorageHandle<'_>,
    day: u32,
    amounts: MetadosisAmounts,
) -> Result<EmissionDay> {
    metadosis_end(storage, day, amounts, metadosis_state::COMPLETED).map(|(record, _)| record)
}

/// Marks Metadosis of `day` `Failed` with no Desis limit. Returns the part of the limit that the
/// loads do not take, which the caller sends to the Promis Limit.
pub fn record_metadosis_failed(
    storage: StorageHandle<'_>,
    day: u32,
    amounts: MetadosisAmounts,
) -> Result<(EmissionDay, U256)> {
    metadosis_end(
        storage,
        day,
        MetadosisAmounts {
            desis_limit: U256::ZERO,
            ..amounts
        },
        metadosis_state::FAILED,
    )
}

fn desis_end(
    storage: StorageHandle<'_>,
    day: u32,
    intexes: U256,
    to_state: u8,
) -> Result<EmissionDay> {
    let contract = EmissionContract::new(storage);
    let mut record = load(&contract, day)?;
    require_open(&record)?;
    if record.desis_state != desis_state::PENDING {
        return Err(EmissionError::InvalidDesisStateTransition {
            day,
            from_state: record.desis_state,
            to_state,
        }
        .into());
    }
    let intexes = to_u128_checked(intexes)?;
    record.intexes = intexes;
    record.promis_limit = record
        .promis_limit
        .checked_sub(to_i128_checked(intexes)?)
        .ok_or(EmissionError::AmountOverflow)?;
    record.desis_state = to_state;
    complete_if_ended(&mut record)?;
    contract.emission_days.update(&record)?;
    Ok(record)
}

/// Marks Desis of `day` `Completed`: its Intex auction gave `intexes` to the winners.
pub fn record_desis_completed(
    storage: StorageHandle<'_>,
    day: u32,
    intexes: U256,
) -> Result<EmissionDay> {
    desis_end(storage, day, intexes, desis_state::COMPLETED)
}

/// Marks Desis of `day` `Canceled`.
pub fn record_desis_canceled(storage: StorageHandle<'_>, day: u32) -> Result<EmissionDay> {
    desis_end(storage, day, U256::ZERO, desis_state::CANCELED)
}

/// Marks Desis of `day` `Failed`.
pub fn record_desis_failed(storage: StorageHandle<'_>, day: u32) -> Result<EmissionDay> {
    desis_end(storage, day, U256::ZERO, desis_state::FAILED)
}

/// Validates the rule `emission_limit == validators + waa + sra + cca + nods + intexes + promis_limit`.
pub fn check_conservation(record: &EmissionDay) -> Result<()> {
    let landed_sum = record.landed_sum().ok_or(EmissionError::AmountOverflow)?;
    if landed_sum != to_i128_checked(record.emission_limit)? {
        return Err(EmissionError::ConservationInvariantViolated {
            day: record.day,
            expected_limit: record.emission_limit,
            landed_sum,
        }
        .into());
    }
    Ok(())
}

/// Completes a `Processing` record once Metadosis and Desis of its day both ended.
fn complete_if_ended(record: &mut EmissionDay) -> Result<()> {
    if record.state == emission_day_state::PROCESSING
        && record.metadosis_state != metadosis_state::PENDING
        && record.desis_state != desis_state::PENDING
    {
        check_conservation(record)?;
        record.state = emission_day_state::COMPLETED;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;

    const DAY: u32 = 20261022;

    /// A started day whose four pools gave out 40, 39, 40 and 0 of their 40 each.
    fn started_day(storage: StorageHandle<'_>) {
        let record = EmissionDay::new(
            DAY,
            WorldwideDay::new(20261021),
            compute_metadosis_start(DAY).unwrap(),
            1_000,
            [40, 40, 40, 40, 840],
        );
        put_emission_day(storage.clone(), &record).unwrap();
        record_day_started(storage.clone(), DAY).unwrap();
        for (pool, allocated) in [
            (RewardPool::Validator, 40),
            (RewardPool::Waa, 39),
            (RewardPool::Sra, 40),
            (RewardPool::Cca, 0),
        ] {
            record_reward_pool(storage.clone(), DAY, pool, U256::from(allocated)).unwrap();
        }
    }

    fn metadosis_amounts() -> MetadosisAmounts {
        MetadosisAmounts {
            nominal_amount: U256::from(1_000),
            lysis_limit: U256::from(320),
            nods: U256::from(320),
            desis_limit: U256::from(900),
        }
    }

    #[test]
    fn the_record_completes_when_desis_ends_after_metadosis() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        started_day(storage.clone());

        let record = record_metadosis_completed(storage.clone(), DAY, metadosis_amounts()).unwrap();
        assert_eq!(record.state, emission_day_state::PROCESSING);
        assert_eq!(record.promis_limit, 41 + 520);

        // The Desis limit took the Promis Limit of older days, so the net of the day is negative.
        let record = record_desis_completed(storage, DAY, U256::from(700)).unwrap();
        assert_eq!(record.state, emission_day_state::COMPLETED);
        assert_eq!(record.desis_state, desis_state::COMPLETED);
        assert_eq!(record.intexes, 700);
        assert_eq!(record.promis_limit, 41 + 520 - 700);
        check_conservation(&record).unwrap();
    }

    #[test]
    fn a_desis_that_ended_first_completes_the_record_at_the_end_of_metadosis() {
        for end in [record_desis_canceled, record_desis_failed] {
            let mut provider = HashMapStorageProvider::new(1);
            let storage = StorageHandle::new(&mut provider);
            started_day(storage.clone());

            let record = end(storage.clone(), DAY).unwrap();
            assert_eq!(record.state, emission_day_state::PROCESSING);
            assert_eq!(record.intexes, 0);
            assert!(end(storage.clone(), DAY).is_err(), "Desis ends once");

            let (record, rest) =
                record_metadosis_failed(storage.clone(), DAY, metadosis_amounts()).unwrap();
            assert_eq!(rest, U256::from(520));
            assert_eq!(record.state, emission_day_state::COMPLETED);
            assert_eq!(record.desis_limit, 0);
            assert_eq!(record.promis_limit, 41 + 520);
            check_conservation(&record).unwrap();
            assert!(record_desis_completed(storage, DAY, U256::ZERO).is_err());
        }
    }

    #[test]
    fn a_pool_result_is_written_once() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        started_day(storage.clone());
        let before = get_emission_day(storage.clone(), DAY).unwrap().unwrap();
        for pool in [
            RewardPool::Validator,
            RewardPool::Waa,
            RewardPool::Sra,
            RewardPool::Cca,
        ] {
            let err = record_reward_pool(storage.clone(), DAY, pool, U256::ZERO).unwrap_err();
            assert!(matches!(
                err,
                outbe_primitives::error::PrecompileError::FatalWithCause(_)
            ));
        }
        assert_eq!(get_emission_day(storage, DAY).unwrap().unwrap(), before);
    }

    #[test]
    fn a_record_that_breaks_the_rule_cannot_complete() {
        let mut provider = HashMapStorageProvider::new(1);
        let storage = StorageHandle::new(&mut provider);
        started_day(storage.clone());
        record_metadosis_completed(storage.clone(), DAY, metadosis_amounts()).unwrap();
        let mut record = get_emission_day(storage.clone(), DAY).unwrap().unwrap();
        record.cca = 1;
        put_emission_day(storage.clone(), &record).unwrap();

        let err = record_desis_completed(storage.clone(), DAY, U256::from(700)).unwrap_err();
        assert!(err.to_string().contains("onservation"), "{err}");
        assert_eq!(
            get_emission_day(storage, DAY).unwrap().unwrap().state,
            emission_day_state::PROCESSING
        );
    }
}
