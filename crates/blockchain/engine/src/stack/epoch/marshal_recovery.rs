//! Durable marshal archives expose the processed height before DKG selection.
use super::super::*;
use commonware_consensus::marshal;
use commonware_cryptography::certificate::Verifier as _;
use commonware_storage::archive::immutable;
use outbe_consensus::marshal_types::{MarshalActor, MarshalMailbox};

pub(super) struct RecoveredMarshal<
    E: BufferPooler + Clock + CryptoRng + Spawner + Storage + Metrics,
> {
    pub(super) actor: MarshalActor<E>,
    pub(super) mailbox: MarshalMailbox,
    pub(super) processed_height: Height,
}

pub(super) async fn recover_marshal<E>(
    ctx: &E,
    page_cache: &CacheRef,
    epoch_length_blocks: u32,
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

    let epocher = commonware_consensus::types::FixedEpocher::new(nonzero_u64(
        u64::from(epoch_length_blocks),
        "epochLengthBlocks",
    )?);
    let view_retention_timeout = u64::from(config::ACTIVITY_TIMEOUT)
        .checked_mul(config::VIEW_RETENTION_MULTIPLIER)
        .ok_or_else(|| eyre::eyre!("view retention timeout overflow"))?;

    let marshal_genesis_anchor = genesis_consensus_block(node)?;
    let (marshal_actor, marshal_mailbox, last_consensus_finalized_opt) =
        marshal::core::Actor::init(
            ctx.child("marshal"),
            finalizations_archive,
            blocks_archive,
            marshal_archive::marshal_config(
                certificate_scheme_provider.clone(),
                marshal_archive::MarshalStart {
                    epocher,
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
    })
}
