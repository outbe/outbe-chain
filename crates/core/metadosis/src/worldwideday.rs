//! The WorldwideDay lifecycle as an `smlang` FSM (`WorldwideDayLifecycle`).
//!
//! The day is *advanced* through its statuses by the clock
//! (`FORMING → LOOKBACK_DELAY → OFFERING → WAITING → READY`), then *settled* once
//! its metadosis limit is credited (`READY → IN_PROGRESS → COMPLETED/FAILED`).
//! Settlement itself is a separate machine — see [`crate::metadosis`]; this module
//! composes its terminal onto the day's status.
//!
//! Storage lives directly in the machine context ([`LifecycleCtx`]): smlang places
//! no `'static` bound on the context type, so the borrowed `StorageHandle<'storage>`
//! is held as a plain field — **no thread-local, no `unsafe`**. The status is
//! persisted as a `u8` and the machine is rebuilt each block via `new_with_state`.

use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result as MdResult},
};
use outbe_tribute::TributeContract;
use smlang::statemachine;

use crate::metadosis::{execute_metadosis, MetadosisStates};
use crate::precompile::IMetadosis;
use crate::schema::{DayType, MetadosisContract, Status, WorldwideDayEntryExt};

// `Status` (the canonical lifecycle status enum) is the single source of truth and
// lives in `schema.rs`; this module drives it through the clock walk + settlement.

/// Window boundaries for one day (loaded from storage).
#[derive(Debug, Clone, Copy)]
pub struct Windows {
    pub forming_end: u64,
    pub lookback_end: u64,
    pub offering_end: u64,
    pub scheduled: u64,
}

/// The status a block time falls into. Pure, total, deterministic.
pub fn time_phase(block_time: u64, w: &Windows) -> Status {
    use Status::*;
    if block_time < w.forming_end {
        Forming
    } else if block_time < w.lookback_end {
        LookbackDelay
    } else if block_time < w.offering_end {
        Offering
    } else if block_time < w.scheduled {
        Waiting
    } else {
        Ready
    }
}

// ---- the WorldwideDayLifecycle machine -----------------------------------------

/// Machine context: the scoped block runtime + the day. Storage is reached via
/// `self.rt.storage` — a borrowed, lifetime-bound handle, held with no `unsafe`.
/// (`BlockRuntimeContext<'storage>` is invariant, so the borrow `'a` and the
/// storage lifetime `'storage` are separate parameters.)
pub struct LifecycleCtx<'a, 'storage> {
    rt: &'a BlockRuntimeContext<'storage>,
    wwd: WorldwideDay,
}

impl<'storage> LifecycleCtx<'_, 'storage> {
    /// Short-lived Metadosis contract facade for one action — an `Rc` clone of the
    /// block's storage handle, never held across calls.
    fn md(&self) -> MetadosisContract<'storage> {
        MetadosisContract::new(self.rt.storage.clone())
    }
}

statemachine! {
    name: WorldwideDayLifecycle,
    custom_error: true,
    derive_states: [Debug, Clone, Copy],
    transitions: {
        // Clock-driven half — fallible per-edge actions touching storage.
        *Forming       + ResolveDayRate / on_resolve_day_rate = LookbackDelay,
        LookbackDelay  + OpenOffering   / on_open_offering    = Offering,
        Offering       + RevealOffering / on_reveal_offering  = Offering,
        Offering       + CloseOffering  / on_close_offering   = Waiting,
        Waiting        + BecomeReady                          = Ready,
        // Settlement coupling — each edge's action persists the status + retires.
        // Intentionally event-silent (no WorldwideDayStatusChange): unlike the
        // clock half, settlement status moves are surfaced via the Metadosis events.
        Ready          + BeginProcessing / on_begin_processing = InProgress,
        InProgress     + Complete        / on_complete         = Completed,
        InProgress     + Fail            / on_fail             = Failed,
    },
}

impl WorldwideDayLifecycleStateMachineContext for LifecycleCtx<'_, '_> {
    type Error = PrecompileError;

    /// Leaving FORMING: snapshot the forming-window VWAPs and resolve the day rate.
    fn on_resolve_day_rate(&mut self) -> MdResult<()> {
        let mut md = self.md();
        store_worldwide_day_vwap_snapshot(&mut md, self.wwd)?;
        resolve_day_rate(&mut md, self.wwd)
    }

    /// Opening OFFERING: unseal tributes and start the auction stage.
    fn on_open_offering(&mut self) -> MdResult<()> {
        let storage = self.rt.storage.clone();
        TributeContract::new(storage.clone()).unseal_day(self.wwd)?;
        let md = MetadosisContract::new(storage.clone());
        let ts = md
            .worldwide_days
            .entry(self.wwd)
            .scheduled_process_time()
            .read()?;
        let coen = md.worldwide_days.entry(self.wwd).current_vwap().read()?;
        outbe_desis::api::dispatch_stage_start(storage, ts, coen)?;
        Ok(())
    }

    /// Closing OFFERING: seal the day's tributes.
    fn on_close_offering(&mut self) -> MdResult<()> {
        TributeContract::new(self.rt.storage.clone()).seal_day(self.wwd)?;
        Ok(())
    }

    /// Staying inside OFFERING on a tick: reveal the auction stage.
    fn on_reveal_offering(&mut self) -> MdResult<()> {
        let md = self.md();
        let ts = md
            .worldwide_days
            .entry(self.wwd)
            .scheduled_process_time()
            .read()?;
        let green = DayType::try_from(md.get_wwd_day_type(self.wwd)?)? == DayType::Green;
        outbe_desis::api::dispatch_stage_reveal(self.rt.storage.clone(), ts, green)?;
        Ok(())
    }

    /// READY → IN_PROGRESS: claim the day for settlement.
    fn on_begin_processing(&mut self) -> MdResult<()> {
        self.md().mark_wwd_in_progress(self.wwd)
    }

    /// IN_PROGRESS → COMPLETED: persist + retire the settled day.
    fn on_complete(&mut self) -> MdResult<()> {
        self.md().mark_wwd_completed(self.wwd)
    }

    /// IN_PROGRESS → FAILED: persist + retire the failed day.
    fn on_fail(&mut self) -> MdResult<()> {
        self.md().mark_wwd_failed(self.wwd)
    }
}

/// Map our `Status` to the macro-generated state marker.
fn to_state(s: Status) -> WorldwideDayLifecycleStates {
    use WorldwideDayLifecycleStates as L;
    match s {
        Status::Forming => L::Forming,
        Status::LookbackDelay => L::LookbackDelay,
        Status::Offering => L::Offering,
        Status::Waiting => L::Waiting,
        Status::Ready => L::Ready,
        Status::InProgress => L::InProgress,
        Status::Completed => L::Completed,
        Status::Failed => L::Failed,
    }
}

/// The clock-driven event that advances one status forward. READY and the
/// settlement statuses do not advance on a tick.
fn advance_event(s: WorldwideDayLifecycleStates) -> Option<WorldwideDayLifecycleEvents> {
    use WorldwideDayLifecycleEvents as E;
    use WorldwideDayLifecycleStates as L;
    match s {
        L::Forming => Some(E::ResolveDayRate),
        L::LookbackDelay => Some(E::OpenOffering),
        L::Offering => Some(E::CloseOffering),
        L::Waiting => Some(E::BecomeReady),
        L::Ready | L::InProgress | L::Completed | L::Failed => None,
    }
}

fn map_smlang_err(e: WorldwideDayLifecycleError<PrecompileError>) -> PrecompileError {
    use WorldwideDayLifecycleError as E;
    match e {
        E::GuardFailed(err) | E::ActionFailed(err) => err,
        other => PrecompileError::Revert(format!("worldwideday transition error: {other:?}")),
    }
}

fn persist_status_change(
    rt: &BlockRuntimeContext,
    wwd: WorldwideDay,
    from: Status,
    to: Status,
) -> MdResult<()> {
    let mut md = MetadosisContract::new(rt.storage.clone());
    md.write_status(wwd, to)?;
    md.emit(IMetadosis::WorldwideDayStatusChange {
        worldwideDay: wwd.into(),
        oldStatus: from as u8,
        newStatus: to as u8,
        blockNumber: rt.block.block_number,
    })
}

impl MetadosisContract<'_> {
    /// Load a day's clock-window boundaries in one read — the single seam shared by
    /// the clock driver and tests, replacing scattered `entry().*_end().read()`.
    pub fn get_day_windows(&self, wwd: WorldwideDay) -> MdResult<Windows> {
        let e = self.worldwide_days.entry(wwd);
        Ok(Windows {
            forming_end: e.forming_end().read()?,
            lookback_end: e.lookback_end().read()?,
            offering_end: e.offering_end().read()?,
            scheduled: e.scheduled_process_time().read()?,
        })
    }
}

// ---- clock driver --------------------------------------------------------------

/// Advance one WorldwideDay by a clock tick. Rebuilds the lifecycle machine at the
/// stored status (`new_with_state`), walks forward to `time_phase(now)` firing each
/// crossed edge's action once, and writes one `WorldwideDayStatusChange`. Staying
/// inside OFFERING reveals the auction. READY and terminal statuses are settlement.
pub fn advance_worldwide_day(rt: &BlockRuntimeContext, wwd: WorldwideDay) -> MdResult<u8> {
    let md = MetadosisContract::new(rt.storage.clone());
    let start = Status::try_from(md.get_wwd_status(wwd)?)?;
    if !start.is_time_driven() {
        return Ok(start as u8);
    }

    let windows = md.get_day_windows(wwd)?;
    let target = time_phase(rt.block.timestamp, &windows);

    if target == start {
        if start == Status::Offering {
            let mut day = WorldwideDayLifecycleStateMachine::new_with_state(
                LifecycleCtx { rt, wwd },
                to_state(start),
            );
            day.process_event(WorldwideDayLifecycleEvents::RevealOffering)
                .map_err(map_smlang_err)?;
        }
        return Ok(start as u8);
    }
    if target < start {
        return Ok(start as u8);
    }

    let target_state = to_state(target);
    let mut day = WorldwideDayLifecycleStateMachine::new_with_state(
        LifecycleCtx { rt, wwd },
        to_state(start),
    );
    while *day.state() != target_state {
        let Some(ev) = advance_event(*day.state()) else {
            return Err(PrecompileError::Revert(format!(
                "worldwideday stalled toward {target:?}"
            )));
        };
        day.process_event(ev).map_err(map_smlang_err)?;
    }

    persist_status_change(rt, wwd, start, target)?;
    Ok(target as u8)
}

// ---- settlement composition ----------------------------------------------------

/// Settle a READY worldwide day. The lifecycle machine owns the settlement
/// transitions — `BeginProcessing`/`Complete`/`Fail` each persist the status and
/// retire the day via their actions. Runs the [`Metadosis`](crate::metadosis)
/// machine and maps its terminal onto the event: `Settled` → `Complete`,
/// `Aborted` → `Fail`; lysis corruption (`Err`) → `Fail` + propagate so the block
/// reverts.
pub fn process_metadosis(rt: &BlockRuntimeContext, wwd: WorldwideDay) -> MdResult<()> {
    let start = Status::try_from(MetadosisContract::new(rt.storage.clone()).get_wwd_status(wwd)?)?;

    let mut day = WorldwideDayLifecycleStateMachine::new_with_state(
        LifecycleCtx { rt, wwd },
        to_state(start),
    );
    day.process_event(WorldwideDayLifecycleEvents::BeginProcessing) // READY → IN_PROGRESS
        .map_err(map_smlang_err)?;

    match execute_metadosis(rt, wwd) {
        Ok(MetadosisStates::Settled) => day
            .process_event(WorldwideDayLifecycleEvents::Complete)
            .map(|_| ())
            .map_err(map_smlang_err),
        Ok(MetadosisStates::Aborted) => day
            .process_event(WorldwideDayLifecycleEvents::Fail)
            .map(|_| ())
            .map_err(map_smlang_err),
        Ok(other) => Err(PrecompileError::Revert(format!(
            "metadosis settled into non-terminal state {other:?}"
        ))),
        Err(err) => {
            // Settle the day FAILED (its action retires it), then propagate the
            // corruption so the block reverts.
            day.process_event(WorldwideDayLifecycleEvents::Fail)
                .map_err(map_smlang_err)?;
            Err(err)
        }
    }
}

// ---- forming-window day-rate effects (shared with the resolve_day_rate edge) ---

/// Snapshot the day's forming-window VWAPs into the Oracle. A window with no oracle
/// data is a deterministic no-op (the Oracle reports `false`); the day then
/// resolves to RED via [`resolve_day_rate`] reading `None`.
pub(crate) fn store_worldwide_day_vwap_snapshot(
    md: &mut MetadosisContract,
    wwd: WorldwideDay,
) -> MdResult<()> {
    let forming_start = md.worldwide_days.entry(wwd).forming_start().read()?;
    let forming_end = md.worldwide_days.entry(wwd).forming_end().read()?;
    outbe_oracle::api::store_worldwide_day_vwap_snapshot(
        md.storage.clone(),
        wwd,
        forming_start,
        forming_end,
    )?;
    Ok(())
}

/// Resolve and persist the day's current/previous VWAP and GREEN/RED type from the
/// Oracle's `COEN/0xUSD` snapshots. Missing data (`None`) reads as zero; the
/// zero-VWAP⇒RED rule lives in [`determine_day_type`] just below.
/// Genuine Oracle faults propagate (no silent zero-fallback).
pub(crate) fn resolve_day_rate(md: &mut MetadosisContract, wwd: WorldwideDay) -> MdResult<()> {
    let current_vwap =
        outbe_oracle::api::day_type_pair_vwap(md.storage.clone(), wwd)?.unwrap_or(U256::ZERO);

    // When today has no VWAP the day is already RED regardless of yesterday, so
    // skip the second Oracle read.
    let previous_vwap = if current_vwap.is_zero() {
        U256::ZERO
    } else {
        outbe_oracle::api::day_type_pair_vwap(md.storage.clone(), wwd.previous_date_key())?
            .unwrap_or(U256::ZERO)
    };

    md.worldwide_days
        .entry(wwd)
        .previous_vwap()
        .write(previous_vwap)?;
    md.worldwide_days
        .entry(wwd)
        .current_vwap()
        .write(current_vwap)?;
    md.set_wwd_day_type(wwd, determine_day_type(previous_vwap, current_vwap))?;
    Ok(())
}

/// GREEN if the rate rose vs. the previous day, RED otherwise (including any
/// missing/zero VWAP). The day-rate resolution rule, used by [`resolve_day_rate`].
fn determine_day_type(previous_vwap: U256, current_vwap: U256) -> DayType {
    if previous_vwap.is_zero() || current_vwap.is_zero() {
        return DayType::Red;
    }
    if current_vwap > previous_vwap {
        DayType::Green
    } else {
        DayType::Red
    }
}

// ---- tests: pure core, storage-free --------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn determine_day_type_rules() {
        let one = U256::from(1u64);
        let two = U256::from(2u64);
        assert_eq!(determine_day_type(one, two), DayType::Green);
        assert_eq!(determine_day_type(two, one), DayType::Red);
        assert_eq!(determine_day_type(one, one), DayType::Red);
        assert_eq!(determine_day_type(U256::ZERO, two), DayType::Red);
        assert_eq!(determine_day_type(one, U256::ZERO), DayType::Red);
    }

    fn windows() -> Windows {
        Windows {
            forming_end: 100,
            lookback_end: 200,
            offering_end: 300,
            scheduled: 312,
        }
    }

    #[test]
    fn time_phase_ladder() {
        let w = windows();
        assert_eq!(time_phase(50, &w), Status::Forming);
        assert_eq!(time_phase(100, &w), Status::LookbackDelay);
        assert_eq!(time_phase(150, &w), Status::LookbackDelay);
        assert_eq!(time_phase(250, &w), Status::Offering);
        assert_eq!(time_phase(305, &w), Status::Waiting);
        assert_eq!(time_phase(312, &w), Status::Ready);
        assert_eq!(time_phase(99_999, &w), Status::Ready);
    }

    #[test]
    fn bootstrap_zero_lookback_collapses() {
        let w = Windows {
            forming_end: 100,
            lookback_end: 100,
            offering_end: 300,
            scheduled: 312,
        };
        assert_eq!(time_phase(100, &w), Status::Offering);
    }

    #[test]
    fn status_u8_roundtrip() {
        for v in 0u8..=7 {
            assert_eq!(Status::try_from(v).unwrap() as u8, v);
        }
        assert!(Status::try_from(8).is_err());
    }

    #[test]
    fn only_forming_through_waiting_are_time_driven() {
        use Status::*;
        for s in [Forming, LookbackDelay, Offering, Waiting] {
            assert!(s.is_time_driven());
        }
        for s in [Ready, InProgress, Completed, Failed] {
            assert!(!s.is_time_driven());
        }
    }

    #[test]
    fn advance_event_topology_is_forward_then_none() {
        use WorldwideDayLifecycleEvents as E;
        use WorldwideDayLifecycleStates as L;
        assert!(matches!(advance_event(L::Forming), Some(E::ResolveDayRate)));
        assert!(matches!(
            advance_event(L::LookbackDelay),
            Some(E::OpenOffering)
        ));
        assert!(matches!(advance_event(L::Offering), Some(E::CloseOffering)));
        assert!(matches!(advance_event(L::Waiting), Some(E::BecomeReady)));
        assert!(advance_event(L::Ready).is_none());
    }
}
