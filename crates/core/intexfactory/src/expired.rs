//! Begin-block expiry sweep: closes a called group's settlement window and returns
//! the Promis load of everything left unrealized to the unallocated limit.

use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::constants::{EXPIRY_STALL_THRESHOLD, MAX_SERIES_ACTIONS_PER_SWEEP};
use crate::runtime::emit_event;
use crate::schema::IntexFactoryContract;

/// Advance the called queue past every group whose window has closed. Groups queue
/// in call order, so a head that is not due ends the pass.
pub(crate) fn sweep_expiry_deadlines(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = &ctx.storage;
    let factory = IntexFactoryContract::new(storage.clone());
    let head = factory.called_head.read()?;
    let tail = factory.called_tail.read()?;
    if head >= tail {
        return Ok(());
    }

    let now = ctx.block.timestamp;
    let mut budget = MAX_SERIES_ACTIONS_PER_SWEEP;
    for index in head..tail {
        // Not checked against the group's size: one larger than the whole budget
        // must still make progress.
        if budget == 0 {
            break;
        }
        let Some((iso_code, worldwide_day)) = factory.called_queue_slot(index)? else {
            continue;
        };
        let key = IntexFactoryContract::scoped(iso_code, worldwide_day.value());
        // Strictly after, like `settle`: a block hook runs before the block's
        // transactions, so `>=` would count a unit still legally settleable.
        if now <= factory.called_group_deadline.read(&key)? {
            break;
        }

        match storage.with_checkpoint(|| expire_group(storage, iso_code, worldwide_day, index)) {
            Ok(members) => {
                budget = budget.saturating_sub(members);
                factory.expiry_attempts.clear(&key)?;
            }
            Err(error) => {
                tracing::warn!(
                    target: "outbe::intexfactory",
                    iso_code,
                    worldwide_day = worldwide_day.value(),
                    error = ?error,
                    "expiry sweep: skipping group"
                );
                let attempts = factory.expiry_attempts.read(&key)?.saturating_add(1);
                factory.expiry_attempts.write(&key, attempts)?;
                if attempts == EXPIRY_STALL_THRESHOLD {
                    emit_event(
                        storage,
                        crate::precompile::IIntexFactory::SeriesExpiryStalled {
                            worldwideDay: worldwide_day.value(),
                            referenceCurrency: iso_code,
                            attempts,
                        },
                    )?;
                }
            }
        }
    }

    IntexFactoryContract::new(storage.clone()).compact_called_queue()
}

/// Expire one group in a single credit. Returns the members expired.
fn expire_group(
    storage: &StorageHandle<'_>,
    iso_code: u16,
    worldwide_day: WorldwideDay,
    queue_index: u32,
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
    factory.remove_called_group(iso_code, worldwide_day, queue_index)?;
    Ok(group.members.len() as u32)
}
