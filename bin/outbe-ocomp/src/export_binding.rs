//! Durable supervisor authority for one published input manifest.
//!
//! The exporter may construct CAS objects, but they do not become computation
//! authority by existing on disk. This store binds these items into one
//! content-addressed record:
//!
//! - the exact journal record of the finalized job
//! - the local reconstruction lease
//! - the closed manifest/catalog
//! - the publication receipt
//!
//! A cold restart must reload and revalidate the same bytes before planning or
//! finalization.

pub(crate) mod storage;

use std::path::{Path, PathBuf};

use alloy_primitives::{keccak256, B256};
use outbe_ocomp_protocol::{
    control::FinalizedJobSpecV1,
    input::{CheckpointIdentityV1, InputManifestV1},
    intent::JobIntentV1,
    profile::ProtocolBundleV1,
    CasObjectRefV1, CommitSnapshotExportV1, ObjectKind, ProtocolError, SchemaLimits,
    SnapshotExportCommittedV1,
};
use thiserror::Error;

use crate::{
    cas::{CasError, FilesystemCas, FilesystemCasReader},
    export_receipt::VerifiedExportReceipt,
    input_ref_catalog::{InputRefCatalogError, VerifiedInputChunkRefCatalog},
    supervisor::DiscoveryRecord,
};

use self::storage::{
    create_private_directory, decode_checkpoint_at, decode_locator_with_magic,
    decode_manifest_ref_at, encode_checkpoint, encode_locator_with_magic, encode_manifest_ref,
    inspect_known_entries, inspect_private_directory, latch_conflict_file, path_exists,
    persist_atomic, read_b256, read_bounded, read_u32, read_u64, verify_envelope_checksum,
    ExclusiveFileLock, FORMAT_VERSION, LOCATOR_FIXED_BYTES,
};

const BINDING_MAGIC: [u8; 8] = *b"OUTBEMB1";
const LOCATOR_MAGIC: [u8; 8] = *b"OUTBEML1";
const CONFLICT_MAGIC: [u8; 8] = *b"OUTBEMC1";
const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
const LOCATOR_FILE: &str = "binding.ref";
const LOCATOR_TEMP_FILE: &str = "binding.ref.tmp";
const CONFLICT_FILE: &str = "binding.abstained";
const LOCK_FILE: &str = "binding.lock";
const KNOWN_ENTRIES: &[&str] = &[LOCATOR_FILE, LOCATOR_TEMP_FILE, CONFLICT_FILE, LOCK_FILE];
const BINDING_CHECKPOINT_OFFSET: usize = 142;
const BINDING_MANIFEST_REF_OFFSET: usize = 248;
const BINDING_MANIFEST_HASH_OFFSET: usize = 290;
const BINDING_EXPORTED_GENERATION_OFFSET: usize = 322;
const BINDING_EXPORT_RECORD_HASH_OFFSET: usize = 330;
const BINDING_CHECKSUM_OFFSET: usize = 362;
const BINDING_FIXED_BYTES: usize = BINDING_CHECKSUM_OFFSET + 32;

pub struct ExportBindingCandidate<'a> {
    pub discovery: &'a DiscoveryRecord,
    pub job_id: B256,
    pub source_pin_generation: u64,
    pub lease_generation: u64,
    pub checkpoint: &'a CheckpointIdentityV1,
    pub manifest_ref: &'a CasObjectRefV1,
    pub committed: &'a SnapshotExportCommittedV1,
    pub bundle: &'a ProtocolBundleV1,
    pub input_refs: &'a VerifiedInputChunkRefCatalog,
}

pub struct ExportReceiptBindingCandidate<'a> {
    pub discovery: &'a DiscoveryRecord,
    pub receipt: &'a VerifiedExportReceipt,
    pub bundle: &'a ProtocolBundleV1,
    pub input_refs: &'a VerifiedInputChunkRefCatalog,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportBindingSealOutcome {
    NewlySealed,
    ExactReplay,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExportedManifestBindingV1 {
    discovery_generation: u64,
    finalized_cursor: u64,
    finalized_job_spec_hash: B256,
    job_id: B256,
    attempt: u32,
    protocol_bundle_hash: B256,
    source_pin_generation: u64,
    lease_generation: u64,
    checkpoint: CheckpointIdentityV1,
    manifest_ref: CasObjectRefV1,
    manifest_hash: B256,
    exported_pin_generation: u64,
    export_record_hash: B256,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedExportedManifestBinding {
    binding_ref: CasObjectRefV1,
    binding: ExportedManifestBindingV1,
    manifest: InputManifestV1,
}

impl VerifiedExportedManifestBinding {
    #[must_use]
    pub fn binding_ref(&self) -> CasObjectRefV1 {
        self.binding_ref.clone()
    }

    #[must_use]
    pub fn manifest_ref(&self) -> CasObjectRefV1 {
        self.binding.manifest_ref.clone()
    }

    #[must_use]
    pub const fn manifest(&self) -> &InputManifestV1 {
        &self.manifest
    }

    #[must_use]
    pub const fn job_id(&self) -> B256 {
        self.binding.job_id
    }

    #[must_use]
    pub const fn export_record_hash(&self) -> B256 {
        self.binding.export_record_hash
    }

    #[must_use]
    pub fn commit_replay_request(&self) -> CommitSnapshotExportV1 {
        CommitSnapshotExportV1 {
            job_id: self.binding.job_id,
            pin_generation: self.binding.source_pin_generation,
            lease_generation: self.binding.lease_generation,
            manifest_hash: self.binding.manifest_hash,
        }
    }

    pub fn require_exact_node_replay(
        &self,
        replayed: &SnapshotExportCommittedV1,
    ) -> Result<(), ExportBindingError> {
        require(replayed.job_id == self.binding.job_id, "node replay job id")?;
        require(
            replayed.pin_generation == self.binding.exported_pin_generation,
            "node replay exported pin generation",
        )?;
        require(
            replayed.record_hash == self.binding.export_record_hash,
            "node replay record hash",
        )
    }
}

pub struct ExportedManifestBindingStore {
    root: PathBuf,
    limits: SchemaLimits,
    abstained: bool,
    _lock: ExclusiveFileLock,
}

impl ExportedManifestBindingStore {
    pub fn open(root: impl AsRef<Path>, limits: SchemaLimits) -> Result<Self, ExportBindingError> {
        let root = root.as_ref().to_path_buf();
        create_private_directory(&root, DIRECTORY_MODE)?;
        let lock = ExclusiveFileLock::acquire(&root, LOCK_FILE, FILE_MODE, "open binding lock")?;
        if path_exists(&root.join(LOCATOR_TEMP_FILE))? {
            return Err(ExportBindingError::AmbiguousTemporary(
                root.join(LOCATOR_TEMP_FILE),
            ));
        }
        inspect_known_entries(&root, KNOWN_ENTRIES)?;
        let abstained = path_exists(&root.join(CONFLICT_FILE))?;
        Ok(Self {
            root,
            limits,
            abstained,
            _lock: lock,
        })
    }

    pub fn seal(
        &mut self,
        cas: &FilesystemCas,
        reader: &FilesystemCasReader,
        candidate: ExportBindingCandidate<'_>,
    ) -> Result<(ExportBindingSealOutcome, VerifiedExportedManifestBinding), ExportBindingError>
    {
        self.require_active()?;
        let binding = derive_binding(reader, &candidate, &self.limits)?;
        let canonical = encode_binding(&binding);
        let binding_ref = cas.publish_bytes(&canonical)?;
        require(
            binding_ref.expected_ocb1_kind.is_none(),
            "local export binding CAS kind",
        )?;
        let published = reader.read_verified(&binding_ref)?;
        require(
            published.bytes() == canonical,
            "supervisor reads the exact published binding",
        )?;

        let locator_path = self.root.join(LOCATOR_FILE);
        if path_exists(&locator_path)? {
            let existing_ref = decode_locator(&read_bounded(&locator_path, LOCATOR_FIXED_BYTES)?)?;
            if existing_ref != binding_ref {
                self.latch_conflict(existing_ref.transport_digest, binding_ref.transport_digest)?;
                return Err(ExportBindingError::ConflictingBinding);
            }
            let verified = self.load_exact(
                reader,
                candidate.discovery,
                candidate.bundle,
                candidate.input_refs,
            )?;
            require(verified.binding == binding, "exact export binding replay")?;
            return Ok((ExportBindingSealOutcome::ExactReplay, verified));
        }

        persist_atomic(
            &self.root,
            &self.root.join(LOCATOR_TEMP_FILE),
            &locator_path,
            FILE_MODE,
            &encode_locator(&binding_ref)?,
        )?;
        let verified = self.load_exact(
            reader,
            candidate.discovery,
            candidate.bundle,
            candidate.input_refs,
        )?;
        Ok((ExportBindingSealOutcome::NewlySealed, verified))
    }

    pub fn seal_receipt(
        &mut self,
        cas: &FilesystemCas,
        reader: &FilesystemCasReader,
        candidate: ExportReceiptBindingCandidate<'_>,
    ) -> Result<(ExportBindingSealOutcome, VerifiedExportedManifestBinding), ExportBindingError>
    {
        let manifest_ref = candidate.receipt.manifest_ref();
        let committed = candidate.receipt.committed();
        self.seal(
            cas,
            reader,
            ExportBindingCandidate {
                discovery: candidate.discovery,
                job_id: candidate.receipt.job_id(),
                source_pin_generation: candidate.receipt.source_pin_generation(),
                lease_generation: candidate.receipt.lease_generation(),
                checkpoint: candidate.receipt.checkpoint(),
                manifest_ref: &manifest_ref,
                committed: &committed,
                bundle: candidate.bundle,
                input_refs: candidate.input_refs,
            },
        )
    }

    pub fn load_exact(
        &self,
        reader: &FilesystemCasReader,
        discovery: &DiscoveryRecord,
        bundle: &ProtocolBundleV1,
        input_refs: &VerifiedInputChunkRefCatalog,
    ) -> Result<VerifiedExportedManifestBinding, ExportBindingError> {
        self.require_active()?;
        inspect_known_entries(&self.root, KNOWN_ENTRIES)?;
        load_binding(
            &self.root,
            &self.limits,
            BindingReadInputs {
                reader,
                spec: &discovery.spec,
                bundle,
                input_refs,
            },
            discovery.cursor,
            CatalogRead::Private,
        )
    }

    fn require_active(&self) -> Result<(), ExportBindingError> {
        if self.abstained || path_exists(&self.root.join(CONFLICT_FILE))? {
            Err(ExportBindingError::Abstained)
        } else {
            Ok(())
        }
    }

    fn latch_conflict(
        &mut self,
        existing: B256,
        candidate: B256,
    ) -> Result<(), ExportBindingError> {
        latch_conflict_file(
            &self.root,
            CONFLICT_FILE,
            FILE_MODE,
            CONFLICT_MAGIC,
            (existing, candidate),
        )?;
        self.abstained = true;
        Ok(())
    }
}

/// Read-only inspection of a stopped source's native exported-manifest binding.
///
/// Opening never creates directories, changes permissions, acquires a writer
/// lock, repairs temporary state, or publishes CAS objects. A legacy lock file
/// may be absent. The caller must keep the source quiescent throughout the audit.
pub struct ExportedManifestBindingReader {
    root: PathBuf,
    limits: SchemaLimits,
}

struct BindingReadInputs<'a> {
    reader: &'a FilesystemCasReader,
    spec: &'a FinalizedJobSpecV1,
    bundle: &'a ProtocolBundleV1,
    input_refs: &'a VerifiedInputChunkRefCatalog,
}

impl ExportedManifestBindingReader {
    pub fn open_existing(
        root: impl AsRef<Path>,
        limits: SchemaLimits,
    ) -> Result<Self, ExportBindingError> {
        let root = root.as_ref().to_path_buf();
        inspect_existing_binding(&root)?;
        Ok(Self { root, limits })
    }

    /// Check the exact native binding against the caller-authenticated immutable
    /// finalized job specification. The caller obtains that specification from
    /// current canonical job/finality state and its retained request checkpoint.
    /// This check does not require a surviving discovery spool record.
    ///
    /// This retains the native owner's comparisons. Full checkpoint height and
    /// schema checks against canonical state remain the caller's responsibility.
    pub fn load_exact(
        &self,
        reader: &FilesystemCasReader,
        spec: &FinalizedJobSpecV1,
        bundle: &ProtocolBundleV1,
        input_refs: &VerifiedInputChunkRefCatalog,
    ) -> Result<VerifiedExportedManifestBinding, ExportBindingError> {
        self.load_with_catalog_read(
            BindingReadInputs {
                reader,
                spec,
                bundle,
                input_refs,
            },
            CatalogRead::Private,
        )
    }

    /// Check the stopped snapshot's immutable binding and ciphertext catalog.
    /// Private Tribute amounts remain the responsibility of live execution.
    pub fn load_integrity(
        &self,
        reader: &FilesystemCasReader,
        spec: &FinalizedJobSpecV1,
        bundle: &ProtocolBundleV1,
        input_refs: &VerifiedInputChunkRefCatalog,
    ) -> Result<VerifiedExportedManifestBinding, ExportBindingError> {
        self.load_with_catalog_read(
            BindingReadInputs {
                reader,
                spec,
                bundle,
                input_refs,
            },
            CatalogRead::Integrity,
        )
    }

    fn load_with_catalog_read(
        &self,
        inputs: BindingReadInputs<'_>,
        catalog_read: CatalogRead,
    ) -> Result<VerifiedExportedManifestBinding, ExportBindingError> {
        inspect_existing_binding(&self.root)?;
        let cursor = inputs.spec.summary.cursor;
        load_binding(&self.root, &self.limits, inputs, cursor, catalog_read)
    }
}

#[derive(Clone, Copy)]
enum CatalogRead {
    Private,
    Integrity,
}

fn inspect_existing_binding(root: &Path) -> Result<(), ExportBindingError> {
    inspect_private_directory(root)?;
    if path_exists(&root.join(LOCATOR_TEMP_FILE))? {
        return Err(ExportBindingError::AmbiguousTemporary(
            root.join(LOCATOR_TEMP_FILE),
        ));
    }
    inspect_known_entries(root, KNOWN_ENTRIES)?;
    if path_exists(&root.join(CONFLICT_FILE))? {
        return Err(ExportBindingError::Abstained);
    }
    Ok(())
}

// Keep the explicit cursor so runtime DiscoveryRecord validation still rejects
// disagreement between its outer cursor and the immutable spec's cursor.
fn load_binding(
    root: &Path,
    limits: &SchemaLimits,
    inputs: BindingReadInputs<'_>,
    cursor: u64,
    catalog_read: CatalogRead,
) -> Result<VerifiedExportedManifestBinding, ExportBindingError> {
    let BindingReadInputs {
        reader,
        spec,
        bundle,
        input_refs,
    } = inputs;
    let locator_path = root.join(LOCATOR_FILE);
    if !path_exists(&locator_path)? {
        return Err(ExportBindingError::MissingBinding);
    }
    let binding_ref = decode_locator(&read_bounded(&locator_path, LOCATOR_FIXED_BYTES)?)?;
    let binding_object = reader.read_verified(&binding_ref)?;
    let binding = decode_binding(binding_object.bytes())?;
    require(
        encode_binding(&binding) == binding_object.bytes(),
        "canonical export binding encoding",
    )?;
    let manifest_object = reader.read_verified(&binding.manifest_ref)?;
    require(
        binding.manifest_ref.expected_ocb1_kind == Some(ObjectKind::InputManifestV1.tag()),
        "typed input manifest reference",
    )?;
    let manifest = InputManifestV1::decode_canonical(manifest_object.bytes(), limits)?;
    manifest.validate_against_bundle(bundle, limits)?;
    validate_binding_authority(&binding, spec, cursor, &manifest, limits)?;
    validate_closed_input_catalog(
        input_refs,
        reader,
        bundle,
        InputManifestAuthority {
            reference: &binding.manifest_ref,
            manifest: &manifest,
        },
        catalog_read,
    )?;
    Ok(VerifiedExportedManifestBinding {
        binding_ref,
        binding,
        manifest,
    })
}

fn derive_binding(
    reader: &FilesystemCasReader,
    candidate: &ExportBindingCandidate<'_>,
    limits: &SchemaLimits,
) -> Result<ExportedManifestBindingV1, ExportBindingError> {
    let manifest_object = reader.read_verified(candidate.manifest_ref)?;
    require(
        candidate.manifest_ref.expected_ocb1_kind == Some(ObjectKind::InputManifestV1.tag()),
        "typed input manifest reference",
    )?;
    let manifest = InputManifestV1::decode_canonical(manifest_object.bytes(), limits)?;
    manifest.validate_against_bundle(candidate.bundle, limits)?;
    validate_closed_input_catalog(
        candidate.input_refs,
        reader,
        candidate.bundle,
        InputManifestAuthority {
            reference: candidate.manifest_ref,
            manifest: &manifest,
        },
        CatalogRead::Private,
    )?;

    let spec_bytes = candidate.discovery.spec.encode_body(limits)?;
    let intent =
        JobIntentV1::decode_canonical(&candidate.discovery.spec.canonical_job_intent.0, limits)?;
    validate_manifest_job_authority(candidate, &intent, &manifest)?;
    let manifest_hash = manifest.manifest_hash(limits)?;
    require(
        candidate.committed.job_id == manifest.job_id,
        "export commit job id",
    )?;
    require(
        candidate.committed.pin_generation
            == candidate
                .source_pin_generation
                .checked_add(1)
                .ok_or(ExportBindingError::IntegerOverflow)?,
        "exported pin generation",
    )?;
    require(
        !candidate.committed.record_hash.is_zero(),
        "export commit record hash",
    )?;

    Ok(ExportedManifestBindingV1 {
        discovery_generation: candidate.discovery.generation,
        finalized_cursor: candidate.discovery.cursor,
        finalized_job_spec_hash: keccak256(spec_bytes),
        job_id: manifest.job_id,
        attempt: manifest.attempt,
        protocol_bundle_hash: manifest.protocol_bundle_hash,
        source_pin_generation: candidate.source_pin_generation,
        lease_generation: candidate.lease_generation,
        checkpoint: manifest.checkpoint.clone(),
        manifest_ref: candidate.manifest_ref.clone(),
        manifest_hash,
        exported_pin_generation: candidate.committed.pin_generation,
        export_record_hash: candidate.committed.record_hash,
    })
}

fn validate_binding_authority(
    binding: &ExportedManifestBindingV1,
    spec: &FinalizedJobSpecV1,
    cursor: u64,
    manifest: &InputManifestV1,
    limits: &SchemaLimits,
) -> Result<(), ExportBindingError> {
    let intent = JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, limits)?;
    // `discovery_generation` belongs to the legacy single-current local
    // journal. It is retained for decoding old local artifacts, but it is not
    // authority in the embedded multi-job path. The exact finalized cursor and
    // full spec hash below bind this receipt more strongly and independently of
    // local discovery order.
    require(binding.finalized_cursor == cursor, "finalized cursor")?;
    require(
        binding.finalized_job_spec_hash == keccak256(spec.encode_body(limits)?),
        "finalized job spec hash",
    )?;
    require(binding.job_id == manifest.job_id, "binding manifest job id")?;
    require(binding.attempt == manifest.attempt, "binding attempt")?;
    require(
        binding.protocol_bundle_hash == manifest.protocol_bundle_hash,
        "binding protocol bundle",
    )?;
    require(
        binding.checkpoint == manifest.checkpoint,
        "binding checkpoint",
    )?;
    require(
        binding.manifest_hash == manifest.manifest_hash(limits)?,
        "binding manifest hash",
    )?;
    require(
        binding.manifest_ref.expected_ocb1_kind == Some(ObjectKind::InputManifestV1.tag()),
        "binding manifest kind",
    )?;
    let expected_exported_pin = binding
        .source_pin_generation
        .checked_add(1)
        .ok_or(ExportBindingError::IntegerOverflow)?;
    require(
        ![binding.source_pin_generation, binding.lease_generation].contains(&0)
            && (binding.exported_pin_generation == expected_exported_pin)
            && !binding.export_record_hash.is_zero(),
        "binding export generations and record",
    )?;
    validate_manifest_job_fields(spec, cursor, &intent, manifest)
}

fn validate_manifest_job_authority(
    candidate: &ExportBindingCandidate<'_>,
    intent: &JobIntentV1,
    manifest: &InputManifestV1,
) -> Result<(), ExportBindingError> {
    require(
        candidate.job_id == candidate.discovery.spec.summary.job_id,
        "snapshot handoff job id",
    )?;
    require(
        candidate.source_pin_generation != 0 && candidate.lease_generation != 0,
        "snapshot handoff generations",
    )?;
    require(
        candidate.checkpoint == &manifest.checkpoint,
        "snapshot handoff checkpoint",
    )?;
    validate_manifest_job_fields(
        &candidate.discovery.spec,
        candidate.discovery.cursor,
        intent,
        manifest,
    )
}

fn validate_manifest_job_fields(
    spec: &FinalizedJobSpecV1,
    cursor: u64,
    intent: &JobIntentV1,
    manifest: &InputManifestV1,
) -> Result<(), ExportBindingError> {
    let summary = &spec.summary;
    require(cursor == summary.cursor, "discovery cursor")?;
    require(manifest.job_id == summary.job_id, "manifest job id")?;
    require(
        manifest.protocol_bundle_hash == summary.protocol_bundle_hash
            && manifest.protocol_bundle_hash == intent.protocol_bundle_hash,
        "manifest protocol bundle",
    )?;
    require(manifest.attempt == intent.attempt, "manifest attempt")?;
    require(manifest.wwd == intent.wwd, "manifest WWD")?;
    require(
        manifest.sealed_tribute_collection_key == intent.sealed_tribute_collection_key,
        "manifest Tribute collection key",
    )?;
    require(
        manifest.sealed_tribute_collection_root == intent.sealed_tribute_collection_root,
        "manifest Tribute collection root",
    )?;
    require(
        manifest.tribute_count == intent.authenticated_day_count,
        "manifest Tribute count",
    )?;
    require(
        manifest.tribute_nominal_total == intent.authenticated_day_nominal,
        "manifest Tribute nominal total",
    )?;
    require(
        manifest.checkpoint.finalized_block_hash == summary.finalized_block_hash,
        "manifest finalized block hash",
    )?;
    require(
        manifest.checkpoint.finalized_state_root == summary.finalized_state_root,
        "manifest finalized state root",
    )?;
    require(
        manifest.checkpoint.finalized_ce_root == intent.ce_sealed_root,
        "manifest finalized CE root",
    )
}

struct InputManifestAuthority<'a> {
    reference: &'a CasObjectRefV1,
    manifest: &'a InputManifestV1,
}

fn validate_closed_input_catalog(
    input_refs: &VerifiedInputChunkRefCatalog,
    reader: &FilesystemCasReader,
    bundle: &ProtocolBundleV1,
    authority: InputManifestAuthority<'_>,
    catalog_read: CatalogRead,
) -> Result<(), ExportBindingError> {
    input_refs.require_manifest_authority(authority.reference, authority.manifest)?;
    let cursor = match catalog_read {
        CatalogRead::Private => input_refs.exact_verified_cursor(reader, bundle)?,
        CatalogRead::Integrity => input_refs.exact_verified_integrity_cursor(reader, bundle)?,
    };
    for reference in cursor {
        let _ = reference?;
    }
    Ok(())
}

fn encode_binding(binding: &ExportedManifestBindingV1) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(BINDING_FIXED_BYTES);
    encoded.extend_from_slice(&BINDING_MAGIC);
    encoded.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    encoded.extend_from_slice(&binding.discovery_generation.to_be_bytes());
    encoded.extend_from_slice(&binding.finalized_cursor.to_be_bytes());
    encoded.extend_from_slice(binding.finalized_job_spec_hash.as_slice());
    encoded.extend_from_slice(binding.job_id.as_slice());
    encoded.extend_from_slice(&binding.attempt.to_be_bytes());
    encoded.extend_from_slice(binding.protocol_bundle_hash.as_slice());
    encoded.extend_from_slice(&binding.source_pin_generation.to_be_bytes());
    encoded.extend_from_slice(&binding.lease_generation.to_be_bytes());
    encode_checkpoint(&mut encoded, &binding.checkpoint);
    encode_manifest_ref(&mut encoded, &binding.manifest_ref);
    encoded.extend_from_slice(binding.manifest_hash.as_slice());
    encoded.extend_from_slice(&binding.exported_pin_generation.to_be_bytes());
    encoded.extend_from_slice(binding.export_record_hash.as_slice());
    encoded.extend_from_slice(keccak256(&encoded).as_slice());
    encoded
}

fn decode_binding(encoded: &[u8]) -> Result<ExportedManifestBindingV1, ExportBindingError> {
    verify_envelope_checksum(
        encoded,
        BINDING_FIXED_BYTES,
        BINDING_MAGIC,
        BINDING_CHECKSUM_OFFSET,
    )?;
    let checkpoint = decode_checkpoint_at(encoded, BINDING_CHECKPOINT_OFFSET)?;
    let (manifest_ref, expected_kind) =
        decode_manifest_ref_at(encoded, BINDING_MANIFEST_REF_OFFSET)?;
    if expected_kind == 0 {
        return Err(ExportBindingError::InvalidEnvelope);
    }
    let binding = ExportedManifestBindingV1 {
        discovery_generation: read_u64(encoded, 10)?,
        finalized_cursor: read_u64(encoded, 18)?,
        finalized_job_spec_hash: read_b256(encoded, 26)?,
        job_id: read_b256(encoded, 58)?,
        attempt: read_u32(encoded, 90)?,
        protocol_bundle_hash: read_b256(encoded, 94)?,
        source_pin_generation: read_u64(encoded, 126)?,
        lease_generation: read_u64(encoded, 134)?,
        checkpoint,
        manifest_ref,
        manifest_hash: read_b256(encoded, BINDING_MANIFEST_HASH_OFFSET)?,
        exported_pin_generation: read_u64(encoded, BINDING_EXPORTED_GENERATION_OFFSET)?,
        export_record_hash: read_b256(encoded, BINDING_EXPORT_RECORD_HASH_OFFSET)?,
    };
    let non_zero_u64s = [
        binding.discovery_generation,
        binding.finalized_cursor,
        binding.source_pin_generation,
        binding.lease_generation,
        binding.manifest_ref.encoded_bytes,
        binding.exported_pin_generation,
    ];
    let non_zero_hashes = [
        binding.finalized_job_spec_hash,
        binding.job_id,
        binding.protocol_bundle_hash,
        binding.manifest_ref.transport_digest,
        binding.manifest_hash,
        binding.export_record_hash,
    ];
    require(
        !non_zero_u64s.contains(&0) && !non_zero_hashes.contains(&B256::ZERO),
        "export binding non-zero fields",
    )?;
    Ok(binding)
}

fn encode_locator(reference: &CasObjectRefV1) -> Result<Vec<u8>, ExportBindingError> {
    Ok(encode_locator_with_magic(
        reference,
        LOCATOR_MAGIC,
        "export binding locator reference",
    )?)
}

fn decode_locator(encoded: &[u8]) -> Result<CasObjectRefV1, ExportBindingError> {
    Ok(decode_locator_with_magic(
        encoded,
        LOCATOR_MAGIC,
        "export binding locator",
    )?)
}

fn require(condition: bool, what: &'static str) -> Result<(), ExportBindingError> {
    if condition {
        Ok(())
    } else {
        Err(ExportBindingError::AuthorityMismatch(what))
    }
}

#[derive(Debug, Error)]
pub enum ExportBindingError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error(transparent)]
    InputCatalog(#[from] InputRefCatalogError),
    #[error("export binding authority mismatch: {0}")]
    AuthorityMismatch(&'static str),
    #[error("export binding integer overflow")]
    IntegerOverflow,
    #[error("export binding is missing")]
    MissingBinding,
    #[error("a different export binding was observed; this domain must abstain")]
    ConflictingBinding,
    #[error("export binding store is permanently abstained")]
    Abstained,
    #[error("export binding envelope is malformed")]
    InvalidEnvelope,
    #[error("export binding store contains an ambiguous temporary file at {0}")]
    AmbiguousTemporary(PathBuf),
    #[error("export binding store contains an unexpected entry at {0}")]
    UnexpectedEntry(PathBuf),
    #[error("export binding path is unsafe: {0}")]
    UnsafePath(PathBuf),
    #[error("export binding store lock is already held at {0}")]
    LockHeld(PathBuf),
    #[error("export binding I/O failed while trying to {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> ExportedManifestBindingV1 {
        ExportedManifestBindingV1 {
            discovery_generation: 7,
            finalized_cursor: 11,
            finalized_job_spec_hash: B256::repeat_byte(0x11),
            job_id: B256::repeat_byte(0x12),
            attempt: 0,
            protocol_bundle_hash: B256::repeat_byte(0x13),
            source_pin_generation: 5,
            lease_generation: 9,
            checkpoint: CheckpointIdentityV1 {
                finalized_block_number: 101,
                finalized_block_hash: B256::repeat_byte(0x14),
                finalized_state_root: B256::repeat_byte(0x15),
                finalized_ce_root: B256::repeat_byte(0x16),
                ce_schema_version: 2,
            },
            manifest_ref: CasObjectRefV1 {
                transport_digest: B256::repeat_byte(0x17),
                encoded_bytes: 1234,
                expected_ocb1_kind: Some(ObjectKind::InputManifestV1.tag()),
            },
            manifest_hash: B256::repeat_byte(0x18),
            exported_pin_generation: 6,
            export_record_hash: B256::repeat_byte(0x19),
        }
    }

    #[test]
    fn fixed_encoding_round_trips_authority() {
        let expected = binding();
        let encoded = encode_binding(&expected);
        assert_eq!(encoded.len(), BINDING_FIXED_BYTES);
        assert_eq!(decode_binding(&encoded).unwrap(), expected);

        let mut changed = encoded;
        changed[BINDING_MANIFEST_HASH_OFFSET] ^= 1;
        assert!(matches!(
            decode_binding(&changed),
            Err(ExportBindingError::InvalidEnvelope)
        ));
    }
}
