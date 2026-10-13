//! Point-key routing with a domain-owned decoder and a numbered shard family.
use super::*;
use crate::PartitionId;

pub struct KeyShardRouting {
    pub subsystem: &'static str,
    pub family: &'static str,
    pub scope: fn(&Key) -> Result<StorageScope, StorageError>,
}
impl PartitionRouting for KeyShardRouting {
    fn point(
        &self,
        _: &Namespace,
        key: &Key,
        _: &dyn PartitionReadSource,
    ) -> Result<Option<ReadLocation>, StorageError> {
        Ok(Some(ReadLocation {
            scope: (self.scope)(key)?,
            require_present: false,
        }))
    }
    fn scan(
        &self,
        _: &Namespace,
        _: ScanRequest<'_>,
        source: &dyn PartitionReadSource,
    ) -> Result<Vec<StorageScope>, StorageError> {
        Ok(source.list_scopes(self.subsystem)?.into_iter().filter(|scope| {
            matches!(&scope.partition, PartitionId::Numbered { family, .. } if family == self.family)
        }).collect())
    }
}

/// Defines the fixed 256-shard body layout and its shared repository indexes.
#[macro_export]
macro_rules! body_partitioning {
    ($id:ty, $count:ident, $family_const:ident, $subsystem:literal, $family:literal, $primary:literal, [$($index:literal),+], $key_error:literal) => {
        pub const $count: u16 = 256;
        pub const $family_const: &str = $family;
        pub fn item_shard(id: $id) -> u32 {
            // Modulo 256 is the final byte of the canonical big-endian identity.
            u32::from(id.as_slice()[<$id>::len_bytes() - 1])
        }
        pub fn item_scope(id: $id) -> Result<$crate::StorageScope, $crate::StorageError> {
            $crate::StorageScope::numbered($subsystem, $family_const, item_shard(id))
        }
        pub fn register(registry: &mut $crate::partitioned::routing::RoutingRegistry) -> Result<(), $crate::StorageError> {
            let routing = $crate::partitioned::routing::KeyShardRouting {
                subsystem: $subsystem, family: $family_const,
                scope: |key| {
                    let id = <$id>::try_from(key.as_bytes()).map_err(|_| $crate::StorageError::Corruption($key_error.into()))?;
                    item_scope(id)
                },
            };
            registry.register($primary, std::sync::Arc::new(routing))?;
            let shared = std::sync::Arc::new($crate::partitioned::routing::SharedRouting($crate::StorageScope::shared($subsystem)?));
            $(registry.register($index, shared.clone())?;)+
            Ok(())
        }
    };
}
