use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

mod queries;
use crate::schema::MetadosisContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

// Alloy 1.6 generates event constructors with the Solidity argument lists.
#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/IMetadosis.sol"
    );
}
pub use abi::IMetadosis;

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
            getWorldwideDay(c) => view(c, |c| queries::worldwide_day(&metadosis, c)),
            getActiveWorldwideDays(_) => metadata::<IMetadosis::getActiveWorldwideDaysCall>(|| {
                let wwds = metadosis.active_wwd.read_all()?;
                Ok(wwds.into_iter().map(u32::from).collect())
            }),
            getWorldwideDaysByStatus(c) => {
                view(c, |c| queries::worldwide_days_by_status(&metadosis, c))
            }
            getBootstrapEndTime(_) => metadata::<IMetadosis::getBootstrapEndTimeCall>(|| {
                metadosis.get_bootstrap_end_time()
            }),
            getWorldwideDayTerminalReceipt(c) => {
                view(c, |c| queries::terminal_receipt(&metadosis, c))
            }
            getCapacityForfeitureReceipt(c) => {
                view(c, |c| queries::capacity_forfeiture_receipt(&metadosis, c))
            }
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
            // Reachable from arbitrary calldata whenever the OCOMP lifecycle is
            // inactive: the EVM dispatcher routes this selector to
            // `commands::submit_verified_result_vote` only while
            // `ocomp_lifecycle_active` is true, so with the lifecycle active this
            // arm is structurally unreachable. Caller-supplied ingress must
            // revert, never `Fatal` - a `Fatal` here aborts the whole payload
            // build for a transaction any external account can submit.
            submitLysisResult(_) => Err(crate::errors::result_vote_rejection(
                crate::errors::vote_rejection_code::LIFECYCLE_INACTIVE,
            )),
        }
    })
}
