//! Daily call scan: force-calls qualified Nod buckets off the Oracle's
//! finalized per-UTC-day VWAPs, then forfeit-burns the Nods of a bucket whose
//! notice period lapsed. The Cycle daily trigger pins the closed UTC day and
//! runs the first slice; later CycleTicks continue the same day.
//!
//! One pass over the dense callable-bucket index applies at most one transition
//! per bucket, in lifecycle order:
//!
//! - *not called* -> *called* when the reference price exceeded the bucket's
//!   call price on at least its `call_threshold` of the trailing
//!   `call_window`.
//! - *called* -> *forfeited* when the bucket's `call_notice_period` has lapsed
//!   with Nods still unpaid. The two can never fire in one pass, since a
//!   bucket called now cannot also be a notice period past its call.
//!
//! All four terms are sealed onto the bucket at issuance and read back from it
//! here, so retuning a constant leaves every issued bucket on the terms it was
//! issued with. Gem and intex give the same guarantee.
//!
//! The breach rule needs no per-bucket streak state: the daily series is global
//! per currency, so one trailing window per currency decides every bucket
//! denominated in it, and the count is recomputed from oracle history on every
//! run rather than carried. Mirrors `outbe_gem::hooks::scan_and_call` and
//! `outbe_credisfactory::called::scan_and_call`, which evaluate the same shape.
//!
//! Qualification and calls both read finalized UTC-day VWAPs, and both stop
//! at `first_full_day` of the bucket's sealed `issued_at`, so delayed
//! materialization cannot inherit pre-issuance days and a partial issuance
//! UTC day does not count.

use alloy_primitives::{B256, U256};
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    block::BlockRuntimeContext,
    daily_sweep::{Scheduled, SweepDays},
    error::Result,
    storage::StorageHandle,
    time::{first_full_day, previous_date_key, timestamp_to_date_key},
};

use crate::{
    api,
    constants::{
        CALL_SWEEP, CALL_WINDOW, MAX_NOD_CALL_VISITS_PER_BLOCK, MAX_NOD_FORFEITS_PER_BLOCK,
        SECS_PER_DAY,
    },
    precompile::INod,
    schema::{CallTerms, NodContract},
};

/// Trailing finalized daily VWAPs of one `COEN/<iso>` pair, newest first.
/// `None` marks a day the pair published no reference price.
type VwapWindow = Vec<(u32, Option<U256>)>;

/// Schedule the day the Oracle has just finalized: open a Called sweep over it
/// and run its first slice, or queue it behind the sweep still in flight.
///
/// Never returns `Err` for missing market data: the Cycle dispatcher propagates
/// a handler error out of the `CycleTick` system transaction, which fails the
/// block, so an unregistered pair, an unpriced currency or an unfinalized day
/// each degrade to "no transition" instead.
pub fn scan_and_call(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let Some(last_closed_day) = closed_day(ctx)? else {
        return Ok(0);
    };
    let mut nod = NodContract::new(ctx.storage.clone());
    if nod.call_sweep_day.read()? == 0 && nod.callable_buckets.len()? == 0 {
        return Ok(0);
    }
    let days = SweepDays {
        current: nod.call_sweep_day.read()?,
        pending: nod.call_pending_day.read()?,
    };
    match days.schedule(last_closed_day) {
        (next, Scheduled::Opened) => {
            start_call_sweep(&nod, next)?;
            run_call_slice(ctx, scope, parent)
        }
        (next, Scheduled::Queued) => {
            nod.call_pending_day.write(next.pending)?;
            Ok(0)
        }
        (next, Scheduled::Replaced { skipped }) => {
            nod.call_pending_day.write(next.pending)?;
            nod.emit(INod::SweepDaySkipped {
                sweep: CALL_SWEEP,
                skippedDay: skipped,
                inFlightDay: next.current,
            })?;
            Ok(0)
        }
        (_, Scheduled::Ignored) => Ok(0),
    }
}

/// The most recent fully-closed UTC day, or `None` while its VWAPs are not final.
pub(crate) fn closed_day(ctx: &BlockRuntimeContext) -> Result<Option<u32>> {
    let last_closed_day = previous_date_key(timestamp_to_date_key(ctx.block.timestamp));
    let finalized = OracleContract::new(ctx.storage.clone())
        .utc_day_vwap_last_finalized
        .read()?;
    if finalized < last_closed_day {
        tracing::warn!(
            target: "outbe::nod",
            last_closed_day,
            finalized,
            "nod: utc-day VWAP not finalized yet, skipping the day's sweeps"
        );
        return Ok(None);
    }
    Ok(Some(last_closed_day))
}

/// Pin the sweep's current day and walk the callable list from the top.
fn start_call_sweep(nod: &NodContract, days: SweepDays) -> Result<()> {
    nod.call_sweep_day.write(days.current)?;
    nod.call_pending_day.write(days.pending)?;
    nod.call_scan_cursor.write(0)?;
    Ok(())
}

fn finish_call_sweep(nod: &NodContract, pinned_day: u32) -> Result<()> {
    let next = SweepDays {
        current: pinned_day,
        pending: nod.call_pending_day.read()?,
    }
    .finish();
    if next.current == 0 {
        nod.call_sweep_day.write(0)
    } else {
        start_call_sweep(nod, next)
    }
}

/// Advance an open sweep by one slice, pinned to the day it opened on so
/// later blocks decide against the same prices. Returns how many buckets
/// were called plus Nods forfeited.
pub fn run_call_slice(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<u32> {
    let mut nod = NodContract::new(ctx.storage.clone());
    let pinned_day = nod.call_sweep_day.read()?;
    if pinned_day == 0 {
        return Ok(0);
    }
    let len = nod.callable_buckets.len()?;
    if len == 0 {
        finish_call_sweep(&nod, pinned_day)?;
        return Ok(0);
    }
    let oracle = OracleContract::new(ctx.storage.clone());
    let finalized = oracle.utc_day_vwap_last_finalized.read()?;
    if finalized < pinned_day {
        tracing::warn!(
            target: "outbe::nod",
            pinned_day,
            finalized,
            "nod call scan: pinned utc-day VWAP not finalized, holding the sweep"
        );
        return Ok(0);
    }

    // Stored as `index + 1`; 0 means "start a fresh pass from the top".
    let initial_cursor = nod.call_scan_cursor.read()?;
    let mut cursor = match initial_cursor {
        0 => len - 1,
        resume => resume.saturating_sub(1).min(len - 1),
    };

    // A VWAP window belongs to one `COEN/<iso>` pair, but the callable index
    // mixes currencies. Cache the windows and keep the single pass; the registry
    // holds a handful of codes, so a linear probe beats a map.
    let mut windows: Vec<(u16, VwapWindow)> = Vec::new();

    let now = ctx.block.timestamp;
    let mut mutated: u32 = 0;
    let mut visited: u32 = 0;
    let mut forfeited: u32 = 0;
    let mut called_days = std::collections::BTreeSet::new();

    // Descending walk: removing a bucket swap-pops the tail into the hole, and
    // the tail is already behind a descending cursor, so no live entry is
    // skipped and none is visited twice.
    let completed = loop {
        if visited >= MAX_NOD_CALL_VISITS_PER_BLOCK {
            break false;
        }
        if let Some(bucket_key) = nod.callable_buckets.get(cursor)? {
            visited = visited.saturating_add(1);
            let called_at = nod.bucket_called_at.read(&bucket_key)?;
            // Paid entitlements retain their bucket terms, but cannot be called or forfeited.
            let has_unpaid = nod.bucket_nod_count.read(&bucket_key)? != 0;
            if has_unpaid && called_at == 0 {
                // Structural reads stay on `?` so infra errors still propagate.
                let terms = nod.read_call_terms(bucket_key)?;
                // Sealed at first issuance. Zero means the bucket predates the
                // stamp: skip rather than treat epoch-midnight as a full day,
                // which would count every observation. Such a bucket can be
                // deleted through the existing empty-bucket path and reissued.
                let issued_at = nod.callable_bucket_issued_at.read(&bucket_key)?;
                let index = window_for(
                    &nod,
                    &ctx.storage,
                    &oracle,
                    &mut windows,
                    terms.reference_currency,
                    pinned_day,
                )?;
                if issued_at != 0
                    && breached_enough(&windows[index].1, &terms, first_full_day(issued_at))
                {
                    // Isolate per-bucket: a deterministic Err rolls back this
                    // bucket's checkpoint and is skipped, so one bad bucket never
                    // halts the daily scan.
                    let res = ctx.storage.with_checkpoint(|| {
                        mark_called(&mut nod, bucket_key, now, terms.call_notice_period)
                    });
                    if res.is_ok() {
                        mutated = mutated.saturating_add(1);
                        called_days.insert(nod.bucket_worldwide_day.read(&bucket_key)?.value());
                    }
                }
            } else if has_unpaid
                && now > api::settlement_deadline_of(called_at, notice_period(&nod, bucket_key)?)
            {
                let budget = MAX_NOD_FORFEITS_PER_BLOCK.saturating_sub(forfeited);
                if budget > 0 {
                    let res = ctx.storage.with_checkpoint(|| {
                        forfeit_members(&ctx.storage, &mut nod, scope, parent, bucket_key, budget)
                    });
                    if let Ok(burned) = res {
                        forfeited = forfeited.saturating_add(burned);
                        mutated = mutated.saturating_add(burned);
                    }
                }
            }
        }
        if cursor == 0 {
            break true;
        }
        cursor -= 1;
    };

    nod.emit_days_metadata_update(&called_days)?;

    let next_cursor = if completed {
        0
    } else {
        cursor.saturating_add(1)
    };
    if next_cursor != initial_cursor {
        nod.call_scan_cursor.write(next_cursor)?;
    }
    if completed {
        finish_call_sweep(&nod, pinned_day)?;
    }
    Ok(mutated)
}

/// True when the bucket's trailing `call_window` carries at least its
/// `call_threshold` of days strictly above its `call_price`.
///
/// Every term comes off the bucket, not from the constants, so a retune cannot
/// re-term a bucket that is already armed. `window` is sized for the widest
/// window in the currency, so this takes only its own prefix.
///
/// Days at or below the call price, and days with no published price, both
/// simply fail to count, so the window absorbs up to `window - threshold` of
/// either. The walk stops at the first UTC day preceding `first_full_day` of
/// the bucket's sealed `issued_at`, so a delayed materialization cannot inherit
/// a breach run from before the right existed, and a partial issuance UTC day
/// does not count. The window is newest-first, so everything beyond that point
/// is older still.
fn breached_enough(window: &[(u32, Option<U256>)], terms: &CallTerms, start_day: u32) -> bool {
    let window_days = terms.call_window / SECS_PER_DAY;
    let threshold_days = terms.call_threshold / SECS_PER_DAY;
    // A bucket armed before the terms existed carries zeroes. Zero days is "no
    // terms", not "every day breaches"; leave it uncallable. Same guard as
    // `outbe_gem::runtime::trigger_call`.
    if window_days == 0 || threshold_days == 0 || threshold_days > window_days {
        return false;
    }
    let mut breaches: u32 = 0;
    for (day, vwap) in window.iter().take(window_days as usize) {
        if *day < start_day {
            break;
        }
        if vwap.is_some_and(|value| value > terms.call_price) {
            breaches += 1;
            if breaches >= threshold_days {
                return true;
            }
        }
    }
    false
}

/// The bucket's sealed notice period. Read on its own in the forfeit arm, which
/// needs no other term.
fn notice_period(nod: &NodContract<'_>, bucket_key: B256) -> Result<u32> {
    nod.callable_bucket_call_notice_period.read(&bucket_key)
}

/// Stamps the call and opens the settlement window the bucket sealed.
fn mark_called(
    nod: &mut NodContract<'_>,
    bucket_key: B256,
    now: u64,
    notice_period: u32,
) -> Result<()> {
    nod.bucket_called_at.write(&bucket_key, now)?;
    nod.emit(INod::NodBucketCalled {
        bucketKey: bucket_key,
        calledAt: now,
        settlementDeadline: api::settlement_deadline_of(now, notice_period),
    })
}

/// Forfeit-burns up to `budget` of a lapsed bucket's remaining unpaid Nods, newest
/// first. Returns how many were burned.
///
/// A bucket holding more members than the budget resumes on the next run, which
/// cannot change an outcome: the deadline has already passed and settlement is
/// closed, so nothing can rescue the remainder. Removing the last member deletes
/// the bucket body and drops it from the callable index.
///
/// Each burned load returns to the Promis Reserve. Lysis drew it out of the day
/// limit and only mining converts it into Gratis, so a load that is destroyed
/// unmined would otherwise leave the reserve with nothing minted against it.
/// The credit is one accumulated write per pass, and the caller's checkpoint
/// makes it atomic with the burns it accounts for.
fn forfeit_members(
    storage: &StorageHandle<'_>,
    nod: &mut NodContract<'_>,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    bucket_key: B256,
    budget: u32,
) -> Result<u32> {
    let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
    let mut burned: u32 = 0;
    let mut credit = U256::ZERO;
    while burned < budget {
        let count = nod.bucket_nod_count.read(&bucket_key)?;
        let Some(last) = count.checked_sub(1) else {
            break;
        };
        let nod_id = nod
            .bucket_nods
            .read(&NodContract::bucket_nod_key(bucket_key, last))?;
        if nod_id.is_zero() {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bucket {bucket_key} member slot {last} is empty during forfeit"
                )),
            );
        }
        let item = api::load_item(storage, scope, parent, nod_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "Nod bucket {bucket_key} member {nod_id} has no body during forfeit"
            ))
        })?;
        if item.body().is_settled || item.body().bucket_key != bucket_key {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bucket {bucket_key} indexes an ineligible member {nod_id}"
                )),
            );
        }
        let owner = item.body().owner;
        let gratis_load_minor = item.body().gratis_load_minor;
        let bucket_id = WwdEntityId::from_day_and_digest(worldwide_day, bucket_key.0);
        let bucket = api::load_bucket(storage, scope, parent, bucket_id)?.ok_or_else(|| {
            outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                "Nod bucket {bucket_key} has no body during forfeit"
            ))
        })?;
        api::remove_nod(storage, scope, item, bucket)?;
        nod.emit(INod::NodForfeited {
            owner,
            nodId: nod_id.to_u256(),
            gratisLoadMinor: gratis_load_minor,
        })?;
        credit = credit.checked_add(gratis_load_minor).ok_or_else(|| {
            outbe_primitives::error::PrecompileError::Revert(
                "Nod forfeit Promis Reserve credit overflow".into(),
            )
        })?;
        burned = burned.saturating_add(1);
    }
    if !credit.is_zero() {
        outbe_promislimit::PromisLimitContract::new(storage.clone())
            .add_to_total_unallocated(credit)?;
    }
    Ok(burned)
}

/// Index into `cache` of the trailing finalized-VWAP window for `COEN/<iso>`,
/// newest first, filling it on first use.
///
/// An unregistered pair caches an empty window: a bucket in that currency can
/// never register a breach, but it must still reach the forfeit arm, so this is
/// a skip of the call check rather than a skip of the bucket.
fn window_for(
    nod: &NodContract<'_>,
    storage: &StorageHandle<'_>,
    oracle: &OracleContract<'_>,
    cache: &mut Vec<(u16, VwapWindow)>,
    iso_code: u16,
    last_closed_day: u32,
) -> Result<usize> {
    if let Some(index) = cache.iter().position(|(code, _)| *code == iso_code) {
        return Ok(index);
    }
    let mut window = Vec::new();
    if let Some(pair_index) = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso_code)? {
        // Widest of the current constant and anything ever armed: a bucket keeps
        // the window it was armed with, so a narrowed constant must not shorten
        // the span the scan collects for it.
        let window_days = nod.max_call_window.read(&iso_code)?.max(CALL_WINDOW) / SECS_PER_DAY;
        window.reserve(window_days as usize);
        let mut day = last_closed_day;
        for _ in 0..window_days {
            window.push((day, oracle.get_utc_day_vwap_for_pair(day, pair_index)?));
            day = previous_date_key(day);
        }
    }
    cache.push((iso_code, window));
    Ok(cache.len() - 1)
}
