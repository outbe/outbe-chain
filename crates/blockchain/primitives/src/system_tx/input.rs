use crate::consensus::DkgBoundaryArtifact;
use crate::consensus_metadata::CertifiedParentAccountingMetadata;
use crate::reshare_artifact::decode_boundary_artifact;
use crate::reshare_artifact::decode_late_finalize_credits_artifact;
use crate::reshare_artifact::encode_boundary_artifact;
use crate::reshare_artifact::encode_late_finalize_credits_artifact;
use crate::reshare_artifact::LateFinalizeCreditsArtifact;
use alloy_primitives::Bytes;

use super::{
    selector_from_input, BodyZone, SysTxSelector, SystemTxError, BOUNDARY_OUTCOME_SELECTOR,
    CERTIFIED_PARENT_ACCOUNTING_SELECTOR, CYCLE_TICK_SELECTOR, HOOK_EVENTS_SELECTOR,
    LATE_FINALIZE_CREDITS_SELECTOR, ORACLE_SLASH_WINDOW_SELECTOR, RADICLE_PUBLICATION_SELECTOR,
    SYSTEM_TX_INPUT_VERSION, TEE_BOOTSTRAP_SELECTOR,
};

/// Versioned calldata body system transactions.
///
/// The wire-format swap is complete: Phase 1 system-tx input carries the V2 slim
/// [`CertifiedParentAccountingMetadata`].
/// The V2 payload omits the dead `encoded_finalize_votes` field
/// (the V2 signer bitmap is authoritative).
/// It carries the V2 `committee_set_hash`, `vrf_material_version`,
/// `vrf_group_public_key_hash`, and `proof_kind` fields that the verifier needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemTx {
    CertifiedParentAccounting {
        metadata: CertifiedParentAccountingMetadata,
    },
    LateFinalizeCredits {
        artifact: LateFinalizeCreditsArtifact,
    },
    CycleTick,
    BoundaryOutcome {
        artifact: DkgBoundaryArtifact,
    },
    TeeBootstrap {
        payload: crate::tee_bootstrap_v2::TeeBootstrapV2,
    },
    OracleSlashWindow,
    HookEvents,
    RadiclePublication {
        batch: crate::radicle_publication::PublicationBatch,
    },
}

impl SystemTx {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::CertifiedParentAccounting { .. } => "CertifiedParentAccounting",
            Self::LateFinalizeCredits { .. } => "LateFinalizeCredits",
            Self::CycleTick => "CycleTick",
            Self::BoundaryOutcome { .. } => "BoundaryOutcome",
            Self::TeeBootstrap { .. } => "TeeBootstrap",
            Self::OracleSlashWindow => "OracleSlashWindow",
            Self::HookEvents => "HookEvents",
            Self::RadiclePublication { .. } => "RadiclePublication",
        }
    }

    pub const fn selector(&self) -> SysTxSelector {
        match self {
            Self::CertifiedParentAccounting { .. } => CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
            Self::LateFinalizeCredits { .. } => LATE_FINALIZE_CREDITS_SELECTOR,
            Self::CycleTick => CYCLE_TICK_SELECTOR,
            Self::BoundaryOutcome { .. } => BOUNDARY_OUTCOME_SELECTOR,
            Self::TeeBootstrap { .. } => TEE_BOOTSTRAP_SELECTOR,
            Self::OracleSlashWindow => ORACLE_SLASH_WINDOW_SELECTOR,
            Self::HookEvents => HOOK_EVENTS_SELECTOR,
            Self::RadiclePublication { .. } => RADICLE_PUBLICATION_SELECTOR,
        }
    }

    pub const fn body_zone(&self) -> BodyZone {
        BodyZone::BeginBlock
    }

    /// Economic and consensus-critical phases fail the block on revert or halt.
    /// Oracle and hook failures are soft only within the aggregate work budget.
    pub const fn revert_fails_block(&self) -> bool {
        !matches!(self, Self::OracleSlashWindow | Self::HookEvents)
    }

    pub const fn begin_order(&self) -> Option<u8> {
        Some(match self {
            Self::CertifiedParentAccounting { .. } => 0,
            Self::LateFinalizeCredits { .. } => 1,
            Self::CycleTick => 2,
            Self::BoundaryOutcome { .. } => 3,
            Self::TeeBootstrap { .. } => 4,
            Self::OracleSlashWindow => 5,
            Self::HookEvents => 6,
            Self::RadiclePublication { .. } => 7,
        })
    }

    pub const fn end_order(&self) -> Option<u8> {
        None
    }

    pub const fn order_in(&self, zone: BodyZone) -> Option<u8> {
        match zone {
            BodyZone::BeginBlock => self.begin_order(),
            BodyZone::EndBlock => self.end_order(),
        }
    }

    /// Encode as `selector(4) || version(1) || canonical_body`.
    pub fn encode(&self) -> Result<Bytes, SystemTxError> {
        let mut out = Vec::new();
        let selector = self.selector();
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
            Self::CycleTick | Self::OracleSlashWindow | Self::HookEvents => {}
            Self::LateFinalizeCredits { artifact } => {
                // Empty batches encode to empty bytes. The mandatory tx then
                // carries an empty body and still drives the window-close settle.
                out.extend_from_slice(
                    encode_late_finalize_credits_artifact(artifact)
                        .map_err(SystemTxError::from_precompile)?
                        .as_ref(),
                );
            }
            Self::RadiclePublication { batch } => {
                out.extend_from_slice(&batch.to_bytes().map_err(SystemTxError::from_precompile)?);
            }
            Self::BoundaryOutcome { artifact } => {
                out.extend_from_slice(
                    encode_boundary_artifact(artifact)
                        .map_err(SystemTxError::from_precompile)?
                        .as_ref(),
                );
            }
            Self::TeeBootstrap { payload } => out.extend_from_slice(
                crate::tee_bootstrap_v2::codec::encode_canonical(payload)
                    .map_err(SystemTxError::codec)?
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
        if !matches!(
            selector,
            CERTIFIED_PARENT_ACCOUNTING_SELECTOR
                | LATE_FINALIZE_CREDITS_SELECTOR
                | CYCLE_TICK_SELECTOR
                | BOUNDARY_OUTCOME_SELECTOR
                | TEE_BOOTSTRAP_SELECTOR
                | ORACLE_SLASH_WINDOW_SELECTOR
                | HOOK_EVENTS_SELECTOR
                | RADICLE_PUBLICATION_SELECTOR
        ) {
            return Err(SystemTxError::UnknownSelector(selector));
        }
        let version = data[4];
        if version != SYSTEM_TX_INPUT_VERSION {
            return Err(SystemTxError::UnsupportedVersion(version));
        }
        let body = &data[5..];
        match selector {
            CERTIFIED_PARENT_ACCOUNTING_SELECTOR => Ok(Self::CertifiedParentAccounting {
                metadata: CertifiedParentAccountingMetadata::decode(body)
                    .map_err(SystemTxError::from_precompile)?,
            }),
            LATE_FINALIZE_CREDITS_SELECTOR => Ok(Self::LateFinalizeCredits {
                // Empty body => empty (no-op) artifact. The matured-window close
                // still runs on execution.
                artifact: decode_late_finalize_credits_artifact(body)
                    .map_err(SystemTxError::from_precompile)?
                    .unwrap_or_default(),
            }),
            CYCLE_TICK_SELECTOR => Self::decode_empty_body(body, Self::CycleTick),
            ORACLE_SLASH_WINDOW_SELECTOR => Self::decode_empty_body(body, Self::OracleSlashWindow),
            HOOK_EVENTS_SELECTOR => Self::decode_empty_body(body, Self::HookEvents),
            RADICLE_PUBLICATION_SELECTOR => Ok(Self::RadiclePublication {
                batch: crate::radicle_publication::PublicationBatch::from_bytes(body)
                    .map_err(SystemTxError::from_precompile)?,
            }),
            BOUNDARY_OUTCOME_SELECTOR => {
                let Some(artifact) =
                    decode_boundary_artifact(body).map_err(SystemTxError::from_precompile)?
                else {
                    return Err(SystemTxError::MissingBoundaryOutcomeBody);
                };
                Ok(Self::BoundaryOutcome { artifact })
            }
            TEE_BOOTSTRAP_SELECTOR => Ok(Self::TeeBootstrap {
                payload: crate::tee_bootstrap_v2::codec::decode_canonical(body)
                    .map_err(SystemTxError::codec)?,
            }),
            other => Err(SystemTxError::UnknownSelector(other)),
        }
    }
    fn decode_empty_body(body: &[u8], value: Self) -> Result<Self, SystemTxError> {
        if body.is_empty() {
            Ok(value)
        } else {
            Err(SystemTxError::UnexpectedBody {
                kind: value.name(),
                len: body.len(),
            })
        }
    }
}
