// OCOMP-TEST-ID: OCM-CAS-001

mod support;

use alloy_primitives::{Address, B256, U256};
use outbe_common::WorldwideDay;
use outbe_compressed_entities::{derive_poseidon_entity_id, encode_tribute_v1, TributeBodyV1};
use outbe_ocomp::{
    cas::{CasLimits, CasWriterRole, FilesystemCas},
    control::poc_schema_limits,
    input_artifacts::{derive_input_chunk_ref, verify_input_artifact_set},
};
use outbe_ocomp_protocol::{
    common::{BoundedBytes, ProofBytes},
    input::{
        authenticated_opening_root, materialize_authenticated_openings, AuthenticatedInputChunkV1,
        CheckpointIdentityV1, Compression, InputChunkKind, InputManifestV1, OpeningSourceKind,
    },
    opening::{
        LysisOpeningsProofV1, OpeningSubjectsV1, RawContractOpeningProofV1, RawStorageSlotV1,
    },
    registry::ObjectKind,
    ListKind, OrderedListLimits,
};

#[test]
fn worker_reconstructs_the_complete_manifest_from_exact_cas_streams() {
    let limits = poc_schema_limits();
    let list_limits = OrderedListLimits::new(16, 1_048_576, 1_024);
    let bundle = support::protocol_bundle();
    let bundle_hash = bundle.protocol_bundle_hash(&limits).unwrap();
    let job_id = B256::repeat_byte(0x30);
    let finalized_state_root = B256::repeat_byte(0x32);
    let day = WorldwideDay::new(20_260_724);
    let owner = Address::repeat_byte(0x44);
    let tribute = TributeBodyV1 {
        tribute_id: derive_poseidon_entity_id(owner, day).unwrap(),
        owner,
        worldwide_day: day,
        issuance_amount_minor: U256::from(99),
        issuance_currency: 826,
        nominal_amount_minor: U256::from(100),
        reference_currency: 978,
        tribute_price_minor: U256::from(3),
        exclude_from_intex_issuance: false,
    };
    let raw_opening = |address, slot_byte, value| RawContractOpeningProofV1 {
        contract_address: address,
        state_root: finalized_state_root,
        ordered_slots: vec![RawStorageSlotV1 {
            slot: B256::repeat_byte(slot_byte),
            value: U256::from(value),
        }],
        account_proof: ProofBytes(vec![0xa1]),
        storage_proof: ProofBytes(vec![0xb1]),
    };
    let materialized = materialize_authenticated_openings(
        &LysisOpeningsProofV1 {
            protocol_bundle_hash: bundle_hash,
            job_id,
            finalized_block_hash: B256::repeat_byte(0x31),
            finalized_state_root,
            wwd: day.value(),
            subjects: OpeningSubjectsV1 {
                owners: vec![owner],
                settlement_isos: vec![840, 978],
            },
            fidelity: raw_opening(Address::repeat_byte(0x45), 0x46, 7),
            oracle: raw_opening(Address::repeat_byte(0x47), 0x48, 8),
        },
        &bundle,
        &limits,
    )
    .unwrap();
    let fidelity = materialized.fidelity;
    let oracle = materialized.oracle;
    let chunks = [
        AuthenticatedInputChunkV1 {
            protocol_bundle_hash: bundle_hash,
            job_id,
            kind: InputChunkKind::Tribute,
            ordinal: 0,
            canonical_records_or_openings: vec![BoundedBytes(encode_tribute_v1(&tribute).unwrap())],
        },
        AuthenticatedInputChunkV1 {
            protocol_bundle_hash: bundle_hash,
            job_id,
            kind: InputChunkKind::Fidelity,
            ordinal: 1,
            canonical_records_or_openings: vec![BoundedBytes(
                fidelity.encode_canonical_record(&limits).unwrap(),
            )],
        },
        AuthenticatedInputChunkV1 {
            protocol_bundle_hash: bundle_hash,
            job_id,
            kind: InputChunkKind::Oracle,
            ordinal: 2,
            canonical_records_or_openings: vec![BoundedBytes(
                oracle.encode_canonical_record(&limits).unwrap(),
            )],
        },
    ];

    let directory = tempfile::tempdir().unwrap();
    let cas = FilesystemCas::open(
        directory.path(),
        CasWriterRole::SnapshotExporter,
        CasLimits {
            max_object_bytes: 1_048_576,
            max_total_bytes: 8_388_608,
        },
    )
    .unwrap();
    let mut chunk_objects = Vec::new();
    let mut chunk_refs = Vec::new();
    for chunk in &chunks {
        let encoded = chunk.encode_canonical(&limits).unwrap();
        let mut reference = cas.publish_bytes(&encoded).unwrap();
        reference.expected_ocb1_kind = Some(ObjectKind::AuthenticatedInputChunkV1.tag());
        let object = cas.read_verified(&reference).unwrap();
        chunk_refs.push(
            derive_input_chunk_ref(&object, &bundle, &limits)
                .unwrap()
                .reference,
        );
        chunk_objects.push(object);
    }
    let chunk_ref_bytes = chunk_refs
        .iter()
        .map(|reference| reference.encode_canonical_record(&limits).unwrap())
        .collect::<Vec<_>>();
    let input_chunk_list_root = outbe_ocomp_protocol::ordered_list_root(
        ListKind::InputChunkReferences,
        &chunk_ref_bytes,
        list_limits,
    )
    .unwrap();
    let fidelity_opening_root = authenticated_opening_root(
        OpeningSourceKind::Fidelity,
        &[fidelity],
        &bundle,
        &limits,
        list_limits,
    )
    .unwrap();
    let oracle_opening_root = authenticated_opening_root(
        OpeningSourceKind::Oracle,
        &[oracle],
        &bundle,
        &limits,
        list_limits,
    )
    .unwrap();
    let manifest = InputManifestV1 {
        protocol_bundle_hash: bundle_hash,
        job_id,
        attempt: 1,
        checkpoint: CheckpointIdentityV1 {
            finalized_block_number: 90,
            finalized_block_hash: B256::repeat_byte(0x31),
            finalized_state_root,
            finalized_ce_root: B256::repeat_byte(0x33),
            ce_schema_version: 1,
        },
        wwd: day.value(),
        sealed_tribute_collection_key: B256::repeat_byte(0x34),
        sealed_tribute_collection_root: B256::repeat_byte(0x35),
        tribute_count: 1,
        tribute_nominal_total: U256::from(100),
        input_chunk_count: 3,
        input_chunk_list_root,
        fidelity_opening_root,
        oracle_opening_root,
        exact_encoded_bytes: chunk_refs
            .iter()
            .map(|reference| reference.encoded_bytes)
            .sum(),
        exact_record_count: 3,
        body_codec_id: bundle.tribute_body_codec_id,
        opening_codec_registry_hash: bundle.opening_codec_registry_hash().unwrap(),
        compression: Compression::None,
    };
    let manifest_bytes = manifest.encode_canonical(&limits).unwrap();
    let mut manifest_ref = cas.publish_bytes(&manifest_bytes).unwrap();
    manifest_ref.expected_ocb1_kind = Some(ObjectKind::InputManifestV1.tag());
    let manifest_object = cas.read_verified(&manifest_ref).unwrap();

    let verified = verify_input_artifact_set(
        &manifest_object,
        &chunk_objects,
        &bundle,
        &limits,
        list_limits,
    )
    .unwrap();
    assert_eq!(
        verified.manifest_hash,
        manifest.manifest_hash(&limits).unwrap()
    );
    assert_eq!(verified.tribute_count, 1);
    assert_eq!(verified.tribute_nominal_total, U256::from(100));

    chunk_objects.swap(0, 1);
    assert!(verify_input_artifact_set(
        &manifest_object,
        &chunk_objects,
        &bundle,
        &limits,
        list_limits,
    )
    .is_err());
}
