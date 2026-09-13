use super::*;

#[test]
fn gas_reserve_is_first_touch_only_and_oog_rolls_back_before_overlay_write() {
    let owner = address!("8200000000000000000000000000000000000008");
    let body = tribute(entity(14, 82), owner, 100);
    let parent = MemoryParent::default();

    let mut provider = HashMapStorageProvider::new(1);
    provider.set_gas_limit(FIRST_TRIBUTE_CLEANUP_GAS + 100_000);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        let first_touch = scope.explicit_gas_checkpoint();
        mint(storage.clone(), &scope, BodyInput::Tribute(&body)).unwrap();
        assert_eq!(storage.gas_used().unwrap(), FIRST_TRIBUTE_CLEANUP_GAS);
        assert_eq!(
            scope.explicit_gas_since(first_touch).unwrap(),
            FIRST_TRIBUTE_CLEANUP_GAS
        );
        let cap = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(body.tribute_id),
        )
        .unwrap()
        .unwrap();
        let before_repeat = storage.gas_used().unwrap();
        let repeat_touch = scope.explicit_gas_checkpoint();
        update(storage.clone(), &scope, cap, BodyInput::Tribute(&body)).unwrap();
        assert_eq!(
            storage.gas_used().unwrap(),
            before_repeat,
            "repeat body/index touches must not reserve cleanup twice"
        );
        assert_eq!(scope.explicit_gas_since(repeat_touch).unwrap(), 0);
    });

    let mut body_oog = HashMapStorageProvider::new(1);
    body_oog.set_gas_limit(FIRST_BODY_TOUCH_CLEANUP_GAS - 1);
    let body_scope = ExecutionScope::new();
    StorageHandle::enter(&mut body_oog, |storage| {
        begin_block(storage.clone(), &body_scope).unwrap();
        let failed_charge = body_scope.explicit_gas_checkpoint();
        assert!(matches!(
            mint(storage.clone(), &body_scope, BodyInput::Tribute(&body)),
            Err(PrecompileError::OutOfGas)
        ));
        assert!(overlay_leaf(storage.clone(), Collection::Tribute, body.tribute_id).is_none());
        let schema = CompressedEntitiesSchema::new(storage);
        assert_eq!(schema.touched.len().unwrap(), 0);
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 0);
        assert_eq!(body_scope.explicit_gas_since(failed_charge).unwrap(), 0);
    });
    assert!(body_oog.get_ordered_events().is_empty());

    // The second index reserve fails after temporary body/first-index writes;
    // the mutation's local checkpoint must still restore every component.
    let mut index_oog = HashMapStorageProvider::new(1);
    index_oog.set_gas_limit(
        FIRST_BODY_TOUCH_CLEANUP_GAS
            + BODY_TOUCHED_LENGTH_CLEANUP_GAS
            + 2 * FIRST_INDEX_TOUCH_CLEANUP_GAS
            + INDEX_TOUCHED_LENGTH_CLEANUP_GAS
            - 1,
    );
    let index_scope = ExecutionScope::new();
    StorageHandle::enter(&mut index_oog, |storage| {
        begin_block(storage.clone(), &index_scope).unwrap();
        let failed_charge = index_scope.explicit_gas_checkpoint();
        assert!(matches!(
            mint(storage.clone(), &index_scope, BodyInput::Tribute(&body)),
            Err(PrecompileError::OutOfGas)
        ));
        assert!(overlay_leaf(storage.clone(), Collection::Tribute, body.tribute_id).is_none());
        let schema = CompressedEntitiesSchema::new(storage);
        assert_eq!(schema.touched.len().unwrap(), 0);
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 0);
        assert_eq!(
            index_scope.explicit_gas_since(failed_charge).unwrap(),
            FIRST_BODY_TOUCH_CLEANUP_GAS
                + BODY_TOUCHED_LENGTH_CLEANUP_GAS
                + FIRST_INDEX_TOUCH_CLEANUP_GAS
                + INDEX_TOUCHED_LENGTH_CLEANUP_GAS,
            "successful explicit deductions remain charged when later work OOGs"
        );
    });
    assert!(index_oog.get_ordered_events().is_empty());
}

#[test]
fn static_context_rejects_mutation_before_overlay_or_event_state() {
    let owner = address!("8210000000000000000000000000000000000008");
    let body = tribute(entity(14, 85), owner, 100);
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage, &scope).unwrap();
    });
    provider.set_static(true);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(matches!(
            mint(storage, &scope, BodyInput::Tribute(&body)),
            Err(PrecompileError::WriteProtection)
        ));
    });
    provider.set_static(false);
    StorageHandle::enter(&mut provider, |storage| {
        let schema = CompressedEntitiesSchema::new(storage.clone());
        assert_eq!(schema.touched.len().unwrap(), 0);
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 0);
        end_block(storage, &scope).unwrap();
    });
    assert!(provider.get_ordered_events().is_empty());
}

#[test]
fn explicit_gas_window_stops_a_system_transaction_before_it_exceeds_its_envelope() {
    let owner = address!("8300000000000000000000000000000000000008");
    let first = tribute(entity(14, 83), owner, 100);
    let second = tribute(entity(14, 84), owner, 101);
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_gas_limit(FIRST_TRIBUTE_CLEANUP_GAS * 2);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        let window = scope
            .begin_explicit_gas_window(FIRST_TRIBUTE_CLEANUP_GAS)
            .expect("system transaction opens its CE gas window");

        mint(storage.clone(), &scope, BodyInput::Tribute(&first)).unwrap();
        assert!(matches!(
            mint(storage.clone(), &scope, BodyInput::Tribute(&second)),
            Err(PrecompileError::OutOfGas)
        ));
        assert_eq!(window.gas_used().unwrap(), FIRST_TRIBUTE_CLEANUP_GAS);
        assert!(overlay_leaf(storage.clone(), Collection::Tribute, first.tribute_id).is_some());
        assert!(overlay_leaf(storage, Collection::Tribute, second.tribute_id).is_none());
    });
}

#[test]
fn ce_work_meter_reserves_unique_keys_and_restores_only_excluded_transactions() {
    let owner = address!("8400000000000000000000000000000000000008");
    let first = tribute(entity(14, 85), owner, 100);
    let second = tribute(entity(14, 86), owner, 101);
    let third = tribute(entity(14, 87), owner, 102);
    let tree = Arc::new(TestAuthenticatedTree::default());
    let scope = ExecutionScope::with_parent_tree(tree, CeWorkConfig::new(3, 4, 11));
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        assert_eq!(scope.ce_work_used().unwrap(), 3);
        mint(storage.clone(), &scope, BodyInput::Tribute(&first)).unwrap();
        assert_eq!(scope.ce_work_used().unwrap(), 7);

        let excluded = scope.ce_work_checkpoint().unwrap();
        let excluded_result: Result<()> = storage.clone().with_checkpoint(|| {
            mint(storage.clone(), &scope, BodyInput::Tribute(&second))?;
            Err(PrecompileError::Revert(
                "payload builder excluded transaction".into(),
            ))
        });
        assert!(matches!(excluded_result, Err(PrecompileError::Revert(_))));
        assert_eq!(scope.ce_work_used().unwrap(), 11);
        scope.restore_ce_work_checkpoint(excluded).unwrap();
        assert_eq!(scope.ce_work_used().unwrap(), 7);
        assert!(overlay_leaf(storage.clone(), Collection::Tribute, second.tribute_id).is_none());

        mint(storage.clone(), &scope, BodyInput::Tribute(&third)).unwrap();
        assert_eq!(scope.ce_work_used().unwrap(), 11);
        assert!(matches!(
            mint(storage, &scope, BodyInput::Tribute(&second)),
            Err(PrecompileError::BlockCeWorkCapacityExhausted)
        ));
    });

    let too_small = ExecutionScope::with_parent_tree(
        Arc::new(TestAuthenticatedTree::default()),
        CeWorkConfig::new(3, 4, 6),
    );
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &too_small).unwrap();
        assert!(matches!(
            mint(storage, &too_small, BodyInput::Tribute(&first)),
            Err(PrecompileError::TransactionCeWorkLimitExceeded)
        ));
    });

    let multi_key = ExecutionScope::with_parent_tree(
        Arc::new(TestAuthenticatedTree::default()),
        CeWorkConfig::new(3, 4, 11),
    );
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &multi_key).unwrap();
        multi_key.begin_ce_work_transaction().unwrap();
        mint(storage.clone(), &multi_key, BodyInput::Tribute(&first)).unwrap();
        mint(storage.clone(), &multi_key, BodyInput::Tribute(&second)).unwrap();
        assert!(matches!(
            mint(storage.clone(), &multi_key, BodyInput::Tribute(&third)),
            Err(PrecompileError::TransactionCeWorkLimitExceeded)
        ));
        assert!(matches!(
            multi_key.take_ce_work_failure(),
            Some(PrecompileError::TransactionCeWorkLimitExceeded)
        ));
        multi_key.end_ce_work_transaction().unwrap();
    });

    let overlapping_transaction = ExecutionScope::with_parent_tree(
        Arc::new(TestAuthenticatedTree::default()),
        CeWorkConfig::new(3, 4, 11),
    );
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &overlapping_transaction).unwrap();
        overlapping_transaction.begin_ce_work_transaction().unwrap();
        mint(
            storage.clone(),
            &overlapping_transaction,
            BodyInput::Tribute(&first),
        )
        .unwrap();
        let first_capability = read(
            storage.clone(),
            &overlapping_transaction,
            &MemoryParent::default(),
            EntityRef::Tribute(first.tribute_id),
        )
        .unwrap()
        .unwrap();
        overlapping_transaction.end_ce_work_transaction().unwrap();

        overlapping_transaction.begin_ce_work_transaction().unwrap();
        delete(storage.clone(), &overlapping_transaction, first_capability).unwrap();
        mint(
            storage.clone(),
            &overlapping_transaction,
            BodyInput::Tribute(&second),
        )
        .unwrap();
        assert!(matches!(
            mint(
                storage,
                &overlapping_transaction,
                BodyInput::Tribute(&third)
            ),
            Err(PrecompileError::TransactionCeWorkLimitExceeded)
        ));
        assert!(matches!(
            overlapping_transaction.take_ce_work_failure(),
            Some(PrecompileError::TransactionCeWorkLimitExceeded)
        ));
        overlapping_transaction.end_ce_work_transaction().unwrap();
    });

    let remaining_capacity = ExecutionScope::with_parent_tree(
        Arc::new(TestAuthenticatedTree::default()),
        CeWorkConfig::new(3, 4, 11),
    );
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &remaining_capacity).unwrap();
        remaining_capacity.begin_ce_work_transaction().unwrap();
        mint(
            storage.clone(),
            &remaining_capacity,
            BodyInput::Tribute(&first),
        )
        .unwrap();
        remaining_capacity.end_ce_work_transaction().unwrap();

        remaining_capacity.begin_ce_work_transaction().unwrap();
        mint(
            storage.clone(),
            &remaining_capacity,
            BodyInput::Tribute(&second),
        )
        .unwrap();
        assert!(matches!(
            mint(storage, &remaining_capacity, BodyInput::Tribute(&third)),
            Err(PrecompileError::BlockCeWorkCapacityExhausted)
        ));
        assert!(matches!(
            remaining_capacity.take_ce_work_failure(),
            Some(PrecompileError::BlockCeWorkCapacityExhausted)
        ));
        remaining_capacity.end_ce_work_transaction().unwrap();
    });
}

#[test]
fn golden_read_list_and_first_touch_gas_coefficients_are_exact() {
    assert_eq!(READ_FIXED_GAS, 200);
    assert_eq!(READ_GAS_PER_CANONICAL_BYTE, 8);
    assert_eq!(INDEX_RECORD_SCAN_GAS, 300);
    assert_eq!(PARENT_ID_GAS, 120);
    assert_eq!(FIRST_BODY_TOUCH_CLEANUP_GAS, 55_000);
    assert_eq!(BODY_TOUCHED_LENGTH_CLEANUP_GAS, 5_000);
    assert_eq!(FIRST_INDEX_TOUCH_CLEANUP_GAS, 25_000);
    assert_eq!(INDEX_TOUCHED_LENGTH_CLEANUP_GAS, 5_000);

    let owner = address!("b00000000000000000000000000000000000000b");
    let overlay = tribute(entity(20, 4), owner, 100);
    let overlay_bytes = stored_tribute(&overlay).encode().len() as u64;
    let parent_a = tribute(entity(20, 2), owner, 200);
    let parent_b = tribute(entity(20, 3), owner, 300);
    let parent_bytes = [stored_tribute(&parent_a), stored_tribute(&parent_b)]
        .map(|body| body.encode().len() as u64);
    let mut parent = MemoryParent::default();
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_gas_limit(u64::MAX);
    let tree = Arc::new(TestAuthenticatedTree::default());
    let scope = scope_with_tree(tree.clone());

    StorageHandle::enter(&mut provider, |storage| {
        seed_parent_tribute(&mut parent, tree.as_ref(), &parent_a);
        seed_parent_tribute(&mut parent, tree.as_ref(), &parent_b);
        begin_block(storage.clone(), &scope).unwrap();
        let first_touch = scope.explicit_gas_checkpoint();
        mint(storage.clone(), &scope, BodyInput::Tribute(&overlay)).unwrap();
        assert_eq!(
            scope.explicit_gas_since(first_touch).unwrap(),
            FIRST_BODY_TOUCH_CLEANUP_GAS
                + BODY_TOUCHED_LENGTH_CLEANUP_GAS
                + 2 * FIRST_INDEX_TOUCH_CLEANUP_GAS
                + INDEX_TOUCHED_LENGTH_CLEANUP_GAS,
            "60k body + 5k first body-list length + 2*25k indexes + 5k first index-list length"
        );

        let overlay_read = scope.explicit_gas_checkpoint();
        read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(overlay.tribute_id),
        )
        .unwrap();
        let read_charge = READ_FIXED_GAS + READ_GAS_PER_CANONICAL_BYTE * overlay_bytes;
        assert_eq!(scope.explicit_gas_since(overlay_read).unwrap(), read_charge);

        let repeat = scope.explicit_gas_checkpoint();
        let cap = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(overlay.tribute_id),
        )
        .unwrap()
        .unwrap();
        update(storage.clone(), &scope, cap, BodyInput::Tribute(&overlay)).unwrap();
        assert_eq!(scope.explicit_gas_since(repeat).unwrap(), read_charge);

        let delta_scan = scope.explicit_gas_checkpoint();
        list(
            storage.clone(),
            &scope,
            &parent,
            QueryRef::TributeByOwner(owner),
            IdPageRequest {
                after: Some(parent_b.tribute_id),
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(
            scope.explicit_gas_since(delta_scan).unwrap(),
            2 * INDEX_RECORD_SCAN_GAS + read_charge
        );

        let parent_page = scope.explicit_gas_checkpoint();
        list(
            storage,
            &scope,
            &parent,
            QueryRef::TributeByOwner(owner),
            IdPageRequest {
                after: None,
                limit: 2,
            },
        )
        .unwrap();
        assert_eq!(
            scope.explicit_gas_since(parent_page).unwrap(),
            2 * INDEX_RECORD_SCAN_GAS
                + 2 * PARENT_ID_GAS
                + parent_bytes
                    .into_iter()
                    .map(|bytes| READ_FIXED_GAS + READ_GAS_PER_CANONICAL_BYTE * bytes)
                    .sum::<u64>()
        );
    });

    let mut body_length_oog = HashMapStorageProvider::new(1);
    body_length_oog
        .set_gas_limit(FIRST_BODY_TOUCH_CLEANUP_GAS + BODY_TOUCHED_LENGTH_CLEANUP_GAS - 1);
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut body_length_oog, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        let checkpoint = scope.explicit_gas_checkpoint();
        assert!(matches!(
            mint(storage.clone(), &scope, BodyInput::Tribute(&overlay)),
            Err(PrecompileError::OutOfGas)
        ));
        assert_eq!(scope.explicit_gas_since(checkpoint).unwrap(), 0);
        assert!(overlay_leaf(storage.clone(), Collection::Tribute, overlay.tribute_id).is_none());
        assert!(CompressedEntitiesSchema::new(storage)
            .touched
            .is_empty()
            .unwrap());
    });

    let mut index_length_oog = HashMapStorageProvider::new(1);
    index_length_oog.set_gas_limit(
        FIRST_BODY_TOUCH_CLEANUP_GAS
            + BODY_TOUCHED_LENGTH_CLEANUP_GAS
            + FIRST_INDEX_TOUCH_CLEANUP_GAS
            + INDEX_TOUCHED_LENGTH_CLEANUP_GAS
            - 1,
    );
    let scope = ExecutionScope::new();
    StorageHandle::enter(&mut index_length_oog, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        let checkpoint = scope.explicit_gas_checkpoint();
        assert!(matches!(
            mint(storage.clone(), &scope, BodyInput::Tribute(&overlay)),
            Err(PrecompileError::OutOfGas)
        ));
        assert_eq!(
            scope.explicit_gas_since(checkpoint).unwrap(),
            FIRST_BODY_TOUCH_CLEANUP_GAS + BODY_TOUCHED_LENGTH_CLEANUP_GAS
        );
        let schema = CompressedEntitiesSchema::new(storage.clone());
        assert!(schema.touched.is_empty().unwrap());
        assert!(schema.touched_index_deltas.is_empty().unwrap());
        assert!(overlay_leaf(storage, Collection::Tribute, overlay.tribute_id).is_none());
    });
}
