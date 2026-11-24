//! Translates typed precompile outcomes at the EVM adapter seam.
use alloy_primitives::Bytes;
use evm2::evm::precompile::PrecompileOutput;
use evm2::precompiles::PrecompileResult;

/// Return execution failures as Revert/Halt and infrastructure failures as Fatal.
/// Error conversion is owned by the domain error, following Tempo's precompile model.
#[doc(hidden)]
pub fn map_outbe_precompile_result(
    result: outbe_primitives::error::Result<Bytes>,
) -> PrecompileResult {
    match result {
        Ok(bytes) => Ok(PrecompileOutput::new(bytes)),
        Err(error) => error.into_precompile_result(),
    }
}
