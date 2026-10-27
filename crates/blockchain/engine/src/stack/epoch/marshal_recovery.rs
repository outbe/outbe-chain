//! Durable marshal archives expose the processed height before DKG selection.
use super::super::*;
use commonware_consensus::marshal;
use commonware_cryptography::certificate::Verifier as _;
use commonware_storage::archive::immutable;
use outbe_consensus::{
    follow::FollowerEpocher,
    marshal_types::{MarshalActor, MarshalMailbox},
};

pub(super) struct RecoveredMarshal<
    E: BufferPooler + Clock + CryptoRng + Spawner + Storage + Metrics,
> {
    pub(super) actor: MarshalActor<E>,
    pub(super) mailbox: MarshalMailbox,
    pub(super) processed_height: Height,
    /// Shared with the epoch supervisor and the finalization actor. Both
    /// record later boundaries in it.
    pub(super) epocher: FollowerEpocher,
}

/// Build the validator marshal epocher from the latest finalized
/// `BoundaryOutcome` in the local canonical chain.
///
/// `latest_boundary` is `(carrier_height, epoch)`: the height of the block
/// that carries the artifact and the epoch that the artifact activates. An
/// epoch above zero anchors the epocher at its carrier. Heights below that
/// anchor stay unknown to the epocher. Epoch zero (or no boundary) anchors
/// epoch zero at height one.
pub(in crate::stack) fn validator_marshal_epocher(
    latest_boundary: Option<(u64, u64)>,
    dkg_rotation_params: DkgRotationParams,
) -> FollowerEpocher {
    let length = dkg_rotation_params.epoch_length_blocks;
    let grace = dkg_rotation_params.activation_grace_blocks;
    match latest_boundary {
        Some((carrier_height, epoch)) if epoch > 0 && carrier_height > 0 => {
            FollowerEpocher::from_anchor(
                length,
                grace,
                Epoch::new(epoch),
                Height::new(carrier_height),
            )
        }
        _ => FollowerEpocher::new(length, grace),
    }
}

pub(super) async fn recover_marshal<E>(
    ctx: &E,
    page_cache: &CacheRef,
    dkg_rotation_params: DkgRotationParams,
    node: &OutbeFullNode,
    certificate_scheme_provider: &HybridSchemeProvider<MinSig>,
) -> Result<RecoveredMarshal<E>>
where
    E: BufferPooler + Clock + CryptoRng + Spawner + Storage + Metrics + Send + Sync + 'static,
{
    let partition_prefix = "outbe-marshal".to_string();

    let finalizations_archive = immutable::Archive::init(
        ctx.child("marshal_finalizations"),
        marshal_archive::archive_config(
            &partition_prefix,
            marshal_archive::ArchiveKind::Finalizations,
            page_cache,
            HybridScheme::<MinSig>::certificate_codec_config_unbounded(),
        )?,
    )
    .await
    .wrap_err("failed to initialize finalizations archive")?;

    let blocks_archive = immutable::Archive::init(
        ctx.child("marshal_blocks"),
        marshal_archive::archive_config(
            &partition_prefix,
            marshal_archive::ArchiveKind::Blocks,
            page_cache,
            (),
        )?,
    )
    .await
    .wrap_err("failed to initialize blocks archive")?;

    let last_execution_height = node
        .provider
        .last_block_number()
        .wrap_err("failed to read execution height for the marshal epocher")?;
    let latest_boundary = recover_latest_boundary_artifact(
        &node.provider,
        last_execution_height,
        dkg_rotation_params,
    )
    .wrap_err("failed to recover the latest boundary for the marshal epocher")?
    .map(|(carrier_height, artifact)| (carrier_height, artifact.epoch));
    let epocher = validator_marshal_epocher(latest_boundary, dkg_rotation_params);
    info!(
        ?latest_boundary,
        "validator marshal epocher anchored on the latest finalized boundary"
    );
    let view_retention_timeout = u64::from(config::ACTIVITY_TIMEOUT)
        .checked_mul(config::VIEW_RETENTION_MULTIPLIER)
        .ok_or(crate::errors::StackEpochError::ViewRetentionTimeoutOverflow)?;

    let marshal_genesis_anchor = genesis_consensus_block(node)?;
    let (marshal_actor, marshal_mailbox, last_consensus_finalized_opt) =
        marshal::core::Actor::init(
            ctx.child("marshal"),
            finalizations_archive,
            blocks_archive,
            marshal_archive::marshal_config(
                certificate_scheme_provider.clone(),
                marshal_archive::MarshalStart {
                    epocher: epocher.clone(),
                    genesis: marshal_genesis_anchor,
                },
                marshal_archive::MarshalSettings {
                    partition_prefix: &partition_prefix,
                    page_cache,
                    view_retention_timeout,
                },
            )?,
        )
        .await;

    // commonware 2026.5.0: `Actor::init` now returns `Option<Height>`. `None`
    // means no durable consensus finalization yet (fresh genesis). Map that to
    // height 0. This keeps the prior non-optional `Height` semantics that the
    // genesis-formation proof, crash-recovery detection, and executor start use.
    let last_consensus_finalized = map_marshal_init_height(last_consensus_finalized_opt.height());

    info!(
        marshal_processed_height = last_consensus_finalized.get(),
        "marshal actor initialized; exact archive/Reth recovery reconciliation pending"
    );

    Ok(RecoveredMarshal {
        actor: marshal_actor,
        mailbox: marshal_mailbox,
        processed_height: last_consensus_finalized,
        epocher,
    })
}
