//! Durable preimages for the ordered input-reference commitment.
//!
//! `InputManifestV1` remains the authority. This local catalog only makes its
//! exact `InputChunkRefV1` preimages discoverable after a cold restart without
//! scanning CAS or retaining a population-sized in-memory vector.

use std::fs;
use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{
    input::{AuthenticatedInputChunkV1, InputChunkKind, InputChunkRefV1, InputManifestV1},
    profile::ProtocolBundleV1,
    CanonicalReader, CanonicalWriter, CasObjectRefV1, ListKind, ObjectKind, OrderedListLimits,
    ProtocolError, SchemaLimits, StreamingOrderedListRoot,
};
use thiserror::Error;

use crate::{
    admission_catalog::{
        decode_envelope, decode_object_ref, encode_envelope, encode_object_ref,
        parse_entry_ordinal, path_exists, reject_orphaned_temps, CatalogLock,
        INPUT_REF_STORAGE_PROFILE,
    },
    cas::{CasError, FilesystemCas, FilesystemCasReader},
    input_artifacts::{
        decode_verified_input_chunk, decode_verified_input_chunk_integrity, derive_input_chunk_ref,
        derive_input_chunk_ref_integrity,
    },
};

mod publisher;

pub use publisher::{InputRefCatalogPublisher, PreparedInputRefCatalogPublisher};

const DIRECTORY_MODE: u32 = 0o750;
const HEADER_MAGIC: [u8; 8] = *b"OUTBICH1";
const ENTRY_MAGIC: [u8; 8] = *b"OUTBICR1";
const CONFLICT_MAGIC: [u8; 8] = *b"OUTBICF1";
const STAGING_MAGIC: [u8; 8] = *b"OUTBICS1";
const HEADER_FILE: &str = "catalog.header";
const PREPARED_FILE: &str = "catalog.prepared";
const STAGING_FILE: &str = "catalog.staging";
const LOCK_FILE: &str = "catalog.lock";
const CONFLICT_FILE: &str = "catalog.abstained";
const ENTRY_SUFFIX: &str = ".input-ref";
const TEMP_SUFFIX: &str = ".tmp";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputRefAdmissionOutcome {
    NewlyAdmitted,
    ExactReplay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputRefCatalogSubjectV1 {
    pub protocol_bundle_hash: B256,
    pub job_id: B256,
    pub attempt: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputRefCatalogSummaryV1 {
    pub input_chunk_count: u32,
    pub input_chunk_list_root: B256,
    pub exact_encoded_bytes: u64,
    pub exact_record_count: u32,
    pub tribute_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InputRefCatalogHeaderV1 {
    manifest_ref: CasObjectRefV1,
    manifest_hash: B256,
    protocol_bundle_hash: B256,
    job_id: B256,
    attempt: u32,
    input_chunk_count: u32,
    input_chunk_list_root: B256,
    exact_encoded_bytes: u64,
    exact_record_count: u32,
    tribute_count: u32,
}

pub struct VerifiedInputChunkRefCatalog {
    root: PathBuf,
    limits: SchemaLimits,
    _list_limits: OrderedListLimits,
    header: InputRefCatalogHeaderV1,
    abstained: bool,
    writable: bool,
    _lock: CatalogLock,
}

impl VerifiedInputChunkRefCatalog {
    pub fn open(
        root: impl AsRef<Path>,
        cas: &FilesystemCas,
        manifest_ref: &CasObjectRefV1,
        limits: SchemaLimits,
        list_limits: OrderedListLimits,
    ) -> Result<Self, InputRefCatalogError> {
        let manifest_object = cas.read_verified(manifest_ref)?;
        let manifest = InputManifestV1::decode_canonical(manifest_object.bytes(), &limits)?;
        let expected = expected_header(manifest_ref, &manifest, &limits)?;
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let lock = INPUT_REF_STORAGE_PROFILE.acquire_lock(&root)?;
        reject_orphaned_temps(&root)?;
        let header_path = root.join(HEADER_FILE);
        let encoded_expected = encode_header(&expected, &limits)?;
        if let Some(existing) = INPUT_REF_STORAGE_PROFILE.load_or_init_header(
            &root,
            &header_path,
            catalog_byte_cap(&limits),
            &encoded_expected,
        )? {
            if decode_header(&existing, &limits)? != expected {
                return Err(InputRefCatalogError::AuthorityMismatch);
            }
        }
        let abstained = path_exists(&root.join(CONFLICT_FILE))?;
        Ok(Self {
            root,
            limits,
            _list_limits: list_limits,
            header: expected,
            abstained,
            writable: true,
            _lock: lock,
        })
    }

    pub fn reopen(
        root: impl AsRef<Path>,
        reader: &FilesystemCasReader,
        limits: SchemaLimits,
        list_limits: OrderedListLimits,
    ) -> Result<Self, InputRefCatalogError> {
        let root = root.as_ref().to_path_buf();
        inspect_private_directory(&root)?;
        let lock = CatalogLock::acquire_shared(&root)?;
        reject_orphaned_temps(&root)?;
        let header_path = root.join(HEADER_FILE);
        if !path_exists(&header_path)? {
            return Err(InputRefCatalogError::MissingHeader);
        }
        let header = decode_header(
            &INPUT_REF_STORAGE_PROFILE.read_bounded(&header_path, catalog_byte_cap(&limits))?,
            &limits,
        )?;
        let manifest_object = reader.read_verified(&header.manifest_ref)?;
        let manifest = InputManifestV1::decode_canonical(manifest_object.bytes(), &limits)?;
        let expected = expected_header(&header.manifest_ref, &manifest, &limits)?;
        if header != expected {
            return Err(InputRefCatalogError::AuthorityMismatch);
        }
        let abstained = path_exists(&root.join(CONFLICT_FILE))?;
        Ok(Self {
            root,
            limits,
            _list_limits: list_limits,
            header,
            abstained,
            writable: false,
            _lock: lock,
        })
    }

    pub fn admit(
        &mut self,
        reference: &InputChunkRefV1,
    ) -> Result<InputRefAdmissionOutcome, InputRefCatalogError> {
        if !self.writable {
            return Err(InputRefCatalogError::ReadOnly);
        }
        self.require_active()?;
        let canonical = reference.encode_canonical_record(&self.limits)?;
        if reference.ordinal >= self.header.input_chunk_count {
            return Err(InputRefCatalogError::UnexpectedReference {
                ordinal: reference.ordinal,
            });
        }
        let path = self.entry_path(reference.ordinal);
        let encoded = encode_entry(&canonical);
        if path_exists(&path)? {
            let existing =
                INPUT_REF_STORAGE_PROFILE.read_bounded(&path, catalog_byte_cap(&self.limits))?;
            if existing == encoded {
                return Ok(InputRefAdmissionOutcome::ExactReplay);
            }
            self.latch_conflict(reference.ordinal, &existing, &encoded)?;
            return Err(InputRefCatalogError::ConflictingReference {
                ordinal: reference.ordinal,
            });
        }
        INPUT_REF_STORAGE_PROFILE.persist_atomic(&self.root, &path, &encoded)?;
        Ok(InputRefAdmissionOutcome::NewlyAdmitted)
    }

    pub fn exact_cursor(&self) -> Result<InputRefCursor<'_>, InputRefCatalogError> {
        self.exact_cursor_observing(|| {})
    }

    pub fn exact_cursor_observing(
        &self,
        on_progress: impl Fn(),
    ) -> Result<InputRefCursor<'_>, InputRefCatalogError> {
        self.require_active()?;
        for step in self.bounded_closure_cursor()? {
            let _ = step?;
            on_progress();
        }
        Ok(InputRefCursor {
            catalog: self,
            next_ordinal: 0,
        })
    }

    pub(crate) fn bounded_closure_cursor(
        &self,
    ) -> Result<InputRefCatalogClosureCursorV1<'_>, InputRefCatalogError> {
        self.require_active()?;
        Ok(InputRefCatalogClosureCursorV1 {
            catalog: self,
            stage: InputRefCatalogClosureStageV1::References,
            next_ordinal: 0,
            root: Some(StreamingOrderedListRoot::new(
                ListKind::InputChunkReferences,
                self.header.input_chunk_count,
            )?),
            exact_encoded_bytes: 0,
            exact_record_count: 0,
            tribute_count: 0,
            previous_kind: None,
            directory: None,
            actual_entry_count: 0,
        })
    }

    pub fn exact_verified_cursor<'a>(
        &'a self,
        reader: &'a FilesystemCasReader,
        bundle: &'a ProtocolBundleV1,
    ) -> Result<VerifiedInputRefCursor<'a>, InputRefCatalogError> {
        self.exact_verified_cursor_observing(reader, bundle, || {})
    }

    pub fn exact_verified_cursor_observing<'a>(
        &'a self,
        reader: &'a FilesystemCasReader,
        bundle: &'a ProtocolBundleV1,
        on_progress: impl Fn(),
    ) -> Result<VerifiedInputRefCursor<'a>, InputRefCatalogError> {
        Ok(VerifiedInputRefCursor {
            inner: self.exact_cursor_observing(on_progress)?,
            reader,
            bundle,
            integrity_only: false,
        })
    }

    /// Verify a closed catalog's CAS objects without opening private amounts.
    pub fn exact_verified_integrity_cursor<'a>(
        &'a self,
        reader: &'a FilesystemCasReader,
        bundle: &'a ProtocolBundleV1,
    ) -> Result<VerifiedInputRefCursor<'a>, InputRefCatalogError> {
        Ok(VerifiedInputRefCursor {
            inner: self.exact_cursor()?,
            reader,
            bundle,
            integrity_only: true,
        })
    }

    pub(crate) fn require_manifest_authority(
        &self,
        manifest_ref: &CasObjectRefV1,
        manifest: &InputManifestV1,
    ) -> Result<(), InputRefCatalogError> {
        self.require_active()?;
        if expected_header(manifest_ref, manifest, &self.limits)? != self.header {
            return Err(InputRefCatalogError::AuthorityMismatch);
        }
        Ok(())
    }

    pub(crate) fn verified_reference_at(
        &self,
        ordinal: u32,
        reader: &FilesystemCasReader,
        bundle: &ProtocolBundleV1,
    ) -> Result<VerifiedInputChunkRefV1, InputRefCatalogError> {
        self.verify_reference(self.read(ordinal)?, reader, bundle)
    }

    pub(crate) fn verified_reference_at_integrity(
        &self,
        ordinal: u32,
        reader: &FilesystemCasReader,
        bundle: &ProtocolBundleV1,
    ) -> Result<VerifiedInputChunkRefV1, InputRefCatalogError> {
        self.verify_reference_integrity(self.read(ordinal)?, reader, bundle)
    }

    fn read(&self, ordinal: u32) -> Result<InputChunkRefV1, InputRefCatalogError> {
        read_reference(&self.root, &self.limits, ordinal)
    }

    pub(crate) fn verify_reference(
        &self,
        reference: InputChunkRefV1,
        reader: &FilesystemCasReader,
        bundle: &ProtocolBundleV1,
    ) -> Result<VerifiedInputChunkRefV1, InputRefCatalogError> {
        self.verify_reference_with_mode(reference, reader, bundle, false)
    }

    fn verify_reference_integrity(
        &self,
        reference: InputChunkRefV1,
        reader: &FilesystemCasReader,
        bundle: &ProtocolBundleV1,
    ) -> Result<VerifiedInputChunkRefV1, InputRefCatalogError> {
        self.verify_reference_with_mode(reference, reader, bundle, true)
    }

    fn verify_reference_with_mode(
        &self,
        reference: InputChunkRefV1,
        reader: &FilesystemCasReader,
        bundle: &ProtocolBundleV1,
        integrity_only: bool,
    ) -> Result<VerifiedInputChunkRefV1, InputRefCatalogError> {
        let object_ref = CasObjectRefV1 {
            transport_digest: reference.transport_digest,
            encoded_bytes: reference.encoded_bytes,
            expected_ocb1_kind: Some(ObjectKind::AuthenticatedInputChunkV1.tag()),
        };
        let object = reader.read_verified(&object_ref)?;
        let derived = if integrity_only {
            derive_input_chunk_ref_integrity(&object, bundle, &self.limits)
        } else {
            derive_input_chunk_ref(&object, bundle, &self.limits)
        }
        .map_err(|_| ProtocolError::InvalidInvariant("input-ref catalog CAS reference derivation"))?
        .reference;
        if derived != reference {
            return Err(ProtocolError::InvalidInvariant("input-ref catalog CAS preimage").into());
        }
        let chunk = if integrity_only {
            decode_verified_input_chunk_integrity(&object, bundle, &self.limits)
        } else {
            decode_verified_input_chunk(&object, bundle, &self.limits)
        }
        .map_err(|_| ProtocolError::InvalidInvariant("input-ref catalog CAS chunk decoding"))?;
        let chunk_tuple = (
            chunk.protocol_bundle_hash,
            chunk.job_id,
            chunk.kind,
            chunk.ordinal,
        );
        let expected_tuple = (
            self.header.protocol_bundle_hash,
            self.header.job_id,
            reference.kind,
            reference.ordinal,
        );
        if chunk_tuple != expected_tuple {
            return Err(
                ProtocolError::InvalidInvariant("input-ref catalog chunk authority").into(),
            );
        }
        Ok(VerifiedInputChunkRefV1 { reference, chunk })
    }

    fn entry_path(&self, ordinal: u32) -> PathBuf {
        entry_path(&self.root, ordinal)
    }

    fn require_active(&self) -> Result<(), InputRefCatalogError> {
        if self.abstained {
            Err(InputRefCatalogError::Abstained)
        } else {
            Ok(())
        }
    }

    fn latch_conflict(
        &mut self,
        ordinal: u32,
        existing: &[u8],
        candidate: &[u8],
    ) -> Result<(), InputRefCatalogError> {
        persist_conflict(&self.root, ordinal, existing, candidate)?;
        self.abstained = true;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InputRefCatalogClosureStepV1 {
    Reference(InputChunkRefV1),
    ReferencesClosed,
    DirectoryEntryChecked,
    Complete,
}

enum InputRefCatalogClosureStageV1 {
    References,
    Directory,
    Complete,
}

pub(crate) struct InputRefCatalogClosureCursorV1<'a> {
    catalog: &'a VerifiedInputChunkRefCatalog,
    stage: InputRefCatalogClosureStageV1,
    next_ordinal: u32,
    root: Option<StreamingOrderedListRoot>,
    exact_encoded_bytes: u64,
    exact_record_count: u32,
    tribute_count: u32,
    previous_kind: Option<InputChunkKind>,
    directory: Option<fs::ReadDir>,
    actual_entry_count: u32,
}

impl Iterator for InputRefCatalogClosureCursorV1<'_> {
    type Item = Result<InputRefCatalogClosureStepV1, InputRefCatalogError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.stage {
            InputRefCatalogClosureStageV1::References => Some(self.step_references()),
            InputRefCatalogClosureStageV1::Directory => Some(self.step_directory()),
            InputRefCatalogClosureStageV1::Complete => None,
        }
    }
}

impl InputRefCatalogClosureCursorV1<'_> {
    fn step_references(&mut self) -> Result<InputRefCatalogClosureStepV1, InputRefCatalogError> {
        if self.next_ordinal < self.catalog.header.input_chunk_count {
            let ordinal = self.next_ordinal;
            self.next_ordinal += 1;
            return self.read_and_accumulate_reference(ordinal);
        }
        let result = self.close_references();
        if result.is_err() {
            self.stage = InputRefCatalogClosureStageV1::Complete;
        }
        result
    }

    fn read_and_accumulate_reference(
        &mut self,
        ordinal: u32,
    ) -> Result<InputRefCatalogClosureStepV1, InputRefCatalogError> {
        let reference = self.catalog.read(ordinal)?;
        if self.previous_kind.is_some_and(|kind| kind > reference.kind) {
            return Err(ProtocolError::InvalidInvariant("input-ref catalog kind order").into());
        }
        self.previous_kind = Some(reference.kind);
        self.exact_encoded_bytes = self
            .exact_encoded_bytes
            .checked_add(reference.encoded_bytes)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "input-ref encoded bytes",
            })?;
        self.exact_record_count = self
            .exact_record_count
            .checked_add(reference.record_count)
            .ok_or(ProtocolError::IntegerOverflow {
                what: "input-ref record count",
            })?;
        if reference.kind == InputChunkKind::Tribute {
            self.tribute_count = self
                .tribute_count
                .checked_add(reference.record_count)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "input-ref Tribute count",
                })?;
        }
        self.root
            .as_mut()
            .ok_or(ProtocolError::InvalidInvariant(
                "input-ref closure root state",
            ))?
            .push(
                &reference.encode_canonical_record(&self.catalog.limits)?,
                self.catalog.limits.max_bounded_bytes,
            )?;
        Ok(InputRefCatalogClosureStepV1::Reference(reference))
    }

    fn close_references(&mut self) -> Result<InputRefCatalogClosureStepV1, InputRefCatalogError> {
        let root = self
            .root
            .take()
            .ok_or(ProtocolError::InvalidInvariant(
                "input-ref closure root state",
            ))?
            .finish()?;
        let actual_closure = (
            root,
            self.exact_encoded_bytes,
            self.exact_record_count,
            self.tribute_count,
        );
        let expected_closure = (
            self.catalog.header.input_chunk_list_root,
            self.catalog.header.exact_encoded_bytes,
            self.catalog.header.exact_record_count,
            self.catalog.header.tribute_count,
        );
        if actual_closure != expected_closure {
            return Err(
                ProtocolError::InvalidInvariant("input-ref catalog manifest closure").into(),
            );
        }
        let directory = fs::read_dir(&self.catalog.root)
            .map_err(|source| io_error("list input-ref catalog", &self.catalog.root, source))?;
        self.directory = Some(directory);
        self.stage = InputRefCatalogClosureStageV1::Directory;
        Ok(InputRefCatalogClosureStepV1::ReferencesClosed)
    }

    fn step_directory(&mut self) -> Result<InputRefCatalogClosureStepV1, InputRefCatalogError> {
        match self.directory.as_mut().and_then(Iterator::next) {
            Some(Ok(entry)) => self.inspect_directory_entry(entry),
            Some(Err(source)) => {
                self.stage = InputRefCatalogClosureStageV1::Complete;
                Err(io_error(
                    "read input-ref catalog",
                    &self.catalog.root,
                    source,
                ))
            }
            None => self.finish_directory(),
        }
    }

    fn finish_directory(&mut self) -> Result<InputRefCatalogClosureStepV1, InputRefCatalogError> {
        self.stage = InputRefCatalogClosureStageV1::Complete;
        if self.actual_entry_count != self.catalog.header.input_chunk_count {
            return Err(ProtocolError::InvalidInvariant("closed input-ref catalog count").into());
        }
        Ok(InputRefCatalogClosureStepV1::Complete)
    }

    fn inspect_directory_entry(
        &mut self,
        entry: fs::DirEntry,
    ) -> Result<InputRefCatalogClosureStepV1, InputRefCatalogError> {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| InputRefCatalogError::UnexpectedCatalogEntry(entry.path()))?;
        if matches!(
            name.as_str(),
            HEADER_FILE | PREPARED_FILE | STAGING_FILE | LOCK_FILE
        ) {
            return Ok(InputRefCatalogClosureStepV1::DirectoryEntryChecked);
        }
        let ordinal = name
            .strip_suffix(ENTRY_SUFFIX)
            .and_then(parse_entry_ordinal)
            .ok_or_else(|| InputRefCatalogError::UnexpectedCatalogEntry(entry.path()))?;
        let is_file = entry
            .file_type()
            .map_err(|source| io_error("inspect input-ref entry", &entry.path(), source))?
            .is_file();
        if !is_file {
            return Err(InputRefCatalogError::UnexpectedCatalogEntry(entry.path()));
        }
        if ordinal >= self.catalog.header.input_chunk_count {
            return Err(InputRefCatalogError::UnexpectedReference { ordinal });
        }
        self.actual_entry_count = self.actual_entry_count.checked_add(1).ok_or(
            ProtocolError::IntegerOverflow {
                what: "input-ref catalog count",
            },
        )?;
        Ok(InputRefCatalogClosureStepV1::DirectoryEntryChecked)
    }
}

pub struct InputRefCursor<'a> {
    catalog: &'a VerifiedInputChunkRefCatalog,
    next_ordinal: u32,
}

impl Iterator for InputRefCursor<'_> {
    type Item = Result<InputChunkRefV1, InputRefCatalogError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_ordinal >= self.catalog.header.input_chunk_count {
            return None;
        }
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        Some(self.catalog.read(ordinal))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self
            .catalog
            .header
            .input_chunk_count
            .saturating_sub(self.next_ordinal);
        let remaining = usize::try_from(remaining).unwrap_or(usize::MAX);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for InputRefCursor<'_> {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedInputChunkRefV1 {
    pub reference: InputChunkRefV1,
    pub chunk: AuthenticatedInputChunkV1,
}

pub struct VerifiedInputRefCursor<'a> {
    inner: InputRefCursor<'a>,
    reader: &'a FilesystemCasReader,
    bundle: &'a ProtocolBundleV1,
    integrity_only: bool,
}

impl Iterator for VerifiedInputRefCursor<'_> {
    type Item = Result<VerifiedInputChunkRefV1, InputRefCatalogError>;

    fn next(&mut self) -> Option<Self::Item> {
        let reference = match self.inner.next()? {
            Ok(reference) => reference,
            Err(error) => return Some(Err(error)),
        };
        Some(if self.integrity_only {
            self.inner
                .catalog
                .verify_reference_integrity(reference, self.reader, self.bundle)
        } else {
            self.inner
                .catalog
                .verify_reference(reference, self.reader, self.bundle)
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for VerifiedInputRefCursor<'_> {}

#[derive(Debug, Error)]
pub enum InputRefCatalogError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error("input-ref catalog authority differs from its immutable header")]
    AuthorityMismatch,
    #[error("input-ref catalog immutable header is missing")]
    MissingHeader,
    #[error("input-ref catalog is permanently abstained")]
    Abstained,
    #[error("input-ref catalog was reopened with read-only authority")]
    ReadOnly,
    #[error("input reference {ordinal} is missing")]
    MissingReference { ordinal: u32 },
    #[error("input reference {ordinal} lies outside the manifest")]
    UnexpectedReference { ordinal: u32 },
    #[error("input reference ordinal gap: expected {expected}, got {actual}")]
    OrdinalGap { expected: u32, actual: u32 },
    #[error("input reference kind regressed at ordinal {ordinal}")]
    KindOrder { ordinal: u32 },
    #[error("input reference {ordinal} conflicts with the durable admission")]
    ConflictingReference { ordinal: u32 },
    #[error("input reference ordinal mismatch: requested {requested}, encoded {encoded}")]
    OrdinalBinding { requested: u32, encoded: u32 },
    #[error("unexpected input-ref catalog entry at {0}")]
    UnexpectedCatalogEntry(PathBuf),
    #[error("unsafe input-ref catalog path at {0}")]
    UnsafePath(PathBuf),
    #[error("ambiguous temporary input-ref file remains at {0}")]
    AmbiguousTemporary(PathBuf),
    #[error("input-ref catalog lock is already held at {0}")]
    LockHeld(PathBuf),
    #[error("input-ref catalog object exceeds its bound")]
    ObjectTooLarge,
    #[error("input-ref catalog object has an invalid envelope")]
    InvalidEnvelope,
    #[error("input-ref catalog I/O failed during {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub(super) fn read_reference(
    root: &Path,
    limits: &SchemaLimits,
    ordinal: u32,
) -> Result<InputChunkRefV1, InputRefCatalogError> {
    let path = entry_path(root, ordinal);
    if !path_exists(&path)? {
        return Err(InputRefCatalogError::MissingReference { ordinal });
    }
    let encoded = INPUT_REF_STORAGE_PROFILE.read_bounded(&path, catalog_byte_cap(limits))?;
    let reference = decode_entry(&encoded, limits)?;
    if reference.ordinal != ordinal {
        return Err(InputRefCatalogError::OrdinalBinding {
            requested: ordinal,
            encoded: reference.ordinal,
        });
    }
    Ok(reference)
}

pub(super) fn entry_path(root: &Path, ordinal: u32) -> PathBuf {
    root.join(format!("{ordinal:010}{ENTRY_SUFFIX}"))
}

pub(super) fn persist_conflict(
    root: &Path,
    ordinal: u32,
    existing: &[u8],
    candidate: &[u8],
) -> Result<(), InputRefCatalogError> {
    let mut marker = Vec::with_capacity(8 + 4 + 32 + 32);
    marker.extend_from_slice(&CONFLICT_MAGIC);
    marker.extend_from_slice(&ordinal.to_be_bytes());
    marker.extend_from_slice(keccak256(existing).as_slice());
    marker.extend_from_slice(keccak256(candidate).as_slice());
    INPUT_REF_STORAGE_PROFILE.persist_atomic(root, &root.join(CONFLICT_FILE), &marker)?;
    Ok(())
}

fn encode_header(
    header: &InputRefCatalogHeaderV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, InputRefCatalogError> {
    let mut body = CanonicalWriter::new(limits.codec);
    encode_object_ref(&mut body, &header.manifest_ref)?;
    body.write_b256(header.manifest_hash)?;
    body.write_b256(header.protocol_bundle_hash)?;
    body.write_b256(header.job_id)?;
    body.write_u32(header.attempt)?;
    body.write_u32(header.input_chunk_count)?;
    body.write_b256(header.input_chunk_list_root)?;
    body.write_u64(header.exact_encoded_bytes)?;
    body.write_u32(header.exact_record_count)?;
    body.write_u32(header.tribute_count)?;
    Ok(encode_envelope(HEADER_MAGIC, &body.into_bytes()))
}

fn expected_header(
    manifest_ref: &CasObjectRefV1,
    manifest: &InputManifestV1,
    limits: &SchemaLimits,
) -> Result<InputRefCatalogHeaderV1, InputRefCatalogError> {
    manifest.validate_semantics(limits)?;
    if manifest_ref.expected_ocb1_kind != Some(ObjectKind::InputManifestV1.tag())
        || manifest_ref.transport_digest.is_zero()
        || manifest_ref.encoded_bytes == 0
    {
        return Err(
            ProtocolError::InvalidInvariant("input-ref catalog manifest descriptor").into(),
        );
    }
    Ok(InputRefCatalogHeaderV1 {
        manifest_ref: manifest_ref.clone(),
        manifest_hash: manifest.manifest_hash(limits)?,
        protocol_bundle_hash: manifest.protocol_bundle_hash,
        job_id: manifest.job_id,
        attempt: manifest.attempt,
        input_chunk_count: manifest.input_chunk_count,
        input_chunk_list_root: manifest.input_chunk_list_root,
        exact_encoded_bytes: manifest.exact_encoded_bytes,
        exact_record_count: manifest.exact_record_count,
        tribute_count: manifest.tribute_count,
    })
}

fn decode_header(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<InputRefCatalogHeaderV1, InputRefCatalogError> {
    let body = decode_envelope(encoded, HEADER_MAGIC)?;
    let mut input = CanonicalReader::new(body, limits.codec)?;
    let header = InputRefCatalogHeaderV1 {
        manifest_ref: decode_object_ref(&mut input)?,
        manifest_hash: input.read_b256()?,
        protocol_bundle_hash: input.read_b256()?,
        job_id: input.read_b256()?,
        attempt: input.read_u32()?,
        input_chunk_count: input.read_u32()?,
        input_chunk_list_root: input.read_b256()?,
        exact_encoded_bytes: input.read_u64()?,
        exact_record_count: input.read_u32()?,
        tribute_count: input.read_u32()?,
    };
    input.finish()?;
    if encode_header(&header, limits)? != encoded {
        return Err(InputRefCatalogError::InvalidEnvelope);
    }
    Ok(header)
}

pub(super) fn encode_entry(canonical_reference: &[u8]) -> Vec<u8> {
    encode_envelope(ENTRY_MAGIC, canonical_reference)
}

fn decode_entry(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<InputChunkRefV1, InputRefCatalogError> {
    let body = decode_envelope(encoded, ENTRY_MAGIC)?;
    let reference = InputChunkRefV1::decode_canonical_record(body, limits)?;
    if encode_entry(&reference.encode_canonical_record(limits)?) != encoded {
        return Err(InputRefCatalogError::InvalidEnvelope);
    }
    Ok(reference)
}

pub(super) fn catalog_byte_cap(limits: &SchemaLimits) -> usize {
    limits
        .codec
        .max_body_bytes
        .saturating_add(HEADER_MAGIC.len() + B256::len_bytes())
}

pub(super) fn create_private_directory(path: &Path) -> Result<(), InputRefCatalogError> {
    reject_symlink_ancestors(path)?;
    fs::create_dir_all(path).map_err(|source| io_error("create directory", path, source))?;
    reject_symlink_ancestors(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(InputRefCatalogError::UnsafePath(path.to_path_buf()));
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE))
        .map_err(|source| io_error("set directory permissions", path, source))
}

fn inspect_private_directory(path: &Path) -> Result<(), InputRefCatalogError> {
    reject_symlink_ancestors(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(InputRefCatalogError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn reject_symlink_ancestors(path: &Path) -> Result<(), InputRefCatalogError> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(InputRefCatalogError::UnsafePath(ancestor.to_path_buf()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("inspect catalog ancestor", ancestor, source)),
        }
    }
    Ok(())
}

pub(super) fn io_error(
    operation: &'static str,
    path: &Path,
    source: std::io::Error,
) -> InputRefCatalogError {
    InputRefCatalogError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
