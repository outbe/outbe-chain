//! Per-block qualification: drains floor-bins crossed by the live COEN/0xUSD
//! rate and qualifies matured (21d) Issued series. Runs in `begin_block`.

use alloy_primitives::U256;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::{
    block::{BlockLifecycle, BlockRuntimeContext},
    error::Result,
    math::{constants::MAX_BIN_ID, tree_math},
    storage::StorageHandle,
};

use outbe_intexregistry::IntexState;

use crate::constants::{MATURITY_PERIOD_SECONDS, QUALIFIER_REFERENCE_ISO};
use crate::schema::IntexFactoryContract;

pub struct IntexLifecycle;

impl BlockLifecycle for IntexLifecycle {
    fn begin_block(ctx: &BlockRuntimeContext) -> Result<()> {
        scan_and_qualify(ctx)?;
        Ok(())
    }
}

/// Returns the number of series promoted Issued -> Qualified this block.
pub fn scan_and_qualify(ctx: &BlockRuntimeContext) -> Result<u32> {
    let oracle = OracleContract::new(ctx.storage.clone());
    let pair_hash = oracle
        .settlement_iso_to_pair
        .read(&QUALIFIER_REFERENCE_ISO)?;
    if pair_hash.is_zero() {
        return Ok(0);
    }
    let rate = oracle.exchange_rate.read(&pair_hash)?;
    if rate.is_zero() {
        return Ok(0);
    }

    let now = ctx.block.timestamp;
    let r_bin = IntexFactoryContract::price_to_bin(rate)?;
    let mut factory = IntexFactoryContract::new(ctx.storage.clone());

    let mut promoted: u32 = 0;
    let mut cursor: u32 = 0;
    loop {
        let next = match tree_math::find_first_left_inclusive(&factory, cursor)? {
            Some(b) if b <= r_bin => b,
            _ => break,
        };

        // Snapshot the bin before mutating: qualify() removes on success.
        let count = factory.unqualified_bin_count.read(&next)?;
        let mut series: Vec<u32> = Vec::with_capacity(count as usize);
        for i in 0..count {
            series.push(
                factory
                    .unqualified_bin_series
                    .read(&IntexFactoryContract::bin_index_key(next, i))?,
            );
        }
        for series_id in series {
            if try_qualify(&ctx.storage, &mut factory, series_id, now, rate)? {
                promoted = promoted.saturating_add(1);
            }
        }

        cursor = match next.checked_add(1) {
            Some(c) if c <= MAX_BIN_ID => c,
            _ => break,
        };
    }
    Ok(promoted)
}

/// Qualify one series if Issued, matured (>21d), and `rate` exceeds its floor.
pub(crate) fn try_qualify(
    storage: &StorageHandle<'_>,
    factory: &mut IntexFactoryContract,
    series_id: u32,
    now: u64,
    rate: U256,
) -> Result<bool> {
    let series = outbe_intexregistry::api::read_series(storage, series_id)?;
    if series.lifecycle_state()? != IntexState::Issued {
        return Ok(false);
    }
    let mature_at = u64::from(series.issued_at).saturating_add(MATURITY_PERIOD_SECONDS);
    if now <= mature_at {
        return Ok(false);
    }
    let floor = series.coen_price_floor;
    if rate <= floor {
        return Ok(false);
    }
    outbe_intexregistry::api::mark_qualified(storage, series_id)?;
    factory.remove_unqualified(series_id, floor)?;
    // Enroll into the call-trigger index for the daily Called scan.
    factory.insert_qualified(series_id, series.coen_price_call_trigger)?;
    crate::runtime::emit_event(
        storage,
        crate::precompile::IIntexFactory::SeriesQualified {
            seriesId: series_id,
        },
    )?;
    Ok(true)
}
