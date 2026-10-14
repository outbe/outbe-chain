use std::fs;
use std::path::{Path, PathBuf};

use outbe_ocomp_protocol::{
    input::{InputChunkKind, InputChunkRefV1, InputManifestV1},
    profile::ProtocolBundleV1,
    CanonicalReader, CanonicalWriter, CasObjectRefV1, ListKind, ObjectKind, OrderedListLimits,
    ProtocolError, SchemaLimits, StreamingOrderedListRoot,
};

use crate::{
    admission_catalog::{
        decode_envelope, encode_envelope, parse_entry_ordinal, path_exists,
        require_fresh_catalog_without_header, storage::sync_directory, CatalogLock,
        INPUT_REF_STORAGE_PROFILE,
    },
    cas::{FilesystemCas, FilesystemCasReader},
    input_artifacts::{decode_verified_input_chunk, derive_input_chunk_ref},
};

use super::{
    catalog_byte_cap, create_private_directory, decode_header, encode_entry, encode_header,
    entry_path, expected_header, io_error, persist_conflict, read_reference,
    InputRefAdmissionOutcome, InputRefCatalogError, InputRefCatalogHeaderV1,
    InputRefCatalogSubjectV1, InputRefCatalogSummaryV1, VerifiedInputChunkRefCatalog,
    CONFLICT_FILE, ENTRY_SUFFIX, HEADER_FILE, LOCK_FILE, PREPARED_FILE, STAGING_FILE,
    STAGING_MAGIC, TEMP_SUFFIX,
};

fn inspect_optional_headers_match(
    final_header: Option<&InputRefCatalogHeaderV1>,
    prepared_header: Option<&InputRefCatalogHeaderV1>,
    subject: InputRefCatalogSubjectV1,
) -> Result<(), InputRefCatalogError> {
    if (final_header.is_some(), prepared_header.is_none()) == (true, true) {
        return Err(InputRefCatalogError::AuthorityMismatch);
    }
    if final_header
        .zip(prepared_header)
        .is_some_and(|(sealed, prepared)| sealed != prepared)
    {
        return Err(InputRefCatalogError::AuthorityMismatch);
    }
    if final_header.or(prepared_header).is_some_and(|header| {
        (header.protocol_bundle_hash, header.job_id, header.attempt)
            != (
                subject.protocol_bundle_hash,
                subject.job_id,
                subject.attempt,
            )
    }) {
        return Err(InputRefCatalogError::AuthorityMismatch);
    }
    Ok(())
}

pub struct InputRefCatalogPublisher {
    root: PathBuf,
    limits: SchemaLimits,
    list_limits: OrderedListLimits,
    subject: InputRefCatalogSubjectV1,
    next_ordinal: u32,
    previous_kind: Option<InputChunkKind>,
    abstained: bool,
    prepared_header: Option<InputRefCatalogHeaderV1>,
    sealed_header: Option<InputRefCatalogHeaderV1>,
    lock: CatalogLock,
}

pub struct PreparedInputRefCatalogPublisher {
    root: PathBuf,
    limits: SchemaLimits,
    list_limits: OrderedListLimits,
    subject: InputRefCatalogSubjectV1,
    summary: InputRefCatalogSummaryV1,
    prepared_header: Option<InputRefCatalogHeaderV1>,
    sealed_header: Option<InputRefCatalogHeaderV1>,
    lock: CatalogLock,
}

impl InputRefCatalogPublisher {
    pub fn open_or_resume(
        root: impl AsRef<Path>,
        subject: InputRefCatalogSubjectV1,
        limits: SchemaLimits,
        list_limits: OrderedListLimits,
    ) -> Result<Self, InputRefCatalogError> {
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root)?;
        let lock = INPUT_REF_STORAGE_PROFILE.acquire_lock(&root)?;
        recover_staging_temps(&root)?;
        let final_header = read_optional_header(&root.join(HEADER_FILE), &limits)?;
        let prepared_header = read_optional_header(&root.join(PREPARED_FILE), &limits)?;
        inspect_optional_headers_match(final_header.as_ref(), prepared_header.as_ref(), subject)?;
        ensure_staging_subject(&root, subject, &limits)?;
        let abstained = path_exists(&root.join(CONFLICT_FILE))?;
        let (next_ordinal, previous_kind) = scan_staged_entries(&root, &limits)?;
        if let Some(header) = final_header.as_ref().or(prepared_header.as_ref()) {
            if next_ordinal > header.input_chunk_count {
                return Err(InputRefCatalogError::UnexpectedReference {
                    ordinal: header.input_chunk_count,
                });
            }
        }
        Ok(Self {
            root,
            limits,
            list_limits,
            subject,
            next_ordinal,
            previous_kind,
            abstained,
            prepared_header,
            sealed_header: final_header,
            lock,
        })
    }

    pub fn append(
        &mut self,
        reference: &InputChunkRefV1,
    ) -> Result<InputRefAdmissionOutcome, InputRefCatalogError> {
        self.require_active()?;
        let canonical = reference.encode_canonical_record(&self.limits)?;
        if reference.ordinal < self.next_ordinal {
            return self.replay_or_conflict(reference.ordinal, &canonical);
        }
        if reference.ordinal > self.next_ordinal {
            return Err(InputRefCatalogError::OrdinalGap {
                expected: self.next_ordinal,
                actual: reference.ordinal,
            });
        }
        self.validate_append_bounds(reference)?;
        let path = entry_path(&self.root, reference.ordinal);
        INPUT_REF_STORAGE_PROFILE.persist_atomic(&self.root, &path, &encode_entry(&canonical))?;
        self.next_ordinal =
            self.next_ordinal
                .checked_add(1)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "staged input-ref ordinal",
                })?;
        self.previous_kind = Some(reference.kind);
        Ok(InputRefAdmissionOutcome::NewlyAdmitted)
    }

    fn replay_or_conflict(
        &mut self,
        ordinal: u32,
        canonical: &[u8],
    ) -> Result<InputRefAdmissionOutcome, InputRefCatalogError> {
        let existing = INPUT_REF_STORAGE_PROFILE.read_bounded(
            &entry_path(&self.root, ordinal),
            catalog_byte_cap(&self.limits),
        )?;
        let candidate = encode_entry(canonical);
        if existing == candidate {
            return Ok(InputRefAdmissionOutcome::ExactReplay);
        }
        persist_conflict(&self.root, ordinal, &existing, &candidate)?;
        self.abstained = true;
        Err(InputRefCatalogError::ConflictingReference { ordinal })
    }

    fn validate_append_bounds(
        &self,
        reference: &InputChunkRefV1,
    ) -> Result<(), InputRefCatalogError> {
        if self
            .sealed_header
            .as_ref()
            .or(self.prepared_header.as_ref())
            .is_some_and(|header| reference.ordinal >= header.input_chunk_count)
        {
            return Err(InputRefCatalogError::UnexpectedReference {
                ordinal: reference.ordinal,
            });
        }
        if self
            .previous_kind
            .is_some_and(|previous| previous > reference.kind)
        {
            return Err(InputRefCatalogError::KindOrder {
                ordinal: reference.ordinal,
            });
        }
        Ok(())
    }

    pub fn prepare(
        self,
        reader: &FilesystemCasReader,
        bundle: &ProtocolBundleV1,
    ) -> Result<PreparedInputRefCatalogPublisher, InputRefCatalogError> {
        self.prepare_observing(reader, bundle, || {})
    }

    pub fn prepare_observing(
        self,
        reader: &FilesystemCasReader,
        bundle: &ProtocolBundleV1,
        on_progress: impl Fn(),
    ) -> Result<PreparedInputRefCatalogPublisher, InputRefCatalogError> {
        self.require_active()?;
        if bundle.protocol_bundle_hash(&self.limits)? != self.subject.protocol_bundle_hash {
            return Err(InputRefCatalogError::AuthorityMismatch);
        }
        let (entry_count, _) =
            scan_staged_entries_observing(&self.root, &self.limits, &on_progress)?;
        if entry_count != self.next_ordinal {
            return Err(InputRefCatalogError::MissingReference {
                ordinal: entry_count.min(self.next_ordinal),
            });
        }
        let summary = self.summarize_staged_entries(reader, bundle, &on_progress)?;
        self.validate_summary_against_headers(summary)?;
        Ok(PreparedInputRefCatalogPublisher {
            root: self.root,
            limits: self.limits,
            list_limits: self.list_limits,
            subject: self.subject,
            summary,
            prepared_header: self.prepared_header,
            sealed_header: self.sealed_header,
            lock: self.lock,
        })
    }

    fn summarize_staged_entries(
        &self,
        reader: &FilesystemCasReader,
        bundle: &ProtocolBundleV1,
        on_progress: &impl Fn(),
    ) -> Result<InputRefCatalogSummaryV1, InputRefCatalogError> {
        let mut root =
            StreamingOrderedListRoot::new(ListKind::InputChunkReferences, self.next_ordinal)?;
        let mut exact_encoded_bytes = 0_u64;
        let mut exact_record_count = 0_u32;
        let mut tribute_count = 0_u32;
        let mut previous_kind = None;
        for ordinal in 0..self.next_ordinal {
            let reference = read_reference(&self.root, &self.limits, ordinal)?;
            if previous_kind.is_some_and(|kind| kind > reference.kind) {
                return Err(InputRefCatalogError::KindOrder { ordinal });
            }
            previous_kind = Some(reference.kind);
            root.push(
                &reference.encode_canonical_record(&self.limits)?,
                self.limits.max_bounded_bytes,
            )?;
            exact_encoded_bytes = exact_encoded_bytes
                .checked_add(reference.encoded_bytes)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "staged input-ref encoded bytes",
                })?;
            exact_record_count = exact_record_count
                .checked_add(reference.record_count)
                .ok_or(ProtocolError::IntegerOverflow {
                    what: "staged input-ref record count",
                })?;
            if reference.kind == InputChunkKind::Tribute {
                tribute_count = tribute_count.checked_add(reference.record_count).ok_or(
                    ProtocolError::IntegerOverflow {
                        what: "staged input-ref Tribute count",
                    },
                )?;
            }
            verify_staged_reference(&reference, self.subject, reader, bundle, &self.limits)?;
            on_progress();
        }
        Ok(InputRefCatalogSummaryV1 {
            input_chunk_count: self.next_ordinal,
            input_chunk_list_root: root.finish()?,
            exact_encoded_bytes,
            exact_record_count,
            tribute_count,
        })
    }

    fn validate_summary_against_headers(
        &self,
        summary: InputRefCatalogSummaryV1,
    ) -> Result<(), InputRefCatalogError> {
        if [self.sealed_header.as_ref(), self.prepared_header.as_ref()]
            .into_iter()
            .flatten()
            .any(|header| !summary_matches_header(summary, header))
        {
            return Err(InputRefCatalogError::AuthorityMismatch);
        }
        Ok(())
    }

    fn require_active(&self) -> Result<(), InputRefCatalogError> {
        if self.abstained {
            Err(InputRefCatalogError::Abstained)
        } else {
            Ok(())
        }
    }
}

impl PreparedInputRefCatalogPublisher {
    #[must_use]
    pub const fn summary(&self) -> InputRefCatalogSummaryV1 {
        self.summary
    }

    pub fn seal(
        self,
        cas: &FilesystemCas,
        manifest_ref: &CasObjectRefV1,
        bundle: &ProtocolBundleV1,
    ) -> Result<VerifiedInputChunkRefCatalog, InputRefCatalogError> {
        let manifest_object = cas.read_verified(manifest_ref)?;
        let manifest = InputManifestV1::decode_canonical(manifest_object.bytes(), &self.limits)?;
        manifest.validate_against_bundle(bundle, &self.limits)?;
        let manifest_tuple = (
            manifest.protocol_bundle_hash,
            manifest.job_id,
            manifest.attempt,
            manifest.input_chunk_count,
            manifest.input_chunk_list_root,
            manifest.exact_encoded_bytes,
            manifest.exact_record_count,
            manifest.tribute_count,
        );
        let expected_tuple = (
            self.subject.protocol_bundle_hash,
            self.subject.job_id,
            self.subject.attempt,
            self.summary.input_chunk_count,
            self.summary.input_chunk_list_root,
            self.summary.exact_encoded_bytes,
            self.summary.exact_record_count,
            self.summary.tribute_count,
        );
        if manifest_tuple != expected_tuple {
            return Err(InputRefCatalogError::AuthorityMismatch);
        }
        let expected = expected_header(manifest_ref, &manifest, &self.limits)?;
        persist_or_verify_header(
            &self.root,
            PREPARED_FILE,
            self.prepared_header.as_ref(),
            &expected,
            &self.limits,
        )?;
        persist_or_verify_header(
            &self.root,
            HEADER_FILE,
            self.sealed_header.as_ref(),
            &expected,
            &self.limits,
        )?;
        Ok(VerifiedInputChunkRefCatalog {
            root: self.root,
            limits: self.limits,
            _list_limits: self.list_limits,
            header: expected,
            abstained: false,
            writable: true,
            _lock: self.lock,
        })
    }
}

fn persist_or_verify_header(
    root: &Path,
    file_name: &str,
    existing: Option<&InputRefCatalogHeaderV1>,
    expected: &InputRefCatalogHeaderV1,
    limits: &SchemaLimits,
) -> Result<(), InputRefCatalogError> {
    if let Some(header) = existing {
        if header != expected {
            return Err(InputRefCatalogError::AuthorityMismatch);
        }
        return Ok(());
    }
    INPUT_REF_STORAGE_PROFILE.persist_atomic(
        root,
        &root.join(file_name),
        &encode_header(expected, limits)?,
    )?;
    Ok(())
}

fn read_optional_header(
    path: &Path,
    limits: &SchemaLimits,
) -> Result<Option<InputRefCatalogHeaderV1>, InputRefCatalogError> {
    if !path_exists(path)? {
        return Ok(None);
    }
    Ok(Some(decode_header(
        &INPUT_REF_STORAGE_PROFILE.read_bounded(path, catalog_byte_cap(limits))?,
        limits,
    )?))
}

fn ensure_staging_subject(
    root: &Path,
    subject: InputRefCatalogSubjectV1,
    limits: &SchemaLimits,
) -> Result<(), InputRefCatalogError> {
    let staging_path = root.join(STAGING_FILE);
    if path_exists(&staging_path)? {
        let actual = decode_staging_subject(
            &INPUT_REF_STORAGE_PROFILE.read_bounded(&staging_path, catalog_byte_cap(limits))?,
            limits,
        )?;
        if actual != subject {
            return Err(InputRefCatalogError::AuthorityMismatch);
        }
        return Ok(());
    }
    require_fresh_catalog_without_header(root)?;
    INPUT_REF_STORAGE_PROFILE.persist_atomic(
        root,
        &staging_path,
        &encode_staging_subject(subject, limits)?,
    )?;
    Ok(())
}

fn encode_staging_subject(
    subject: InputRefCatalogSubjectV1,
    limits: &SchemaLimits,
) -> Result<Vec<u8>, InputRefCatalogError> {
    let mut body = CanonicalWriter::new(limits.codec);
    body.write_b256(subject.protocol_bundle_hash)?;
    body.write_b256(subject.job_id)?;
    body.write_u32(subject.attempt)?;
    Ok(encode_envelope(STAGING_MAGIC, &body.into_bytes()))
}

fn decode_staging_subject(
    encoded: &[u8],
    limits: &SchemaLimits,
) -> Result<InputRefCatalogSubjectV1, InputRefCatalogError> {
    let body = decode_envelope(encoded, STAGING_MAGIC)?;
    let mut input = CanonicalReader::new(body, limits.codec)?;
    let subject = InputRefCatalogSubjectV1 {
        protocol_bundle_hash: input.read_b256()?,
        job_id: input.read_b256()?,
        attempt: input.read_u32()?,
    };
    input.finish()?;
    if encode_staging_subject(subject, limits)? != encoded {
        return Err(InputRefCatalogError::InvalidEnvelope);
    }
    Ok(subject)
}

fn scan_staged_entries(
    root: &Path,
    limits: &SchemaLimits,
) -> Result<(u32, Option<InputChunkKind>), InputRefCatalogError> {
    scan_staged_entries_observing(root, limits, &|| {})
}

fn scan_staged_entries_observing(
    root: &Path,
    limits: &SchemaLimits,
    on_progress: &impl Fn(),
) -> Result<(u32, Option<InputChunkKind>), InputRefCatalogError> {
    let entries =
        fs::read_dir(root).map_err(|source| io_error("list catalog directory", root, source))?;
    let mut count = 0_u32;
    for entry in entries {
        let entry = entry.map_err(|source| io_error("read catalog directory", root, source))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| InputRefCatalogError::UnexpectedCatalogEntry(entry.path()))?;
        if matches!(
            name.as_str(),
            HEADER_FILE | PREPARED_FILE | STAGING_FILE | LOCK_FILE | CONFLICT_FILE
        ) {
            continue;
        }
        name.strip_suffix(ENTRY_SUFFIX)
            .and_then(parse_entry_ordinal)
            .ok_or_else(|| InputRefCatalogError::UnexpectedCatalogEntry(entry.path()))?;
        if !entry
            .file_type()
            .map_err(|source| io_error("inspect input-ref entry", &entry.path(), source))?
            .is_file()
        {
            return Err(InputRefCatalogError::UnexpectedCatalogEntry(entry.path()));
        }
        count = count.checked_add(1).ok_or(ProtocolError::IntegerOverflow {
            what: "staged input-ref entry count",
        })?;
        on_progress();
    }
    let mut previous_kind = None;
    for ordinal in 0..count {
        let reference = read_reference(root, limits, ordinal)?;
        if previous_kind.is_some_and(|kind| kind > reference.kind) {
            return Err(InputRefCatalogError::KindOrder { ordinal });
        }
        previous_kind = Some(reference.kind);
        on_progress();
    }
    Ok((count, previous_kind))
}

fn verify_staged_reference(
    reference: &InputChunkRefV1,
    subject: InputRefCatalogSubjectV1,
    reader: &FilesystemCasReader,
    bundle: &ProtocolBundleV1,
    limits: &SchemaLimits,
) -> Result<(), InputRefCatalogError> {
    let object_ref = CasObjectRefV1 {
        transport_digest: reference.transport_digest,
        encoded_bytes: reference.encoded_bytes,
        expected_ocb1_kind: Some(ObjectKind::AuthenticatedInputChunkV1.tag()),
    };
    let object = reader.read_verified(&object_ref)?;
    let derived = derive_input_chunk_ref(&object, bundle, limits)
        .map_err(|_| ProtocolError::InvalidInvariant("staged input-ref CAS reference derivation"))?
        .reference;
    if &derived != reference {
        return Err(ProtocolError::InvalidInvariant("staged input-ref CAS preimage").into());
    }
    let chunk = decode_verified_input_chunk(&object, bundle, limits)
        .map_err(|_| ProtocolError::InvalidInvariant("staged input-ref chunk decoding"))?;
    let chunk_tuple = (
        chunk.protocol_bundle_hash,
        chunk.job_id,
        chunk.kind,
        chunk.ordinal,
    );
    let expected_tuple = (
        subject.protocol_bundle_hash,
        subject.job_id,
        reference.kind,
        reference.ordinal,
    );
    if chunk_tuple != expected_tuple {
        return Err(ProtocolError::InvalidInvariant("staged input-ref chunk authority").into());
    }
    Ok(())
}

fn summary_matches_header(
    summary: InputRefCatalogSummaryV1,
    header: &InputRefCatalogHeaderV1,
) -> bool {
    let summary_tuple = (
        summary.input_chunk_count,
        summary.input_chunk_list_root,
        summary.exact_encoded_bytes,
        summary.exact_record_count,
        summary.tribute_count,
    );
    let header_tuple = (
        header.input_chunk_count,
        header.input_chunk_list_root,
        header.exact_encoded_bytes,
        header.exact_record_count,
        header.tribute_count,
    );
    summary_tuple == header_tuple
}

fn recover_staging_temps(root: &Path) -> Result<(), InputRefCatalogError> {
    let entries =
        fs::read_dir(root).map_err(|source| io_error("list catalog directory", root, source))?;
    let mut removed = false;
    for entry in entries {
        let entry = entry.map_err(|source| io_error("read catalog directory", root, source))?;
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(TEMP_SUFFIX))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io_error("inspect temporary catalog object", &path, source))?;
        if (
            metadata.file_type().is_file(),
            metadata.file_type().is_symlink(),
        ) != (true, false)
        {
            return Err(InputRefCatalogError::AmbiguousTemporary(path));
        }
        fs::remove_file(&path)
            .map_err(|source| io_error("remove temporary catalog object", &path, source))?;
        removed = true;
    }
    if removed {
        sync_directory(root)?;
    }
    Ok(())
}
