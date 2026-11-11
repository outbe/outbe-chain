//! Pure system-transaction layout and header/body bindings.
use super::{
    split_system_layout, validate_system_tx_set_for_publication, BlockHeightRegime, SystemTx,
    SystemTxError, SystemTxLayout, CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
    CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
    CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX,
};
use crate::{
    block::{BlockOrder, BlockSystemFlags},
    consensus::DkgBoundaryArtifact,
    consensus_metadata::CertifiedParentAccountingMetadata,
    error::PrecompileError,
    radicle_publication::PublicationBatch,
    reshare_artifact::{
        decode_boundary_artifact, decode_late_finalize_credits_artifact, encode_boundary_artifact,
        encode_late_finalize_credits_artifact, LateFinalizeCreditsArtifact,
    },
    reshare_artifact::{
        decode_outbe_block_artifacts, ConsensusHeaderArtifact, OutbeBlockArtifacts,
    },
    system_tx_context::{SystemTxPayloadValidationError, SystemTxValidationContext},
    tee_attestation_v1::CodecError,
    tee_bootstrap_v2::{
        codec::{decode_canonical, encode_canonical},
        TeeBootstrapV2,
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
    #[error("invalid Radicle publication binding: {0}")]
    Publication(#[source] PublicationBindingError),
    #[error("invalid parent accounting binding: {0}")]
    ParentAccounting(#[source] ParentAccountingBindingError),
    #[error("invalid late credits binding: {0}")]
    LateCredits(#[source] LateCreditsBindingError),
    #[error("invalid boundary binding: {0}")]
    Boundary(#[source] BoundaryBindingError),
    #[error("invalid TeeBootstrap payload at block {block_number}: {source}")]
    TeeBootstrap {
        block_number: u64,
        #[source]
        source: CodecError,
    },
    #[error("unexpected non-empty system tx payload for {module} ({len} bytes)")]
    UnexpectedNonEmptyPayload { module: &'static str, len: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum PublicationBindingError {
    #[error("decode payload: {0}")]
    PayloadDecode(#[source] PrecompileError),
    #[error("non-canonical payload")]
    NonCanonical,

    #[error("decode publication input: {0}")]
    Decode(#[source] SystemTxError),
    #[error("publication system transaction differs from header batch")]
    Mismatch,
    #[error("missing publication system transaction")]
    Missing,
}

/// Failure to bind the first begin transaction to the header's parent.
#[derive(Debug, thiserror::Error)]
pub enum ParentAccountingBindingError {
    #[error("decode payload: {0}")]
    PayloadDecode(#[source] PrecompileError),
    #[error("non-canonical payload")]
    NonCanonical,

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
    #[error("decode payload: {0}")]
    PayloadDecode(#[source] PrecompileError),
    #[error("non-canonical payload")]
    NonCanonical,

    #[error("decode system tx input: {0}")]
    Decode(#[source] SystemTxError),
    #[error("BoundaryOutcome system tx artifact mismatch")]
    Mismatch,
    #[error("missing BoundaryOutcome system tx for header artifact")]
    Missing,
}

/// Failure to bind `LateFinalizeCredits` calldata to the header artifact.
#[derive(Debug, thiserror::Error)]
pub enum LateCreditsBindingError {
    #[error("decode payload: {0}")]
    PayloadDecode(#[source] PrecompileError),
    #[error("non-canonical payload")]
    NonCanonical,

    #[error("decode system tx input: {0}")]
    Decode(#[source] SystemTxError),
    #[error("LateFinalizeCredits system tx artifact does not match header late_finalize_credits")]
    Mismatch,
    #[error("header carries late_finalize_credits but block has no LateFinalizeCredits system tx")]
    Missing,
}

/// Decode header artifacts before validating the layout and active membership.
/// Envelope, signer and execution checks belong to the caller.
pub fn validate_system_layout<'a>(
    body: &'a OutbeBlockBody,
    header: &OutbeHeader,
    order: &BlockOrder,
) -> Result<(SystemTxLayout<'a>, OutbeBlockArtifacts), LayoutBindingError> {
    let artifacts = decode_outbe_block_artifacts(header.extra_data().as_ref())
        .map_err(LayoutBindingError::Artifacts)?;
    let layout = split_system_layout(&body.transactions).map_err(LayoutBindingError::Layout)?;
    validate_system_tx_set_for_publication(
        order,
        &layout,
        header.number(),
        BlockSystemFlags::from_artifacts(&artifacts),
    )
    .map_err(LayoutBindingError::Set)?;
    let ctx = SystemTxValidationContext {
        regime: BlockHeightRegime::for_block(header.number()),
        block_number: header.number(),
        parent_hash: header.parent_hash(),
        artifacts: &artifacts,
    };
    for tx in layout.begin.iter().chain(&layout.end) {
        let input = SystemTx::decode((*tx).input()).map_err(LayoutBindingError::Layout)?;
        let step = order.step_for_tx(&input).ok_or_else(|| {
            LayoutBindingError::Set(SystemTxError::UnexpectedSystemTx {
                stage: input.stage(),
                expected: None,
                actual: input.stage_name(),
            })
        })?;
        (step.validate_payload)(input.payload(), &ctx)?;
    }
    Ok((layout, artifacts))
}

impl From<SystemTxPayloadValidationError> for LayoutBindingError {
    fn from(error: SystemTxPayloadValidationError) -> Self {
        match error {
            SystemTxPayloadValidationError::ParentAccounting(error) => {
                Self::ParentAccounting(error)
            }
            SystemTxPayloadValidationError::LateCredits(error) => Self::LateCredits(error),
            SystemTxPayloadValidationError::Boundary(error) => Self::Boundary(error),
            SystemTxPayloadValidationError::Publication(error) => Self::Publication(error),
            SystemTxPayloadValidationError::TeeBootstrap {
                block_number,
                source,
            } => Self::TeeBootstrap {
                block_number,
                source,
            },
            SystemTxPayloadValidationError::UnexpectedNonEmptyPayload { module, len } => {
                Self::UnexpectedNonEmptyPayload { module, len }
            }
        }
    }
}

pub fn validate_parent_accounting_payload(
    payload: &[u8],
    parent_hash: B256,
) -> Result<CertifiedParentAccountingMetadata, ParentAccountingBindingError> {
    let metadata = CertifiedParentAccountingMetadata::decode(payload)
        .map_err(ParentAccountingBindingError::PayloadDecode)?;
    let reencoded = metadata
        .encode()
        .map_err(ParentAccountingBindingError::PayloadDecode)?;
    if reencoded.as_ref() != payload {
        return Err(ParentAccountingBindingError::NonCanonical);
    }
    if metadata.finalized_block_hash != parent_hash {
        return Err(ParentAccountingBindingError::HashMismatch {
            expected: parent_hash,
            actual: metadata.finalized_block_hash,
        });
    }
    Ok(metadata)
}

pub fn validate_late_finalize_credits_payload(
    payload: &[u8],
    artifacts: &OutbeBlockArtifacts,
) -> Result<LateFinalizeCreditsArtifact, LateCreditsBindingError> {
    let artifact = decode_late_finalize_credits_artifact(payload)
        .map_err(LateCreditsBindingError::PayloadDecode)?
        .unwrap_or_default();
    let reencoded = encode_late_finalize_credits_artifact(&artifact)
        .map_err(LateCreditsBindingError::PayloadDecode)?;
    if reencoded.as_ref() != payload {
        return Err(LateCreditsBindingError::NonCanonical);
    }
    if artifact != artifacts.late_finalize_credits.clone().unwrap_or_default() {
        return Err(LateCreditsBindingError::Mismatch);
    }
    Ok(artifact)
}

pub fn validate_boundary_outcome_payload(
    payload: &[u8],
    artifacts: &OutbeBlockArtifacts,
) -> Result<DkgBoundaryArtifact, BoundaryBindingError> {
    let artifact = decode_boundary_artifact(payload)
        .map_err(BoundaryBindingError::PayloadDecode)?
        .ok_or(BoundaryBindingError::Missing)?;
    let reencoded =
        encode_boundary_artifact(&artifact).map_err(BoundaryBindingError::PayloadDecode)?;
    if reencoded.as_ref() != payload {
        return Err(BoundaryBindingError::NonCanonical);
    }
    if !matches!(&artifacts.consensus_header_artifact, Some(ConsensusHeaderArtifact::BoundaryOutcome(expected)) if expected == &artifact)
    {
        return Err(BoundaryBindingError::Mismatch);
    }
    Ok(artifact)
}

pub fn validate_publication_payload(
    payload: &[u8],
    artifacts: &OutbeBlockArtifacts,
) -> Result<PublicationBatch, PublicationBindingError> {
    let batch =
        PublicationBatch::from_bytes(payload).map_err(PublicationBindingError::PayloadDecode)?;
    let reencoded = alloy_primitives::Bytes::from(
        batch
            .to_bytes()
            .map_err(PublicationBindingError::PayloadDecode)?,
    );
    if reencoded.as_ref() != payload {
        return Err(PublicationBindingError::NonCanonical);
    }
    if artifacts.radicle_publication.as_ref() != Some(&batch) {
        return Err(PublicationBindingError::Mismatch);
    }
    Ok(batch)
}

pub fn validate_tee_bootstrap_payload(
    payload: &[u8],
    block_number: u64,
) -> Result<TeeBootstrapV2, SystemTxPayloadValidationError> {
    let validate = || {
        let decoded = decode_canonical(payload)?;
        let reencoded = encode_canonical(&decoded)?;
        if reencoded.as_ref() != payload {
            return Err(CodecError::NonCanonical("TeeBootstrap payload"));
        }
        decoded.preflight()?;
        Ok(decoded)
    };
    validate().map_err(|source| SystemTxPayloadValidationError::TeeBootstrap {
        block_number,
        source,
    })
}

pub fn validate_publication_binding(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), PublicationBindingError> {
    for tx in &layout.begin {
        let input = SystemTx::decode((*tx).input()).map_err(PublicationBindingError::Decode)?;
        if let SystemTx::Consensus {
            step_index: CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX,
            payload,
        } = input
        {
            validate_publication_payload(&payload, artifacts)?;
            return Ok(());
        }
    }
    if artifacts.radicle_publication.is_some() {
        Err(PublicationBindingError::Missing)
    } else {
        Ok(())
    }
}

pub fn validate_parent_accounting_binding(
    layout: &SystemTxLayout<'_>,
    header: &OutbeHeader,
) -> Result<(), ParentAccountingBindingError> {
    if !BlockHeightRegime::for_block(header.number()).has_parent_finalization_accounting() {
        return Ok(());
    }
    let tx = layout
        .begin
        .first()
        .ok_or(ParentAccountingBindingError::Missing {
            block_number: header.number(),
        })?;
    let input = SystemTx::decode((*tx).input()).map_err(ParentAccountingBindingError::Decode)?;
    let SystemTx::Consensus {
        step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
        payload,
    } = input
    else {
        return Err(ParentAccountingBindingError::WrongKind);
    };
    validate_parent_accounting_payload(&payload, header.parent_hash())?;
    Ok(())
}

/// Scan every input, including malformed entries after a matching boundary.
pub fn validate_boundary_outcome_binding(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), BoundaryBindingError> {
    if !matches!(
        &artifacts.consensus_header_artifact,
        Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
    ) {
        return Ok(());
    }
    let mut matched = false;
    for tx in layout.begin.iter().chain(&layout.end) {
        let input = SystemTx::decode((*tx).input()).map_err(BoundaryBindingError::Decode)?;
        if let SystemTx::Consensus {
            step_index: CONSENSUS_BOUNDARY_OUTCOME_STEP_INDEX,
            payload,
        } = input
        {
            validate_boundary_outcome_payload(&payload, artifacts)?;
            matched = true;
        }
    }
    if matched {
        Ok(())
    } else {
        Err(BoundaryBindingError::Missing)
    }
}

pub fn validate_late_credits_binding(
    layout: &SystemTxLayout<'_>,
    artifacts: &OutbeBlockArtifacts,
) -> Result<(), LateCreditsBindingError> {
    for tx in layout.begin.iter().chain(&layout.end) {
        let input = SystemTx::decode((*tx).input()).map_err(LateCreditsBindingError::Decode)?;
        if let SystemTx::Consensus {
            step_index: CONSENSUS_LATE_FINALIZE_CREDITS_STEP_INDEX,
            payload,
        } = input
        {
            validate_late_finalize_credits_payload(&payload, artifacts)?;
            return Ok(());
        }
    }
    if artifacts
        .late_finalize_credits
        .as_ref()
        .is_some_and(|credits| !credits.batches.is_empty())
    {
        Err(LateCreditsBindingError::Missing)
    } else {
        Ok(())
    }
}
