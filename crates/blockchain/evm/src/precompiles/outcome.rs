//! Translates typed precompile outcomes at the EVM adapter seam.
use alloy_primitives::Bytes;
use alloy_sol_types::{Revert, SolError};
use revm::precompile::{PrecompileHalt, PrecompileOutput, PrecompileResult};

/// ABI-encode a revert reason as the Solidity-standard `Error(string)`
/// (selector `0x08c379a0` followed by `abi.encode(reason)`).
pub(super) fn encode_revert_reason(msg: String) -> Bytes {
    Bytes::from(Revert::from(msg).abi_encode())
}

/// Translate the outbe-level [`outbe_primitives::error::PrecompileError`] (the
/// flat error type returned from every outbe precompile dispatch function)
/// into a revm [`PrecompileResult`] that the EVM interpreter understands.
///
/// `actual_gas` is the total gas charge attributed to this precompile call
/// (`PRECOMPILE_BASE_GAS` plus any storage-op gas). It is reported on
/// success and `Revert*` paths so the interpreter charges the caller
/// correctly; `Halt(OOG)` reports zero gas because revm treats OOG halts
/// as "consume everything" via `spend_all` in
/// `revm-handler::precompile_output_to_interpreter_result`.
///
/// The mapping is exhaustive over `PrecompileError`'s declared variants;
/// the trailing wildcard arm exists only to satisfy `#[non_exhaustive]`
/// from outbe-primitives and surfaces unknown variants as `Fatal` rather
/// than panicking. The `SubCall(_)` arm remains fatal until the adapter has
/// a protocol mapping that distinguishes child-frame halts from contract
/// reverts without changing consensus behavior.
#[doc(hidden)]
pub fn map_outbe_precompile_result(
    result: outbe_primitives::error::Result<Bytes>,
    actual_gas: u64,
) -> PrecompileResult {
    match result {
        Ok(bytes) => Ok(PrecompileOutput::new(actual_gas, bytes, 0)),
        Err(outbe_primitives::error::PrecompileError::OutOfGas) => {
            Ok(PrecompileOutput::halt(PrecompileHalt::OutOfGas, 0))
        }
        Err(outbe_primitives::error::PrecompileError::Revert(msg)) => Ok(PrecompileOutput::revert(
            actual_gas,
            encode_revert_reason(msg),
            0,
        )),
        Err(error @ outbe_primitives::error::PrecompileError::BodyReadCorruption(_)) => Ok(
            PrecompileOutput::revert(actual_gas, encode_revert_reason(error.to_string()), 0),
        ),
        Err(outbe_primitives::error::PrecompileError::RevertBytes(bytes)) => {
            Ok(PrecompileOutput::revert(actual_gas, bytes, 0))
        }
        Err(outbe_primitives::error::PrecompileError::WriteProtection) => Ok(
            PrecompileOutput::halt(PrecompileHalt::other("state change during static call"), 0),
        ),
        Err(outbe_primitives::error::PrecompileError::SubCall(err)) => Err(
            revm::precompile::PrecompileError::Fatal(format!("sub-call error: {err:?}")),
        ),
        Err(outbe_primitives::error::PrecompileError::Unsupported) => Err(
            revm::precompile::PrecompileError::Fatal("precompile reported Unsupported".to_string()),
        ),
        Err(e) => Err(revm::precompile::PrecompileError::Fatal(e.to_string())),
    }
}
