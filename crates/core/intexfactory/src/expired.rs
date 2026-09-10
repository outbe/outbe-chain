//! Begin-block expiry sweep: closes a called group's settlement window and returns
//! the Promis load of everything left unrealized to the unallocated limit.

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use outbe_intex::IntexState;

use crate::constants::MAX_SERIES_ACTIONS_PER_BLOCK;
use crate::runtime::emit_event;
use crate::schema::IntexFactoryContract;

/// Park a group in a later bucket. A failure here leaves it where it is, and the
/// sweep re-reads that bucket next block; propagating would fail the block instead.
fn defer_group(storage: &StorageHandle<'_>, iso_code: u16, worldwide_day: WorldwideDay, day: u32) {
    let deferred = storage.with_checkpoint(|| {
        IntexFactoryContract::new(storage.clone()).defer_called_group(iso_code, worldwide_day, day)
    });
    if let Err(error) = deferred {
        tracing::warn!(
            target: "outbe::intexfactory",
            iso_code,
            worldwide_day = worldwide_day.value(),
            error = ?error,
            "expiry sweep: could not defer group"
        );
    }
}

/// Outcome of one pass over a called group.
struct GroupExpiry {
    /// Members walked, for the sweep's budget accounting.
    members: u32,
    /// Members a deterministic error left unretired. A non-zero count keeps the
    /// group alive so the next sweep can finish it.
    pending: u32,
}

/// Retire every group whose settlement window has closed, earliest bucket first.
pub(crate) fn sweep_expiry_deadlines(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = &ctx.storage;
    let now = ctx.block.timestamp;
    let mut budget = MAX_SERIES_ACTIONS_PER_BLOCK;

    while budget > 0 {
        let mut factory = IntexFactoryContract::new(storage.clone());
        let Some(day) = factory.first_expiry_day()? else {
            break;
        };
        // A deadline lies inside its own bucket, so an open one holds nobody due.
        if now < IntexFactoryContract::bucket_end(day) {
            break;
        }
        let len = factory.expiry_bucket_len.read(&day)?;
        let resume = match factory.expiry_sweep_day.read()? == day {
            true => factory.expiry_cursor.read()?.min(len),
            false => 0,
        };

        let mut slot = resume;
        while slot < len {
            if budget == 0 {
                break;
            }
            budget -= 1;
            let Some((iso_code, worldwide_day)) = factory.expiry_slot(day, slot)? else {
                slot += 1;
                continue;
            };
            let key = IntexFactoryContract::scoped(iso_code, worldwide_day.value());
            // Strictly after, like `settle`: a hook runs before the block's transactions.
            if now <= factory.called_group_deadline.read(&key)? {
                slot += 1;
                continue;
            }

            // A group the pass cannot finish is parked an hour ahead rather than
            // dropped: dropping it would strand its members' Promis load.
            let retry_day = IntexFactoryContract::deadline_bucket(now).saturating_add(1);
            match storage.with_checkpoint(|| expire_group(storage, iso_code, worldwide_day)) {
                // The slot itself was already charged above.
                Ok(expiry) => {
                    budget = budget.saturating_sub(expiry.members.saturating_sub(1));
                    if expiry.pending != 0 {
                        tracing::warn!(
                            target: "outbe::intexfactory",
                            iso_code,
                            worldwide_day = worldwide_day.value(),
                            pending = expiry.pending,
                            "expiry sweep: group deferred with members left"
                        );
                        defer_group(storage, iso_code, worldwide_day, retry_day);
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        target: "outbe::intexfactory",
                        iso_code,
                        worldwide_day = worldwide_day.value(),
                        error = ?error,
                        "expiry sweep: group deferred after an error"
                    );
                    defer_group(storage, iso_code, worldwide_day, retry_day);
                }
            }
            slot += 1;
        }

        if slot < len {
            factory.expiry_sweep_day.write(day)?;
            factory.expiry_cursor.write(slot)?;
            break;
        }
        factory.expiry_sweep_day.write(0)?;
        factory.expiry_cursor.write(0)?;
        // Anything left broke the invariant above; retiring it keeps the tree moving.
        if factory.expiry_bucket_live.read(&day)? != 0 {
            let dropped = factory.force_retire_bucket(day)?;
            tracing::warn!(
                target: "outbe::intexfactory",
                day,
                dropped,
                "expiry sweep: bucket outlived its day, retiring it"
            );
        }
    }
    Ok(())
}

/// Expire one group in a single credit.
///
/// A member the pass could not retire leaves the group in place, so the next
/// sweep picks it up again: the group index is the retry record, and dropping it
/// would strand that member's Promis load with nothing minted against it.
///
/// Re-walking a group is therefore normal, and members retired by an earlier
/// pass are already credited. They are skipped rather than re-expired, so a
/// retry credits each load exactly once.
fn expire_group(
    storage: &StorageHandle<'_>,
    iso_code: u16,
    worldwide_day: WorldwideDay,
) -> Result<GroupExpiry> {
    let mut factory = IntexFactoryContract::new(storage.clone());
    let group = factory.called_group(iso_code, worldwide_day)?;

    let mut credit = U256::ZERO;
    let mut pending = 0u32;
    for &series_id in &group.members {
        // Per member: a shared checkpoint would roll the whole group's credit back.
        let returned = storage.with_checkpoint(|| {
            if outbe_intex::api::read_series(storage, series_id)?.lifecycle_state()?
                == IntexState::Expired
            {
                return Ok(U256::ZERO);
            }
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
                    returnedPromis: returned,
                },
            )?;
            Ok(returned)
        });
        let returned = match returned {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(target: "outbe::intexfactory", series = %series_id, error = ?error, "expiry sweep: series left for the next pass");
                pending += 1;
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
    if pending == 0 {
        factory.remove_called_group(iso_code, worldwide_day)?;
    }
    Ok(GroupExpiry {
        members: group.members.len() as u32,
        pending,
    })
}
