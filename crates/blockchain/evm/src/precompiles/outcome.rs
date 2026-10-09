//! Translates typed precompile outcomes at the EVM adapter seam.
use alloy_primitives::Bytes;
use revm::precompile::{PrecompileOutput, PrecompileResult};

/// Return execution failures as Revert/Halt and infrastructure failures as Fatal.
/// Error conversion is owned by the domain error, following Tempo's precompile model.
#[doc(hidden)]
pub fn map_outbe_precompile_result(
    result: outbe_primitives::error::Result<Bytes>,
    actual_gas: u64,
) -> PrecompileResult {
    map_outbe_precompile_result_with_refund(result, actual_gas, 0)
}

pub(super) fn map_outbe_precompile_result_with_refund(
    result: outbe_primitives::error::Result<Bytes>,
    actual_gas: u64,
    refund: i64,
) -> PrecompileResult {
    match result {
        Ok(bytes) => {
            let mut output = PrecompileOutput::new(actual_gas, bytes, 0);
            output.gas_refunded = refund;
            Ok(output)
        }
        Err(error) => error.into_precompile_result(actual_gas),
    }
}
