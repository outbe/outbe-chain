use super::clearing::{clearing_run_gas, fetch_targets};
use super::*;
use outbe_primitives::error::PrecompileError;
use sweep::Sweep;

mod sweep;

/// Cycle `advance_auctions` trigger: advance every scheduled day that has a due step.
/// Each day runs in its own checkpoint. A day that reverts is retried on a later pass.
/// The sweep stops before a day whose due steps the rest of the trigger budget cannot pay for.
pub fn advance_auctions(ctx: &BlockRuntimeContext) -> Result<()> {
    schedule_tick(&ctx.storage, ctx.block.timestamp)
}

pub(crate) fn schedule_tick(storage: &StorageHandle<'_>, now: u64) -> Result<()> {
    storage.with_checkpoint(|| poll_schedule(storage, now))
}

fn poll_schedule(storage: &StorageHandle<'_>, now: u64) -> Result<()> {
    let contract = storage.contract::<DesisContract>();
    let count = contract.sched_active_count.read()?;
    if count == 0 {
        return Ok(());
    }
    let cursor = contract.sched_poll_cursor.read()?;
    let mut sweep = Sweep::new(cursor % count, count);
    let mut next = 0;
    while let Some(index) = sweep.next() {
        let day = contract.sched_active_at.read(&index)?.into();
        if !budget_pays_for(storage, due_work_gas(&contract, day, now)?)? {
            // The next run starts with the days that this run did not reach.
            next = index;
            break;
        }
        match storage.with_checkpoint(|| advance_day(storage, day, now)) {
            Ok(()) => {}
            Err(e @ (PrecompileError::Revert(_) | PrecompileError::RevertBytes(_))) => {
                tracing::warn!(target: "outbe::desis", %day, error = ?e, "schedule tick: skipping day");
            }
            Err(e) => return Err(e),
        }
        if contract.sched_active_slot.read(&day)? == 0 {
            sweep.removed(contract.sched_active_count.read()?);
        } else {
            sweep.kept();
        }
    }
    if next != cursor {
        contract.sched_poll_cursor.write(next)?;
    }
    Ok(())
}

/// Whether the rest of the trigger budget pays for `gas`. A run outside a trigger scope has no budget.
fn budget_pays_for(storage: &StorageHandle<'_>, gas: u64) -> Result<bool> {
    Ok(storage
        .trigger_budget_remaining()?
        .is_none_or(|remaining| remaining >= gas))
}

/// The anchor and the windows that `advance_day` reads for a day.
fn day_windows(contract: &DesisContract<'_>, emission_day: EmissionDate) -> Result<(u64, Windows)> {
    let anchor = u64::from(contract.auction_at.read(&emission_day)?);
    Ok((anchor, Windows::from_anchor(anchor)))
}

/// Upper bound of the work `advance_day` does for a day at `now`, over every step that is due.
fn due_work_gas(contract: &DesisContract<'_>, emission_day: EmissionDate, now: u64) -> Result<u64> {
    let mut stage = contract.read_stage(emission_day)?;
    let (anchor, windows) = day_windows(contract, emission_day)?;
    if due_cancel(contract, emission_day, stage, windows, now)?.is_some() {
        return Ok(ADVANCE_DAY_BASE_GAS + ADVANCE_START_GAS);
    }
    let mut gas = ADVANCE_DAY_BASE_GAS;
    // A commit end flips the stage, and the reveal end can be due in the same run.
    if stage == AuctionStage::CommittingBids
        && now >= windows.commit_end
        && !windows.unfinished_past_deadline(stage, now)
    {
        gas += ADVANCE_DAY_BASE_GAS;
        stage = AuctionStage::RevealingBids;
    }
    Ok(gas + step_gas(stage, anchor, windows, now))
}

/// The work of the one step due for a day at `stage`, after any commit end.
fn step_gas(stage: AuctionStage, anchor: u64, windows: Windows, now: u64) -> u64 {
    match stage {
        AuctionStage::Completed | AuctionStage::Canceled | AuctionStage::Failed => 0,
        _ if windows.unfinished_past_deadline(stage, now) => ADVANCE_FAIL_GAS,
        // A start leaves at least `MIN_COMMIT_WINDOW_SECONDS` of commit, or cancels the day.
        AuctionStage::None if now >= anchor => ADVANCE_START_GAS,
        // The larger of the two ends of the reveal stage: the clearing start or the cancel.
        AuctionStage::RevealingBids if now >= windows.reveal_end => {
            ADVANCE_REVEAL_END_GAS.max(ADVANCE_START_GAS)
        }
        _ => 0,
    }
}

/// Walk one day's schedule: start at the anchor, move to `RevealingBids` at commit end, collect
/// the bids at reveal end, and end a day that found no result by its last clearing time. A
/// cancel that a brief or Metadosis asked for comes first, until that last clearing time.
fn advance_day(storage: &StorageHandle<'_>, emission_day: EmissionDate, now: u64) -> Result<()> {
    while advance_step(storage, emission_day, now)? {}
    Ok(())
}

/// One step of a day's schedule. `true` when the day moved on and another step may be due.
fn advance_step(storage: &StorageHandle<'_>, emission_day: EmissionDate, now: u64) -> Result<bool> {
    let mut contract = storage.contract::<DesisContract>();
    let stage = contract.read_stage(emission_day)?;
    let (anchor, windows) = day_windows(&contract, emission_day)?;
    if let Some(reason) = due_cancel(&contract, emission_day, stage, windows, now)? {
        cancel_auction(storage, &mut contract, emission_day, stage, reason)?;
        return Ok(false);
    }
    match stage {
        AuctionStage::Completed | AuctionStage::Canceled | AuctionStage::Failed => {
            contract.remove_sched_active(emission_day).map(|()| false)
        }
        _ if windows.unfinished_past_deadline(stage, now) => {
            end_past_deadline(storage, &mut contract, emission_day, stage).map(|()| false)
        }
        AuctionStage::None if now >= anchor => {
            let outcome = start_auction(storage, &mut contract, emission_day, windows, now)?;
            Ok(matches!(outcome, StartOutcome::Started))
        }
        AuctionStage::CommittingBids if now >= windows.commit_end => contract
            .write_stage(emission_day, AuctionStage::RevealingBids)
            .map(|()| true),
        AuctionStage::RevealingBids if now >= windows.reveal_end => {
            end_reveal(storage, &mut contract, emission_day, now).map(|()| false)
        }
        _ => Ok(false),
    }
}

/// A day out of time: one that never started is a late start, a started one failed.
fn end_past_deadline(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
    stage: AuctionStage,
) -> Result<()> {
    if stage == AuctionStage::None {
        cancel_auction(storage, contract, emission_day, stage, CANCELED_LATE_START)
    } else {
        fail_auction(storage, contract, emission_day, stage)
    }
}

/// The clearing needs the size, so a day with no brief by the end of its reveal is canceled.
fn end_reveal(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
    now: u64,
) -> Result<()> {
    if contract.desis_limit_set.read(&emission_day)? == 0 {
        cancel_auction(
            storage,
            contract,
            emission_day,
            AuctionStage::RevealingBids,
            CANCELED_NO_LIMIT,
        )
    } else {
        arm_clearing(storage, emission_day, now)
    }
}

/// The cancel due now. Past its last clearing time a started day fails instead, so a cancel that
/// cannot be sent does not hold the day.
fn due_cancel(
    contract: &DesisContract<'_>,
    emission_day: EmissionDate,
    stage: AuctionStage,
    windows: Windows,
    now: u64,
) -> Result<Option<u8>> {
    if stage != AuctionStage::None && windows.unfinished_past_deadline(stage, now) {
        return Ok(None);
    }
    pending_cancel(contract, emission_day, stage)
}

/// The cancel a day with no result waits for: one that was asked for, or one its brief gives a
/// started day.
fn pending_cancel(
    contract: &DesisContract<'_>,
    emission_day: EmissionDate,
    stage: AuctionStage,
) -> Result<Option<u8>> {
    if !matches!(
        stage,
        AuctionStage::None | AuctionStage::CommittingBids | AuctionStage::RevealingBids
    ) {
        return Ok(None);
    }
    let asked = contract.cancel_reason.read(&emission_day)?;
    if asked != 0 {
        return Ok(Some(asked));
    }
    if stage == AuctionStage::None {
        return Ok(None);
    }
    let promis_load_minor = u128::try_from(contract.config_promis_load_minor.read(&emission_day)?)
        .map_err(|_| DesisError::InvalidEmissionDay(emission_day))?;
    brief_cancel_reason(contract, emission_day, promis_load_minor)
}

/// Cancel a day before its result. A started day sends the cancel to every chain it started on.
/// A day that never started is canceled with no message.
fn cancel_auction(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
    stage: AuctionStage,
    reason: u8,
) -> Result<()> {
    if stage != AuctionStage::None {
        send_intex_auction_cancel(storage, emission_day, reason)?;
    }
    contract.emit(IDesis::DesisCanceled {
        emissionDay: emission_day.into(),
        reason,
    })?;
    return_unused_desis_limit(storage, contract, emission_day)?;
    contract.cancel_reason.write(&emission_day, 0)?;
    contract.write_stage(emission_day, AuctionStage::Canceled)?;
    if stage != AuctionStage::None {
        contract.queue_intake_cleanup(emission_day)?;
    }
    contract.remove_sched_active(emission_day)
}

/// A started day with no result by its last clearing time. Queue its bids for cleanup. Nothing goes to
/// the other chains: they read the day as failed from its own schedule, and the refund time limit
/// returns the bids there.
fn fail_auction(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
    stage: AuctionStage,
) -> Result<()> {
    contract.emit(IDesis::DesisFailed {
        emissionDay: emission_day.into(),
        stage: IDesis::AuctionStage::try_from(stage as u8)
            .map_err(|_| DesisError::InvalidStageTransition)?,
    })?;
    contract.remove_gate_active(emission_day)?;
    contract.clearing_initiated.write(&emission_day, 0u8)?;
    contract.pending_desis_limit_units.write(&emission_day, 0)?;
    contract.clearing_deadline.clear(&emission_day)?;
    contract.day_bid_count.write(&emission_day, 0)?;
    return_unused_desis_limit(storage, contract, emission_day)?;
    contract.write_stage(emission_day, AuctionStage::Failed)?;
    contract.queue_intake_cleanup(emission_day)?;
    contract.remove_sched_active(emission_day)
}

/// The last moment a started day may still clear: early enough that its result reaches every
/// chain before the day ends there.
pub(super) fn last_clearing(
    contract: &DesisContract<'_>,
    emission_day: EmissionDate,
) -> Result<u64> {
    let anchor = u64::from(contract.auction_at.read(&emission_day)?);
    Ok(Windows::from_anchor(anchor).last_clearing)
}

/// u32 wire timestamp (bounded until 2106).
fn ts32(ts: u64) -> Result<u32> {
    u32::try_from(ts).map_err(|_| DesisError::ScheduleTimestampExceedsU32.into())
}

/// The ends of a day's commit, reveal and issuance windows, and its last clearing time.
#[derive(Clone, Copy)]
struct Windows {
    commit_end: u64,
    reveal_end: u64,
    issuance_end: u64,
    last_clearing: u64,
}

impl Windows {
    /// Whether the day is out of time: unstarted past its issuance window, or started and past
    /// its last clearing time.
    fn unfinished_past_deadline(&self, stage: AuctionStage, now: u64) -> bool {
        match stage {
            AuctionStage::None => now >= self.issuance_end,
            AuctionStage::CommittingBids
            | AuctionStage::RevealingBids
            | AuctionStage::CollectingBids => now >= self.last_clearing,
            _ => false,
        }
    }

    fn from_anchor(anchor: u64) -> Self {
        let commit_end = anchor.saturating_add(COMMIT_WINDOW_SECONDS);
        let reveal_end = commit_end.saturating_add(REVEAL_WINDOW_SECONDS);
        let issuance_end = reveal_end.saturating_add(INTEX_ISSUANCE_WINDOW_SECONDS);
        Self {
            commit_end,
            reveal_end,
            issuance_end,
            last_clearing: issuance_end.saturating_sub(RESULT_DELIVERY_MARGIN_SECONDS),
        }
    }
}

enum StartOutcome {
    /// Auction started. The schedule loop continues from `CommittingBids`.
    Started,
    /// Day was canceled and retired. The schedule loop stops.
    Retired,
}

/// Start a scheduled day. An unpriced or late day, or one whose brief came with no limit or a
/// sub-unit limit, is canceled with no message. Any other day starts live.
fn start_auction(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
    windows: Windows,
    now: u64,
) -> Result<StartOutcome> {
    let Windows {
        commit_end,
        reveal_end,
        issuance_end,
        ..
    } = windows;
    let rows: Vec<ReferenceCurrencyPrice> = outbe_oracle::api::priced_reference_currencies_of_day(
        storage.clone(),
        emission_day.value(),
    )?
    .into_iter()
    .map(|(iso_code, entry_price_minor)| ReferenceCurrencyPrice {
        iso_code,
        entry_price_minor,
        // `fix_call_terms` fixes it once the day's rows are chosen.
        call_price_minor: U256::ZERO,
    })
    .collect();
    let promis_load_minor = set_intex_promis_load(contract, emission_day, &rows)?;
    let mut config = AuctionConfig::from_reference_prices(rows, promis_load_minor);
    fold_profile(storage, contract, &mut config)?;
    fix_call_terms(storage, &mut config)?;
    contract.write_auction_config(emission_day, &config)?;
    let start = IntexAuctionStart {
        emission_day,
        config: &config,
        commit_end: ts32(commit_end)?,
        reveal_end: ts32(reveal_end)?,
        issuance_end: ts32(issuance_end)?,
    };

    let brief = brief_cancel_reason(contract, emission_day, config.promis_load_minor)?;
    let unpriced = config.reference_prices.is_empty();
    let late = commit_end.saturating_sub(now) < MIN_COMMIT_WINDOW_SECONDS;
    let no_targets = outbe_intexfactory::targets::auction_target_chains(storage)?.is_empty();
    let reason = [
        brief.filter(|reason| *reason == CANCELED_NO_LIMIT),
        unpriced.then_some(CANCELED_UNPRICED),
        brief,
        late.then_some(CANCELED_LATE_START),
        no_targets.then_some(CANCELED_NO_TARGETS),
    ]
    .into_iter()
    .flatten()
    .next();
    if let Some(reason) = reason {
        cancel_auction(storage, contract, emission_day, AuctionStage::None, reason)?;
        return Ok(StartOutcome::Retired);
    }
    send_intex_auction_start(storage, start)?;
    contract.write_stage(emission_day, AuctionStage::CommittingBids)?;
    contract.emit(IDesis::DesisStarted {
        emissionDay: emission_day.into(),
        commitEnd: commit_end,
        revealEnd: reveal_end,
        issuanceEnd: issuance_end,
        unitPromisLoadMinor: U256::from(config.promis_load_minor),
        minBidRate: config.min_bid_rate,
        minBidQuantity: u32::from(config.min_bid_quantity),
        bidBondMinor: U256::from(config.bid_bond_minor),
    })?;
    Ok(StartOutcome::Started)
}

/// Return a retiring day's unused Desis Limit to PromisLimit and record a zero
/// allocation. A zero remainder is not credited.
fn return_unused_desis_limit(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
) -> Result<()> {
    let unused = contract.pending_desis_limit_minor.read(&emission_day)?;
    contract
        .pending_desis_limit_minor
        .write(&emission_day, U256::ZERO)?;
    contract.emit(IDesis::DesisLimitAllocated {
        emissionDay: emission_day.into(),
        amountMinor: U256::ZERO,
    })?;
    if !unused.is_zero() {
        PromisLimitContract::new(storage.clone())
            .add(outbe_promislimit::LimitSource::DesisLimit, unused)?;
    }
    Ok(())
}

/// Arm the clearing from the briefed Desis Limit: convert raw PROMIS to whole Intex
/// units, start the fan-in gate and broadcast the collecting-bids stage.
fn arm_clearing(storage: &StorageHandle<'_>, emission_day: EmissionDate, now: u64) -> Result<()> {
    let mut contract = storage.contract::<DesisContract>();
    let config = contract.read_auction_config(emission_day)?;
    if config.promis_load_minor == 0 {
        return Err(DesisError::InvalidEmissionDay(emission_day).into());
    }
    let desis_limit_minor = u128::try_from(contract.pending_desis_limit_minor.read(&emission_day)?)
        .map_err(|_| DesisError::InvalidEmissionDay(emission_day))?;
    let desis_limit_units =
        (desis_limit_minor / config.promis_load_minor).min(u128::from(u32::MAX)) as u32;

    contract.clearing_initiated.write(&emission_day, 1u8)?;
    contract
        .pending_desis_limit_units
        .write(&emission_day, desis_limit_units)?;
    contract
        .clearing_deadline
        .write(&emission_day, now.saturating_add(BIDS_FANIN_TIMEOUT_SECS))?;
    contract.push_gate_active(emission_day)?;
    contract.write_stage(emission_day, AuctionStage::CollectingBids)?;

    let targets = fetch_targets(storage, emission_day)?;
    if targets.len() > MAX_TARGET_CHAINS {
        return Err(DesisError::TooManyTargets(emission_day, targets.len()).into());
    }
    let payload = outbe_intex::wire::encode_intex_auction_clearing_start(emission_day.value());
    let label = Label::new(
        MSG_INTEX_AUCTION_CLEARING_START,
        emission_day.value(),
        [0; 3],
    );
    for chain_id in targets {
        let gas = outbe_intex::gas::intex_auction_clearing_start(clearing_run_gas(
            storage,
            emission_day,
            chain_id,
        )?);
        contract
            .clearing_gas
            .write(&DesisContract::chain_key(emission_day, chain_id), gas)?;
        crate::outbox::push_to(storage, &[chain_id], label, gas, &payload)?;
    }
    Ok(())
}
