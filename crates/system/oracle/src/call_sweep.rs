//! Each factory's daily pass: calls pinned to a closed day's prices, followed by
//! its required forfeits. Every block resumes with one persisted shared allowance.

use std::fmt::Display;

use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins::{self, CallBinStore, Visit},
    call_breach::ScanTerms,
    daily_sweep::{PinnedDay, Progress, Scheduled, SweepDays},
    error::{decide, Decided, PrecompileError, Result, SweepFailure},
    storage::{dsl::Value, StorageHandle},
    sweep_budget::{BlockAllowance, SweepBudget},
    time::{timestamp_to_date_key, SECONDS_PER_DAY},
};

use crate::api::get_all_reference_currencies;
use crate::call_window::{CallWindow, CallWindows};
use crate::schema::OracleContract;

/// One factory's coupled calls and forfeits, in persisted phase order.
pub trait CallSweep<'storage> {
    /// Names the right in the logs of a day the Oracle has not finalized.
    const CONSUMER: &'static str;
    /// Required forfeit stages after calls (series, then optional positions/cleanup).
    const FORFEIT_PHASES: u8;

    type Bins<'a>: CallBinStore<'storage>
    where
        Self: 'a;

    fn days(&self) -> PinnedDay<'_, 'storage>;
    fn phase(&self) -> &Value<'storage, u8>;
    fn block_budget(&self) -> &Value<'storage, U256>;
    fn genesis_day(&self) -> &Value<'storage, u32>;
    /// One reference currency's bins.
    fn bins(&self, reference_currency: u16) -> Self::Bins<'_>;
    /// The currency the walk in flight stands at.
    fn currency_cursor(&self) -> &Value<'storage, u32>;

    /// Sorts a failed call. A node-local failure fails the block.
    fn classify(error: &PrecompileError) -> SweepFailure {
        error.sweep_failure()
    }

    /// Points every cursor at the start of a freshly opened day.
    fn reset_cursors(&self, ctx: &BlockRuntimeContext) -> Result<()> {
        self.currency_cursor().write(0)?;
        for reference_currency in get_all_reference_currencies(ctx)? {
            self.bins(reference_currency)
                .scan_cursor()
                .write(&reference_currency, 0)?;
        }
        Ok(())
    }

    /// Walks one slice of `pinned_day`. Returns the calls made and whether the day ended.
    fn slice(
        &mut self,
        ctx: &BlockRuntimeContext,
        pinned_day: u32,
        budget: &mut SweepBudget,
    ) -> Result<(u32, bool)>;

    /// `phase` is one-based. Counts never substitute for explicit completion.
    fn forfeit(
        &mut self,
        ctx: &BlockRuntimeContext,
        pinned_day: u32,
        phase: u8,
        budget: &mut SweepBudget,
    ) -> Result<Progress>;
}

/// Seeds the first eligible day at fresh-genesis activation, before any halt.
/// Repeated initialization does not change the original boundary.
pub fn initialize<'s, S: CallSweep<'s>>(ctx: &BlockRuntimeContext, sweep: &mut S) -> Result<()> {
    if sweep.genesis_day().read()? == 0 {
        sweep
            .genesis_day()
            .write(timestamp_to_date_key(ctx.block.timestamp))?;
    }
    Ok(())
}

/// Opens the first closed day not yet walked, or queues the one the Oracle has just
/// finalized behind the day in flight.
pub fn schedule<'s, S: CallSweep<'s>>(ctx: &BlockRuntimeContext, sweep: &mut S) -> Result<()> {
    let genesis_day = sweep.genesis_day().read()?;
    if genesis_day == 0 {
        return Err(PrecompileError::revert("daily sweep not initialized"));
    }
    if ctx.block.timestamp < SECONDS_PER_DAY {
        return Ok(());
    }
    let Some(day) = crate::closed_day::finalized_closed_day(
        ctx.storage.clone(),
        ctx.block.timestamp,
        S::CONSUMER,
    )?
    else {
        return Ok(());
    };
    if day < genesis_day {
        return Ok(());
    }
    if sweep.days().current.read()? == 0 && sweep.days().latest.read()? == 0 {
        sweep.days().open(SweepDays {
            current: genesis_day,
            latest: day,
        })?;
        sweep.phase().write(0)?;
        return sweep.reset_cursors(ctx);
    }
    match sweep.days().schedule(day)? {
        (next, Scheduled::Opened) => {
            sweep.days().open(next)?;
            sweep.phase().write(0)?;
            sweep.reset_cursors(ctx)
        }
        (_, Scheduled::Queued | Scheduled::Ignored) => Ok(()),
    }
}

/// Runs calls and every required forfeit stage with one persisted block allowance.
/// Re-entry may resume the selected day, but cannot select a second day at this height.
/// A local failure rolls back progress, economic mutations, and allowance together.
pub fn continue_day<'s, S: CallSweep<'s>>(ctx: &BlockRuntimeContext, sweep: &mut S) -> Result<u32> {
    ctx.with_checkpoint(|| continue_pass(ctx, sweep))
}

fn continue_pass<'s, S: CallSweep<'s>>(ctx: &BlockRuntimeContext, sweep: &mut S) -> Result<u32> {
    schedule(ctx, sweep)?;
    let pinned_day = sweep.days().current.read()?;
    let mut allowance = BlockAllowance::for_block(
        sweep.block_budget().read()?,
        ctx.block.block_number,
        pinned_day,
    )?;
    if allowance.day != pinned_day {
        return Ok(0);
    }
    // Persist even an idle selection: scheduling later at this height cannot open
    // a second pass, nor can another entrypoint replenish the allowance.
    sweep.block_budget().write(allowance.pack())?;
    if pinned_day == 0 {
        return Ok(0);
    }
    if pinned_day < sweep.genesis_day().read()? || pinned_day > sweep.days().latest.read()? {
        return Err(PrecompileError::revert("invalid daily sweep day range"));
    }
    let today = timestamp_to_date_key(ctx.block.timestamp);
    if pinned_day >= today {
        return Err(PrecompileError::revert("daily sweep day has not closed"));
    }
    let finalized = OracleContract::new(ctx.storage.clone())
        .utc_day_vwap_last_finalized
        .read()?;
    if finalized < pinned_day {
        tracing::warn!(
            target: "outbe::oracle",
            consumer = S::CONSUMER,
            pinned_day,
            finalized,
            "call sweep: pinned utc-day VWAP not finalized, holding the sweep"
        );
        return Ok(0);
    }
    let mut phase = sweep.phase().read()?;
    if S::FORFEIT_PHASES == 0 || S::FORFEIT_PHASES == u8::MAX || phase > S::FORFEIT_PHASES {
        return Err(PrecompileError::revert("invalid daily sweep phase"));
    }
    let mut called = 0;
    if phase == 0 {
        let (count, complete) = sweep.slice(ctx, pinned_day, &mut allowance.budget)?;
        called = count;
        if !complete {
            sweep.block_budget().write(allowance.pack())?;
            return Ok(called);
        }
        phase = 1;
        sweep.phase().write(phase)?;
    }
    while phase <= S::FORFEIT_PHASES {
        match sweep.forfeit(ctx, pinned_day, phase, &mut allowance.budget)? {
            Progress::Pending => {
                sweep.block_budget().write(allowance.pack())?;
                return Ok(called);
            }
            Progress::Complete if phase == S::FORFEIT_PHASES => break,
            Progress::Complete => {
                phase += 1;
                sweep.phase().write(phase)?;
            }
        }
    }
    if let Some(next) = sweep.days().finish(pinned_day)? {
        sweep.days().open(next)?;
        sweep.phase().write(0)?;
        sweep.reset_cursors(ctx)?;
    }
    sweep.block_budget().write(allowance.pack())?;
    Ok(called)
}

/// How a day's walk reads one right's call index. `scan_terms` names the widest terms a
/// currency's entries were sealed with, and `skipped` reports a currency whose window
/// price fits no bin.
pub struct CallIndex<'c, 's, Bins, Terms, Skipped> {
    pub currency_cursor: &'c Value<'s, u32>,
    pub bins: Bins,
    pub scan_terms: Terms,
    pub skipped: Skipped,
}

/// One block's slice of the pinned day: every currency from the cursor, each walked by
/// `walk` up to the highest bin its window can have breached. Returns whether the day ended.
pub fn walk_day<'s, B, Bins, Terms, Skipped>(
    ctx: &BlockRuntimeContext,
    pinned_day: u32,
    budget: &mut SweepBudget,
    index: CallIndex<'_, 's, Bins, Terms, Skipped>,
    mut walk: impl FnMut(u16, &CallWindow, u32, &mut SweepBudget) -> Result<bool>,
) -> Result<bool>
where
    B: CallBinStore<'s>,
    Bins: Fn(u16) -> B,
    Terms: FnMut(u16) -> Result<ScanTerms>,
    Skipped: FnMut(u16) -> Result<()>,
{
    let CallIndex {
        currency_cursor,
        bins,
        mut scan_terms,
        mut skipped,
    } = index;
    let currencies = get_all_reference_currencies(ctx)?;
    let mut windows = CallWindows::new(ctx.storage.clone(), pinned_day);
    call_bins::walk_currencies(
        &currencies,
        currency_cursor,
        budget,
        |reference_currency, budget| {
            let Some((window, ceiling)) = currency_ceiling(
                &bins(reference_currency),
                &mut windows,
                || scan_terms(reference_currency),
                || skipped(reference_currency),
            )?
            else {
                return Ok(true);
            };
            walk(reference_currency, window, ceiling, budget)
        },
    )
}

/// The currency's window and the highest bin a breached entry can sit in. `None`
/// when the currency is settled for the pinned day: nothing left past its cursor, too
/// few priced days, or a price no bin covers. The last marks the day failed and calls
/// `skipped` once, so later slices pass the currency by.
pub fn currency_ceiling<'w, 's, S: CallBinStore<'s>>(
    bins: &S,
    windows: &'w mut CallWindows<'_>,
    terms: impl FnOnce() -> Result<ScanTerms>,
    skipped: impl FnOnce() -> Result<()>,
) -> Result<Option<(&'w CallWindow, u32)>> {
    let (reference_currency, pinned_day) = (bins.currency(), windows.last_day());
    if bins.failed_day().read(&reference_currency)? == pinned_day || !call_bins::pending(bins)? {
        return Ok(None);
    }
    let window = windows.window(reference_currency, terms)?;
    let Some(ceiling) = window.ceiling() else {
        return Ok(None);
    };
    match call_bins::price_to_bin(ceiling) {
        Ok(bin) => Ok(Some((window, bin))),
        Err(error) => {
            tracing::warn!(target: "outbe::oracle", reference_currency, error = ?error, "call sweep: window price out of range, skipping currency for the day");
            bins.failed_day().write(&reference_currency, pinned_day)?;
            skipped()?;
            Ok(None)
        }
    }
}

/// Calls one entry in its own checkpoint. `call` returns the writes it made, 0 when
/// the entry was not called. A deterministic failure rolls back only this entry, and
/// running out of gas resumes on it next block.
pub fn call_entry<'s, S: CallSweep<'s>>(
    storage: &StorageHandle<'_>,
    budget: &mut SweepBudget,
    entry: impl Display,
    call: impl FnOnce() -> Result<u32>,
) -> Result<Visit> {
    match decide(storage.with_checkpoint(call), S::classify)? {
        Decided::Done(writes) => {
            budget.admit_writes(writes);
            Ok(Visit::Next)
        }
        Decided::Stopped => {
            budget.stop();
            Ok(Visit::Stop)
        }
        Decided::Skipped(error) => {
            tracing::warn!(target: "outbe::oracle", consumer = S::CONSUMER, %entry, error = ?error, "call sweep: skipping entry");
            Ok(Visit::Next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::{Address, B256};
    use outbe_primitives::{
        block::BlockContext,
        storage::{dsl::Map, hashmap::HashMapStorageProvider},
        time::date_key_to_utc_timestamp,
    };

    struct TestSweep<'s> {
        current: Value<'s, u32>,
        latest: Value<'s, u32>,
        phase: Value<'s, u8>,
        allowance: Value<'s, U256>,
        genesis: Value<'s, u32>,
        currency: Value<'s, u32>,
        remaining: Map<'s, u8, u32>,
        root: Map<'s, u16, U256>,
        mid: Map<'s, u64, U256>,
        leaf: Map<'s, u64, U256>,
        count: Map<'s, u64, u32>,
        at: Map<'s, B256, u64>,
        slot: Map<'s, u64, u64>,
        cursor: Map<'s, u16, u64>,
        failed: Map<'s, u16, u32>,
        fail_phase: Option<u8>,
    }

    struct TestBins<'a, 's>(&'a TestSweep<'s>, u16);
    outbe_primitives::impl_call_bins!(TestBins<u64> {
        root: root, mid: mid, leaf: leaf, count: count, at: at,
        slot: slot, cursor: cursor, failed: failed
    });

    impl<'s> TestSweep<'s> {
        fn new(storage: StorageHandle<'s>, address: Address) -> Self {
            Self {
                current: Value::new(U256::from(1), address, storage.clone()),
                latest: Value::new(U256::from(2), address, storage.clone()),
                phase: Value::new(U256::from(3), address, storage.clone()),
                allowance: Value::new(U256::from(4), address, storage.clone()),
                genesis: Value::new(U256::from(5), address, storage.clone()),
                currency: Value::new(U256::from(6), address, storage.clone()),
                remaining: Map::new(U256::from(7), address, storage.clone()),
                root: Map::new(U256::from(8), address, storage.clone()),
                mid: Map::new(U256::from(9), address, storage.clone()),
                leaf: Map::new(U256::from(10), address, storage.clone()),
                count: Map::new(U256::from(11), address, storage.clone()),
                at: Map::new(U256::from(12), address, storage.clone()),
                slot: Map::new(U256::from(13), address, storage.clone()),
                cursor: Map::new(U256::from(14), address, storage.clone()),
                failed: Map::new(U256::from(15), address, storage),
                fail_phase: None,
            }
        }

        fn work(&self, phase: u8, budget: &mut SweepBudget) -> Result<(u32, bool)> {
            let mut remaining = self.remaining.read(&phase)?;
            let mut count = 0;
            while remaining != 0 && !budget.spent() {
                if !budget.visit() || !budget.write() {
                    break;
                }
                count += 1;
                remaining -= 1;
            }
            self.remaining.write(&phase, remaining)?;
            Ok((count, remaining == 0))
        }
    }

    impl<'s> CallSweep<'s> for TestSweep<'s> {
        const CONSUMER: &'static str = "test";
        const FORFEIT_PHASES: u8 = 2;
        type Bins<'a>
            = TestBins<'a, 's>
        where
            Self: 'a;

        fn days(&self) -> PinnedDay<'_, 's> {
            PinnedDay {
                current: &self.current,
                latest: &self.latest,
            }
        }
        fn phase(&self) -> &Value<'s, u8> {
            &self.phase
        }
        fn block_budget(&self) -> &Value<'s, U256> {
            &self.allowance
        }
        fn genesis_day(&self) -> &Value<'s, u32> {
            &self.genesis
        }
        fn bins(&self, currency: u16) -> Self::Bins<'_> {
            TestBins(self, currency)
        }
        fn currency_cursor(&self) -> &Value<'s, u32> {
            &self.currency
        }
        fn reset_cursors(&self, _ctx: &BlockRuntimeContext) -> Result<()> {
            self.currency.write(0)
        }
        fn slice(
            &mut self,
            _ctx: &BlockRuntimeContext,
            _day: u32,
            budget: &mut SweepBudget,
        ) -> Result<(u32, bool)> {
            self.work(0, budget)
        }
        fn forfeit(
            &mut self,
            _ctx: &BlockRuntimeContext,
            _day: u32,
            phase: u8,
            budget: &mut SweepBudget,
        ) -> Result<Progress> {
            if self.fail_phase == Some(phase) {
                return Err(PrecompileError::revert("injected stage failure"));
            }
            Ok(if self.work(phase, budget)?.1 {
                Progress::Complete
            } else {
                Progress::Pending
            })
        }
    }

    fn fixture(test: impl FnOnce(BlockRuntimeContext<'_>, &mut TestSweep<'_>)) {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(0, date_key_to_utc_timestamp(20260101), 1),
                storage.clone(),
            );
            let mut sweep = TestSweep::new(storage, Address::with_last_byte(99));
            initialize(&ctx, &mut sweep).unwrap();
            test(ctx, &mut sweep);
        });
    }

    fn ready(ctx: &mut BlockRuntimeContext<'_>, height: u64) {
        ctx.block.block_number = height;
        ctx.block.timestamp = date_key_to_utc_timestamp(20260104);
        OracleContract::new(ctx.storage.clone())
            .utc_day_vwap_last_finalized
            .write(20260103)
            .unwrap();
    }

    #[test]
    fn phases_share_allowance_resume_and_cannot_advance_twice_at_one_height() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            sweep.remaining.write(&0, 40).unwrap();
            sweep.remaining.write(&1, 40).unwrap();
            sweep.remaining.write(&2, 1).unwrap();
            assert_eq!(continue_day(&ctx, sweep).unwrap(), 40);
            assert_eq!(sweep.current.read().unwrap(), 20260101);
            assert_eq!(sweep.phase.read().unwrap(), 1);
            assert_eq!(sweep.remaining.read(&1).unwrap(), 16);
            assert_eq!(sweep.remaining.read(&2).unwrap(), 1);
            let mut restarted = TestSweep::new(ctx.storage.clone(), Address::with_last_byte(99));
            let sweep = &mut restarted;
            assert_eq!(continue_day(&ctx, sweep).unwrap(), 0);
            assert_eq!(sweep.remaining.read(&1).unwrap(), 16);
            ctx.block.block_number = 2;
            continue_day(&ctx, sweep).unwrap();
            assert_eq!(sweep.remaining.read(&2).unwrap(), 0);
            assert_eq!(sweep.current.read().unwrap(), 20260102);
            let allowance = sweep.allowance.read().unwrap();
            continue_day(&ctx, sweep).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 20260102);
            assert_eq!(sweep.allowance.read().unwrap(), allowance);
            ctx.block.block_number = 3;
            continue_day(&ctx, sweep).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 20260103);
            ctx.block.block_number = 4;
            continue_day(&ctx, sweep).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 0);
            ctx.block.block_number = 5;
            continue_day(&ctx, sweep).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 0);
        });
    }

    #[test]
    fn oracle_readiness_retries_without_losing_genesis_or_empty_call_forfeits() {
        fixture(|mut ctx, sweep| {
            ctx.block.timestamp = date_key_to_utc_timestamp(20260104);
            ctx.block.block_number = 1;
            sweep.remaining.write(&1, 1).unwrap();
            continue_day(&ctx, sweep).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 0);
            ready(&mut ctx, 2);
            initialize(&ctx, sweep).unwrap();
            assert_eq!(sweep.genesis.read().unwrap(), 20260101);
            continue_day(&ctx, sweep).unwrap();
            assert_eq!(sweep.remaining.read(&1).unwrap(), 0);
            assert_eq!(sweep.current.read().unwrap(), 20260102);
        });
    }

    #[test]
    fn failed_stage_rolls_back_calls_phase_and_allowance() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            sweep.remaining.write(&0, 1).unwrap();
            sweep.fail_phase = Some(1);
            assert!(continue_day(&ctx, sweep).is_err());
            assert_eq!(sweep.remaining.read(&0).unwrap(), 1);
            assert_eq!(sweep.current.read().unwrap(), 0);
            assert_eq!(sweep.phase.read().unwrap(), 0);
            assert_eq!(sweep.allowance.read().unwrap(), U256::ZERO);
            sweep.fail_phase = None;
            assert_eq!(continue_day(&ctx, sweep).unwrap(), 1);
        });
    }

    #[test]
    fn incomplete_calls_hold_forfeits_and_the_pinned_day() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            sweep.remaining.write(&0, 70).unwrap();
            sweep.remaining.write(&1, 1).unwrap();
            assert_eq!(continue_day(&ctx, sweep).unwrap(), 64);
            assert_eq!(sweep.phase.read().unwrap(), 0);
            assert_eq!(sweep.remaining.read(&0).unwrap(), 6);
            assert_eq!(sweep.remaining.read(&1).unwrap(), 1);
            assert_eq!(sweep.current.read().unwrap(), 20260101);
            assert_eq!(continue_day(&ctx, sweep).unwrap(), 0);
            ctx.block.block_number = 2;
            assert_eq!(continue_day(&ctx, sweep).unwrap(), 6);
            assert_eq!(sweep.remaining.read(&1).unwrap(), 0);
            assert_eq!(sweep.current.read().unwrap(), 20260102);
        });
    }

    #[test]
    fn invalid_phase_and_missing_bootstrap_return_errors_without_progress() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            sweep.genesis.write(0).unwrap();
            assert!(continue_day(&ctx, sweep).is_err());
            sweep.genesis.write(20260101).unwrap();
            schedule(&ctx, sweep).unwrap();
            sweep.phase.write(3).unwrap();
            assert!(continue_day(&ctx, sweep).is_err());
            assert_eq!(sweep.current.read().unwrap(), 20260101);
            assert_eq!(sweep.allowance.read().unwrap(), U256::ZERO);
        });
    }

    #[test]
    fn a_stalled_factory_does_not_hold_another_factory_or_share_its_allowance() {
        fixture(|mut ctx, stalled| {
            let mut independent = TestSweep::new(ctx.storage.clone(), Address::with_last_byte(100));
            initialize(&ctx, &mut independent).unwrap();
            stalled.remaining.write(&0, 70).unwrap();
            independent.remaining.write(&0, 1).unwrap();
            ready(&mut ctx, 1);
            assert_eq!(continue_day(&ctx, stalled).unwrap(), 64);
            assert_eq!(continue_day(&ctx, &mut independent).unwrap(), 1);
            assert_eq!(stalled.current.read().unwrap(), 20260101);
            assert_eq!(independent.current.read().unwrap(), 20260102);
            assert_eq!(stalled.remaining.read(&0).unwrap(), 6);
        });
    }

    #[test]
    fn a_gas_stopped_call_exhausts_the_persisted_allowance() {
        fixture(|ctx, _sweep| {
            let mut allowance = BlockAllowance::new(1, 20260101);
            assert_eq!(
                call_entry::<TestSweep<'_>>(&ctx.storage, &mut allowance.budget, "entry", || Err(
                    PrecompileError::OutOfGas
                ),)
                .unwrap(),
                Visit::Stop
            );
            let mut resumed = BlockAllowance::for_block(allowance.pack(), 1, 20260101).unwrap();
            assert!(resumed.budget.spent());
            assert!(!resumed.budget.visit());
            assert!(!resumed.budget.admit_writes(100));
        });
    }
}
