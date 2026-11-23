use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::SolError;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::time::WorldwideDay;

use super::CHAIN_ID;
use crate::schema::TributeFactoryContract;

#[test]
fn su_hash_can_only_be_used_once() {
    use crate::schema::SU_HASH_RETAIN;

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    let events_before = storage.events.clone();
    let ordered_before = storage.get_ordered_events().to_vec();
    StorageHandle::enter(&mut storage, |storage| {
        let expect_reused = |err: PrecompileError, _expected_hash: B256| {
            assert_eq!(
                err.to_string(),
                PrecompileError::from(crate::errors::TributeFactoryError::SuHashAlreadyUsed)
                    .to_string()
            );
        };

        // Distinct hashes across the array all succeed and populate the retention ring.
        let (a, b, c) = (
            B256::repeat_byte(0xAA),
            B256::repeat_byte(0xBB),
            B256::repeat_byte(0xDD),
        );
        let mut factory = TributeFactoryContract::new(storage.clone());
        factory
            .mark_su_hashes_used(&[a, b])
            .expect("distinct hashes ok");
        assert_eq!(factory.su_hash_ring_seq.read().unwrap(), 2);
        assert_eq!(factory.su_hash_ring.read(&0).unwrap(), a);
        assert_eq!(factory.su_hash_ring.read(&1).unwrap(), b);

        // The factory rejects reuse in a later call inside the retention window.
        expect_reused(factory.mark_su_hashes_used(&[a]).unwrap_err(), a);

        // The factory rejects a duplicate within a single array.
        let dup = B256::repeat_byte(0xCC);
        expect_reused(
            storage
                .with_checkpoint(|| {
                    TributeFactoryContract::new(storage.clone()).mark_su_hashes_used(&[dup, dup])
                })
                .unwrap_err(),
            dup,
        );

        // Near the wrap point (seq = SU_HASH_RETAIN), a multi-hash batch `[c, a]`
        // rejects `a` even though `c` would evict ring slot 0 (`a`) during phase 2.
        factory.su_hash_ring_seq.write(SU_HASH_RETAIN).unwrap();
        expect_reused(
            storage
                .with_checkpoint(|| {
                    TributeFactoryContract::new(storage.clone()).mark_su_hashes_used(&[c, a])
                })
                .unwrap_err(),
            a,
        );
        assert!(factory.has_used_su_hash(a).unwrap());
        assert!(!factory.has_used_su_hash(c).unwrap());

        // Wrapping at `seq = SU_HASH_RETAIN` (the (SU_HASH_RETAIN + 1)-th entry)
        // evicts the first hash (`a` at index 0) while keeping `b` and `c` live.
        factory
            .mark_su_hashes_used(&[c])
            .expect("wrap evicts oldest hash");
        assert!(!factory.has_used_su_hash(a).unwrap());
        assert!(factory.has_used_su_hash(b).unwrap());
        assert!(factory.has_used_su_hash(c).unwrap());
        assert_eq!(factory.su_hash_ring_seq.read().unwrap(), SU_HASH_RETAIN + 1);
        assert_eq!(factory.su_hash_ring.read(&0).unwrap(), c);

        // Self-eviction is skipped when the ring entry at `seq % SU_HASH_RETAIN`
        // already matches the incoming hash.
        let self_hash = B256::repeat_byte(0xEE);
        factory
            .su_hash_prune_ring()
            .seed_for_test_with_retain::<SU_HASH_RETAIN>(SU_HASH_RETAIN, self_hash)
            .unwrap();
        factory.mark_su_hashes_used(&[self_hash]).unwrap();
        assert!(factory.has_used_su_hash(self_hash).unwrap());
        assert_eq!(factory.su_hash_ring_seq.read().unwrap(), SU_HASH_RETAIN + 1);

        // Cursor overflow at u64::MAX reverts with SuHashRingSeqOverflow and rolls back.
        factory.su_hash_ring_seq.write(u64::MAX).unwrap();
        let ring_tail_idx = u64::MAX % SU_HASH_RETAIN;
        let ring_tail_before = factory.su_hash_ring.read(&ring_tail_idx).unwrap();
        let overflow = storage
            .with_checkpoint(|| {
                TributeFactoryContract::new(storage.clone()).mark_su_hashes_used(&[a])
            })
            .unwrap_err();
        let expected_overflow =
            crate::precompile::ITributeFactory::SuHashRingSeqOverflow {}.abi_encode();
        assert!(
            matches!(&overflow, PrecompileError::RevertBytes(bytes) if bytes.as_ref() == expected_overflow),
            "unexpected overflow error: {overflow:?}"
        );
        assert!(!factory.has_used_su_hash(a).unwrap());
        assert_eq!(factory.su_hash_ring_seq.read().unwrap(), u64::MAX);
        assert_eq!(
            factory.su_hash_ring.read(&ring_tail_idx).unwrap(),
            ring_tail_before
        );
    });
    assert_eq!(storage.events, events_before);
    assert_eq!(storage.get_ordered_events(), ordered_before);
}

#[test]
fn test_storage_dsl_layout() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let factory = TributeFactoryContract::new(storage.clone());
        assert_eq!(factory.used_su_hashes.base_slot(), U256::ZERO);
        assert_eq!(factory.su_hash_ring.base_slot(), U256::from(1));
        assert_eq!(factory.su_hash_ring_seq.slot(), U256::from(2));
    });
}

#[test]
fn currency_price_unavailable_encodes_selector() {
    let wwd = WorldwideDay::new(20261001);
    let err: PrecompileError =
        crate::errors::TributeFactoryError::CurrencyPriceUnavailable { worldwide_day: wwd }.into();
    let expected_bytes: Bytes = crate::precompile::ITributeFactory::CurrencyPriceUnavailable {
        worldwideDay: wwd.value(),
    }
    .abi_encode()
    .into();
    assert!(matches!(err, PrecompileError::RevertBytes(bytes) if bytes == expected_bytes));
}
