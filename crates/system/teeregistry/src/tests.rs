use alloy_primitives::B256;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::runtime::TeeBootstrapData;
use crate::schema::TeeRegistry;

const CHAIN_ID: u64 = 1;

fn sample_data() -> TeeBootstrapData {
    TeeBootstrapData {
        tribute_offer_public_key: B256::repeat_byte(0xAA),
        policy_hash: B256::repeat_byte(0xBB),
        key_epoch: 0,
        tribute_offer_epoch: 0,
        dkg_transcript_hash: B256::repeat_byte(0xCC),
        committee_snapshot_block: 1,
        committee_snapshot_hash: B256::repeat_byte(0xDD),
        tribute_offer_group_public_key: alloy_primitives::Bytes::from(vec![0xEE; 96]),
    }
}

fn with_registry(f: impl FnOnce(&mut TeeRegistry<'_>)) {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        let mut registry = TeeRegistry::new(storage.clone());
        f(&mut registry);
    });
}

#[test]
fn bootstrap_writes_and_reads_back() {
    with_registry(|reg| {
        assert!(!reg.is_bootstrapped().unwrap());

        let data = sample_data();
        reg.write_bootstrap(&data).unwrap();

        assert!(reg.is_bootstrapped().unwrap());
        assert_eq!(
            reg.offer_public_key().unwrap(),
            data.tribute_offer_public_key
        );
        assert_eq!(reg.policy_hash().unwrap(), data.policy_hash);
        assert_eq!(reg.key_epoch().unwrap(), data.key_epoch);
        assert_eq!(reg.tribute_offer_epoch().unwrap(), data.tribute_offer_epoch);
        assert_eq!(reg.group_public_key_len.read().unwrap(), 96);
        for index in 0..3 {
            assert_eq!(
                reg.group_public_key.read(&index).unwrap(),
                B256::repeat_byte(0xEE)
            );
        }
    });
}

#[test]
fn bootstrap_is_idempotent_reject() {
    with_registry(|reg| {
        reg.write_bootstrap(&sample_data()).unwrap();
        // A second bootstrap must be rejected (registry no longer empty).
        assert!(reg.write_bootstrap(&sample_data()).is_err());
    });
}

#[test]
fn empty_registry_reads_zero() {
    with_registry(|reg| {
        assert!(!reg.is_bootstrapped().unwrap());
        assert_eq!(reg.offer_public_key().unwrap(), B256::ZERO);
    });
}
