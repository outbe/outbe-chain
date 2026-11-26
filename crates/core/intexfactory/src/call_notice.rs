//! Queue of call notices, sent every block after the call slice: each run of one day and one
//! call time goes into the outbox as call notices to every chain that is not Off.

use alloy_primitives::U256;
use outbe_intex::SeriesId;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};
use outbe_system_common::pass_budget::PassBudget;
use outbe_system_common::time::EmissionDate;

use crate::constants::MAX_SERIES_PER_CALL_NOTICE;
use crate::schema::IntexFactoryContract;

/// A Called entry packs its call time into the low bytes the 14-byte `SeriesId` leaves
/// free, so the origin's stamp reaches the target instead of its delivery time.
pub fn pack_call_notice(series_id: SeriesId, called_at: u64) -> U256 {
    series_id.to_word() | U256::from(called_at)
}

/// Whether a Called entry belongs to the run a message is being built for. The wire carries
/// one day and one call time for the whole batch, so both must match for a series to ride along.
pub fn joins_run(day: EmissionDate, called_at: u64, series_day: EmissionDate, ts: u64) -> bool {
    ts == called_at && series_day == day
}

/// The Emission Day the series' auction ran on; `None` once the series is deleted.
fn series_day(storage: &StorageHandle<'_>, id: SeriesId) -> Result<Option<EmissionDate>> {
    Ok(outbe_intex::api::find_series(storage, id)?.map(|series| series.emission_day))
}

fn unpack_call_notice(entry: U256) -> (SeriesId, u64) {
    (
        SeriesId::from_word(entry),
        (entry & U256::from(u64::MAX)).to::<u64>(),
    )
}

pub(crate) fn enqueue_notice(factory: &IntexFactoryContract, entry: U256) -> Result<()> {
    let tail = factory.notify_tail.read()?;
    factory.notify_at.write(&tail, entry)?;
    factory.notify_tail.write(tail.saturating_add(1))?;
    Ok(())
}

/// Puts the queued notices into the outbox, one write of the sweep budget per message.
pub fn send_call_notices(ctx: &BlockRuntimeContext) -> Result<()> {
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let head = factory.notify_head.read()?;
    let tail = factory.notify_tail.read()?;
    if head >= tail {
        return Ok(());
    }
    let chains = crate::targets::series_target_chains(&ctx.storage)?
        .len()
        .max(1);
    let mut budget = PassBudget::per_block();
    let mut index = head;
    while index < tail && !budget.spent() {
        let (first_id, called_at) = unpack_call_notice(factory.notify_at.read(&index)?);
        // A series is deleted only when nothing of it is left to call.
        let Some(emission_day) = series_day(&ctx.storage, first_id)? else {
            factory.notify_at.clear(&index)?;
            index += 1;
            continue;
        };
        // Each batch of the run is one outbox message to each chain.
        let run_cap = (budget.writes_left() as usize / chains)
            .max(1)
            .saturating_mul(MAX_SERIES_PER_CALL_NOTICE);
        let mut run = vec![first_id];
        let mut next = index + 1;
        while next < tail && run.len() < run_cap {
            let (id, ts) = unpack_call_notice(factory.notify_at.read(&next)?);
            if !series_day(&ctx.storage, id)?
                .is_some_and(|day| joins_run(emission_day, called_at, day, ts))
            {
                break;
            }
            run.push(id);
            next += 1;
        }
        let messages = run.len().div_ceil(MAX_SERIES_PER_CALL_NOTICE) * chains;
        if !budget.admit_writes(messages as u32) {
            break;
        }
        for slot in index..next {
            factory.notify_at.clear(&slot)?;
        }
        crate::call::notify_called(&ctx.storage, emission_day, called_at, &run)?;
        index = next;
    }
    if index >= tail {
        factory.notify_head.write(0)?;
        factory.notify_tail.write(0)?;
    } else {
        factory.notify_head.write(index)?;
    }
    Ok(())
}
