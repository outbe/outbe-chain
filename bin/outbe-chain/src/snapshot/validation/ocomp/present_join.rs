//! present join obligations for the offline OCOMP audit.
use super::*;

pub(super) const PRESENT_RECEIPT: u8 = 1;
pub(super) const PRESENT_BINDING: u8 = 2;
pub(super) const PRESENT_INPUTS: u8 = 4;
pub(super) const PRESENT_ADMISSIONS: u8 = 8;
pub(super) const PRESENT_REFERENCES: u8 = 16;
pub(super) const PRESENT_ACK: u8 = 32;

/// Scratch-only population union. Native file paths never seed canonical inventory.
pub(super) struct PresentJobUnion {
    pub(super) db: DatabaseEnv,
    pub(super) _directory: tempfile::TempDir,
}

impl PresentJobUnion {
    pub(super) fn create(parent: &Path, protected: &ProtectedPaths) -> eyre::Result<Self> {
        validate_layout(&[], protected, &[parent.to_path_buf()])?;
        let directory = tempfile::Builder::new()
            .prefix("ocomp-present-")
            .tempdir_in(parent)?;
        let mut db = create_db(directory.path(), DatabaseArguments::default())?;
        db.create_and_track_tables_for::<InventoryRows>()?;
        Ok(Self {
            db,
            _directory: directory,
        })
    }

    pub(super) fn key(prefix: u8, job: B256) -> Vec<u8> {
        let mut key = vec![prefix];
        key.extend_from_slice(job.as_slice());
        key
    }

    pub(super) fn add(&self, job: B256, flag: u8) -> eyre::Result<()> {
        let key = Self::key(b'u', job);
        let tx = self.db.tx_mut()?;
        let previous = tx.get::<InventoryRows>(key.clone())?.map_or(0, |v| v[0]);
        tx.put::<InventoryRows>(key, vec![previous | flag])?;
        tx.commit()?;
        Ok(())
    }

    // d/l are provisional authorities emitted by discovery/local-result walkers.
    // Publish to a only when their enclosing native walk has succeeded.
    pub(super) fn save_job(&self, prefix: u8, job: &OcompJobRecordV1) -> eyre::Result<()> {
        let Some(finalized) = &job.finalized else {
            return Ok(());
        };
        let key = Self::key(prefix, finalized.job_id);
        let encoded = job.encode_canonical(&poc_schema_limits())?;
        let tx = self.db.tx_mut()?;
        if let Some(previous) = tx.get::<InventoryRows>(key.clone())? {
            ensure!(
                previous == encoded,
                "conflicting canonical authority for present job"
            );
        }
        tx.put::<InventoryRows>(key, encoded)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn visit_prefix(
        &self,
        prefix: &[u8],
        visitor: &mut impl FnMut(Vec<u8>, Vec<u8>) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        let mut next = prefix.to_vec();
        loop {
            // Release the read transaction before callback-side scratch writes.
            let row = {
                let tx = self.db.tx()?;
                let mut cursor = tx.cursor_read::<InventoryRows>()?;
                cursor.seek(next.clone())?
            };
            let Some((key, value)) = row else { break };
            if !key.starts_with(prefix) {
                break;
            }
            next = key.clone();
            next.push(0);
            visitor(key, value)?;
        }
        Ok(())
    }

    pub(super) fn publish_jobs(&self, prefix: u8) -> eyre::Result<()> {
        self.visit_prefix(&[prefix], &mut |_, bytes| {
            let job = OcompJobRecordV1::decode_canonical(&bytes, &poc_schema_limits())?;
            self.save_job(b'a', &job)
        })
    }

    pub(super) fn job(&self, id: B256) -> eyre::Result<Option<OcompJobRecordV1>> {
        self.db
            .tx()?
            .get::<InventoryRows>(Self::key(b'a', id))?
            .map(|bytes| {
                OcompJobRecordV1::decode_canonical(&bytes, &poc_schema_limits()).map_err(Into::into)
            })
            .transpose()
    }

    pub(super) fn save_evidence(&self, prefix: u8, job: B256, bytes: Vec<u8>) -> eyre::Result<()> {
        let key = Self::key(prefix, job);
        let tx = self.db.tx_mut()?;
        if let Some(previous) = tx.get::<InventoryRows>(key.clone())? {
            ensure!(
                previous == bytes,
                "conflicting surviving evidence for same job"
            );
        }
        tx.put::<InventoryRows>(key, bytes)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn publish_evidence(&self, from: u8, to: u8, flag: u8) -> eyre::Result<()> {
        self.visit_prefix(&[from], &mut |key, bytes| {
            ensure!(key.len() == 33, "invalid scratch evidence key");
            let job = B256::from_slice(&key[1..]);
            self.save_evidence(to, job, bytes)?;
            if flag != 0 {
                self.add(job, flag)?;
            }
            Ok(())
        })
    }

    pub(super) fn save_ack(
        &self,
        ack: &outbe_ocomp::discovery_spool::StoredDiscoveryAckV1,
    ) -> eyre::Result<()> {
        let mut bytes = ack.reference.encode_fixed();
        bytes.extend_from_slice(&ack.lease_generation.to_be_bytes());
        bytes.extend_from_slice(ack.manifest_hash.as_slice());
        bytes.extend_from_slice(&ack.committed.encode_body(&poc_schema_limits())?);
        self.save_evidence(b'k', ack.committed.job_id, bytes)
    }

    pub(super) fn ack(
        &self,
        job: B256,
    ) -> eyre::Result<Option<outbe_ocomp::discovery_spool::StoredDiscoveryAckV1>> {
        use outbe_ocomp::{
            discovery_control::DiscoveryAckRefV1, discovery_spool::StoredDiscoveryAckV1,
        };
        let fixed = DiscoveryAckRefV1::FIXED_BYTES;
        self.db
            .tx()?
            .get::<InventoryRows>(Self::key(b'c', job))?
            .map(|bytes| {
                ensure!(bytes.len() > fixed + 40, "invalid scratch discovery ACK");
                Ok(StoredDiscoveryAckV1 {
                    reference: DiscoveryAckRefV1::decode_fixed(&bytes[..fixed])?,
                    lease_generation: u64::from_be_bytes(bytes[fixed..fixed + 8].try_into()?),
                    manifest_hash: B256::from_slice(&bytes[fixed + 8..fixed + 40]),
                    committed: outbe_ocomp_protocol::SnapshotExportCommittedV1::decode_body(
                        &bytes[fixed + 40..],
                        &poc_schema_limits(),
                    )?,
                })
            })
            .transpose()
    }

    pub(super) fn save_result_binding(
        &self,
        job: B256,
        result: &outbe_ocomp_protocol::result::LysisResultV1,
    ) -> eyre::Result<()> {
        let mut bytes = result.input_manifest_hash.as_slice().to_vec();
        bytes.extend_from_slice(result.plan_hash.as_slice());
        self.save_evidence(b'm', job, bytes)
    }

    pub(super) fn result_binding(&self, job: B256) -> eyre::Result<Option<(B256, B256)>> {
        self.db
            .tx()?
            .get::<InventoryRows>(Self::key(b'v', job))?
            .map(|bytes| {
                ensure!(bytes.len() == 64, "invalid scratch local-result binding");
                Ok((
                    B256::from_slice(&bytes[..32]),
                    B256::from_slice(&bytes[32..]),
                ))
            })
            .transpose()
    }

    pub(super) fn save_export(
        &self,
        job: B256,
        export: outbe_node::ocomp::retention::ExportAuthorityV1,
    ) -> eyre::Result<()> {
        let mut bytes = export.source_generation.to_be_bytes().to_vec();
        bytes.extend_from_slice(&export.lease_generation.to_be_bytes());
        bytes.extend_from_slice(export.manifest_hash.as_slice());
        let tx = self.db.tx_mut()?;
        tx.put::<InventoryRows>(Self::key(b'e', job), bytes)?;
        tx.commit()?;
        Ok(())
    }

    pub(super) fn export(
        &self,
        job: B256,
    ) -> eyre::Result<Option<outbe_node::ocomp::retention::ExportAuthorityV1>> {
        self.db
            .tx()?
            .get::<InventoryRows>(Self::key(b'e', job))?
            .map(|bytes| {
                ensure!(bytes.len() == 48, "invalid scratch export authority");
                Ok(outbe_node::ocomp::retention::ExportAuthorityV1 {
                    source_generation: u64::from_be_bytes(bytes[..8].try_into()?),
                    lease_generation: u64::from_be_bytes(bytes[8..16].try_into()?),
                    manifest_hash: B256::from_slice(&bytes[16..]),
                })
            })
            .transpose()
    }

    pub(super) fn save_refs(
        &self,
        job: B256,
        ordinal: u32,
        refs: &[outbe_ocomp_protocol::CasObjectRefV1],
    ) -> eyre::Result<()> {
        self.add(job, PRESENT_REFERENCES)?;
        let tx = self.db.tx_mut()?;
        for (index, reference) in refs.iter().enumerate() {
            let mut key = Self::key(b'r', job);
            key.extend_from_slice(&ordinal.to_be_bytes());
            key.extend_from_slice(&u32::try_from(index)?.to_be_bytes());
            let mut bytes = reference.transport_digest.as_slice().to_vec();
            bytes.extend_from_slice(&reference.encoded_bytes.to_be_bytes());
            match reference.expected_ocb1_kind {
                None => bytes.push(0),
                Some(kind) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&kind.to_be_bytes());
                }
            }
            tx.put::<InventoryRows>(key, bytes)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(super) fn visit_refs(
        &self,
        job: B256,
        visitor: &mut impl FnMut(outbe_ocomp_protocol::CasObjectRefV1) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        self.visit_prefix(&Self::key(b'r', job), &mut |_, bytes| {
            ensure!(
                bytes.len() == 41 || bytes.len() == 43,
                "invalid scratch reference"
            );
            let expected_ocb1_kind = match bytes[40] {
                0 if bytes.len() == 41 => None,
                1 if bytes.len() == 43 => Some(u16::from_be_bytes(bytes[41..].try_into()?)),
                _ => eyre::bail!("invalid scratch reference kind"),
            };
            visitor(outbe_ocomp_protocol::CasObjectRefV1 {
                transport_digest: B256::from_slice(&bytes[..32]),
                encoded_bytes: u64::from_be_bytes(bytes[32..40].try_into()?),
                expected_ocb1_kind,
            })
        })
    }
}

#[derive(Default)]
pub(super) struct PresentJoinErrors {
    pub(super) first: Option<eyre::Report>,
    pub(super) failed: bool,
}
impl PresentJoinErrors {
    pub(super) fn observe<T>(&mut self, result: eyre::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                let error = present_join_error(error);
                let failed = error.downcast_ref::<Incomplete>().is_none();
                if self.first.is_none() || (failed && !self.failed) {
                    self.first = Some(error);
                }
                self.failed |= failed;
                None
            }
        }
    }
    pub(super) fn finish(self) -> eyre::Result<()> {
        self.first.map_or(Ok(()), Err)
    }
}
pub(super) fn present_join_error(error: eyre::Report) -> eyre::Report {
    if error.downcast_ref::<Incomplete>().is_some() {
        error
    } else if missing_native_input(error.as_ref()) {
        error.wrap_err(Incomplete(
            "missing evidence for present OCOMP relation".into(),
        ))
    } else {
        error
    }
}
pub(super) fn present_count(
    report: &mut super::super::report::ValidationReport,
    name: &str,
    count: u64,
) {
    report
        .inventory_bounds
        .push(super::super::report::InventoryBounds {
            name: name.into(),
            start: 0,
            end_exclusive: count,
            visited: count,
        });
}
pub(super) fn existing_directory(path: &Path) -> eyre::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            ensure!(
                metadata.is_dir(),
                "not a native directory: {}",
                path.display()
            );
            Ok(true)
        }
    }
}
pub(super) fn existing_file(path: &Path) -> eyre::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            ensure!(metadata.is_file(), "not a native file: {}", path.display());
            Ok(true)
        }
    }
}
pub(super) fn directory_has_entries(path: &Path) -> eyre::Result<bool> {
    if !existing_directory(path)? {
        return Ok(false);
    }
    Ok(std::fs::read_dir(path)?.next().transpose()?.is_some())
}

pub(super) fn scan_present_jobs(
    root: &Path,
    flag: u8,
    work: &PresentJobUnion,
) -> eyre::Result<u64> {
    if !existing_directory(root)? {
        return Ok(0);
    }
    let mut count = 0_u64;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_dir(),
            "present job locator is not a directory"
        );
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| eyre::eyre!("invalid present job locator"))?;
        // The exporter retains inventory/opening work beside published input catalogs.
        if flag == PRESENT_INPUTS && name == ".work" {
            continue;
        }
        let mut bytes = [0; 32];
        hex::decode_to_slice(name, &mut bytes)?;
        let job = B256::from(bytes);
        ensure!(
            !job.is_zero() && name == hex::encode(job),
            "noncanonical present job locator"
        );
        let path = if flag == PRESENT_ADMISSIONS {
            entry.path().join("admissions")
        } else {
            entry.path()
        };
        // Empty directories do not fabricate a complete local stage.
        if directory_has_entries(&path)? {
            work.add(job, flag)?;
        }
        count = count
            .checked_add(1)
            .ok_or_else(|| eyre::eyre!("present job count overflow"))?;
    }
    Ok(count)
}
