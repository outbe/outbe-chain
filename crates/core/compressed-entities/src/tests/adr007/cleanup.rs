use super::*;

#[test]
fn cleanup_zeroes_overlay_and_phase_rejects_post_end_access() {
    let owner = address!("8000000000000000000000000000000000000008");
    let body = tribute(entity(14, 8), owner, 100);
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);
    let mut locator = B256::ZERO;

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&body)).unwrap();
        let capability = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(body.tribute_id),
        )
        .unwrap()
        .unwrap();
        locator = body_locator(Collection::Tribute, body.tribute_id).unwrap();
        let seal = end_block(storage.clone(), &scope).unwrap();

        assert_eq!(State::new(storage.clone()).root().unwrap(), seal.new_root);
        assert_ne!(seal.new_root, seal.parent_root);
        let schema = CompressedEntitiesSchema::new(storage.clone());
        assert_eq!(schema.touched.len().unwrap(), 0);
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 0);
        assert!(schema.pending_word.read(&locator).unwrap().is_zero());
        assert!(schema.pending_body.get_bytes(&locator).is_empty().unwrap());
        assert_eq!(
            schema.body_identity_collection.read(&locator).unwrap(),
            0,
            "cleanup must clear the identity presence marker"
        );
        assert!(matches!(
            read(
                storage.clone(),
                &scope,
                &parent,
                EntityRef::Tribute(body.tribute_id)
            ),
            Err(PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            list(
                storage.clone(),
                &scope,
                &parent,
                QueryRef::TributeByOwner(owner),
                IdPageRequest {
                    after: None,
                    limit: 1,
                },
            ),
            Err(PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            mint(storage.clone(), &scope, BodyInput::Tribute(&body)),
            Err(PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            update(
                storage.clone(),
                &scope,
                capability.clone(),
                BodyInput::Tribute(&body),
            ),
            Err(PrecompileError::Fatal(_))
        ));
        assert!(matches!(
            delete(storage, &scope, capability),
            Err(PrecompileError::Fatal(_))
        ));
    });

    let pending_slot = locator.mapping_slot(U256::from(4));
    assert_eq!(
        provider
            .storage
            .get(&(COMPRESSED_ENTITIES_ADDRESS, pending_slot))
            .copied()
            .unwrap_or_default(),
        U256::ZERO
    );
}

#[test]
fn begin_block_rejects_a_dirty_prior_overlay_without_repairing_it() {
    let owner = address!("8100000000000000000000000000000000000008");
    let body = tribute(entity(14, 81), owner, 100);
    let first_scope = ExecutionScope::new();
    let second_scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &first_scope).unwrap();
        mint(storage.clone(), &first_scope, BodyInput::Tribute(&body)).unwrap();
        assert!(matches!(
            begin_block(storage.clone(), &second_scope),
            Err(PrecompileError::Fatal(_))
        ));
        let schema = CompressedEntitiesSchema::new(storage);
        assert_eq!(schema.touched.len().unwrap(), 1);
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 2);
    });
}

fn populate_cleanup_fixture(
    provider: &mut HashMapStorageProvider,
    scope: &ExecutionScope,
    fixtures: &[FixtureBody],
) {
    StorageHandle::enter(provider, |storage| {
        begin_block(storage.clone(), scope).unwrap();
        for fixture in fixtures {
            mint(storage.clone(), scope, fixture.input()).unwrap();
        }
    });
}

#[test]
fn every_cleanup_write_boundary_rolls_back_the_complete_end_block_cleanup() {
    let owner = address!("8d0000000000000000000000000000000000008d");
    let fixtures = [
        FixtureBody::Tribute(tribute(entity(14, 0x8d), owner, 100)),
        FixtureBody::NodItem(nod_item(entity(14, 0x8e), owner)),
        FixtureBody::NodBucket(NodBucketBodyV1 {
            bucket_key: B256::repeat_byte(0x8f),
            worldwide_day: WorldwideDay::new(14),
            floor_price_minor: U256::from(10),
            is_qualified: false,
            entry_price_minor: U256::from(11),
            reference_currency: 840,
        }),
    ];

    let mut baseline = HashMapStorageProvider::new(1);
    let baseline_scope = ExecutionScope::new();
    populate_cleanup_fixture(&mut baseline, &baseline_scope, &fixtures);
    let baseline_preview = StorageHandle::enter(&mut baseline, |storage| {
        crate::preview_end_block(storage, &baseline_scope).unwrap()
    });
    baseline.clear_mutation_failure();
    let baseline_seal = StorageHandle::enter(&mut baseline, |storage| {
        end_block(storage, &baseline_scope).unwrap()
    });
    assert_eq!(baseline_seal, baseline_preview);
    let cleanup_operations = baseline.clear_mutation_failure();
    assert!(cleanup_operations > 0);

    for position in [FaultPosition::Before, FaultPosition::After] {
        for failure_at in 0..cleanup_operations {
            let mut provider = HashMapStorageProvider::new(1);
            let scope = ExecutionScope::new();
            populate_cleanup_fixture(&mut provider, &scope, &fixtures);
            let preview = StorageHandle::enter(&mut provider, |storage| {
                crate::preview_end_block(storage, &scope).unwrap()
            });
            let storage_before = provider.storage.clone();
            let events_before = provider.get_ordered_events().to_vec();
            provider.clear_mutation_failure();
            arm_fault(&mut provider, position, failure_at);

            let error = StorageHandle::enter(&mut provider, |storage| {
                end_block(storage, &scope).unwrap_err()
            });
            assert!(matches!(error, PrecompileError::Storage(_)));
            provider.clear_mutation_failure();
            assert_eq!(&provider.storage, &storage_before);
            assert_eq!(provider.get_ordered_events(), events_before);

            StorageHandle::enter(&mut provider, |storage| {
                let schema = CompressedEntitiesSchema::new(storage);
                assert_eq!(schema.touched.len().unwrap(), 3);
                assert_eq!(schema.touched_index_deltas.len().unwrap(), 4);
            });
            let retry =
                StorageHandle::enter(&mut provider, |storage| end_block(storage, &scope).unwrap());
            assert_eq!(retry, preview);
        }
    }
}

#[test]
fn maximum_v1_body_footprint_and_storage_tail_cleanup_are_exact() {
    let day = WorldwideDay::new(u32::MAX);
    let id = WwdEntityId::from_day_and_digest(day, [0xff; 32]);
    let maximum = NodItemBodyV1 {
        nod_id: id,
        owner: Address::repeat_byte(0xff),
        gratis_load_minor: U256::MAX,
        worldwide_day: day,
        league_id: u16::MAX,
        floor_price_minor: U256::MAX,
        bucket_key: B256::repeat_byte(0xff),
        issuance_currency: u16::MAX,
        reference_currency: u16::MAX,
        issued_at: u64::MAX,
    };
    let maximum_stored = StoredBody::new_v1(encode_nod_item_v1(&maximum).unwrap())
        .unwrap()
        .encode();
    assert_eq!(maximum_stored.len(), MAX_STORED_BODY_BYTES_V1);

    // The reserve only covers the tail it prepays for, so the Nod item has to
    // stay the largest of the three v1 bodies.
    let widest_tribute = TributeBodyV1 {
        tribute_id: id,
        owner: Address::repeat_byte(0xff),
        worldwide_day: day,
        issuance_amount_minor: U256::MAX,
        issuance_currency: u16::MAX,
        nominal_amount_minor: U256::MAX,
        reference_currency: u16::MAX,
        tribute_price_minor: U256::MAX,
        exclude_from_intex_issuance: true,
    };
    assert!(stored_tribute(&widest_tribute).encode().len() <= MAX_STORED_BODY_BYTES_V1);
    assert!(
        StoredBody::new_v1(encode_nod_bucket_v1(&widest_bucket(day)).unwrap())
            .unwrap()
            .encode()
            .len()
            <= MAX_STORED_BODY_BYTES_V1
    );

    let scope = ExecutionScope::new();
    let parent = MemoryParent::default();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::NodItem(&maximum)).unwrap();
        let locator = body_locator(Collection::NodItem, id).unwrap();
        let base = locator.mapping_slot(U256::from(5));
        let data_start = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
        let maximum_slots = maximum_stored.len().div_ceil(32);
        assert_eq!(
            CompressedEntitiesSchema::new(storage.clone())
                .pending_body
                .get_bytes(&locator)
                .read()
                .unwrap(),
            maximum_stored
        );
        assert!(!storage
            .sload(
                COMPRESSED_ENTITIES_ADDRESS,
                data_start + U256::from(maximum_slots - 1),
            )
            .unwrap()
            .is_zero());

        let cap = read(storage.clone(), &scope, &parent, EntityRef::NodItem(id))
            .unwrap()
            .unwrap();
        delete(storage.clone(), &scope, cap).unwrap();
        for slot in 0..maximum_slots {
            assert!(storage
                .sload(COMPRESSED_ENTITIES_ADDRESS, data_start + U256::from(slot))
                .unwrap()
                .is_zero());
        }

        mint(storage.clone(), &scope, BodyInput::NodItem(&maximum)).unwrap();
        end_block(storage.clone(), &scope).unwrap();
        assert!(storage
            .sload(COMPRESSED_ENTITIES_ADDRESS, base)
            .unwrap()
            .is_zero());
        for slot in 0..maximum_slots {
            assert!(storage
                .sload(COMPRESSED_ENTITIES_ADDRESS, data_start + U256::from(slot))
                .unwrap()
                .is_zero());
        }
    });
}

fn widest_bucket(day: WorldwideDay) -> NodBucketBodyV1 {
    NodBucketBodyV1 {
        bucket_key: B256::repeat_byte(0xff),
        worldwide_day: day,
        floor_price_minor: U256::MAX,
        is_qualified: true,
        entry_price_minor: U256::MAX,
        reference_currency: u16::MAX,
    }
}

/// Shrinking a body must clear truncated bytes, including padding in its final word.
#[test]
fn shrinking_a_body_zeroes_truncated_bytes_and_storage_padding() {
    let day = WorldwideDay::new(u32::MAX);
    let id = WwdEntityId::from_day_and_digest(day, [0xff; 32]);
    let widest = NodItemBodyV1 {
        league_id: u16::MAX,
        issuance_currency: u16::MAX,
        reference_currency: u16::MAX,
        issued_at: u64::MAX,
        ..nod_item(id, Address::repeat_byte(0xff))
    };
    let narrowest = NodItemBodyV1 {
        league_id: 0,
        issuance_currency: 0,
        reference_currency: 0,
        issued_at: 0,
        ..widest.clone()
    };
    let stored = |body: &NodItemBodyV1| {
        StoredBody::new_v1(encode_nod_item_v1(body).unwrap())
            .unwrap()
            .encode()
    };
    let widest_bytes = stored(&widest);
    let narrowest_bytes = stored(&narrowest);
    assert!(narrowest_bytes.len() < widest_bytes.len());
    let widest_slots = widest_bytes.len().div_ceil(32);

    let scope = ExecutionScope::new();
    let parent = MemoryParent::default();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::NodItem(&widest)).unwrap();
        let locator = body_locator(Collection::NodItem, id).unwrap();
        let base = locator.mapping_slot(U256::from(5));
        let data_start = U256::from_be_bytes(keccak256(base.to_be_bytes::<32>()).0);
        let cap = read(storage.clone(), &scope, &parent, EntityRef::NodItem(id))
            .unwrap()
            .unwrap();
        update(storage.clone(), &scope, cap, BodyInput::NodItem(&narrowest)).unwrap();
        let mut actual = Vec::new();
        for slot in 0..widest_slots {
            actual.extend_from_slice(
                &storage
                    .sload(COMPRESSED_ENTITIES_ADDRESS, data_start + U256::from(slot))
                    .unwrap()
                    .to_be_bytes::<32>(),
            );
        }
        assert_eq!(&actual[..narrowest_bytes.len()], narrowest_bytes);
        assert!(actual[narrowest_bytes.len()..]
            .iter()
            .all(|byte| *byte == 0));
    });
}
