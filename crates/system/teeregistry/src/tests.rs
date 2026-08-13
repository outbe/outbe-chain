use alloy_primitives::{Address, B256};
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

#[test]
fn bootstrap_writes_and_reads_back() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut reg = TeeRegistry::new(storage.clone());
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
        assert_eq!(
            reg.prior_group_public_key().unwrap(),
            data.tribute_offer_group_public_key
        );
    });
}

/// A reshare accepts only the exact enclave keys already bound to each
/// validator's role-neutral NodeHost and preserves bootstrap authority.
#[test]
fn reshare_registrations_validate_bindings_and_preserve_offer_key() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut reg = TeeRegistry::new(storage.clone());
        reg.write_bootstrap(&sample_data()).unwrap();
        let offer_before = reg.offer_public_key().unwrap();
        let v_stay = Address::repeat_byte(0x11);
        let v_new = Address::repeat_byte(0x13);
        let stay_node = B256::repeat_byte(0x81);
        let new_node = B256::repeat_byte(0x82);
        reg.validator_v1_node_hash
            .write(&v_stay, stay_node)
            .unwrap();
        reg.validator_v1_node_hash.write(&v_new, new_node).unwrap();
        reg.v1_node_recipient_x25519
            .write(&stay_node, B256::repeat_byte(0x91))
            .unwrap();
        reg.v1_node_attestation_ed25519
            .write(&stay_node, B256::repeat_byte(0x92))
            .unwrap();
        reg.v1_node_noise_responder_x25519
            .write(&stay_node, B256::repeat_byte(0x93))
            .unwrap();
        reg.v1_node_recipient_x25519
            .write(&new_node, B256::repeat_byte(0xA1))
            .unwrap();
        reg.v1_node_attestation_ed25519
            .write(&new_node, B256::repeat_byte(0xA2))
            .unwrap();
        reg.v1_node_noise_responder_x25519
            .write(&new_node, B256::repeat_byte(0xA3))
            .unwrap();
        let regs = [
            (
                v_stay,
                B256::repeat_byte(0x91),
                B256::repeat_byte(0x92),
                B256::repeat_byte(0x93),
            ),
            (
                v_new,
                B256::repeat_byte(0xA1),
                B256::repeat_byte(0xA2),
                B256::repeat_byte(0xA3),
            ),
        ];
        reg.record_reshare_registrations(&regs).unwrap();
        assert!(reg
            .record_reshare_registrations(&[(
                v_stay,
                B256::repeat_byte(0xFF),
                B256::repeat_byte(0x92),
                B256::repeat_byte(0x93),
            )])
            .is_err());
        assert_eq!(reg.offer_public_key().unwrap(), offer_before);
        assert!(reg.is_bootstrapped().unwrap());
    });
}

#[test]
fn bootstrap_is_idempotent_reject() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut reg = TeeRegistry::new(storage.clone());
        reg.write_bootstrap(&sample_data()).unwrap();
        // A second bootstrap must be rejected (registry no longer empty).
        assert!(reg.write_bootstrap(&sample_data()).is_err());
    });
}

#[test]
fn empty_registry_reads_zero() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let reg = TeeRegistry::new(storage.clone());
        assert!(!reg.is_bootstrapped().unwrap());
        assert_eq!(reg.offer_public_key().unwrap(), B256::ZERO);
    });
}

#[test]
fn boundary_recipient_keys_recorded_and_overwritten() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut reg = TeeRegistry::new(storage.clone());
        let val_a = Address::repeat_byte(0x11);
        let val_b = Address::repeat_byte(0x12);

        // Unannounced validators read zero.
        assert_eq!(reg.announced_recipient_key(val_a).unwrap(), B256::ZERO);

        reg.record_boundary_recipient_keys(&[
            (val_a, B256::repeat_byte(0xA1)),
            (val_b, B256::repeat_byte(0xB1)),
        ])
        .unwrap();
        assert_eq!(
            reg.announced_recipient_key(val_a).unwrap(),
            B256::repeat_byte(0xA1)
        );
        assert_eq!(
            reg.announced_recipient_key(val_b).unwrap(),
            B256::repeat_byte(0xB1)
        );

        // Latest announcement wins (key rotation).
        reg.record_boundary_recipient_keys(&[(val_a, B256::repeat_byte(0xA2))])
            .unwrap();
        assert_eq!(
            reg.announced_recipient_key(val_a).unwrap(),
            B256::repeat_byte(0xA2)
        );
        // Untouched validator keeps its prior announcement.
        assert_eq!(
            reg.announced_recipient_key(val_b).unwrap(),
            B256::repeat_byte(0xB1)
        );
    });
}

#[test]
fn boundary_recipient_keys_are_independent_of_node_host_binding() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut reg = TeeRegistry::new(storage.clone());
        let validator = Address::repeat_byte(0x11);

        // A boundary announcement does not bootstrap the registry or create a
        // validator-to-NodeHost association.
        reg.record_boundary_recipient_keys(&[(validator, B256::repeat_byte(0xA1))])
            .unwrap();
        assert!(!reg.is_bootstrapped().unwrap());
        assert_eq!(
            reg.validator_v1_node_hash.read(&validator).unwrap(),
            B256::ZERO
        );
        assert_eq!(
            reg.announced_recipient_key(validator).unwrap(),
            B256::repeat_byte(0xA1)
        );
    });
}
