use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
};

use crate::contract::OracleContract;
use crate::scurve;
use crate::tally;

pub struct OracleLifecycle;

impl BlockLifecycle for OracleLifecycle {
    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        run_begin_block(ctx)
    }
}

/// Runs only the slash-window half of Oracle lifecycle.
///
/// The executor calls this through the receipt-visible `OracleSlashWindow`
/// begin-zone system phase after optional `BoundaryOutcome` and before user
/// transactions. This preserves deterministic penalties without hiding
/// operator-critical events outside EVM receipts.
pub fn run_slash_window(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut oracle = OracleContract::new(ctx.storage.clone());
    let block_number = ctx.block.block_number;
    let timestamp = ctx.block.timestamp;

    let initialized = oracle.config_is_initialized.read()?;
    if !initialized {
        return Ok(());
    }

    let slash_window = oracle.config_slash_window.read()?;
    if slash_window > 0 && block_number > 0 && block_number.is_multiple_of(slash_window) {
        tally::slash_and_reset_counters(&mut oracle, timestamp)?;
    }

    Ok(())
}

/// Called from pre-execution hooks every block.
///
/// At vote period boundaries: tallies votes, updates exchange rates, writes
/// price snapshots, and counts miss/success/abstain per validator.
///
/// At UTC day boundaries: runs S-curve peak detection for each active pair.
///
/// Slash-window force-exits are deliberately deferred to the receipt-visible
/// `OracleSlashWindow` system phase so a same-block `BoundaryOutcome` can
/// activate its target set before Oracle penalties mark underperformers EXITING.
fn run_begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
    let mut oracle = OracleContract::new(ctx.storage.clone());
    let block_number = ctx.block.block_number;
    let timestamp = ctx.block.timestamp;

    let initialized = oracle.config_is_initialized.read()?;
    if !initialized {
        return Ok(());
    }

    let vote_period = oracle.config_vote_period.read()?;

    // Tally at end of vote period (skip block 0)
    // Block 0 is always skipped (no votes possible during genesis).
    // With vote_period=1, first tally runs at block 1 (one block delay).
    if vote_period > 0 && block_number > 0 && block_number.is_multiple_of(vote_period) {
        tally::run_tally(&mut oracle, block_number, timestamp)?;
    }

    // Daily S-curve processing at UTC day boundary
    let current_day = scurve::truncate_to_day(timestamp);
    let last_processed = oracle.scurve_last_processed_day.read()?;
    if current_day > last_processed && timestamp > 0 {
        let pair_count = oracle.pair_count.read()?;
        for pid in 1..=pair_count {
            let hash = oracle.pair_id_to_hash.read(&pid)?;
            let is_target = oracle.vote_target.read(&hash)?;
            if is_target {
                scurve::process_daily_scurve(&mut oracle, pid, timestamp)?;
            }
        }
        oracle.scurve_last_processed_day.write(current_day)?;
    }

    Ok(())
}
