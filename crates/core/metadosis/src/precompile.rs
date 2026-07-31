use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

use crate::schema::MetadosisContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IMetadosis.sol"
);

/// Dispatches an ABI-encoded call to the Metadosis precompile (view-only).
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IMetadosis::IMetadosisCalls::abi_decode, |call| {
        let metadosis = MetadosisContract::new(storage);
        use IMetadosis::IMetadosisCalls::*;
        match call {
            getWorldwideDay(c) => view(c, |c| {
                let Some(day) = metadosis.worldwide_days.get(c.wwd.into())? else {
                    return Err(outbe_primitives::storage::dsl::missing_record_err(
                        "WorldwideDay",
                    ));
                };
                Ok((
                    day.status,
                    day.day_type,
                    day.forming_start,
                    day.forming_end,
                    day.lookback_end,
                    day.offering_end,
                    day.scheduled_process_time,
                    day.previous_vwap,
                    day.current_vwap,
                )
                    .into())
            }),
            getActiveWorldwideDays(_) => metadata::<IMetadosis::getActiveWorldwideDaysCall>(|| {
                let wwds = metadosis.active_wwd.read_all()?;
                Ok(wwds.into_iter().map(u32::from).collect())
            }),
            getWorldwideDaysByStatus(c) => view(c, |c| {
                let wwds = metadosis.get_active_wwd_by_status(c.status)?;
                Ok(wwds.into_iter().map(u32::from).collect())
            }),
            getBootstrapEndTime(_) => metadata::<IMetadosis::getBootstrapEndTimeCall>(|| {
                metadosis.get_bootstrap_end_time()
            }),
            getOffchainJob(c) => view(c, |c| {
                crate::ocomp::views::get_offchain_job(metadosis.storage.clone(), c.intentId)
                    .map(Bytes::from)
            }),
            getOffchainVoteAccountability(c) => view(c, |c| {
                crate::ocomp::views::get_offchain_vote_accountability(
                    metadosis.storage.clone(),
                    c.jobId,
                )
                .map(Bytes::from)
            }),
            getActiveLysisGeneration(c) => view(c, |c| {
                crate::ocomp::views::get_active_lysis_generation(
                    metadosis.storage.clone(),
                    c.wwd.into(),
                )
                .map(Bytes::from)
            }),
            getLysisTerminalReceipt(c) => view(c, |c| {
                crate::ocomp::views::get_lysis_terminal_receipt(
                    metadosis.storage.clone(),
                    c.intentId,
                )
                .map(Bytes::from)
            }),
            submitLysisResult(_) => Err(outbe_primitives::error::PrecompileError::Fatal(
                "OCOMP result vote requires current-block execution context".into(),
            )),
        }
    })
}
