mod adversarial;
use std::sync::{Arc, Mutex, MutexGuard};

use alloy_primitives::{Address, U256};
use outbe_primitives::time::WorldwideDay;
use proptest::prelude::*;

use super::*;
use crate::{
    encode_nod_bucket_v1, encode_nod_item_v1, encode_tribute_v1, CandidateCacheLimits, CeMdbx,
    CeTopologyV1, EntityRef, EnvironmentIdentity, FinalLeafMutation, NodBucketBodyV1,
    NodItemBodyV1, PartitionRef, TributeBodyV1, LOCAL_STORAGE_SCHEMA_VERSION,
};
use outbe_primitives::reshare_artifact::{
    encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
};

static PROOF_TEST_LOCK: Mutex<()> = Mutex::new(());

fn proof_test_guard() -> MutexGuard<'static, ()> {
    PROOF_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn service() -> (tempfile::TempDir, Arc<CompressedTreeService>, B256) {
    let directory = tempfile::tempdir().unwrap();
    let genesis_hash = B256::repeat_byte(0x11);
    let db = CeMdbx::open(
        directory.path(),
        EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 7,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon2-catalog-v3".into(),
            vendor_revision: "adr013-test".into(),
        },
        FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: crate::sealed_root(B256::ZERO).unwrap(),
        },
    )
    .unwrap();
    let service = Arc::new(
        CompressedTreeService::new(
            db,
            CandidateCacheLimits {
                max_candidates: 2,
                max_encoded_bytes: 1_000_000,
            },
        )
        .unwrap(),
    );
    (directory, service, genesis_hash)
}

fn bucket_body(last: u8) -> (WwdEntityId, Vec<u8>, crate::Commitment) {
    let body = NodBucketBodyV1 {
        settled_nods: 0,
        bucket_key: B256::repeat_byte(last),
        worldwide_day: WorldwideDay::new(20_260_717),
        entry_price_minor: U256::from(11),
        reference_currency: 840,
    };
    let id = body.entity_id();
    let stored =
        StoredBody::new(crate::BODY_SCHEMA_V1, encode_nod_bucket_v1(&body).unwrap()).unwrap();
    let leaf = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        stored.schema_version(),
        id,
        stored.payload(),
    )
    .unwrap();
    (id, stored.encode(), leaf)
}

fn stored_body(id: WwdEntityId, payload: Vec<u8>) -> (Vec<u8>, crate::Commitment) {
    let stored = StoredBody::new(crate::BODY_SCHEMA_V1, payload).unwrap();
    let leaf = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        stored.schema_version(),
        id,
        stored.payload(),
    )
    .unwrap();
    (stored.encode(), leaf)
}

fn finalize_one(
    service: &CompressedTreeService,
    genesis_hash: B256,
    entity: EntityRef,
    leaf: crate::Commitment,
) -> (FinalizedMarker, SelectedHeaderV1) {
    let parent_root = crate::sealed_root(B256::ZERO).unwrap();
    let block_hash = B256::repeat_byte(0x22);
    let provisional = service
        .open_parent(ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: genesis_hash,
            root: parent_root,
        })
        .unwrap()
        .prepare_seal(
            1,
            &[FinalLeafMutation {
                entity,
                final_leaf: Some(leaf),
            }],
            &[],
        )
        .unwrap();
    let root = provisional.new_root();
    service.publish_candidate(block_hash, provisional).unwrap();
    let marker = service
        .apply_finalized(1, block_hash, root)
        .unwrap()
        .marker();
    let extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
        compressed_entities_root: Some(CompressedEntitiesRootArtifact {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            r_sealed: root,
        }),
        ..Default::default()
    })
    .unwrap()
    .to_vec();
    (
        marker,
        SelectedHeaderV1 {
            block_number: 1,
            block_hash,
            extra_data,
        },
    )
}

fn finalize_empty(service: &CompressedTreeService, genesis_hash: B256) -> SelectedHeaderV1 {
    let parent_root = crate::sealed_root(B256::ZERO).unwrap();
    let provisional = service
        .open_parent(ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 0,
            block_hash: genesis_hash,
            root: parent_root,
        })
        .unwrap()
        .prepare_seal(1, &[], &[])
        .unwrap();
    let root = provisional.new_root();
    let block_hash = B256::repeat_byte(0x24);
    service.publish_candidate(block_hash, provisional).unwrap();
    service.apply_finalized(1, block_hash, root).unwrap();
    SelectedHeaderV1 {
        block_number: 1,
        block_hash,
        extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                r_sealed: root,
            }),
            ..Default::default()
        })
        .unwrap()
        .to_vec(),
    }
}

#[test]
fn frozen_collection_proof_binds_encrypted_creator_key_and_body() {
    let _guard = proof_test_guard();
    let (_dir, service, genesis_hash) = service();
    let day = WorldwideDay::new(20_260_717);
    let id = WwdEntityId::from_day_and_digest(day, [0x23; 32]);
    let mut blob = vec![0x91; outbe_primitives::tribute_encryption::TRIBUTE_PUBLIC_KEY_BLOB_LEN];
    blob[..8].copy_from_slice(&1u64.to_be_bytes());
    let mut amounts = vec![0x74; outbe_primitives::tribute_encryption::TRIBUTE_AMOUNTS_BLOB_LEN];
    amounts[..8].copy_from_slice(&1u64.to_be_bytes());
    let mut body = outbe_primitives::tribute_encryption::EncryptedTributeV2 {
        context: outbe_primitives::tribute_encryption::TributeContextV2 {
            chain_id: 7,
            tribute_id: id,
            owner: Address::repeat_byte(0x12),
            worldwide_day: day,
            issuance_currency: 840,
            reference_currency: 978,
            tribute_price_minor: U256::from(99),
            offer_input_hash: B256::repeat_byte(0x42),
        },
        encrypted_creator_public_key: blob,
        encrypted_amounts: amounts,
    };
    let stored = StoredBody::new(
        crate::TRIBUTE_BODY_SCHEMA_V2,
        crate::encode_tribute_v2(&body).unwrap(),
    )
    .unwrap();
    let leaf = body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        stored.schema_version(),
        id,
        stored.payload(),
    )
    .unwrap();
    let (_, header) = finalize_one(&service, genesis_hash, EntityRef::Tribute(id), leaf);
    let request = PointReadRequestV1 {
        domain_id: 1,
        raw_id: id,
    };
    let response = service
        .serve_point_read_v1(
            7,
            request,
            |_, _| Some(header.clone()),
            |_, _| Some(stored.encode()),
        )
        .unwrap();
    assert_eq!(
        verify_point_read_v1(7, request, &header, &response).unwrap(),
        VerifiedPointReadV1::Present
    );
    let PointReadResultV1::Present { evidence, .. } = response else {
        panic!("present proof")
    };
    let proof = CollectionBodyProofV1::from(&evidence);
    let collection = collection_key(CeDomain::Tribute, id).unwrap();
    let root = verify_collection(
        CeDomain::Tribute,
        id,
        collection,
        B256::from(*leaf.as_bytes()),
        &proof,
    )
    .unwrap();
    drop(service);
    assert!(
        verify_body_in_collection(root, CeDomain::Tribute, id, &stored.encode(), &proof).is_ok()
    );
    assert!(
        verify_body_in_collection(B256::ZERO, CeDomain::Tribute, id, &stored.encode(), &proof)
            .is_err()
    );
    let other = WwdEntityId::from_day_and_digest(day, [0x24; 32]);
    assert!(
        verify_body_in_collection(root, CeDomain::Tribute, other, &stored.encode(), &proof)
            .is_err()
    );
    body.encrypted_creator_public_key[12] ^= 1;
    let substituted = StoredBody::new(
        crate::TRIBUTE_BODY_SCHEMA_V2,
        crate::encode_tribute_v2(&body).unwrap(),
    )
    .unwrap();
    assert!(
        verify_body_in_collection(root, CeDomain::Tribute, id, &substituted.encode(), &proof)
            .is_err()
    );
}

#[test]
fn present_and_both_absence_variants_verify_against_header() {
    let _guard = proof_test_guard();
    let (_dir, service, genesis_hash) = service();
    let (id, body, leaf) = bucket_body(7);
    let (marker, header) = finalize_one(&service, genesis_hash, EntityRef::NodBucket(id), leaf);
    let request = PointReadRequestV1 {
        domain_id: 3,
        raw_id: id,
    };
    let present = service
        .serve_point_read_v1(
            7,
            request,
            |height, hash| {
                assert_eq!((height, hash), (marker.height, marker.block_hash));
                Some(header.clone())
            },
            |domain, raw_id| {
                assert_eq!((domain, raw_id), (CeDomain::NodBucket, id));
                Some(body.clone())
            },
        )
        .unwrap();
    assert_eq!(
        verify_point_read_v1(7, request, &header, &present).unwrap(),
        VerifiedPointReadV1::Present
    );

    let (missing, _, _) = bucket_body(8);
    let missing_request = PointReadRequestV1 {
        domain_id: 3,
        raw_id: missing,
    };
    let entity_absent = service
        .serve_point_read_v1(
            7,
            missing_request,
            |_, _| Some(header.clone()),
            |_, _| panic!("absence must not read Mongo"),
        )
        .unwrap();
    assert!(matches!(
        entity_absent,
        PointReadResultV1::Absent {
            evidence: AbsentEvidenceV1::EntityAbsentInCollection { .. },
            ..
        }
    ));
    assert_eq!(
        verify_point_read_v1(7, missing_request, &header, &entity_absent).unwrap(),
        VerifiedPointReadV1::Absent
    );

    let tribute_request = PointReadRequestV1 {
        domain_id: 1,
        raw_id: missing,
    };
    let collection_absent = service
        .serve_point_read_v1(
            7,
            tribute_request,
            |_, _| Some(header.clone()),
            |_, _| panic!("absence must not read Mongo"),
        )
        .unwrap();
    assert!(matches!(
        collection_absent,
        PointReadResultV1::Absent {
            evidence: AbsentEvidenceV1::CollectionAbsent { .. },
            ..
        }
    ));
    assert_eq!(
        verify_point_read_v1(7, tribute_request, &header, &collection_absent).unwrap(),
        VerifiedPointReadV1::Absent
    );
}

#[test]
fn bad_header_body_and_request_fail_closed() {
    let _guard = proof_test_guard();
    let (_genesis_dir, genesis_service, _) = service();
    let (genesis_id, _, _) = bucket_body(6);
    assert_eq!(
        genesis_service
            .serve_point_read_v1(
                7,
                PointReadRequestV1 {
                    domain_id: CeDomain::NodBucket.id(),
                    raw_id: genesis_id,
                },
                |_, _| panic!("genesis must not perform a header lookup"),
                |_, _| panic!("genesis must not perform a body lookup"),
            )
            .unwrap(),
        PointReadResultV1::Unavailable
    );
    let (_dir, service, genesis_hash) = service();
    let (id, body, leaf) = bucket_body(7);
    let (_, header) = finalize_one(&service, genesis_hash, EntityRef::NodBucket(id), leaf);
    let request = PointReadRequestV1 {
        domain_id: 3,
        raw_id: id,
    };
    let mut wrong_header = header.clone();
    wrong_header.block_hash = B256::repeat_byte(0x99);
    assert_eq!(
        service
            .serve_point_read_v1(
                7,
                request,
                |_, _| Some(wrong_header),
                |_, _| Some(body.clone())
            )
            .unwrap(),
        PointReadResultV1::Unavailable
    );
    assert_eq!(
        service
            .serve_point_read_v1(
                7,
                request,
                |_, _| Some(header.clone()),
                |_, _| Some(vec![1, 2, 3]),
            )
            .unwrap(),
        PointReadResultV1::Unavailable
    );
    assert_eq!(
        service
            .serve_point_read_v1(7, request, |_, _| Some(header.clone()), |_, _| None)
            .unwrap(),
        PointReadResultV1::Unavailable
    );
    let (_, wrong_identity_body, _) = bucket_body(8);
    assert_eq!(
        service
            .serve_point_read_v1(
                7,
                request,
                |_, _| Some(header.clone()),
                |_, _| Some(wrong_identity_body),
            )
            .unwrap(),
        PointReadResultV1::Unavailable
    );
    let changed = NodBucketBodyV1 {
        settled_nods: 0,
        bucket_key: B256::repeat_byte(7),
        worldwide_day: WorldwideDay::new(20_260_717),
        entry_price_minor: U256::from(999),
        reference_currency: 840,
    };
    assert_eq!(changed.entity_id(), id);
    let (wrong_leaf_body, _) = stored_body(id, encode_nod_bucket_v1(&changed).unwrap());
    assert_eq!(
        service
            .serve_point_read_v1(
                7,
                request,
                |_, _| Some(header.clone()),
                |_, _| Some(wrong_leaf_body),
            )
            .unwrap(),
        PointReadResultV1::Unavailable
    );
    assert_eq!(
        service.serve_point_read_v1(
            7,
            PointReadRequestV1 {
                domain_id: 99,
                raw_id: id
            },
            |_, _| None,
            |_, _| None,
        ),
        Err(PointReadRequestError::UnknownDomain(99))
    );
}

mod transport;

#[test]
fn one_snapshot_survives_two_finalized_writes_before_mongo_read() {
    let _guard = proof_test_guard();
    let (_dir, service, genesis_hash) = service();
    let (id, body_h1, leaf_h1) = bucket_body(7);
    let (marker_h1, header_h1) =
        finalize_one(&service, genesis_hash, EntityRef::NodBucket(id), leaf_h1);
    let (_, _body_h2, leaf_h2) = bucket_body(8);
    let request = PointReadRequestV1 {
        domain_id: 3,
        raw_id: id,
    };
    let writer = Arc::clone(&service);
    let selected_header = header_h1.clone();
    let package = service
        .serve_point_read_v1(
            7,
            request,
            move |height, hash| {
                assert_eq!((height, hash), (1, marker_h1.block_hash));
                let block2 = B256::repeat_byte(0x52);
                let candidate2 = writer
                    .open_parent(ExactParentIdentity {
                        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                        block_number: marker_h1.height,
                        block_hash: marker_h1.block_hash,
                        root: marker_h1.new_root,
                    })
                    .unwrap()
                    .prepare_seal(
                        2,
                        &[FinalLeafMutation {
                            entity: EntityRef::NodBucket(id),
                            final_leaf: Some(leaf_h2),
                        }],
                        &[],
                    )
                    .unwrap();
                let root2 = candidate2.new_root();
                writer.publish_candidate(block2, candidate2).unwrap();
                let marker2 = writer.apply_finalized(2, block2, root2).unwrap().marker();

                let block3 = B256::repeat_byte(0x53);
                let candidate3 = writer
                    .open_parent(ExactParentIdentity {
                        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                        block_number: marker2.height,
                        block_hash: marker2.block_hash,
                        root: marker2.new_root,
                    })
                    .unwrap()
                    .prepare_seal(
                        3,
                        &[FinalLeafMutation {
                            entity: EntityRef::NodBucket(id),
                            final_leaf: None,
                        }],
                        &[],
                    )
                    .unwrap();
                let root3 = candidate3.new_root();
                writer.publish_candidate(block3, candidate3).unwrap();
                writer.apply_finalized(3, block3, root3).unwrap();
                Some(selected_header.clone())
            },
            |_, _| Some(body_h1),
        )
        .unwrap();
    assert_eq!(service.finalized_marker().unwrap().height, 3);
    assert_eq!(
        verify_point_read_v1(7, request, &header_h1, &package).unwrap(),
        VerifiedPointReadV1::Present
    );
}

#[test]
fn two_nodes_emit_identical_packages_for_the_same_finalized_state() {
    let _guard = proof_test_guard();
    let (_dir_a, node_a, genesis_a) = service();
    let (_dir_b, node_b, genesis_b) = service();
    let (id, body, leaf) = bucket_body(9);
    let (_, header_a) = finalize_one(&node_a, genesis_a, EntityRef::NodBucket(id), leaf);
    let (_, header_b) = finalize_one(&node_b, genesis_b, EntityRef::NodBucket(id), leaf);
    assert_eq!(header_a, header_b);
    let request = PointReadRequestV1 {
        domain_id: CeDomain::NodBucket.id(),
        raw_id: id,
    };
    let package_a = node_a
        .serve_point_read_v1(
            7,
            request,
            |_, _| Some(header_a.clone()),
            |_, _| Some(body.clone()),
        )
        .unwrap();
    let package_b = node_b
        .serve_point_read_v1(7, request, |_, _| Some(header_b.clone()), |_, _| Some(body))
        .unwrap();
    assert_eq!(package_a, package_b);
    assert_eq!(
        verify_point_read_v1(7, request, &header_a, &package_b).unwrap(),
        VerifiedPointReadV1::Present
    );
}

#[test]
fn pre_retirement_reader_finishes_and_next_request_proves_collection_absence() {
    let _guard = proof_test_guard();
    let (_dir, service, genesis_hash) = service();
    let day = WorldwideDay::new(20_260_717);
    let id = WwdEntityId::from_day_and_digest(day, [0x71; 32]);
    let tribute = TributeBodyV1 {
        tribute_id: id,
        owner: Address::repeat_byte(0x72),
        worldwide_day: day,
        issuance_amount_minor: U256::from(1),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(2),
        reference_currency: 978,
        tribute_price_minor: U256::from(3),
    };
    let (body, leaf) = stored_body(id, encode_tribute_v1(&tribute).unwrap());
    let (marker1, header1) = finalize_one(&service, genesis_hash, EntityRef::Tribute(id), leaf);
    let request = PointReadRequestV1 {
        domain_id: 1,
        raw_id: id,
    };
    let writer = Arc::clone(&service);
    let old_header = header1.clone();
    let marker2 = std::cell::Cell::new(None);
    let old_package = service
        .serve_point_read_v1(
            7,
            request,
            |_, _| {
                let block2 = B256::repeat_byte(0x62);
                let candidate = writer
                    .open_parent(ExactParentIdentity {
                        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                        block_number: marker1.height,
                        block_hash: marker1.block_hash,
                        root: marker1.new_root,
                    })
                    .unwrap()
                    .prepare_seal(2, &[], &[PartitionRef::TributeWwd(day)])
                    .unwrap();
                let root2 = candidate.new_root();
                writer.publish_candidate(block2, candidate).unwrap();
                marker2.set(Some(
                    writer.apply_finalized(2, block2, root2).unwrap().marker(),
                ));
                Some(old_header.clone())
            },
            |_, _| Some(body),
        )
        .unwrap();
    assert_eq!(
        verify_point_read_v1(7, request, &header1, &old_package).unwrap(),
        VerifiedPointReadV1::Present
    );
    let marker2 = marker2.get().unwrap();
    let header2 = SelectedHeaderV1 {
        block_number: marker2.height,
        block_hash: marker2.block_hash,
        extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                r_sealed: marker2.new_root,
            }),
            ..Default::default()
        })
        .unwrap()
        .to_vec(),
    };
    let absent = service
        .serve_point_read_v1(
            7,
            request,
            |_, _| Some(header2.clone()),
            |_, _| panic!("retired collection absence must not read Mongo"),
        )
        .unwrap();
    assert!(matches!(
        absent,
        PointReadResultV1::Absent {
            evidence: AbsentEvidenceV1::CollectionAbsent { .. },
            ..
        }
    ));
    assert_eq!(
        verify_point_read_v1(7, request, &header2, &absent).unwrap(),
        VerifiedPointReadV1::Absent
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn proof_and_package_decoders_never_panic_on_bounded_adversarial_bytes(
        proof in proptest::collection::vec(any::<u8>(), 0..2048),
        raw in proptest::collection::vec(any::<u8>(), 0..1024),
    ) {
        let _guard = proof_test_guard();
        let proof_json = serde_json::json!(format!("0x{}", hex::encode(proof)));
        let _ = serde_json::from_value::<CkbCompiledProofV1>(proof_json);
        let _ = serde_json::from_slice::<PointReadResultV1>(&raw);
        let _ = serde_json::from_slice::<PointReadRequestV1>(&raw);
    }
}
