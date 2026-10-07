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

/// Walk one day's schedule: start at the anchor, flip to Revealing at commit
/// end, arm the clearing gate at reveal end, retire overdue and terminal days.
fn advance_day(storage: &StorageHandle<'_>, worldwide_day: WorldwideDay, now: u64) -> Result<()> {
    loop {
        let mut contract = storage.contract::<DesisContract>();
        let stage = contract.read_stage(worldwide_day)?;
        let stored_anchor = u64::from(contract.auction_at.read(&worldwide_day)?);
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
        let commit_end = anchor.saturating_add(COMMIT_WINDOW_SECONDS);
        let reveal_end = commit_end.saturating_add(u64::from(REVEAL_WINDOW_SECONDS));
        let issuance_end = reveal_end.saturating_add(SETTLEMENT_WINDOW_SECONDS);
        let windows = Windows {
            commit_end,
            reveal_end,
            issuance_end,
        };
        match stage {
            AuctionStage::Cleared | AuctionStage::Cancelled => {
                return contract.remove_sched_active(worldwide_day);
            }
            _ if now >= issuance_end => {
                // A day that never started is sent as a late start while the router accepts it.
                let started_late = stage == AuctionStage::Briefed
                    && storage
                        .with_checkpoint(|| {
                            start_auction(storage, &mut contract, worldwide_day, windows, now)
                        })
                        .is_ok();
                return if started_late {
                    Ok(())
                } else {
                    retire_overdue(storage, &mut contract, worldwide_day)
                };
            }
            AuctionStage::Briefed if now >= anchor => {
                // The chain has to run the windows the START message announces.
                #[cfg(feature = "e2e-test")]
                contract.auction_at.write(&worldwide_day, ts32(anchor)?)?;
                if let StartOutcome::Retired =
                    start_auction(storage, &mut contract, worldwide_day, windows, now)?
                {
                    return Ok(());
                }
            }
            AuctionStage::Started if now >= commit_end => {
                contract.write_stage(worldwide_day, AuctionStage::Revealing)?;
            }
            AuctionStage::Revealing if now >= reveal_end => {
                return arm_clearing(storage, worldwide_day, now);
            }
            _ => return Ok(()),
        }
    }
}

fn retire_overdue(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    contract.emit(IDesis::AuctionOverdue {
        worldwideDay: worldwide_day.into(),
    })?;
    refund_unused_desis_limit(storage, contract, worldwide_day)?;
    contract.remove_gate_active(worldwide_day)?;
    contract.write_stage(worldwide_day, AuctionStage::Cancelled)?;
    contract.remove_sched_active(worldwide_day)
}

/// u32 wire timestamp (bounded until 2106).
fn ts32(ts: u64) -> Result<u32> {
    u32::try_from(ts).map_err(|_| PrecompileError::Revert("schedule timestamp exceeds u32".into()))
}

/// The ends of a day's commit, reveal and issuance windows.
#[derive(Clone, Copy)]
struct Windows {
    commit_end: u64,
    reveal_end: u64,
    issuance_end: u64,
}

enum StartOutcome {
    /// Auction started. The schedule loop continues from `Started`.
    Started,
    /// Day was cancelled and retired. The schedule loop stops.
    Retired,
}

/// Dispatch the START message for a briefed day: a red, unpriced, sub-unit or late day is
/// born cancelled, otherwise it starts green.
fn start_auction(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    worldwide_day: WorldwideDay,
    windows: Windows,
    now: u64,
) -> Result<StartOutcome> {
    let Windows {
        commit_end,
        reveal_end,
        issuance_end,
    } = windows;
    // The day before this start, not before the brief.
    let utc_day = previous_date_key(timestamp_to_date_key(now));
    let rows: Vec<ReferenceCurrencyPrice> =
        outbe_oracle::api::priced_reference_currencies(storage.clone(), utc_day)?
            .into_iter()
            .map(|(iso_code, entry_price_minor)| ReferenceCurrencyPrice {
                iso_code,
                entry_price_minor,
            })
            .collect();
    let promis_load_minor = step_promis_load(contract, worldwide_day, &rows)?;
    let rows = choose_reference_prices(contract, worldwide_day, rows)?;
    let mut config = AuctionConfig::from_reference_prices(rows, promis_load_minor);
    let iparams = fold_profile(storage, contract, &mut config)?;
    contract.write_auction_config(worldwide_day, &config)?;
    let start = StageStart {
        worldwide_day,
        config: &config,
        iparams: &iparams,
        commit_end: ts32(commit_end)?,
        reveal_end: ts32(reveal_end)?,
        issuance_end: ts32(issuance_end)?,
    };

    // A day nobody could price cannot hold an auction, and ends as a red day does. But
    // unlike a red day, it was briefed with a limit, which has to be returned.
    let unpriced = config.reference_prices.is_empty();
    let red = contract.brief_green.read(&worldwide_day)? == 0;
    let desis_limit_minor = contract.pending_desis_limit_minor.read(&worldwide_day)?;
    let below_one_unit = desis_limit_minor < U256::from(config.promis_load_minor);
    let late = commit_end.saturating_sub(now) < MIN_COMMIT_WINDOW_SECONDS;
    if unpriced || red || below_one_unit || late {
        send_stage_start(storage, start, DAY_STATE_RED)?;
        contract.write_stage(worldwide_day, AuctionStage::Cancelled)?;
        if unpriced {
            contract.emit(IDesis::AuctionCancelledUnpriced {
                worldwideDay: worldwide_day.into(),
            })?;
            refund_unused_desis_limit(storage, contract, worldwide_day)?;
        } else if red {
            contract.emit(IDesis::AuctionCancelledRedDay {
                worldwideDay: worldwide_day.into(),
            })?;
        } else if below_one_unit {
            contract.emit(IDesis::AuctionCancelledBelowOneUnit {
                worldwideDay: worldwide_day.into(),
                desisLimitMinor: desis_limit_minor,
                promisLoadMinor: config.promis_load_minor,
            })?;
            refund_unused_desis_limit(storage, contract, worldwide_day)?;
        } else {
            contract.emit(IDesis::AuctionCancelledLateStart {
                worldwideDay: worldwide_day.into(),
            })?;
            refund_unused_desis_limit(storage, contract, worldwide_day)?;
        }
        contract.remove_sched_active(worldwide_day)?;
        return Ok(StartOutcome::Retired);
    }
    send_stage_start(storage, start, DAY_STATE_GREEN)?;
    contract.write_stage(worldwide_day, AuctionStage::Started)?;
    contract.emit(IDesis::AuctionCreated {
        worldwideDay: worldwide_day.into(),
    })?;
    Ok(StartOutcome::Started)
}

/// Return a retiring day's unused Desis Limit to PromisLimit, recording its
/// Desis Allocation as zero. No-op once the limit was consumed at clearing (or
/// for a red day, which briefs zero).
fn refund_unused_desis_limit(
    storage: &StorageHandle<'_>,
    contract: &mut DesisContract<'_>,
    worldwide_day: WorldwideDay,
) -> Result<()> {
    let unused = contract.pending_desis_limit_minor.read(&worldwide_day)?;
    if unused.is_zero() {
        return Ok(());
    }
    contract
        .pending_desis_limit_minor
        .write(&worldwide_day, U256::ZERO)?;
    contract.emit(IDesis::DesisAllocationRecorded {
        worldwideDay: worldwide_day.into(),
        desisLimitMinor: unused,
        desisAllocationMinor: U256::ZERO,
    })?;
    contract.emit(IDesis::UnusedDesisLimitReported {
        worldwideDay: worldwide_day.into(),
        unusedDesisLimitMinor: unused,
    })?;
    PromisLimitContract::new(storage.clone()).add_to_total_unallocated(unused)?;
    Ok(())
}

/// Arm the clearing from the briefed Desis Limit: convert raw PROMIS to whole Intex
/// units, start the fan-in gate and broadcast the clearing stage.
fn arm_clearing(storage: &StorageHandle<'_>, worldwide_day: WorldwideDay, now: u64) -> Result<()> {
    let mut contract = storage.contract::<DesisContract>();
    let config = contract.read_auction_config(worldwide_day)?;
    if config.promis_load_minor == 0 {
        return Err(DesisError::InvalidWorldwideDay(worldwide_day).into());
    }
    let desis_limit_minor =
        u128::try_from(contract.pending_desis_limit_minor.read(&worldwide_day)?)
            .map_err(|_| DesisError::InvalidWorldwideDay(worldwide_day))?;
    let desis_limit_units =
        (desis_limit_minor / config.promis_load_minor).min(u128::from(u32::MAX)) as u32;

    contract.clearing_initiated.write(&worldwide_day, 1u8)?;
    contract
        .pending_desis_limit_units
        .write(&worldwide_day, desis_limit_units)?;
    contract
        .clearing_deadline
        .write(&worldwide_day, now.saturating_add(BIDS_FANIN_TIMEOUT_SECS))?;
    contract.push_gate_active(worldwide_day)?;
    contract.write_stage(worldwide_day, AuctionStage::Clearing)?;

    // Per chain, each in its own checkpoint. A chain whose send fails takes neither the day's
    // arming nor the other chains' rounds with it. The fan-in deadline covers the one left behind.
    for chain_id in fetch_targets(storage, worldwide_day)? {
        let sent = storage.with_checkpoint(|| {
            let gas = clearing_round_gas(storage, worldwide_day, chain_id)?;
            storage.call(
                ORIGIN_ROUTER_ADDRESS,
                U256::ZERO,
                IOriginRouter::sendAuctionStageClearingCall {
                    worldwideDay: worldwide_day.into(),
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
                %worldwide_day,
                chain_id,
                error = ?error,
                "clearing round: chain skipped"
            );
        }
    }
    Ok(())
}
