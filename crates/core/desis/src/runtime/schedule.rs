use super::clearing::{clearing_round_gas, fetch_targets};
use super::*;

/// Cycle `auction_advance` trigger: advance every scheduled auction. Each day
/// runs in its own checkpoint. An Err reverts that day (retried next slot).
pub fn tick_schedule(ctx: &BlockRuntimeContext) -> Result<()> {
    schedule_tick(&ctx.storage, ctx.block.timestamp)
}

pub(crate) fn schedule_tick(storage: &StorageHandle<'_>, now: u64) -> Result<()> {
    let count = {
        let contract = storage.contract::<DesisContract>();
        contract.sched_active_count.read()?
    };
    if count == 0 {
        return Ok(());
    }
    // Snapshot the set before iterating: transitions swap-pop it.
    let mut days = Vec::with_capacity(count as usize);
    {
        let contract = storage.contract::<DesisContract>();
        for i in 0..count {
            days.push(contract.sched_active_at.read(&i)?.into());
        }
    }
    for day in days {
        let res = storage.with_checkpoint(|| advance_day(storage, day, now));
        if let Err(e) = res {
            tracing::warn!(target: "outbe::desis", %day, error = ?e, "schedule tick: skipping day");
        }
    }
    Ok(())
}

/// Walk one day's schedule: start at the anchor, move to `RevealingBids` at commit end, collect
/// the bids at reveal end, and end a day that found no result by its last clearing time.
fn advance_day(storage: &StorageHandle<'_>, emission_day: EmissionDate, now: u64) -> Result<()> {
    loop {
        let mut contract = storage.contract::<DesisContract>();
        let stage = contract.read_stage(emission_day)?;
        let stored_anchor = u64::from(contract.auction_at.read(&emission_day)?);
        // An e2e day never reaches its production anchor, so a briefed day starts
        // from the tick that observes the brief.
        #[cfg(feature = "e2e-test")]
        let anchor = if stage == AuctionStage::Briefed {
            now
        } else {
            stored_anchor
        };
        #[cfg(not(feature = "e2e-test"))]
        let anchor = stored_anchor;
        let windows = Windows::from_anchor(anchor);
        match stage {
            AuctionStage::Completed | AuctionStage::Cancelled | AuctionStage::Failed => {
                return contract.remove_sched_active(emission_day);
            }
            _ if windows.unfinished_past_deadline(stage, now) => {
                return if stage == AuctionStage::Briefed {
                    end_unstarted(storage, &mut contract, emission_day, windows, now)
                } else {
                    fail_auction(storage, &mut contract, emission_day, stage)
                };
            }
            AuctionStage::Briefed if now >= anchor => {
                // The chain has to run the windows the START message announces.
                #[cfg(feature = "e2e-test")]
                contract.auction_at.write(&emission_day, ts32(anchor)?)?;
                if let StartOutcome::Retired =
                    start_auction(storage, &mut contract, emission_day, windows, now)?
                {
                    return Ok(());
                }
            }
            AuctionStage::CommittingBids if now >= windows.commit_end => {
                contract.write_stage(emission_day, AuctionStage::RevealingBids)?;
            }
            AuctionStage::RevealingBids if now >= windows.reveal_end => {
                return arm_clearing(storage, emission_day, now);
            }
            _ => return Ok(()),
        }
    }
}

/// A day that never started, out of time: it is sent as a late start while the router accepts
/// it, and is cancelled otherwise.
fn end_unstarted(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
    windows: Windows,
    now: u64,
) -> Result<()> {
    let started_late = storage
        .with_checkpoint(|| start_auction(storage, contract, emission_day, windows, now))
        .is_ok();
    if started_late {
        Ok(())
    } else {
        cancel_unstarted(storage, contract, emission_day)
    }
}

/// A day whose START no chain ever took. It never ran, so it is cancelled with no message.
fn cancel_unstarted(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
) -> Result<()> {
    contract.emit(IDesis::AuctionCancelledLateStart {
        emissionDay: emission_day.into(),
    })?;
    refund_unused_desis_limit(storage, contract, emission_day)?;
    contract.write_stage(emission_day, AuctionStage::Cancelled)?;
    contract.remove_sched_active(emission_day)
}

/// A started day with no result by its last clearing time. Its bids are dropped. Nothing goes to
/// the other chains: they read the day as failed from its own schedule, and the refund time limit
/// returns the bids there.
fn fail_auction(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
    stage: AuctionStage,
) -> Result<()> {
    contract.emit(IDesis::AuctionFailed {
        emissionDay: emission_day.into(),
        stage: stage as u8,
    })?;
    contract.remove_gate_active(emission_day)?;
    contract.clearing_initiated.write(&emission_day, 0u8)?;
    contract.pending_desis_limit_units.write(&emission_day, 0)?;
    contract.clearing_deadline.clear(&emission_day)?;
    // The router names the day's chains. A day must still end when the router cannot answer,
    // so its bids then stay behind.
    match fetch_targets(storage, emission_day) {
        Ok(chains) => {
            for chain_id in chains {
                contract.reset_chain_intake(emission_day, chain_id)?;
            }
            contract.day_bid_count.write(&emission_day, 0)?;
        }
        Err(error) => tracing::warn!(
            target: "outbe::desis",
            %emission_day,
            error = ?error,
            "failed day: bids kept, the router named no chains"
        ),
    }
    refund_unused_desis_limit(storage, contract, emission_day)?;
    contract.write_stage(emission_day, AuctionStage::Failed)?;
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
            AuctionStage::Briefed => now >= self.issuance_end,
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
    /// Day was cancelled and retired. The schedule loop stops.
    Retired,
}

/// Dispatch the START message for a briefed day: a day with no limit, an unpriced, sub-unit
/// or late day is born cancelled, otherwise it starts live.
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
    let promis_load_minor = step_promis_load(contract, emission_day, &rows)?;
    let mut config = AuctionConfig::from_reference_prices(rows, promis_load_minor);
    fold_profile(storage, contract, &mut config)?;
    fix_call_terms(storage, &mut config)?;
    contract.write_auction_config(emission_day, &config)?;
    let start = StageStart {
        emission_day,
        config: &config,
        commit_end: ts32(commit_end)?,
        reveal_end: ts32(reveal_end)?,
        issuance_end: ts32(issuance_end)?,
    };

    // A day nobody could price cannot hold an auction, and ends as a day with no limit does.
    // But unlike that day, it was briefed with a limit, which has to be returned.
    let unpriced = config.reference_prices.is_empty();
    let no_limit = contract.brief_has_limit.read(&emission_day)? == 0;
    let desis_limit_minor = contract.pending_desis_limit_minor.read(&emission_day)?;
    let below_one_unit = desis_limit_minor < U256::from(config.promis_load_minor);
    let late = commit_end.saturating_sub(now) < MIN_COMMIT_WINDOW_SECONDS;
    if unpriced || no_limit || below_one_unit || late {
        send_stage_start(storage, start, DAY_STATE_CANCELLED)?;
        contract.write_stage(emission_day, AuctionStage::Cancelled)?;
        if no_limit {
            contract.emit(IDesis::AuctionCancelledNoLimit {
                emissionDay: emission_day.into(),
            })?;
        } else if unpriced {
            contract.emit(IDesis::AuctionCancelledUnpriced {
                emissionDay: emission_day.into(),
            })?;
            refund_unused_desis_limit(storage, contract, emission_day)?;
        } else if below_one_unit {
            contract.emit(IDesis::AuctionCancelledBelowOneUnit {
                emissionDay: emission_day.into(),
                desisLimitMinor: desis_limit_minor,
                promisLoadMinor: config.promis_load_minor,
            })?;
            refund_unused_desis_limit(storage, contract, emission_day)?;
        } else {
            contract.emit(IDesis::AuctionCancelledLateStart {
                emissionDay: emission_day.into(),
            })?;
            refund_unused_desis_limit(storage, contract, emission_day)?;
        }
        contract.remove_sched_active(emission_day)?;
        return Ok(StartOutcome::Retired);
    }
    send_stage_start(storage, start, DAY_STATE_LIVE)?;
    contract.write_stage(emission_day, AuctionStage::CommittingBids)?;
    contract.emit(IDesis::AuctionCreated {
        emissionDay: emission_day.into(),
    })?;
    Ok(StartOutcome::Started)
}

/// Return a retiring day's unused Desis Limit to PromisLimit, recording its
/// Desis Allocation as zero. No-op once the limit was consumed at clearing (or
/// for a day with no limit, which briefs zero).
fn refund_unused_desis_limit(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    emission_day: EmissionDate,
) -> Result<()> {
    let unused = contract.pending_desis_limit_minor.read(&emission_day)?;
    if unused.is_zero() {
        return Ok(());
    }
    contract
        .pending_desis_limit_minor
        .write(&emission_day, U256::ZERO)?;
    contract.emit(IDesis::DesisAllocationRecorded {
        emissionDay: emission_day.into(),
        desisLimitMinor: unused,
        desisAllocationMinor: U256::ZERO,
    })?;
    contract.emit(IDesis::UnusedDesisLimitReported {
        emissionDay: emission_day.into(),
        unusedDesisLimitMinor: unused,
    })?;
    PromisLimitContract::new(storage.clone()).checked_credit(unused)?;
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

    // Per chain, each in its own checkpoint. A chain whose send fails takes neither the day's
    // arming nor the other chains' rounds with it. The fan-in deadline covers the one left behind.
    for chain_id in fetch_targets(storage, emission_day)? {
        let sent = storage.with_checkpoint(|| {
            let gas = clearing_round_gas(storage, emission_day, chain_id)?;
            storage.call(
                ORIGIN_ROUTER_ADDRESS,
                U256::ZERO,
                IOriginRouter::sendAuctionStageCollectingBidsCall {
                    emissionDay: emission_day.into(),
                    dstChainId: chain_id,
                    gasLimit: U256::from(gas),
                }
                .abi_encode()
                .into(),
            )?;
            Ok(())
        });
        if let Err(error) = sent {
            tracing::warn!(
                target: "outbe::desis",
                %emission_day,
                chain_id,
                error = ?error,
                "collecting-bids round: chain skipped"
            );
        }
    }
    Ok(())
}
