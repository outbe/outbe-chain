use outbe_primitives::error::PrecompileError;
use outbe_protocol::protocol::zkproof::ProofMarshalingError;

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
    #[error("zk_verify: combined proof length is {actual} bytes, expected {expected}")]
    WrongCombinedProofLength { expected: usize, actual: usize },
    #[error("zk_verify: emit chain ID word is not a right-aligned uint64")]
    InvalidEmitChainId,
    #[error("zk_verify: emit owner word exceeds the 160-bit address bound")]
    InvalidEmitOwnerField,
    #[error("zk_verify: emit mint limb {0} is outside its canonical range")]
    InvalidEmitMintLimb(usize),
    #[error("zk verifier CRS initialization failed: {0}")]
    CrsInitialization(String),
    #[error("zk verification backend failed: {0}")]
    VerificationBackend(String),
}

impl From<ProofMarshalingError> for ZkProofError {
    fn from(error: ProofMarshalingError) -> Self {
        match error {
            ProofMarshalingError::InputTooShort(actual) => Self::InputTooShort(actual),
            ProofMarshalingError::MalformedAbi(message) => Self::MalformedAbi(message),
            ProofMarshalingError::CombinedProofTooShort(actual) => {
                Self::CombinedProofTooShort(actual)
            }
            ProofMarshalingError::WrongPublicInputCount { expected, actual } => {
                Self::WrongPublicInputCount { expected, actual }
            }
            ProofMarshalingError::TruncatedPublicInputs { expected, actual } => {
                Self::TruncatedPublicInputs { expected, actual }
            }
            ProofMarshalingError::NonCanonicalPublicInput(index) => {
                Self::NonCanonicalPublicInput(index)
            }
            ProofMarshalingError::WrongCombinedProofLength { expected, actual } => {
                Self::WrongCombinedProofLength { expected, actual }
            }
            ProofMarshalingError::InvalidEmitChainId => Self::InvalidEmitChainId,
            ProofMarshalingError::InvalidEmitOwner => Self::InvalidEmitOwnerField,
            ProofMarshalingError::InvalidEmitMintLimb(index) => Self::InvalidEmitMintLimb(index),
        }
    }
}

impl From<ZkProofError> for PrecompileError {
    fn from(err: ZkProofError) -> Self {
        PrecompileError::Revert(err.to_string())
    }
}
