//! Resident upgrade export and bounded finalized authorization proofs.
use crate::{
    dcap_protocol::DcapOnboardingContextV1,
    errors::{TransportError, UpgradeTransferError},
    finalized_admission::*,
};
use alloy_primitives::Bytes;
use serde::{Deserialize, Serialize};

pub const MAX_UPGRADE_PROOF_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_UPGRADE_COMMITTEES: usize = 4096;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpgradeKeyProofV1 {
    pub anchor_outcome: Bytes,
    pub committee_transitions: Vec<Bytes>,
    pub admission: Bytes,
}
impl UpgradeKeyProofV1 {
    pub fn validate(&self) -> Result<(), TransportError> {
        if !self.has_valid_dimensions() {
            return Err(UpgradeTransferError::DimensionsExceedLimits.into());
        }
        let mut total = self.anchor_outcome.len() + self.admission.len();
        for item in &self.committee_transitions {
            if item.is_empty() || item.len() > MAX_COMMITTEE_TRANSITION_RECORD_BYTES {
                return Err(UpgradeTransferError::InvalidCommitteeRecordSize.into());
            }
            total = total
                .checked_add(item.len())
                .ok_or(UpgradeTransferError::ProofSizeOverflow)?;
        }
        if total > MAX_UPGRADE_PROOF_BYTES {
            return Err(UpgradeTransferError::ExceedsAggregateLimit.into());
        }
        Ok(())
    }

    fn has_valid_dimensions(&self) -> bool {
        let anchor_valid = !self.anchor_outcome.is_empty()
            && self.anchor_outcome.len() <= MAX_COMMITTEE_OUTCOME_BYTES;
        let transition_count_valid = self.committee_transitions.len() <= MAX_UPGRADE_COMMITTEES;
        let admission_valid = !self.admission.is_empty()
            && self.admission.len() <= MAX_FINALIZED_ADMISSION_RECORD_BYTES;
        anchor_valid && transition_count_valid && admission_valid
    }
}

pub fn export_from_network_source(
    context: DcapOnboardingContextV1,
    proof: &UpgradeKeyProofV1,
    legacy_direct_dev: bool,
) -> Result<Vec<u8>, TransportError> {
    proof.validate()?;
    let runtime = crate::in_process_tee_context()
        .ok_or(crate::InProcessDispatchError::ContextNotInstalled)?;
    runtime
        .export_from_network_source_v1(context, proof, legacy_direct_dev)
        .map_err(crate::InProcessDispatchError::from)?
        .encode_canonical()
        .map_err(|code| {
            crate::InProcessDispatchError::Resident(crate::in_process::InProcessError::Dcap(code))
                .into()
        })
}
