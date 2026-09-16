use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolCall, SolEvent, SolValue};
use commonware_codec::Encode;
use commonware_cryptography::bls12381::primitives::{
    group::Private,
    ops::{self, sign_message},
    variant::MinSig,
};
use outbe_primitives::addresses::L2_REGISTRY_ADDRESS;
use outbe_primitives::chain::{DEVNET_CHAIN_ID, MAINNET_CHAIN_ID, TESTNET_CHAIN_ID};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::api::{check_zk_merkle_root_signature, ZkOfferCheck, ZK_MERKLE_ROOT_NAMESPACE};
use crate::precompile;
use crate::schema::L2RegistryContract;

const CHAIN_ID: u64 = 1;
const L2_CHAIN_ID: u64 = 0xdead;

sol! {
    interface UnauthorizedL2Registration {
        function registerNetwork(uint64 chainId, address l1Address, bytes publicKey) external;
    }
}

fn l1_addr() -> Address {
    Address::repeat_byte(0x11)
}

fn keypair() -> (Private, Vec<u8>) {
    let (private, public) = ops::keypair::<_, MinSig>(&mut rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let public = public.encode().to_vec();
    (private, public)
}

fn revert_message(err: PrecompileError) -> String {
    match err {
        PrecompileError::Revert(msg) => msg,
        other => panic!("expected revert, got {other:?}"),
    }
}

#[test]
fn register_and_owner_remove_roundtrip() {
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
        assert_eq!(registry.l1_to_chain.read(&l1_addr()).unwrap(), L2_CHAIN_ID);

        let query = precompile::IL2Registry::getNetworkCall {
            chainId: L2_CHAIN_ID,
        };
        let response =
            precompile::dispatch(storage.clone(), &query.abi_encode(), l1_addr(), U256::ZERO)
                .unwrap();
        assert_eq!(
            response.as_ref(),
            (l1_addr(), Bytes::copy_from_slice(&public))
                .abi_encode_params()
                .as_slice(),
        );

        registry.remove_network(l1_addr(), L2_CHAIN_ID).unwrap();
        assert!(!registry.networks.exists(L2_CHAIN_ID).unwrap());
        assert_eq!(registry.l1_to_chain.read(&l1_addr()).unwrap(), 0);

        // The L1 address is free for a fresh registration.
        registry
            .register_network(L2_CHAIN_ID, l1_addr(), &public)
            .unwrap();
        assert_eq!(
            registry.load_network(L2_CHAIN_ID).unwrap().l1_address,
            l1_addr()
        );
    });
}

#[test]
fn registration_publishes_operator_and_key() {
    let (_, public) = keypair();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        L2RegistryContract::new(storage)
            .register_network(L2_CHAIN_ID, l1_addr(), &public)
            .unwrap();
    });

    let event = provider
        .get_events(L2_REGISTRY_ADDRESS)
        .iter()
        .find_map(|log| precompile::IL2Registry::L2NetworkRegistered::decode_log_data(log).ok())
        .expect("registration event");
    assert_eq!(event.chainId, L2_CHAIN_ID);
    assert_eq!(event.l1Address, l1_addr());
    assert_eq!(event.publicKey.as_ref(), public.as_slice());
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
            .register_network(L2_CHAIN_ID, l1_addr(), &public[..95])
            .unwrap_err();
        assert!(revert_message(err).contains("96 bytes"));

        let err = registry
            .register_network(L2_CHAIN_ID, l1_addr(), &[0xAB; 96])
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
fn zero_chain_id_is_rejected_on_every_host_network() {
    let (_, public) = keypair();
    for host in [DEVNET_CHAIN_ID, TESTNET_CHAIN_ID, MAINNET_CHAIN_ID, 31_337] {
        let mut provider = HashMapStorageProvider::new(host);
        StorageHandle::enter(&mut provider, |storage| {
            let mut registry = L2RegistryContract::new(storage.clone());
            let error = registry
                .register_network(0, l1_addr(), &public)
                .unwrap_err();
            assert!(matches!(error, PrecompileError::Revert(_)));
            assert!(!registry.networks.exists(0).unwrap());
            assert_eq!(registry.l1_to_chain.read(&l1_addr()).unwrap(), 0);
            registry
                .register_network(L2_CHAIN_ID, l1_addr(), &public)
                .unwrap();
            assert_eq!(
                registry.load_network(L2_CHAIN_ID).unwrap().l1_address,
                l1_addr()
            );
        });
    }
}

#[test]
fn owner_remove_requires_registration() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let mut registry = L2RegistryContract::new(storage.clone());
        let err = registry.remove_network(l1_addr(), L2_CHAIN_ID).unwrap_err();
        assert!(revert_message(err).contains("not registered"));
    });
}

#[test]
fn registration_requires_governance() {
    let (_, public) = keypair();
    let call = UnauthorizedL2Registration::registerNetworkCall {
        chainId: L2_CHAIN_ID,
        l1Address: l1_addr(),
        publicKey: Bytes::from(public),
    }
    .abi_encode();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        precompile::dispatch(
            storage.clone(),
            &call,
            Address::repeat_byte(0xaa),
            U256::ZERO,
        )
        .unwrap_err();
        assert!(!L2RegistryContract::new(storage)
            .networks
            .exists(L2_CHAIN_ID)
            .unwrap());
    });
}

#[test]
fn public_remove_rejects_non_owner_without_effects() {
    let (_, public) = keypair();
    let stranger = Address::repeat_byte(0x22);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        L2RegistryContract::new(storage.clone())
            .register_network(L2_CHAIN_ID, l1_addr(), &public)
            .unwrap();

        let call = precompile::IL2Registry::removeNetworkCall {
            chainId: L2_CHAIN_ID,
        };
        let err = precompile::dispatch(storage.clone(), &call.abi_encode(), stranger, U256::ZERO)
            .unwrap_err();

        assert!(revert_message(err).contains("owner"));
        let registry = L2RegistryContract::new(storage);
        assert_eq!(
            registry.load_network(L2_CHAIN_ID).unwrap().l1_address,
            l1_addr()
        );
        assert_eq!(registry.l1_to_chain.read(&l1_addr()).unwrap(), L2_CHAIN_ID);
    });
}

#[test]
fn public_remove_allows_owner_and_replay_is_not_registered() {
    let (_, public) = keypair();
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut provider, |storage| {
        L2RegistryContract::new(storage.clone())
            .register_network(L2_CHAIN_ID, l1_addr(), &public)
            .unwrap();

        let call = precompile::IL2Registry::removeNetworkCall {
            chainId: L2_CHAIN_ID,
        };
        precompile::dispatch(storage.clone(), &call.abi_encode(), l1_addr(), U256::ZERO).unwrap();

        let registry = L2RegistryContract::new(storage.clone());
        assert!(!registry.networks.exists(L2_CHAIN_ID).unwrap());
        assert_eq!(registry.l1_to_chain.read(&l1_addr()).unwrap(), 0);

        let err =
            precompile::dispatch(storage, &call.abi_encode(), l1_addr(), U256::ZERO).unwrap_err();
        assert!(revert_message(err).contains("not registered"));
    });
}

#[test]
fn zk_signature_check_paths() {
    let (private, public) = keypair();
    let root = [0x42; 32];
    let good_sig = sign_message::<MinSig>(&private, ZK_MERKLE_ROOT_NAMESPACE, &root)
        .encode()
        .to_vec();

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        // An unregistered chain is reported to the admission gate.
        assert_eq!(
            check_zk_merkle_root_signature(storage.clone(), L2_CHAIN_ID, &root, &good_sig).unwrap(),
            ZkOfferCheck::NotRegistered
        );

        let mut registry = L2RegistryContract::new(storage.clone());
        registry
            .register_network(L2_CHAIN_ID, l1_addr(), &public)
            .unwrap();

        // The signature is checked against the selected network's key.
        assert_eq!(
            check_zk_merkle_root_signature(storage.clone(), L2_CHAIN_ID, &root, &good_sig).unwrap(),
            ZkOfferCheck::Verified {
                chain_id: L2_CHAIN_ID
            }
        );
        assert_eq!(
            check_zk_merkle_root_signature(storage.clone(), L2_CHAIN_ID + 1, &root, &good_sig)
                .unwrap(),
            ZkOfferCheck::NotRegistered
        );

        // Empty root.
        let err = check_zk_merkle_root_signature(storage.clone(), L2_CHAIN_ID, &[], &good_sig)
            .unwrap_err();
        assert!(revert_message(err).contains("exactly 32 bytes"));

        // Malformed signature bytes.
        let err = check_zk_merkle_root_signature(storage.clone(), L2_CHAIN_ID, &root, &[0x01; 8])
            .unwrap_err();
        assert!(revert_message(err).contains("invalid BLS signature"));

        // Signature over a different message.
        let wrong_sig = sign_message::<MinSig>(&private, ZK_MERKLE_ROOT_NAMESPACE, &[0x24; 32])
            .encode()
            .to_vec();
        let err = check_zk_merkle_root_signature(storage.clone(), L2_CHAIN_ID, &root, &wrong_sig)
            .unwrap_err();
        assert!(revert_message(err).contains("invalid BLS signature"));

        // Another registered network's signature cannot authenticate this chain.
        let (other_private, other_public) = keypair();
        registry
            .register_network(L2_CHAIN_ID + 1, Address::repeat_byte(0x22), &other_public)
            .unwrap();
        let foreign_sig = sign_message::<MinSig>(&other_private, ZK_MERKLE_ROOT_NAMESPACE, &root)
            .encode()
            .to_vec();
        let err = check_zk_merkle_root_signature(storage.clone(), L2_CHAIN_ID, &root, &foreign_sig)
            .unwrap_err();
        assert!(revert_message(err).contains("invalid BLS signature"));
        assert_eq!(
            check_zk_merkle_root_signature(storage, L2_CHAIN_ID + 1, &root, &foreign_sig).unwrap(),
            ZkOfferCheck::Verified {
                chain_id: L2_CHAIN_ID + 1
            }
        );
    });
}
