//! Durable, fail-closed catalog of supervisor-verified unit admissions.
//!
//! A unit becomes visible to reducers and finalization only through this
//! catalog. Exact replay is idempotent. A second, different admission for the
//! same plan ordinal permanently latches the catalog into abstention instead
//! of selecting one of the competing artifacts.

use std::fs;
use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, B256};
use outbe_lysis::program_v1::planner::LysisPlanTopologyV1;
use outbe_ocomp_protocol::{
    input::InputManifestV1,
    result::OutputManifestEntryV1,
    unit::{PlanCommitmentV1, UnitInterval, UnitPhase, UnitSpecV1},
    CanonicalReader, CanonicalWriter, CasObjectRefV1, ObjectKind, ProtocolError, SchemaLimits,
};
use thiserror::Error;

use crate::cas::{CasError, FilesystemCas, FilesystemCasReader};

pub(crate) mod storage;
pub(crate) use storage::{
    decode_envelope, decode_object_ref, encode_envelope, encode_object_ref, parse_entry_ordinal,
    path_exists, reject_orphaned_temps, require_fresh_catalog_without_header, CatalogLock,
    ADMISSION_STORAGE_PROFILE, INPUT_REF_STORAGE_PROFILE,
};

#[cfg(test)]
mod tests;

const CATALOG_DIRECTORY_MODE: u32 = 0o700;
const HEADER_MAGIC: [u8; 8] = *b"OUTBADH1";
const ENTRY_MAGIC: [u8; 8] = *b"OUTBADM1";
const CONFLICT_MAGIC: [u8; 8] = *b"OUTBCNF1";
const HEADER_FILE: &str = "catalog.header";
const LOCK_FILE: &str = "catalog.lock";
const CONFLICT_FILE: &str = "catalog.abstained";
const ENTRY_SUFFIX: &str = ".admission";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdmittedResultChunkV1 {
    pub output_manifest_entry: OutputManifestEntryV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedAdmissionRecordV1 {
    pub protocol_bundle_hash: B256,
    pub job_id: B256,
    pub attempt: u32,
    pub plan_hash: B256,
    pub plan_ordinal: u32,
    pub unit_id: B256,
    pub artifact_ref: CasObjectRefV1,
    pub result_chunk: Option<AdmittedResultChunkV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionOutcome {
    NewlyAdmitted,
    ExactReplay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionPositionV1 {
    pub plan_ordinal: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AdmissionCatalogHeaderV1 {
    protocol_bundle_hash: B256,
    job_id: B256,
    attempt: u32,
    input_manifest_ref: CasObjectRefV1,
    input_manifest_hash: B256,
    plan_ref: CasObjectRefV1,
    plan_hash: B256,
    tribute_count: u32,
    primary_work_unit_count: u32,
    primary_work_unit_root: B256,
    exact_plan_unit_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AdmissionPlanAuthorityV1 {
    pub protocol_bundle_hash: B256,
    pub job_id: B256,
    pub attempt: u32,
    pub plan_hash: B256,
    pub tribute_count: u32,
    pub primary_work_unit_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AdmissionPinnedPlanV1 {
    pub plan_ref: CasObjectRefV1,
    pub input_manifest_ref: CasObjectRefV1,
    pub input_manifest: InputManifestV1,
    pub plan: PlanCommitmentV1,
}

pub struct VerifiedAdmissionCatalog {
    root: PathBuf,
    limits: SchemaLimits,
    header: AdmissionCatalogHeaderV1,
    abstained: bool,
    _lock: CatalogLock,
}

/// Immutable access to an existing admission catalog under its shared lock.
///
/// Opening never creates a directory, header, or lock file. The native catalog
/// remains private so a reader cannot admit records or latch a conflict.
pub struct AdmissionCatalogReader {
    inner: VerifiedAdmissionCatalog,
}

impl AdmissionCatalogReader {
    pub fn open_existing(
        root: impl AsRef<Path>,
        reader: &FilesystemCasReader,
        limits: SchemaLimits,
    ) -> Result<Self, AdmissionCatalogError> {
        let root = root.as_ref().to_path_buf();
        inspect_private_directory(&root)?;
        let lock = CatalogLock::acquire_shared(&root)?;
        Ok(Self {
            inner: VerifiedAdmissionCatalog::reopen_with_lock(root, reader, limits, lock)?,
        })
    }

    pub fn read(
        &self,
        plan_ordinal: u32,
    ) -> Result<VerifiedAdmissionRecordV1, AdmissionCatalogError> {
        self.inner.read(plan_ordinal)
    }

    pub fn exact_plan_cursor(&self) -> Result<AdmissionCursor<'_>, AdmissionCatalogError> {
        self.inner.exact_plan_cursor()
    }

    #[must_use]
    pub const fn is_abstained(&self) -> bool {
        self.inner.is_abstained()
    }

    pub(crate) fn verified_view(&self) -> &VerifiedAdmissionCatalog {
        &self.inner
    }
}

impl VerifiedAdmissionCatalog {
    pub fn open(
        root: impl AsRef<Path>,
        cas: &FilesystemCas,
        plan_ref: &CasObjectRefV1,
        input_manifest_ref: &CasObjectRefV1,
        limits: SchemaLimits,
    ) -> Result<Self, AdmissionCatalogError> {
        let expected = expected_header_with(plan_ref, input_manifest_ref, &limits, |reference| {
            Ok(cas.read_verified(reference)?.bytes().to_vec())
        })?;
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let lock = ADMISSION_STORAGE_PROFILE.acquire_lock(&root)?;
        reject_orphaned_temps(&root)?;
        let header_path = root.join(HEADER_FILE);
        let encoded_expected = encode_header(&expected, &limits)?;
        if let Some(existing) = ADMISSION_STORAGE_PROFILE.load_or_init_header(
            &root,
            &header_path,
            catalog_byte_cap(&limits),
            &encoded_expected,
        )? {
            if decode_header(&existing, &limits)? != expected {
                return Err(AdmissionCatalogError::AuthorityMismatch);
            }
        }
        let abstained = path_exists(&root.join(CONFLICT_FILE))?;
        Ok(Self {
            root,
            limits,
            header: expected,
            abstained,
            _lock: lock,
        })
    }

    pub fn reopen(
        root: impl AsRef<Path>,
        reader: &FilesystemCasReader,
        limits: SchemaLimits,
    ) -> Result<Self, AdmissionCatalogError> {
        let root = root.as_ref().to_path_buf();
        inspect_private_directory(&root)?;
        let lock = ADMISSION_STORAGE_PROFILE.acquire_lock(&root)?;
        Self::reopen_with_lock(root, reader, limits, lock)
    }

    fn reopen_with_lock(
        root: PathBuf,
        reader: &FilesystemCasReader,
        limits: SchemaLimits,
        lock: CatalogLock,
    ) -> Result<Self, AdmissionCatalogError> {
        reject_orphaned_temps(&root)?;
        let header_path = root.join(HEADER_FILE);
        if !path_exists(&header_path)? {
            return Err(AdmissionCatalogError::MissingHeader);
        }
        let header = decode_header(
            &ADMISSION_STORAGE_PROFILE.read_bounded(&header_path, catalog_byte_cap(&limits))?,
            &limits,
        )?;
        let expected = expected_header_with(
            &header.plan_ref,
            &header.input_manifest_ref,
            &limits,
            |reference| Ok(reader.read_verified(reference)?.bytes().to_vec()),
        )?;
        if header != expected {
            return Err(AdmissionCatalogError::AuthorityMismatch);
        }
        let abstained = path_exists(&root.join(CONFLICT_FILE))?;
        Ok(Self {
            root,
            limits,
            header,
            abstained,
            _lock: lock,
        })
    }

    pub fn admit_verified_unit(
        &mut self,
        position: AdmissionPositionV1,
        spec: &UnitSpecV1,
        artifact_ref: CasObjectRefV1,
        result_chunk_entry: Option<OutputManifestEntryV1>,
    ) -> Result<AdmissionOutcome, AdmissionCatalogError> {
        spec.validate_semantics(&self.limits)?;
        if position.plan_ordinal >= self.header.exact_plan_unit_count
            || spec.protocol_bundle_hash != self.header.protocol_bundle_hash
            || spec.job_id != self.header.job_id
            || spec.attempt != self.header.attempt
        {
            return Err(AdmissionCatalogError::AuthorityMismatch);
        }
        match (&spec.phase, &spec.interval, &result_chunk_entry) {
            (UnitPhase::RootReduce, UnitInterval::BinaryReducerNode(node), Some(entry))
                if node.level == 0 && node.index == entry.chunk_ordinal => {}
            (UnitPhase::RootReduce, UnitInterval::BinaryReducerNode(node), None)
                if node.level > 0 => {}
            (_, _, None) if spec.phase != UnitPhase::RootReduce => {}
            _ => {
                return Err(ProtocolError::InvalidInvariant(
                    "verified admission result chunk position",
                )
                .into());
            }
        }
        let record = VerifiedAdmissionRecordV1 {
            protocol_bundle_hash: spec.protocol_bundle_hash,
            job_id: spec.job_id,
            attempt: spec.attempt,
            plan_hash: self.header.plan_hash,
            plan_ordinal: position.plan_ordinal,
            unit_id: spec.unit_id(&self.limits)?,
            artifact_ref,
            result_chunk: result_chunk_entry.map(|output_manifest_entry| AdmittedResultChunkV1 {
                output_manifest_entry,
            }),
        };
        self.admit(&record)
    }

    fn admit(
        &mut self,
        record: &VerifiedAdmissionRecordV1,
    ) -> Result<AdmissionOutcome, AdmissionCatalogError> {
        if self.abstained {
            return Err(AdmissionCatalogError::Abstained);
        }
        validate_record(record, &self.limits)?;
        let path = self.entry_path(record.plan_ordinal);
        let canonical = encode_record(record, &self.limits)?;
        if path_exists(&path)? {
            let existing_bytes =
                ADMISSION_STORAGE_PROFILE.read_bounded(&path, catalog_byte_cap(&self.limits))?;
            let existing = decode_record(&existing_bytes, &self.limits)?;
            if existing == *record {
                return Ok(AdmissionOutcome::ExactReplay);
            }
            self.latch_conflict(record.plan_ordinal, &existing_bytes, &canonical)?;
            return Err(AdmissionCatalogError::ConflictingAdmission {
                plan_ordinal: record.plan_ordinal,
            });
        }

        ADMISSION_STORAGE_PROFILE.persist_atomic(&self.root, &path, &canonical)?;
        Ok(AdmissionOutcome::NewlyAdmitted)
    }

    pub fn read(
        &self,
        plan_ordinal: u32,
    ) -> Result<VerifiedAdmissionRecordV1, AdmissionCatalogError> {
        self.require_active()?;
        let path = self.entry_path(plan_ordinal);
        if !path_exists(&path)? {
            return Err(AdmissionCatalogError::MissingAdmission { plan_ordinal });
        }
        let encoded =
            ADMISSION_STORAGE_PROFILE.read_bounded(&path, catalog_byte_cap(&self.limits))?;
        let record = decode_record(&encoded, &self.limits)?;
        if record.plan_ordinal != plan_ordinal {
            return Err(AdmissionCatalogError::OrdinalBinding {
                requested: plan_ordinal,
                encoded: record.plan_ordinal,
            });
        }
        Ok(record)
    }

    pub fn exact_plan_cursor(&self) -> Result<AdmissionCursor<'_>, AdmissionCatalogError> {
        self.require_active()?;
        let expected_plan_hash = self.header.plan_hash;
        let expected_count = self.header.exact_plan_unit_count;
        // This compatibility API is deliberately eager and preserves its
        // precise missing-ordinal error. Security-sensitive finalization uses
        // `bounded_directory_cursor` directly and never pays this O(N) pass.
        for plan_ordinal in 0..expected_count {
            let _ = self.read(plan_ordinal)?;
        }
        for step in self.bounded_directory_cursor()? {
            let _ = step?;
        }
        Ok(AdmissionCursor {
            catalog: self,
            next_ordinal: 0,
            expected_count,
            expected_plan_hash,
        })
    }

    pub(crate) fn bounded_directory_cursor(
        &self,
    ) -> Result<AdmissionDirectoryCursorV1<'_>, AdmissionCatalogError> {
        self.require_active()?;
        Ok(AdmissionDirectoryCursorV1 {
            catalog: self,
            entries: fs::read_dir(&self.root)
                .map_err(|source| io_error("list catalog directory", &self.root, source))?,
            actual_count: 0,
            complete: false,
        })
    }

    #[must_use]
    pub const fn is_abstained(&self) -> bool {
        self.abstained
    }

    pub(crate) const fn plan_authority(&self) -> AdmissionPlanAuthorityV1 {
        AdmissionPlanAuthorityV1 {
            protocol_bundle_hash: self.header.protocol_bundle_hash,
            job_id: self.header.job_id,
            attempt: self.header.attempt,
            plan_hash: self.header.plan_hash,
            tribute_count: self.header.tribute_count,
            primary_work_unit_count: self.header.primary_work_unit_count,
        }
    }

    pub(crate) fn reload_pinned_plan(
        &self,
        reader: &FilesystemCasReader,
    ) -> Result<AdmissionPinnedPlanV1, AdmissionCatalogError> {
        self.require_active()?;
        let plan_object = reader.read_verified(&self.header.plan_ref)?;
        let manifest_object = reader.read_verified(&self.header.input_manifest_ref)?;
        let expected = expected_header(
            &self.header.plan_ref,
            plan_object.bytes(),
            &self.header.input_manifest_ref,
            manifest_object.bytes(),
            &self.limits,
        )?;
        if expected != self.header {
            return Err(AdmissionCatalogError::AuthorityMismatch);
        }
        Ok(AdmissionPinnedPlanV1 {
            plan_ref: self.header.plan_ref.clone(),
            input_manifest_ref: self.header.input_manifest_ref.clone(),
            input_manifest: InputManifestV1::decode_canonical(
                manifest_object.bytes(),
                &self.limits,
            )?,
            plan: PlanCommitmentV1::decode_canonical_record(plan_object.bytes(), &self.limits)?,
        })
    }

    fn require_active(&self) -> Result<(), AdmissionCatalogError> {
        if self.abstained {
            Err(AdmissionCatalogError::Abstained)
        } else {
            Ok(())
        }
    }

    fn entry_path(&self, plan_ordinal: u32) -> PathBuf {
        self.root.join(format!("{plan_ordinal:010}{ENTRY_SUFFIX}"))
    }

    fn latch_conflict(
        &mut self,
        plan_ordinal: u32,
        existing: &[u8],
        candidate: &[u8],
    ) -> Result<(), AdmissionCatalogError> {
        let mut marker = Vec::with_capacity(8 + 4 + 32 + 32);
        marker.extend_from_slice(&CONFLICT_MAGIC);
        marker.extend_from_slice(&plan_ordinal.to_be_bytes());
        marker.extend_from_slice(keccak256(existing).as_slice());
        marker.extend_from_slice(keccak256(candidate).as_slice());
        let path = self.root.join(CONFLICT_FILE);
        ADMISSION_STORAGE_PROFILE.persist_atomic(&self.root, &path, &marker)?;
        self.abstained = true;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdmissionDirectoryStepV1 {
    EntryChecked,
    Complete,
}

pub(crate) struct AdmissionDirectoryCursorV1<'a> {
    catalog: &'a VerifiedAdmissionCatalog,
    entries: fs::ReadDir,
    actual_count: u32,
    complete: bool,
}

impl Iterator for AdmissionDirectoryCursorV1<'_> {
    type Item = Result<AdmissionDirectoryStepV1, AdmissionCatalogError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.complete {
            return None;
        }
        match self.entries.next() {
            Some(Ok(entry)) => Some(self.inspect_entry(entry)),
            Some(Err(source)) => {
                self.complete = true;
                Some(Err(io_error(
                    "read catalog directory",
                    &self.catalog.root,
                    source,
                )))
            }
            None => Some(self.finish_directory()),
        }
    }
}

impl AdmissionDirectoryCursorV1<'_> {
    fn finish_directory(&mut self) -> Result<AdmissionDirectoryStepV1, AdmissionCatalogError> {
        self.complete = true;
        if self.actual_count != self.catalog.header.exact_plan_unit_count {
            return Err(ProtocolError::InvalidInvariant("closed admission count").into());
        }
        Ok(AdmissionDirectoryStepV1::Complete)
    }

    fn inspect_entry(
        &mut self,
        entry: fs::DirEntry,
    ) -> Result<AdmissionDirectoryStepV1, AdmissionCatalogError> {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| AdmissionCatalogError::UnexpectedCatalogEntry(entry.path()))?;
        if name == HEADER_FILE || name == LOCK_FILE {
            return Ok(AdmissionDirectoryStepV1::EntryChecked);
        }
        let plan_ordinal = name
            .strip_suffix(ENTRY_SUFFIX)
            .and_then(parse_entry_ordinal)
            .ok_or_else(|| AdmissionCatalogError::UnexpectedCatalogEntry(entry.path()))?;
        let is_file = entry
            .file_type()
            .map_err(|source| io_error("inspect catalog entry", &entry.path(), source))?
            .is_file();
        if !is_file {
            return Err(AdmissionCatalogError::UnexpectedCatalogEntry(entry.path()));
        }
        if plan_ordinal >= self.catalog.header.exact_plan_unit_count {
            return Err(AdmissionCatalogError::UnexpectedAdmission { plan_ordinal });
        }
        self.actual_count =
            self.actual_count
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "closed admission count",
                })?;
        Ok(AdmissionDirectoryStepV1::EntryChecked)
    }
}

pub struct AdmissionCursor<'a> {
    catalog: &'a VerifiedAdmissionCatalog,
    next_ordinal: u32,
    expected_count: u32,
    expected_plan_hash: B256,
}

impl Iterator for AdmissionCursor<'_> {
    type Item = Result<VerifiedAdmissionRecordV1, AdmissionCatalogError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_ordinal >= self.expected_count {
            return None;
        }
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        Some(self.catalog.read(ordinal).and_then(|record| {
            if record.plan_hash != self.expected_plan_hash {
                return Err(AdmissionCatalogError::PlanBinding {
                    plan_ordinal: ordinal,
                    expected: self.expected_plan_hash,
                    actual: record.plan_hash,
                });
            }
            Ok(record)
        }))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.expected_count.saturating_sub(self.next_ordinal);
        let remaining = usize::try_from(remaining).unwrap_or(usize::MAX);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for AdmissionCursor<'_> {}

#[derive(Debug, Error)]
pub enum AdmissionCatalogError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error("verified-admission catalog authority differs from its immutable header")]
    AuthorityMismatch,
    #[error("verified-admission catalog immutable header is missing")]
    MissingHeader,
    #[error("verified-admission catalog is permanently abstained")]
    Abstained,
    #[error("plan ordinal {plan_ordinal} already has a different verified admission")]
    ConflictingAdmission { plan_ordinal: u32 },
    #[error("verified admission for plan ordinal {plan_ordinal} is missing")]
    MissingAdmission { plan_ordinal: u32 },
    #[error("verified admission for plan ordinal {plan_ordinal} is outside the closed plan")]
    UnexpectedAdmission { plan_ordinal: u32 },
    #[error(
        "admission plan binding mismatch at ordinal {plan_ordinal}: expected {expected}, got {actual}"
    )]
    PlanBinding {
        plan_ordinal: u32,
        expected: B256,
        actual: B256,
    },
    #[error("unexpected entry exists in the admission catalog at {0}")]
    UnexpectedCatalogEntry(PathBuf),
    #[error("admission ordinal binding mismatch: requested {requested}, encoded {encoded}")]
    OrdinalBinding { requested: u32, encoded: u32 },
    #[error("ambiguous temporary admission file remains at {0}")]
    AmbiguousTemporary(PathBuf),
    #[error("admission catalog lock is already held at {0}")]
    LockHeld(PathBuf),
    #[error("admission catalog object exceeds the configured byte cap")]
    ObjectTooLarge,
    #[error("admission catalog object has an invalid envelope")]
    InvalidEnvelope,
    #[error("admission catalog I/O failed during {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn expected_header_with(
    plan_ref: &CasObjectRefV1,
    input_manifest_ref: &CasObjectRefV1,
    limits: &SchemaLimits,
    read_object: impl Fn(&CasObjectRefV1) -> Result<Vec<u8>, AdmissionCatalogError>,
) -> Result<AdmissionCatalogHeaderV1, AdmissionCatalogError> {
    let plan_bytes = read_object(plan_ref)?;
    let manifest_bytes = read_object(input_manifest_ref)?;
    expected_header(
        plan_ref,
        &plan_bytes,
        input_manifest_ref,
        &manifest_bytes,
        limits,
    )
}

fn expected_header(
    plan_ref: &CasObjectRefV1,
    plan_bytes: &[u8],
    input_manifest_ref: &CasObjectRefV1,
    manifest_bytes: &[u8],
    limits: &SchemaLimits,
) -> Result<AdmissionCatalogHeaderV1, AdmissionCatalogError> {
    let descriptor_checks = (
        plan_ref.transport_digest.is_zero(),
        plan_ref.encoded_bytes == 0,
        plan_ref.expected_ocb1_kind,
        input_manifest_ref.transport_digest.is_zero(),
        input_manifest_ref.encoded_bytes == 0,
        input_manifest_ref.expected_ocb1_kind,
    );
    let expected_descriptors = (
        false,
        false,
        None,
        false,
        false,
        Some(ObjectKind::InputManifestV1.tag()),
    );
    if descriptor_checks != expected_descriptors {
        return Err(
            ProtocolError::InvalidInvariant("admission catalog authority descriptor").into(),
        );
    }
    let plan = PlanCommitmentV1::decode_canonical_record(plan_bytes, limits)?;
    let manifest = InputManifestV1::decode_canonical(manifest_bytes, limits)?;
    plan.validate_semantics()?;
    manifest.validate_semantics(limits)?;
    let input_manifest_hash = manifest.manifest_hash(limits)?;
    let plan_subject = (
        plan.protocol_bundle_hash,
        plan.job_id,
        plan.attempt,
        plan.input_manifest_hash,
        plan.wwd,
        plan.tribute_count,
    );
    let manifest_subject = (
        manifest.protocol_bundle_hash,
        manifest.job_id,
        manifest.attempt,
        input_manifest_hash,
        manifest.wwd,
        manifest.tribute_count,
    );
    if plan_subject != manifest_subject {
        return Err(AdmissionCatalogError::AuthorityMismatch);
    }
    let topology = LysisPlanTopologyV1::new(plan.primary_work_unit_count)
        .map_err(|_| ProtocolError::InvalidInvariant("admission catalog plan topology"))?;
    let exact_plan_unit_count = topology.total_unit_count();
    if exact_plan_unit_count == 0 {
        return Err(ProtocolError::InvalidInvariant("admission catalog plan size").into());
    }
    Ok(AdmissionCatalogHeaderV1 {
        protocol_bundle_hash: plan.protocol_bundle_hash,
        job_id: plan.job_id,
        attempt: plan.attempt,
        input_manifest_ref: input_manifest_ref.clone(),
        input_manifest_hash,
        plan_ref: plan_ref.clone(),
        plan_hash: plan.plan_hash(limits)?,
        tribute_count: plan.tribute_count,
        primary_work_unit_count: plan.primary_work_unit_count,
        primary_work_unit_root: plan.primary_work_unit_root,
        exact_plan_unit_count,
    })
}

fn encode_header(
    header: &AdmissionCatalogHeaderV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, AdmissionCatalogError> {
    let mut body = CanonicalWriter::new(limits.codec);
    body.write_b256(header.protocol_bundle_hash)?;
    body.write_b256(header.job_id)?;
    body.write_u32(header.attempt)?;
    encode_object_ref(&mut body, &header.input_manifest_ref)?;
    body.write_b256(header.input_manifest_hash)?;
    encode_object_ref(&mut body, &header.plan_ref)?;
    body.write_b256(header.plan_hash)?;
    body.write_u32(header.tribute_count)?;
    body.write_u32(header.primary_work_unit_count)?;
    body.write_b256(header.primary_work_unit_root)?;
    body.write_u32(header.exact_plan_unit_count)?;
    Ok(encode_envelope(HEADER_MAGIC, &body.into_bytes()))
}

fn decode_header(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<AdmissionCatalogHeaderV1, AdmissionCatalogError> {
    let body = decode_envelope(encoded, HEADER_MAGIC)?;
    let mut input = CanonicalReader::new(body, limits.codec)?;
    let header = AdmissionCatalogHeaderV1 {
        protocol_bundle_hash: input.read_b256()?,
        job_id: input.read_b256()?,
        attempt: input.read_u32()?,
        input_manifest_ref: decode_object_ref(&mut input)?,
        input_manifest_hash: input.read_b256()?,
        plan_ref: decode_object_ref(&mut input)?,
        plan_hash: input.read_b256()?,
        tribute_count: input.read_u32()?,
        primary_work_unit_count: input.read_u32()?,
        primary_work_unit_root: input.read_b256()?,
        exact_plan_unit_count: input.read_u32()?,
    };
    input.finish()?;
    if encode_header(&header, limits)? != encoded {
        return Err(AdmissionCatalogError::InvalidEnvelope);
    }
    Ok(header)
}

fn validate_record(
    record: &VerifiedAdmissionRecordV1,
    limits: &SchemaLimits,
) -> Result<(), AdmissionCatalogError> {
    let required_checks = (
        record.protocol_bundle_hash.is_zero(),
        record.job_id.is_zero(),
        record.plan_hash.is_zero(),
        record.unit_id.is_zero(),
        record.artifact_ref.transport_digest.is_zero(),
        record.artifact_ref.encoded_bytes == 0,
        record.artifact_ref.expected_ocb1_kind,
    );
    let expected_checks = (
        false,
        false,
        false,
        false,
        false,
        false,
        Some(ObjectKind::UnitArtifactV1.tag()),
    );
    if required_checks != expected_checks {
        return Err(ProtocolError::InvalidInvariant("verified admission binding").into());
    }
    if let Some(chunk) = &record.result_chunk {
        chunk
            .output_manifest_entry
            .encode_canonical_record(limits)?;
        if chunk
            .output_manifest_entry
            .result_chunk_ref
            .expected_ocb1_kind
            != Some(ObjectKind::ResultChunkV1.tag())
        {
            return Err(
                ProtocolError::InvalidInvariant("admitted result chunk object kind").into(),
            );
        }
    }
    Ok(())
}

fn catalog_byte_cap(limits: &SchemaLimits) -> usize {
    limits
        .codec
        .max_body_bytes
        .saturating_add(ENTRY_MAGIC.len() + B256::len_bytes())
}

fn encode_record(
    record: &VerifiedAdmissionRecordV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, AdmissionCatalogError> {
    validate_record(record, limits)?;
    let mut body = CanonicalWriter::new(limits.codec);
    body.write_b256(record.protocol_bundle_hash)?;
    body.write_b256(record.job_id)?;
    body.write_u32(record.attempt)?;
    body.write_b256(record.plan_hash)?;
    body.write_u32(record.plan_ordinal)?;
    body.write_b256(record.unit_id)?;
    encode_object_ref(&mut body, &record.artifact_ref)?;
    body.write_option(record.result_chunk.as_ref(), |writer, chunk| {
        let encoded = chunk
            .output_manifest_entry
            .encode_canonical_record(limits)?;
        writer.write_bounded_bytes(&encoded, limits.max_bounded_bytes)
    })?;
    Ok(encode_envelope(ENTRY_MAGIC, &body.into_bytes()))
}

fn decode_record(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<VerifiedAdmissionRecordV1, AdmissionCatalogError> {
    let body = decode_envelope(encoded, ENTRY_MAGIC)?;
    let mut input = CanonicalReader::new(body, limits.codec)?;
    let record = VerifiedAdmissionRecordV1 {
        protocol_bundle_hash: input.read_b256()?,
        job_id: input.read_b256()?,
        attempt: input.read_u32()?,
        plan_hash: input.read_b256()?,
        plan_ordinal: input.read_u32()?,
        unit_id: input.read_b256()?,
        artifact_ref: decode_object_ref(&mut input)?,
        result_chunk: input.read_option(|reader| {
            let encoded = reader.read_bounded_bytes(limits.max_bounded_bytes)?;
            Ok(AdmittedResultChunkV1 {
                output_manifest_entry: OutputManifestEntryV1::decode_canonical_record(
                    encoded, limits,
                )?,
            })
        })?,
    };
    input.finish()?;
    validate_record(&record, limits)?;
    if encode_record(&record, limits)? != encoded {
        return Err(AdmissionCatalogError::InvalidEnvelope);
    }
    Ok(record)
}

fn create_private_directory(path: &Path) -> Result<(), AdmissionCatalogError> {
    fs::create_dir_all(path).map_err(|source| io_error("create directory", path, source))?;
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AdmissionCatalogError::InvalidEnvelope);
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(CATALOG_DIRECTORY_MODE))
        .map_err(|source| io_error("set directory permissions", path, source))
}

fn inspect_private_directory(path: &Path) -> Result<(), AdmissionCatalogError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(AdmissionCatalogError::InvalidEnvelope);
    }
    Ok(())
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> AdmissionCatalogError {
    AdmissionCatalogError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
