use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::runtime::validate_agent_reward_addresses;
use crate::schema::TributeFactoryContract;

const CHAIN_ID: u64 = 1;

mod l2_zk_gate {
    use alloy_primitives::{Address, Bytes, U256};
    use outbe_compressed_entities::{
        EntityRef, ExecutionScope, IdPage, IdPageRequest, ParentBodySource, ParentBodySourceError,
        QueryRef, StoredBody,
    };
    use outbe_l2registry::L2RegistryContract;
    use outbe_primitives::error::PrecompileError;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_primitives::storage::StorageHandle;

    use crate::runtime::OfferTributeInput;
    use crate::schema::TributeFactoryContract;

    /// The zk gate runs before any finalized-parent read, so an inert source
    /// is enough for these tests.
    struct NoParentBodies;

    impl ParentBodySource for NoParentBodies {
        fn get(
            &self,
            _entity: EntityRef,
        ) -> core::result::Result<Option<StoredBody>, ParentBodySourceError> {
            Ok(None)
        }

        fn list(
            &self,
            _query: QueryRef,
            _request: IdPageRequest,
        ) -> core::result::Result<IdPage, ParentBodySourceError> {
            Ok(IdPage {
                ids: Vec::new(),
                next_after: None,
            })
        }
    }

    const L2_CHAIN_ID: u64 = 4242;

    fn caller() -> Address {
        Address::repeat_byte(0x77)
    }

    fn offer(zk_merkle_root: &[u8], signature: &[u8]) -> OfferTributeInput {
        OfferTributeInput {
            caller: caller(),
            cipher_text: Bytes::new(),
            nonce: Bytes::new(),
            ephemeral_pubkey: U256::ZERO,
            reference_currency: 840,
            exclude_from_intex_issuance: false,
            zk_merkle_root: Bytes::copy_from_slice(zk_merkle_root),
            signature: Bytes::copy_from_slice(signature),
        }
    }

    fn revert_message(err: PrecompileError) -> String {
        match err {
            PrecompileError::Revert(msg) => msg,
            other => panic!("expected revert, got {other:?}"),
        }
    }

    #[test]
    fn offer_rejects_invalid_l2_signature_when_zk_enabled() {
        use commonware_codec::Encode;
        use commonware_cryptography::{bls12381, Signer as _};
        use commonware_math::algebra::Random;

        let private = bls12381::PrivateKey::random(rand_core::OsRng);
        let public = private.public_key().encode().to_vec();
        let root = b"l2-zk-merkle-root".to_vec();

        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let mut registry = L2RegistryContract::new(storage.clone());
            registry
                .register_network(L2_CHAIN_ID, caller(), &public)
                .unwrap();
            registry.set_zk_enabled(L2_CHAIN_ID, true).unwrap();

            let scope = ExecutionScope::new();
            let mut factory = TributeFactoryContract::new(storage.clone());

            // Enabled + missing signature: the gate rejects before any
            // oracle/metadosis/enclave work.
            let err = factory
                .offer_tribute(&scope, &NoParentBodies, offer(&root, &[]))
                .unwrap_err();
            assert!(revert_message(err).contains("invalid BLS signature"));

            // Enabled + valid signature: the gate passes and the offer
            // proceeds to the next stage (no OFFERING day in this fixture).
            let good_sig = private
                .sign(outbe_l2registry::api::ZK_MERKLE_ROOT_NAMESPACE, &root)
                .encode()
                .to_vec();
            let mut factory = TributeFactoryContract::new(storage.clone());
            let err = factory
                .offer_tribute(&scope, &NoParentBodies, offer(&root, &good_sig))
                .unwrap_err();
            assert!(revert_message(err).contains("no worldwide day is OFFERING"));
        });
    }

    #[test]
    fn offer_skips_signature_check_for_unregistered_and_disabled() {
        use commonware_codec::Encode;
        use commonware_cryptography::{bls12381, Signer as _};
        use commonware_math::algebra::Random;

        let private = bls12381::PrivateKey::random(rand_core::OsRng);
        let public = private.public_key().encode().to_vec();

        let mut storage = HashMapStorageProvider::new(super::CHAIN_ID);
        StorageHandle::enter(&mut storage, |storage| {
            let scope = ExecutionScope::new();

            // Unregistered caller with empty zk fields sails past the gate.
            let mut factory = TributeFactoryContract::new(storage.clone());
            let err = factory
                .offer_tribute(&scope, &NoParentBodies, offer(&[], &[]))
                .unwrap_err();
            assert!(revert_message(err).contains("no worldwide day is OFFERING"));

            // Registered but zk disabled: still no signature requirement.
            let mut registry = L2RegistryContract::new(storage.clone());
            registry
                .register_network(L2_CHAIN_ID, caller(), &public)
                .unwrap();
            let mut factory = TributeFactoryContract::new(storage.clone());
            let err = factory
                .offer_tribute(&scope, &NoParentBodies, offer(&[], &[]))
                .unwrap_err();
            assert!(revert_message(err).contains("no worldwide day is OFFERING"));
        });
    }
}

#[test]
fn test_validate_agent_reward_both_empty() {
    assert!(validate_agent_reward_addresses(&[], &[]).is_ok());
}

#[test]
fn test_validate_agent_reward_both_present() {
    let wallets = vec!["0x1111111111111111111111111111111111111111".to_string()];
    let sfas = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate_agent_reward_addresses(&wallets, &sfas).is_ok());
}

#[test]
fn test_validate_agent_reward_wallets_only() {
    let wallets = vec!["0x1111111111111111111111111111111111111111".to_string()];
    assert!(validate_agent_reward_addresses(&wallets, &[]).is_err());
}

#[test]
fn test_validate_agent_reward_sfa_only() {
    let sfas = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate_agent_reward_addresses(&[], &sfas).is_err());
}

#[test]
fn test_validate_agent_reward_invalid_address() {
    let wallets = vec!["not_a_valid_address".to_string()];
    let sfas = vec!["0x2222222222222222222222222222222222222222".to_string()];
    assert!(validate_agent_reward_addresses(&wallets, &sfas).is_err());
}

#[test]
fn test_storage_dsl_layout_is_compatible_with_previous_slots() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |storage| {
        let factory = TributeFactoryContract::new(storage.clone());
        assert_eq!(
            factory.used_su_hashes.base_slot(),
            alloy_primitives::U256::ZERO
        );
    });
}
