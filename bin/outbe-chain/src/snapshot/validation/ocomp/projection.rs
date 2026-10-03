//! projection obligations for the offline OCOMP audit.
use super::*;

pub(super) struct PresentRetainedCount(u64);
impl outbe_tribute::RetainedTributeAuditVisitor for PresentRetainedCount {
    fn visit_retained(
        &mut self,
        _: outbe_tribute::RetainedTributeAuditEntry,
    ) -> Result<(), outbe_compressed_entities::CeAuditError> {
        self.0 = self.0.checked_add(1).ok_or_else(|| {
            outbe_compressed_entities::CeAuditError::Invalid("retained body count overflow".into())
        })?;
        Ok(())
    }
}

pub(super) fn verify_present_projection_structure(
    layout: &crate::snapshot::config::RequestedLayout,
    scratch: &Path,
) -> eyre::Result<(u64, u64)> {
    use outbe_compressed_entities::{
        CeAuditLimits, CeAuditWork, CeDomain, IdPageRequest, MAX_ID_PAGE_LIMIT,
    };
    use outbe_nod::NodRepositoryReader;
    use outbe_offchain_storage::partitioned::adapters::RocksPartitionReadView;
    use outbe_offchain_storage::{PartitionedStorage, StorageReaderHandle};
    use outbe_tribute::{RetainedTributeReader, TributeRepositoryReader};
    let location = layout
        .projection
        .as_ref()
        .ok_or_else(|| Incomplete("missing selected OCOMP projection configuration".into()))?;
    let database = projection_database(&location.root);
    if !existing_file(&database.join("CURRENT"))? {
        return Err(Incomplete("missing selected OCOMP projection CURRENT".into()).into());
    }
    let secondary = tempfile::Builder::new()
        .prefix("ocomp-present-bodies-")
        .tempdir_in(scratch)?;
    let reader: StorageReaderHandle = std::sync::Arc::new(PartitionedStorage::read_only(
        std::sync::Arc::new(RocksPartitionReadView::open(
            &location.root,
            secondary.path(),
        )?),
        outbe_offchain_data::entity_partition_routing()?,
    ));
    let work = CeAuditWork::create(
        secondary.path().join("audit-work"),
        CeAuditLimits::default(),
    )?;
    let tribute = TributeRepositoryReader::new(reader.clone());
    let nod = NodRepositoryReader::new(reader.clone());
    tribute.audit_indexes(&work)?;
    nod.audit_indexes(&work)?;
    nod.audit_partition_locations(&work)?;
    let mut live = 0_u64;
    for domain in [CeDomain::Tribute, CeDomain::NodItem, CeDomain::NodBucket] {
        let mut after = None;
        loop {
            let request = IdPageRequest {
                after,
                limit: MAX_ID_PAGE_LIMIT,
            };
            let page = match domain {
                CeDomain::Tribute => tribute.scan_stored_bodies(request)?,
                CeDomain::NodItem => nod.scan_stored_items(request)?,
                CeDomain::NodBucket => nod.scan_stored_buckets(request)?,
            };
            increment_present(&mut live, u64::try_from(page.entries.len())?)?;
            after = page.next_after;
            if after.is_none() {
                break;
            }
        }
    }
    let mut retained = PresentRetainedCount(0);
    RetainedTributeReader::new(reader.clone()).audit_retained(&work, &mut retained)?;
    // Reader-owned repositories and scratch work drop before secondary cleanup.
    Ok((live, retained.0))
}
