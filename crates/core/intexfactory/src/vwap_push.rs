//! Finalized daily VWAPs for the target chains' registries: each closed UTC day goes out once, in order.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::addresses::ORIGIN_ROUTER_ADDRESS;
use outbe_primitives::time::{next_date_key, previous_date_key};
use outbe_primitives::{block::BlockRuntimeContext, error::Result, storage::StorageHandle};

use crate::constants::{INITIAL_BACKFILL_DAYS, MAX_VWAP_DAYS_PER_FIRING, MAX_VWAP_ROWS};
use crate::schema::IntexFactoryContract;
use crate::sol_ext::IOriginRouter;

/// Cycle-trigger entry: send the finalized days the targets have not had yet.
pub fn run(ctx: &BlockRuntimeContext) -> Result<()> {
    let storage = ctx.storage.clone();
    let factory = IntexFactoryContract::new(storage.clone());
    let finalized = OracleContract::new(storage.clone())
        .utc_day_vwap_last_finalized
        .read()?;
    if finalized == 0 {
        return Ok(());
    }
    let sent = factory.vwap_sent_day.read()?;
    let mut day = if sent == 0 {
        backfill_start(finalized)
    } else {
        next_date_key(sent)
    };
    for _ in 0..MAX_VWAP_DAYS_PER_FIRING {
        if day > finalized {
            break;
        }
        let rows = day_rows(&storage, day)?;
        if !rows.is_empty() && !send(&storage, day, rows) {
            break;
        }
        factory.vwap_sent_day.write(day)?;
        day = next_date_key(day);
    }
    Ok(())
}

/// First day a fresh sender covers: the history a series still open can read.
pub(crate) fn backfill_start(finalized: u32) -> u32 {
    (1..INITIAL_BACKFILL_DAYS).fold(finalized, |day, _| previous_date_key(day))
}

/// The day's priced reference currencies. A price past the wire type saturates: it stays above every floor.
pub(crate) fn day_rows(
    storage: &StorageHandle<'_>,
    day: u32,
) -> Result<Vec<IOriginRouter::DailyVwap>> {
    let mut rows = Vec::new();
    for iso_code in outbe_oracle::api::reference_currencies(storage.clone())? {
        let Some(index) = outbe_oracle::api::coen_pair_index_opt(storage.clone(), iso_code)? else {
            continue;
        };
        let Some(vwap) = outbe_oracle::api::get_utc_day_vwap(storage.clone(), day, index)? else {
            continue;
        };
        rows.push(IOriginRouter::DailyVwap {
            isoCode: iso_code,
            vwapMinor: vwap.saturating_to(),
        });
        if rows.len() == MAX_VWAP_ROWS {
            break;
        }
    }
    Ok(rows)
}

/// Whether the router took the day; a refusal leaves it for the next firing.
fn send(storage: &StorageHandle<'_>, day: u32, rows: Vec<IOriginRouter::DailyVwap>) -> bool {
    let call = IOriginRouter::sendDailyVwapCall { utcDay: day, rows };
    let sent = storage.with_checkpoint(|| {
        storage.call(ORIGIN_ROUTER_ADDRESS, U256::ZERO, call.abi_encode().into())?;
        Ok(())
    });
    if let Err(error) = &sent {
        tracing::warn!(target: "outbe::intexfactory", day, error = ?error, "daily vwap: router refused, retrying next firing");
    }
    sent.is_ok()
}
