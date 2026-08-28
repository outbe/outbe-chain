//! Begin-block expiry sweep: closes the settlement window of a called group and
//! returns the Promis load of everything left unrealised to the unallocated
//! limit. It only reads and writes storage — expiry on the ERC-1155 is derived,
//! so nothing here has to call a contract — which is what lets it live in a
//! block hook.

use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    storage::StorageHandle,
};

use crate::constants::MAX_SERIES_ACTIONS_PER_SWEEP;
use crate::runtime::emit_event;
use crate::schema::IntexFactoryContract;

/// Advance the called queue past every group whose window has closed.
///
/// Groups queue in call order, so the head decides the common block: if it is
/// not due, nothing behind it is either and the pass costs two reads. A notice
/// period frozen per series can in principle order two groups against their
/// arrival; the younger one then waits for the head, which delays its credit but
/// never loses it.
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
        if budget == 0 {
            break;
        }
        let Some((iso_code, worldwide_day)) = factory.called_queue_slot(index)? else {
            continue;
        };
        let key = IntexFactoryContract::scoped(iso_code, worldwide_day.value());
        // Strictly after: settlement is still legal at the deadline itself, and a
        // block hook runs before this block's transactions. Crediting at `>=`
        // would count a unit that is about to be settled in the same block.
        if now <= factory.called_group_deadline.read(&key)? {
            break;
        }

        // Isolated per group, like the other sweeps: a deterministic failure is
        // logged and retried next block instead of halting the block.
        match storage.with_checkpoint(|| expire_group(storage, iso_code, worldwide_day, index)) {
            Ok(members) => budget = budget.saturating_sub(members),
            Err(error) => {
                tracing::warn!(
                    target: "outbe::intexfactory",
                    iso_code,
                    worldwide_day = worldwide_day.value(),
                    error = ?error,
                    "expiry sweep: skipping group"
                );
            }
        }
    }

    IntexFactoryContract::new(storage.clone()).compact_called_queue()
}

/// Expire one group: every member's unrealised load returns to the pool in a
/// single credit, and the group leaves the queue. Returns the members expired.
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
        // The load is the series' own frozen copy, not the day's auction config:
        // the record is what its units were issued against.
        let series = outbe_intex::api::read_series(storage, series_id)?;
        let forfeited = outbe_intex::api::expire_series(storage, series_id)?;
        let returned = series
            .promis_load_minor
            .checked_mul(U256::from(forfeited))
            .ok_or_else(|| PrecompileError::Revert("forfeited promis load overflow".into()))?;
        credit = credit
            .checked_add(returned)
            .ok_or_else(|| PrecompileError::Revert("forfeited promis credit overflow".into()))?;

        emit_event(
            storage,
            crate::precompile::IIntexFactory::SeriesExpired {
                seriesId: series_id.into(),
                forfeitedUnits: forfeited,
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
