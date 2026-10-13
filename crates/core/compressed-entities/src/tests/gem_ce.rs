use super::*;
use crate::*;
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::GEM_ADDRESS;
use std::collections::BTreeMap;

pub(super) fn gem() -> GemBodyV1 {
    GemBodyV1 {
        gem_id: WwdEntityId::from([0xff; 32]),
        owner: Address::repeat_byte(0x22),
        gem_type: 2,
        promis_load_minor: U256::from(11),
        floor_price_minor: U256::from(12),
        issuance_currency: 840,
        reference_currency: 978,
        issued_at: 123,
        bucket_key: B256::repeat_byte(0x33),
        state: 0,
        called_at: 0,
        settled_at: 0,
    }
}

pub(super) fn bucket() -> GemBucketBodyV1 {
    GemBucketBodyV1 {
        bucket_key: B256::repeat_byte(0x33),
        worldwide_day: WorldwideDay::new(20_261_012),
        entry_price_minor: U256::from(13),
        reference_currency: 978,
        settled_gems: 0,
    }
}

#[test]
fn gem_and_bucket_encode_one_canonical_protobuf_and_reject_schema_2() {
    let item = gem();
    let group = bucket();
    let payload = encode_gem_v1(&item).unwrap();
    // Raw keccak ids do not carry a valid worldwide day.
    assert_eq!(decode_gem_v1(&payload).unwrap(), item);
    assert_eq!(
        encode_gem_v1(&decode_gem_v1(&payload).unwrap()).unwrap(),
        payload
    );
    assert_eq!(
        hex::encode(&payload),
        concat!(
            "0a20ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "121422222222222222222222222222222222222222221802",
            "2220000000000000000000000000000000000000000000000000000000000000000b",
            "2a20000000000000000000000000000000000000000000000000000000000000000c",
            "30c80638d207407b4a203333333333333333333333333333333333333333333333333333333333333333"
        )
    );
    let group_payload = encode_gem_bucket_v1(&group).unwrap();
    assert_eq!(decode_gem_bucket_v1(&group_payload).unwrap(), group);
    assert_eq!(
        encode_gem_bucket_v1(&decode_gem_bucket_v1(&group_payload).unwrap()).unwrap(),
        group_payload
    );
    for schema in [1, 2] {
        let item_bytes = StoredBody::new(schema, payload.clone()).unwrap().encode();
        let bucket_bytes = StoredBody::new(schema, group_payload.clone())
            .unwrap()
            .encode();
        assert_eq!(decode_stored_gem_v1(&item_bytes).is_ok(), schema == 1);
        assert_eq!(
            decode_stored_gem_bucket_v1(&bucket_bytes).is_ok(),
            schema == 1
        );
        if schema == 2 {
            assert!(
                crate::proof::canonical_body_leaf(CeDomain::Gem, item.gem_id, &item_bytes).is_err()
            );
            assert!(crate::proof::canonical_body_leaf(
                CeDomain::GemBucket,
                group.entity_id(),
                &bucket_bytes
            )
            .is_err());
        }
    }
    for suffix in [vec![0x50, 0], vec![0x68, 1], vec![0x10, 1]] {
        assert!(decode_gem_v1(&[payload.clone(), suffix].concat()).is_err());
    }
    let mut invalid = item.clone();
    invalid.state = 1;
    assert!(encode_gem_v1(&invalid).is_err());
    let mut settled = item;
    settled.state = 3;
    settled.called_at = 456;
    settled.settled_at = 789;
    assert_eq!(
        decode_gem_v1(&encode_gem_v1(&settled).unwrap()).unwrap(),
        settled
    );
    for suffix in [vec![0x28, 0], vec![0x30, 1], vec![0x10, 1]] {
        assert!(decode_gem_bucket_v1(&[group_payload.clone(), suffix].concat()).is_err());
    }
}

#[test]
fn gem_membership_touch_is_owner_only_and_bucket_touches_none() {
    let item = gem();
    let group = bucket();
    let scope = ExecutionScope::default();
    let parent = MemoryParent::default();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::GemBucket(&group)).unwrap();
        let schema = CompressedEntitiesSchema::new(storage.clone());
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 0);
        mint(storage.clone(), &scope, BodyInput::Gem(&item)).unwrap();
        assert_eq!(schema.touched_index_deltas.len().unwrap(), 1);
        let page = list(
            storage.clone(),
            &scope,
            &parent,
            QueryRef::GemByOwner(item.owner),
            IdPageRequest {
                after: None,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(page.bodies().len(), 1);
        assert_eq!(page.bodies()[0].payload().as_gem(), Some(&item));
        let current = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Gem(item.gem_id),
        )
        .unwrap()
        .unwrap();
        delete(storage.clone(), &scope, current).unwrap();
        assert!(list(
            storage.clone(),
            &scope,
            &parent,
            QueryRef::GemByOwner(item.owner),
            IdPageRequest {
                after: None,
                limit: 10
            }
        )
        .unwrap()
        .bodies()
        .is_empty());
        let current = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::GemBucket(group.entity_id()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(current.payload().as_gem_bucket(), Some(&group));
        delete(storage.clone(), &scope, current).unwrap();
        let sealed = end_block(storage, &scope).unwrap();
        assert_eq!(sealed.staged_tree_batch.changed_collections.len(), 0);
    });
}

#[test]
fn proof_siblings_are_eight_for_k256_and_four_for_tribute() {
    let item = gem();
    let group = bucket();
    let day = WorldwideDay::new(20_261_012);
    let tribute = tribute(
        WwdEntityId::from_day_and_digest(day, [7; 32]),
        item.owner,
        100,
    );
    let nod = nod_item(tribute.tribute_id, item.owner);
    let nod_bucket = NodBucketBodyV1 {
        bucket_key: group.bucket_key,
        worldwide_day: day,
        entry_price_minor: group.entry_price_minor,
        reference_currency: group.reference_currency,
        settled_nods: 0,
    };
    for (domain, id, payload, expected) in [
        (
            CeDomain::Tribute,
            tribute.tribute_id,
            encode_tribute_v1(&tribute).unwrap(),
            4,
        ),
        (
            CeDomain::NodItem,
            nod.nod_id,
            encode_nod_item_v1(&nod).unwrap(),
            8,
        ),
        (
            CeDomain::NodBucket,
            nod_bucket.entity_id(),
            encode_nod_bucket_v1(&nod_bucket).unwrap(),
            8,
        ),
        (CeDomain::Gem, item.gem_id, encode_gem_v1(&item).unwrap(), 8),
        (
            CeDomain::GemBucket,
            group.entity_id(),
            encode_gem_bucket_v1(&group).unwrap(),
            8,
        ),
    ] {
        let bytes = StoredBody::new(1, payload).unwrap().encode();
        let (root, evidence) = crate::proof::single_body_proof(domain, id, &bytes).unwrap();
        assert_eq!(evidence.shard_top_siblings.len(), expected);
        verify_body_in_collection(root, domain, id, &bytes, &evidence).unwrap();
        let mut short = evidence.clone();
        short.shard_top_siblings.pop();
        assert!(verify_body_in_collection(root, domain, id, &bytes, &short).is_err());
        let mut long = evidence.clone();
        long.shard_top_siblings.push(B256::ZERO);
        assert!(verify_body_in_collection(root, domain, id, &bytes, &long).is_err());
        let mut wrong = evidence.clone();
        wrong.shard_top_siblings[0] = B256::with_last_byte(1);
        assert!(verify_body_in_collection(root, domain, id, &bytes, &wrong).is_err());
        if expected == 8 {
            assert!(serde_json::from_value::<CollectionBodyProofV1>(
                serde_json::to_value(long).unwrap()
            )
            .is_err());
        }
    }
}

#[test]
fn one_block_seals_tribute_nod_gem_and_gem_bucket() {
    let item = gem();
    let group = bucket();
    let tribute = tribute(group.entity_id(), item.owner, 100);
    let nod = nod_item(group.entity_id(), item.owner);
    let nod_bucket = NodBucketBodyV1 {
        bucket_key: group.bucket_key,
        worldwide_day: group.worldwide_day,
        entry_price_minor: group.entry_price_minor,
        reference_currency: group.reference_currency,
        settled_nods: 0,
    };
    let scope = ExecutionScope::default();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        for input in [
            BodyInput::Tribute(&tribute),
            BodyInput::NodItem(&nod),
            BodyInput::NodBucket(&nod_bucket),
            BodyInput::Gem(&item),
            BodyInput::GemBucket(&group),
        ] {
            mint(storage.clone(), &scope, input).unwrap();
        }
        let sealed = end_block(storage, &scope).unwrap();
        assert_eq!(sealed.staged_tree_batch.changed_collections.len(), 5);
        assert_ne!(sealed.new_root, B256::ZERO);
    });
}

#[test]
fn gem_stored_event_is_recomputed_and_create_then_delete_collapses() {
    let item = gem();
    let group = bucket();
    let scope = ExecutionScope::default();
    let parent = MemoryParent::default();
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::Gem(&item)).unwrap();
        mint(storage.clone(), &scope, BodyInput::GemBucket(&group)).unwrap();
        for entity in [
            EntityRef::Gem(item.gem_id),
            EntityRef::GemBucket(group.entity_id()),
        ] {
            let current = read(storage.clone(), &scope, &parent, entity)
                .unwrap()
                .unwrap();
            delete(storage.clone(), &scope, current).unwrap();
        }
        end_block(storage, &scope).unwrap();
    });
    let logs = provider.get_ordered_events();
    assert_eq!(logs.len(), 4);
    let events: Vec<_> = logs
        .iter()
        .map(|log| {
            assert_eq!(log.address, GEM_ADDRESS);
            decode_canonical_body_event(log.address, &log.data)
                .unwrap()
                .unwrap()
        })
        .collect();
    let parent = BTreeMap::from([
        (EntityRef::Gem(item.gem_id), None),
        (EntityRef::GemBucket(group.entity_id()), None),
    ]);
    assert!(reconstruct_effective_final_mutations(&events, &parent)
        .unwrap()
        .is_empty());
    let mut stored = crate::runtime::GemBodyStored::decode_log_data(&logs[0].data).unwrap();
    stored.newCommitment = B256::with_last_byte(1);
    assert!(decode_canonical_body_event(GEM_ADDRESS, &stored.encode_log_data()).is_err());
    stored.schemaVersion = 2;
    assert!(decode_canonical_body_event(GEM_ADDRESS, &stored.encode_log_data()).is_err());
    let mut stored = crate::runtime::GemBucketBodyStored::decode_log_data(&logs[1].data).unwrap();
    stored.schemaVersion = 2;
    assert!(decode_canonical_body_event(GEM_ADDRESS, &stored.encode_log_data()).is_err());
}

/// One reward Gem delivery mints a Gem body, its owner index record and, for the first
/// recipient of a day, the bucket body. The system envelope reserve must cover the
/// end-block cleanup prepaid on those first touches plus a bucket read.
#[test]
fn reward_gem_delivery_reserve_covers_one_item_and_bucket_mint() {
    use crate::runtime::{READ_FIXED_GAS, READ_GAS_PER_CANONICAL_BYTE};
    use crate::state::{
        BODY_TOUCHED_LENGTH_CLEANUP_GAS, FIRST_BODY_TOUCH_CLEANUP_GAS,
        FIRST_INDEX_TOUCH_CLEANUP_GAS, INDEX_TOUCHED_LENGTH_CLEANUP_GAS,
    };
    use outbe_primitives::system_tx::REWARD_GEM_DELIVERY_CE_GAS_RESERVE;

    let bucket_read = READ_FIXED_GAS
        + READ_GAS_PER_CANONICAL_BYTE * crate::state::MAX_STORED_BODY_BYTES_V1 as u64;
    let one_delivery = 2 * FIRST_BODY_TOUCH_CLEANUP_GAS
        + FIRST_INDEX_TOUCH_CLEANUP_GAS
        + BODY_TOUCHED_LENGTH_CLEANUP_GAS
        + INDEX_TOUCHED_LENGTH_CLEANUP_GAS
        + bucket_read;
    assert!(
        one_delivery <= REWARD_GEM_DELIVERY_CE_GAS_RESERVE,
        "one reward delivery needs {one_delivery} CE gas, reserve is {REWARD_GEM_DELIVERY_CE_GAS_RESERVE}"
    );
}
