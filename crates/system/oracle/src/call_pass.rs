//! Each factory uses a closed day's prices for calls, then processes its required forfeits.
//! Each block resumes the pass with one stored allowance shared by all phases.

use std::fmt::Display;

use alloy_primitives::U256;
use outbe_primitives::{
    block::BlockRuntimeContext,
    call_bins::{self, CallBinStore, Visit},
    call_breach::ScanTerms,
    day_pass::{DayPassError, PassDays, PinnedDay, Progress, Scheduled},
    error::{decide, Decided, PassFailure, PrecompileError, Result},
    pass_budget::{BlockAllowance, PassBudget},
    storage::{dsl::Value, StorageHandle},
    time::{timestamp_to_date_key, TimeError, SECONDS_PER_DAY},
};

use crate::call_window::{CallWindow, CallWindows};
use crate::constants::REFERENCE_CURRENCIES;
use crate::schema::OracleContract;

/// Selects maintenance phases without bypassing their stored prerequisites.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Selection {
    pub process_call: bool,
    pub process_forfeit: bool,
    pub process_cleanup: bool,
}

impl Selection {
    fn enabled<'s, S: CallPass<'s>>(self, phase: u8) -> bool {
        if phase == 0 {
            self.process_call
        } else if S::CLEANUP_PHASE == Some(phase) {
            self.process_cleanup
        } else {
            self.process_forfeit
        }
    }
}

/// This trait processes one factory's calls and forfeits in the stored phase order.
pub trait CallPass<'storage> {
    /// Names the right in the logs of a day the Oracle has not finalized.
    const CONSUMER: &'static str;
    /// This constant counts the required stages after calls: series forfeits, then optional position expiry or cleanup.
    const FORFEIT_PHASES: u8;
    /// A coupled cleanup stage, if any, within `1..=FORFEIT_PHASES`.
    const CLEANUP_PHASE: Option<u8> = None;

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
    fn classify(error: &PrecompileError) -> PassFailure {
        error.pass_failure()
    }

    /// Points every cursor at the start of a freshly opened day.
    fn reset_cursors(&self, _ctx: &BlockRuntimeContext) -> Result<()> {
        self.currency_cursor().write(0)?;
        for reference_currency in REFERENCE_CURRENCIES {
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
        budget: &mut PassBudget,
    ) -> Result<(u32, bool)>;

    /// The `phase` argument starts at one. The method reports completion explicitly rather than through counts.
    fn forfeit(
        &mut self,
        ctx: &BlockRuntimeContext,
        pinned_day: u32,
        phase: u8,
        budget: &mut PassBudget,
    ) -> Result<Progress>;
}

/// This function initializes the first eligible day when a new genesis activates, before any halt.
/// Repeated initialization does not change the original boundary.
pub fn initialize<'s, S: CallPass<'s>>(ctx: &BlockRuntimeContext, sweep: &mut S) -> Result<()> {
    if sweep.genesis_day().read()? == 0 {
        sweep
            .genesis_day()
            .write(timestamp_to_date_key(ctx.block.timestamp))?;
    }
    Ok(())
}

/// Opens the first closed day not yet walked, or queues the one the Oracle has just
/// finalized behind the day in flight.
pub fn schedule<'s, S: CallPass<'s>>(ctx: &BlockRuntimeContext, sweep: &mut S) -> Result<()> {
    let (next, scheduled) = projected_schedule(ctx, sweep)?;
    match scheduled {
        Scheduled::Opened => {
            sweep.days().open(next)?;
            sweep.phase().write(0)?;
            sweep.reset_cursors(ctx)
        }
        Scheduled::Queued => sweep.days().latest.write(next.latest),
        Scheduled::Ignored => Ok(()),
    }
}

fn projected_schedule<'s, S: CallPass<'s>>(
    ctx: &BlockRuntimeContext,
    sweep: &S,
) -> Result<(PassDays, Scheduled)> {
    let genesis_day = sweep.genesis_day().read()?;
    if genesis_day == 0 {
        return Err(PrecompileError::revert("daily sweep not initialized"));
    }
    let days = sweep.days().days().map_err(sweep_error)?;
    // Validate persisted progress even when Oracle lag prevents new scheduling.
    day_ready(ctx, sweep, days.current)?;
    if ctx.block.timestamp < SECONDS_PER_DAY {
        return Ok((days, Scheduled::Ignored));
    }
    let Some(day) = crate::closed_day::finalized_closed_day(
        ctx.storage.clone(),
        ctx.block.timestamp,
        S::CONSUMER,
    )?
    else {
        return Ok((days, Scheduled::Ignored));
    };
    if day < genesis_day {
        return Ok((days, Scheduled::Ignored));
    }
    if days == PassDays::default() {
        return Ok((
            PassDays {
                current: genesis_day,
                latest: day,
            },
            Scheduled::Opened,
        ));
    }
    days.schedule(day)
        .map_err(|err| sweep_error(DayPassError::Time(err)))
}

fn validate_phase<'s, S: CallPass<'s>>(phase: u8) -> Result<()> {
    if S::FORFEIT_PHASES == 0
        || S::FORFEIT_PHASES == u8::MAX
        || phase > S::FORFEIT_PHASES
        || S::CLEANUP_PHASE.is_some_and(|cleanup| cleanup == 0 || cleanup > S::FORFEIT_PHASES)
    {
        return Err(PrecompileError::revert("invalid daily sweep phase"));
    }
    Ok(())
}

/// Reports daily phase obligations, independent of readiness and block allowances.
/// Factories compose independent cleanup queues separately.
pub fn pending_work<'s, S: CallPass<'s>>(
    ctx: &BlockRuntimeContext,
    sweep: &S,
) -> Result<(bool, bool, bool)> {
    let stored_phase = sweep.phase().read()?;
    validate_phase::<S>(stored_phase)?;
    let (days, scheduled) = projected_schedule(ctx, sweep)?;
    if days.current == 0 {
        return Ok((false, false, false));
    }
    let phase = if scheduled == Scheduled::Opened {
        0
    } else {
        stored_phase
    };
    let queued = days.current < days.latest;
    let forfeit = (phase.max(1)..=S::FORFEIT_PHASES).any(|stage| Some(stage) != S::CLEANUP_PHASE);
    Ok((
        phase == 0 || queued,
        forfeit || (queued && (1..=S::FORFEIT_PHASES).any(|stage| Some(stage) != S::CLEANUP_PHASE)),
        S::CLEANUP_PHASE.is_some_and(|cleanup| phase <= cleanup || queued),
    ))
}

fn sweep_error(err: DayPassError) -> PrecompileError {
    match err {
        DayPassError::Storage(err) => err,
        DayPassError::Time(time @ TimeError::PreGenesis { .. }) => {
            PrecompileError::Fatal(time.to_string())
        }
        DayPassError::Time(err) => crate::errors::date_step_error(err),
    }
}

/// This function runs calls and every required forfeit stage with one stored allowance for the block.
/// Repeated calls may resume the selected day, but cannot select a second day at this block height.
/// A local failure restores progress, economic state, and the allowance together.
pub fn continue_day<'s, S: CallPass<'s>>(
    ctx: &BlockRuntimeContext,
    sweep: &mut S,
    selection: Selection,
) -> Result<u32> {
    if selection == Selection::default() {
        return Ok(0);
    }
    ctx.with_checkpoint(|| continue_pass(ctx, sweep, selection))
}

fn continue_pass<'s, S: CallPass<'s>>(
    ctx: &BlockRuntimeContext,
    sweep: &mut S,
    selection: Selection,
) -> Result<u32> {
    if selection.process_call {
        schedule(ctx, sweep)?;
    }
    let pinned_day = sweep.days().current.read()?;
    let phase = sweep.phase().read()?;
    validate_phase::<S>(phase)?;
    // A disabled prerequisite must not select an idle allowance for this height.
    if !selection.enabled::<S>(phase) || (pinned_day == 0 && !selection.process_call) {
        return Ok(0);
    }
    let mut allowance = BlockAllowance::for_block(
        sweep.block_budget().read()?,
        ctx.block.block_number,
        pinned_day,
    )?;
    if allowance.day != pinned_day {
        return Ok(0);
    }
    // Store the selection even when no work is pending.
    // Later scheduling cannot open a second pass at this block height.
    // Another entrypoint cannot reset the allowance at this block height.
    sweep.block_budget().write(allowance.pack())?;
    if day_ready(ctx, sweep, pinned_day)? {
        let (called, progress) =
            run_phases(ctx, sweep, pinned_day, &mut allowance.budget, selection)?;
        if progress == Progress::Complete {
            if let Some(next) = sweep.days().finish(pinned_day).map_err(sweep_error)? {
                sweep.days().open(next)?;
                sweep.phase().write(0)?;
                sweep.reset_cursors(ctx)?;
            }
        }
        sweep.block_budget().write(allowance.pack())?;
        Ok(called)
    } else {
        Ok(0)
    }
}

fn day_ready<'s, S: CallPass<'s>>(
    ctx: &BlockRuntimeContext,
    sweep: &S,
    pinned_day: u32,
) -> Result<bool> {
    if pinned_day == 0 {
        return Ok(false);
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
    }
    Ok(finalized >= pinned_day)
}

fn run_phases<'s, S: CallPass<'s>>(
    ctx: &BlockRuntimeContext,
    sweep: &mut S,
    pinned_day: u32,
    budget: &mut PassBudget,
    selection: Selection,
) -> Result<(u32, Progress)> {
    let mut phase = sweep.phase().read()?;
    validate_phase::<S>(phase)?;
    let mut called = 0;
    if phase == 0 {
        let (count, complete) = sweep.slice(ctx, pinned_day, budget)?;
        called = count;
        if !complete {
            return Ok((called, Progress::Pending));
        }
        phase = 1;
        sweep.phase().write(phase)?;
    }
    while phase <= S::FORFEIT_PHASES {
        if !selection.enabled::<S>(phase) {
            return Ok((called, Progress::Pending));
        }
        match sweep.forfeit(ctx, pinned_day, phase, budget)? {
            Progress::Pending => return Ok((called, Progress::Pending)),
            Progress::Complete if phase == S::FORFEIT_PHASES => break,
            Progress::Complete => {
                phase += 1;
                sweep.phase().write(phase)?;
            }
        }
    }
    Ok((called, Progress::Complete))
}

/// How a day's walk reads one right's call index. `scan_terms` names the widest terms a
/// currency's entries were sealed with. A currency whose window price fits no bin is
/// logged and passed by for the day.
pub struct CallIndex<'c, 's, Bins, Terms> {
    pub currency_cursor: &'c Value<'s, u32>,
    pub bins: Bins,
    pub scan_terms: Terms,
}

/// One block's slice of the pinned day: every currency from the cursor, each walked by
/// `walk` up to the highest bin its window can have breached. Returns whether the day ended.
pub fn walk_day<'s, B, Bins, Terms>(
    ctx: &BlockRuntimeContext,
    pinned_day: u32,
    budget: &mut PassBudget,
    index: CallIndex<'_, 's, Bins, Terms>,
    mut walk: impl FnMut(u16, &CallWindow, u32, &mut PassBudget) -> Result<bool>,
) -> Result<bool>
where
    B: CallBinStore<'s>,
    Bins: Fn(u16) -> B,
    Terms: FnMut(u16) -> Result<ScanTerms>,
{
    let CallIndex {
        currency_cursor,
        bins,
        mut scan_terms,
    } = index;
    let currencies = REFERENCE_CURRENCIES;
    let mut windows = CallWindows::new(ctx.storage.clone(), pinned_day);
    call_bins::walk_currencies(
        &currencies,
        currency_cursor,
        budget,
        |reference_currency, budget| {
            let Some((window, ceiling)) =
                currency_ceiling(&bins(reference_currency), &mut windows, || {
                    scan_terms(reference_currency)
                })?
            else {
                return Ok(true);
            };
            walk(reference_currency, window, ceiling, budget)
        },
    )
}

/// The currency's window and the highest bin a breached entry can sit in. `None`
/// when the currency is settled for the pinned day: nothing left past its cursor, too
/// few priced days, or a price no bin covers. The last marks the day failed, so later
/// slices pass the currency by.
pub fn currency_ceiling<'w, 's, S: CallBinStore<'s>>(
    bins: &S,
    windows: &'w mut CallWindows<'_>,
    terms: impl FnOnce() -> Result<ScanTerms>,
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
            tracing::warn!(target: "outbe::oracle", reference_currency, error = ?error, "call pass: window price out of range, skipping currency for the day");
            bins.failed_day().write(&reference_currency, pinned_day)?;
            Ok(None)
        }
    }
}

/// Calls one entry in its own checkpoint. `call` returns the writes it made, 0 when
/// the entry was not called. A deterministic failure rolls back only this entry, and
/// running out of gas resumes on it next block.
pub fn call_entry<'s, S: CallPass<'s>>(
    storage: &StorageHandle<'_>,
    budget: &mut PassBudget,
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

    const ALL: Selection = Selection {
        process_call: true,
        process_forfeit: true,
        process_cleanup: true,
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

        fn work(&self, phase: u8, budget: &mut PassBudget) -> Result<(u32, bool)> {
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

    impl<'s> CallPass<'s> for TestSweep<'s> {
        const CONSUMER: &'static str = "test";
        const FORFEIT_PHASES: u8 = 2;
        const CLEANUP_PHASE: Option<u8> = Some(2);
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
            budget: &mut PassBudget,
        ) -> Result<(u32, bool)> {
            self.work(0, budget)
        }
        fn forfeit(
            &mut self,
            _ctx: &BlockRuntimeContext,
            _day: u32,
            phase: u8,
            budget: &mut PassBudget,
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
    fn all_flag_combinations_respect_each_stored_prerequisite() {
        for initial_phase in 0..=2 {
            for bits in 0..8 {
                let selection = Selection {
                    process_call: bits & 1 != 0,
                    process_forfeit: bits & 2 != 0,
                    process_cleanup: bits & 4 != 0,
                };
                fixture(|mut ctx, sweep| {
                    ready(&mut ctx, 1);
                    ctx.block.timestamp = date_key_to_utc_timestamp(20260102);
                    OracleContract::new(ctx.storage.clone())
                        .utc_day_vwap_last_finalized
                        .write(20260101)
                        .unwrap();
                    sweep
                        .days()
                        .open(PassDays {
                            current: 20260101,
                            latest: 20260101,
                        })
                        .unwrap();
                    sweep.phase.write(initial_phase).unwrap();
                    for phase in 0..=2 {
                        sweep.remaining.write(&phase, 1).unwrap();
                    }
                    continue_day(&ctx, sweep, selection).unwrap();
                    let enabled = [
                        selection.process_call,
                        selection.process_forfeit,
                        selection.process_cleanup,
                    ];
                    let mut blocked = false;
                    let mut expected_phase = initial_phase;
                    for phase in initial_phase..=2 {
                        blocked |= !enabled[usize::from(phase)];
                        assert_eq!(
                            sweep.remaining.read(&phase).unwrap(),
                            u32::from(blocked),
                            "phase {initial_phase}, flags {bits}, stage {phase}"
                        );
                        if !blocked {
                            expected_phase = (phase + 1).min(2);
                        }
                    }
                    for phase in 0..initial_phase {
                        assert_eq!(sweep.remaining.read(&phase).unwrap(), 1);
                    }
                    assert_eq!(sweep.phase.read().unwrap(), expected_phase);
                    assert_eq!(
                        sweep.current.read().unwrap(),
                        if blocked { 20260101 } else { 0 }
                    );
                    if !enabled[usize::from(initial_phase)] {
                        assert_eq!(sweep.allowance.read().unwrap(), U256::ZERO);
                    }
                });
            }
        }
    }

    #[test]
    fn disabled_prerequisites_cannot_pin_an_idle_allowance_or_schedule() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            let later_only = Selection {
                process_call: false,
                ..ALL
            };
            continue_day(&ctx, sweep, later_only).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 0);
            assert_eq!(sweep.latest.read().unwrap(), 0);
            assert_eq!(sweep.allowance.read().unwrap(), U256::ZERO);
            // An already scheduled call pass is also blocked without selecting a budget.
            schedule(&ctx, sweep).unwrap();
            continue_day(&ctx, sweep, later_only).unwrap();
            assert_eq!(sweep.allowance.read().unwrap(), U256::ZERO);
            sweep.remaining.write(&0, 1).unwrap();
            assert_eq!(continue_day(&ctx, sweep, ALL).unwrap(), 1);
            assert_eq!(sweep.current.read().unwrap(), 20260102);
        });
    }

    #[test]
    fn later_only_resume_shares_allowance_and_hands_over_without_scheduling() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            sweep.remaining.write(&0, 64).unwrap();
            sweep.remaining.write(&1, 1).unwrap();
            sweep.remaining.write(&2, 1).unwrap();
            let calls_only = Selection {
                process_call: true,
                ..Selection::default()
            };
            assert_eq!(continue_day(&ctx, sweep, calls_only).unwrap(), 64);
            assert_eq!(sweep.phase.read().unwrap(), 1);
            assert_eq!(pending_work(&ctx, sweep).unwrap(), (true, true, true));
            let forfeits_only = Selection {
                process_forfeit: true,
                ..Selection::default()
            };
            let saved = sweep.allowance.read().unwrap();
            continue_day(&ctx, sweep, forfeits_only).unwrap();
            assert_eq!(sweep.remaining.read(&1).unwrap(), 1);
            assert_eq!(sweep.allowance.read().unwrap(), saved);
            ctx.block.block_number = 2;
            continue_day(&ctx, sweep, forfeits_only).unwrap();
            assert_eq!(sweep.phase.read().unwrap(), 2);
            let cleanup_only = Selection {
                process_cleanup: true,
                ..Selection::default()
            };
            ctx.block.timestamp = date_key_to_utc_timestamp(20260106);
            OracleContract::new(ctx.storage.clone())
                .utc_day_vwap_last_finalized
                .write(20260105)
                .unwrap();
            continue_day(&ctx, sweep, cleanup_only).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 20260102);
            assert_eq!(
                sweep.latest.read().unwrap(),
                20260103,
                "later flags cannot schedule newer days"
            );
            sweep.remaining.write(&0, 1).unwrap();
            continue_day(&ctx, sweep, ALL).unwrap();
            assert_eq!(
                sweep.remaining.read(&0).unwrap(),
                1,
                "handover cannot execute a second observation day at this height"
            );
        });
    }

    #[test]
    fn pending_projection_is_read_only_and_preserves_genesis_range() {
        let mut provider = HashMapStorageProvider::new(1);
        let address = Address::with_last_byte(99);
        StorageHandle::enter(&mut provider, |storage| {
            let sweep = TestSweep::new(storage.clone(), address);
            sweep.genesis.write(20260101).unwrap();
            OracleContract::new(storage)
                .utc_day_vwap_last_finalized
                .write(20260103)
                .unwrap();
        });
        let before = provider.storage.clone();
        provider.set_static(true);
        StorageHandle::enter(&mut provider, |storage| {
            let mut ctx = BlockRuntimeContext::new(
                BlockContext::empty_for_tests(1, date_key_to_utc_timestamp(20260104), 1),
                storage.clone(),
            );
            let mut sweep = TestSweep::new(storage, address);
            assert_eq!(pending_work(&ctx, &sweep).unwrap(), (true, true, true));
            assert_eq!(
                projected_schedule(&ctx, &sweep).unwrap(),
                (
                    PassDays {
                        current: 20260101,
                        latest: 20260103
                    },
                    Scheduled::Opened
                )
            );
            // All false must not even initialize scheduling or persist an idle allowance.
            continue_day(&ctx, &mut sweep, Selection::default()).unwrap();
            ctx.block.timestamp = date_key_to_utc_timestamp(20260105);
            assert_eq!(
                pending_work(&ctx, &sweep).unwrap(),
                (false, false, false),
                "a new unfinalized closed day is not eligible"
            );
        });
        assert_eq!(provider.storage, before);
        assert!(provider.events.is_empty());
    }

    #[test]
    fn pending_obligations_include_lag_empty_phases_and_backlog_not_allowances() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            sweep
                .days()
                .open(PassDays {
                    current: 20260103,
                    latest: 20260103,
                })
                .unwrap();
            let oracle = OracleContract::new(ctx.storage.clone());
            oracle.utc_day_vwap_last_finalized.write(20260102).unwrap();
            sweep
                .allowance
                .write(
                    BlockAllowance {
                        height: 1,
                        day: 20260103,
                        budget: PassBudget::new(0, 0),
                    }
                    .pack(),
                )
                .unwrap();
            assert_eq!(pending_work(&ctx, sweep).unwrap(), (true, true, true));
            sweep.phase.write(1).unwrap();
            assert_eq!(pending_work(&ctx, sweep).unwrap(), (false, true, true));
            sweep.phase.write(2).unwrap();
            assert_eq!(pending_work(&ctx, sweep).unwrap(), (false, false, true));
            // Finishing cleanup cannot erase the call/forfeit obligations of a queued day.
            sweep.current.write(20260102).unwrap();
            assert_eq!(pending_work(&ctx, sweep).unwrap(), (true, true, true));
            sweep.current.write(0).unwrap();
            oracle.utc_day_vwap_last_finalized.write(20260103).unwrap();
            assert_eq!(pending_work(&ctx, sweep).unwrap(), (false, false, false));
            ctx.block.timestamp = date_key_to_utc_timestamp(20260105);
            oracle.utc_day_vwap_last_finalized.write(20260104).unwrap();
            assert_eq!(pending_work(&ctx, sweep).unwrap(), (true, true, true));
            sweep.phase.write(3).unwrap();
            assert!(pending_work(&ctx, sweep).is_err());
        });
    }

    #[test]
    fn phases_share_allowance_resume_and_cannot_advance_twice_at_one_height() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            sweep.remaining.write(&0, 40).unwrap();
            sweep.remaining.write(&1, 40).unwrap();
            sweep.remaining.write(&2, 1).unwrap();
            assert_eq!(continue_day(&ctx, sweep, ALL).unwrap(), 40);
            assert_eq!(sweep.current.read().unwrap(), 20260101);
            assert_eq!(sweep.phase.read().unwrap(), 1);
            assert_eq!(sweep.remaining.read(&1).unwrap(), 16);
            assert_eq!(sweep.remaining.read(&2).unwrap(), 1);
            let mut restarted = TestSweep::new(ctx.storage.clone(), Address::with_last_byte(99));
            let sweep = &mut restarted;
            assert_eq!(continue_day(&ctx, sweep, ALL).unwrap(), 0);
            assert_eq!(sweep.remaining.read(&1).unwrap(), 16);
            ctx.block.block_number = 2;
            continue_day(&ctx, sweep, ALL).unwrap();
            assert_eq!(sweep.remaining.read(&2).unwrap(), 0);
            assert_eq!(sweep.current.read().unwrap(), 20260102);
            let allowance = sweep.allowance.read().unwrap();
            continue_day(&ctx, sweep, ALL).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 20260102);
            assert_eq!(sweep.allowance.read().unwrap(), allowance);
            ctx.block.block_number = 3;
            continue_day(&ctx, sweep, ALL).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 20260103);
            ctx.block.block_number = 4;
            continue_day(&ctx, sweep, ALL).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 0);
            ctx.block.block_number = 5;
            continue_day(&ctx, sweep, ALL).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 0);
        });
    }

    #[test]
    fn oracle_readiness_retries_without_losing_genesis_or_empty_call_forfeits() {
        fixture(|mut ctx, sweep| {
            ctx.block.timestamp = date_key_to_utc_timestamp(20260104);
            ctx.block.block_number = 1;
            sweep.remaining.write(&1, 1).unwrap();
            continue_day(&ctx, sweep, ALL).unwrap();
            assert_eq!(sweep.current.read().unwrap(), 0);
            ready(&mut ctx, 2);
            initialize(&ctx, sweep).unwrap();
            assert_eq!(sweep.genesis.read().unwrap(), 20260101);
            continue_day(&ctx, sweep, ALL).unwrap();
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
            assert!(continue_day(&ctx, sweep, ALL).is_err());
            assert_eq!(sweep.remaining.read(&0).unwrap(), 1);
            assert_eq!(sweep.current.read().unwrap(), 0);
            assert_eq!(sweep.phase.read().unwrap(), 0);
            assert_eq!(sweep.allowance.read().unwrap(), U256::ZERO);
            sweep.fail_phase = None;
            assert_eq!(continue_day(&ctx, sweep, ALL).unwrap(), 1);
        });
    }

    #[test]
    fn incomplete_calls_hold_forfeits_and_the_pinned_day() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            sweep.remaining.write(&0, 70).unwrap();
            sweep.remaining.write(&1, 1).unwrap();
            assert_eq!(continue_day(&ctx, sweep, ALL).unwrap(), 64);
            assert_eq!(sweep.phase.read().unwrap(), 0);
            assert_eq!(sweep.remaining.read(&0).unwrap(), 6);
            assert_eq!(sweep.remaining.read(&1).unwrap(), 1);
            assert_eq!(sweep.current.read().unwrap(), 20260101);
            assert_eq!(continue_day(&ctx, sweep, ALL).unwrap(), 0);
            ctx.block.block_number = 2;
            assert_eq!(continue_day(&ctx, sweep, ALL).unwrap(), 6);
            assert_eq!(sweep.remaining.read(&1).unwrap(), 0);
            assert_eq!(sweep.current.read().unwrap(), 20260102);
        });
    }

    #[test]
    fn invalid_phase_and_missing_bootstrap_return_errors_without_progress() {
        fixture(|mut ctx, sweep| {
            ready(&mut ctx, 1);
            sweep.genesis.write(0).unwrap();
            assert!(continue_day(&ctx, sweep, ALL).is_err());
            sweep.genesis.write(20260101).unwrap();
            schedule(&ctx, sweep).unwrap();
            sweep.phase.write(3).unwrap();
            assert!(continue_day(&ctx, sweep, ALL).is_err());
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
            assert_eq!(continue_day(&ctx, stalled, ALL).unwrap(), 64);
            assert_eq!(continue_day(&ctx, &mut independent, ALL).unwrap(), 1);
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
