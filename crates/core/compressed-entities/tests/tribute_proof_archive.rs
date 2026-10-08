use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    body_commitment, derive_poseidon_entity_id, encode_tribute_v2,
    tribute_partition_root_from_leaves, verify_body_in_collection, BoundedTributePartitionVerifier,
    CeDomain, StoredBody, TributePartitionExpectationV1, TributePartitionWorkConfig,
    ACTIVE_COMMITMENT_SCHEME, TRIBUTE_BODY_SCHEMA_V2,
};
use outbe_primitives::{
    time::WorldwideDay,
    tribute_encryption::{EncryptedTributeV2, TributeContextV2},
};

#[test]
fn frozen_inventory_proofs_survive_reopen_and_reject_substituted_bodies() {
    let day = WorldwideDay::new(20261007);
    let bodies = (1..=64)
        .map(|index| {
            let owner = Address::from_word(B256::from(U256::from(index)));
            let id = derive_poseidon_entity_id(owner, day).unwrap();
            let immutable = |len| {
                let mut blob = vec![0x42; len];
                blob[..8].copy_from_slice(&1_u64.to_be_bytes());
                blob
            };
            let body = EncryptedTributeV2 {
                context: TributeContextV2 {
                    chain_id: 54322345,
                    tribute_id: id,
                    owner,
                    worldwide_day: day,
                    issuance_currency: 840,
                    reference_currency: 840,
                    tribute_price_minor: U256::from(1_000_000),
                    exclude_from_intex_issuance: false,
                    offer_input_hash: B256::from(U256::from(index)),
                },
                encrypted_creator_public_key: immutable(56),
                encrypted_amounts: immutable(88),
            };
            let stored =
                StoredBody::new(TRIBUTE_BODY_SCHEMA_V2, encode_tribute_v2(&body).unwrap()).unwrap();
            let commitment = body_commitment(
                ACTIVE_COMMITMENT_SCHEME,
                TRIBUTE_BODY_SCHEMA_V2,
                id,
                stored.payload(),
            );
            (id, stored.encode(), commitment)
        })
        .collect::<Vec<_>>();
    let leaves = bodies
        .iter()
        .map(|(id, _, commitment)| (*id, *commitment.as_ref().unwrap()))
        .collect::<Vec<_>>();
    let root = tribute_partition_root_from_leaves(day, leaves.iter().copied()).unwrap();
    let expectation = TributePartitionExpectationV1 {
        day,
        exact_leaf_count: 64,
        expected_collection_root: root,
        commitment_scheme: ACTIVE_COMMITMENT_SCHEME,
    };
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("archive");
    let mut builder = BoundedTributePartitionVerifier::create(
        &path,
        expectation,
        TributePartitionWorkConfig {
            records_per_run: 3,
            merge_fan_in: 2,
        },
    )
    .unwrap();
    for (id, commitment) in leaves.iter().rev() {
        builder.push(*id, *commitment).unwrap();
    }
    let archive = builder.finish_with_archive(|| {}).unwrap();
    assert_eq!(archive.collection_root(), root);
    drop(archive);
    let archive =
        outbe_compressed_entities::open_tribute_proof_archive(&path, expectation).unwrap();
    for (index, (id, body, _)) in bodies.iter().enumerate() {
        let proof = archive.proof(*id).unwrap();
        verify_body_in_collection(root, CeDomain::Tribute, *id, body, &proof).unwrap();
        assert!(verify_body_in_collection(
            root,
            CeDomain::Tribute,
            *id,
            &bodies[(index + 1) % bodies.len()].1,
            &proof
        )
        .is_err());
    }
    let disk_nodes: u64 = std::fs::read_dir(&path)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("proof-shard-")
        })
        .map(|entry| entry.metadata().unwrap().len())
        .sum();
    assert!(
        disk_nodes < 64 * 2 * 182,
        "zero chains must not create disk nodes"
    );
    let mut wrong = expectation;
    wrong.expected_collection_root = B256::repeat_byte(0x11);
    assert!(outbe_compressed_entities::open_tribute_proof_archive(&path, wrong).is_err());
}
