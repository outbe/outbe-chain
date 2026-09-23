use crate::consensus::DkgBoundaryArtifact;
use crate::consensus_metadata::CertifiedParentAccountingMetadata;
use crate::reshare_artifact::decode_boundary_artifact;
use crate::reshare_artifact::decode_late_finalize_credits_artifact;
use crate::reshare_artifact::encode_boundary_artifact;
use crate::reshare_artifact::encode_late_finalize_credits_artifact;
use crate::reshare_artifact::LateFinalizeCreditsArtifact;
use alloy_primitives::Bytes;

use super::{
    selector_from_input, system_tx_kind_from_selector, SystemTxError, SystemTxKind,
    SYSTEM_TX_INPUT_VERSION,
};

/// Versioned calldata body system transactions.
///
/// completed the wire-format swap: Phase 1 system-tx input now
/// carries the V2 slim
/// [`crate::consensus_metadata::CertifiedParentAccountingMetadata`]
/// instead of the V1 `ConsensusMetadataEnvelope`. The V2 payload omits the
/// dead `encoded_finalize_votes` field (V2 signer bitmap is authoritative)
/// and carries the V2 `committee_set_hash`, `vrf_material_version`,
/// `vrf_group_public_key_hash`, and `proof_kind` fields the verifier needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemTxInputV2 {
    CertifiedParentAccounting {
        metadata: CertifiedParentAccountingMetadata,
    },
    LateFinalizeCredits {
        artifact: LateFinalizeCreditsArtifact,
    },
    OcompLifecycleBegin,
    CycleTick,
    RewardsGemDelivery,
    BoundaryOutcome {
        artifact: DkgBoundaryArtifact,
    },
    TeeBootstrap {
        payload: crate::tee_bootstrap_v2::TeeBootstrapV2,
    },
    OracleSlashWindow,
    HookEvents,
    OcompTerminalRequest,
}

impl SystemTxInputV2 {
    pub const fn kind(&self) -> SystemTxKind {
        match self {
            Self::CertifiedParentAccounting { .. } => SystemTxKind::CertifiedParentAccounting,
            Self::LateFinalizeCredits { .. } => SystemTxKind::LateFinalizeCredits,
            Self::OcompLifecycleBegin => SystemTxKind::OcompLifecycleBegin,
            Self::CycleTick => SystemTxKind::CycleTick,
            Self::RewardsGemDelivery => SystemTxKind::RewardsGemDelivery,
            Self::BoundaryOutcome { .. } => SystemTxKind::BoundaryOutcome,
            Self::TeeBootstrap { .. } => SystemTxKind::TeeBootstrap,
            Self::OracleSlashWindow => SystemTxKind::OracleSlashWindow,
            Self::HookEvents => SystemTxKind::HookEvents,
            Self::OcompTerminalRequest => SystemTxKind::OcompTerminalRequest,
        }
    }

    /// Encode as `selector(4) || version(1) || canonical_body`.
    pub fn encode(&self) -> Result<Bytes, SystemTxError> {
        let mut out = Vec::new();
        let selector = self.kind().selector();
        out.extend_from_slice(&selector);
        out.push(SYSTEM_TX_INPUT_VERSION);
        match self {
            Self::CertifiedParentAccounting { metadata } => {
                out.extend_from_slice(
                    metadata
                        .encode()
                        .map_err(SystemTxError::from_precompile)?
                        .as_ref(),
                );
            }
            Self::OcompLifecycleBegin
            | Self::CycleTick
            | Self::RewardsGemDelivery
            | Self::OracleSlashWindow
            | Self::HookEvents
            | Self::OcompTerminalRequest => {}
            Self::LateFinalizeCredits { artifact } => {
                // Empty batches encode to empty bytes - the mandatory tx then
                // carries an empty body and still drives the window-close settle.
                out.extend_from_slice(
                    encode_late_finalize_credits_artifact(artifact)
                        .map_err(SystemTxError::from_precompile)?
                        .as_ref(),
                );
            }
            Self::BoundaryOutcome { artifact } => {
                out.extend_from_slice(
                    encode_boundary_artifact(artifact)
                        .map_err(SystemTxError::from_precompile)?
                        .as_ref(),
                );
            }
            Self::TeeBootstrap { payload } => out.extend_from_slice(
                payload
                    .encode_canonical()
                    .map_err(|error| SystemTxError::Codec(error.to_string()))?
                    .as_ref(),
            ),
        }
        Ok(Bytes::from(out))
    }

    pub fn decode(data: &[u8]) -> Result<Self, SystemTxError> {
        if data.len() < 5 {
            return Err(SystemTxError::InputTooShort { len: data.len() });
        }
        let selector = selector_from_input(data)?;
        let kind = system_tx_kind_from_selector(selector)?;
        let version = data[4];
        if version != SYSTEM_TX_INPUT_VERSION {
            return Err(SystemTxError::UnsupportedVersion(version));
        }
        let body = &data[5..];
        match kind {
            SystemTxKind::CertifiedParentAccounting => Ok(Self::CertifiedParentAccounting {
                metadata: CertifiedParentAccountingMetadata::decode(body)
                    .map_err(SystemTxError::from_precompile)?,
            }),
            SystemTxKind::LateFinalizeCredits => Ok(Self::LateFinalizeCredits {
                // Empty body => empty (no-op) artifact; the matured-window close
                // still runs on execution.
                artifact: decode_late_finalize_credits_artifact(body)
                    .map_err(SystemTxError::from_precompile)?
                    .unwrap_or_default(),
            }),
            SystemTxKind::OcompLifecycleBegin => {
                if !body.is_empty() {
                    return Err(SystemTxError::UnexpectedBody {
                        kind,
                        len: body.len(),
                    });
                }
                Ok(Self::OcompLifecycleBegin)
            }
            SystemTxKind::CycleTick => {
                if !body.is_empty() {
                    return Err(SystemTxError::UnexpectedBody {
                        kind,
                        len: body.len(),
                    });
                }
                Ok(Self::CycleTick)
            }
            SystemTxKind::RewardsGemDelivery => {
                if !body.is_empty() {
                    return Err(SystemTxError::UnexpectedBody {
                        kind,
                        len: body.len(),
                    });
                }
                Ok(Self::RewardsGemDelivery)
            }
            SystemTxKind::OracleSlashWindow => {
                if !body.is_empty() {
                    return Err(SystemTxError::UnexpectedBody {
                        kind,
                        len: body.len(),
                    });
                }
                Ok(Self::OracleSlashWindow)
            }
            SystemTxKind::HookEvents => {
                if !body.is_empty() {
                    return Err(SystemTxError::UnexpectedBody {
                        kind,
                        len: body.len(),
                    });
                }
                Ok(Self::HookEvents)
            }
            SystemTxKind::OcompTerminalRequest => {
                if !body.is_empty() {
                    return Err(SystemTxError::UnexpectedBody {
                        kind,
                        len: body.len(),
                    });
                }
                Ok(Self::OcompTerminalRequest)
            }
            SystemTxKind::BoundaryOutcome => {
                let Some(artifact) =
                    decode_boundary_artifact(body).map_err(SystemTxError::from_precompile)?
                else {
                    return Err(SystemTxError::MissingBoundaryOutcomeBody);
                };
                Ok(Self::BoundaryOutcome { artifact })
            }
            SystemTxKind::TeeBootstrap => Ok(Self::TeeBootstrap {
                payload: crate::tee_bootstrap_v2::TeeBootstrapV2::decode_canonical(body)
                    .map_err(|error| SystemTxError::Codec(error.to_string()))?,
            }),
        }
    }
}
