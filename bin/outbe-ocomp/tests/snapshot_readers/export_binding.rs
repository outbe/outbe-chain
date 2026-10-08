mod support {
    include!("../support/mod.rs");
}
use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
use outbe_ocomp::{
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::poc_schema_limits,
    export_binding::{
        ExportBindingCandidate, ExportedManifestBindingReader, ExportedManifestBindingStore,
    },
    input_artifacts::derive_input_chunk_ref,
    input_ref_catalog::VerifiedInputChunkRefCatalog,
    supervisor::DiscoveryRecord,
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    control::FinalizedJobSpecV1,
    input::{
        AuthenticatedInputChunkV1, CheckpointIdentityV1, Compression, InputChunkKind,
        InputManifestV1,
    },
    intent::JobIntentV1,
    profile::ProtocolBundleV1,
    CasObjectRefV1, ListKind, ObjectKind, OrderedListLimits, SchemaLimits,
    SnapshotExportCommittedV1,
};
use outbe_primitives::time::WorldwideDay;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

struct Fixture {
    directory: tempfile::TempDir,
    binding_root: PathBuf,
    cas_root: PathBuf,
    reader: FilesystemCasReader,
    catalog: VerifiedInputChunkRefCatalog,
    limits: SchemaLimits,
    bundle: ProtocolBundleV1,
    spec: FinalizedJobSpecV1,
    binding_ref: CasObjectRefV1,
    manifest_ref: CasObjectRefV1,
    chunk_ref: CasObjectRefV1,
    committed: SnapshotExportCommittedV1,
}

fn fixture(seed: u8) -> Fixture {
    let limits = poc_schema_limits();
    let list_limits = OrderedListLimits::new(16, 4096, 4096);
    let bundle = support::protocol_bundle();
    let spec = support::finalized_job_spec(seed, 90, 1, B256::repeat_byte(250));
    let intent = JobIntentV1::decode_canonical(&spec.canonical_job_intent.0, &limits).unwrap();
    let directory = support::tempdir().unwrap();
    let cas_root = directory.path().join("cas");
    let binding_root = directory.path().join("binding");
    let cas_limits = CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: 8_388_608,
    };
    let cas = FilesystemCas::open(&cas_root, CasWriterRole::SnapshotExporter, cas_limits).unwrap();
    let reader = FilesystemCasReader::open(&cas_root, cas_limits).unwrap();
    let day = WorldwideDay::new(intent.wwd);
    let (chunk_ref, input_ref) = publish_tribute_chunk(&cas, &reader, &bundle, &spec, day);
    let manifest = InputManifestV1 {
        protocol_bundle_hash: spec.summary.protocol_bundle_hash,
        job_id: spec.summary.job_id,
        attempt: intent.attempt,
        checkpoint: CheckpointIdentityV1 {
            finalized_block_number: spec.summary.cursor,
            finalized_block_hash: spec.summary.finalized_block_hash,
            finalized_state_root: spec.summary.finalized_state_root,
            finalized_ce_root: intent.ce_sealed_root,
            ce_schema_version: 1,
        },
        wwd: intent.wwd,
        sealed_tribute_collection_key: intent.sealed_tribute_collection_key,
        sealed_tribute_collection_root: intent.sealed_tribute_collection_root,
        tribute_count: intent.authenticated_day_count,
        tribute_nominal_total: intent.authenticated_day_nominal,
        input_chunk_count: 1,
        input_chunk_list_root: outbe_ocomp_protocol::ordered_list_root(
            ListKind::InputChunkReferences,
            &[input_ref.encode_canonical_record(&limits).unwrap()],
            list_limits,
        )
        .unwrap(),
        fidelity_opening_root: B256::repeat_byte(201),
        oracle_opening_root: B256::repeat_byte(202),
        exact_encoded_bytes: input_ref.encoded_bytes,
        exact_record_count: input_ref.record_count,
        body_codec_id: bundle.tribute_body_codec_id,
        opening_codec_registry_hash: bundle.opening_codec_registry_hash().unwrap(),
        compression: Compression::None,
    };
    let mut manifest_ref = cas
        .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
        .unwrap();
    manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
    let mut catalog = VerifiedInputChunkRefCatalog::open(
        directory.path().join("input-refs"),
        &cas,
        &manifest_ref,
        limits,
        list_limits,
    )
    .unwrap();
    catalog.admit(&input_ref).unwrap();
    let committed = SnapshotExportCommittedV1 {
        job_id: spec.summary.job_id,
        pin_generation: 12,
        record_hash: B256::repeat_byte(203),
    };
    // Only the native producer uses the legacy discovery record. The offline
    // consumer below retains the authenticated immutable spec, not this record.
    let discovery = DiscoveryRecord {
        generation: 7,
        cursor: spec.summary.cursor,
        spec: spec.clone(),
    };
    let binding_ref = seal_fixture_binding(
        &binding_root,
        limits,
        FixtureBindingAuthority {
            cas: &cas,
            reader: &reader,
            discovery: &discovery,
            manifest_ref: &manifest_ref,
            manifest: &manifest,
            committed: &committed,
            bundle: &bundle,
            catalog: &catalog,
        },
    );
    Fixture {
        directory,
        binding_root,
        cas_root,
        reader,
        catalog,
        limits,
        bundle,
        spec,
        binding_ref,
        manifest_ref,
        chunk_ref,
        committed,
    }
}

struct FixtureBindingAuthority<'a> {
    cas: &'a FilesystemCas,
    reader: &'a FilesystemCasReader,
    discovery: &'a DiscoveryRecord,
    manifest_ref: &'a CasObjectRefV1,
    manifest: &'a InputManifestV1,
    committed: &'a SnapshotExportCommittedV1,
    bundle: &'a ProtocolBundleV1,
    catalog: &'a VerifiedInputChunkRefCatalog,
}

fn seal_fixture_binding(
    root: &Path,
    limits: SchemaLimits,
    authority: FixtureBindingAuthority<'_>,
) -> CasObjectRefV1 {
    let mut store = ExportedManifestBindingStore::open(root, limits).unwrap();
    store
        .seal(
            authority.cas,
            authority.reader,
            ExportBindingCandidate {
                discovery: authority.discovery,
                job_id: authority.discovery.spec.summary.job_id,
                source_pin_generation: 11,
                lease_generation: 17,
                checkpoint: &authority.manifest.checkpoint,
                manifest_ref: authority.manifest_ref,
                committed: authority.committed,
                bundle: authority.bundle,
                input_refs: authority.catalog,
            },
        )
        .unwrap()
        .1
        .binding_ref()
}

fn publish_tribute_chunk(
    cas: &FilesystemCas,
    reader: &FilesystemCasReader,
    bundle: &ProtocolBundleV1,
    spec: &FinalizedJobSpecV1,
    day: WorldwideDay,
) -> (CasObjectRefV1, outbe_ocomp_protocol::input::InputChunkRefV1) {
    let limits = poc_schema_limits();
    let owner = Address::repeat_byte(1);
    let tribute = TributeBodyV1 {
        tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
        owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(1),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(1),
        reference_currency: 978,
        tribute_price_minor: U256::from(1),
        exclude_from_intex_issuance: false,
    };
    let chunk = AuthenticatedInputChunkV1 {
        protocol_bundle_hash: spec.summary.protocol_bundle_hash,
        job_id: spec.summary.job_id,
        kind: InputChunkKind::Tribute,
        ordinal: 0,
        canonical_records_or_openings: vec![BoundedBytes(encode_tribute_v1(&tribute).unwrap())],
    };
    let mut chunk_ref = cas
        .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
        .unwrap();
    chunk_ref.expected_ocb1_kind = Some(ObjectKind::AuthenticatedInputChunkV1.tag());
    let input_ref =
        derive_input_chunk_ref(&reader.read_verified(&chunk_ref).unwrap(), bundle, &limits)
            .unwrap()
            .reference;
    (chunk_ref, input_ref)
}

type Fingerprint = BTreeMap<PathBuf, (u32, Option<PathBuf>, Vec<u8>)>;
fn snapshot(root: &Path) -> Fingerprint {
    fn visit(root: &Path, path: &Path, output: &mut Fingerprint) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let link = metadata
            .file_type()
            .is_symlink()
            .then(|| fs::read_link(path).unwrap());
        let bytes = if metadata.is_file() {
            fs::read(path).unwrap()
        } else {
            Vec::new()
        };
        output.insert(
            path.strip_prefix(root).unwrap().to_path_buf(),
            (metadata.mode(), link, bytes),
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), output);
            }
        }
    }
    let mut output = BTreeMap::new();
    visit(root, root, &mut output);
    output
}
fn cas_path(f: &Fixture, reference: &CasObjectRefV1) -> PathBuf {
    let digest = hex::encode(reference.transport_digest.as_slice());
    f.cas_root
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..])
}
fn load(
    f: &Fixture,
    spec: &FinalizedJobSpecV1,
) -> Result<
    outbe_ocomp::export_binding::VerifiedExportedManifestBinding,
    outbe_ocomp::export_binding::ExportBindingError,
> {
    ExportedManifestBindingReader::open_existing(&f.binding_root, f.limits)?
        .load_exact(&f.reader, spec, &f.bundle, &f.catalog)
}

#[test]
fn authentic_spec_reads_native_binding_without_discovery_spool_or_binding_lock() {
    let f = fixture(20);
    fs::remove_file(f.binding_root.join("binding.lock")).unwrap();
    fs::set_permissions(&f.binding_root, fs::Permissions::from_mode(0o750)).unwrap();
    fs::set_permissions(
        f.binding_root.join("binding.ref"),
        fs::Permissions::from_mode(0o640),
    )
    .unwrap();
    assert!(!f.directory.path().join("discovery-spool-v1").exists());
    let before = snapshot(f.directory.path());
    for _ in 0..2 {
        let verified = load(&f, &f.spec).unwrap();
        assert_eq!(verified.binding_ref(), f.binding_ref);
        assert_eq!(verified.manifest_ref(), f.manifest_ref);
        assert_eq!(verified.job_id(), f.spec.summary.job_id);
        assert_eq!(verified.commit_replay_request().pin_generation, 11);
        assert_eq!(verified.commit_replay_request().lease_generation, 17);
        verified.require_exact_node_replay(&f.committed).unwrap();
    }
    assert_eq!(snapshot(f.directory.path()), before);
}

#[test]
fn substitutions_of_spec_catalog_and_commit_are_rejected_without_source_changes() {
    let f = fixture(20);
    let other = fixture(40);
    let before = snapshot(f.directory.path());
    for field in 0..9 {
        let mut spec = f.spec.clone();
        match field {
            0 => spec.summary.cursor += 1,
            1 => spec.summary.job_id = other.spec.summary.job_id,
            2 => spec.summary.finalized_block_hash = other.spec.summary.finalized_block_hash,
            3 => spec.summary.finalized_state_root = other.spec.summary.finalized_state_root,
            4 => spec.summary.protocol_bundle_hash = B256::repeat_byte(249),
            5 => spec.summary.intent_id = other.spec.summary.intent_id,
            6 => spec.summary.open_height += 1,
            7 => spec.summary.deadline_height += 1,
            _ => spec.canonical_job_intent = other.spec.canonical_job_intent.clone(),
        }
        assert!(load(&f, &spec).is_err(), "spec field {field}");
    }
    let reader = ExportedManifestBindingReader::open_existing(&f.binding_root, f.limits).unwrap();
    assert!(reader
        .load_exact(&f.reader, &f.spec, &f.bundle, &other.catalog)
        .is_err());
    let binding = load(&f, &f.spec).unwrap();
    let wrong = SnapshotExportCommittedV1 {
        pin_generation: 13,
        ..f.committed.clone()
    };
    assert!(binding.require_exact_node_replay(&wrong).is_err());
    assert_eq!(snapshot(f.directory.path()), before);
}

#[test]
fn missing_corrupt_and_symlinked_binding_paths_are_never_repaired() {
    let f = fixture(20);
    let missing = f.directory.path().join("missing");
    assert!(ExportedManifestBindingReader::open_existing(&missing, f.limits).is_err());
    assert!(!missing.exists());
    let alias = f.directory.path().join("alias");
    symlink(&f.binding_root, &alias).unwrap();
    let before = snapshot(f.directory.path());
    assert!(ExportedManifestBindingReader::open_existing(&alias, f.limits).is_err());
    assert_eq!(snapshot(f.directory.path()), before);
    for name in ["binding.ref.tmp", "binding.abstained", "unexpected"] {
        let path = f.binding_root.join(name);
        fs::write(&path, b"preserved evidence").unwrap();
        let before = snapshot(f.directory.path());
        assert!(load(&f, &f.spec).is_err(), "{name}");
        assert_eq!(snapshot(f.directory.path()), before);
        fs::remove_file(path).unwrap();
    }
    let locator = f.binding_root.join("binding.ref");
    let bytes = fs::read(&locator).unwrap();
    fs::remove_file(&locator).unwrap();
    let before = snapshot(f.directory.path());
    assert!(load(&f, &f.spec).is_err());
    assert_eq!(snapshot(f.directory.path()), before);
    for corrupted in [
        vec![],
        bytes[..bytes.len() - 1].to_vec(),
        [bytes.as_slice(), b"x"].concat(),
        vec![0; bytes.len()],
    ] {
        fs::write(&locator, corrupted).unwrap();
        let before = snapshot(f.directory.path());
        assert!(load(&f, &f.spec).is_err());
        assert_eq!(snapshot(f.directory.path()), before);
    }
    fs::remove_file(&locator).unwrap();
    let target = f.directory.path().join("external-locator");
    fs::write(&target, bytes).unwrap();
    symlink(&target, &locator).unwrap();
    let before = snapshot(f.directory.path());
    assert!(load(&f, &f.spec).is_err());
    assert_eq!(snapshot(f.directory.path()), before);
}

#[test]
fn cas_binding_manifest_and_chunk_corruption_fail_without_mutation() {
    let f = fixture(20);
    for reference in [&f.binding_ref, &f.manifest_ref, &f.chunk_ref] {
        let path = cas_path(&f, reference);
        let original = fs::read(&path).unwrap();
        let mut corrupt = original.clone();
        corrupt[0] ^= 0xff;
        fs::write(&path, corrupt).unwrap();
        let before = snapshot(f.directory.path());
        assert!(load(&f, &f.spec).is_err());
        assert_eq!(snapshot(f.directory.path()), before);
        fs::remove_file(&path).unwrap();
        let before = snapshot(f.directory.path());
        assert!(load(&f, &f.spec).is_err());
        assert_eq!(snapshot(f.directory.path()), before);
        fs::write(&path, original).unwrap();
    }
    load(&f, &f.spec).unwrap();
}

#[test]
fn runtime_cursor_rejection_survives_shared_validation_and_reader_does_not_lock() {
    let f = fixture(20);
    let store = ExportedManifestBindingStore::open(&f.binding_root, f.limits).unwrap();
    let mut discovery = DiscoveryRecord {
        generation: 99,
        cursor: f.spec.summary.cursor,
        spec: f.spec.clone(),
    };
    // Legacy journal generation is intentionally not immutable authority.
    store
        .load_exact(&f.reader, &discovery, &f.bundle, &f.catalog)
        .unwrap();
    discovery.cursor += 1;
    assert!(store
        .load_exact(&f.reader, &discovery, &f.bundle, &f.catalog)
        .is_err());
    let before = snapshot(f.directory.path());
    load(&f, &f.spec).unwrap();
    assert_eq!(snapshot(f.directory.path()), before);
}
