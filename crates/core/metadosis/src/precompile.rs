use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::error::{DispatchInputError, Result};

/// Selectors on this precompile that accept native value.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!(
        #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
        "../../../contracts/precompiles/src/IMetadosis.sol"
    );
}
pub use abi::IMetadosis;

/// Dispatches an ABI-encoded call to the Metadosis precompile.
///
/// `IMetadosis` exposes only events and custom errors (`getEmissionDay` lives on `IEmission` at `EMISSION_ADDRESS`).
pub fn dispatch(
    _storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    if data.len() < 4 {
        return Err(DispatchInputError::MissingFunctionSelector.into());
    }
    Err(DispatchInputError::DecodeError {
        reason: "unknown function selector for IMetadosis".into(),
    }
    .into())
}
