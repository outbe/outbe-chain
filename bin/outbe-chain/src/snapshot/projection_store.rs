//! Locate the RocksDB directories inside an off-chain projection root.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use alloy_primitives::B256;
use outbe_compressed_entities::{
    body_commitment, CeAuditWork, CeDomain, IdPageRequest, ACTIVE_COMMITMENT_SCHEME,
    MAX_ID_PAGE_LIMIT,
};
use outbe_offchain_storage::{DayDirectory, RocksDbReader, StorageReaderHandle};
use outbe_primitives::time::WorldwideDay;
use outbe_tribute::{
    read_tribute_day_mark, RetainedTributeAuditEntry, RetainedTributeAuditVisitor,
    RetainedTributePin, RetainedTributeReader, RetainedTributeRef, TributeDayMark,
    TributeRepositoryReader,
};

/// Shared database directory for an off-chain root.
///
/// A day layout stores that database in `shared/`. A root that still has its
/// `CURRENT` file beside the day directories keeps the single database.
pub(crate) fn projection_database(root: &Path) -> PathBuf {
    let shared = root.join("shared");
    if shared.join("CURRENT").is_file() {
        shared
    } else {
        root.to_path_buf()
    }
}

/// Read-only secondaries over one off-chain root, each opened under `scratch`.
pub(crate) struct ProjectionDatabases {
    shared: StorageReaderHandle,
    live_tribute_days: Vec<(u32, StorageReaderHandle)>,
    retained_tribute_days: Vec<(RetainedTributePin, StorageReaderHandle)>,
    nod_days: Vec<StorageReaderHandle>,
}

impl ProjectionDatabases {
    /// The caller has found `CURRENT` in `projection_database(root)`. A Tribute day kept
    /// for a lease is retained, not live; a day marked for removal is left out.
    pub(crate) fn open(root: &Path, scratch: &Path) -> eyre::Result<Self> {
        let shared: StorageReaderHandle = Arc::new(RocksDbReader::open(
            &projection_database(root),
            &scratch.join("shared"),
        )?);
        let directory = DayDirectory::inspect(root);
        let mut live_tribute_days = Vec::new();
        let mut retained_tribute_days = Vec::new();
        for day in directory.list_tribute_days()? {
            let path = directory.tribute_day_path(day);
            if !path.join("CURRENT").is_file() {
                continue;
            }
            let lease = match read_tribute_day_mark(shared.as_ref(), day)? {
                None => None,
                Some(TributeDayMark::Retained(lease)) => Some(lease),
                Some(_) => continue,
            };
            let reader: StorageReaderHandle = Arc::new(RocksDbReader::open(
                &path,
                &scratch.join(format!("tribute-day-{day}")),
            )?);
            match lease {
                None => live_tribute_days.push((day, reader)),
                Some(input_lease_id) => retained_tribute_days.push((
                    RetainedTributePin {
                        input_lease_id,
                        worldwide_day: WorldwideDay::new(day),
                    },
                    reader,
                )),
            }
        }
        let mut nod_days = Vec::new();
        for day in directory.list_nod_days()? {
            let path = directory.nod_day_path(day);
            if path.join("CURRENT").is_file() {
                nod_days.push(Arc::new(RocksDbReader::open(
                    &path,
                    &scratch.join(format!("nod-day-{day}")),
                )?) as StorageReaderHandle);
            }
        }
        Ok(Self {
            shared,
            live_tribute_days,
            retained_tribute_days,
            nod_days,
        })
    }

    pub(crate) fn shared(&self) -> &StorageReaderHandle {
        &self.shared
    }

    /// The shared database, then every live Tribute day.
    pub(crate) fn live_tributes(&self) -> impl Iterator<Item = &StorageReaderHandle> {
        std::iter::once(&self.shared).chain(self.live_tribute_days.iter().map(|(_, day)| day))
    }

    /// The shared database, then every Nod day.
    pub(crate) fn nods(&self) -> impl Iterator<Item = &StorageReaderHandle> {
        std::iter::once(&self.shared).chain(&self.nod_days)
    }

    /// The databases that hold the live bodies of `domain`.
    pub(crate) fn live(&self, domain: CeDomain) -> Vec<&StorageReaderHandle> {
        match domain {
            CeDomain::Tribute => self.live_tributes().collect(),
            CeDomain::NodItem | CeDomain::NodBucket => self.nods().collect(),
        }
    }

    /// The database of one live or retained Tribute day.
    pub(crate) fn tribute_day(&self, day: WorldwideDay) -> Option<StorageReaderHandle> {
        let live = self
            .live_tribute_days
            .iter()
            .find(|(live, _)| *live == day.value())
            .map(|(_, reader)| reader);
        let retained = self
            .retained_tribute_days
            .iter()
            .find(|(pin, _)| pin.worldwide_day == day)
            .map(|(_, reader)| reader);
        live.or(retained).cloned()
    }

    /// Audits the retained namespaces of the shared database, then visits every body of a
    /// day kept for a lease after auditing that day's indexes.
    pub(crate) fn audit_retained(
        &self,
        work: &CeAuditWork,
        visitor: &mut impl RetainedTributeAuditVisitor,
    ) -> eyre::Result<()> {
        RetainedTributeReader::new(self.shared.clone()).audit_retained(work, visitor)?;
        for (pin, day) in &self.retained_tribute_days {
            let tributes = TributeRepositoryReader::new(day.clone());
            tributes.audit_indexes(work)?;
            let mut after = None;
            loop {
                let page = tributes.scan_stored_bodies(IdPageRequest {
                    after,
                    limit: MAX_ID_PAGE_LIMIT,
                })?;
                for (tribute_id, stored_body) in page.entries {
                    eyre::ensure!(
                        tribute_id.worldwide_day() == pin.worldwide_day,
                        "retained day {} holds Tribute {tribute_id}",
                        pin.worldwide_day.value()
                    );
                    let commitment = body_commitment(
                        ACTIVE_COMMITMENT_SCHEME,
                        stored_body.schema_version(),
                        tribute_id,
                        stored_body.payload(),
                    )?;
                    visitor.visit_retained(RetainedTributeAuditEntry {
                        pin: *pin,
                        reference: RetainedTributeRef {
                            tribute_id,
                            body_commitment: B256::from(*commitment.as_bytes()),
                        },
                        stored_body,
                    })?;
                }
                after = page.next_after;
                if after.is_none() {
                    break;
                }
            }
        }
        Ok(())
    }
}
