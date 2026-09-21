use super::*;

#[derive(Clone, Copy)]
enum BodyVersion {
    Original,
    Updated,
}

#[derive(Clone, Copy)]
enum MatrixMutation {
    Mint(BodyVersion),
    Update(BodyVersion),
    Delete,
}

fn exercise_transition_sequence(
    original: &FixtureBody,
    updated: &FixtureBody,
    sequence: &[(MatrixMutation, bool)],
) {
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);
    let mut expected: Option<BodyVersion> = None;
    let mut last_capability: Option<VerifiedBody> = None;
    let mut successful_operations = 0;

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        for (operation, should_succeed) in sequence {
            let selected = |version: BodyVersion| match version {
                BodyVersion::Original => original,
                BodyVersion::Updated => updated,
            };
            let result = match operation {
                MatrixMutation::Mint(version) => {
                    mint(storage.clone(), &scope, selected(*version).input())
                }
                MatrixMutation::Update(version) => {
                    let current = read(storage.clone(), &scope, &parent, original.entity_ref())
                        .unwrap()
                        .or_else(|| last_capability.clone())
                        .expect("update scenario retains a prior value capability");
                    last_capability = Some(current.clone());
                    update(storage.clone(), &scope, current, selected(*version).input())
                }
                MatrixMutation::Delete => {
                    let current = read(storage.clone(), &scope, &parent, original.entity_ref())
                        .unwrap()
                        .or_else(|| last_capability.clone())
                        .expect("delete scenario retains a prior value capability");
                    last_capability = Some(current.clone());
                    delete(storage.clone(), &scope, current)
                }
            };

            if *should_succeed {
                result.unwrap();
                successful_operations += 1;
                expected = match operation {
                    MatrixMutation::Mint(version) | MatrixMutation::Update(version) => {
                        Some(*version)
                    }
                    MatrixMutation::Delete => None,
                };
            } else {
                assert!(matches!(result, Err(PrecompileError::Revert(_))));
            }

            let current = read(storage.clone(), &scope, &parent, original.entity_ref()).unwrap();
            match (expected, current) {
                (None, None) => {}
                (Some(version), Some(verified)) => selected(version).assert_verified(&verified),
                _ => panic!("transition produced the wrong observable existence state"),
            }
        }

        let schema = CompressedEntitiesSchema::new(storage.clone());
        assert_eq!(schema.touched.len().unwrap(), 1);
        assert_eq!(
            schema.touched_index_deltas.len().unwrap(),
            original.expected_index_touches()
        );
        end_block(storage, &scope).unwrap();
    });

    assert_eq!(provider.get_ordered_events().len(), successful_operations);
    assert!(provider
        .get_ordered_events()
        .iter()
        .all(|event| event.address == original.emitter()));
}

#[test]
fn same_block_transition_matrix_is_overlay_first_and_single_touch() {
    let owner = address!("1000000000000000000000000000000000000001");
    let id = entity(7, 1);
    let first = tribute(id, owner, 100);
    let second = tribute(id, owner, 200);
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&first)).unwrap();
        assert_eq!(parent.get_calls.get(), 0);
        let minted = read(storage.clone(), &scope, &parent, EntityRef::Tribute(id))
            .unwrap()
            .unwrap();
        assert_eq!(minted.payload().as_tribute().unwrap(), &first);
        assert!(matches!(
            mint(storage.clone(), &scope, BodyInput::Tribute(&first)),
            Err(PrecompileError::Revert(_))
        ));

        update(storage.clone(), &scope, minted, BodyInput::Tribute(&second)).unwrap();
        let updated = read(storage.clone(), &scope, &parent, EntityRef::Tribute(id))
            .unwrap()
            .unwrap();
        assert_eq!(updated.payload().as_tribute().unwrap(), &second);
        delete(storage.clone(), &scope, updated).unwrap();
        assert!(
            read(storage.clone(), &scope, &parent, EntityRef::Tribute(id))
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            delete(
                storage.clone(),
                &scope,
                // A capability can only be acquired while present, so use a
                // mint/read cycle to test the absent rejection below.
                {
                    mint(storage.clone(), &scope, BodyInput::Tribute(&first)).unwrap();
                    let cap = read(storage.clone(), &scope, &parent, EntityRef::Tribute(id))
                        .unwrap()
                        .unwrap();
                    delete(storage.clone(), &scope, cap.clone()).unwrap();
                    cap
                }
            ),
            Err(PrecompileError::Revert(_))
        ));
        mint(storage.clone(), &scope, BodyInput::Tribute(&second)).unwrap();

        let schema = CompressedEntitiesSchema::new(storage);
        assert_eq!(schema.touched.len().unwrap(), 1);
        // Two memberships, each touched once despite the repeated sequence.
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 2);
        assert_eq!(parent.get_calls.get(), 0);
    });
}

#[test]
fn completed_seal_projection_is_unavailable_before_end_and_exact_after_end() {
    let day = WorldwideDay::new(2026_0707);
    let body = tribute(
        entity(day.value(), 0x71),
        address!("7100000000000000000000000000000000000071"),
        100,
    );
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&body)).unwrap();

        assert!(scope.completed_sealed_root().is_err());
        assert!(scope
            .completed_partition_root(PartitionRef::TributeWwd(day))
            .is_err());

        let preview = crate::preview_end_block(storage.clone(), &scope).unwrap();
        assert_eq!(scope.provisional_sealed_root().unwrap(), preview.new_root);
        let provisional_collection = scope
            .provisional_partition_root(PartitionRef::TributeWwd(day))
            .unwrap();
        assert_eq!(
            provisional_collection.partition(),
            PartitionRef::TributeWwd(day)
        );
        assert!(!provisional_collection.root().is_zero());

        let seal = end_block(storage, &scope).unwrap();
        assert_eq!(seal, preview);
        assert_eq!(scope.completed_sealed_root().unwrap(), seal.new_root);
        let collection = scope
            .completed_partition_root(PartitionRef::TributeWwd(day))
            .unwrap();
        assert_eq!(collection.partition(), PartitionRef::TributeWwd(day));
        assert!(!collection.root().is_zero());
        assert_eq!(
            collection,
            scope
                .sealed_collection_root(&seal, PartitionRef::TributeWwd(day))
                .unwrap()
        );
        assert_eq!(collection, provisional_collection);
        let payload = crate::encode_tribute_v1(&body).unwrap();
        let commitment = crate::body_commitment(
            crate::ACTIVE_COMMITMENT_SCHEME,
            crate::BODY_SCHEMA_V1,
            body.tribute_id,
            &payload,
        )
        .unwrap();
        assert_eq!(
            crate::tribute_partition_root_from_leaves(day, [(body.tribute_id, commitment)],)
                .unwrap(),
            collection.root(),
        );
    });
}

#[test]
fn same_leaf_aba_capability_remains_value_valid() {
    let owner = address!("2000000000000000000000000000000000000002");
    let body = tribute(entity(8, 2), owner, 100);
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&body)).unwrap();
        let old = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(body.tribute_id),
        )
        .unwrap()
        .unwrap();
        update(
            storage.clone(),
            &scope,
            old.clone(),
            BodyInput::Tribute(&body),
        )
        .unwrap();
        // The current authenticated value is identical, so the earlier value
        // capability deliberately remains valid.
        delete(storage, &scope, old).unwrap();
    });
}

#[test]
fn nod_item_and_bucket_follow_the_same_closed_transition_lifecycle() {
    let owner = address!("2100000000000000000000000000000000000002");
    let mut item = nod_item(entity(8, 21), owner);
    let mut bucket = NodBucketBodyV1 {
        settled_nods: 0,
        bucket_key: B256::repeat_byte(22),
        worldwide_day: WorldwideDay::new(8),
        floor_price_minor: U256::from(10),
        is_qualified: false,
        entry_price_minor: U256::from(11),
        reference_currency: 840,
    };
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::NodItem(&item)).unwrap();
        mint(storage.clone(), &scope, BodyInput::NodBucket(&bucket)).unwrap();
        let old_item = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::NodItem(item.nod_id),
        )
        .unwrap()
        .unwrap();
        let old_bucket = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::NodBucket(bucket.entity_id()),
        )
        .unwrap()
        .unwrap();
        item.gratis_load_minor = U256::from(99);
        bucket.is_qualified = true;
        update(storage.clone(), &scope, old_item, BodyInput::NodItem(&item)).unwrap();
        update(
            storage.clone(),
            &scope,
            old_bucket,
            BodyInput::NodBucket(&bucket),
        )
        .unwrap();
        let item_cap = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::NodItem(item.nod_id),
        )
        .unwrap()
        .unwrap();
        let bucket_cap = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::NodBucket(bucket.entity_id()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(item_cap.payload().as_nod_item().unwrap(), &item);
        assert_eq!(bucket_cap.payload().as_nod_bucket().unwrap(), &bucket);
        delete(storage.clone(), &scope, item_cap).unwrap();
        delete(storage.clone(), &scope, bucket_cap).unwrap();
        assert!(read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::NodItem(item.nod_id)
        )
        .unwrap()
        .is_none());
        assert!(read(
            storage,
            &scope,
            &parent,
            EntityRef::NodBucket(bucket.entity_id())
        )
        .unwrap()
        .is_none());
    });
    assert!(provider
        .get_ordered_events()
        .iter()
        .all(|log| log.address == NOD_ADDRESS));
    assert_eq!(provider.get_ordered_events().len(), 6);
}

#[test]
fn every_typed_collection_obeys_the_complete_same_block_transition_matrix() {
    let owner = address!("2150000000000000000000000000000000000002");
    let tribute_original = tribute(entity(8, 0x31), owner, 100);
    let tribute_updated = tribute(tribute_original.tribute_id, owner, 200);

    let nod_original = nod_item(entity(8, 0x32), owner);
    let mut nod_updated = nod_original.clone();
    nod_updated.gratis_load_minor = U256::from(99);

    let bucket_original = NodBucketBodyV1 {
        settled_nods: 0,
        bucket_key: B256::repeat_byte(0x33),
        worldwide_day: WorldwideDay::new(8),
        floor_price_minor: U256::from(10),
        is_qualified: false,
        entry_price_minor: U256::from(11),
        reference_currency: 840,
    };
    let mut bucket_updated = bucket_original.clone();
    bucket_updated.is_qualified = true;

    let fixtures = [
        (
            FixtureBody::Tribute(tribute_original),
            FixtureBody::Tribute(tribute_updated),
        ),
        (
            FixtureBody::NodItem(nod_original),
            FixtureBody::NodItem(nod_updated),
        ),
        (
            FixtureBody::NodBucket(bucket_original),
            FixtureBody::NodBucket(bucket_updated),
        ),
    ];

    for (original, updated) in &fixtures {
        exercise_transition_sequence(
            original,
            updated,
            &[
                (MatrixMutation::Mint(BodyVersion::Original), true),
                (MatrixMutation::Mint(BodyVersion::Original), false),
            ],
        );
        exercise_transition_sequence(
            original,
            updated,
            &[
                (MatrixMutation::Mint(BodyVersion::Original), true),
                (MatrixMutation::Update(BodyVersion::Updated), true),
                (MatrixMutation::Update(BodyVersion::Original), true),
                (MatrixMutation::Delete, true),
            ],
        );
        exercise_transition_sequence(
            original,
            updated,
            &[
                (MatrixMutation::Mint(BodyVersion::Original), true),
                (MatrixMutation::Delete, true),
                (MatrixMutation::Update(BodyVersion::Updated), false),
                (MatrixMutation::Delete, false),
                (MatrixMutation::Mint(BodyVersion::Updated), true),
            ],
        );
        exercise_transition_sequence(
            original,
            updated,
            &[
                (MatrixMutation::Mint(BodyVersion::Original), true),
                (MatrixMutation::Update(BodyVersion::Original), true),
            ],
        );
    }
}

#[test]
fn stale_or_wrong_identity_capability_reverts_without_mutation() {
    let owner = address!("2200000000000000000000000000000000000002");
    let first = tribute(entity(8, 23), owner, 100);
    let second = tribute(first.tribute_id, owner, 200);
    let other = tribute(entity(8, 24), owner, 300);
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&first)).unwrap();
        let stale = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(first.tribute_id),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            update(
                storage.clone(),
                &scope,
                stale.clone(),
                BodyInput::Tribute(&other)
            ),
            Err(PrecompileError::Revert(_))
        ));
        update(
            storage.clone(),
            &scope,
            stale.clone(),
            BodyInput::Tribute(&second),
        )
        .unwrap();
        assert!(matches!(
            delete(storage.clone(), &scope, stale),
            Err(PrecompileError::Revert(_))
        ));
        let current = read(
            storage,
            &scope,
            &parent,
            EntityRef::Tribute(first.tribute_id),
        )
        .unwrap()
        .unwrap();
        assert_eq!(current.payload().as_tribute().unwrap(), &second);
    });
}

#[test]
fn canonical_events_use_domain_emitters_and_survive_as_ordered_operations() {
    let owner = address!("3000000000000000000000000000000000000003");
    let id = entity(9, 3);
    let first = tribute(id, owner, 100);
    let second = tribute(id, owner, 200);
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&first)).unwrap();
        let cap = read(storage.clone(), &scope, &parent, EntityRef::Tribute(id))
            .unwrap()
            .unwrap();
        update(storage.clone(), &scope, cap, BodyInput::Tribute(&second)).unwrap();
        let cap = read(storage.clone(), &scope, &parent, EntityRef::Tribute(id))
            .unwrap()
            .unwrap();
        delete(storage, &scope, cap).unwrap();
    });

    let logs = provider.get_ordered_events();
    assert_eq!(logs.len(), 3);
    assert!(logs.iter().all(|log| log.address == TRIBUTE_ADDRESS));
    let mint_event = TributeBodyStored::decode_log_data(&logs[0].data).unwrap();
    let update_event = TributeBodyStored::decode_log_data(&logs[1].data).unwrap();
    let delete_event = TributeBodyDeleted::decode_log_data(&logs[2].data).unwrap();
    assert_eq!(mint_event.tributeId, id.to_u256());
    assert_eq!(mint_event.previousCommitment, B256::ZERO);
    assert_eq!(
        mint_event.canonicalPayload,
        encode_tribute_v1(&first).unwrap()
    );
    assert_eq!(update_event.previousCommitment, mint_event.newCommitment);
    assert_ne!(update_event.newCommitment, mint_event.newCommitment);
    assert_eq!(delete_event.previousCommitment, update_event.newCommitment);

    let nod = nod_item(entity(10, 4), owner);
    let nod_scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        // Finish the previous scope first; cleanup emits no event.
        end_block(storage.clone(), &scope).unwrap();
        begin_block(storage.clone(), &nod_scope).unwrap();
        mint(storage, &nod_scope, BodyInput::NodItem(&nod)).unwrap();
    });
    let last = provider.get_ordered_events().last().unwrap();
    assert_eq!(last.address, NOD_ADDRESS);
    assert_eq!(last.data.topics()[0], NodBodyStored::SIGNATURE_HASH);
}

#[test]
fn first_touch_lists_preserve_the_exact_deterministic_operation_order() {
    let owner = address!("7f00000000000000000000000000000000000007");
    let first = tribute(entity(24, 1), owner, 10);
    let second = tribute(entity(24, 2), owner, 20);
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&second)).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&first)).unwrap();

        let schema = CompressedEntitiesSchema::new(storage.clone());
        assert_eq!(schema.touched.len().unwrap(), 2);
        assert_eq!(
            schema.touched.get(0).unwrap(),
            Some(body_locator(Collection::Tribute, second.tribute_id).unwrap())
        );
        assert_eq!(
            schema.touched.get(1).unwrap(),
            Some(body_locator(Collection::Tribute, first.tribute_id).unwrap())
        );

        let expected_indexes = [
            IndexRecord::owner(IndexKind::TributeByOwner, owner, second.tribute_id).key(),
            IndexRecord::day(second.worldwide_day, second.tribute_id).key(),
            IndexRecord::owner(IndexKind::TributeByOwner, owner, first.tribute_id).key(),
            IndexRecord::day(first.worldwide_day, first.tribute_id).key(),
        ];
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 4);
        for (index, expected) in expected_indexes.into_iter().enumerate() {
            assert_eq!(
                schema
                    .touched_index_deltas
                    .get(u32::try_from(index).unwrap())
                    .unwrap(),
                Some(expected)
            );
        }

        end_block(storage, &scope).unwrap();
    });
}
