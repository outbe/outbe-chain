//! Independent primary, locator and physical partition completeness audit.
use super::*;
use crate::partitioning::{item_scope, owner_shard, NOD_LOCATIONS_NAMESPACE, NOD_SHARD_COUNT};

impl NodRepositoryReader {
    /// Checks every physical primary against its owner-derived shard and every locator.
    /// Neither population is selected using the other, so missing indexes stay visible.
    pub fn audit_partition_locations(&self, work: &CeAuditWork) -> Result<(), CeAuditError> {
        let expected = NodAuditEntries::new(&self.storage, NODS_NAMESPACE)
            .map_err(index_audit_error)?
            .map(|entry| {
                let entry = entry.map_err(index_audit_error)?;
                let id = parse_primary_key(entry.key.as_bytes()).map_err(index_audit_error)?;
                let body = decode_item(id, entry.value.as_bytes()).map_err(index_audit_error)?;
                let scope = item_scope(body.owner).map_err(index_audit_error)?;
                let correct = self
                    .storage
                    .get_record(
                        namespace(NODS_NAMESPACE)
                            .map_err(index_audit_error)?
                            .with_scope(scope),
                        &entry.key,
                    )
                    .map_err(index_audit_error)?;
                if correct.as_ref().is_none_or(|record| {
                    record.value != entry.value || record.metadata != entry.metadata
                }) {
                    return Err(CeAuditError::Invalid(
                        "Nod primary is outside its owner partition".into(),
                    ));
                }
                Ok(location_record(
                    id,
                    owner_shard(body.owner).map_err(index_audit_error)?,
                ))
            });
        let actual = NodAuditEntries::new(&self.storage, NOD_LOCATIONS_NAMESPACE)
            .map_err(index_audit_error)?
            .map(|entry| {
                let entry = entry.map_err(index_audit_error)?;
                let id = parse_primary_key(entry.key.as_bytes()).map_err(index_audit_error)?;
                let bytes: [u8; 4] = entry
                    .value
                    .as_bytes()
                    .try_into()
                    .map_err(|_| CeAuditError::Invalid("invalid Nod location width".into()))?;
                let shard = u32::from_be_bytes(bytes);
                if shard >= u32::from(NOD_SHARD_COUNT) || entry.metadata.is_some() {
                    return Err(CeAuditError::Invalid("invalid Nod location".into()));
                }
                Ok(location_record(id, shard))
            });
        work.compare_records(PRIMARY_KEY_LEN, expected, actual)
    }
}
fn location_record(id: WwdEntityId, shard: u32) -> [u8; PRIMARY_KEY_LEN + 4] {
    let mut record = [0; PRIMARY_KEY_LEN + 4];
    record[..PRIMARY_KEY_LEN].copy_from_slice(id.as_slice());
    record[PRIMARY_KEY_LEN..].copy_from_slice(&shard.to_be_bytes());
    record
}
