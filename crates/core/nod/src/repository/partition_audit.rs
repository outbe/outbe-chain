//! Check physical ID placement and exact owner-index membership independently.
use super::*;
use crate::partitioning::item_scope;

impl NodRepositoryReader {
    /// Checks every physical primary against its ID-derived shard and the owner index.
    pub fn audit_partition_locations(&self, work: &CeAuditWork) -> Result<(), CeAuditError> {
        for entry in
            NodAuditEntries::new(&self.storage, NODS_NAMESPACE).map_err(index_audit_error)?
        {
            let entry = entry.map_err(index_audit_error)?;
            let id = parse_primary_key(entry.key.as_bytes()).map_err(index_audit_error)?;
            decode_item(id, entry.value.as_bytes()).map_err(index_audit_error)?;
            let correct = self
                .storage
                .get_record(
                    namespace(NODS_NAMESPACE)
                        .map_err(index_audit_error)?
                        .with_scope(item_scope(id).map_err(index_audit_error)?),
                    &entry.key,
                )
                .map_err(index_audit_error)?;
            if correct.as_ref().is_none_or(|record| {
                record.value != entry.value || record.metadata != entry.metadata
            }) {
                return Err(CeAuditError::Invalid(
                    "NOD primary is outside its ID partition".into(),
                ));
            }
        }
        self.audit_indexes(work)
    }
}
