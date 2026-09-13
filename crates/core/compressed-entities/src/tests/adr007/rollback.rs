use super::*;

#[test]
fn outer_checkpoint_reverts_commitment_overlay_indexes_and_event_together() {
    let owner = address!("4000000000000000000000000000000000000004");
    let body = tribute(entity(11, 5), owner, 100);
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        let transaction_gas = scope.explicit_gas_checkpoint();
        let failed: Result<()> = storage.with_checkpoint(|| {
            mint(storage.clone(), &scope, BodyInput::Tribute(&body))?;
            Err(PrecompileError::Revert("outer transaction reverted".into()))
        });
        assert!(matches!(failed, Err(PrecompileError::Revert(_))));
        assert!(overlay_leaf(storage.clone(), Collection::Tribute, body.tribute_id).is_none());
        assert!(read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(body.tribute_id)
        )
        .unwrap()
        .is_none());
        let schema = CompressedEntitiesSchema::new(storage);
        assert_eq!(schema.touched.len().unwrap(), 0);
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 0);
        assert_eq!(
            scope.explicit_gas_since(transaction_gas).unwrap(),
            FIRST_TRIBUTE_CLEANUP_GAS,
            "journal rollback must not refund explicit work gas"
        );
    });
    assert!(provider.get_ordered_events().is_empty());
}

#[derive(Clone, Copy)]
enum FaultMutation {
    Mint,
    Update,
    Delete,
}

#[derive(Clone, Copy)]
pub(super) enum FaultPosition {
    Before,
    After,
}

fn prepare_fault_mutation(
    provider: &mut HashMapStorageProvider,
    scope: &ExecutionScope,
    original: &FixtureBody,
    mutation: FaultMutation,
) -> Option<VerifiedBody> {
    StorageHandle::enter(provider, |storage| {
        begin_block(storage.clone(), scope).unwrap();
        if matches!(mutation, FaultMutation::Mint) {
            return None;
        }
        mint(storage.clone(), scope, original.input()).unwrap();
        read(
            storage,
            scope,
            &MemoryParent::default(),
            original.entity_ref(),
        )
        .unwrap()
    })
}

fn apply_fault_mutation(
    storage: StorageHandle<'_>,
    scope: &ExecutionScope,
    original: &FixtureBody,
    updated: &FixtureBody,
    mutation: FaultMutation,
    capability: Option<VerifiedBody>,
) -> Result<()> {
    match mutation {
        FaultMutation::Mint => mint(storage, scope, original.input()),
        FaultMutation::Update => update(
            storage,
            scope,
            capability.expect("update fixture has a value capability"),
            updated.input(),
        ),
        FaultMutation::Delete => delete(
            storage,
            scope,
            capability.expect("delete fixture has a value capability"),
        ),
    }
}

pub(super) fn arm_fault(
    provider: &mut HashMapStorageProvider,
    position: FaultPosition,
    operation: usize,
) {
    match position {
        FaultPosition::Before => provider.fail_mutation_at(operation),
        FaultPosition::After => provider.fail_after_mutation_at(operation),
    }
}

fn exercise_every_fault_boundary(
    original: &FixtureBody,
    updated: &FixtureBody,
    mutation: FaultMutation,
) {
    let mut baseline = HashMapStorageProvider::new(1);
    let baseline_scope = ExecutionScope::new();
    let capability = prepare_fault_mutation(&mut baseline, &baseline_scope, original, mutation);
    baseline.clear_mutation_failure();
    StorageHandle::enter(&mut baseline, |storage| {
        apply_fault_mutation(
            storage,
            &baseline_scope,
            original,
            updated,
            mutation,
            capability,
        )
        .unwrap();
    });
    let mutation_operations = baseline.clear_mutation_failure();
    assert!(mutation_operations > 0);

    for position in [FaultPosition::Before, FaultPosition::After] {
        for failure_at in 0..mutation_operations {
            let mut provider = HashMapStorageProvider::new(1);
            let scope = ExecutionScope::new();
            let capability = prepare_fault_mutation(&mut provider, &scope, original, mutation);
            let storage_before = provider.storage.clone();
            let events_before = provider.get_ordered_events().to_vec();
            provider.clear_mutation_failure();
            arm_fault(&mut provider, position, failure_at);

            let error = StorageHandle::enter(&mut provider, |storage| {
                apply_fault_mutation(storage, &scope, original, updated, mutation, capability)
                    .unwrap_err()
            });
            assert!(matches!(error, PrecompileError::Storage(_)));
            provider.clear_mutation_failure();
            assert_eq!(&provider.storage, &storage_before);
            assert_eq!(provider.get_ordered_events(), events_before);

            StorageHandle::enter(&mut provider, |storage| {
                let current = read(
                    storage.clone(),
                    &scope,
                    &MemoryParent::default(),
                    original.entity_ref(),
                )
                .unwrap();
                let schema = CompressedEntitiesSchema::new(storage.clone());
                if matches!(mutation, FaultMutation::Mint) {
                    assert!(current.is_none());
                    assert_eq!(schema.touched.len().unwrap(), 0);
                    assert_eq!(schema.touched_index_deltas.len().unwrap(), 0);
                } else {
                    original.assert_verified(&current.unwrap());
                    assert_eq!(schema.touched.len().unwrap(), 1);
                    assert_eq!(
                        schema.touched_index_deltas.len().unwrap(),
                        original.expected_index_touches()
                    );
                }
                end_block(storage, &scope).unwrap();
            });
        }
    }
}

#[test]
fn every_mutation_write_and_event_boundary_rolls_back_for_all_typed_collections() {
    let owner = address!("8a0000000000000000000000000000000000008a");
    let moved_owner = address!("8b0000000000000000000000000000000000008b");

    let tribute_original = tribute(entity(14, 0x8a), owner, 100);
    let mut tribute_updated = tribute(tribute_original.tribute_id, moved_owner, 200);
    tribute_updated.worldwide_day = tribute_original.worldwide_day;

    let nod_original = nod_item(entity(14, 0x8b), owner);
    let mut nod_updated = nod_original.clone();
    nod_updated.owner = moved_owner;
    nod_updated.gratis_load_minor = U256::from(99);

    let bucket_original = NodBucketBodyV1 {
        bucket_key: B256::repeat_byte(0x8c),
        worldwide_day: WorldwideDay::new(14),
        floor_price_minor: U256::from(10),
        is_qualified: false,
        entry_price_minor: U256::from(11),
        reference_currency: 840,
    };
    let mut bucket_updated = bucket_original.clone();
    bucket_updated.is_qualified = true;

    for (original, updated) in [
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
    ] {
        for mutation in [
            FaultMutation::Mint,
            FaultMutation::Update,
            FaultMutation::Delete,
        ] {
            exercise_every_fault_boundary(&original, &updated, mutation);
        }
    }
}
