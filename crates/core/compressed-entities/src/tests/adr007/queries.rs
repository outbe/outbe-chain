use super::*;

#[derive(Default)]
struct ScriptedParent {
    bodies: HashMap<EntityRef, StoredBody>,
    pages: RefCell<VecDeque<IdPage>>,
}

impl ParentBodySource for ScriptedParent {
    fn get(
        &self,
        entity: EntityRef,
    ) -> core::result::Result<Option<StoredBody>, ParentBodySourceError> {
        Ok(self.bodies.get(&entity).cloned())
    }

    fn list(
        &self,
        _query: QueryRef,
        _request: IdPageRequest,
    ) -> core::result::Result<IdPage, ParentBodySourceError> {
        Ok(self.pages.borrow_mut().pop_front().unwrap_or(IdPage {
            ids: Vec::new(),
            next_after: None,
        }))
    }
}

fn nod_item_commitment(body: &NodItemBodyV1) -> crate::Commitment {
    let payload = encode_nod_item_v1(body).unwrap();
    body_commitment(
        ACTIVE_COMMITMENT_SCHEME,
        BODY_SCHEMA_V1,
        body.nod_id,
        &payload,
    )
    .unwrap()
}

fn seed_parent_nod_item(
    parent: &mut MemoryParent,
    tree: &TestAuthenticatedTree,
    body: &NodItemBodyV1,
) {
    parent.insert_nod_item(body);
    tree.insert(EntityRef::NodItem(body.nod_id), nod_item_commitment(body));
}

/// The bucket key an identity is consistent with.
///
/// `NodBucketBodyV1::entity_id` derives the identity as `wwd ++ key[4..]`, so
/// the key's leading four bytes are not recoverable from it. Fixtures that
/// start from an identity rebuild the one key that re-derives to it.
fn bucket_key_for(id: WwdEntityId) -> B256 {
    let mut key = [0_u8; 32];
    key[4..].copy_from_slice(&id.body());
    B256::from(key)
}

#[test]
fn untouched_reads_use_parent_once_and_classify_missing_committed_body() {
    let owner = address!("2300000000000000000000000000000000000002");
    let present = tribute(entity(8, 25), owner, 100);
    let missing = tribute(entity(8, 26), owner, 200);
    let stale_tribute = tribute(entity(8, 27), owner, 300);
    let stale_nod = nod_item(entity(8, 28), owner);
    let stale_bucket_id = entity(8, 29);
    let stale_bucket = NodBucketBodyV1 {
        settled_nods: 0,
        bucket_key: bucket_key_for(stale_bucket_id),
        worldwide_day: stale_bucket_id.worldwide_day(),
        floor_price_minor: U256::from(4),
        entry_price_minor: U256::from(5),
        reference_currency: 840,
    };
    let missing_bucket_id = entity(8, 30);
    let missing_bucket = NodBucketBodyV1 {
        settled_nods: 0,
        bucket_key: bucket_key_for(missing_bucket_id),
        worldwide_day: missing_bucket_id.worldwide_day(),
        floor_price_minor: U256::from(6),
        entry_price_minor: U256::from(7),
        reference_currency: 840,
    };
    let mut parent = MemoryParent::default();
    let tree = Arc::new(TestAuthenticatedTree::default());
    let scope = scope_with_tree(tree.clone());
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        seed_parent_tribute(&mut parent, tree.as_ref(), &present);
        tree.insert(
            EntityRef::Tribute(missing.tribute_id),
            tribute_commitment(&missing),
        );
        parent.insert_tribute(&stale_tribute);
        parent.insert_nod_item(&stale_nod);
        parent.bodies.insert(
            EntityRef::NodBucket(stale_bucket_id),
            StoredBody::new_v1(encode_nod_bucket_v1(&stale_bucket).unwrap()).unwrap(),
        );
        tree.insert(
            EntityRef::NodBucket(missing_bucket_id),
            body_commitment(
                ACTIVE_COMMITMENT_SCHEME,
                BODY_SCHEMA_V1,
                missing_bucket_id,
                &encode_nod_bucket_v1(&missing_bucket).unwrap(),
            )
            .unwrap(),
        );
        begin_block(storage.clone(), &scope).unwrap();
        let loaded = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(present.tribute_id),
        )
        .unwrap()
        .unwrap();
        assert_eq!(loaded.payload().as_tribute().unwrap(), &present);
        assert_eq!(parent.get_calls.get(), 1);
        for absent in [
            EntityRef::Tribute(stale_tribute.tribute_id),
            EntityRef::NodItem(stale_nod.nod_id),
            EntityRef::NodBucket(stale_bucket_id),
        ] {
            assert!(read(storage.clone(), &scope, &parent, absent)
                .unwrap()
                .is_none());
        }
        assert_eq!(
            parent.get_calls.get(),
            1,
            "authenticated absence must bypass even stale Mongo rows in every collection"
        );
        assert!(matches!(
            read(
                storage.clone(),
                &scope,
                &parent,
                EntityRef::Tribute(missing.tribute_id)
            ),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
        assert_eq!(parent.get_calls.get(), 2);
        assert!(matches!(
            read(
                storage,
                &scope,
                &parent,
                EntityRef::NodBucket(missing_bucket_id)
            ),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
        assert_eq!(parent.get_calls.get(), 3);
    });
}

#[test]
fn merged_list_applies_removals_additions_pagination_and_overlay_bodies() {
    let owner1 = address!("5000000000000000000000000000000000000005");
    let owner2 = address!("6000000000000000000000000000000000000006");
    let a = tribute(entity(12, 1), owner1, 10);
    let b = tribute(entity(12, 2), owner1, 20);
    let c = tribute(entity(12, 3), owner1, 30);
    let d = tribute(entity(12, 4), owner1, 40);
    let b_moved = tribute(b.tribute_id, owner2, 21);
    let mut parent = MemoryParent::default();
    let tree = Arc::new(TestAuthenticatedTree::default());
    let scope = scope_with_tree(tree.clone());
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        for body in [&a, &b, &c] {
            seed_parent_tribute(&mut parent, tree.as_ref(), body);
        }
        begin_block(storage.clone(), &scope).unwrap();
        let a_cap = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(a.tribute_id),
        )
        .unwrap()
        .unwrap();
        delete(storage.clone(), &scope, a_cap).unwrap();
        let b_cap = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(b.tribute_id),
        )
        .unwrap()
        .unwrap();
        update(storage.clone(), &scope, b_cap, BodyInput::Tribute(&b_moved)).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&d)).unwrap();

        let first = list(
            storage.clone(),
            &scope,
            &parent,
            QueryRef::TributeByOwner(owner1),
            IdPageRequest {
                after: None,
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(
            first
                .bodies()
                .iter()
                .map(|body| body.entity_id())
                .collect::<Vec<_>>(),
            vec![c.tribute_id]
        );
        assert_eq!(first.next_after(), Some(c.tribute_id));
        let second = list(
            storage.clone(),
            &scope,
            &parent,
            QueryRef::TributeByOwner(owner1),
            IdPageRequest {
                after: first.next_after(),
                limit: 1,
            },
        )
        .unwrap();
        assert_eq!(second.bodies()[0].entity_id(), d.tribute_id);
        assert_eq!(second.next_after(), None);
        assert_eq!(second.bodies()[0].payload().as_tribute().unwrap(), &d);

        let moved = list(
            storage,
            &scope,
            &parent,
            QueryRef::TributeByOwner(owner2),
            IdPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(moved.bodies()[0].payload().as_tribute().unwrap(), &b_moved);
    });
    assert!(parent.list_calls.get() >= 3);
}

#[test]
fn malformed_parent_order_is_rejected_as_corruption() {
    let owner = address!("7000000000000000000000000000000000000007");
    let a = tribute(entity(13, 1), owner, 10);
    let b = tribute(entity(13, 2), owner, 20);
    let mut parent = MemoryParent {
        reverse_pages: true,
        ..MemoryParent::default()
    };
    let tree = Arc::new(TestAuthenticatedTree::default());
    let scope = scope_with_tree(tree.clone());
    let mut provider = HashMapStorageProvider::new(1);

    StorageHandle::enter(&mut provider, |storage| {
        seed_parent_tribute(&mut parent, tree.as_ref(), &a);
        seed_parent_tribute(&mut parent, tree.as_ref(), &b);
        begin_block(storage.clone(), &scope).unwrap();
        let result = list(
            storage,
            &scope,
            &parent,
            QueryRef::TributeByOwner(owner),
            IdPageRequest {
                after: None,
                limit: 2,
            },
        );
        assert!(matches!(
            result,
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });
}

#[test]
fn page_limit_outside_fork_bound_is_a_deterministic_revert() {
    let parent = MemoryParent::default();
    let tree = Arc::new(TestAuthenticatedTree::default());
    let scope = scope_with_tree(tree.clone());
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        for limit in [0, MAX_ID_PAGE_LIMIT + 1] {
            assert!(matches!(
                list(
                    storage.clone(),
                    &scope,
                    &parent,
                    QueryRef::NodAll,
                    IdPageRequest { after: None, limit }
                ),
                Err(PrecompileError::Revert(_))
            ));
        }
    });
    assert_eq!(parent.list_calls.get(), 0);
}

#[test]
fn all_four_query_kinds_resolve_same_block_membership_and_overlay_bodies() {
    let owner = address!("c00000000000000000000000000000000000000c");
    let tribute = tribute(entity(21, 1), owner, 100);
    let nod = nod_item(entity(21, 2), owner);
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&tribute)).unwrap();
        mint(storage.clone(), &scope, BodyInput::NodItem(&nod)).unwrap();
        for (query, expected) in [
            (QueryRef::TributeByOwner(owner), tribute.tribute_id),
            (
                QueryRef::TributeByDay(tribute.worldwide_day),
                tribute.tribute_id,
            ),
            (QueryRef::NodByOwner(owner), nod.nod_id),
            (QueryRef::NodAll, nod.nod_id),
        ] {
            let page = list(
                storage.clone(),
                &scope,
                &parent,
                query,
                IdPageRequest {
                    after: None,
                    limit: 10,
                },
            )
            .unwrap();
            assert_eq!(
                page.bodies()
                    .iter()
                    .map(|body| body.entity_id())
                    .collect::<Vec<_>>(),
                vec![expected]
            );
            assert_eq!(page.next_after(), None);
        }
    });
}

#[test]
fn all_four_query_kinds_merge_non_empty_parent_pages_with_exact_pagination() {
    let owner = address!("c10000000000000000000000000000000000000c");

    let tribute_parent = [
        tribute(entity(24, 1), owner, 101),
        tribute(entity(24, 3), owner, 103),
        tribute(entity(24, 5), owner, 105),
    ];
    let tribute_added = tribute(entity(24, 2), owner, 102);
    let mut tribute_source = MemoryParent::default();
    let tribute_tree = Arc::new(TestAuthenticatedTree::default());
    let tribute_scope = scope_with_tree(tribute_tree.clone());
    let mut tribute_provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut tribute_provider, |storage| {
        for body in &tribute_parent {
            seed_parent_tribute(&mut tribute_source, tribute_tree.as_ref(), body);
        }
        begin_block(storage.clone(), &tribute_scope).unwrap();
        let removed = read(
            storage.clone(),
            &tribute_scope,
            &tribute_source,
            EntityRef::Tribute(tribute_parent[1].tribute_id),
        )
        .unwrap()
        .unwrap();
        delete(storage.clone(), &tribute_scope, removed).unwrap();
        mint(
            storage.clone(),
            &tribute_scope,
            BodyInput::Tribute(&tribute_added),
        )
        .unwrap();

        for query in [
            QueryRef::TributeByOwner(owner),
            QueryRef::TributeByDay(WorldwideDay::new(24)),
        ] {
            let first = list(
                storage.clone(),
                &tribute_scope,
                &tribute_source,
                query,
                IdPageRequest {
                    after: None,
                    limit: 2,
                },
            )
            .unwrap();
            assert_eq!(
                first
                    .bodies()
                    .iter()
                    .map(VerifiedBody::entity_id)
                    .collect::<Vec<_>>(),
                vec![tribute_parent[0].tribute_id, tribute_added.tribute_id]
            );
            assert_eq!(first.next_after(), Some(tribute_added.tribute_id));
            assert_eq!(
                first.bodies()[1].payload().as_tribute(),
                Some(&tribute_added)
            );

            let second = list(
                storage.clone(),
                &tribute_scope,
                &tribute_source,
                query,
                IdPageRequest {
                    after: first.next_after(),
                    limit: 2,
                },
            )
            .unwrap();
            assert_eq!(
                second
                    .bodies()
                    .iter()
                    .map(VerifiedBody::entity_id)
                    .collect::<Vec<_>>(),
                vec![tribute_parent[2].tribute_id]
            );
            assert_eq!(second.next_after(), None);
        }
    });
    assert!(tribute_source.list_calls.get() >= 6);

    let nod_parent = [
        nod_item(entity(25, 1), owner),
        nod_item(entity(25, 3), owner),
        nod_item(entity(25, 5), owner),
    ];
    let nod_added = nod_item(entity(25, 2), owner);
    let mut nod_source = MemoryParent::default();
    let nod_tree = Arc::new(TestAuthenticatedTree::default());
    let nod_scope = scope_with_tree(nod_tree.clone());
    let mut nod_provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut nod_provider, |storage| {
        for body in &nod_parent {
            seed_parent_nod_item(&mut nod_source, nod_tree.as_ref(), body);
        }
        begin_block(storage.clone(), &nod_scope).unwrap();
        let removed = read(
            storage.clone(),
            &nod_scope,
            &nod_source,
            EntityRef::NodItem(nod_parent[1].nod_id),
        )
        .unwrap()
        .unwrap();
        delete(storage.clone(), &nod_scope, removed).unwrap();
        mint(storage.clone(), &nod_scope, BodyInput::NodItem(&nod_added)).unwrap();

        for query in [QueryRef::NodByOwner(owner), QueryRef::NodAll] {
            let first = list(
                storage.clone(),
                &nod_scope,
                &nod_source,
                query,
                IdPageRequest {
                    after: None,
                    limit: 2,
                },
            )
            .unwrap();
            assert_eq!(
                first
                    .bodies()
                    .iter()
                    .map(VerifiedBody::entity_id)
                    .collect::<Vec<_>>(),
                vec![nod_parent[0].nod_id, nod_added.nod_id]
            );
            assert_eq!(first.next_after(), Some(nod_added.nod_id));
            assert_eq!(first.bodies()[1].payload().as_nod_item(), Some(&nod_added));

            let second = list(
                storage.clone(),
                &nod_scope,
                &nod_source,
                query,
                IdPageRequest {
                    after: first.next_after(),
                    limit: 2,
                },
            )
            .unwrap();
            assert_eq!(
                second
                    .bodies()
                    .iter()
                    .map(VerifiedBody::entity_id)
                    .collect::<Vec<_>>(),
                vec![nod_parent[2].nod_id]
            );
            assert_eq!(second.next_after(), None);
        }
    });
    assert!(nod_source.list_calls.get() >= 6);
}

#[test]
fn pagination_and_parent_corruption_boundaries_fail_closed() {
    let owner = address!("d00000000000000000000000000000000000000d");
    let id = entity(22, 1);
    let wrong_day = entity(23, 1);

    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        let parent = MemoryParent::default();
        assert!(matches!(
            list(
                storage,
                &scope,
                &parent,
                QueryRef::TributeByDay(WorldwideDay::new(22)),
                IdPageRequest {
                    after: Some(wrong_day),
                    limit: 1,
                },
            ),
            Err(PrecompileError::Revert(_))
        ));
    });

    for (page, query) in [
        (
            IdPage {
                ids: vec![id, entity(22, 2)],
                next_after: None,
            },
            QueryRef::TributeByOwner(owner),
        ),
        (
            IdPage {
                ids: vec![id],
                next_after: Some(entity(22, 2)),
            },
            QueryRef::TributeByOwner(owner),
        ),
        (
            IdPage {
                ids: Vec::new(),
                next_after: Some(id),
            },
            QueryRef::TributeByOwner(owner),
        ),
        (
            IdPage {
                ids: vec![wrong_day],
                next_after: None,
            },
            QueryRef::TributeByDay(WorldwideDay::new(22)),
        ),
    ] {
        let scripted = ScriptedParent {
            pages: RefCell::new(VecDeque::from([page])),
            ..ScriptedParent::default()
        };
        let scope = ExecutionScope::new();
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            begin_block(storage.clone(), &scope).unwrap();
            assert!(matches!(
                list(
                    storage,
                    &scope,
                    &scripted,
                    query,
                    IdPageRequest {
                        after: None,
                        limit: 1,
                    },
                ),
                Err(PrecompileError::BodyReadCorruption(_))
            ));
        });
    }

    let body = tribute(id, owner, 100);
    let scripted = ScriptedParent {
        pages: RefCell::new(VecDeque::from([IdPage {
            ids: vec![id],
            next_after: None,
        }])),
        ..ScriptedParent::default()
    };
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Tribute(&body)).unwrap();
        assert!(matches!(
            list(
                storage,
                &scope,
                &scripted,
                QueryRef::TributeByOwner(owner),
                IdPageRequest {
                    after: None,
                    limit: 1,
                },
            ),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });

    let scripted = ScriptedParent {
        bodies: HashMap::from([(EntityRef::Tribute(id), stored_tribute(&body))]),
        pages: RefCell::new(VecDeque::from([IdPage {
            ids: Vec::new(),
            next_after: None,
        }])),
    };
    let tree = Arc::new(TestAuthenticatedTree::default());
    tree.insert(EntityRef::Tribute(id), tribute_commitment(&body));
    let scope = scope_with_tree(tree);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        let cap = read(storage.clone(), &scope, &scripted, EntityRef::Tribute(id))
            .unwrap()
            .unwrap();
        delete(storage.clone(), &scope, cap).unwrap();
        assert!(matches!(
            list(
                storage,
                &scope,
                &scripted,
                QueryRef::TributeByOwner(owner),
                IdPageRequest {
                    after: None,
                    limit: 1,
                },
            ),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });

    let wrong_owner_body = tribute(
        id,
        address!("9900000000000000000000000000000000000009"),
        100,
    );
    let scripted = ScriptedParent {
        bodies: HashMap::from([(EntityRef::Tribute(id), stored_tribute(&wrong_owner_body))]),
        pages: RefCell::new(VecDeque::from([IdPage {
            ids: vec![id],
            next_after: None,
        }])),
    };
    let tree = Arc::new(TestAuthenticatedTree::default());
    tree.insert(
        EntityRef::Tribute(id),
        tribute_commitment(&wrong_owner_body),
    );
    let scope = scope_with_tree(tree);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        assert!(matches!(
            list(
                storage,
                &scope,
                &scripted,
                QueryRef::TributeByOwner(owner),
                IdPageRequest {
                    after: None,
                    limit: 1,
                },
            ),
            Err(PrecompileError::BodyReadCorruption(_))
        ));
    });
}
