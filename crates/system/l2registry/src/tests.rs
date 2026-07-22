use alloy_primitives::Address;
use commonware_codec::Encode;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_math::algebra::Random;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api::{check_zk_merkle_root_signature, ZkOfferCheck, ZK_MERKLE_ROOT_NAMESPACE};
use crate::schema::L2RegistryContract;

const CHAIN_ID: u64 = 1;
const L2_CHAIN_ID: u64 = 4242;

fn l1_addr() -> Address {
    Address::repeat_byte(0x11)
}

fn keypair() -> (bls12381::PrivateKey, Vec<u8>) {
    let private = bls12381::PrivateKey::random(rand_core::OsRng);
    let public = private.public_key().encode().to_vec();
    (private, public)
}

fn revert_message(err: PrecompileError) -> String {
    match err {
        PrecompileError::Revert(msg) => msg,
        other => panic!("expected revert, got {other:?}"),
    }
}

#[test]
fn register_toggle_remove_roundtrip() {
    let (_, public) = keypair();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut registry = L2RegistryContract::new(storage.clone());
        registry
            .register_network(L2_CHAIN_ID, l1_addr(), &public)
            .unwrap();

        let record = registry.load_network(L2_CHAIN_ID).unwrap();
        assert_eq!(record.l1_address, l1_addr());
        assert_eq!(record.public_key_bytes().as_slice(), public.as_slice());
        assert!(!record.zk_enabled);
        assert_eq!(registry.l1_to_chain.read(&l1_addr()).unwrap(), L2_CHAIN_ID);

        registry.set_zk_enabled(L2_CHAIN_ID, true).unwrap();
        assert!(registry.load_network(L2_CHAIN_ID).unwrap().zk_enabled);
        registry.set_zk_enabled(L2_CHAIN_ID, false).unwrap();
        assert!(!registry.load_network(L2_CHAIN_ID).unwrap().zk_enabled);

        registry.remove_network(L2_CHAIN_ID).unwrap();
        assert!(!registry.networks.exists(L2_CHAIN_ID).unwrap());
        assert_eq!(registry.l1_to_chain.read(&l1_addr()).unwrap(), 0);

        // The l1 address is free for a fresh registration after removal.
        registry
            .register_network(L2_CHAIN_ID + 1, l1_addr(), &public)
            .unwrap();
    });
}

#[test]
fn register_rejects_invalid_inputs() {
    let (_, public) = keypair();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut registry = L2RegistryContract::new(storage.clone());

        let err = registry
            .register_network(0, l1_addr(), &public)
            .unwrap_err();
        assert!(revert_message(err).contains("chain id"));

        let err = registry
            .register_network(L2_CHAIN_ID, Address::ZERO, &public)
            .unwrap_err();
        assert!(revert_message(err).contains("l1 address"));

        let err = registry
            .register_network(L2_CHAIN_ID, l1_addr(), &public[..47])
            .unwrap_err();
        assert!(revert_message(err).contains("48 bytes"));

        let err = registry
            .register_network(L2_CHAIN_ID, l1_addr(), &[0xAB; 48])
            .unwrap_err();
        assert!(revert_message(err).contains("group element"));
    });
}

#[test]
fn register_rejects_duplicates() {
    let (_, public) = keypair();
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut registry = L2RegistryContract::new(storage.clone());
        registry
            .register_network(L2_CHAIN_ID, l1_addr(), &public)
            .unwrap();

        let err = registry
            .register_network(L2_CHAIN_ID, Address::repeat_byte(0x22), &public)
            .unwrap_err();
        assert!(revert_message(err).contains("already registered"));

        let err = registry
            .register_network(L2_CHAIN_ID + 1, l1_addr(), &public)
            .unwrap_err();
        assert!(revert_message(err).contains("already registered"));
    });
}

#[test]
fn toggle_and_remove_require_registration() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut registry = L2RegistryContract::new(storage.clone());
        let err = registry.set_zk_enabled(L2_CHAIN_ID, true).unwrap_err();
        assert!(revert_message(err).contains("not registered"));
        let err = registry.remove_network(L2_CHAIN_ID).unwrap_err();
        assert!(revert_message(err).contains("not registered"));
    });
}

#[test]
fn zk_signature_check_paths() {
    let (private, public) = keypair();
    let root = b"zk-merkle-root".to_vec();
    let good_sig = private
        .sign(ZK_MERKLE_ROOT_NAMESPACE, &root)
        .encode()
        .to_vec();

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        // Unregistered caller: no check applies.
        assert_eq!(
            check_zk_merkle_root_signature(storage.clone(), l1_addr(), &root, &good_sig).unwrap(),
            ZkOfferCheck::NotRegistered
        );

        let mut registry = L2RegistryContract::new(storage.clone());
        registry
            .register_network(L2_CHAIN_ID, l1_addr(), &public)
            .unwrap();

        // Registered, zk disabled: signature is not checked.
        assert_eq!(
            check_zk_merkle_root_signature(storage.clone(), l1_addr(), &root, &[]).unwrap(),
            ZkOfferCheck::Disabled {
                chain_id: L2_CHAIN_ID
            }
        );

        let mut registry = L2RegistryContract::new(storage.clone());
        registry.set_zk_enabled(L2_CHAIN_ID, true).unwrap();

        // Enabled + valid signature.
        assert_eq!(
            check_zk_merkle_root_signature(storage.clone(), l1_addr(), &root, &good_sig).unwrap(),
            ZkOfferCheck::Verified {
                chain_id: L2_CHAIN_ID
            }
        );

        // Enabled + empty root.
        let err =
            check_zk_merkle_root_signature(storage.clone(), l1_addr(), &[], &good_sig).unwrap_err();
        assert!(revert_message(err).contains("zkMerkleRoot is required"));

        // Enabled + malformed signature bytes.
        let err = check_zk_merkle_root_signature(storage.clone(), l1_addr(), &root, &[0x01; 8])
            .unwrap_err();
        assert!(revert_message(err).contains("invalid BLS signature"));

        // Enabled + signature over a different message.
        let wrong_sig = private
            .sign(ZK_MERKLE_ROOT_NAMESPACE, b"other-root")
            .encode()
            .to_vec();
        let err = check_zk_merkle_root_signature(storage.clone(), l1_addr(), &root, &wrong_sig)
            .unwrap_err();
        assert!(revert_message(err).contains("invalid BLS signature"));

        // Enabled + signature by a different key.
        let (other_private, _) = keypair();
        let foreign_sig = other_private
            .sign(ZK_MERKLE_ROOT_NAMESPACE, &root)
            .encode()
            .to_vec();
        let err = check_zk_merkle_root_signature(storage.clone(), l1_addr(), &root, &foreign_sig)
            .unwrap_err();
        assert!(revert_message(err).contains("invalid BLS signature"));
    });
}
