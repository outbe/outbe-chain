//! Pure system-transaction layout and header/body bindings.
use super::{
    split_system_layout, validate_system_tx_set_for_activation, OcompLifecycleActivation,
    SystemTxError, SystemTxInputV2, SystemTxKind, SystemTxLayout,
};
use crate::{
    error::PrecompileError,
    reshare_artifact::{
        decode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    },
    OutbeBlockBody, OutbeHeader,
};
use alloy_consensus::{BlockHeader as _, Transaction as _};
use alloy_primitives::B256;

/// Failure stage while decoding artifacts and validating the system zones.
#[derive(Debug, thiserror::Error)]
pub enum LayoutBindingError {
    #[error("decode Outbe block artifacts: {0}")]
    Artifacts(#[source] PrecompileError),
    #[error("invalid system tx layout: {0}")]
    Layout(#[source] SystemTxError),
    #[error("invalid system tx set: {0}")]
    Set(#[source] SystemTxError),
}

/// Failure to bind the first begin transaction to the header's parent.
#[derive(Debug, thiserror::Error)]
pub enum ParentAccountingBindingError {
    #[error("missing CertifiedParentAccounting system tx")]
    Missing { block_number: u64 },
    #[error("decode CertifiedParentAccounting input: {0}")]
    Decode(#[source] SystemTxError),
    #[error("expected CertifiedParentAccounting system tx at begin ordinal 0")]
    WrongKind,
    #[error("CertifiedParentAccounting metadata hash must match block parent: expected {expected}, got {actual}")]
    HashMismatch { expected: B256, actual: B256 },
}

/// Failure to bind BoundaryOutcome calldata to the header artifact.
#[derive(Debug, thiserror::Error)]
pub enum BoundaryBindingError {
    #[error("decode system tx input: {0}")]
    Decode(#[source] SystemTxError),
    #[error("BoundaryOutcome system tx artifact mismatch")]
    Mismatch,
    #[error("missing BoundaryOutcome system tx for header artifact")]
    Missing,
}

/// Decode header artifacts before validating the layout and active membership.
/// Envelope, signer and execution checks belong to the caller.
pub fn validate_system_layout<'a>(
    body: &'a OutbeBlockBody,
    header: &OutbeHeader,
    activation: OcompLifecycleActivation,
) -> Result<(SystemTxLayout<'a>, OutbeBlockArtifacts), LayoutBindingError> {
    let artifacts = decode_outbe_block_artifacts(header.extra_data().as_ref())
        .map_err(LayoutBindingError::Artifacts)?;
    let layout = split_system_layout(&body.transactions).map_err(LayoutBindingError::Layout)?;
    let has_boundary_outcome = matches!(
        &artifacts.consensus_header_artifact,
        Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
    );
    let has_tee_bootstrap = layout.has_begin_kind(SystemTxKind::TeeBootstrap);
    validate_system_tx_set_for_activation(
        &layout,
        header.number(),
        has_boundary_outcome,
        has_tee_bootstrap,
        activation,
    )
    .map_err(LayoutBindingError::Set)?;
    Ok((layout, artifacts))
}

/// Bind CertifiedParentAccounting to the parent hash from block two onward.
/// Other metadata and certificate checks belong to the accounting verifier.
pub fn validate_parent_accounting_binding(
    layout: &SystemTxLayout<'_>,
    header: &OutbeHeader,
) -> Result<(), ParentAccountingBindingError> {
    if header.number() < 2 {
        return Ok(());
    }
    let tx = *layout
        .begin
        .first()
        .ok_or(ParentAccountingBindingError::Missing {
            block_number: header.number(),
        })?;
    let input = SystemTxInputV2::decode(tx.input().as_ref())
        .map_err(ParentAccountingBindingError::Decode)?;
    let SystemTxInputV2::CertifiedParentAccounting { metadata } = input else {
        return Err(ParentAccountingBindingError::WrongKind);
    };
    if metadata.finalized_block_hash != header.parent_hash() {
        return Err(ParentAccountingBindingError::HashMismatch {
            expected: header.parent_hash(),
            actual: metadata.finalized_block_hash,
        });
    }
    Ok(())
}

/// Check header/body parity when the header carries BoundaryOutcome.
/// Scan every system input, including inputs after a matching artifact.
pub fn validate_boundary_outcome_binding(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), BoundaryBindingError> {
    let Some(ConsensusHeaderArtifact::BoundaryOutcome(header_artifact)) =
        artifacts.consensus_header_artifact.as_ref()
    else {
        return Ok(());
    };
    let mut matched = false;
    for tx in layout.begin.iter().chain(layout.end.iter()) {
        let tx = *tx;
        let input =
            SystemTxInputV2::decode(tx.input().as_ref()).map_err(BoundaryBindingError::Decode)?;
        if let SystemTxInputV2::BoundaryOutcome { artifact } = input {
            if &artifact != header_artifact {
                return Err(BoundaryBindingError::Mismatch);
            }
            matched = true;
        }
    }
    if !matched {
        return Err(BoundaryBindingError::Missing);
    }
    Ok(())
}
