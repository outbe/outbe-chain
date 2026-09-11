//! Daily Nod qualification, call and forfeit hook.
//!
//! Each daily run reads each reference currency's COEN VWAP for the previous
//! completed UTC day after the oracle has finalized it and
//! promotes any unqualified bucket whose `floor_price_minor < rate`. The
//! comparison is strict - a bucket priced exactly at the rate stays
//! unqualified until the rate moves strictly above its floor.
//! Qualification is a monotonic latch - once a bucket is qualified it stays
//! that way, so `mine_gratis` only has to read the cached `is_qualified` bit.
//!
//! Implementation (PancakeSwap-Liquidity-Book bin index):
//! - `floor_price_minor` is mapped to a 24-bit `bin_id` on a log-spaced
//!   ladder (`BIN_STEP_BP = 25` => 0.25% per bin) via `state::price_to_bin`.
//! - Unqualified buckets are stored in `unqualified_bin_count` /
//!   `unqualified_bin_buckets`, and a 3-level radix-256 bitmap trie
//!   (`bin_tree_root`/`bin_tree_mid`/`bin_tree_leaf`) marks non-empty bins.
//! - Each run: walk set bins in ascending `bin_id` order via
//!   `bin_tree::find_first_left_inclusive`. Bins strictly below `r_bin` hold
//!   only floors `< rate` (any floor equal to the rate maps into `r_bin`), so
//!   they drain wholesale; the tail bin (`bin_id == r_bin`) checks each
//!   bucket's exact `floor_price_minor < rate` so a coarse bin neither
//!   qualifies a bucket above the rate nor one priced exactly at it.
//!
//! Multi-currency: a floor is only comparable to the rate of its own
//! `reference_currency`, so every bin column is namespaced by ISO code and
//! each reference currency walks an independent trie. The daily run reads the
//! oracle's whole reference-currency registry in order, prices each one, and
//! shares a single `MAX_BUCKET_QUALIFICATIONS_PER_RUN` budget across them;
//! each currency resumes from its own per-bin cursor next daily run. A currency
//! whose COEN pair is unregistered or has no VWAP for that day is skipped.
//! Qualification waits for finalization and never falls back to a live rate,
//! an older day, or a WorldwideDay VWAP.

use alloy_primitives::U256;
use outbe_compressed_entities::{ExecutionScope, ParentBodySource, WwdEntityId};
use outbe_oracle::{
    api::{coen_pair_index_opt, get_all_reference_currencies, get_utc_day_vwap},
    schema::OracleContract,
};
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    time::{previous_date_key, timestamp_to_date_key},
};

use crate::{
    api, constants::MAX_BUCKET_QUALIFICATIONS_PER_RUN, schema::NodContract, state::CurrencyBins,
};

/// Cycle daily-trigger entry. Qualification arms buckets before the call scan.
/// The Cycle dispatcher owns scheduling and the checkpoint for both scans.
pub fn run_daily(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    qualify_nods(ctx, scope, parent)?;
    crate::called::scan_and_call(ctx, scope, parent)?;
    Ok(())
}

/// Qualifies Nod buckets using the same block scope and parent source as transactions.
///
/// Reads every reference currency the oracle knows about and qualifies each
/// one's buckets against its own previous completed UTC-day VWAP. Waits for
/// Oracle finalization and skips currencies with no registered pair or daily
/// price. An uninitialized registry does no work.
pub fn qualify_nods(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
) -> Result<()> {
    let previous_day = previous_date_key(timestamp_to_date_key(ctx.block.timestamp));
    let oracle = OracleContract::new(ctx.storage.clone());
    if oracle.utc_day_vwap_last_finalized.read()? < previous_day {
        return Ok(());
    }
    let nod = NodContract::new(ctx.storage.clone());
    let mut budget = MAX_BUCKET_QUALIFICATIONS_PER_RUN;
    for iso_code in get_all_reference_currencies(ctx)? {
        if budget == 0 {
            break;
        }
        if nod.bin_tree_root.read(&iso_code)?.is_zero() {
            continue;
        }
        let Some(index) = coen_pair_index_opt(ctx.storage.clone(), iso_code)? else {
            continue;
        };
        let Some(rate) = get_utc_day_vwap(ctx.storage.clone(), previous_day, index)? else {
            continue;
        };
        let inspected = qualify_buckets_with_rate(ctx, scope, parent, iso_code, rate, budget)?;
        budget = budget.saturating_sub(inspected);
    }
    Ok(())
}

/// Qualifies one reference currency's buckets, inspecting at most `budget`
/// bucket bodies. Returns how many it inspected so the caller can share one
/// per-run budget across currencies.
///
/// Used by the daily qualifier and behavioral tests.
pub fn qualify_buckets_with_rate(
    ctx: &BlockRuntimeContext,
    scope: &ExecutionScope,
    parent: &impl ParentBodySource,
    iso_code: u16,
    rate: U256,
    budget: u32,
) -> Result<u32> {
    if budget == 0 {
        return Ok(0);
    }
    let r_bin = NodContract::price_to_bin(rate)?;
    let mut nod = NodContract::new(ctx.storage.clone());
    let mut bin_cursor = 0_u32;
    let mut inspected = 0_u32;
    loop {
        if inspected == budget {
            break;
        }
        let next = match tree_math::find_first_left_inclusive(
            &CurrencyBins(&nod, iso_code),
            bin_cursor,
        )? {
            Some(bin) if bin <= r_bin => bin,
            _ => break,
        };
        let scoped = NodContract::scoped(iso_code, next);
        let strict = next < r_bin;
        let initial_count = nod.unqualified_bin_count.read(&scoped)?;
        let mut count = initial_count;
        if count == 0 {
            return Err(
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bin tree references empty bin {iso_code}:{next}"
                )),
            );
        }
        let initial_cursor = nod.unqualified_bin_scan_cursor.read(&scoped)?;
        let mut index = initial_cursor;
        if index >= count {
            index = 0;
        }
        while index < count && inspected < budget {
            let key = NodContract::bin_index_key(iso_code, next, index);
            let bucket_key = nod.unqualified_bin_buckets.read(&key)?;
            if bucket_key.is_zero() {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod unqualified-bin entry {iso_code}:{next}:{index} has an empty bucket key"
                    )),
                );
            }
            let worldwide_day = nod.bucket_worldwide_day.read(&bucket_key)?;
            let bucket_id = WwdEntityId::from_day_and_digest(worldwide_day, bucket_key.0);
            let loaded =
                api::load_bucket(&ctx.storage, scope, parent, bucket_id)?.ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod unqualified-bin entry {iso_code}:{next}:{index} references missing bucket {bucket_id}"
                ))
                })?;
            let bucket = loaded.body();
            if bucket.is_qualified
                || bucket.reference_currency != iso_code
                || NodContract::price_to_bin(bucket.floor_price_minor)? != next
            {
                return Err(
                    outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                        "Nod unqualified-bin entry {iso_code}:{next}:{index} mismatches bucket {bucket_key}"
                    )),
                );
            }
            if !strict && bucket.floor_price_minor >= rate {
                index += 1;
                inspected += 1;
                continue;
            }
            nod.qualify_bucket_loaded(scope, loaded)?;
            let last = count.checked_sub(1).ok_or_else(|| {
                outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                    "Nod bin {iso_code}:{next} count underflow"
                ))
            })?;
            if index != last {
                let replacement = nod
                    .unqualified_bin_buckets
                    .read(&NodContract::bin_index_key(iso_code, next, last))?;
                if replacement.is_zero() {
                    return Err(
                        outbe_primitives::error::PrecompileError::BodyReadCorruption(format!(
                            "Nod unqualified-bin entry {iso_code}:{next}:{last} has an empty bucket key"
                        )),
                    );
                }
                nod.unqualified_bin_buckets.write(&key, replacement)?;
            }
            nod.unqualified_bin_buckets.write(
                &NodContract::bin_index_key(iso_code, next, last),
                alloy_primitives::B256::ZERO,
            )?;
            count = last;
            inspected += 1;
        }

        if count != initial_count {
            nod.unqualified_bin_count.write(&scoped, count)?;
        }
        let next_cursor = if index >= count { 0 } else { index };
        if next_cursor != initial_cursor {
            nod.unqualified_bin_scan_cursor
                .write(&scoped, next_cursor)?;
        }
        if count == 0 {
            tree_math::remove(&CurrencyBins(&nod, iso_code), next)?;
        } else if index < count {
            break;
        }

        bin_cursor = match next.checked_add(1) {
            Some(next) if next <= MAX_BIN_ID => next,
            _ => break,
        };
    }
    Ok(inspected)
}
