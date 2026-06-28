//! Per-block qualification: drains floor-bins crossed by the live COEN/0xUSD
//! rate and qualifies matured (21d) Issued series. Runs in `begin_block`.

use alloy_primitives::U256;
use alloy_sol_types::SolCall;
use outbe_oracle::contract::OracleContract;
use outbe_primitives::{
    addresses::INTEX_FACTORY_ADDRESS,
    block::{BlockLifecycle, BlockRuntimeContext},
    error::{PrecompileError, Result},
    math::{constants::MAX_BIN_ID, tree_math},
    storage::StorageHandle,
};

use outbe_intex::IntexState;

use crate::constants::{INTEX_NFT1155_ADDRESS, ORIGIN_MESSENGER_ADDRESS, QUALIFIER_REFERENCE_ISO};
use crate::schema::IntexFactoryContract;
use crate::sol_ext::{IIntexNFT1155, IOriginMessenger, MessagingFee};

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
    // Deterministic out-of-range rate: skip the block's scan instead of halting it.
    let r_bin = match IntexFactoryContract::price_to_bin(rate) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "outbe::intexfactory", error = ?e, "qualify scan: rate out of range, skipping block");
            return Ok(0);
        }
    };
    let mut factory = IntexFactoryContract::new(ctx.storage.clone());
    let maturity_secs = crate::config::read(&factory)?.maturity_period_secs;

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
            // Isolate per-series: a deterministic Err rolls back this series' checkpoint and is
            // skipped (logged), so one bad series cannot halt the block. Infra errors that recur
            // every series still surface via the structural reads above, which keep `?`.
            let res = ctx.storage.with_checkpoint(|| {
                try_qualify(
                    &ctx.storage,
                    &mut factory,
                    series_id,
                    maturity_secs,
                    now,
                    rate,
                )
            });
            match res {
                Ok(true) => promoted = promoted.saturating_add(1),
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(target: "outbe::intexfactory", series_id, error = ?e, "qualify scan: skipping series");
                }
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
    maturity_secs: u64,
    now: u64,
    rate: U256,
) -> Result<bool> {
    let series = outbe_intex::api::read_series(storage, series_id)?;
    if series.reference_currency != QUALIFIER_REFERENCE_ISO {
        return Ok(false);
    }
    if series.lifecycle_state()? != IntexState::Issued {
        return Ok(false);
    }
    let mature_at = u64::from(series.issued_at).saturating_add(maturity_secs);
    if now <= mature_at {
        return Ok(false);
    }
    let floor = series.floor_price_minor;
    if rate <= floor {
        return Ok(false);
    }
    outbe_intex::api::mark_qualified(storage, series_id)?;
    mark_nft_qualified(storage, series_id)?;
    factory.remove_unqualified(series_id, floor)?;
    factory.insert_qualified(series_id, series.call_price_minor)?;

    // Notify the target chain of the Qualified transition via LayerZero; best-effort.
    // OriginMessenger failure (e.g. exhausted relay float) does not revert the
    // state transition. The target chain can reconcile series state from the origin chain.
    let _ = notify_lz_qualified(storage, series_id);

    crate::runtime::emit_event(
        storage,
        crate::precompile::IIntexFactory::SeriesQualified {
            seriesId: series_id,
        },
    )?;
    Ok(true)
}

fn notify_lz_qualified(storage: &StorageHandle<'_>, series_id: u32) -> Result<()> {
    let quote_ret = storage.staticcall(
        ORIGIN_MESSENGER_ADDRESS,
        IOriginMessenger::quoteSendMarkQualifiedCall {
            seriesId: series_id,
            extraOptions: alloy_primitives::Bytes::new(),
            payInLzToken: false,
        }
        .abi_encode()
        .into(),
    )?;
    let fee = IOriginMessenger::quoteSendMarkQualifiedCall::abi_decode_returns(&quote_ret)
        .map_err(|_| PrecompileError::Revert("quoteSendMarkQualified undecodable".into()))?;
    storage.call(
        ORIGIN_MESSENGER_ADDRESS,
        U256::ZERO,
        IOriginMessenger::sendMarkQualifiedCall {
            seriesId: series_id,
            extraOptions: alloy_primitives::Bytes::new(),
            fee: MessagingFee {
                nativeFee: fee.nativeFee,
                lzTokenFee: fee.lzTokenFee,
            },
            refundAddress: INTEX_FACTORY_ADDRESS,
        }
        .abi_encode()
        .into(),
    )?;
    Ok(())
}

fn mark_nft_qualified(storage: &StorageHandle<'_>, series_id: u32) -> Result<()> {
    storage.call(
        INTEX_NFT1155_ADDRESS,
        U256::ZERO,
        IIntexNFT1155::markQualifiedCall {
            seriesId: series_id,
        }
        .abi_encode()
        .into(),
    )?;
    Ok(())
}
