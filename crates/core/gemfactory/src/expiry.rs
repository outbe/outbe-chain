//! Daily position-expiry sweep: returns the Promis capacity of every GemPosition
//! whose one-year validity has lapsed to the Reserve. Driven by the Cycle daily
//! trigger.
//!
//! One pass over the dense active-position index. Expiry is a pure function of
//! `parked_at` and the block timestamp, so — unlike the price-path scans — this
//! reads no oracle history and needs no finalized-day guard.

use outbe_primitives::{block::BlockRuntimeContext, error::Result};

use crate::constants::{MAX_POSITION_EXPIRY_VISITS, POSITION_VALIDITY_SECONDS};
use crate::runtime;
use crate::schema::GemFactoryContract;

/// Cycle daily-trigger entry: runs the sweep, discarding the count.
pub fn run_expiry_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    sweep_expired(ctx)?;
    Ok(())
}

/// Retires expired positions and returns how many were retired this run.
pub fn sweep_expired(ctx: &BlockRuntimeContext) -> Result<u32> {
    let factory = GemFactoryContract::new(ctx.storage.clone());
    let len = factory.active_len()?;
    if len == 0 {
        return Ok(0);
    }

    // Stored as `index + 1`; 0 means "start a fresh pass from the top".
    let mut cursor = match factory.expiry_scan_cursor.read()? {
        0 => len - 1,
        resume => resume.saturating_sub(1).min(len - 1),
    };

    let now = ctx.block.timestamp;
    let mut retired: u32 = 0;
    let mut visited: u32 = 0;

    // Descending walk: `remove_active` swap-pops the tail into the hole, and the
    // tail is already behind a descending cursor, so no live entry is skipped.
    let completed = loop {
        if visited >= MAX_POSITION_EXPIRY_VISITS {
            break false;
        }
        if let Some(position_id) = factory.active_at(cursor)? {
            // The arm is pure storage and arithmetic, so a deterministic error
            // is isolated to this position and skipped — one bad position never
            // halts the daily run. Same shape as the gem and credis scans.
            let outcome = ctx.storage.with_checkpoint(|| {
                let record = factory
                    .positions
                    .get(position_id)?
                    .ok_or(crate::errors::GemFactoryError::PositionNotFound)?;
                if now < record.parked_at.saturating_add(POSITION_VALIDITY_SECONDS) {
                    return Ok(false);
                }
                runtime::reclaim_expired_position(&ctx.storage, position_id)?;
                Ok(true)
            });
            match outcome {
                Ok(true) => retired = retired.saturating_add(1),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(
                        target: "outbe::gemfactory",
                        %position_id,
                        error = ?e,
                        "position expiry sweep: skipping position",
                    );
                }
            }
        }
        visited = visited.saturating_add(1);
        if cursor == 0 {
            break true;
        }
        cursor -= 1;
    };

    // `cursor` is the next index to visit when the budget cut the pass short.
    factory.expiry_scan_cursor.write(if completed {
        0
    } else {
        cursor.saturating_add(1)
    })?;
    Ok(retired)
}
