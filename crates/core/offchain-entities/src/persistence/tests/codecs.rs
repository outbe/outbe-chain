use super::*;

#[test]
fn typed_codecs_round_trip_and_reject_trailing_unknown_and_zero_leaf() {
    let field = FieldValue::try_from(b256(1)).unwrap();
    let node = BranchNode {
        left: MergeValue::Value(field),
        right: MergeValue::MergeWithZero {
            base_node: FieldValue::try_from(b256(2)).unwrap(),
            zero_bits: FieldValue::try_from(b256(3)).unwrap(),
            zero_count: 0,
        },
    };
    let encoded = node.encode();
    assert_eq!(BranchNode::decode(&encoded).unwrap(), node);
    let mut trailing = encoded;
    trailing.push(7);
    assert!(matches!(
        BranchNode::decode(&trailing),
        Err(PersistenceError::TrailingBytes { .. })
    ));
    assert!(matches!(
        BranchNode::decode(&[2; 66]),
        Err(PersistenceError::UnknownMergeValueTag(2))
    ));
    assert!(matches!(
        LeafValue::try_from(B256::ZERO),
        Err(PersistenceError::ZeroPersistedLeaf)
    ));
}

#[test]
fn v3_tree_namespaces_are_typed_canonical_and_domain_bounded() {
    let entity = crate::WwdEntityId::from([7_u8; 32]);
    let collection = crate::collection_key(crate::CeDomain::NodItem, entity).unwrap();
    let namespaces = [
        TreeNamespace::Catalog,
        TreeNamespace::CollectionShard(collection, 0),
        TreeNamespace::CollectionShard(collection, K_PROVISIONAL - 1),
    ];
    for namespace in namespaces {
        let encoded = namespace.encode();
        assert_eq!(TreeNamespace::decode(&encoded).unwrap(), namespace);
    }
    assert!(TreeNamespace::decode(&[]).is_err());
    assert!(TreeNamespace::decode(&[2]).is_err());
    assert!(TreeNamespace::decode(&[0, 0]).is_err());
    assert!(TreeNamespace::decode(
        &TreeNamespace::CollectionShard(collection, K_PROVISIONAL).encode()
    )
    .is_err());
}

#[test]
fn staged_tree_and_branch_keys_follow_ckb_reversed_byte_order() {
    let mut natural_high = [0_u8; 32];
    natural_high[0] = 2;
    let mut ckb_high = [0_u8; 32];
    ckb_high[31] = 1;
    let natural_high = B256::from(natural_high);
    let ckb_high = B256::from(ckb_high);

    assert!(TreeKey::try_from(natural_high).unwrap() < TreeKey::try_from(ckb_high).unwrap());
    assert!(BranchKey::new(7, natural_high).unwrap() < BranchKey::new(7, ckb_high).unwrap());
    assert!(BranchKey::new(6, ckb_high).unwrap() < BranchKey::new(7, natural_high).unwrap());
}

#[test]
fn marker_and_environment_identity_have_deterministic_exact_codecs() {
    let value = marker(7);
    let encoded = value.encode();
    assert_eq!(encoded.len(), 140);
    assert_eq!(FinalizedMarker::decode(&encoded).unwrap(), value);
    assert!(FinalizedMarker::decode(&encoded[..139]).is_err());

    let environment = identity();
    let encoded = environment.encode().unwrap();
    assert_eq!(EnvironmentIdentity::decode(&encoded).unwrap(), environment);
    let mut trailing = encoded;
    trailing.push(1);
    assert!(matches!(
        EnvironmentIdentity::decode(&trailing),
        Err(PersistenceError::TrailingBytes { .. })
    ));
}
