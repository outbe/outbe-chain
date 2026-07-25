mod support;

use std::path::PathBuf;

use alloy_primitives::{Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
use outbe_ocomp::{
    cas::{CasLimits, CasWriterRole, FilesystemCas, FilesystemCasReader},
    control::poc_schema_limits,
    input_artifacts::derive_input_chunk_ref,
    input_ref_catalog::{
        InputRefAdmissionOutcome, InputRefCatalogError, VerifiedInputChunkRefCatalog,
    },
};
use outbe_ocomp_protocol::{
    common::BoundedBytes,
    input::{
        AuthenticatedInputChunkV1, CheckpointIdentityV1, Compression, InputChunkKind,
        InputChunkRefV1, InputManifestV1,
    },
    registry::ObjectKind,
    CasObjectRefV1, ListKind, OrderedListLimits,
};

fn hash(byte: u8) -> B256 {
    B256::repeat_byte(byte)
}

fn input_ref(kind: InputChunkKind, ordinal: u32, records: u32, byte: u8) -> InputChunkRefV1 {
    InputChunkRefV1 {
        kind,
        ordinal,
        record_count: records,
        first_key: BoundedBytes(vec![byte]),
        last_key_inclusive: BoundedBytes(vec![byte.saturating_add(1)]),
        encoded_bytes: u64::from(records) * 100,
        semantic_digest: hash(byte),
        transport_digest: hash(byte.saturating_add(1)),
    }
}

struct Fixture {
    cas: FilesystemCas,
    catalog_root: PathBuf,
    limits: outbe_ocomp_protocol::SchemaLimits,
    list_limits: OrderedListLimits,
    references: [InputChunkRefV1; 2],
    manifest_ref: CasObjectRefV1,
    _directory: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let limits = poc_schema_limits();
    let list_limits = OrderedListLimits::new(16, 1024, 4096);
    let references = [
        input_ref(InputChunkKind::Tribute, 0, 2, 10),
        input_ref(InputChunkKind::Fidelity, 1, 1, 20),
    ];
    let encoded = references
        .iter()
        .map(|reference| reference.encode_canonical_record(&limits).unwrap())
        .collect::<Vec<_>>();
    let manifest = InputManifestV1 {
        protocol_bundle_hash: hash(1),
        job_id: hash(2),
        attempt: 3,
        checkpoint: CheckpointIdentityV1 {
            finalized_block_number: 4,
            finalized_block_hash: hash(5),
            finalized_state_root: hash(6),
            finalized_ce_root: hash(7),
            ce_schema_version: 1,
        },
        wwd: 20_260_725,
        sealed_tribute_collection_key: hash(8),
        sealed_tribute_collection_root: hash(9),
        tribute_count: 2,
        tribute_nominal_total: U256::from(100),
        input_chunk_count: 2,
        input_chunk_list_root: outbe_ocomp_protocol::ordered_list_root(
            ListKind::InputChunkReferences,
            &encoded,
            list_limits,
        )
        .unwrap(),
        fidelity_opening_root: hash(11),
        oracle_opening_root: hash(12),
        exact_encoded_bytes: references
            .iter()
            .map(|reference| reference.encoded_bytes)
            .sum(),
        exact_record_count: references
            .iter()
            .map(|reference| reference.record_count)
            .sum(),
        body_codec_id: hash(13),
        opening_codec_registry_hash: hash(14),
        compression: Compression::None,
    };
    let directory = tempfile::tempdir().unwrap();
    let cas = FilesystemCas::open(
        directory.path().join("cas"),
        CasWriterRole::SnapshotExporter,
        CasLimits {
            max_object_bytes: 1_048_576,
            max_total_bytes: 8_388_608,
        },
    )
    .unwrap();
    let mut manifest_ref = cas
        .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
        .unwrap();
    manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
    Fixture {
        cas,
        catalog_root: directory.path().join("input-refs"),
        limits,
        list_limits,
        references,
        manifest_ref,
        _directory: directory,
    }
}

#[test]
fn exact_input_refs_survive_cold_restart_under_the_manifest_authority() {
    let fixture = fixture();

    {
        let mut catalog = VerifiedInputChunkRefCatalog::open(
            &fixture.catalog_root,
            &fixture.cas,
            &fixture.manifest_ref,
            fixture.limits,
            fixture.list_limits,
        )
        .unwrap();
        for reference in &fixture.references {
            assert_eq!(
                catalog.admit(reference).unwrap(),
                InputRefAdmissionOutcome::NewlyAdmitted
            );
        }
    }

    let catalog = VerifiedInputChunkRefCatalog::open(
        &fixture.catalog_root,
        &fixture.cas,
        &fixture.manifest_ref,
        fixture.limits,
        fixture.list_limits,
    )
    .unwrap();
    assert_eq!(
        catalog
            .exact_cursor()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap(),
        fixture.references
    );
}

#[test]
fn partial_input_ref_catalog_never_opens_an_exact_cursor() {
    let fixture = fixture();
    let mut catalog = VerifiedInputChunkRefCatalog::open(
        &fixture.catalog_root,
        &fixture.cas,
        &fixture.manifest_ref,
        fixture.limits,
        fixture.list_limits,
    )
    .unwrap();
    catalog.admit(&fixture.references[0]).unwrap();

    assert!(matches!(
        catalog.exact_cursor(),
        Err(InputRefCatalogError::MissingReference { ordinal: 1 })
    ));
}

#[test]
fn input_ref_state_without_its_header_is_never_rebound_to_a_manifest() {
    let fixture = fixture();
    {
        let mut catalog = VerifiedInputChunkRefCatalog::open(
            &fixture.catalog_root,
            &fixture.cas,
            &fixture.manifest_ref,
            fixture.limits,
            fixture.list_limits,
        )
        .unwrap();
        catalog.admit(&fixture.references[0]).unwrap();
    }
    std::fs::remove_file(fixture.catalog_root.join("catalog.header")).unwrap();

    assert!(matches!(
        VerifiedInputChunkRefCatalog::open(
            &fixture.catalog_root,
            &fixture.cas,
            &fixture.manifest_ref,
            fixture.limits,
            fixture.list_limits,
        ),
        Err(InputRefCatalogError::MissingHeader)
    ));
}

#[test]
fn cold_restart_reopens_each_input_chunk_from_authoritative_cas_and_rederives_its_ref() {
    let limits = poc_schema_limits();
    let list_limits = OrderedListLimits::new(16, 4096, 4096);
    let bundle = support::protocol_bundle();
    let protocol_bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let job_id = hash(30);
    let day = WorldwideDay::new(20_260_725);
    let owner = Address::repeat_byte(31);
    let tribute = TributeBodyV1 {
        tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
        owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(10),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(11),
        reference_currency: 978,
        tribute_price_minor: U256::from(12),
        exclude_from_intex_issuance: false,
    };
    let chunk = AuthenticatedInputChunkV1 {
        protocol_bundle_hash,
        job_id,
        kind: InputChunkKind::Tribute,
        ordinal: 0,
        canonical_records_or_openings: vec![BoundedBytes(encode_tribute_v1(&tribute).unwrap())],
    };
    let directory = tempfile::tempdir().unwrap();
    let cas_limits = CasLimits {
        max_object_bytes: 1_048_576,
        max_total_bytes: 8_388_608,
    };
    let cas = FilesystemCas::open(
        directory.path().join("cas"),
        CasWriterRole::SnapshotExporter,
        cas_limits,
    )
    .unwrap();
    let mut chunk_ref = cas
        .publish_bytes(&chunk.encode_canonical(&limits).unwrap())
        .unwrap();
    chunk_ref.expected_ocb1_kind = Some(ObjectKind::AuthenticatedInputChunkV1.tag());
    let derived_ref =
        derive_input_chunk_ref(&cas.read_verified(&chunk_ref).unwrap(), &bundle, &limits)
            .unwrap()
            .reference;
    let encoded_ref = derived_ref.encode_canonical_record(&limits).unwrap();
    let manifest = InputManifestV1 {
        protocol_bundle_hash,
        job_id,
        attempt: 1,
        checkpoint: CheckpointIdentityV1 {
            finalized_block_number: 40,
            finalized_block_hash: hash(41),
            finalized_state_root: hash(42),
            finalized_ce_root: hash(43),
            ce_schema_version: 1,
        },
        wwd: day.value(),
        sealed_tribute_collection_key: hash(44),
        sealed_tribute_collection_root: hash(45),
        tribute_count: 1,
        tribute_nominal_total: tribute.nominal_amount_minor,
        input_chunk_count: 1,
        input_chunk_list_root: outbe_ocomp_protocol::ordered_list_root(
            ListKind::InputChunkReferences,
            &[encoded_ref],
            list_limits,
        )
        .unwrap(),
        fidelity_opening_root: hash(46),
        oracle_opening_root: hash(47),
        exact_encoded_bytes: derived_ref.encoded_bytes,
        exact_record_count: 1,
        body_codec_id: bundle.tribute_body_codec_id,
        opening_codec_registry_hash: bundle.opening_codec_registry_hash().unwrap(),
        compression: Compression::None,
    };
    let mut manifest_ref = cas
        .publish_bytes(&manifest.encode_canonical(&limits).unwrap())
        .unwrap();
    manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
    let catalog_path = directory.path().join("input-refs");
    {
        let mut catalog = VerifiedInputChunkRefCatalog::open(
            &catalog_path,
            &cas,
            &manifest_ref,
            limits,
            list_limits,
        )
        .unwrap();
        catalog.admit(&derived_ref).unwrap();
    }
    drop(cas);

    let reader = FilesystemCasReader::open(directory.path().join("cas"), cas_limits).unwrap();
    let catalog =
        VerifiedInputChunkRefCatalog::reopen(&catalog_path, &reader, limits, list_limits).unwrap();
    let reopened = catalog
        .exact_verified_cursor(&reader, &bundle)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0].reference, derived_ref);
    assert_eq!(reopened[0].chunk, chunk);
}
