//! Begin-block expiry sweep: closes a called group's settlement window and returns
//! the Promis load of everything left unrealized to the unallocated limit.

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result, SweepFailure},
    storage::StorageHandle,
};

use crate::constants::MAX_SERIES_ACTIONS_PER_BLOCK;
use crate::runtime::emit_event;
use crate::schema::IntexFactoryContract;

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
    let mut sweep = ExpirySweep {
        storage: &ctx.storage,
        now: ctx.block.timestamp,
        budget: MAX_SERIES_ACTIONS_PER_BLOCK,
    };
    while sweep.budget > 0 {
        let mut factory = IntexFactoryContract::new(sweep.storage.clone());
        let Some(day) = factory.first_expiry_day()? else {
            break;
        };
        // A deadline lies inside its own bucket, so an open one holds nobody due.
        if sweep.now < IntexFactoryContract::bucket_end(day) {
            break;
        }
        if !sweep.sweep_bucket(&mut factory, day)? {
            break;
        }
    }
    Ok(())
}

/// One block's expiry budget, shared across buckets and groups.
struct ExpirySweep<'a, 'storage> {
    storage: &'a StorageHandle<'storage>,
    now: u64,
    budget: u32,
}

impl ExpirySweep<'_, '_> {
    /// Returns false when this bucket still needs another block's budget.
    fn sweep_bucket(&mut self, factory: &mut IntexFactoryContract, day: u32) -> Result<bool> {
        let len = factory.expiry_bucket_len.read(&day)?;
        let resume = match factory.expiry_sweep_day.read()? == day {
            true => factory.expiry_cursor.read()?.min(len),
            false => 0,
        };

        let mut slot = resume;
        while slot < len && self.budget > 0 {
            self.budget -= 1;
            if !self.sweep_slot(factory, day, slot)? {
                break;
            }
            slot += 1;
        }

        if slot < len {
            factory.expiry_sweep_day.write(day)?;
            factory.expiry_cursor.write(slot)?;
            return Ok(false);
        }
        factory.expiry_sweep_day.write(0)?;
        factory.expiry_cursor.write(0)?;
        if factory.expiry_bucket_live.read(&day)? != 0 {
            self.requeue_bucket(factory, day)?;
        }
        Ok(true)
    }

    /// Expire the group in `slot` once its window has closed. False means the
    /// group is still in this bucket, so the pass stops on it.
    fn sweep_slot(&mut self, factory: &IntexFactoryContract, day: u32, slot: u32) -> Result<bool> {
        let Some((iso_code, worldwide_day)) = factory.expiry_slot(day, slot)? else {
            return Ok(true);
        };
        let key = IntexFactoryContract::scoped(iso_code, worldwide_day.value());
        // Strictly after, like `settleIntex`: a hook runs before the block's transactions.
        if self.now <= factory.called_group_deadline.read(&key)? {
            return Ok(true);
        }

        // Parked an hour ahead rather than dropped.
        let retry_day = IntexFactoryContract::deadline_bucket(self.now).saturating_add(1);
        let storage = self.storage;
        match storage.with_checkpoint(|| expire_group(storage, iso_code, worldwide_day)) {
            // The slot itself was already charged above.
            Ok(expiry) => {
                self.budget = self.budget.saturating_sub(expiry.members.saturating_sub(1));
                if expiry.pending == 0 {
                    return Ok(true);
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
        defer_group(storage, iso_code, worldwide_day, retry_day)
    }

    fn requeue_bucket(&self, factory: &mut IntexFactoryContract, day: u32) -> Result<()> {
        let requeued = factory.force_retire_bucket(day, self.now)?;
        for &(iso_code, worldwide_day, retry_day) in &requeued {
            emit_event(
                self.storage,
                crate::precompile::IIntexFactory::ExpiryDeferred {
                    referenceCurrency: iso_code,
                    worldwideDay: worldwide_day.value(),
                    retryAt: IntexFactoryContract::bucket_end(retry_day),
                },
            )?;
        }
        tracing::warn!(
            target: "outbe::intexfactory",
            day,
            requeued = requeued.len(),
            "expiry sweep: bucket outlived its day, requeued what it held"
        );
        Ok(())
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
