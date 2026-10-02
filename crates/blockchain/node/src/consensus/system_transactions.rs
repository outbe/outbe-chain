//! Stateless system-transaction header and body bindings, in rejection order.
use super::{
    consensus_other, ConsensusError, OcompLifecycleActivation, OutbeBlockBody, OutbeHeader,
    REWARDS_ADDRESS,
};
use alloy_consensus::{BlockHeader as _, Transaction as _};
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
    let artifacts = outbe_primitives::reshare_artifact::decode_outbe_block_artifacts(
        header.extra_data().as_ref(),
    )
    .map_err(|error| consensus_other(format!("decode Outbe block artifacts: {error}")))?;
    let has_boundary_outcome = matches!(
        &artifacts.consensus_header_artifact,
        Some(outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::BoundaryOutcome(_))
    );
    let layout = outbe_evm::system_tx::split_system_layout(&body.transactions)
        .map_err(|error| consensus_other(format!("invalid system tx layout: {error}")))?;
    let has_tee_bootstrap = layout.has_begin_kind(outbe_evm::system_tx::SystemTxKind::TeeBootstrap);
    outbe_evm::system_tx::validate_system_tx_set_for_activation(
        &layout,
        header.number(),
        has_boundary_outcome,
        has_tee_bootstrap,
        activation,
    )
    .map_err(|error| consensus_other(format!("invalid system tx set: {error}")))?;

    Ok((layout, artifacts))
}

pub(super) fn validate_parent_accounting(
    layout: &SystemTxLayout<'_>,
    header: &OutbeHeader,
) -> Result<(), ConsensusError> {
    if header.number() >= 2 {
        let finalization_tx = *layout.begin.first().ok_or_else(|| {
            consensus_other(format!(
                "missing CertifiedParentAccounting system tx for block {}",
                header.number()
            ))
        })?;
        let input = outbe_evm::system_tx::SystemTxInputV2::decode(finalization_tx.input().as_ref())
            .map_err(|error| {
                consensus_other(format!("decode CertifiedParentAccounting input: {error}"))
            })?;
        let outbe_evm::system_tx::SystemTxInputV2::CertifiedParentAccounting { metadata } = input
        else {
            return Err(consensus_other(
                "expected CertifiedParentAccounting system tx at begin ordinal 0",
            ));
        };
        if metadata.finalized_block_hash != header.parent_hash() {
            return Err(consensus_other(format!(
                "CertifiedParentAccounting metadata hash must match block parent: expected {}, got {}",
                header.parent_hash(),
                metadata.finalized_block_hash
            )));
        }
    }

    Ok(())
}

pub(super) fn validate_boundary_outcome(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), ConsensusError> {
    let Some(outbe_primitives::reshare_artifact::ConsensusHeaderArtifact::BoundaryOutcome(
        header_artifact,
    )) = artifacts.consensus_header_artifact.as_ref()
    else {
        return Ok(());
    };
    let mut matched = false;
    for input in system_inputs(layout) {
        let input = input?;
        if let outbe_evm::system_tx::SystemTxInputV2::BoundaryOutcome { artifact } = input {
            if &artifact != header_artifact {
                return Err(consensus_other(
                    "BoundaryOutcome system tx artifact mismatch",
                ));
            }
            matched = true;
        }
    }
    if !matched {
        return Err(consensus_other(
            "missing BoundaryOutcome system tx for header artifact",
        ));
    }
    Ok(())
}

pub(super) fn validate_late_credits(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), ConsensusError> {
    // bind the header's `late_finalize_credits` artifact (tag
    // 0x06 - hash-committed and BLS-verified pre-exec) to the body's
    // `LateFinalizeCredits` system-tx calldata, so the artifact that is verified
    // is exactly the one that settles fees. Mirrors the BoundaryOutcome parity
    // above. The header `Option` maps to the calldata artifact via the proposer
    // build path's `unwrap_or_default()`: `None => empty`, `Some(a) => a`.
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
