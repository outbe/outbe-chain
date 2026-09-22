//! Read-only projection structure and live body equality at actual saved frontiers.

use std::{path::Path, sync::Arc};

use outbe_compressed_entities::{
    CeAuditWork, CeBodyAudit, CeBodyAuditReport, CeDomain, FinalizedMarker, IdPageRequest,
    MAX_ID_PAGE_LIMIT,
};
use outbe_nod::NodRepositoryReader;
use outbe_offchain_data::{read_projection_state, ProjectionCheckpoint, ProjectionConfig};
use outbe_offchain_storage::{RocksDbReader, StorageReaderHandle};
use outbe_tribute::{RetainedTributeAuditVisitor, RetainedTributeReader, TributeRepositoryReader};

use super::Incomplete;
use crate::snapshot::config::RequestedLayout;

/// Successfully completed primary/index checks, with equality reported separately.
#[derive(Debug)]
pub(crate) struct ProjectionBodyReport {
    pub checkpoint: ProjectionCheckpoint,
    pub equality: Result<CeBodyAuditReport, Incomplete>,
}

/// One immutable projection view shared by state, bodies, indexes and retention.
pub(crate) struct ProjectionBodyView {
    // Fields drop in declaration order: release the database before its scratch.
    reader: StorageReaderHandle,
    checkpoint: ProjectionCheckpoint,
    _scratch: tempfile::TempDir,
}

impl ProjectionBodyView {
    pub(crate) fn open(layout: &RequestedLayout, scratch_parent: &Path) -> eyre::Result<Self> {
        let projection = layout
            .projection
            .as_ref()
            .ok_or_else(|| Incomplete("missing projection configuration".into()))?;
        let mut protected = layout.protected.clone();
        protected.0.extend([
            layout.chain_root.clone(),
            layout.consensus_root.clone(),
            layout.ocomp_root.clone(),
            layout.static_files_root.clone(),
            layout.execution_rocksdb_root.clone(),
            projection.root.clone(),
        ]);
        outbe_snapshot::layout::validate_layout(&[], &protected, &[scratch_parent.to_path_buf()])?;
        match std::fs::metadata(projection.root.join("CURRENT")) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(
                    Incomplete("missing projection database or CURRENT file".into()).into(),
                );
            }
            Err(error) => return Err(error.into()),
        }
        let scratch = tempfile::Builder::new()
            .prefix("projection-audit-")
            .tempdir_in(scratch_parent)?;
        let reader: StorageReaderHandle =
            Arc::new(RocksDbReader::open(&projection.root, scratch.path())?);
        let checkpoint = read_projection_state(
            ProjectionConfig {
                chain_id: layout.chain.chain().id(),
                genesis_hash: layout.chain.genesis_hash(),
                start_block: projection.start_block,
            },
            reader.clone(),
        )?
        .and_then(|state| state.checkpoint)
        .ok_or_else(|| Incomplete("missing initialized projection checkpoint".into()))?;
        Ok(Self {
            reader,
            checkpoint,
            _scratch: scratch,
        })
    }

    /// `expected` is the completed CE audit leaf stream at `marker`. The caller
    /// retains the independent CE report even when live equality is incomplete.
    pub(crate) fn verify_bodies(
        &self,
        marker: &FinalizedMarker,
        mut expected: CeBodyAudit<'_>,
        work: &CeAuditWork,
    ) -> eyre::Result<ProjectionBodyReport> {
        let tribute = TributeRepositoryReader::new(self.reader.clone());
        let nod = NodRepositoryReader::new(self.reader.clone());
        tribute.audit_indexes(work)?;
        nod.audit_indexes(work)?;
        let same_frontier = marker.height == self.checkpoint.block_number
            && marker.block_hash == self.checkpoint.block_hash;
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
                if same_frontier {
                    for (id, body) in page.entries {
                        expected.push_body(domain, id, &body.encode())?;
                    }
                }
                after = page.next_after;
                if after.is_none() {
                    break;
                }
            }
        }
        let equality = if same_frontier {
            Ok(expected.finish()?)
        } else {
            Err(Incomplete(format!(
                "live body equality requires the same CE and projection identity: Q={} ({}) P={} ({})",
                marker.height, marker.block_hash,
                self.checkpoint.block_number, self.checkpoint.block_hash,
            )))
        };
        Ok(ProjectionBodyReport {
            checkpoint: self.checkpoint,
            equality,
        })
    }

    /// Retained rows are a separate population; historical lease/partition
    /// obligations are authenticated by the task06 caller, never by live CE.
    pub(crate) fn audit_retained(
        &self,
        work: &CeAuditWork,
        visitor: &mut impl RetainedTributeAuditVisitor,
    ) -> eyre::Result<()> {
        Ok(RetainedTributeReader::new(self.reader.clone()).audit_retained(work, visitor)?)
    }
}
