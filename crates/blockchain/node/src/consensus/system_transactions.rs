//! Stateless system-transaction header and body bindings, in rejection order.
use super::{consensus_other, ConsensusError, OutbeBlockBody, OutbeHeader, REWARDS_ADDRESS};
use alloy_consensus::BlockHeader as _;
use outbe_primitives::system_tx::binding::{self, ParentAccountingBindingError};
use outbe_primitives::{reshare_artifact::OutbeBlockArtifacts, system_tx::SystemTxLayout};

pub(super) fn validate_beneficiary(header: &OutbeHeader) -> Result<(), ConsensusError> {
    let regime = outbe_primitives::block::BlockHeightRegime::for_block(header.number());
    if !regime.is_genesis() && header.beneficiary() != REWARDS_ADDRESS {
        return Err(
            crate::errors::ConsensusPolicyError::NonGenesisBeneficiaryMismatch {
                expected: REWARDS_ADDRESS,
                actual: header.beneficiary(),
            }
            .into(),
        );
    }

    Ok(())
}

pub(super) fn validate_layout<'a>(
    body: &'a OutbeBlockBody,
    header: &OutbeHeader,
) -> Result<(SystemTxLayout<'a>, OutbeBlockArtifacts), ConsensusError> {
    binding::validate_system_layout(body, header).map_err(crate::errors::consensus_display_error)
}

pub(super) fn validate_parent_accounting(
    layout: &SystemTxLayout<'_>,
    header: &OutbeHeader,
) -> Result<(), ConsensusError> {
    binding::validate_parent_accounting_binding(layout, header).map_err(|error| {
        consensus_other(match error {
            ParentAccountingBindingError::Missing { block_number } => {
                format!("missing CertifiedParentAccounting system tx for block {block_number}")
            }
            error => error.to_string(),
        })
    })
}

pub(super) fn validate_boundary_outcome(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), ConsensusError> {
    binding::validate_boundary_outcome_binding(layout, artifacts)
        .map_err(crate::errors::consensus_display_error)
}

pub(super) fn validate_late_credits(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), ConsensusError> {
    binding::validate_late_credits_binding(layout, artifacts)
        .map_err(crate::errors::consensus_display_error)
}

#[cfg(test)]
mod tests;
