//! Stateless system-transaction header and body bindings, in rejection order.
use super::{
    consensus_other, ConsensusError, OcompLifecycleActivation, OutbeBlockBody, OutbeHeader,
    REWARDS_ADDRESS,
};
use alloy_consensus::{BlockHeader as _, Transaction as _};
use outbe_primitives::system_tx::binding::{self, ParentAccountingBindingError};
use outbe_primitives::{reshare_artifact::OutbeBlockArtifacts, system_tx::SystemTxLayout};

pub(super) fn validate_beneficiary(header: &OutbeHeader) -> Result<(), ConsensusError> {
    if header.number() > 0 && header.beneficiary() != REWARDS_ADDRESS {
        return Err(consensus_other(format!(
            "non-genesis block beneficiary must be REWARDS_ADDRESS {}: got {}",
            REWARDS_ADDRESS,
            header.beneficiary()
        )));
    }

    Ok(())
}

pub(super) fn validate_layout<'a>(
    body: &'a OutbeBlockBody,
    header: &OutbeHeader,
    activation: OcompLifecycleActivation,
) -> Result<(SystemTxLayout<'a>, OutbeBlockArtifacts), ConsensusError> {
    binding::validate_system_layout(body, header, activation)
        .map_err(|error| consensus_other(error.to_string()))
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
        .map_err(|error| consensus_other(error.to_string()))
}

pub(super) fn validate_late_credits(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), ConsensusError> {
    // Bind the header's `late_finalize_credits` artifact to the body's
    // `LateFinalizeCredits` system-tx calldata. The artifact has tag 0x06 and is
    // hash-committed and BLS-verified pre-exec. The binding makes sure that the
    // verified artifact is exactly the one that settles fees. This mirrors the
    // BoundaryOutcome parity above. The header `Option` maps to the calldata
    // artifact via the proposer build path's `unwrap_or_default()`:
    // `None => empty`, `Some(a) => a`.
    let header_credits = artifacts.late_finalize_credits.clone().unwrap_or_default();
    let mut found = false;
    for input in system_inputs(layout) {
        let input = input?;
        if let outbe_evm::system_tx::SystemTxInputV2::LateFinalizeCredits { artifact } = input {
            if artifact != header_credits {
                return Err(consensus_other(
                    "LateFinalizeCredits system tx artifact does not match header late_finalize_credits",
                ));
            }
            found = true;
            break;
        }
    }
    // No body tx (block < 2): the header must then carry no credits.
    if !found && !header_credits.batches.is_empty() {
        return Err(consensus_other(
            "header carries late_finalize_credits but block has no LateFinalizeCredits system tx",
        ));
    }
    Ok(())
}

fn decode_system_input(
    tx: &reth_ethereum::TransactionSigned,
) -> Result<outbe_evm::system_tx::SystemTxInputV2, ConsensusError> {
    outbe_evm::system_tx::SystemTxInputV2::decode(tx.input().as_ref())
        .map_err(|error| consensus_other(format!("decode system tx input: {error}")))
}

fn system_inputs<'a>(
    layout: &'a SystemTxLayout<'_>,
) -> impl Iterator<Item = Result<outbe_evm::system_tx::SystemTxInputV2, ConsensusError>> + 'a {
    layout
        .begin
        .iter()
        .chain(layout.end.iter())
        .map(|tx| decode_system_input(tx))
}

#[cfg(test)]
mod tests;
