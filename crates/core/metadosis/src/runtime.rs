use outbe_primitives::time::WorldwideDay;
#[cfg(test)]
use outbe_primitives::time::{
    date_key_to_utc_timestamp as primitives_date_key_to_timestamp,
    timestamp_to_date_key as utc_date_key,
};
use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::schema::MetadosisContract;
use crate::{aggregate::ValidatedWwdAggregate, lifecycle, settlement::process_ready_candidate};

/// Converts a unix timestamp to a yyyymmdd date key (UTC).
#[cfg(test)]
pub fn timestamp_to_date_key(timestamp: u64) -> u32 {
    utc_date_key(timestamp)
}

/// Returns the unix timestamp for midnight UTC of a yyyymmdd date key.
///
/// Re-export of [`outbe_primitives::time::date_key_to_utc_timestamp`] for
/// backward compatibility with existing call sites in this crate. New
/// code should depend on `outbe_primitives::time` directly.
#[cfg(test)]
pub fn date_key_to_timestamp(date_key: u32) -> u64 {
    primitives_date_key_to_timestamp(date_key)
}

/// Public entry point. Each hourly ProtocolCycle pass calls it once. The call
/// comes after an optional contiguous completed UTC day receives its terminal
/// Metadosis credit. The function runs the full WWD lifecycle:
/// 1. bootstrap (block 1 only).
/// 2. `create_worldwide_day_if_needed`.
/// 3. exhaustive reducer advancement for active WWDs.
/// 4. one local settlement or confidential Lysis for a READY WWD.
///
/// Old name: `run_begin_block` (Phase 5.1 of the Cycle epic). Before the
/// Cycle epic, a dedicated `MetadosisLifecycle::begin_block` lifecycle hook
/// called this function on every block. With the Cycle epic, the only
/// legitimate caller is the hourly ProtocolCycle handler. The follow-up
/// cleanup deleted the `MetadosisLifecycle` wrapper. Tests that drive the WWD
/// state machine sub-day call this function directly.
pub fn start_metadosis(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut metadosis = MetadosisContract::new(ctx.storage.clone());
    let timestamp = ctx.block.timestamp;
    lifecycle::validate_metadosis_timestamp(timestamp)?;

    if ctx.block.block_number == 1 {
        lifecycle::init_genesis_day_inner(&mut metadosis, ctx)?;
    }

    lifecycle::create_worldwide_day_if_needed(&mut metadosis, ctx, timestamp)?;

    lifecycle::advance_active_worldwide_days(ctx)?;

    let aggregate = ValidatedWwdAggregate::load_and_validate(ctx.storage.clone())?;

    if let Some(current) = aggregate.ready_records().next() {
        process_ready_candidate(&mut metadosis, ctx, current)?;
    }

    // `MetadosisContract::commit_retire_terminal_wwd` places terminal days in `closed_wwd`.
    // The bounded queue deletes the oldest record past `MAX_RECORDS_KEPT`, avoiding a per-tick
    // scan.

    Ok(())
}

pub fn advance_active_worldwide_days(ctx: &BlockRuntimeContext) -> Result<()> {
    lifecycle::advance_active_worldwide_days(ctx)
}

/// Genesis-block (block 1) metadosis bootstrap: engage the testnet/devnet
/// bootstrap window and create the first worldwide day. Idempotent.
///
/// `outbe_cycle::lifecycle::CycleLifecycle::begin_block` calls this function
/// from the begin-zone CycleTick phase at block 1. This entry point is
/// necessary because ProtocolCycle only *anchors* `last_executed_at` on its
/// first encounter (block 1), so it never invokes [`start_metadosis`] there. Without
/// this entry point, the first worldwide day would not exist until the first
/// block after the next UTC-hour boundary.
pub fn init_genesis_day(ctx: &BlockRuntimeContext) -> Result<()> {
    lifecycle::init_genesis_day(ctx)
}

pub fn create_worldwide_day_for_date(
    metadosis: &mut MetadosisContract,
    ctx: &BlockRuntimeContext,
    wwd: WorldwideDay,
) -> Result<()> {
    lifecycle::create_worldwide_day_for_date(metadosis, ctx, wwd)
}
