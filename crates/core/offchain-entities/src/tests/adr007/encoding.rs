use super::*;

#[test]
fn storage_layout_uses_exact_slots_zero_through_thirteen() {
    let owner = address!("9000000000000000000000000000000000000009");
    let body = tribute(entity(15, 9), owner, 100);
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);
    let mut locator = B256::ZERO;

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage, &scope, BodyInput::Tribute(&body)).unwrap();
        locator = body_locator(Collection::Tribute, body.tribute_id).unwrap();
    });

    let pending_slot = locator.mapping_slot(U256::from(4));
    let identity_slot = locator.mapping_slot(U256::from(10));
    let identity_collection_slot = locator.mapping_slot(U256::from(13));
    assert_eq!(
        provider.storage[&(COMPRESSED_ENTITIES_ADDRESS, U256::ZERO)],
        U256::from(4)
    );
    assert_eq!(
        provider
            .storage
            .get(&(COMPRESSED_ENTITIES_ADDRESS, U256::from(1)))
            .copied()
            .unwrap_or_default(),
        U256::from_be_slice(crate::sealed_root(B256::ZERO).unwrap().as_slice())
    );
    for reserved in [U256::from(2), U256::from(3)] {
        assert_eq!(
            provider
                .storage
                .get(&(COMPRESSED_ENTITIES_ADDRESS, reserved))
                .copied()
                .unwrap_or_default(),
            U256::ZERO
        );
    }
    assert_eq!(
        provider.storage[&(COMPRESSED_ENTITIES_ADDRESS, pending_slot)],
        tribute_commitment(&body).to_u256()
    );
    assert_eq!(
        provider.storage[&(COMPRESSED_ENTITIES_ADDRESS, U256::from(6))],
        U256::from(1)
    );
    // One word each: the identity at slot 10 and its collection marker at 13.
    assert_eq!(
        provider.storage[&(COMPRESSED_ENTITIES_ADDRESS, identity_slot)],
        body.tribute_id.to_u256()
    );
    assert_eq!(
        provider.storage[&(COMPRESSED_ENTITIES_ADDRESS, identity_collection_slot)],
        U256::from(Collection::Tribute.id())
    );
    // No base slot exists beyond 13.
    assert!(!provider
        .storage
        .contains_key(&(COMPRESSED_ENTITIES_ADDRESS, U256::from(14))));
}

#[test]
fn body_codecs_cover_all_three_closed_variants() {
    let owner = address!("a00000000000000000000000000000000000000a");
    let item = nod_item(entity(16, 10), owner);
    let bucket = NodBucketBodyV1 {
        settled_nods: 0,
        bucket_key: B256::repeat_byte(11),
        worldwide_day: WorldwideDay::new(16),
        floor_price_minor: U256::from(12),
        entry_price_minor: U256::from(14),
        reference_currency: 840,
    };
    assert!(!encode_nod_item_v1(&item).unwrap().is_empty());
    assert!(!encode_nod_bucket_v1(&bucket).unwrap().is_empty());
    // Pin the central event signatures independently from their Rust types.
    assert_eq!(
        TributeBodyStored::SIGNATURE_HASH,
        keccak256("TributeBodyStored(uint256,uint32,uint32,bytes32,bytes32,bytes)")
    );
    assert_eq!(
        NodBodyStored::SIGNATURE_HASH,
        keccak256("NodBodyStored(uint256,uint32,uint32,bytes32,bytes32,bytes)")
    );
}

#[test]
fn exact_overlay_locator_and_record_vectors_are_protocol_pinned() {
    let id = WwdEntityId::from_day_and_digest(WorldwideDay::new(42), [0x11; 32]);
    let cases = [
        (
            Collection::Tribute,
            b256!("efe7430ecc84638c827107a7b8c81e65149beafaaa94bf3ccdd5a7ce047a7e27"),
        ),
        (
            Collection::NodItem,
            b256!("d6a30f530d7d202867408fcdb9dd825155f914cd830a3b5db6f599e710ef27ba"),
        ),
        (
            Collection::NodBucket,
            b256!("c4f00d37b7f206de8ac85d35728a73ecba6856f54bd9ead4fd2a42e80562f32f"),
        ),
    ];

    for (collection, expected_locator) in cases {
        assert_eq!(body_locator(collection, id).unwrap(), expected_locator);
    }
}

#[test]
fn exact_index_record_key_and_status_vectors_are_protocol_pinned() {
    let id = WwdEntityId::from_day_and_digest(WorldwideDay::new(42), [0x11; 32]);
    let owner = Address::repeat_byte(0x22);
    let cases = [
        (
            IndexRecord::owner(IndexKind::TributeByOwner, owner, id),
            concat!(
                "010114",
                "2222222222222222222222222222222222222222",
                "0000002a",
                "11111111111111111111111111111111111111111111111111111111"
            ),
            b256!("c3acc7bd8c73c96af18b80b2415d643c6d68e64573cc620ed0e086b13b79e165"),
        ),
        (
            IndexRecord::day(WorldwideDay::new(42), id),
            concat!(
                "0102040000002a0000002a",
                "11111111111111111111111111111111111111111111111111111111"
            ),
            b256!("b31e3d821bf8623570743703cba38e51b5f66dbed443719bc06b95f7405148bb"),
        ),
        (
            IndexRecord::owner(IndexKind::NodByOwner, owner, id),
            concat!(
                "010314",
                "2222222222222222222222222222222222222222",
                "0000002a",
                "11111111111111111111111111111111111111111111111111111111"
            ),
            b256!("d74fd5e8d1103a315a0503320b48ae5d88cd7a48ce67781bd9c8b6bbd2868ae6"),
        ),
        (
            IndexRecord::nod_all(id),
            concat!(
                "0104000000002a",
                "11111111111111111111111111111111111111111111111111111111"
            ),
            b256!("638c71f7b03c234465ab777494472e896c80f87cf31e7afb7a8b2b441ccd661f"),
        ),
    ];

    for (record, expected_hex, expected_key) in cases {
        let expected = hex::decode(expected_hex).unwrap();
        assert_eq!(record.encode(), expected);
        assert_eq!(IndexRecord::decode(&expected).unwrap(), record);
        assert_eq!(record.key(), expected_key);
    }

    let commitment = tribute_commitment(&tribute(id, owner, 1));
    assert_eq!(PendingWord::Untouched.encode(), U256::ZERO);
    assert_eq!(PendingWord::Set(commitment).encode(), commitment.to_u256());
    assert_eq!(PendingWord::Deleted.encode(), U256::MAX);
    for (word, expected) in [
        (U256::ZERO, PendingWord::Untouched),
        (commitment.to_u256(), PendingWord::Set(commitment)),
        (U256::MAX, PendingWord::Deleted),
    ] {
        assert_eq!(PendingWord::decode(word).unwrap(), expected);
    }
    for (status, word) in [
        (DeltaStatus::NeverTouched, 0_u64),
        (DeltaStatus::Added, 1),
        (DeltaStatus::Removed, 2),
        (DeltaStatus::NoChangeTouched, 3),
    ] {
        assert_eq!(status.encode(), U256::from(word));
        assert_eq!(DeltaStatus::decode(U256::from(word)).unwrap(), status);
    }
}

#[test]
fn overlay_wire_decoders_reject_every_noncanonical_boundary_class() {
    let id = WwdEntityId::from_day_and_digest(WorldwideDay::new(42), [0x11; 32]);
    let owner = Address::repeat_byte(0x22);
    let modulus = U256::from_be_bytes::<32>(
        hex::decode("30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001")
            .unwrap()
            .try_into()
            .unwrap(),
    );
    for invalid in [modulus, modulus + U256::from(1), U256::MAX - U256::from(1)] {
        assert!(matches!(
            PendingWord::decode(invalid),
            Err(PrecompileError::Fatal(_))
        ));
    }
    for invalid in [U256::from(4), U256::from(u64::MAX), U256::MAX] {
        assert!(matches!(
            DeltaStatus::decode(invalid),
            Err(PrecompileError::Fatal(_))
        ));
    }

    for invalid in [0_u8, 4, u8::MAX] {
        assert!(
            matches!(Collection::from_id(invalid), Err(PrecompileError::Fatal(_))),
            "collection id {invalid} must not decode"
        );
    }

    let valid_index = IndexRecord::owner(IndexKind::TributeByOwner, owner, id).encode();
    for invalid in [
        valid_index[..38].to_vec(),
        {
            let mut value = valid_index.clone();
            value[0] = 2;
            value
        },
        {
            let mut value = valid_index.clone();
            value[1] = 9;
            value
        },
        {
            let mut value = valid_index.clone();
            value[2] = 19;
            value
        },
        {
            let mut value = valid_index.clone();
            value.push(0);
            value
        },
    ] {
        assert!(matches!(
            IndexRecord::decode(&invalid),
            Err(PrecompileError::Fatal(_))
        ));
    }
}
