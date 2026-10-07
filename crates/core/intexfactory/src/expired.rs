//! Begin-block expiry sweep: closes a called group's settlement window and returns
//! the Promis load of everything left unrealized to the unallocated limit.

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    expiry_queue::{self, ExpiryHandler, Step},
    storage::StorageHandle,
};

use crate::constants::MAX_SERIES_ACTIONS_PER_BLOCK;
use crate::runtime::emit_event;
use crate::schema::IntexFactoryContract;
use crate::state::ExpiryHours;

/// Park a group in a later bucket. False means it is still in this bucket and
/// the caller must stop the pass, or the tail sweep requeues it.
fn defer_group(
    storage: &StorageHandle<'_>,
    iso_code: u16,
    worldwide_day: WorldwideDay,
    day: u32,
) -> Result<bool> {
    let deferred = storage.with_checkpoint(|| {
        IntexFactoryContract::new(storage.clone()).defer_called_group(
            iso_code,
            worldwide_day,
            day,
        )?;
        emit_event(
            storage,
            crate::precompile::IIntexFactory::ExpiryDeferred {
                referenceCurrency: iso_code,
                worldwideDay: worldwide_day.value(),
                retryAt: IntexFactoryContract::bucket_end(day),
            },
        )
    });
    if let Err(error) = deferred {
        if error.sweep_failure() == SweepFailure::Propagate {
            return Err(error);
        }
        tracing::warn!(
            target: "outbe::intexfactory",
            iso_code,
            worldwide_day = worldwide_day.value(),
            error = ?error,
            "expiry sweep: could not defer group, stopping the pass"
        );
        return Ok(false);
    }
    Ok(true)
}

struct GroupExpiry {
    /// Members walked, for the sweep's budget accounting.
    members: u32,
    /// Members left unretired. A non-zero count keeps the group alive.
    pending: u32,
}

/// Retire every group whose settlement window has closed, earliest bucket first.
pub(crate) fn sweep_expiry_deadlines(ctx: &BlockRuntimeContext) -> Result<()> {
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let mut expiry = IntexExpiry {
        storage: &ctx.storage,
        now: ctx.block.timestamp,
    };
    let mut budget = MAX_SERIES_ACTIONS_PER_BLOCK;
    expiry_queue::sweep(
        &ExpiryHours(&factory),
        ctx.block.timestamp,
        &mut budget,
        &mut expiry,
    )
}

struct IntexExpiry<'a, 'storage> {
    storage: &'a StorageHandle<'storage>,
    now: u64,
}

impl ExpiryHandler<u64> for IntexExpiry<'_, '_> {
    /// Expire the group once its window has closed. A group that cannot move on
    /// holds the pass on its slot.
    fn expire(&mut self, key: u64, budget: &mut u32) -> Result<Step> {
        *budget -= 1;
        let (iso_code, worldwide_day) = IntexFactoryContract::unscoped(key);
        // Parked an hour ahead rather than dropped.
        let retry_day = IntexFactoryContract::deadline_bucket(self.now).saturating_add(1);
        let storage = self.storage;
        match storage.with_checkpoint(|| expire_group(storage, iso_code, worldwide_day)) {
            Ok(expiry) => {
                *budget = budget.saturating_sub(expiry.members.saturating_sub(1));
                if expiry.pending == 0 {
                    return Ok(Step::Done);
                }
                tracing::warn!(
                    target: "outbe::intexfactory",
                    iso_code,
                    worldwide_day = worldwide_day.value(),
                    pending = expiry.pending,
                    "expiry sweep: group deferred with members left"
                );
            }
            Err(error) if error.sweep_failure() == SweepFailure::Propagate => {
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(
                    target: "outbe::intexfactory",
                    iso_code,
                    worldwide_day = worldwide_day.value(),
                    error = ?error,
                    "expiry sweep: group deferred after an error"
                );
            }
        }
        Ok(
            if defer_group(storage, iso_code, worldwide_day, retry_day)? {
                Step::Done
            } else {
                Step::Hold
            },
        )
    }

    /// A group still in a walked bucket moves to the bucket its deadline falls in,
    /// never before the next hour. An emptied group leaves the queue.
    fn retire_leftover(&mut self, key: u64) -> Result<()> {
        let mut factory = IntexFactoryContract::new(self.storage.clone());
        let (iso_code, worldwide_day) = IntexFactoryContract::unscoped(key);
        if factory.called_group_count.read(&key)? == 0 {
            return factory.remove_called_group(iso_code, worldwide_day);
        }
        let retry_day =
            IntexFactoryContract::deadline_bucket(factory.called_group_deadline.read(&key)?)
                .max(IntexFactoryContract::deadline_bucket(self.now).saturating_add(1));
        factory.defer_called_group(iso_code, worldwide_day, retry_day)?;
        tracing::warn!(
            target: "outbe::intexfactory",
            iso_code,
            worldwide_day = worldwide_day.value(),
            "expiry sweep: bucket outlived its day, requeued its group"
        );
        emit_event(
            self.storage,
            crate::precompile::IIntexFactory::ExpiryDeferred {
                referenceCurrency: iso_code,
                worldwideDay: worldwide_day.value(),
                retryAt: IntexFactoryContract::bucket_end(retry_day),
            },
        )
    }
}

/// Expire one group in a single credit. A group with a member left over is walked
/// again, so only that member stays in it and each load is credited exactly once.
fn expire_group(
    storage: &StorageHandle<'_>,
    iso_code: u16,
    worldwide_day: WorldwideDay,
) -> Result<GroupExpiry> {
    let mut factory = IntexFactoryContract::new(storage.clone());
    let group = factory.called_group(iso_code, worldwide_day)?;

    let mut credit = U256::ZERO;
    let mut left = Vec::new();
    for &series_id in &group.members {
        // Per member: a shared checkpoint would roll the whole group's credit back.
        let returned = storage.with_checkpoint(|| {
            let forfeited = outbe_intex::api::expire_series(storage, series_id)?;
            let returned = forfeited
                .promis_load_minor
                .checked_mul(U256::from(forfeited.units))
                .ok_or_else(|| PrecompileError::Revert("forfeited promis load overflow".into()))?;
            emit_event(
                storage,
                crate::precompile::IIntexFactory::SeriesExpired {
                    seriesId: series_id.into(),
                    forfeitedUnits: forfeited.units,
                    returnedPromisMinor: returned,
                },
            )?;
            Ok(returned)
        });
        let returned = match returned {
            Ok(value) => value,
            Err(error) if error.sweep_failure() == SweepFailure::Propagate => return Err(error),
            Err(error) => {
                tracing::warn!(target: "outbe::intexfactory", series = %series_id, error = ?error, "expiry sweep: series left for the next pass");
                left.push(series_id);
                continue;
            }
        };
        credit = credit
            .checked_add(returned)
            .ok_or_else(|| PrecompileError::Revert("forfeited promis credit overflow".into()))?;
    }

    if !credit.is_zero() {
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .add_to_total_unallocated(credit)?;
    }
    if left.is_empty() {
        factory.remove_called_group(iso_code, worldwide_day)?;
    } else {
        factory.retain_called_group(iso_code, worldwide_day, &left)?;
    }
    Ok(GroupExpiry {
        members: group.members.len() as u32,
        pending: left.len() as u32,
    })
}
