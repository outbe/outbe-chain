//! Direct, restart-safe construction of proof-backed NOD materialization batches.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use alloy_primitives::B256;
use outbe_lysis::program_v1::{
    planner::{LysisPlanTopologyV1, PlannedUnitPositionV1, PRIMARY_WORK_SHARD_SIZE},
    result::{decode_root_reduce_output, LysisListSubtreeCarrierV1, RootReduceOutputV1},
};
use outbe_ocomp_protocol::{
    list::{leaf_hash, node_hash, pad_hash},
    nod_materialization::{NodMaterializationBatchV1, NodMaterializationHeadV1},
    result::NodActionV1,
    unit::UnitPhase,
    CasObjectRefV1, ListKind, ProtocolError,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    lysis_plan_audit::{ExactLysisPlanError, LocalLysisPlanAuditV1},
    lysis_result_catalog::{verified_result_chunk_at, LysisResultCatalogError},
};

mod proof;

const REFERENCE_VERSION: u16 = 1;
const REFERENCE_SUFFIX: &str = ".materialization-refs-v1.json";
const TEMP_SUFFIX: &str = ".tmp";
const MAX_REFERENCE_FILE_BYTES: u64 = 128 * 1024;
const MAX_DEPENDENCY_COUNT: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltNodMaterializationBatchV1 {
    pub batch: NodMaterializationBatchV1,
    pub dependencies: Vec<CasObjectRefV1>,
}

pub(crate) fn aligned_subtree_height(cursor: u32, maximum: u8) -> u8 {
    if cursor == 0 {
        maximum
    } else {
        maximum.min(cursor.trailing_zeros() as u8)
    }
}

pub fn build_nod_materialization_batch(
    audit: &LocalLysisPlanAuditV1<'_>,
    head: &NodMaterializationHeadV1,
    configured_subtree_height: u8,
) -> Result<NodMaterializationBatchV1, NodMaterializationBuildErrorV1> {
    Ok(
        build_nod_materialization_batch_with_references(audit, head, configured_subtree_height)?
            .batch,
    )
}

pub fn build_nod_materialization_batch_with_references(
    audit: &LocalLysisPlanAuditV1<'_>,
    head: &NodMaterializationHeadV1,
    configured_subtree_height: u8,
) -> Result<BuiltNodMaterializationBatchV1, NodMaterializationBuildErrorV1> {
    proof::require_head_authority(audit, head)?;
    let subtree = proof::MaterializationSubtree::new(head, configured_subtree_height)?;
    let page = proof::ActionPage::load(audit, head, &subtree)?;
    let tree = proof::PageTree::build(audit, head, &subtree, &page)?;
    let proof = proof::MaterializationProof::build(audit, &subtree, &page, &tree)?;

    let batch = NodMaterializationBatchV1 {
        queue_sequence: head.queue_sequence,
        first_nod_ordinal: head.next_nod_ordinal,
        actions: page.actions,
        root_path: proof.root_path,
    };
    outbe_ocomp_protocol::nod_materialization::verify_nod_materialization_batch(
        &batch,
        head,
        configured_subtree_height,
        audit.limits(),
    )?;
    Ok(BuiltNodMaterializationBatchV1 {
        batch,
        dependencies: proof.dependencies,
    })
}

fn require_action_ordinals(
    actions: &[NodActionV1],
    first: u32,
    worldwide_day: u32,
) -> Result<(), NodMaterializationBuildErrorV1> {
    for (offset, action) in actions.iter().enumerate() {
        let ordinal = first
            .checked_add(
                u32::try_from(offset).map_err(|_| ProtocolError::IntegerOverflow {
                    what: "materialization action offset",
                })?,
            )
            .ok_or(ProtocolError::IntegerOverflow {
                what: "materialization action ordinal",
            })?;
        if action.raw_ordinal != ordinal || action.wwd != worldwide_day {
            return Err(NodMaterializationBuildErrorV1::ActionOrder);
        }
    }
    Ok(())
}

pub(crate) fn normalize_dependencies(
    dependencies: &mut Vec<CasObjectRefV1>,
) -> Result<(), NodMaterializationBuildErrorV1> {
    dependencies.sort_by_key(|reference| reference.transport_digest);
    dependencies.dedup();
    if dependencies.is_empty()
        || dependencies.len() > MAX_DEPENDENCY_COUNT
        || dependencies
            .iter()
            .any(|reference| reference.transport_digest.is_zero() || reference.encoded_bytes == 0)
    {
        return Err(NodMaterializationBuildErrorV1::InvalidDependencies);
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MaterializationReferenceRecordV1 {
    version: u16,
    job_id: B256,
    dependencies: Vec<MaterializationReferenceV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MaterializationReferenceV1 {
    transport_digest: B256,
    encoded_bytes: u64,
    expected_ocb1_kind: Option<u16>,
}

impl From<CasObjectRefV1> for MaterializationReferenceV1 {
    fn from(reference: CasObjectRefV1) -> Self {
        Self {
            transport_digest: reference.transport_digest,
            encoded_bytes: reference.encoded_bytes,
            expected_ocb1_kind: reference.expected_ocb1_kind,
        }
    }
}

impl From<MaterializationReferenceV1> for CasObjectRefV1 {
    fn from(reference: MaterializationReferenceV1) -> Self {
        Self {
            transport_digest: reference.transport_digest,
            encoded_bytes: reference.encoded_bytes,
            expected_ocb1_kind: reference.expected_ocb1_kind,
        }
    }
}

pub struct MaterializationReferenceStoreV1 {
    root: PathBuf,
}

/// Existing public reference files, addressed by their native job/ordinal path.
/// Ordinals are retained locators, not assertions about the current chain cursor.
pub struct MaterializationReferenceReaderV1 {
    root: PathBuf,
}

impl MaterializationReferenceReaderV1 {
    pub fn open_existing(root: impl AsRef<Path>) -> Result<Self, MaterializationReferenceErrorV1> {
        let root = root.as_ref().to_path_buf();
        inspect_reference_directory(&root)?;
        Ok(Self { root })
    }

    pub fn load_exact(
        &self,
        job_id: B256,
        ordinal: u32,
    ) -> Result<Option<Vec<CasObjectRefV1>>, MaterializationReferenceErrorV1> {
        if job_id.is_zero() {
            return Err(MaterializationReferenceErrorV1::InvalidRecord);
        }
        inspect_reference_directory(&self.root)?;
        let job = self.root.join(hex::encode(job_id));
        let directory = job.join(ordinal.to_string());
        for path in [&job, &directory] {
            match inspect_reference_directory(path) {
                Err(MaterializationReferenceErrorV1::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    return Ok(None);
                }
                result => result?,
            }
        }
        // Reuse the owner's exact path and codec without its creating constructor.
        MaterializationReferenceStoreV1 { root: directory }.load_exact(job_id)
    }

    /// Stream every surviving nested record. Empty directories left by native
    /// release are valid. Observations are provisional until the walk succeeds.
    pub fn visit_references(
        &self,
        visitor: &mut impl FnMut(
            B256,
            u32,
            Vec<CasObjectRefV1>,
        ) -> Result<(), MaterializationReferenceErrorV1>,
    ) -> Result<(), MaterializationReferenceErrorV1> {
        inspect_reference_directory(&self.root)?;
        for job in fs::read_dir(&self.root).map_err(|source| io_error(&self.root, source))? {
            let job = job.map_err(|source| io_error(&self.root, source))?;
            visit_job_references(job, visitor)?;
        }
        Ok(())
    }
}

fn visit_job_references(
    job: fs::DirEntry,
    visitor: &mut impl FnMut(
        B256,
        u32,
        Vec<CasObjectRefV1>,
    ) -> Result<(), MaterializationReferenceErrorV1>,
) -> Result<(), MaterializationReferenceErrorV1> {
    let path = job.path();
    inspect_reference_directory(&path)?;
    let name = job.file_name();
    let name = name
        .to_str()
        .ok_or(MaterializationReferenceErrorV1::InvalidRecord)?;
    let mut bytes = [0; 32];
    hex::decode_to_slice(name, &mut bytes)
        .map_err(|_| MaterializationReferenceErrorV1::InvalidRecord)?;
    let job_id = B256::from(bytes);
    if job_id.is_zero() || name != hex::encode(job_id) {
        return Err(MaterializationReferenceErrorV1::InvalidRecord);
    }
    for ordinal in fs::read_dir(&path).map_err(|source| io_error(&path, source))? {
        let ordinal = ordinal.map_err(|source| io_error(&path, source))?;
        visit_ordinal_references(job_id, ordinal, visitor)?;
    }
    Ok(())
}

fn visit_ordinal_references(
    job_id: B256,
    ordinal: fs::DirEntry,
    visitor: &mut impl FnMut(
        B256,
        u32,
        Vec<CasObjectRefV1>,
    ) -> Result<(), MaterializationReferenceErrorV1>,
) -> Result<(), MaterializationReferenceErrorV1> {
    let directory = ordinal.path();
    inspect_reference_directory(&directory)?;
    let name = ordinal.file_name();
    let name = name
        .to_str()
        .ok_or(MaterializationReferenceErrorV1::InvalidRecord)?;
    let ordinal: u32 = name
        .parse()
        .map_err(|_| MaterializationReferenceErrorV1::InvalidRecord)?;
    if name != ordinal.to_string() {
        return Err(MaterializationReferenceErrorV1::InvalidRecord);
    }
    let codec = MaterializationReferenceStoreV1 {
        root: directory.clone(),
    };
    let expected = codec.path(job_id);
    for entry in fs::read_dir(&directory).map_err(|source| io_error(&directory, source))? {
        let path = entry.map_err(|source| io_error(&directory, source))?.path();
        if path != expected {
            return Err(
                if path.extension().is_some_and(|extension| extension == "tmp") {
                    MaterializationReferenceErrorV1::AmbiguousTemp(path)
                } else {
                    MaterializationReferenceErrorV1::InvalidRecord
                },
            );
        }
        let references = codec
            .load_exact(job_id)?
            .ok_or(MaterializationReferenceErrorV1::Missing)?;
        visitor(job_id, ordinal, references)?;
    }
    Ok(())
}

fn inspect_reference_directory(path: &Path) -> Result<(), MaterializationReferenceErrorV1> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.is_dir() {
        return Err(MaterializationReferenceErrorV1::UnsafePath(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

impl MaterializationReferenceStoreV1 {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, MaterializationReferenceErrorV1> {
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        reject_orphaned_temps(&root)?;
        Ok(Self { root })
    }

    pub fn pin_exact(
        &self,
        job_id: B256,
        dependencies: &[CasObjectRefV1],
    ) -> Result<(), MaterializationReferenceErrorV1> {
        if job_id.is_zero() {
            return Err(MaterializationReferenceErrorV1::InvalidRecord);
        }
        let mut dependencies = dependencies.to_vec();
        normalize_dependencies(&mut dependencies)
            .map_err(|_| MaterializationReferenceErrorV1::InvalidRecord)?;
        if let Some(existing) = self.load_exact(job_id)? {
            return if existing == dependencies {
                Ok(())
            } else {
                Err(MaterializationReferenceErrorV1::ConflictingReplay)
            };
        }
        let record = MaterializationReferenceRecordV1 {
            version: REFERENCE_VERSION,
            job_id,
            dependencies: dependencies.into_iter().map(Into::into).collect(),
        };
        persist_atomic(
            &self.root,
            &self.path(job_id),
            &serde_json::to_vec(&record)?,
        )
    }

    pub fn load_exact(
        &self,
        job_id: B256,
    ) -> Result<Option<Vec<CasObjectRefV1>>, MaterializationReferenceErrorV1> {
        let path = self.path(job_id);
        let bytes = match read_bounded(&path) {
            Ok(bytes) => bytes,
            Err(MaterializationReferenceErrorV1::Missing) => return Ok(None),
            Err(error) => return Err(error),
        };
        let record: MaterializationReferenceRecordV1 = serde_json::from_slice(&bytes)?;
        let mut dependencies = record
            .dependencies
            .iter()
            .cloned()
            .map(Into::into)
            .collect::<Vec<CasObjectRefV1>>();
        normalize_dependencies(&mut dependencies)
            .map_err(|_| MaterializationReferenceErrorV1::InvalidRecord)?;
        if record.version != REFERENCE_VERSION
            || record.job_id != job_id
            || dependencies
                != record
                    .dependencies
                    .into_iter()
                    .map(Into::into)
                    .collect::<Vec<CasObjectRefV1>>()
        {
            return Err(MaterializationReferenceErrorV1::InvalidRecord);
        }
        Ok(Some(dependencies))
    }

    pub fn release(&self, job_id: B256) -> Result<(), MaterializationReferenceErrorV1> {
        let path = self.path(job_id);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(io_error(&path, source)),
        }
    }

    fn path(&self, job_id: B256) -> PathBuf {
        self.root.join(format!(
            "{}{}",
            hex::encode(job_id.as_slice()),
            REFERENCE_SUFFIX
        ))
    }
}

fn create_private_directory(path: &Path) -> Result<(), MaterializationReferenceErrorV1> {
    fs::create_dir_all(path).map_err(|source| io_error(path, source))?;
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(MaterializationReferenceErrorV1::UnsafePath(
            path.to_path_buf(),
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error(path, source))
}

fn reject_orphaned_temps(path: &Path) -> Result<(), MaterializationReferenceErrorV1> {
    for entry in fs::read_dir(path).map_err(|source| io_error(path, source))? {
        let entry = entry.map_err(|source| io_error(path, source))?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(TEMP_SUFFIX))
        {
            return Err(MaterializationReferenceErrorV1::AmbiguousTemp(entry.path()));
        }
    }
    Ok(())
}

fn persist_atomic(
    root: &Path,
    target: &Path,
    bytes: &[u8],
) -> Result<(), MaterializationReferenceErrorV1> {
    if bytes.len() as u64 > MAX_REFERENCE_FILE_BYTES {
        return Err(MaterializationReferenceErrorV1::InvalidRecord);
    }
    let temp = target.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|source| io_error(&temp, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error(&temp, source))?;
    file.sync_all().map_err(|source| io_error(&temp, source))?;
    fs::rename(&temp, target).map_err(|source| io_error(target, source))?;
    sync_directory(root)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, MaterializationReferenceErrorV1> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                MaterializationReferenceErrorV1::Missing
            } else {
                io_error(path, source)
            }
        })?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_REFERENCE_FILE_BYTES {
        return Err(MaterializationReferenceErrorV1::UnsafePath(
            path.to_path_buf(),
        ));
    }
    read_reference_bytes(&mut file, path, metadata.len())
}

fn read_reference_bytes(
    reader: &mut impl std::io::Read,
    path: &Path,
    length: u64,
) -> Result<Vec<u8>, MaterializationReferenceErrorV1> {
    let limit = length
        .checked_add(1)
        .ok_or(MaterializationReferenceErrorV1::InvalidRecord)?;
    let mut bytes = Vec::with_capacity(length as usize);
    reader
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 != length {
        return Err(MaterializationReferenceErrorV1::InvalidRecord);
    }
    Ok(bytes)
}

fn sync_directory(path: &Path) -> Result<(), MaterializationReferenceErrorV1> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: std::io::Error) -> MaterializationReferenceErrorV1 {
    MaterializationReferenceErrorV1::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug, Error)]
pub enum NodMaterializationBuildErrorV1 {
    #[error("materialization head does not match the exact audited Lysis job")]
    AuthorityMismatch,
    #[error("the exact result chunk does not contain the requested actions")]
    MissingActions,
    #[error("materialization actions are not the exact ordered WWD slice")]
    ActionOrder,
    #[error("materialization proof is missing a required sibling")]
    MissingSibling,
    #[error("ROOT_REDUCE sibling does not match its exact plan position")]
    UpperSiblingMismatch,
    #[error("materialization dependency set is invalid")]
    InvalidDependencies,
    #[error(transparent)]
    ResultCatalog(#[from] LysisResultCatalogError),
    #[error(transparent)]
    Plan(#[from] ExactLysisPlanError),
    #[error(transparent)]
    Planner(#[from] outbe_lysis::program_v1::planner::PlannerErrorV1),
    #[error(transparent)]
    Artifact(#[from] outbe_lysis::program_v1::artifacts::LysisArtifactErrorV1),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

#[derive(Debug, Error)]
pub enum MaterializationReferenceErrorV1 {
    #[error("materialization reference record is missing")]
    Missing,
    #[error("materialization reference record is invalid")]
    InvalidRecord,
    #[error("materialization reference exact replay conflicts with the durable record")]
    ConflictingReplay,
    #[error("materialization reference path is unsafe: {0}")]
    UnsafePath(PathBuf),
    #[error("materialization reference store has an ambiguous temporary file: {0}")]
    AmbiguousTemp(PathBuf),
    #[error("materialization reference JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("materialization reference I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod reference_reader_bounds_tests {
    use super::*;

    #[test]
    fn growing_reference_stops_after_observed_length_plus_one() {
        let mut source = std::io::Cursor::new(vec![7; 100]);
        assert!(read_reference_bytes(&mut source, Path::new("record.json"), 2).is_err());
        assert_eq!(source.position(), 3);
    }

    #[test]
    fn stable_reference_is_exact_and_shortened_reference_is_rejected() {
        let mut source = std::io::Cursor::new(vec![1, 2, 3]);
        assert_eq!(
            read_reference_bytes(&mut source, Path::new("record.json"), 3).unwrap(),
            vec![1, 2, 3]
        );
        let mut source = std::io::Cursor::new(vec![1, 2]);
        assert!(read_reference_bytes(&mut source, Path::new("record.json"), 3).is_err());
    }
}
