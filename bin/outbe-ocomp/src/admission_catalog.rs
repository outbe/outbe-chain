//! Durable, fail-closed catalog of supervisor-verified unit admissions.
//!
//! A unit becomes visible to reducers and finalization only through this
//! catalog. Exact replay is idempotent. A second, different admission for the
//! same plan ordinal permanently latches the catalog into abstention instead
//! of selecting one of the competing artifacts.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{
    result::OutputManifestEntryV1,
    unit::{UnitInterval, UnitPhase, UnitSpecV1},
    CanonicalReader, CanonicalWriter, CasObjectRefV1, ObjectKind, ProtocolError, SchemaLimits,
};
use thiserror::Error;

const CATALOG_DIRECTORY_MODE: u32 = 0o700;
const CATALOG_FILE_MODE: u32 = 0o600;
const ENTRY_MAGIC: [u8; 8] = *b"OUTBADM1";
const CONFLICT_MAGIC: [u8; 8] = *b"OUTBCNF1";
const LOCK_FILE: &str = "catalog.lock";
const CONFLICT_FILE: &str = "catalog.abstained";
const ENTRY_SUFFIX: &str = ".admission";
const TEMP_SUFFIX: &str = ".tmp";

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
    pub plan_hash: B256,
    pub plan_ordinal: u32,
}

pub struct VerifiedAdmissionCatalog {
    root: PathBuf,
    limits: SchemaLimits,
    abstained: bool,
    _lock: CatalogLock,
}

impl VerifiedAdmissionCatalog {
    pub fn open(
        root: impl AsRef<Path>,
        limits: SchemaLimits,
    ) -> Result<Self, AdmissionCatalogError> {
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let lock = CatalogLock::acquire(&root)?;
        reject_orphaned_temps(&root)?;
        let abstained = path_exists(&root.join(CONFLICT_FILE))?;
        Ok(Self {
            root,
            limits,
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
        if position.plan_hash.is_zero() {
            return Err(ProtocolError::InvalidInvariant("verified admission plan hash").into());
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
            plan_hash: position.plan_hash,
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
            let existing_bytes = read_bounded(&path, catalog_byte_cap(&self.limits))?;
            let existing = decode_record(&existing_bytes, &self.limits)?;
            if existing == *record {
                return Ok(AdmissionOutcome::ExactReplay);
            }
            self.latch_conflict(record.plan_ordinal, &existing_bytes, &canonical)?;
            return Err(AdmissionCatalogError::ConflictingAdmission {
                plan_ordinal: record.plan_ordinal,
            });
        }

        persist_atomic(&self.root, &path, &canonical)?;
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
        let encoded = read_bounded(&path, catalog_byte_cap(&self.limits))?;
        let record = decode_record(&encoded, &self.limits)?;
        if record.plan_ordinal != plan_ordinal {
            return Err(AdmissionCatalogError::OrdinalBinding {
                requested: plan_ordinal,
                encoded: record.plan_ordinal,
            });
        }
        Ok(record)
    }

    pub fn exact_order_cursor(
        &self,
        expected_count: u32,
    ) -> Result<AdmissionCursor<'_>, AdmissionCatalogError> {
        self.require_active()?;
        Ok(AdmissionCursor {
            catalog: self,
            next_ordinal: 0,
            expected_count,
        })
    }

    #[must_use]
    pub const fn is_abstained(&self) -> bool {
        self.abstained
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
        persist_atomic(&self.root, &path, &marker)?;
        self.abstained = true;
        Ok(())
    }
}

pub struct AdmissionCursor<'a> {
    catalog: &'a VerifiedAdmissionCatalog,
    next_ordinal: u32,
    expected_count: u32,
}

impl Iterator for AdmissionCursor<'_> {
    type Item = Result<VerifiedAdmissionRecordV1, AdmissionCatalogError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_ordinal >= self.expected_count {
            return None;
        }
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        Some(self.catalog.read(ordinal))
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
    #[error("verified-admission catalog is permanently abstained")]
    Abstained,
    #[error("plan ordinal {plan_ordinal} already has a different verified admission")]
    ConflictingAdmission { plan_ordinal: u32 },
    #[error("verified admission for plan ordinal {plan_ordinal} is missing")]
    MissingAdmission { plan_ordinal: u32 },
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

fn validate_record(
    record: &VerifiedAdmissionRecordV1,
    limits: &SchemaLimits,
) -> Result<(), AdmissionCatalogError> {
    if record.protocol_bundle_hash.is_zero()
        || record.job_id.is_zero()
        || record.plan_hash.is_zero()
        || record.unit_id.is_zero()
        || record.artifact_ref.transport_digest.is_zero()
        || record.artifact_ref.encoded_bytes == 0
        || record.artifact_ref.expected_ocb1_kind != Some(ObjectKind::UnitArtifactV1.tag())
    {
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
    let body = body.into_bytes();

    let mut output = Vec::with_capacity(ENTRY_MAGIC.len() + 32 + body.len());
    output.extend_from_slice(&ENTRY_MAGIC);
    output.extend_from_slice(keccak256(&body).as_slice());
    output.extend_from_slice(&body);
    Ok(output)
}

fn decode_record(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<VerifiedAdmissionRecordV1, AdmissionCatalogError> {
    let envelope_bytes = ENTRY_MAGIC.len() + B256::len_bytes();
    if encoded.len() < envelope_bytes || encoded[..ENTRY_MAGIC.len()] != ENTRY_MAGIC {
        return Err(AdmissionCatalogError::InvalidEnvelope);
    }
    let expected_digest = B256::from_slice(&encoded[ENTRY_MAGIC.len()..envelope_bytes]);
    let body = &encoded[envelope_bytes..];
    if keccak256(body) != expected_digest {
        return Err(AdmissionCatalogError::InvalidEnvelope);
    }

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

fn encode_object_ref(
    output: &mut CanonicalWriter,
    reference: &CasObjectRefV1,
) -> Result<(), ProtocolError> {
    output.write_b256(reference.transport_digest)?;
    output.write_u64(reference.encoded_bytes)?;
    output.write_option(reference.expected_ocb1_kind.as_ref(), |writer, kind| {
        writer.write_u16(*kind)
    })
}

fn decode_object_ref(input: &mut CanonicalReader<'_>) -> Result<CasObjectRefV1, ProtocolError> {
    Ok(CasObjectRefV1 {
        transport_digest: input.read_b256()?,
        encoded_bytes: input.read_u64()?,
        expected_ocb1_kind: input.read_option(|reader| reader.read_u16())?,
    })
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

fn reject_orphaned_temps(root: &Path) -> Result<(), AdmissionCatalogError> {
    let entries =
        fs::read_dir(root).map_err(|source| io_error("list catalog directory", root, source))?;
    for entry in entries {
        let entry = entry.map_err(|source| io_error("read catalog directory", root, source))?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(TEMP_SUFFIX))
        {
            return Err(AdmissionCatalogError::AmbiguousTemporary(path));
        }
    }
    Ok(())
}

fn persist_atomic(root: &Path, target: &Path, bytes: &[u8]) -> Result<(), AdmissionCatalogError> {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(AdmissionCatalogError::InvalidEnvelope)?;
    let temp = root.join(format!("{file_name}{TEMP_SUFFIX}"));
    if path_exists(&temp)? {
        return Err(AdmissionCatalogError::AmbiguousTemporary(temp));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(CATALOG_FILE_MODE)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&temp)
        .map_err(|source| io_error("create temporary admission", &temp, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error("write temporary admission", &temp, source))?;
    file.sync_all()
        .map_err(|source| io_error("fsync temporary admission", &temp, source))?;
    fs::rename(&temp, target).map_err(|source| io_error("install admission", target, source))?;
    sync_directory(root)
}

fn read_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, AdmissionCatalogError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_error("open admission", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect admission", path, source))?;
    if !metadata.file_type().is_file()
        || metadata.len() > u64::try_from(max_bytes).unwrap_or(u64::MAX)
    {
        return Err(AdmissionCatalogError::ObjectTooLarge);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(
        u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .map_err(|source| io_error("read admission", path, source))?;
    if bytes.len() > max_bytes {
        return Err(AdmissionCatalogError::ObjectTooLarge);
    }
    Ok(bytes)
}

fn path_exists(path: &Path) -> Result<bool, AdmissionCatalogError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(AdmissionCatalogError::InvalidEnvelope);
            }
            Ok(true)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect path", path, source)),
    }
}

fn sync_directory(path: &Path) -> Result<(), AdmissionCatalogError> {
    let directory =
        File::open(path).map_err(|source| io_error("open catalog directory", path, source))?;
    directory
        .sync_all()
        .map_err(|source| io_error("fsync catalog directory", path, source))
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> AdmissionCatalogError {
    AdmissionCatalogError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

struct CatalogLock {
    file: File,
}

impl CatalogLock {
    #[allow(unsafe_code)]
    fn acquire(root: &Path) -> Result<Self, AdmissionCatalogError> {
        let path = root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(CATALOG_FILE_MODE)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|source| io_error("open catalog lock", &path, source))?;
        // SAFETY: `file` owns a live descriptor for the complete flock call.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let source = std::io::Error::last_os_error();
            if source.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(AdmissionCatalogError::LockHeld(path));
            }
            return Err(io_error("lock catalog", &path, source));
        }
        Ok(Self { file })
    }
}

impl Drop for CatalogLock {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: `self.file` remains open for the complete flock call.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_ocomp_protocol::local_control::poc_schema_limits;

    fn hash(byte: u8) -> B256 {
        B256::repeat_byte(byte)
    }

    fn record(plan_ordinal: u32) -> VerifiedAdmissionRecordV1 {
        VerifiedAdmissionRecordV1 {
            protocol_bundle_hash: hash(1),
            job_id: hash(2),
            attempt: 3,
            plan_hash: hash(4),
            plan_ordinal,
            unit_id: hash(u8::try_from(plan_ordinal + 5).unwrap()),
            artifact_ref: CasObjectRefV1 {
                transport_digest: hash(6),
                encoded_bytes: 100,
                expected_ocb1_kind: Some(ObjectKind::UnitArtifactV1.tag()),
            },
            result_chunk: None,
        }
    }

    #[test]
    fn exact_replay_and_restart_preserve_one_admission() {
        let root = tempfile::tempdir().unwrap();
        let limits = poc_schema_limits();
        let admitted = record(0);
        {
            let mut catalog = VerifiedAdmissionCatalog::open(root.path(), limits).unwrap();
            assert_eq!(
                catalog.admit(&admitted).unwrap(),
                AdmissionOutcome::NewlyAdmitted
            );
            assert_eq!(
                catalog.admit(&admitted).unwrap(),
                AdmissionOutcome::ExactReplay
            );
        }

        let catalog = VerifiedAdmissionCatalog::open(root.path(), limits).unwrap();
        assert_eq!(catalog.read(0).unwrap(), admitted);
        assert_eq!(
            catalog
                .exact_order_cursor(1)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![admitted]
        );
    }

    #[test]
    fn conflicting_valid_admission_permanently_abstains_without_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let limits = poc_schema_limits();
        let first = record(0);
        let mut second = first.clone();
        second.artifact_ref.transport_digest = hash(99);
        {
            let mut catalog = VerifiedAdmissionCatalog::open(root.path(), limits).unwrap();
            catalog.admit(&first).unwrap();
            assert!(matches!(
                catalog.admit(&second),
                Err(AdmissionCatalogError::ConflictingAdmission { plan_ordinal: 0 })
            ));
            assert!(catalog.is_abstained());
            assert!(matches!(
                catalog.read(0),
                Err(AdmissionCatalogError::Abstained)
            ));
        }

        let catalog = VerifiedAdmissionCatalog::open(root.path(), limits).unwrap();
        assert!(catalog.is_abstained());
        assert!(matches!(
            catalog.exact_order_cursor(1),
            Err(AdmissionCatalogError::Abstained)
        ));
    }

    #[test]
    fn exact_order_cursor_stops_on_a_missing_plan_ordinal() {
        let root = tempfile::tempdir().unwrap();
        let limits = poc_schema_limits();
        let mut catalog = VerifiedAdmissionCatalog::open(root.path(), limits).unwrap();
        catalog.admit(&record(0)).unwrap();
        catalog.admit(&record(2)).unwrap();

        let mut cursor = catalog.exact_order_cursor(3).unwrap();
        assert_eq!(cursor.next().unwrap().unwrap().plan_ordinal, 0);
        assert!(matches!(
            cursor.next().unwrap(),
            Err(AdmissionCatalogError::MissingAdmission { plan_ordinal: 1 })
        ));
    }

    #[test]
    fn admission_read_rejects_a_symlink_substitution() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let limits = poc_schema_limits();
        {
            let mut catalog = VerifiedAdmissionCatalog::open(root.path(), limits).unwrap();
            catalog.admit(&record(0)).unwrap();
        }
        let entry = root.path().join(format!("{:010}{ENTRY_SUFFIX}", 0));
        let substitute = root.path().join("substitute");
        fs::write(&substitute, b"not an admission").unwrap();
        fs::remove_file(&entry).unwrap();
        symlink(&substitute, &entry).unwrap();

        let catalog = VerifiedAdmissionCatalog::open(root.path(), limits).unwrap();
        assert!(matches!(
            catalog.read(0),
            Err(AdmissionCatalogError::InvalidEnvelope)
        ));
    }
}
