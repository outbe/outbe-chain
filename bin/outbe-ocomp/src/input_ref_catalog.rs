//! Durable preimages for the ordered input-reference commitment.
//!
//! `InputManifestV1` remains the authority. This local catalog only makes its
//! exact `InputChunkRefV1` preimages discoverable after a cold restart without
//! scanning CAS or retaining a population-sized in-memory vector.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
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
    cas::{CasError, FilesystemCasReader},
    input_artifacts::{decode_verified_input_chunk, derive_input_chunk_ref},
};

const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const HEADER_MAGIC: [u8; 8] = *b"OUTBICH1";
const ENTRY_MAGIC: [u8; 8] = *b"OUTBICR1";
const CONFLICT_MAGIC: [u8; 8] = *b"OUTBICF1";
const HEADER_FILE: &str = "catalog.header";
const LOCK_FILE: &str = "catalog.lock";
const CONFLICT_FILE: &str = "catalog.abstained";
const ENTRY_SUFFIX: &str = ".input-ref";
const TEMP_SUFFIX: &str = ".tmp";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputRefAdmissionOutcome {
    NewlyAdmitted,
    ExactReplay,
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
    _lock: CatalogLock,
}

impl VerifiedInputChunkRefCatalog {
    pub fn open(
        root: impl AsRef<Path>,
        manifest_ref: &CasObjectRefV1,
        manifest: &InputManifestV1,
        limits: SchemaLimits,
        list_limits: OrderedListLimits,
    ) -> Result<Self, InputRefCatalogError> {
        let expected = expected_header(manifest_ref, manifest, &limits)?;
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let lock = CatalogLock::acquire(&root)?;
        reject_orphaned_temps(&root)?;
        let header_path = root.join(HEADER_FILE);
        if path_exists(&header_path)? {
            let actual = decode_header(
                &read_bounded(&header_path, catalog_byte_cap(&limits))?,
                &limits,
            )?;
            if actual != expected {
                return Err(InputRefCatalogError::AuthorityMismatch);
            }
        } else {
            persist_atomic(&root, &header_path, &encode_header(&expected, &limits)?)?;
        }
        let abstained = path_exists(&root.join(CONFLICT_FILE))?;
        Ok(Self {
            root,
            limits,
            _list_limits: list_limits,
            header: expected,
            abstained,
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
        let lock = CatalogLock::acquire(&root)?;
        reject_orphaned_temps(&root)?;
        let header_path = root.join(HEADER_FILE);
        if !path_exists(&header_path)? {
            return Err(InputRefCatalogError::MissingHeader);
        }
        let header = decode_header(
            &read_bounded(&header_path, catalog_byte_cap(&limits))?,
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
            _lock: lock,
        })
    }

    pub fn admit(
        &mut self,
        reference: &InputChunkRefV1,
    ) -> Result<InputRefAdmissionOutcome, InputRefCatalogError> {
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
            let existing = read_bounded(&path, catalog_byte_cap(&self.limits))?;
            if existing == encoded {
                return Ok(InputRefAdmissionOutcome::ExactReplay);
            }
            self.latch_conflict(reference.ordinal, &existing, &encoded)?;
            return Err(InputRefCatalogError::ConflictingReference {
                ordinal: reference.ordinal,
            });
        }
        persist_atomic(&self.root, &path, &encoded)?;
        Ok(InputRefAdmissionOutcome::NewlyAdmitted)
    }

    pub fn exact_cursor(&self) -> Result<InputRefCursor<'_>, InputRefCatalogError> {
        self.require_active()?;
        self.verify_closed_catalog()?;
        Ok(InputRefCursor {
            catalog: self,
            next_ordinal: 0,
        })
    }

    pub fn exact_verified_cursor<'a>(
        &'a self,
        reader: &'a FilesystemCasReader,
        bundle: &'a ProtocolBundleV1,
    ) -> Result<VerifiedInputRefCursor<'a>, InputRefCatalogError> {
        Ok(VerifiedInputRefCursor {
            inner: self.exact_cursor()?,
            reader,
            bundle,
        })
    }

    fn verify_closed_catalog(&self) -> Result<(), InputRefCatalogError> {
        let mut actual_count = 0_u32;
        let entries = fs::read_dir(&self.root)
            .map_err(|source| io_error("list input-ref catalog", &self.root, source))?;
        for entry in entries {
            let entry =
                entry.map_err(|source| io_error("read input-ref catalog", &self.root, source))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| InputRefCatalogError::UnexpectedCatalogEntry(entry.path()))?;
            if name == HEADER_FILE || name == LOCK_FILE {
                continue;
            }
            let Some(prefix) = name.strip_suffix(ENTRY_SUFFIX) else {
                return Err(InputRefCatalogError::UnexpectedCatalogEntry(entry.path()));
            };
            let ordinal = parse_entry_ordinal(prefix)
                .ok_or_else(|| InputRefCatalogError::UnexpectedCatalogEntry(entry.path()))?;
            if !entry
                .file_type()
                .map_err(|source| io_error("inspect input-ref entry", &entry.path(), source))?
                .is_file()
            {
                return Err(InputRefCatalogError::UnexpectedCatalogEntry(entry.path()));
            }
            if ordinal >= self.header.input_chunk_count {
                return Err(InputRefCatalogError::UnexpectedReference { ordinal });
            }
            actual_count = actual_count
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "input-ref catalog count",
                })?;
        }
        if actual_count != self.header.input_chunk_count {
            for ordinal in 0..self.header.input_chunk_count {
                if !path_exists(&self.entry_path(ordinal))? {
                    return Err(InputRefCatalogError::MissingReference { ordinal });
                }
            }
            return Err(ProtocolError::InvalidInvariant("closed input-ref catalog count").into());
        }

        let mut root = StreamingOrderedListRoot::new(
            ListKind::InputChunkReferences,
            self.header.input_chunk_count,
        )?;
        let mut exact_encoded_bytes = 0_u64;
        let mut exact_record_count = 0_u32;
        let mut tribute_count = 0_u32;
        let mut previous_kind = None;
        for ordinal in 0..self.header.input_chunk_count {
            let reference = self.read(ordinal)?;
            if let Some(kind) = previous_kind {
                if kind > reference.kind {
                    return Err(
                        ProtocolError::InvalidInvariant("input-ref catalog kind order").into(),
                    );
                }
            }
            previous_kind = Some(reference.kind);
            exact_encoded_bytes = exact_encoded_bytes
                .checked_add(reference.encoded_bytes)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "input-ref encoded bytes",
                })?;
            exact_record_count = exact_record_count
                .checked_add(reference.record_count)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "input-ref record count",
                })?;
            if reference.kind == InputChunkKind::Tribute {
                tribute_count = tribute_count.checked_add(reference.record_count).ok_or(
                    ProtocolError::IntegerOverflow {
                        what: "input-ref Tribute count",
                    },
                )?;
            }
            root.push(
                &reference.encode_canonical_record(&self.limits)?,
                self.limits.max_bounded_bytes,
            )?;
        }
        if root.finish()? != self.header.input_chunk_list_root
            || exact_encoded_bytes != self.header.exact_encoded_bytes
            || exact_record_count != self.header.exact_record_count
            || tribute_count != self.header.tribute_count
        {
            return Err(
                ProtocolError::InvalidInvariant("input-ref catalog manifest closure").into(),
            );
        }
        Ok(())
    }

    fn read(&self, ordinal: u32) -> Result<InputChunkRefV1, InputRefCatalogError> {
        let path = self.entry_path(ordinal);
        if !path_exists(&path)? {
            return Err(InputRefCatalogError::MissingReference { ordinal });
        }
        let encoded = read_bounded(&path, catalog_byte_cap(&self.limits))?;
        let reference = decode_entry(&encoded, &self.limits)?;
        if reference.ordinal != ordinal {
            return Err(InputRefCatalogError::OrdinalBinding {
                requested: ordinal,
                encoded: reference.ordinal,
            });
        }
        Ok(reference)
    }

    fn entry_path(&self, ordinal: u32) -> PathBuf {
        self.root.join(format!("{ordinal:010}{ENTRY_SUFFIX}"))
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
        let mut marker = Vec::with_capacity(8 + 4 + 32 + 32);
        marker.extend_from_slice(&CONFLICT_MAGIC);
        marker.extend_from_slice(&ordinal.to_be_bytes());
        marker.extend_from_slice(keccak256(existing).as_slice());
        marker.extend_from_slice(keccak256(candidate).as_slice());
        persist_atomic(&self.root, &self.root.join(CONFLICT_FILE), &marker)?;
        self.abstained = true;
        Ok(())
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
}

impl Iterator for VerifiedInputRefCursor<'_> {
    type Item = Result<VerifiedInputChunkRefV1, InputRefCatalogError>;

    fn next(&mut self) -> Option<Self::Item> {
        let reference = match self.inner.next()? {
            Ok(reference) => reference,
            Err(error) => return Some(Err(error)),
        };
        Some((|| {
            let object_ref = CasObjectRefV1 {
                transport_digest: reference.transport_digest,
                encoded_bytes: reference.encoded_bytes,
                expected_ocb1_kind: Some(ObjectKind::AuthenticatedInputChunkV1.tag()),
            };
            let object = self.reader.read_verified(&object_ref)?;
            let derived = derive_input_chunk_ref(&object, self.bundle, &self.inner.catalog.limits)
                .map_err(|_| {
                    ProtocolError::InvalidInvariant("input-ref catalog CAS reference derivation")
                })?
                .reference;
            if derived != reference {
                return Err(
                    ProtocolError::InvalidInvariant("input-ref catalog CAS preimage").into(),
                );
            }
            let chunk =
                decode_verified_input_chunk(&object, self.bundle, &self.inner.catalog.limits)
                    .map_err(|_| {
                        ProtocolError::InvalidInvariant("input-ref catalog CAS chunk decoding")
                    })?;
            if chunk.protocol_bundle_hash != self.inner.catalog.header.protocol_bundle_hash
                || chunk.job_id != self.inner.catalog.header.job_id
                || chunk.kind != reference.kind
                || chunk.ordinal != reference.ordinal
            {
                return Err(
                    ProtocolError::InvalidInvariant("input-ref catalog chunk authority").into(),
                );
            }
            Ok(VerifiedInputChunkRefV1 { reference, chunk })
        })())
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
    #[error("input reference {ordinal} is missing")]
    MissingReference { ordinal: u32 },
    #[error("input reference {ordinal} lies outside the manifest")]
    UnexpectedReference { ordinal: u32 },
    #[error("input reference {ordinal} conflicts with the durable admission")]
    ConflictingReference { ordinal: u32 },
    #[error("input reference ordinal mismatch: requested {requested}, encoded {encoded}")]
    OrdinalBinding { requested: u32, encoded: u32 },
    #[error("unexpected input-ref catalog entry at {0}")]
    UnexpectedCatalogEntry(PathBuf),
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

fn encode_entry(canonical_reference: &[u8]) -> Vec<u8> {
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

fn encode_envelope(magic: [u8; 8], body: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(magic.len() + B256::len_bytes() + body.len());
    output.extend_from_slice(&magic);
    output.extend_from_slice(keccak256(body).as_slice());
    output.extend_from_slice(body);
    output
}

fn decode_envelope(encoded: &[u8], magic: [u8; 8]) -> Result<&[u8], InputRefCatalogError> {
    let body_offset = magic.len() + B256::len_bytes();
    if encoded.len() < body_offset || encoded[..magic.len()] != magic {
        return Err(InputRefCatalogError::InvalidEnvelope);
    }
    let expected = B256::from_slice(&encoded[magic.len()..body_offset]);
    let body = &encoded[body_offset..];
    if keccak256(body) != expected {
        return Err(InputRefCatalogError::InvalidEnvelope);
    }
    Ok(body)
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

fn parse_entry_ordinal(prefix: &str) -> Option<u32> {
    if prefix.len() != 10 || !prefix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let ordinal = prefix.parse::<u32>().ok()?;
    (format!("{ordinal:010}") == prefix).then_some(ordinal)
}

fn catalog_byte_cap(limits: &SchemaLimits) -> usize {
    limits
        .codec
        .max_body_bytes
        .saturating_add(HEADER_MAGIC.len() + B256::len_bytes())
}

fn create_private_directory(path: &Path) -> Result<(), InputRefCatalogError> {
    fs::create_dir_all(path).map_err(|source| io_error("create directory", path, source))?;
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(InputRefCatalogError::InvalidEnvelope);
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(DIRECTORY_MODE))
        .map_err(|source| io_error("set directory permissions", path, source))
}

fn inspect_private_directory(path: &Path) -> Result<(), InputRefCatalogError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(InputRefCatalogError::InvalidEnvelope);
    }
    Ok(())
}

fn reject_orphaned_temps(root: &Path) -> Result<(), InputRefCatalogError> {
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
            return Err(InputRefCatalogError::AmbiguousTemporary(path));
        }
    }
    Ok(())
}

fn persist_atomic(root: &Path, target: &Path, bytes: &[u8]) -> Result<(), InputRefCatalogError> {
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(InputRefCatalogError::InvalidEnvelope)?;
    let temp = root.join(format!("{file_name}{TEMP_SUFFIX}"));
    if path_exists(&temp)? {
        return Err(InputRefCatalogError::AmbiguousTemporary(temp));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&temp)
        .map_err(|source| io_error("create temporary catalog object", &temp, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error("write temporary catalog object", &temp, source))?;
    file.sync_all()
        .map_err(|source| io_error("fsync temporary catalog object", &temp, source))?;
    fs::rename(&temp, target)
        .map_err(|source| io_error("install catalog object", target, source))?;
    sync_directory(root)
}

fn read_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, InputRefCatalogError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|source| io_error("open catalog object", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect catalog object", path, source))?;
    if !metadata.file_type().is_file()
        || metadata.len() > u64::try_from(max_bytes).unwrap_or(u64::MAX)
    {
        return Err(InputRefCatalogError::ObjectTooLarge);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(
        u64::try_from(max_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .map_err(|source| io_error("read catalog object", path, source))?;
    if bytes.len() > max_bytes {
        return Err(InputRefCatalogError::ObjectTooLarge);
    }
    Ok(bytes)
}

fn path_exists(path: &Path) -> Result<bool, InputRefCatalogError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(InputRefCatalogError::InvalidEnvelope);
            }
            Ok(true)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect path", path, source)),
    }
}

fn sync_directory(path: &Path) -> Result<(), InputRefCatalogError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("fsync catalog directory", path, source))
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> InputRefCatalogError {
    InputRefCatalogError::Io {
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
    fn acquire(root: &Path) -> Result<Self, InputRefCatalogError> {
        let path = root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|source| io_error("open catalog lock", &path, source))?;
        // SAFETY: `file` owns a live descriptor for the complete flock call.
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let source = std::io::Error::last_os_error();
            if source.raw_os_error() == Some(libc::EWOULDBLOCK) {
                return Err(InputRefCatalogError::LockHeld(path));
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
