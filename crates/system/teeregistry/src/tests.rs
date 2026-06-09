use alloy_primitives::{Address, B256};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::runtime::{TeeBootstrapData, TeeRegistration};
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
        registrations: vec![
            TeeRegistration {
                validator: Address::repeat_byte(0x11),
                recipient_x25519: B256::repeat_byte(0x21),
                attestation_pub: B256::repeat_byte(0x22),
                noise_static_pub: B256::repeat_byte(0x23),
                mrenclave: B256::repeat_byte(0x24),
                mrsigner: B256::repeat_byte(0x25),
                isv_svn: 3,
                keys_hash: B256::repeat_byte(0x26),
            },
            TeeRegistration {
                validator: Address::repeat_byte(0x12),
                recipient_x25519: B256::repeat_byte(0x31),
                attestation_pub: B256::repeat_byte(0x32),
                noise_static_pub: B256::repeat_byte(0x33),
                mrenclave: B256::repeat_byte(0x34),
                mrsigner: B256::repeat_byte(0x35),
                isv_svn: 4,
                keys_hash: B256::repeat_byte(0x36),
            },
        ],
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
        assert_eq!(reg.registered_count.read().unwrap(), 2);

        for expected in &data.registrations {
            assert_eq!(reg.registration(expected.validator).unwrap(), *expected);
        }
    });
}

/// R5.2: a reshare re-registration rewrites the per-validator enclave keys
/// (recipient_x25519 / attestation_pub / noise_static_pub) for the new committee
/// while PRESERVING the offer key, the bootstrapped flag, and the snapshot slots.
#[test]
fn reshare_registrations_rewrite_keys_and_preserve_offer_key() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut reg = TeeRegistry::new(storage.clone());
        reg.write_bootstrap(&sample_data()).unwrap();
        let offer_before = reg.offer_public_key().unwrap();
        let count_before = reg.registered_count.read().unwrap();

        // Reshare: rotate keys for the existing validator 0x11 and add a fresh
        // validator 0x13 (new committee member).
        let v_stay = Address::repeat_byte(0x11);
        let v_new = Address::repeat_byte(0x13);
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

        // Per-validator keys updated for both the staying and the new validator.
        let stay = reg.registration(v_stay).unwrap();
        assert_eq!(stay.recipient_x25519, B256::repeat_byte(0x91));
        assert_eq!(stay.attestation_pub, B256::repeat_byte(0x92));
        assert_eq!(stay.noise_static_pub, B256::repeat_byte(0x93));
        let new = reg.registration(v_new).unwrap();
        assert_eq!(new.recipient_x25519, B256::repeat_byte(0xA1));
        assert_eq!(new.attestation_pub, B256::repeat_byte(0xA2));
        assert_eq!(new.noise_static_pub, B256::repeat_byte(0xA3));

        // Offer key, bootstrapped flag, and registered_count are untouched.
        assert_eq!(reg.offer_public_key().unwrap(), offer_before);
        assert!(reg.is_bootstrapped().unwrap());
        assert_eq!(reg.registered_count.read().unwrap(), count_before);
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
fn boundary_recipient_keys_are_independent_of_registration() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut reg = TeeRegistry::new(storage.clone());
        let validator = Address::repeat_byte(0x11);

        // A boundary announcement does not bootstrap the registry nor populate
        // the authoritative registration `recipient_x25519` (slot 9).
        reg.record_boundary_recipient_keys(&[(validator, B256::repeat_byte(0xA1))])
            .unwrap();
        assert!(!reg.is_bootstrapped().unwrap());
        assert_eq!(
            reg.registration(validator).unwrap().recipient_x25519,
            B256::ZERO
        );
        assert_eq!(
            reg.announced_recipient_key(validator).unwrap(),
            B256::repeat_byte(0xA1)
        );
    });
}

#[test]
fn register_enclave_writes_keys_and_counts_first_time() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut reg = TeeRegistry::new(storage.clone());
        let validator = Address::repeat_byte(0x41);

        assert_eq!(reg.registered_count.read().unwrap(), 0);

        let ok = reg
            .register_enclave(
                validator,
                B256::repeat_byte(0x51), // recipient_x25519
                B256::repeat_byte(0x52), // attestation_pub
                B256::repeat_byte(0x53), // noise_static_pub
                B256::repeat_byte(0x54), // mrenclave
                B256::repeat_byte(0x55), // mrsigner
                7,                       // isv_svn
            )
            .unwrap();
        assert!(ok);

        let bundle = reg.registration(validator).unwrap();
        assert_eq!(bundle.recipient_x25519, B256::repeat_byte(0x51));
        assert_eq!(bundle.attestation_pub, B256::repeat_byte(0x52));
        assert_eq!(bundle.noise_static_pub, B256::repeat_byte(0x53));
        assert_eq!(bundle.mrenclave, B256::repeat_byte(0x54));
        assert_eq!(bundle.mrsigner, B256::repeat_byte(0x55));
        assert_eq!(bundle.isv_svn, 7);
        assert_ne!(bundle.keys_hash, B256::ZERO);
        // First-time registrant bumps the count.
        assert_eq!(reg.registered_count.read().unwrap(), 1);
    });
}

#[test]
fn register_enclave_reregistration_rotates_without_double_counting() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut reg = TeeRegistry::new(storage.clone());
        let validator = Address::repeat_byte(0x42);

        reg.register_enclave(
            validator,
            B256::repeat_byte(0x61),
            B256::repeat_byte(0x62),
            B256::repeat_byte(0x63),
            B256::repeat_byte(0x64),
            B256::repeat_byte(0x65),
            1,
        )
        .unwrap();
        let first_hash = reg.registration(validator).unwrap().keys_hash;
        assert_eq!(reg.registered_count.read().unwrap(), 1);

        // Re-register the same validator with new key material (rotation).
        reg.register_enclave(
            validator,
            B256::repeat_byte(0x71),
            B256::repeat_byte(0x72),
            B256::repeat_byte(0x73),
            B256::repeat_byte(0x74),
            B256::repeat_byte(0x75),
            2,
        )
        .unwrap();

        let bundle = reg.registration(validator).unwrap();
        assert_eq!(bundle.recipient_x25519, B256::repeat_byte(0x71));
        assert_eq!(bundle.isv_svn, 2);
        assert_ne!(bundle.keys_hash, first_hash);
        // Rotation must NOT double-count the validator.
        assert_eq!(reg.registered_count.read().unwrap(), 1);
    });
}

// Drive the FULL precompile dispatch routing for `registerEnclave`
// (selector parse → `_abi_register_enclave` → `register_enclave` → storage), proving
// an EOA tx with this calldata writes the per-validator registration on-chain.
mod register_enclave_dispatch {
    use super::*;
    use alloy_primitives::U256;
    use alloy_sol_types::{sol, SolCall as _};
    use outbe_primitives::error::PrecompileError;

    sol! {
        // Byte-identical to the precompile's #[contract_public("registerEnclave(...)")].
        function registerEnclave(
            uint256 recipientX25519,
            uint256 attestationPub,
            uint256 noiseStaticPub,
            uint256 mrenclave,
            uint256 mrsigner,
            uint16 isvSvn
        ) external returns (bool);
    }

    fn calldata(seed: u8, isv: u16) -> Vec<u8> {
        registerEnclaveCall {
            recipientX25519: U256::from(0x10 + seed),
            attestationPub: U256::from(0x20 + seed),
            noiseStaticPub: U256::from(0x30 + seed),
            mrenclave: U256::from(0x40 + seed),
            mrsigner: U256::from(0x50 + seed),
            isvSvn: isv,
        }
        .abi_encode()
    }

    #[test]
    fn dispatch_writes_registration_and_returns_true() {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let caller = Address::repeat_byte(0x77);
            let data = calldata(1, 9);

            let ret = crate::precompile::dispatch(storage.clone(), &data, caller, U256::ZERO)
                .expect("dispatch must succeed");
            let decoded = registerEnclaveCall::abi_decode_returns(&ret).expect("decode bool");
            assert!(decoded);

            let reg = TeeRegistry::new(storage);
            let bundle = reg.registration(caller).unwrap();
            assert_eq!(bundle.recipient_x25519, B256::from(U256::from(0x11)));
            assert_eq!(bundle.attestation_pub, B256::from(U256::from(0x21)));
            assert_eq!(bundle.noise_static_pub, B256::from(U256::from(0x31)));
            assert_eq!(bundle.mrenclave, B256::from(U256::from(0x41)));
            assert_eq!(bundle.mrsigner, B256::from(U256::from(0x51)));
            assert_eq!(bundle.isv_svn, 9);
            assert_ne!(bundle.keys_hash, B256::ZERO);
            assert_eq!(reg.registered_count.read().unwrap(), 1);
        });
    }

    #[test]
    fn dispatch_rejects_nonzero_value() {
        let mut storage = HashMapStorageProvider::new(CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let caller = Address::repeat_byte(0x78);
            let data = calldata(2, 3);
            // registerEnclave does not accept native value → the macro's value guard
            // rejects msg.value != 0 before any state write.
            let err = crate::precompile::dispatch(storage.clone(), &data, caller, U256::from(1u64))
                .expect_err("nonzero value must be rejected");
            assert!(matches!(err, PrecompileError::Revert(_)));

            // No registration was written.
            let reg = TeeRegistry::new(storage);
            assert_eq!(
                reg.registration(caller).unwrap().recipient_x25519,
                B256::ZERO
            );
        });
    }
}
