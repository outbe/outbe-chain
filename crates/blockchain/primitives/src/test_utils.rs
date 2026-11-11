//! Shared artifact construction for tests and protocol benchmarks.

pub mod sol_interface;
pub mod state_proof;

use crate::consensus::{DkgBoundaryArtifact, ReshareResult};
use crate::radicle_publication::{
    PublicationAction, PublicationAttestation, PublicationBatch, PublicationBody,
};
use alloy_primitives::{address, Bytes, FixedBytes, B256, U256};

pub fn sample_system_tx_boundary(block_number: u64) -> DkgBoundaryArtifact {
    DkgBoundaryArtifact {
        epoch: 8,
        dkg_cycle: 2,
        freeze_height: block_number - 2,
        planned_activation_height: block_number,
        target_set_hash: B256::repeat_byte(0x33),
        vrf_material_version: 3,
        vrf_group_public_key: B256::repeat_byte(0x44),
        vrf_group_public_key_bytes: Bytes::from_static(&[0x44u8; 96]),
        committee_set_hash: B256::repeat_byte(0x66),
        is_validator_set_change: true,
        outcome: Bytes::from_static(b"boundary"),
        is_full_dkg: false,
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set: vec![address!("0x3333333333333333333333333333333333333333")],
            active_set_hash: B256::repeat_byte(0x55),
        },
    }
}

/// Build call-breach test prices for days 100, 99, and earlier.
pub fn call_breach_prices(prices: &[Option<u64>]) -> Vec<(u32, Option<U256>)> {
    prices
        .iter()
        .enumerate()
        .map(|(back, price)| (100 - back as u32, price.map(U256::from)))
        .collect()
}

// Framing fixtures only; these bytes do not prove valid committee signatures.
pub fn sample_radicle_publication_body() -> PublicationBody {
    PublicationBody {
        chain_id: 0x0102030405060708,
        genesis_hash: B256::repeat_byte(1),
        repo_id: FixedBytes::repeat_byte(2),
        descriptor_hash: B256::repeat_byte(3),
        owner_bls_public: FixedBytes::repeat_byte(4),
        players_commitment: B256::repeat_byte(5),
        epoch: 0x1112131415161718,
        material_version: 0x2122232425262728,
        material_hash: B256::repeat_byte(6),
        finalized_anchor_height: 0x3132333435363738,
        finalized_anchor_hash: B256::repeat_byte(7),
        sequence: 0x4142434445464748,
        previous_receipt_hash: B256::repeat_byte(8),
        action: PublicationAction::Review,
        proposal_id: U256::from_be_bytes([12; 32]),
        patch_id: FixedBytes::repeat_byte(13),
        revision_id: FixedBytes::repeat_byte(14),
        expected_target_head: FixedBytes::repeat_byte(15),
        package_manifest_hash: B256::repeat_byte(16),
    }
}

pub fn sample_radicle_publication_batch(count: usize) -> PublicationBatch {
    PublicationBatch {
        body: sample_radicle_publication_body(),
        attestations: (0..count)
            .map(|index| PublicationAttestation {
                participant_index: index as u32,
                signature: FixedBytes::repeat_byte(index as u8 + 1),
            })
            .collect(),
    }
}

use crate::{
    block::{begin_step, consensus_step, end_step, BlockOrder, ProtocolModule},
    reshare_artifact::{
        encode_boundary_artifact, encode_late_finalize_credits_artifact, ConsensusHeaderArtifact,
    },
    system_tx::binding::{
        validate_boundary_outcome_payload, validate_late_finalize_credits_payload,
        validate_parent_accounting_payload, validate_publication_payload,
        validate_tee_bootstrap_payload,
    },
    system_tx_context::{
        SystemTxBuildContext, SystemTxPayloadBuildError, SystemTxPayloadValidationError,
        SystemTxValidationContext,
    },
};

struct ParentAccounting;
impl ProtocolModule for ParentAccounting {
    const NAME: &'static str = "certified_parent_accounting";
    fn build_payload(ctx: &SystemTxBuildContext<'_>) -> Result<Bytes, SystemTxPayloadBuildError> {
        let metadata = ctx.parent_consensus_metadata.ok_or(
            SystemTxPayloadBuildError::MissingParentConsensusMetadata {
                block_number: ctx.block_number,
            },
        )?;
        if metadata.finalized_block_hash != ctx.parent_hash {
            return Err(
                SystemTxPayloadBuildError::CertifiedParentAccountingHashMismatch {
                    parent_hash: ctx.parent_hash,
                    metadata_hash: metadata.finalized_block_hash,
                },
            );
        }
        Ok(metadata.encode()?)
    }
    fn validate_payload(
        payload: &[u8],
        ctx: &SystemTxValidationContext<'_>,
    ) -> Result<(), SystemTxPayloadValidationError> {
        validate_parent_accounting_payload(payload, ctx.parent_hash)?;
        Ok(())
    }
}
struct LateCredits;
impl ProtocolModule for LateCredits {
    const NAME: &'static str = "late_finalize_credits";
    fn build_payload(ctx: &SystemTxBuildContext<'_>) -> Result<Bytes, SystemTxPayloadBuildError> {
        Ok(encode_late_finalize_credits_artifact(
            &ctx.artifacts
                .late_finalize_credits
                .clone()
                .unwrap_or_default(),
        )?)
    }
    fn validate_payload(
        payload: &[u8],
        ctx: &SystemTxValidationContext<'_>,
    ) -> Result<(), SystemTxPayloadValidationError> {
        validate_late_finalize_credits_payload(payload, ctx.artifacts)?;
        Ok(())
    }
}
struct BoundaryOutcome;
impl ProtocolModule for BoundaryOutcome {
    const NAME: &'static str = "boundary_outcome";
    fn build_payload(ctx: &SystemTxBuildContext<'_>) -> Result<Bytes, SystemTxPayloadBuildError> {
        let Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact)) =
            &ctx.artifacts.consensus_header_artifact
        else {
            return Err(SystemTxPayloadBuildError::MissingBlock1BoundaryOutcome);
        };
        Ok(encode_boundary_artifact(artifact)?)
    }
    fn validate_payload(
        payload: &[u8],
        ctx: &SystemTxValidationContext<'_>,
    ) -> Result<(), SystemTxPayloadValidationError> {
        validate_boundary_outcome_payload(payload, ctx.artifacts)?;
        Ok(())
    }
}
struct TeeBootstrap;
impl ProtocolModule for TeeBootstrap {
    const NAME: &'static str = "tee_bootstrap";
    fn build_payload(ctx: &SystemTxBuildContext<'_>) -> Result<Bytes, SystemTxPayloadBuildError> {
        let payload = ctx
            .pending_tee_bootstrap
            .ok_or(SystemTxPayloadBuildError::MissingBlock1TeeBootstrapPayload)?;
        if ctx
            .expected_tee_policy
            .is_some_and(|expected| expected != &payload.authority.policy)
        {
            return Err(SystemTxPayloadBuildError::Block1TeeBootstrapPolicyScheduleMismatch);
        }
        payload.preflight()?;
        Ok(crate::tee_bootstrap_v2::codec::encode_canonical(payload)?)
    }
    fn validate_payload(
        payload: &[u8],
        ctx: &SystemTxValidationContext<'_>,
    ) -> Result<(), SystemTxPayloadValidationError> {
        validate_tee_bootstrap_payload(payload, ctx.block_number)?;
        Ok(())
    }
}
struct RadiclePublication;
impl ProtocolModule for RadiclePublication {
    const NAME: &'static str = "radicle_publication";
    fn build_payload(ctx: &SystemTxBuildContext<'_>) -> Result<Bytes, SystemTxPayloadBuildError> {
        let batch = ctx
            .artifacts
            .radicle_publication
            .as_ref()
            .ok_or(SystemTxPayloadBuildError::MissingPublicationBatch)?;
        Ok(Bytes::from(batch.to_bytes()?))
    }
    fn validate_payload(
        payload: &[u8],
        ctx: &SystemTxValidationContext<'_>,
    ) -> Result<(), SystemTxPayloadValidationError> {
        validate_publication_payload(payload, ctx.artifacts)?;
        Ok(())
    }
}

macro_rules! empty_module {
    ($($module:ident => $name:literal),* $(,)?) => { $(
        struct $module;
        impl ProtocolModule for $module { const NAME: &'static str = $name; }
    )* };
}
empty_module!(ValidatorLiveness => "validator_liveness", Vote => "vote", Update => "update", EnclaveUpgradeDeadline => "enclave_upgrade_deadline", Rewards => "rewards", Staking => "staking", Oracle => "oracle", OracleSlash => "oracle_slash", HyperlaneSlash => "hyperlane_slash", NodFactory => "nod_factory", GemFactory => "gem_factory", IntexFactory => "intex_factory", CredisFactory => "credis_factory", GemCleanup => "gem_cleanup");
struct CycleTick;
impl ProtocolModule for CycleTick {
    const NAME: &'static str = "cycle_tick";
    const NEEDS_CYCLE_ACTIVE_UTC_DAY: bool = true;
}

/// Isolated lower-layer tests use real payload codecs and header/body binding checks.
pub const TEST_BLOCK_ORDER: BlockOrder = BlockOrder {
    consensus: &[
        consensus_step::<ParentAccounting>()
            .with_validators()
            .active_when(|regime, _| regime.has_parent_finalization_accounting()),
        consensus_step::<LateCredits>()
            .active_when(|regime, _| regime.has_parent_finalization_accounting()),
        consensus_step::<BoundaryOutcome>()
            .with_validators()
            .active_when(|regime, flags| {
                regime.requires_genesis_bootstrap_txs() || flags.has_boundary_outcome
            }),
        consensus_step::<TeeBootstrap>()
            .with_validators()
            .active_when(|regime, _| regime.requires_genesis_bootstrap_txs()),
        consensus_step::<ValidatorLiveness>()
            .with_validators()
            .active_when(|regime, _| regime.has_system_tx_prefix()),
        consensus_step::<RadiclePublication>().active_when(|_, flags| flags.has_publication),
    ],
    begin_block: &[
        begin_step::<Vote>().with_validators(),
        begin_step::<Update>(),
        begin_step::<EnclaveUpgradeDeadline>(),
        begin_step::<Rewards>(),
        begin_step::<Staking>(),
        begin_step::<Oracle>().with_validators(),
        begin_step::<OracleSlash>().soft_fail(),
        begin_step::<HyperlaneSlash>().soft_fail(),
        begin_step::<CycleTick>().with_validators(),
    ],
    end_block: &[
        end_step::<NodFactory>().with_process_ready_entitlements(),
        end_step::<GemFactory>().with_process_ready_entitlements(),
        end_step::<IntexFactory>().with_process_ready_entitlements(),
        end_step::<CredisFactory>().with_process_ready_entitlements(),
        end_step::<GemCleanup>().with_process_ready_entitlements(),
    ],
};
