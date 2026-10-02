//! Shared archive settings for deterministic marshal test harnesses.

use std::num::{NonZeroU64, NonZeroUsize};

use commonware_runtime::buffer::paged::CacheRef;
use commonware_storage::archive::immutable;

pub(crate) enum MarshalArchiveKind {
    Finalizations,
    Blocks,
}

impl MarshalArchiveKind {
    fn partition_suffix(&self) -> &'static str {
        match self {
            Self::Finalizations => "finalizations",
            Self::Blocks => "blocks",
        }
    }
}

/// Archive configuration inputs shared by one marshal harness.
pub(crate) struct MarshalArchiveFixture<'a> {
    pub(crate) partition_prefix: &'a str,
    pub(crate) page_cache: &'a CacheRef,
    pub(crate) items_per_section: NonZeroU64,
    pub(crate) replay_buffer: NonZeroUsize,
    pub(crate) write_buffer: NonZeroUsize,
}

impl MarshalArchiveFixture<'_> {
    pub(crate) fn config<C>(
        &self,
        kind: MarshalArchiveKind,
        codec_config: C,
    ) -> immutable::Config<C> {
        let prefix = self.partition_prefix;
        let suffix = kind.partition_suffix();
        immutable::Config {
            metadata_partition: format!("{prefix}-{suffix}-metadata"),
            freezer_table_partition: format!("{prefix}-{suffix}-freezer-table"),
            freezer_table_initial_size: 64,
            freezer_table_resize_frequency: 10,
            freezer_table_resize_chunk_size: 10,
            freezer_key_partition: format!("{prefix}-{suffix}-freezer-key"),
            freezer_key_page_cache: self.page_cache.clone(),
            freezer_value_partition: format!("{prefix}-{suffix}-freezer-value"),
            freezer_value_target_size: 1024,
            freezer_value_compression: None,
            ordinal_partition: format!("{prefix}-{suffix}-ordinal"),
            items_per_section: self.items_per_section,
            codec_config,
            replay_buffer: self.replay_buffer,
            freezer_key_write_buffer: self.write_buffer,
            freezer_value_write_buffer: self.write_buffer,
            ordinal_write_buffer: self.write_buffer,
        }
    }
}
