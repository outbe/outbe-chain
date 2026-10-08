use alloy_primitives::{B256, U256};
use outbe_oracle::{api::get_all_reference_currencies, schema::OracleContract};
use outbe_primitives::{
    address_pair::AddressPair,
    block::{BlockLifecycle, BlockRuntimeContext},
    daily_sweep::{PinnedDay, Scheduled, SweepDays},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    time::previous_date_key,
};

use outbe_primitives::call_bins::{pack_cursor, unpack_cursor};

use crate::constants::{CALL_SWEEP, MAX_BUCKET_VISITS_PER_BLOCK};
use crate::precompile::IGem::{BatchMetadataUpdate, CallScanSkipped, SweepDaySkipped};
use crate::schema::GemContract;
use crate::state::BucketBins;

mod expiry;

pub struct GemLifecycle;

impl BlockLifecycle for GemLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        // A call sweep that the daily trigger could not finish in one pass continues
        // here. It continues block by block and does not wait a day for the next trigger.
        run_call_slice(ctx)?;
        expiry::sweep_expired(ctx)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// The most recent fully-closed UTC day, or `None` while its VWAPs are not final.
fn closed_day(ctx: &BlockRuntimeContext) -> Result<Option<u32>> {
    outbe_oracle::closed_day::finalized_closed_day(ctx.storage.clone(), ctx.block.timestamp, "gem")
}

pub(crate) use outbe_primitives::daily_sweep::currency_position;

fn pinned_call_day<'a, 'storage>(gem: &'a GemContract<'storage>) -> PinnedDay<'a, 'storage> {
    PinnedDay {
        current: &gem.call_sweep_day,
        pending: &gem.call_pending_day,
    }
}

/// Trailing finalized daily VWAPs of one pair, newest first. `None` marks a day
/// the pair had no data for.
type VwapWindow = Vec<(u32, Option<U256>)>;

/// Cycle daily-trigger entry: open the day's Called sweep, discarding the count.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
}

/// Cycle daily-trigger entry: schedule the day that the Oracle has just finalized.
/// This opens a Called sweep over the day and runs its first slice, or queues the
/// day behind the sweep still in flight.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let Some(last_closed_day) = closed_day(ctx)? else {
        return Ok(0);
    };

    let mut gem = GemContract::new(ctx.storage.clone());
    let scheduled = pinned_call_day(&gem).schedule(last_closed_day)?;
    match scheduled {
        (next, Scheduled::Opened) => {
            start_call_sweep(ctx, &gem, next)?;
            run_call_slice(ctx)
        }
        (next, Scheduled::Replaced { skipped }) => {
            gem.emit(SweepDaySkipped {
                sweep: CALL_SWEEP,
                skippedDay: skipped,
                inFlightDay: next.current,
            })?;
            Ok(0)
        }
        (_, Scheduled::Queued | Scheduled::Ignored) => Ok(0),
    }
}

/// Pin the sweep's current day and walk it from the first currency's lowest bin.
fn start_call_sweep(ctx: &BlockRuntimeContext, gem: &GemContract, days: SweepDays) -> Result<()> {
    pinned_call_day(gem).open(days)?;
    gem.call_currency_cursor.write(0)?;
    for iso_code in get_all_reference_currencies(ctx)? {
        gem.bucket_scan_cursor.write(&iso_code, 0)?;
    }
    Ok(())
}

/// Advance an open sweep by one slice, pinned to the day it opened on so blocks
/// of it decide against the same prices. Returns how many buckets were called.
pub fn run_call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let called = call_slice(ctx)?;
    // Gem ids carry no order, so a call can only refresh the whole range.
    if called != 0 {
        GemContract::new(ctx.storage.clone()).emit(BatchMetadataUpdate {
            _fromTokenId: U256::ZERO,
            _toTokenId: U256::MAX,
        })?;
    }
    Ok(called)
}

fn call_slice(ctx: &BlockRuntimeContext) -> Result<u32> {
    let gem = GemContract::new(ctx.storage.clone());
    let pinned_day = gem.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let currencies = get_all_reference_currencies(ctx)?;
    let oracle = OracleContract::new(ctx.storage.clone());
    let start = currency_position(&currencies, gem.call_currency_cursor.read()?);
    let live_window = crate::config::read_from(&gem, ctx.block.chain_id)?.call_window_seconds;
    let mut sweep = CallSweep {
        ctx,
        gem,
        oracle,
        pinned_day,
        live_window,
        windows: Vec::new(),
        budget: MAX_BUCKET_VISITS_PER_BLOCK,
    };
    let mut called: u32 = 0;
    // One pass down the list: a currency closed behind the cursor is never walked
    // again, so every sweep ends however much it leaves undecided.
    for &iso_code in currencies.iter().skip(start) {
        if sweep.budget == 0 {
            sweep.gem.call_currency_cursor.write(u32::from(iso_code))?;
            return Ok(called);
        }
        let (calls, finished) = sweep.scan_currency(iso_code)?;
        called = called.saturating_add(calls);
        if !finished {
            sweep.gem.call_currency_cursor.write(u32::from(iso_code))?;
            return Ok(called);
        }
    }
    sweep.finish()?;
    Ok(called)
}

/// Prices and progress pinned to one day's call sweep.
struct CallSweep<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    gem: GemContract<'storage>,
    oracle: OracleContract<'storage>,
    pinned_day: u32,
    live_window: u32,
    windows: Vec<(u16, VwapWindow)>,
    budget: u32,
}

impl CallSweep<'_, '_> {
    fn scan_currency(&mut self, iso_code: u16) -> Result<(u32, bool)> {
        // A currency this day's pass could not price is settled for the day.
        if self.gem.call_scan_failed_day.read(&iso_code)? == self.pinned_day {
            return Ok((0, true));
        }
        // Peek the trie before pricing the currency: a drained one costs three
        // reads here instead of a whole VWAP window.
        let (cursor_bin, _) = unpack_cursor(self.gem.bucket_scan_cursor.read(&iso_code)?);
        if tree_math::find_first_left_inclusive(&BucketBins(&self.gem, iso_code), cursor_bin)?
            .is_none()
        {
            self.gem.bucket_scan_cursor.write(&iso_code, 0)?;
            return Ok((0, true));
        }
        let index = self.window_for(iso_code)?;
        let window = self.windows[index].1.as_slice();
        // Nothing priced above the window's high can have breached.
        let Some(high) = window.iter().filter_map(|(_, vwap)| *vwap).max() else {
            return Ok((0, true));
        };
        let ceiling = match GemContract::price_to_bin(high) {
            Ok(bin) => bin,
            Err(error) => {
                tracing::warn!(target: "outbe::gem", iso_code, error = ?error, "call scan: window price out of range, skipping currency for the day");
                self.gem
                    .call_scan_failed_day
                    .write(&iso_code, self.pinned_day)?;
                self.gem.emit(CallScanSkipped {
                    referenceCurrency: iso_code,
                    utcDay: self.pinned_day,
                })?;
                return Ok((0, true));
            }
        };
        call_currency(self.ctx, iso_code, window, ceiling, &mut self.budget)
    }

    fn finish(&self) -> Result<()> {
        // The next day starts on the next block, so no slice mixes two days' prices.
        let next = pinned_call_day(&self.gem).finish(self.pinned_day)?;
        match next {
            Some(next) => start_call_sweep(self.ctx, &self.gem, next),
            None => Ok(()),
        }
    }

    /// Index of the trailing finalized-VWAP window for `COEN/<iso>`, newest first,
    /// filling it on first use. An unregistered pair caches an empty window.
    fn window_for(&mut self, iso_code: u16) -> Result<usize> {
        if let Some(index) = self.windows.iter().position(|(code, _)| *code == iso_code) {
            return Ok(index);
        }
        let pair_index = self
            .oracle
            .pair_index_of(AddressPair::new_coen_to(iso_code))?;
        let mut window = Vec::new();
        if pair_index != 0 {
            // Use the widest of the live profile and anything ever issued. A gem keeps the
            // window it was issued with, so a narrowed profile must not shorten the span.
            let window_days = self
                .gem
                .max_call_window_seconds
                .read(&iso_code)?
                .max(self.live_window)
                / 86_400;
            window.reserve(window_days as usize);
            let mut day = self.pinned_day;
            for _ in 0..window_days {
                window.push((day, self.oracle.get_utc_day_vwap_for_pair(day, pair_index)?));
                day = previous_date_key(day);
            }
        }
        self.windows.push((iso_code, window));
        Ok(self.windows.len() - 1)
    }
}

/// Walk one currency's bucket bins up to `ceiling`, resuming where it stopped.
/// Returns the calls made and whether the eligible range was walked to the end.
///
/// Each bin is walked from the top, so a call's swap-pop only moves a bucket already
/// visited, and a bin wider than the budget resumes inside itself.
pub(crate) fn call_currency(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    window: &[(u32, Option<U256>)],
    ceiling: u32,
    budget: &mut u32,
) -> Result<(u32, bool)> {
    let gem = GemContract::new(ctx.storage.clone());
    let (mut from_bin, mut remaining) = unpack_cursor(gem.bucket_scan_cursor.read(&iso_code)?);
    let mut scan = BucketCallScan {
        ctx,
        gem,
        iso_code,
        window,
        budget,
    };
    let mut called: u32 = 0;
    loop {
        let bin =
            match tree_math::find_first_left_inclusive(&BucketBins(&scan.gem, iso_code), from_bin)?
            {
                Some(bin) if bin <= ceiling => bin,
                _ => {
                    scan.gem.bucket_scan_cursor.write(&iso_code, 0)?;
                    return Ok((called, true));
                }
            };
        let (calls, finished) = scan.visit_bin(bin, from_bin, remaining)?;
        called = called.saturating_add(calls);
        if !finished {
            return Ok((called, false));
        }
        remaining = 0;
        from_bin = match bin.checked_add(1) {
            Some(next) if next <= MAX_BIN_ID => next,
            _ => {
                scan.gem.bucket_scan_cursor.write(&iso_code, 0)?;
                return Ok((called, true));
            }
        };
    }
}

/// A currency's reverse bucket walk shares one budget across its bins.
struct BucketCallScan<'a, 'storage> {
    ctx: &'a BlockRuntimeContext<'storage>,
    gem: GemContract<'storage>,
    iso_code: u16,
    window: &'a [(u32, Option<U256>)],
    budget: &'a mut u32,
}

impl BucketCallScan<'_, '_> {
    fn visit_bin(&mut self, bin: u32, from_bin: u32, remaining: u32) -> Result<(u32, bool)> {
        let count = self
            .gem
            .bucket_bin_count
            .read(&GemContract::scoped(self.iso_code, bin))?;
        let mut remaining = if bin == from_bin && remaining != 0 {
            remaining.min(count)
        } else {
            count
        };
        let mut called: u32 = 0;
        while remaining > 0 {
            if *self.budget == 0 {
                self.gem
                    .bucket_scan_cursor
                    .write(&self.iso_code, pack_cursor(bin, remaining))?;
                return Ok((called, false));
            }
            *self.budget -= 1;
            remaining -= 1;
            let bucket = self.gem.bucket_bin_at.read(&GemContract::bin_index_key(
                self.iso_code,
                bin,
                remaining,
            ))?;
            if self.call_bucket(bucket)? {
                called = called.saturating_add(1);
            }
        }
        Ok((called, true))
    }

    /// Protocol errors roll back only this bucket. Node-local failures fail the block.
    fn call_bucket(&mut self, bucket: B256) -> Result<bool> {
        match self.ctx.storage.with_checkpoint(|| {
            self.gem
                .trigger_bucket_call(self.window, bucket, self.ctx.block.timestamp)
        }) {
            Ok(called) => Ok(called),
            Err(error) if error.is_node_local() => Err(error),
            Err(error) => {
                tracing::warn!(target: "outbe::gem", %bucket, error = ?error, "call scan: skipping bucket");
                Ok(false)
            }
        }
    }
}
