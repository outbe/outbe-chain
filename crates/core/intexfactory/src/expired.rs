//! Begin-block expiry sweep: closes a called group's settlement window and returns
//! the Promis load of everything left unrealized to the unallocated limit.

use alloy_primitives::U256;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::constants::{MAX_EXPIRY_BUCKETS_PER_BLOCK, MAX_SERIES_ACTIONS_PER_BLOCK};
use crate::runtime::emit_event;
use crate::schema::IntexFactoryContract;

/// Retire every group whose settlement window has closed, oldest deadline day
/// first. Groups sit in the bucket of the day they expire in, so call order does
/// not matter and one group nobody can retire never holds up another.
pub(crate) fn sweep_expiry_deadlines(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = &ctx.storage;
    let now = ctx.block.timestamp;
    let mut budget = MAX_SERIES_ACTIONS_PER_BLOCK;
    let mut buckets = MAX_EXPIRY_BUCKETS_PER_BLOCK;

    while budget > 0 && buckets > 0 {
        let mut factory = IntexFactoryContract::new(storage.clone());
        // Always from the bottom: a short notice can drop a group into a day the
        // cursor has already passed, and the tree makes restarting free anyway.
        let Some(day) = factory.first_expiry_day()? else {
            break;
        };
        if now <= factory.expiry_bucket_min.read(&day)? {
            break;
        }
        buckets -= 1;

        let len = factory.expiry_bucket_len.read(&day)?;
        let resume = (factory.expiry_sweep_day.read()? == day)
            .then(|| factory.expiry_cursor.read())
            .transpose()?
            .unwrap_or(0);

        let mut earliest = u64::MAX;
        let mut slot = resume;
        while slot < len {
            // Not checked against the group's size: one larger than the whole
            // budget must still make progress.
            if budget == 0 {
                break;
            }
            let Some((iso_code, worldwide_day)) = factory.expiry_slot(day, slot)? else {
                slot += 1;
                continue;
            };
            let key = IntexFactoryContract::scoped(iso_code, worldwide_day.value());
            let mut deadline = factory.called_group_deadline.read(&key)?;
            // Holders whose call notice never left cannot settle, so their window
            // stays open until it does or the grace runs out.
            if let Some(grace_until) = factory.notice_grace_until(iso_code, worldwide_day)? {
                deadline = deadline.max(grace_until);
            }
            // Strictly after, like `settle`: a block hook runs before the block's
            // transactions, so `>=` would count a unit still legally settleable.
            if now <= deadline {
                earliest = earliest.min(deadline);
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
                        "expiry sweep: quarantining group"
                    );
                    // Out of the bucket, not retried forever: its members keep their
                    // records, and the day behind it must still drain.
                    factory.release_expiry_slot(day, slot, key)?;
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
        // Everything left in the day is still waiting, so nothing here is due until
        // the earliest of them; without this the tree would hand it back every block.
        if factory.expiry_bucket_live.read(&day)? != 0 {
            factory.expiry_bucket_min.write(&day, earliest)?;
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
