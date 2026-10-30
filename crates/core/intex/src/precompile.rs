//! Read-only view precompile for the Intex module.
//!
//! Writes stay Rust-to-Rust (IntexFactory). This surface only exposes reads so
//! off-chain consumers can observe the canonical series identity + lifecycle, and
//! wallets can hold the Settled units as an ERC-1155 collection. Every method is a
//! view, and the transfers revert. `reject_value` rejects any `msg.value` before a read.

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::{
    ERC1155_INTERFACE_ID, ERC1155_METADATA_URI_INTERFACE_ID, ERC165_INTERFACE_ID,
};
use outbe_primitives::error::Result;

use crate::constants::{SETTLED_TOKEN_NAME, SETTLED_TOKEN_SYMBOL};
use crate::errors::{date_step_error, IntexError};
use crate::schema::{CallStamp, IntexContract, SeriesRecord};

const SUPPORTED_INTERFACES: [[u8; 4]; 3] = [
    ERC165_INTERFACE_ID,
    ERC1155_INTERFACE_ID,
    ERC1155_METADATA_URI_INTERFACE_ID,
];

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

// Generated ABI methods follow the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/IIntex.sol"
    );
}
pub use abi::IIntex;

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IIntex::IIntexCalls::abi_decode, |call| {
        let registry = IntexContract::new(storage.clone());
        use IIntex::IIntexCalls::*;
        match call {
            getIntexSeries(c) => view(c, |c| {
                let record = load_by_id(&registry, c.intexId)?;
                let settled = registry.settled_units.read(&c.intexId)?;
                let mined = registry.mined_units.read(&c.intexId)?;
                let gem_factory = registry.gem_factory_units.read(&c.intexId)?;
                let stamp = registry.call_stamp(&record)?;
                let now = storage.now_secs()?;
                to_abi_data(&record, [settled, mined, gem_factory], stamp, now)
            }),
            seriesUnitCounts(c) => view(c, |c| {
                let series_id = load_by_id(&registry, c.intexId)?.series_id;
                let counts = crate::api::unit_counts(&storage, series_id)?;
                Ok(IIntex::UnitCounts {
                    issuedUnits: counts.issued,
                    activeUnits: counts.active,
                    settledUnits: counts.settled,
                    minedUnits: counts.mined,
                    gemFactoryUnits: counts.gem_factory,
                    forfeitedUnits: counts.forfeited,
                })
            }),
            seriesExists(c) => view(c, |c| registry.series.exists(c.intexId)),
            totalSeries(_) => metadata::<IIntex::totalSeriesCall>(|| {
                Ok(U256::from(registry.read_total_series()?))
            }),
            seriesAt(c) => view(c, |c| {
                let index =
                    u32::try_from(c.index).map_err(|_| IntexError::SeriesIndexOutOfRange)?;
                Ok(registry.read_series_id_at(index)?.into())
            }),
            supportsInterface(c) => {
                view(c, |c| Ok(SUPPORTED_INTERFACES.contains(&c.interfaceId.0)))
            }
            name(_) => metadata::<IIntex::nameCall>(|| Ok(SETTLED_TOKEN_NAME.to_string())),
            symbol(_) => metadata::<IIntex::symbolCall>(|| Ok(SETTLED_TOKEN_SYMBOL.to_string())),
            balanceOf(c) => view(c, |c| settled_balance(&registry, c.account, c.id)),
            balanceOfBatch(c) => view(c, |c| {
                if c.accounts.len() != c.ids.len() {
                    return Err(IntexError::BatchLengthMismatch.into());
                }
                c.accounts
                    .iter()
                    .zip(&c.ids)
                    .map(|(account, id)| settled_balance(&registry, *account, *id))
                    .collect()
            }),
            isApprovedForAll(c) => view(c, |_| Ok(false)),
            setApprovalForAll(_) | safeTransferFrom(_) | safeBatchTransferFrom(_) => {
                Err(IntexError::NonTransferable.into())
            }
            uri(c) => view(c, |c| {
                Ok(crate::metadata::settled_token_uri(&load_by_id(
                    &registry, c.id,
                )?))
            }),
        }
    })
}

fn load_by_id(registry: &IntexContract<'_>, intex_id: U256) -> Result<SeriesRecord> {
    registry
        .series
        .get(intex_id)?
        .ok_or_else(|| IntexError::SeriesNotFound.into())
}

/// An id no series has holds nothing.
fn settled_balance(registry: &IntexContract<'_>, account: Address, id: U256) -> Result<U256> {
    let units = registry
        .owner_settled_units
        .read(&IntexContract::owner_units_key(id, account))?;
    Ok(U256::from(units))
}

fn to_abi_data(
    r: &SeriesRecord,
    [settled, mined, gem_factory]: [u32; 3],
    stamp: CallStamp,
    now: u64,
) -> Result<IIntex::SeriesData> {
    Ok(IIntex::SeriesData {
        intexId: r.intex_id,
        seriesId: r.series_id.into(),
        promisLoadMinor: r.promis_load_minor,
        entryPriceMinor: r.entry_price_minor,
        issuedUnits: r.issued_units,
        callWindow: r.call_window_seconds,
        callThreshold: r.call_threshold_seconds,
        callPriceMinor: r.call_price_minor,
        state: stamp.state(now) as u8,
        issuedAt: r.issued_at,
        calledAt: stamp.called_at,
        callNoticePeriod: r.call_notice_period_seconds,
        issuanceCurrency: r.issuance_currency,
        referenceCurrency: r.reference_currency,
        worldwideDay: r.worldwide_day.into(),
        settledUnits: settled,
        minedUnits: mined,
        gemFactoryUnits: gem_factory,
        settlementDeadline: stamp.deadline,
        groupKey: r.group_key().map_err(date_step_error)?,
    })
}
