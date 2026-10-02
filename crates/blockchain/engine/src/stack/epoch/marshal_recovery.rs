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
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-finalizations-metadata"),
            freezer_table_partition: format!("{partition_prefix}-finalizations-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-finalizations-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-finalizations-freezer-value"),
            freezer_value_target_size: config::FREEZER_VALUE_TARGET_SIZE,
            freezer_value_compression: config::FREEZER_VALUE_COMPRESSION,
            ordinal_partition: format!("{partition_prefix}-finalizations-ordinal"),
            items_per_section: nonzero_u64(
                config::IMMUTABLE_ITEMS_PER_SECTION,
                "IMMUTABLE_ITEMS_PER_SECTION",
            )?,
            codec_config: HybridScheme::<MinSig>::certificate_codec_config_unbounded(),
            replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
            freezer_key_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            freezer_value_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            ordinal_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
        },
    )
    .await
    .wrap_err("failed to initialize finalizations archive")?;

    let blocks_archive = immutable::Archive::init(
        ctx.child("marshal_blocks"),
        immutable::Config {
            metadata_partition: format!("{partition_prefix}-blocks-metadata"),
            freezer_table_partition: format!("{partition_prefix}-blocks-freezer-table"),
            freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
            freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
            freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
            freezer_key_partition: format!("{partition_prefix}-blocks-freezer-key"),
            freezer_key_page_cache: page_cache.clone(),
            freezer_value_partition: format!("{partition_prefix}-blocks-freezer-value"),
            freezer_value_target_size: config::FREEZER_VALUE_TARGET_SIZE,
            freezer_value_compression: config::FREEZER_VALUE_COMPRESSION,
            ordinal_partition: format!("{partition_prefix}-blocks-ordinal"),
            items_per_section: nonzero_u64(
                config::IMMUTABLE_ITEMS_PER_SECTION,
                "IMMUTABLE_ITEMS_PER_SECTION",
            )?,
            codec_config: (),
            replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
            freezer_key_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            freezer_value_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
            ordinal_write_buffer: nonzero_usize(
                config::MARSHAL_WRITE_BUFFER,
                "MARSHAL_WRITE_BUFFER",
            )?,
        },
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
            marshal::Config {
                provider: certificate_scheme_provider.clone(),
                epocher,
                start: marshal::Start::Genesis(marshal_genesis_anchor),
                partition_prefix: partition_prefix.clone(),
                mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
                view_retention: ViewDelta::new(view_retention_timeout),
                prunable_items_per_section: nonzero_u64(
                    config::PRUNABLE_ITEMS_PER_SECTION,
                    "PRUNABLE_ITEMS_PER_SECTION",
                )?,
                page_cache: page_cache.clone(),
                replay_buffer: nonzero_usize(
                    config::MARSHAL_REPLAY_BUFFER,
                    "MARSHAL_REPLAY_BUFFER",
                )?,
                key_write_buffer: nonzero_usize(
                    config::MARSHAL_WRITE_BUFFER,
                    "MARSHAL_WRITE_BUFFER",
                )?,
                value_write_buffer: nonzero_usize(
                    config::MARSHAL_WRITE_BUFFER,
                    "MARSHAL_WRITE_BUFFER",
                )?,
                block_codec_config: (),
                max_repair: nonzero_usize(config::MAX_REPAIR, "MAX_REPAIR")?,
                max_pending_acks: nonzero_usize(config::MAX_PENDING_ACKS, "MAX_PENDING_ACKS")?,
                strategy: commonware_parallel::Sequential,
            },
        )
        .await;

    // commonware 2026.5.0: `Actor::init` now returns `Option<Height>` - `None`
    // means no durable consensus finalization yet (fresh genesis). Map that to
    // height 0, preserving the prior non-optional `Height` semantics used by the
    // genesis-formation proof, crash-recovery detection, and executor start.
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
