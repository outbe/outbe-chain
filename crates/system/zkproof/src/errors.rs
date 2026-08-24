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
    #[error("zk_verify: combined proof public input count is {actual}, expected {expected}")]
    WrongPublicInputCount { expected: usize, actual: usize },
    #[error("zk_verify: combined proof public inputs are truncated ({actual} < {expected} bytes)")]
    TruncatedPublicInputs { expected: usize, actual: usize },
    #[error("zk_verify: public input at index {0} is not a canonical BN254 field element")]
    NonCanonicalPublicInput(usize),
    #[error("zk_verify: full combined proof length is {actual} bytes, expected {expected}")]
    WrongCombinedProofLength { expected: usize, actual: usize },
    #[error("zk_verify: combined proof is too large ({actual} bytes, maximum {maximum})")]
    CombinedProofTooLarge { maximum: usize, actual: usize },
    #[error("zk_verify: combined proof has an empty proof section")]
    EmptyProofSection,
    #[error("zk_verify: combined proof section is {0} bytes, not a multiple of 32")]
    UnalignedProofSection(usize),
    #[error("zk_verify: emit owner word at index {0} is not a single padded byte")]
    InvalidEmitOwnerByte(usize),
    #[error("zk_verify: emit mint units word is not a right-aligned uint64")]
    InvalidEmitMintUnits,
    #[error("zk verifier CRS initialization failed: {0}")]
    CrsInitialization(String),
    #[error("zk verification backend failed: {0}")]
    VerificationBackend(String),
}

impl From<ZkProofError> for PrecompileError {
    fn from(err: ZkProofError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
