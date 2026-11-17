//! Explicit evidence capability; DCAP never falls back to development evidence.
use super::{InProcessError, Result};
use outbe_primitives::tee_attestation_v1::AttestationMode;

pub trait HardwareProofSourceV1: Send + Sync {
    fn code_identity(&self) -> Result<(alloy_primitives::B256, alloy_primitives::B256, u16, u16)>;
    fn quote(&self, report_data: [u8; 64]) -> Result<Vec<u8>>;
}
pub enum ProofSourceV1 {
    NoHardware,
    Hardware(std::sync::Arc<dyn HardwareProofSourceV1>),
}
impl std::fmt::Debug for ProofSourceV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProofSourceV1([REDACTED])")
    }
}
impl ProofSourceV1 {
    pub fn ensure_available(&self, mode: AttestationMode) -> Result<()> {
        if mode == AttestationMode::DcapRequired && matches!(self, Self::NoHardware) {
            return Err(InProcessError::HardwareProofUnavailable);
        }
        Ok(())
    }
    pub fn quote(&self, report_data: [u8; 64]) -> Result<Vec<u8>> {
        match self {
            Self::Hardware(source) => source.quote(report_data),
            Self::NoHardware => Err(InProcessError::HardwareProofUnavailable),
        }
    }
}
