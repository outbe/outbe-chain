//! Per-block qualification: drains floor-bins crossed by the live COEN rate and
//! qualifies Issued series past their qualification period. Runs in `begin_block`.
//! A floor compares only to its own currency's rate, so each currency walks its own
//! trie with its own cursor; they share one per-block budget.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_intex::SeriesId;
use outbe_oracle::api::{coen_rate_for_opt, get_all_reference_currencies};
use outbe_primitives::storage::types::Storable;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    storage::StorageHandle,
};

use outbe_intex::IntexState;

use crate::constants::ORIGIN_ROUTER_ADDRESS;
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;
use crate::state::UnqualifiedBinTree;

pub struct IntexLifecycle;

impl BlockLifecycle for IntexLifecycle {
    type Context<'a, 'storage> = BlockRuntimeContext<'storage>;
    type EndBlockResult = ();

    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        scan_and_qualify(ctx)?;
        // Drain in-flight payouts first, then start rounds for any series whose
        // proceeds fan-in deadline has passed.
        crate::runtime::drain_distributions(&ctx.storage)?;
        crate::runtime::sweep_proceeds_deadlines(&ctx.storage, ctx.block.timestamp)?;
        Ok(())
    }

    fn end_block(_ctx: &BlockRuntimeContext) -> Result<Self::EndBlockResult> {
        Ok(())
    }
}

/// Max series visited per begin-block qualify scan; the cursor resumes the rest next block.
pub(crate) const MAX_SERIES_PER_BLOCK: u32 = 256;

/// Number of series promoted Issued -> Qualified this block. Every reference currency is
/// scanned against its own rate, sharing one [`MAX_SERIES_PER_BLOCK`] budget; an unpriced
/// one is skipped for the block rather than halting it.
pub fn scan_and_qualify(ctx: &BlockRuntimeContext) -> Result<u32> {
    let currencies = get_all_reference_currencies(ctx)?;
    if currencies.is_empty() {
        return Ok(0);
    }
    let factory = IntexFactoryContract::new(ctx.storage.clone());
    let start = factory.qualify_currency_cursor.read()? as usize % currencies.len();

    let mut budget = MAX_SERIES_PER_BLOCK;
    let mut promoted: u32 = 0;
    let mut resume_at = start;
    for offset in 0..currencies.len() {
        let at = (start + offset) % currencies.len();
        if budget == 0 {
            // Resume here, so a heavy currency cannot starve the ones behind it.
            resume_at = at;
            break;
        }
        let Some(rate) = coen_rate_for_opt(ctx.storage.clone(), currencies[at])? else {
            continue;
        };
        let (qualified, inspected) = qualify_currency(ctx, currencies[at], rate, budget)?;
        promoted = promoted.saturating_add(qualified);
        budget = budget.saturating_sub(inspected);
    }
    factory.qualify_currency_cursor.write(resume_at as u32)?;
    Ok(promoted)
}

/// Qualifies one reference currency's series, visiting at most `budget` of them.
/// Returns `(promoted, visited)` so the caller can share one per-block budget.
fn qualify_currency(
    ctx: &BlockRuntimeContext,
    iso_code: u16,
    rate: U256,
    budget: u32,
) -> Result<(u32, u32)> {
    let now = ctx.block.timestamp;
    // Deterministic out-of-range rate: skip this currency instead of halting the block.
    let r_bin = match IntexFactoryContract::price_to_bin(rate) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "outbe::intexfactory", iso_code, error = ?e, "qualify scan: rate out of range, skipping currency");
            return Ok((0, 0));
        }
    };
    let mut factory = IntexFactoryContract::new(ctx.storage.clone());
    let qualification_period = crate::config::read(&factory)?.qualification_period;

    let mut promoted: u32 = 0;
    // Cap per-block work and resume next block from a persisted bin cursor: the scan
    // no longer scales with the active-series population. A series qualifies within one full sweep
    // (bounded lag); the resulting state is unchanged. Whole bins are processed atomically, so the
    // cursor is bin-granular (no within-bin index that removal-shifts could desync).
    let mut processed: u32 = 0;
    let mut cursor: u32 = factory.qualify_scan_cursor.read(&iso_code)?;
    loop {
        if processed >= budget {
            factory.qualify_scan_cursor.write(&iso_code, cursor)?;
            break;
        }
        let next = match tree_math::find_first_left_inclusive(
            &UnqualifiedBinTree(&factory, iso_code),
            cursor,
        )? {
            Some(b) if b <= r_bin => b,
            _ => {
                // End of the eligible range: next block starts a fresh sweep from the bottom.
                factory.qualify_scan_cursor.write(&iso_code, 0)?;
                break;
            }
        };

        // Snapshot the bin before mutating: qualify() removes on success.
        let count = factory
            .unqualified_bin_count
            .read(&IntexFactoryContract::scoped(iso_code, next))?;
        let mut series: Vec<SeriesId> = Vec::with_capacity(count as usize);
        for i in 0..count {
            series.push(SeriesId::from_word(
                factory
                    .unqualified_bin_series
                    .read(&IntexFactoryContract::bin_index_key(iso_code, next, i))?,
            ));
        }
        for series_id in series {
            // Isolate per-series: a deterministic Err rolls back this series' checkpoint and is
            // skipped (logged), so one bad series cannot halt the block. Infra errors that recur
            // every series still surface via the structural reads above, which keep `?`.
            let res = ctx.storage.with_checkpoint(|| {
                try_qualify(
                    &ctx.storage,
                    &mut factory,
                    series_id,
                    qualification_period,
                    now,
                    rate,
                )
            });
            match res {
                Ok(true) => promoted = promoted.saturating_add(1),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(target: "outbe::intexfactory", series_id = %series_id, error = ?e, "qualify scan: skipping series");
                }
            }
        }
        processed = processed.saturating_add(count);

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => {
                // Reached the top bin: wrap to a fresh sweep next block.
                factory.qualify_scan_cursor.write(&iso_code, 0)?;
                break;
            }
        };
    }
    Ok((promoted, processed))
}

/// Qualify one series if Issued, past its qualification period, and `rate` exceeds its floor.
pub(crate) fn try_qualify(
    storage: &StorageHandle<'_>,
    factory: &mut IntexFactoryContract,
    series_id: SeriesId,
    qualification_period: u32,
    now: u64,
    rate: U256,
) -> Result<bool> {
    let series = outbe_intex::api::read_series(storage, series_id)?;
    if series.lifecycle_state()? != IntexState::Issued {
        return Ok(false);
    }
    let qualifies_at = u64::from(series.issued_at).saturating_add(u64::from(qualification_period));
    if now <= qualifies_at {
        return Ok(false);
    }
    let floor = series.floor_price_minor;
    if rate <= floor {
        return Ok(false);
    }
    outbe_intex::api::mark_qualified(storage, series_id)?;
    factory.remove_unqualified(series_id, series.reference_currency, floor)?;
    factory.insert_qualified(
        series_id,
        series.reference_currency,
        series.call_price_minor,
    )?;

    // Notify the target chain of the Qualified transition via ERC-7786; best-effort.
    // OriginRouter failure (e.g. exhausted relay float) does not revert the
    // state transition. The target chain can reconcile series state from the origin chain.
    let _ = notify_qualified(storage, series_id, series.worldwide_day.value());

    crate::runtime::emit_event(
        storage,
        crate::precompile::IIntexFactory::SeriesQualified {
            seriesId: series_id.into(),
        },
    )?;
    Ok(true)
}

fn notify_qualified(
    storage: &StorageHandle<'_>,
    series_id: SeriesId,
    worldwide_day: u32,
) -> Result<()> {
    // Relay-float-funded: value 0, so the router self-quotes and pays the bridge fee from its float.
    storage.call(
        ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
        IOriginRouter::sendMarkQualifiedCall {
            seriesId: series_id.into(),
            worldwideDay: worldwide_day,
        }
        .abi_encode()
        .into(),
    )?;
    Ok(())
}
