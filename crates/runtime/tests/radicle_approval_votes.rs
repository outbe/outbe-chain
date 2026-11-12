#[allow(dead_code)]
#[path = "radicle_approval/support.rs"]
mod support;
use support::*;

#[test]
fn bootstrap_permission_uses_one_current_finalized_state_and_stops_after_binding() {
    let (mut storage, _) = repository_authority_storage();
    let seeded: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../system/coderegistry/tests/fixtures/governments-genesis-storage.json"
    )))
    .unwrap();
    for (slot, value) in seeded["storage"].as_object().unwrap() {
        storage.storage.insert(
            (CODE_REGISTRY_ADDRESS, slot.parse().unwrap()),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    let mut states = BTreeMap::from([(block(3).hash, snapshot(&storage))]);
    let slots = [
        (10u64, U256::ZERO),
        (10, U256::from(1)),
        (12, U256::from(1)),
        (13, U256::from(1)),
        (14, U256::from(1)),
        (15, U256::from(1)),
    ];
    for (index, (slot, value)) in slots.into_iter().enumerate() {
        let original = storage
            .storage
            .insert((CODE_REGISTRY_ADDRESS, U256::from(slot)), value);
        states.insert(block(index as u64 + 4).hash, snapshot(&storage));
        match original {
            Some(value) => {
                storage
                    .storage
                    .insert((CODE_REGISTRY_ADDRESS, U256::from(slot)), value);
            }
            None => {
                storage
                    .storage
                    .remove(&(CODE_REGISTRY_ADDRESS, U256::from(slot)));
            }
        }
    }
    let provider = ApprovalProvider {
        states,
        canonical: (3..10).map(|i| (i, block(i).hash)).collect(),
        finalized: Some(BlockNumHash::new(3, block(3).hash)),
        reads: Mutex::new(vec![]),
    };
    let mut reader =
        RethSnapshotReader::new(provider, 2026, B256::repeat_byte(7), protocol_state());
    let state = reader.read_current_bootstrap_signing_state().unwrap();
    assert_eq!(state.authority().block(), block(3));
    assert_eq!(state.authority().epoch(), 2);
    assert_eq!(state.descriptor().description, "Network governance");
    assert_eq!(
        reader.provider.reads.lock().unwrap().as_slice(),
        &[block(3).hash]
    );
    for i in 4..10 {
        reader.provider.finalized = Some(BlockNumHash::new(i, block(i).hash));
        assert!(
            reader.read_current_bootstrap_signing_state().is_err(),
            "state {i} accepted"
        );
    }
    reader.provider.finalized = None;
    assert!(reader.read_current_bootstrap_signing_state().is_err());
    assert_eq!(reader.provider.reads.lock().unwrap().len(), 7);
    reader.provider.finalized = Some(BlockNumHash::new(3, B256::repeat_byte(99)));
    assert!(reader.read_current_bootstrap_signing_state().is_err());
    assert_eq!(reader.provider.reads.lock().unwrap().len(), 7);
}

#[test]
fn publication_delivery_progress_uses_one_finalized_state_and_preserves_accepted_history() {
    use commonware_codec::Encode as _;
    use commonware_cryptography::Signer as _;
    use outbe_primitives::radicle_publication::{
        players_commitment, PublicationAttestation, PublicationBatch, PUBLICATION_NAMESPACE,
    };
    let (mut storage, committee) = repository_authority_storage();
    let seeded: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../system/coderegistry/tests/fixtures/governments-genesis-storage.json"
    )))
    .unwrap();
    for (slot, value) in seeded["storage"].as_object().unwrap() {
        storage.storage.insert(
            (CODE_REGISTRY_ADDRESS, slot.parse().unwrap()),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    let (mut body, manifest) = bootstrap_fixture();
    body.package_manifest_hash = manifest.hash().unwrap();
    body.descriptor_hash = storage
        .enter(|s| CodeRegistry::new(s).config_descriptor_hash.read())
        .unwrap();
    body.owner_bls_public =
        alloy_primitives::FixedBytes::from_slice(&committee.vrf_group_public_key_bytes);
    body.players_commitment = players_commitment(
        committee
            .committee
            .iter()
            .map(|m| (m.address, &m.consensus_pubkey)),
    )
    .unwrap();
    body.epoch = 2;
    body.material_version = committee.vrf_material_version;
    let mut keys: Vec<_> = (101..105).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|k| k.public_key().encode());
    let batch = |indices: &[usize]| PublicationBatch {
        body: body.clone(),
        attestations: indices
            .iter()
            .map(|&i| PublicationAttestation {
                participant_index: i as u32,
                signature: alloy_primitives::FixedBytes::from_slice(
                    keys[i]
                        .sign(PUBLICATION_NAMESPACE, &body.to_bytes())
                        .encode()
                        .as_ref(),
                ),
            })
            .collect(),
    };
    storage.set_block_number(3);
    assert!(storage
        .enter(|s| CodeRegistry::new(s).apply_publication_batch(&batch(&[0, 1])))
        .unwrap()
        .receipt
        .is_none());
    let pending = snapshot(&storage);
    storage.set_block_number(4);
    let accepted = storage
        .enter(|s| CodeRegistry::new(s).apply_publication_batch(&batch(&[2])))
        .unwrap()
        .receipt
        .unwrap();
    let complete = snapshot(&storage);
    storage
        .enter(|s| ValidatorSet::new(s).advance_epoch(1005, 5))
        .unwrap();
    let rotated = snapshot(&storage);
    let provider = ApprovalProvider {
        states: [
            (block(3).hash, pending),
            (block(4).hash, complete),
            (block(5).hash, rotated),
        ]
        .into(),
        canonical: (3..6).map(|i| (i, block(i).hash)).collect(),
        finalized: Some(BlockNumHash::new(3, block(3).hash)),
        reads: Mutex::new(vec![]),
    };
    let mut reader =
        RethSnapshotReader::new(provider, 2026, B256::repeat_byte(7), protocol_state());
    let progress = reader.read_current_publication_progress(&body).unwrap();
    assert_eq!(progress.block(), block(3));
    assert_eq!(progress.body_hash(), body.receipt_hash());
    assert_eq!(progress.attesters(), [0, 1]);
    assert!(progress.receipt().is_none());
    assert_eq!(*reader.provider.reads.lock().unwrap(), [block(3).hash]);
    let mut other = body.clone();
    other.package_manifest_hash = B256::repeat_byte(41);
    assert!(reader
        .read_current_publication_progress(&other)
        .unwrap()
        .attesters()
        .is_empty());
    for height in [4, 5] {
        reader.provider.finalized = Some(BlockNumHash::new(height, block(height).hash));
        let progress = reader.read_current_publication_progress(&body).unwrap();
        assert_eq!(progress.block(), block(height));
        assert_eq!(progress.receipt().unwrap().receipt(), &accepted);
        assert!(progress.attesters().is_empty());
        assert!(reader.read_current_publication_progress(&other).is_err());
    }
    let before = reader.provider.reads.lock().unwrap().len();
    let mut wrong = body.clone();
    wrong.genesis_hash = B256::repeat_byte(99);
    assert!(reader.read_current_publication_progress(&wrong).is_err());
    reader.provider.finalized = None;
    assert!(reader.read_current_publication_progress(&body).is_err());
    reader.provider.finalized = Some(BlockNumHash::new(5, B256::repeat_byte(99)));
    assert!(reader.read_current_publication_progress(&body).is_err());
    assert_eq!(reader.provider.reads.lock().unwrap().len(), before);
    reader.provider.finalized = Some(BlockNumHash::new(5, block(5).hash));
    reader.provider.states.remove(&block(5).hash);
    assert!(reader.read_current_publication_progress(&body).is_err());
}
