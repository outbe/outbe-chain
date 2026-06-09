use alloy_primitives::Address;
use outbe_primitives::error::PrecompileError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FidelityError {
    #[error("fidelity index {index} for {address} exceeds u32::MAX")]
    IndexOutOfRange { address: Address, index: u64 },
}

impl From<FidelityError> for PrecompileError {
    fn from(value: FidelityError) -> Self {
        PrecompileError::Revert(value.to_string())
    }
}
