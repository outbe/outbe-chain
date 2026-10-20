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
            getBootstrapEndTime(_) => metadata::<IMetadosis::getBootstrapEndTimeCall>(|| {
                metadosis.get_bootstrap_end_time()
            }),
            getWorldwideDayTerminalReceipt(c) => {
                view(c, |c| queries::terminal_receipt(&metadosis, c))
            }
            getCapacityForfeitureReceipt(c) => {
                view(c, |c| queries::capacity_forfeiture_receipt(&metadosis, c))
            }
        }
    })
}
