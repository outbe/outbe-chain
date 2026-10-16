use super::*;
use std::cell::Cell;

#[test]
fn future_or_unknown_finality_never_reads_a_header() {
    for finalized in [None, Some(9)] {
        assert!(finalized_header_at(finalized, 10, || {
            panic!("an ineligible point read must not load a header")
        })
        .is_none());
    }
}

#[test]
fn eligible_finality_loads_once_and_preserves_unavailable_or_selected_header() {
    let calls = Cell::new(0);
    assert!(finalized_header_at(Some(10), 10, || {
        calls.set(calls.get() + 1);
        None
    })
    .is_none());
    let header = finalized_header_at(Some(11), 10, || {
        calls.set(calls.get() + 1);
        Some(SelectedHeaderV1 {
            block_number: 10,
            block_hash: B256::repeat_byte(7),
            extra_data: vec![1, 2, 3],
        })
    })
    .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(header.extra_data, [1, 2, 3]);
    assert_eq!(header.block_hash, B256::repeat_byte(7));
}

#[test]
fn point_read_domain_4_and_domain_5_round_trip_present_then_absent() {
    use alloy_primitives::{Address, U256};
    use outbe_compressed_entities::{
        body_commitment, sealed_root, verify_point_read_v1, CandidateCacheLimits, CeMdbx,
        CeTopologyV1, EntityRef, EnvironmentIdentity, ExactParentIdentity, FinalLeafMutation,
        FinalizedMarker, VerifiedPointReadV1, ACTIVE_COMMITMENT_SCHEME,
        LOCAL_STORAGE_SCHEMA_VERSION,
    };
    use outbe_offchain_storage::MemoryStorage;
    use outbe_primitives::{
        reshare_artifact::{
            encode_outbe_block_artifacts, CompressedEntitiesRootArtifact, OutbeBlockArtifacts,
        },
        time::WorldwideDay,
    };
    let directory = tempfile::tempdir().unwrap();
    let genesis_hash = B256::repeat_byte(0x91);
    let empty_root = sealed_root(B256::ZERO).unwrap();
    let db = CeMdbx::open(
        directory.path(),
        EnvironmentIdentity {
            local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
            chain_id: 91,
            genesis_hash,
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            topology: CeTopologyV1.encode(),
            tree_format: "ckb-smt-v0.6.1-poseidon2-catalog-v3".into(),
            vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".into(),
        },
        FinalizedMarker {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            height: 0,
            block_hash: genesis_hash,
            parent_block_hash: B256::ZERO,
            parent_root: B256::ZERO,
            new_root: empty_root,
        },
    )
    .unwrap();
    let storage = Arc::new(MemoryStorage::new());
    let writer = outbe_gem::gem_writer(storage.clone(), storage.clone());
    let runtime = PointReadRuntime {
        tree: Arc::new(
            CompressedTreeService::new(
                db,
                CandidateCacheLimits {
                    max_candidates: 2,
                    max_encoded_bytes: 1_000_000,
                },
            )
            .unwrap(),
        ),
        bodies: outbe_offchain_data::runtime_body_readers(storage),
        chain_id: 91,
    };
    let mut item = outbe_gem::GemItemState {
        gem_id: WwdEntityId::from(B256::repeat_byte(0xff)),
        owner: Address::repeat_byte(1),
        gem_type: 1,
        promis_load_minor: U256::from(100),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 123,
        bucket_key: B256::repeat_byte(3),
        state: 0,
        called_at: 0,
        settled_at: 0,
    };
    let mut bucket = outbe_gem::GemBucketState {
        bucket_key: item.bucket_key,
        worldwide_day: WorldwideDay::new(20260715),
        entry_price_minor: U256::from(3),
        reference_currency: 978,
        settled_gems: 0,
    };
    let identities = [
        (4, EntityRef::Gem(item.gem_id), item.gem_id),
        (
            5,
            EntityRef::GemBucket(bucket.entity_id()),
            bucket.entity_id(),
        ),
    ];
    let mut parent = ExactParentIdentity {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        block_number: 0,
        block_hash: genesis_hash,
        root: empty_root,
    };
    let mut previous_header: Option<SelectedHeaderV1> = None;
    for number in 1..=3 {
        if number == 2 {
            item.promis_load_minor += U256::from(1);
            bucket.settled_gems = 1;
        }
        if number == 3 {
            writer.delete_item(item.gem_id).unwrap();
            writer.delete_bucket(bucket.entity_id()).unwrap();
        } else {
            writer.put_item(&item).unwrap();
            writer.put_bucket(&bucket).unwrap();
        }
        // Projection at a newer frontier cannot authenticate against the old finalized root.
        if let Some(header) = &previous_header {
            for (domain_id, _, raw_id) in identities {
                let result = runtime
                    .tree
                    .serve_point_read_v1(
                        91,
                        PointReadRequestV1 { domain_id, raw_id },
                        |_, _| Some(header.clone()),
                        |domain, id| runtime.body(domain, id),
                    )
                    .unwrap();
                assert!(matches!(result, PointReadResultV1::Unavailable));
            }
        }
        let payloads = [
            outbe_compressed_entities::encode_gem_v1(&outbe_gem::canonical_item(&item)).unwrap(),
            outbe_compressed_entities::encode_gem_bucket_v1(&outbe_gem::canonical_bucket(&bucket))
                .unwrap(),
        ];
        let mutations: Vec<_> = identities
            .iter()
            .zip(payloads.iter())
            .map(|((_, entity, id), payload)| FinalLeafMutation {
                entity: *entity,
                final_leaf: (number != 3)
                    .then(|| body_commitment(ACTIVE_COMMITMENT_SCHEME, 1, *id, payload).unwrap()),
            })
            .collect();
        let prepared = runtime
            .tree
            .open_parent(parent)
            .unwrap()
            .prepare_seal(number, &mutations, &[])
            .unwrap();
        let root = prepared.new_root();
        let hash = B256::from(U256::from(number));
        runtime.tree.publish_candidate(hash, prepared).unwrap();
        runtime.tree.apply_finalized(number, hash, root).unwrap();
        let header = SelectedHeaderV1 {
            block_number: number,
            block_hash: hash,
            extra_data: encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                    commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                    r_sealed: root,
                }),
                ..Default::default()
            })
            .unwrap()
            .to_vec(),
        };
        for (domain_id, _, raw_id) in identities {
            let request = PointReadRequestV1 { domain_id, raw_id };
            let result = runtime
                .tree
                .serve_point_read_v1(
                    91,
                    request,
                    |_, _| Some(header.clone()),
                    |domain, id| runtime.body(domain, id),
                )
                .unwrap();
            assert_eq!(
                verify_point_read_v1(91, request, &header, &result).unwrap(),
                if number == 3 {
                    VerifiedPointReadV1::Absent
                } else {
                    VerifiedPointReadV1::Present
                }
            );
            let mut wrong_header = header.clone();
            wrong_header.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
                compressed_entities_root: Some(CompressedEntitiesRootArtifact {
                    commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
                    r_sealed: empty_root,
                }),
                ..Default::default()
            })
            .unwrap()
            .to_vec();
            if number != 3 {
                assert!(verify_point_read_v1(91, request, &wrong_header, &result).is_err());
            }
        }
        parent = ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: number,
            block_hash: hash,
            root,
        };
        previous_header = Some(header);
    }
}
