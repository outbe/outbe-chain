//! Begin-block expiry sweep: closes a called group's settlement window and returns
//! the Promis load of everything left unrealized to the unallocated limit.

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::constants::{
    MAX_EXPIRY_BUCKETS_PER_BLOCK, MAX_EXPIRY_SLOTS_PER_BLOCK, MAX_SERIES_ACTIONS_PER_BLOCK,
};
use crate::runtime::emit_event;
use crate::schema::IntexFactoryContract;

/// Retire every group whose settlement window has closed, oldest deadline day
/// first. Groups sit in the bucket of the day they expire in, so call order does
/// not matter and one group nobody can retire never holds up another.
pub(crate) fn sweep_expiry_deadlines(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = &ctx.storage;
    let now = ctx.block.timestamp;
    let mut budget = MAX_SERIES_ACTIONS_PER_BLOCK;
    let mut slots = MAX_EXPIRY_SLOTS_PER_BLOCK;
    let mut buckets = MAX_EXPIRY_BUCKETS_PER_BLOCK;

    while budget > 0 && slots > 0 && buckets > 0 {
        let mut factory = IntexFactoryContract::new(storage.clone());
        // Always from the bottom: a group can be re-bucketed into a day the cursor
        // has already passed, and the tree makes restarting free anyway.
        let Some(day) = factory.first_expiry_day()? else {
            break;
        };
        // A deadline lies inside its own day by construction, so a day that has not
        // closed yet holds nobody who is due - and no later day can be due either.
        if now < IntexFactoryContract::day_end(day) {
            break;
        }
        buckets -= 1;

        let len = factory.expiry_bucket_len.read(&day)?;
        // A retired bucket clears its length, so a cursor left over from an earlier
        // fill must not be trusted past the current end.
        let resume = match factory.expiry_sweep_day.read()? == day {
            true => factory.expiry_cursor.read()?.min(len),
            false => 0,
        };

        let mut slot = resume;
        while slot < len {
            // Slots, not just actions: an empty or undue slot still costs a read, and
            // without its own budget one long bucket walks unbounded in a single block.
            if budget == 0 || slots == 0 {
                break;
            }
            slots -= 1;
            let Some((iso_code, worldwide_day)) = factory.expiry_slot(day, slot)? else {
                slot += 1;
                continue;
            };
            let key = IntexFactoryContract::scoped(iso_code, worldwide_day.value());
            // Strictly after, like `settle`: a block hook runs before the block's
            // transactions, so `>=` would count a unit still legally settleable.
            if now <= factory.called_group_deadline.read(&key)? {
                slot += 1;
                continue;
            }

            match storage
                .with_checkpoint(|| expire_group(storage, iso_code, worldwide_day, day, slot))
            {
                Ok(members) => budget = budget.saturating_sub(members),
                Err(error) => {
                    tracing::warn!(
                        target: "outbe::intexfactory",
                        iso_code,
                        worldwide_day = worldwide_day.value(),
                        error = ?error,
                        "expiry sweep: retiring group without credit"
                    );
                    // Dropped whole, not just out of the bucket: leaving its records
                    // behind would refuse the pair a later call forever.
                    factory.remove_called_group(iso_code, worldwide_day, day, slot)?;
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
        // A closed day whose entries all stayed put would otherwise be handed back
        // every block; nothing here can become due later, so it is done.
        if factory.expiry_bucket_live.read(&day)? != 0 {
            break;
        }
    }
    Ok(())
}

/// Expire one group in a single credit. Returns the members expired.
fn expire_group(
    storage: &StorageHandle<'_>,
    iso_code: u16,
    worldwide_day: WorldwideDay,
    day: u32,
    slot: u32,
) -> Result<u32> {
    let mut factory = IntexFactoryContract::new(storage.clone());
    let group = factory.called_group(iso_code, worldwide_day)?;

    let mut credit = U256::ZERO;
    for &series_id in &group.members {
        let forfeited = outbe_intex::api::expire_series(storage, series_id)?;
        let returned = forfeited
            .promis_load_minor
            .checked_mul(U256::from(forfeited.units))
            .ok_or_else(|| PrecompileError::Revert("forfeited promis load overflow".into()))?;
        credit = credit
            .checked_add(returned)
            .ok_or_else(|| PrecompileError::Revert("forfeited promis credit overflow".into()))?;

        emit_event(
            storage,
            crate::precompile::IIntexFactory::SeriesExpired {
                seriesId: series_id.into(),
                forfeitedUnits: forfeited.units,
                returnedPromis: returned,
            },
        )?;
    }

    if !credit.is_zero() {
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .add_to_total_unallocated(credit)?;
    }
    factory.remove_called_group(iso_code, worldwide_day, day, slot)?;
    Ok(group.members.len() as u32)
}
