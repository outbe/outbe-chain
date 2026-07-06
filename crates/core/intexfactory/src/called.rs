//! Daily Called scan: force-calls a Qualified series once its COEN VWAP exceeded
//! the call trigger on `threshold_days` of the last `window_days`. Candidates
//! come from the call-trigger bin index; counts are recomputed from oracle VWAP
//! history each run. Driven by the Cycle daily trigger.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_common::WorldwideDay;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::{
    block::BlockRuntimeContext,
    error::{PrecompileError, Result},
    math::{constants::MAX_BIN_ID, tree_math},
    storage::StorageHandle,
};

use outbe_intex::IntexState;

use crate::constants::{INTEX_NFT1155_ADDRESS, ORIGIN_ROUTER_ADDRESS, QUALIFIER_REFERENCE_ISO};
use crate::schema::IntexFactoryContract;
use crate::sol_ext::{IIntexNFT1155, IOriginRouter};
use crate::state::QualifiedBinTree;

/// Run the daily Called scan. Returns the number of series force-called.
pub fn scan_and_call(ctx: &BlockRuntimeContext) -> Result<u32> {
    let oracle = OracleContract::new(ctx.storage.clone());
    let pair_hash = oracle
        .settlement_iso_to_pair
        .read(&QUALIFIER_REFERENCE_ISO)?;
    if pair_hash.is_zero() {
        return Ok(0);
    }
    let pair_id = oracle.pair_hash_to_id.read(&pair_hash)?;
    if pair_id == 0 {
        return Ok(0);
    }

    // Most recent completed day (finalized VWAP).
    let today = WorldwideDay::from_timestamp(ctx.block.timestamp).previous_date_key();
    let vwap_today = match oracle.get_worldwide_day_vwap_for_pair_id(today, pair_id)? {
        Some(v) if !v.is_zero() => v,
        _ => return Ok(0),
    };

    // Deterministic out-of-range VWAP: skip this daily scan instead of halting the block.
    let v_bin = match IntexFactoryContract::price_to_bin(vwap_today) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "outbe::intexfactory", error = ?e, "call scan: vwap out of range, skipping run");
            return Ok(0);
        }
    };
    let mut factory = IntexFactoryContract::new(ctx.storage.clone());

    let mut called: u32 = 0;
    let mut cursor: u32 = 0;
    loop {
        let next = match tree_math::find_first_left_inclusive(&QualifiedBinTree(&factory), cursor)?
        {
            Some(b) if b <= v_bin => b,
            _ => break,
        };

        // Snapshot before mutating: try_call removes Called series.
        let count = factory.qualified_bin_count.read(&next)?;
        let mut series: Vec<u32> = Vec::with_capacity(count as usize);
        for i in 0..count {
            series.push(
                factory
                    .qualified_bin_series
                    .read(&IntexFactoryContract::bin_index_key(next, i))?,
            );
        }
        for series_id in series {
            // Isolate per-series: a deterministic Err rolls back this series' checkpoint and is
            // skipped (logged); structural reads above keep `?` so infra errors still propagate.
            let res = ctx.storage.with_checkpoint(|| {
                try_call(
                    &ctx.storage,
                    &mut factory,
                    &oracle,
                    series_id,
                    pair_id,
                    today,
                    ctx.block.timestamp,
                )
            });
            match res {
                Ok(true) => called = called.saturating_add(1),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(target: "outbe::intexfactory", series_id, error = ?e, "call scan: skipping series");
                }
            }
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => break,
        };
    }
    Ok(called)
}

/// Cycle daily-trigger entry: runs the Called scan, discarding the count.
pub fn run_daily(ctx: &BlockRuntimeContext) -> Result<()> {
    scan_and_call(ctx)?;
    Ok(())
}

/// Force-call one series if Qualified and its VWAP breached the call trigger on
/// at least `threshold_days` of the last `window_days` completed days.
pub(crate) fn try_call(
    storage: &StorageHandle<'_>,
    factory: &mut IntexFactoryContract,
    oracle: &OracleContract,
    series_id: u32,
    pair_id: u32,
    today: WorldwideDay,
    now_ts: u64,
) -> Result<bool> {
    let series = outbe_intex::api::read_series(storage, series_id)?;
    if series.reference_currency != QUALIFIER_REFERENCE_ISO {
        return Ok(false);
    }
    if series.lifecycle_state()? != IntexState::Qualified {
        return Ok(false);
    }
    let trigger = series.call_price_minor;
    let window = u32::from(series.call_window_days);
    let threshold = u32::from(series.call_threshold_days);
    if window == 0 || threshold == 0 {
        return Ok(false);
    }

    // Breach-days (VWAP > trigger) within the window, not before issuance.
    let issued_wwd = WorldwideDay::from_timestamp(u64::from(series.issued_at));
    let mut breaches: u32 = 0;
    let mut day = today;
    for _ in 0..window {
        if day < issued_wwd {
            break;
        }
        if let Some(v) = oracle.get_worldwide_day_vwap_for_pair_id(day, pair_id)? {
            if v > trigger {
                breaches += 1;
            }
        }
        day = day.previous_date_key();
    }
    if breaches < threshold {
        return Ok(false);
    }

    // u32 timestamp; bounded until 2106 (matches issued_at).
    let called_at = u32::try_from(now_ts)
        .map_err(|_| PrecompileError::Revert("block timestamp exceeds u32".into()))?;
    outbe_intex::api::mark_called(storage, series_id, called_at)?;
    mark_nft_called(storage, series_id)?;
    factory.remove_qualified(series_id, trigger)?;

    // Notify the target chain of the Called transition via ERC-7786; best-effort.
    // OriginRouter failure (e.g. exhausted relay float) does not revert the
    // state transition. The target chain can reconcile series state from the origin chain.
    let _ = notify_lz_called(storage, series_id);

    crate::runtime::emit_event(
        storage,
        crate::precompile::IIntexFactory::SeriesCalled {
            seriesId: series_id,
            calledAt: called_at,
        },
    )?;
    Ok(true)
}

fn notify_lz_called(storage: &StorageHandle<'_>, series_id: u32) -> Result<()> {
    // Relay-float-funded: value 0, so the messenger self-quotes and pays the bridge fee from its float.
    storage.call(
        ORIGIN_ROUTER_ADDRESS,
        U256::ZERO,
        IOriginRouter::sendMarkCalledCall {
            seriesId: series_id,
        }
        .abi_encode()
        .into(),
    )?;
    Ok(())
}

fn mark_nft_called(storage: &StorageHandle<'_>, series_id: u32) -> Result<()> {
    storage.call(
        INTEX_NFT1155_ADDRESS,
        U256::ZERO,
        IIntexNFT1155::markCalledCall {
            seriesId: series_id,
        }
        .abi_encode()
        .into(),
    )?;
    Ok(())
}
