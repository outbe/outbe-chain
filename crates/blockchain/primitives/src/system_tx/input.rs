use crate::consensus::DkgBoundaryArtifact;
use crate::consensus_metadata::CertifiedParentAccountingMetadata;
use crate::reshare_artifact::decode_boundary_artifact;
use crate::reshare_artifact::decode_late_finalize_credits_artifact;
use crate::reshare_artifact::encode_boundary_artifact;
use crate::reshare_artifact::encode_late_finalize_credits_artifact;
use crate::reshare_artifact::LateFinalizeCreditsArtifact;
use alloy_primitives::Bytes;

use super::{
    selector_from_input, BlockExecutionStage, BodyZone, SysTxSelector, SystemTxError,
    BOUNDARY_OUTCOME_SELECTOR, CERTIFIED_PARENT_ACCOUNTING_SELECTOR, CREDIS_FACTORY_SELECTOR,
    CYCLE_TICK_SELECTOR, ENCLAVE_UPGRADE_DEADLINE_SELECTOR, GEM_CLEANUP_SELECTOR,
    GEM_FACTORY_SELECTOR, HYPERLANE_SLASH_SELECTOR, INTEX_FACTORY_SELECTOR,
    LATE_FINALIZE_CREDITS_SELECTOR, NOD_FACTORY_ORDINAL, NOD_FACTORY_SELECTOR, ORACLE_SELECTOR,
    ORACLE_SLASH_SELECTOR, RADICLE_PUBLICATION_SELECTOR, REWARDS_SELECTOR, STAKING_SELECTOR,
    SYSTEM_TX_INPUT_VERSION, TEE_BOOTSTRAP_SELECTOR, UPDATE_SELECTOR, VALIDATOR_LIVENESS_SELECTOR,
    VOTE_SELECTOR,
};

/// Discriminant for each of the 20 deterministic system-transaction kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemTxKind {
    // Stage 1: CONSENSUS_ORDER (6 kinds)
    CertifiedParentAccounting,
    LateFinalizeCredits,
    BoundaryOutcome,
    TeeBootstrap,
    ValidatorLiveness,
    RadiclePublication,
    // Stage 2: BEGIN_BLOCK_ORDER (9 kinds)
    Vote,
    Update,
    EnclaveUpgradeDeadline,
    Rewards,
    Staking,
    Oracle,
    OracleSlash,
    HyperlaneSlash,
    CycleTick,
    // Stage 4: END_BLOCK_ORDER (5 kinds, ordinals 24..=28)
    NodFactory,
    GemFactory,
    IntexFactory,
    CredisFactory,
    GemCleanup,
}

/// Canonical Stage 1 (`ConsensusStage`) system-transaction order.
pub const CONSENSUS_ORDER: [SystemTxKind; 6] = [
    SystemTxKind::CertifiedParentAccounting,
    SystemTxKind::LateFinalizeCredits,
    SystemTxKind::BoundaryOutcome,
    SystemTxKind::TeeBootstrap,
    SystemTxKind::ValidatorLiveness,
    SystemTxKind::RadiclePublication,
];

/// Canonical Stage 2 (`BeginBlock`) system-transaction order.
pub const BEGIN_BLOCK_ORDER: [SystemTxKind; 9] = [
    SystemTxKind::Vote,
    SystemTxKind::Update,
    SystemTxKind::EnclaveUpgradeDeadline,
    SystemTxKind::Rewards,
    SystemTxKind::Staking,
    SystemTxKind::Oracle,
    SystemTxKind::OracleSlash,
    SystemTxKind::HyperlaneSlash,
    SystemTxKind::CycleTick,
];

/// Canonical Stage 4 (`EndBlock`) system-transaction order (ordinals `24..=28`).
pub const END_BLOCK_ORDER: [SystemTxKind; 5] = [
    SystemTxKind::NodFactory,
    SystemTxKind::GemFactory,
    SystemTxKind::IntexFactory,
    SystemTxKind::CredisFactory,
    SystemTxKind::GemCleanup,
];

/// Complete list of all 20 system-transaction kinds in canonical block order.
pub const ALL_KINDS: [SystemTxKind; 20] = [
    SystemTxKind::CertifiedParentAccounting,
    SystemTxKind::LateFinalizeCredits,
    SystemTxKind::BoundaryOutcome,
    SystemTxKind::TeeBootstrap,
    SystemTxKind::ValidatorLiveness,
    SystemTxKind::RadiclePublication,
    SystemTxKind::Vote,
    SystemTxKind::Update,
    SystemTxKind::EnclaveUpgradeDeadline,
    SystemTxKind::Rewards,
    SystemTxKind::Staking,
    SystemTxKind::Oracle,
    SystemTxKind::OracleSlash,
    SystemTxKind::HyperlaneSlash,
    SystemTxKind::CycleTick,
    SystemTxKind::NodFactory,
    SystemTxKind::GemFactory,
    SystemTxKind::IntexFactory,
    SystemTxKind::CredisFactory,
    SystemTxKind::GemCleanup,
];

impl SystemTxKind {
    pub const CONSENSUS_ORDER: [Self; 6] = CONSENSUS_ORDER;
    pub const BEGIN_BLOCK_ORDER: [Self; 9] = BEGIN_BLOCK_ORDER;
    pub const END_BLOCK_ORDER: [Self; 5] = END_BLOCK_ORDER;
    pub const ALL_KINDS: [Self; 20] = ALL_KINDS;

    pub const fn name(self) -> &'static str {
        match self {
            Self::CertifiedParentAccounting => "CertifiedParentAccounting",
            Self::LateFinalizeCredits => "LateFinalizeCredits",
            Self::BoundaryOutcome => "BoundaryOutcome",
            Self::TeeBootstrap => "TeeBootstrap",
            Self::ValidatorLiveness => "ValidatorLiveness",
            Self::RadiclePublication => "RadiclePublication",
            Self::Vote => "Vote",
            Self::Update => "Update",
            Self::EnclaveUpgradeDeadline => "EnclaveUpgradeDeadline",
            Self::Rewards => "Rewards",
            Self::Staking => "Staking",
            Self::Oracle => "Oracle",
            Self::OracleSlash => "OracleSlash",
            Self::HyperlaneSlash => "HyperlaneSlash",
            Self::CycleTick => "CycleTick",
            Self::NodFactory => "NodFactory",
            Self::GemFactory => "GemFactory",
            Self::IntexFactory => "IntexFactory",
            Self::CredisFactory => "CredisFactory",
            Self::GemCleanup => "GemCleanup",
        }
    }

    pub const fn selector(self) -> SysTxSelector {
        match self {
            Self::CertifiedParentAccounting => CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
            Self::LateFinalizeCredits => LATE_FINALIZE_CREDITS_SELECTOR,
            Self::BoundaryOutcome => BOUNDARY_OUTCOME_SELECTOR,
            Self::TeeBootstrap => TEE_BOOTSTRAP_SELECTOR,
            Self::ValidatorLiveness => VALIDATOR_LIVENESS_SELECTOR,
            Self::RadiclePublication => RADICLE_PUBLICATION_SELECTOR,
            Self::Vote => VOTE_SELECTOR,
            Self::Update => UPDATE_SELECTOR,
            Self::EnclaveUpgradeDeadline => ENCLAVE_UPGRADE_DEADLINE_SELECTOR,
            Self::Rewards => REWARDS_SELECTOR,
            Self::Staking => STAKING_SELECTOR,
            Self::Oracle => ORACLE_SELECTOR,
            Self::OracleSlash => ORACLE_SLASH_SELECTOR,
            Self::HyperlaneSlash => HYPERLANE_SLASH_SELECTOR,
            Self::CycleTick => CYCLE_TICK_SELECTOR,
            Self::NodFactory => NOD_FACTORY_SELECTOR,
            Self::GemFactory => GEM_FACTORY_SELECTOR,
            Self::IntexFactory => INTEX_FACTORY_SELECTOR,
            Self::CredisFactory => CREDIS_FACTORY_SELECTOR,
            Self::GemCleanup => GEM_CLEANUP_SELECTOR,
        }
    }

    pub const fn from_selector(selector: SysTxSelector) -> Option<Self> {
        match selector {
            CERTIFIED_PARENT_ACCOUNTING_SELECTOR => Some(Self::CertifiedParentAccounting),
            LATE_FINALIZE_CREDITS_SELECTOR => Some(Self::LateFinalizeCredits),
            BOUNDARY_OUTCOME_SELECTOR => Some(Self::BoundaryOutcome),
            TEE_BOOTSTRAP_SELECTOR => Some(Self::TeeBootstrap),
            VALIDATOR_LIVENESS_SELECTOR => Some(Self::ValidatorLiveness),
            RADICLE_PUBLICATION_SELECTOR => Some(Self::RadiclePublication),
            VOTE_SELECTOR => Some(Self::Vote),
            UPDATE_SELECTOR => Some(Self::Update),
            ENCLAVE_UPGRADE_DEADLINE_SELECTOR => Some(Self::EnclaveUpgradeDeadline),
            REWARDS_SELECTOR => Some(Self::Rewards),
            STAKING_SELECTOR => Some(Self::Staking),
            ORACLE_SELECTOR => Some(Self::Oracle),
            ORACLE_SLASH_SELECTOR => Some(Self::OracleSlash),
            HYPERLANE_SLASH_SELECTOR => Some(Self::HyperlaneSlash),
            CYCLE_TICK_SELECTOR => Some(Self::CycleTick),
            NOD_FACTORY_SELECTOR => Some(Self::NodFactory),
            GEM_FACTORY_SELECTOR => Some(Self::GemFactory),
            INTEX_FACTORY_SELECTOR => Some(Self::IntexFactory),
            CREDIS_FACTORY_SELECTOR => Some(Self::CredisFactory),
            GEM_CLEANUP_SELECTOR => Some(Self::GemCleanup),
            _ => None,
        }
    }

    pub const fn stage(self) -> BlockExecutionStage {
        match self {
            Self::CertifiedParentAccounting
            | Self::LateFinalizeCredits
            | Self::BoundaryOutcome
            | Self::TeeBootstrap
            | Self::ValidatorLiveness
            | Self::RadiclePublication => BlockExecutionStage::ConsensusStage,
            Self::Vote
            | Self::Update
            | Self::EnclaveUpgradeDeadline
            | Self::Rewards
            | Self::Staking
            | Self::Oracle
            | Self::OracleSlash
            | Self::HyperlaneSlash
            | Self::CycleTick => BlockExecutionStage::BeginBlock,
            Self::NodFactory
            | Self::GemFactory
            | Self::IntexFactory
            | Self::CredisFactory
            | Self::GemCleanup => BlockExecutionStage::EndBlock,
        }
    }

    pub const fn zone_position(self) -> (BodyZone, u8) {
        match self {
            Self::CertifiedParentAccounting => (BodyZone::BeginBlock, 0),
            Self::LateFinalizeCredits => (BodyZone::BeginBlock, 1),
            Self::BoundaryOutcome => (BodyZone::BeginBlock, 2),
            Self::TeeBootstrap => (BodyZone::BeginBlock, 3),
            Self::ValidatorLiveness => (BodyZone::BeginBlock, 4),
            Self::RadiclePublication => (BodyZone::BeginBlock, 5),
            Self::Vote => (BodyZone::BeginBlock, 6),
            Self::Update => (BodyZone::BeginBlock, 7),
            Self::EnclaveUpgradeDeadline => (BodyZone::BeginBlock, 8),
            Self::Rewards => (BodyZone::BeginBlock, 9),
            Self::Staking => (BodyZone::BeginBlock, 10),
            Self::Oracle => (BodyZone::BeginBlock, 11),
            Self::OracleSlash => (BodyZone::BeginBlock, 12),
            Self::HyperlaneSlash => (BodyZone::BeginBlock, 13),
            Self::CycleTick => (BodyZone::BeginBlock, 14),
            Self::NodFactory => (BodyZone::EndBlock, 0),
            Self::GemFactory => (BodyZone::EndBlock, 1),
            Self::IntexFactory => (BodyZone::EndBlock, 2),
            Self::CredisFactory => (BodyZone::EndBlock, 3),
            Self::GemCleanup => (BodyZone::EndBlock, 4),
        }
    }

    pub const fn body_zone(self) -> BodyZone {
        self.zone_position().0
    }

    pub const fn begin_order(self) -> Option<u8> {
        match self.zone_position() {
            (BodyZone::BeginBlock, order) => Some(order),
            (BodyZone::EndBlock, _) => None,
        }
    }

    pub const fn end_order(self) -> Option<u8> {
        match self.zone_position() {
            (BodyZone::EndBlock, order) => Some(order),
            (BodyZone::BeginBlock, _) => None,
        }
    }

    pub const fn nonce_ordinal(self) -> Option<u8> {
        match self.end_order() {
            Some(order) => Some(NOD_FACTORY_ORDINAL + order),
            None => None,
        }
    }

    /// Decision 4A:
    /// - `true` for all 6 `CONSENSUS_ORDER` kinds + `Vote`, `Update`,
    ///   `EnclaveUpgradeDeadline`, `Rewards`, `Staking`, `Oracle`, `CycleTick`.
    /// - `false` for `OracleSlash`, `HyperlaneSlash`, and all 5 `END_BLOCK_ORDER` kinds.
    pub const fn revert_fails_block(self) -> bool {
        !matches!(
            self,
            Self::OracleSlash
                | Self::HyperlaneSlash
                | Self::NodFactory
                | Self::GemFactory
                | Self::IntexFactory
                | Self::CredisFactory
                | Self::GemCleanup
        )
    }

    pub const fn order_in(self, zone: BodyZone) -> Option<u8> {
        match zone {
            BodyZone::BeginBlock => self.begin_order(),
            BodyZone::EndBlock => self.end_order(),
        }
    }

    /// Converts a payload-free `SystemTxKind` into its corresponding [`SystemTx`].
    pub fn into_empty_system_tx(self) -> Option<SystemTx> {
        if matches!(self, Self::LateFinalizeCredits) {
            return None;
        }
        SystemTx::decode_kind_body(self, &[]).ok()
    }
}

/// Versioned calldata body system transactions across the 4-stage block lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemTx {
    // Stage 1: CONSENSUS_ORDER
    CertifiedParentAccounting {
        metadata: CertifiedParentAccountingMetadata,
    },
    LateFinalizeCredits {
        artifact: LateFinalizeCreditsArtifact,
    },
    BoundaryOutcome {
        artifact: DkgBoundaryArtifact,
    },
    TeeBootstrap {
        payload: crate::tee_bootstrap_v2::TeeBootstrapV2,
    },
    ValidatorLiveness,
    RadiclePublication {
        batch: crate::radicle_publication::PublicationBatch,
    },
    // Stage 2: BEGIN_BLOCK_ORDER
    Vote,
    Update,
    EnclaveUpgradeDeadline,
    Rewards,
    Staking,
    Oracle,
    OracleSlash,
    HyperlaneSlash,
    CycleTick,
    // Stage 4: END_BLOCK_ORDER
    NodFactory,
    GemFactory,
    IntexFactory,
    CredisFactory,
    GemCleanup,
}

impl SystemTx {
    pub const fn kind(&self) -> SystemTxKind {
        match self {
            Self::CertifiedParentAccounting { .. } => SystemTxKind::CertifiedParentAccounting,
            Self::LateFinalizeCredits { .. } => SystemTxKind::LateFinalizeCredits,
            Self::BoundaryOutcome { .. } => SystemTxKind::BoundaryOutcome,
            Self::TeeBootstrap { .. } => SystemTxKind::TeeBootstrap,
            Self::ValidatorLiveness => SystemTxKind::ValidatorLiveness,
            Self::RadiclePublication { .. } => SystemTxKind::RadiclePublication,
            Self::Vote => SystemTxKind::Vote,
            Self::Update => SystemTxKind::Update,
            Self::EnclaveUpgradeDeadline => SystemTxKind::EnclaveUpgradeDeadline,
            Self::Rewards => SystemTxKind::Rewards,
            Self::Staking => SystemTxKind::Staking,
            Self::Oracle => SystemTxKind::Oracle,
            Self::OracleSlash => SystemTxKind::OracleSlash,
            Self::HyperlaneSlash => SystemTxKind::HyperlaneSlash,
            Self::CycleTick => SystemTxKind::CycleTick,
            Self::NodFactory => SystemTxKind::NodFactory,
            Self::GemFactory => SystemTxKind::GemFactory,
            Self::IntexFactory => SystemTxKind::IntexFactory,
            Self::CredisFactory => SystemTxKind::CredisFactory,
            Self::GemCleanup => SystemTxKind::GemCleanup,
        }
    }

    pub const fn name(&self) -> &'static str {
        self.kind().name()
    }

    pub const fn selector(&self) -> SysTxSelector {
        self.kind().selector()
    }

    pub const fn stage(&self) -> BlockExecutionStage {
        self.kind().stage()
    }

    pub const fn body_zone(&self) -> BodyZone {
        self.kind().body_zone()
    }

    /// Nonce ordinal. Begin-zone transactions use their body index. End-zone
    /// transactions use stable ordinals (`24..=28`) so begin growth cannot collide with them.
    pub const fn nonce_ordinal(&self) -> Option<u8> {
        self.kind().nonce_ordinal()
    }

    /// Decision 4A: `true` for consensus- and economic-critical phases; `false`
    /// for `OracleSlash`, `HyperlaneSlash`, and the 5 `END_BLOCK_ORDER` phases.
    pub const fn revert_fails_block(&self) -> bool {
        self.kind().revert_fails_block()
    }

    pub const fn begin_order(&self) -> Option<u8> {
        self.kind().begin_order()
    }

    pub const fn end_order(&self) -> Option<u8> {
        self.kind().end_order()
    }

    pub const fn order_in(&self, zone: BodyZone) -> Option<u8> {
        self.kind().order_in(zone)
    }

    /// Encode as `selector(4) || version(1) || canonical_body`.
    pub fn encode(&self) -> Result<Bytes, SystemTxError> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.selector());
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
            Self::LateFinalizeCredits { artifact } => {
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
                crate::tee_bootstrap_v2::codec::encode_canonical(payload)
                    .map_err(SystemTxError::codec)?
                    .as_ref(),
            ),
            Self::RadiclePublication { batch } => {
                out.extend_from_slice(&batch.to_bytes().map_err(SystemTxError::from_precompile)?);
            }
            Self::ValidatorLiveness => {}
            Self::Vote => {}
            Self::Update => {}
            Self::EnclaveUpgradeDeadline => {}
            Self::Rewards => {}
            Self::Staking => {}
            Self::Oracle => {}
            Self::OracleSlash => {}
            Self::HyperlaneSlash => {}
            Self::CycleTick => {}
            Self::NodFactory => {}
            Self::GemFactory => {}
            Self::IntexFactory => {}
            Self::CredisFactory => {}
            Self::GemCleanup => {}
        }
        Ok(Bytes::from(out))
    }

    pub fn decode(data: &[u8]) -> Result<Self, SystemTxError> {
        if data.len() < 5 {
            return Err(SystemTxError::InputTooShort { len: data.len() });
        }
        let selector = selector_from_input(data)?;
        let Some(kind) = SystemTxKind::from_selector(selector) else {
            return Err(SystemTxError::UnknownSelector(selector));
        };
        let version = data[4];
        if version != SYSTEM_TX_INPUT_VERSION {
            return Err(SystemTxError::UnsupportedVersion(version));
        }
        Self::decode_kind_body(kind, &data[5..])
    }

    fn decode_kind_body(kind: SystemTxKind, body: &[u8]) -> Result<Self, SystemTxError> {
        match kind {
            SystemTxKind::CertifiedParentAccounting => Ok(Self::CertifiedParentAccounting {
                metadata: CertifiedParentAccountingMetadata::decode(body)
                    .map_err(SystemTxError::from_precompile)?,
            }),
            SystemTxKind::LateFinalizeCredits => Ok(Self::LateFinalizeCredits {
                artifact: decode_late_finalize_credits_artifact(body)
                    .map_err(SystemTxError::from_precompile)?
                    .unwrap_or_default(),
            }),
            SystemTxKind::BoundaryOutcome => {
                let Some(artifact) =
                    decode_boundary_artifact(body).map_err(SystemTxError::from_precompile)?
                else {
                    return Err(SystemTxError::MissingBoundaryOutcomeBody);
                };
                Ok(Self::BoundaryOutcome { artifact })
            }
            SystemTxKind::TeeBootstrap => Ok(Self::TeeBootstrap {
                payload: crate::tee_bootstrap_v2::codec::decode_canonical(body)
                    .map_err(SystemTxError::codec)?,
            }),
            SystemTxKind::RadiclePublication => Ok(Self::RadiclePublication {
                batch: crate::radicle_publication::PublicationBatch::from_bytes(body)
                    .map_err(SystemTxError::from_precompile)?,
            }),
            SystemTxKind::ValidatorLiveness => {
                Self::decode_empty_body(body, Self::ValidatorLiveness)
            }
            SystemTxKind::Vote => Self::decode_empty_body(body, Self::Vote),
            SystemTxKind::Update => Self::decode_empty_body(body, Self::Update),
            SystemTxKind::EnclaveUpgradeDeadline => {
                Self::decode_empty_body(body, Self::EnclaveUpgradeDeadline)
            }
            SystemTxKind::Rewards => Self::decode_empty_body(body, Self::Rewards),
            SystemTxKind::Staking => Self::decode_empty_body(body, Self::Staking),
            SystemTxKind::Oracle => Self::decode_empty_body(body, Self::Oracle),
            SystemTxKind::OracleSlash => Self::decode_empty_body(body, Self::OracleSlash),
            SystemTxKind::HyperlaneSlash => Self::decode_empty_body(body, Self::HyperlaneSlash),
            SystemTxKind::CycleTick => Self::decode_empty_body(body, Self::CycleTick),
            SystemTxKind::NodFactory => Self::decode_empty_body(body, Self::NodFactory),
            SystemTxKind::GemFactory => Self::decode_empty_body(body, Self::GemFactory),
            SystemTxKind::IntexFactory => Self::decode_empty_body(body, Self::IntexFactory),
            SystemTxKind::CredisFactory => Self::decode_empty_body(body, Self::CredisFactory),
            SystemTxKind::GemCleanup => Self::decode_empty_body(body, Self::GemCleanup),
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
