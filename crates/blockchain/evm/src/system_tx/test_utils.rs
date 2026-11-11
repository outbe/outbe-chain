use super::{begin_step, consensus_step, end_step, BlockOrder};
use alloy_primitives::Bytes;
use outbe_primitives::{
    block::ProtocolModule,
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
        Ok(outbe_primitives::tee_bootstrap_v2::codec::encode_canonical(
            payload,
        )?)
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
