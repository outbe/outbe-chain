use outbe_primitives::error::PrecompileError;

use crate::constants::MAX_INPUTS;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ZkProofError {
    #[error("poseidon: empty input")]
    EmptyInput,
    #[error("poseidon: input length {0} is not a multiple of 32")]
    UnalignedInput(usize),
    #[error("poseidon: {0} inputs exceeds maximum supported ({MAX_INPUTS})")]
    TooManyInputs(usize),
    #[error("poseidon: parameter setup failed: {0}")]
    SetupFailed(String),
    #[error("poseidon: hash failed: {0}")]
    HashFailed(String),
    #[error("zk_verify: input too short ({0} < 64 bytes)")]
    InputTooShort(usize),
    #[error("zk_verify: malformed ABI input ({0})")]
    MalformedAbi(&'static str),
    #[error("zk_verify: combined proof is too short ({0} < 4 bytes)")]
    CombinedProofTooShort(usize),
    #[error("zk_verify: full proof public input count is {actual}, expected {expected}")]
    WrongPublicInputCount { expected: usize, actual: usize },
    #[error("zk_verify: combined proof public inputs are truncated ({actual} < {expected} bytes)")]
    TruncatedPublicInputs { expected: usize, actual: usize },
    #[error("zk_verify: public input at index {0} is not a canonical BN254 field element")]
    NonCanonicalPublicInput(usize),
    #[error("zk_verify: malformed combined proof ({0})")]
    MalformedCombinedProof(&'static str),
}

impl From<ZkProofError> for PrecompileError {
    fn from(err: ZkProofError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
