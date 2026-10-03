//! Shared production settings for marshal and its immutable archives.
use super::*;
use commonware_storage::archive::immutable;

pub(super) enum ArchiveKind {
    Finalizations,
    Blocks,
}
impl ArchiveKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Finalizations => "finalizations",
            Self::Blocks => "blocks",
        }
    }
}
pub(super) fn archive_config<C>(
    partition_prefix: &str,
    kind: ArchiveKind,
    page_cache: &CacheRef,
    codec_config: C,
) -> Result<immutable::Config<C>> {
    let kind = kind.as_str();
    Ok(immutable::Config {
        metadata_partition: format!("{partition_prefix}-{kind}-metadata"),
        freezer_table_partition: format!("{partition_prefix}-{kind}-freezer-table"),
        freezer_table_initial_size: config::FREEZER_TABLE_INITIAL_SIZE,
        freezer_table_resize_frequency: config::FREEZER_TABLE_RESIZE_FREQUENCY,
        freezer_table_resize_chunk_size: config::FREEZER_TABLE_RESIZE_CHUNK_SIZE,
        freezer_key_partition: format!("{partition_prefix}-{kind}-freezer-key"),
        freezer_key_page_cache: page_cache.clone(),
        freezer_value_partition: format!("{partition_prefix}-{kind}-freezer-value"),
        freezer_value_target_size: config::FREEZER_VALUE_TARGET_SIZE,
        freezer_value_compression: config::FREEZER_VALUE_COMPRESSION,
        ordinal_partition: format!("{partition_prefix}-{kind}-ordinal"),
        items_per_section: nonzero_u64(
            config::IMMUTABLE_ITEMS_PER_SECTION,
            "IMMUTABLE_ITEMS_PER_SECTION",
        )?,
        codec_config,
        replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
        freezer_key_write_buffer: nonzero_usize(
            config::MARSHAL_WRITE_BUFFER,
            "MARSHAL_WRITE_BUFFER",
        )?,
        freezer_value_write_buffer: nonzero_usize(
            config::MARSHAL_WRITE_BUFFER,
            "MARSHAL_WRITE_BUFFER",
        )?,
        ordinal_write_buffer: nonzero_usize(config::MARSHAL_WRITE_BUFFER, "MARSHAL_WRITE_BUFFER")?,
    })
}

pub(super) struct MarshalSettings<'a> {
    pub(super) partition_prefix: &'a str,
    pub(super) page_cache: &'a CacheRef,
    pub(super) view_retention_timeout: u64,
}
pub(super) struct MarshalStart<ES> {
    pub(super) epocher: ES,
    pub(super) genesis: outbe_consensus::block::ConsensusBlock,
}
pub(super) fn marshal_config<ES: commonware_consensus::types::Epocher>(
    provider: HybridSchemeProvider<MinSig>,
    start: MarshalStart<ES>,
    settings: MarshalSettings<'_>,
) -> Result<
    commonware_consensus::marshal::Config<
        HybridSchemeProvider<MinSig>,
        ES,
        commonware_parallel::Sequential,
        outbe_consensus::block::ConsensusBlock,
        outbe_consensus::block::ConsensusBlock,
    >,
> {
    Ok(commonware_consensus::marshal::Config {
        provider,
        epocher: start.epocher,
        start: commonware_consensus::marshal::Start::Genesis(start.genesis),
        partition_prefix: settings.partition_prefix.to_owned(),
        mailbox_size: nonzero_usize(config::ENGINE_MAILBOX_SIZE, "ENGINE_MAILBOX_SIZE")?,
        view_retention: ViewDelta::new(settings.view_retention_timeout),
        prunable_items_per_section: nonzero_u64(
            config::PRUNABLE_ITEMS_PER_SECTION,
            "PRUNABLE_ITEMS_PER_SECTION",
        )?,
        page_cache: settings.page_cache.clone(),
        replay_buffer: nonzero_usize(config::MARSHAL_REPLAY_BUFFER, "MARSHAL_REPLAY_BUFFER")?,
        key_write_buffer: nonzero_usize(config::MARSHAL_WRITE_BUFFER, "MARSHAL_WRITE_BUFFER")?,
        value_write_buffer: nonzero_usize(config::MARSHAL_WRITE_BUFFER, "MARSHAL_WRITE_BUFFER")?,
        block_codec_config: (),
        max_repair: nonzero_usize(config::MAX_REPAIR, "MAX_REPAIR")?,
        max_pending_acks: nonzero_usize(config::MAX_PENDING_ACKS, "MAX_PENDING_ACKS")?,
        strategy: commonware_parallel::Sequential,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_runtime::{deterministic, Runner as _};

    #[test]
    fn production_archives_preserve_partition_names_and_storage_settings() {
        deterministic::Runner::default().start(|ctx| async move {
            let cache = CacheRef::from_pooler(
                &ctx,
                NonZeroU16::new(4096).unwrap(),
                NonZeroUsize::new(4).unwrap(),
            );
            for (kind, expected_prefix) in [
                (ArchiveKind::Finalizations, "outbe-marshal-finalizations"),
                (ArchiveKind::Blocks, "outbe-marshal-blocks"),
            ] {
                let settings = archive_config("outbe-marshal", kind, &cache, 17u8).unwrap();
                assert_eq!(
                    settings.metadata_partition,
                    format!("{expected_prefix}-metadata")
                );
                assert_eq!(
                    settings.freezer_table_partition,
                    format!("{expected_prefix}-freezer-table")
                );
                assert_eq!(
                    settings.freezer_key_partition,
                    format!("{expected_prefix}-freezer-key")
                );
                assert_eq!(
                    settings.freezer_value_partition,
                    format!("{expected_prefix}-freezer-value")
                );
                assert_eq!(
                    settings.ordinal_partition,
                    format!("{expected_prefix}-ordinal")
                );
                // Fixed baseline values keep this independent of the production builder.
                assert_eq!(settings.freezer_table_initial_size, 2_097_152);
                assert_eq!(settings.freezer_table_resize_frequency, 4);
                assert_eq!(settings.freezer_table_resize_chunk_size, 65_536);
                assert_eq!(settings.freezer_value_target_size, 1_073_741_824);
                assert_eq!(settings.freezer_value_compression, Some(3));
                assert_eq!(settings.items_per_section.get(), 262_144);
                assert_eq!(settings.codec_config, 17);
                assert_eq!(settings.replay_buffer.get(), 8_388_608);
                assert_eq!(settings.freezer_key_write_buffer.get(), 1_048_576);
                assert_eq!(settings.freezer_value_write_buffer.get(), 1_048_576);
                assert_eq!(settings.ordinal_write_buffer.get(), 1_048_576);
            }
        });
    }
}
