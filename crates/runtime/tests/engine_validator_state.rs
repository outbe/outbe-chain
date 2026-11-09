//! Validator-set reads through the production engine protocol state.

use alloy_primitives::{Address, B256, U256};
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_p2p::Address as CommonwareAddress;
use eyre::Result;
use outbe_engine::validators::{
    has_pending_set_change, read_consensus_validators_from_state,
    read_local_tee_runtime_admission_from_state,
    read_reshare_target_with_empty_tee_exclusions_from_state, read_validators_from_state,
    LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeIdentityV1, LocalTeeRuntimeRejectionV1,
    RethStateAccess, ValidatorP2pAddress,
};
use outbe_primitives::consensus_p2p::{encode_v1, P2pAddress, P2P_ADDRESS_VERSION_V1};
use outbe_primitives::storage::{
    hashmap::HashMapStorageProvider, readonly::ReadOnlyBlockContext, StorageHandle,
};
use outbe_primitives::tee_attestation_v1::NodeIdV1;
use outbe_validatorset::test_support::StorageOverrides;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

const OWNER: Address = Address::repeat_byte(0xA0);

fn valid_consensus_pubkey(seed: u8) -> [u8; 48] {
    let key = bls12381::PrivateKey::from_seed(seed as u64);
    let encoded = key.public_key().encode();
    let mut out = [0u8; 48];
    out.copy_from_slice(&encoded[..48]);
    out
}

/// Test implementation of [`RethStateAccess`] backed by a raw storage map.
struct TestStateAccess {
    data: HashMap<(Address, U256), U256>,
}

impl RethStateAccess for TestStateAccess {
    fn storage_read(&self, address: Address, key: B256) -> Result<Option<U256>> {
        let key_u256 = U256::from_be_bytes(key.0);
        Ok(self.data.get(&(address, key_u256)).copied())
    }
}

fn populated_p2p_state(
    p2p_writer: impl FnOnce(&mut outbe_validatorset::contract::ValidatorSet<'_>, Address),
) -> TestStateAccess {
    let validator = Address::with_last_byte(0x11);
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        vs.config_owner.write(OWNER).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.set_config_max_validators(128).unwrap();
        let minimum = alloy_primitives::U256::from(1_000u64);
        vs.test_seed_authoritative_min_stake(minimum).unwrap();
        vs.register_validator(OWNER, validator, &valid_consensus_pubkey(11))
            .unwrap();
        vs.test_seed_authoritative_bonded(validator, minimum)
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        p2p_writer(&mut vs, validator);
    });
    TestStateAccess {
        data: provider.storage.clone(),
    }
}

#[test]
fn frozen_target_uses_validator_lifecycle_and_emits_empty_tee_compatibility_list() {
    let validators = vec![
        Address::with_last_byte(0x21),
        Address::with_last_byte(0x22),
        Address::with_last_byte(0x23),
        Address::with_last_byte(0x24),
    ];
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        vs.config_owner.write(OWNER).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.set_config_max_validators(128).unwrap();
        let minimum = alloy_primitives::U256::from(1_000u64);
        vs.test_seed_authoritative_min_stake(minimum).unwrap();
        for (index, &validator) in validators.iter().enumerate() {
            let consensus_pubkey = valid_consensus_pubkey((index + 1) as u8);
            vs.register_validator(OWNER, validator, &consensus_pubkey)
                .unwrap();
            vs.test_seed_authoritative_bonded(validator, minimum)
                .unwrap();
            vs.activate_validator_via_boundary_for_test(validator)
                .unwrap();
        }
    });
    let access = TestStateAccess {
        data: provider.storage,
    };
    let frozen = read_reshare_target_with_empty_tee_exclusions_from_state(
        &outbe_runtime::OutbeRuntime::new(),
        &access,
    )
    .unwrap();

    assert_eq!(frozen.validator_set.addresses, validators);
    assert!(frozen.tee_expired_target_exclusions.is_empty());
}

fn tee_runtime_admission_state(valid_until: u64) -> (TestStateAccess, [u8; 33], Address, B256) {
    let validator = Address::with_last_byte(0x31);
    let reth_p2p_public: [u8; 33] = k256::ecdsa::SigningKey::from_bytes((&[0x31; 32]).into())
        .unwrap()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let node_hash = NodeIdV1 { reth_p2p_public }.node_id_hash().unwrap();
    let enclave_id = B256::repeat_byte(0x32);
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(9);
    StorageHandle::enter(&mut provider, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.set_config_max_validators(128).unwrap();
        let minimum = alloy_primitives::U256::from(1_000u64);
        vs.test_seed_authoritative_min_stake(minimum).unwrap();
        vs.register_validator(OWNER, validator, &valid_consensus_pubkey(0x31))
            .unwrap();
        vs.test_seed_authoritative_bonded(validator, minimum)
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();

        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        registry
            .validator_v1_node_hash
            .write(&validator, node_hash)
            .unwrap();
        registry
            .v1_node_enclave_id
            .write(&node_hash, enclave_id)
            .unwrap();
        registry
            .v1_node_binding_id
            .write(&node_hash, B256::repeat_byte(0x33))
            .unwrap();
        registry
            .v1_node_intent_hash
            .write(&node_hash, B256::repeat_byte(0x34))
            .unwrap();
        registry
            .v1_node_valid_until
            .write(&node_hash, valid_until)
            .unwrap();
    });
    (
        TestStateAccess {
            data: provider.storage.clone(),
        },
        reth_p2p_public,
        validator,
        enclave_id,
    )
}

#[test]
fn finalized_runtime_admission_requires_exact_live_local_binding() {
    let deadline = 1_800_001_000;
    let (access, reth_p2p_public, validator, enclave_id) = tee_runtime_admission_state(deadline);
    let identity = LocalTeeRuntimeIdentityV1 {
        reth_p2p_public,
        expected_enclave_id: Some(enclave_id),
        validator: Some(validator),
    };

    assert_eq!(
        read_local_tee_runtime_admission_from_state(
            &outbe_runtime::OutbeRuntime::new(),
            &access,
            ReadOnlyBlockContext {
                chain_id: 1,
                genesis_hash: B256::repeat_byte(0x41),
                block_number: 9,
                timestamp: deadline - 1,
                genesis_timestamp: 1_700_000_000,
            },
            identity,
        )
        .unwrap(),
        LocalTeeRuntimeAdmissionV1::Ready {
            valid_until: deadline
        }
    );

    assert_eq!(
        read_local_tee_runtime_admission_from_state(
            &outbe_runtime::OutbeRuntime::new(),
            &access,
            ReadOnlyBlockContext {
                chain_id: 1,
                genesis_hash: B256::repeat_byte(0x41),
                block_number: 9,
                timestamp: deadline,
                genesis_timestamp: 1_700_000_000,
            },
            identity,
        )
        .unwrap(),
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::Expired {
            valid_until: deadline
        })
    );

    let wrong_enclave = LocalTeeRuntimeIdentityV1 {
        expected_enclave_id: Some(B256::repeat_byte(0xFF)),
        ..identity
    };
    assert_eq!(
        read_local_tee_runtime_admission_from_state(
            &outbe_runtime::OutbeRuntime::new(),
            &access,
            ReadOnlyBlockContext {
                chain_id: 1,
                genesis_hash: B256::repeat_byte(0x41),
                block_number: 9,
                timestamp: deadline - 1,
                genesis_timestamp: 1_700_000_000,
            },
            wrong_enclave,
        )
        .unwrap(),
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::EnclaveIdentityMismatch)
    );
}

#[test]
fn finalized_runtime_retires_old_validator_and_full_node_despite_live_lease() {
    let (access, public, validator, enclave_id) = tee_runtime_admission_state(100_000);
    let mut provider = HashMapStorageProvider::new(1);
    provider.storage = access.data;
    provider.enter(|storage| {
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        registry
            .strict_upgrade_proposal
            .write(U256::from(7))
            .unwrap();
        registry.strict_upgrade_height.write(50).unwrap();
        registry
            .strict_upgrade_successor
            .write(B256::repeat_byte(0xe1))
            .unwrap();
        registry
            .strict_upgrade_predecessor
            .write(B256::repeat_byte(0xe2))
            .unwrap();
        let node = NodeIdV1 {
            reth_p2p_public: public,
        }
        .node_id_hash()
        .unwrap();
        registry
            .v1_node_policy_hash
            .write(&node, B256::repeat_byte(0xe2))
            .unwrap();
    });
    let access = TestStateAccess {
        data: provider.storage,
    };
    for role in [None, Some(validator)] {
        let identity = LocalTeeRuntimeIdentityV1 {
            reth_p2p_public: public,
            expected_enclave_id: Some(enclave_id),
            validator: role,
        };
        for height in [49, 50] {
            let result = read_local_tee_runtime_admission_from_state(
                &outbe_runtime::OutbeRuntime::new(),
                &access,
                ReadOnlyBlockContext {
                    chain_id: 1,
                    genesis_hash: B256::repeat_byte(0x41),
                    block_number: height,
                    timestamp: 10_000,
                    genesis_timestamp: 1_700_000_000,
                },
                identity,
            )
            .unwrap();
            assert_eq!(
                result,
                if height == 49 {
                    LocalTeeRuntimeAdmissionV1::Ready {
                        valid_until: 100_000,
                    }
                } else {
                    LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::RetiredEnclave)
                }
            );
        }
    }
}

#[test]
fn finalized_runtime_admission_rejects_jailed_validator_with_live_lease() {
    let deadline = 1_800_001_000;
    let (mut access, reth_p2p_public, validator, enclave_id) =
        tee_runtime_admission_state(deadline);
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(10);
    provider.storage = access.data;
    StorageHandle::enter(&mut provider, |storage| {
        outbe_validatorset::contract::ValidatorSet::new(storage)
            .jail_validator_for_tee_expiry(validator)
            .unwrap();
    });
    access = TestStateAccess {
        data: provider.storage.clone(),
    };

    let admission = read_local_tee_runtime_admission_from_state(
        &outbe_runtime::OutbeRuntime::new(),
        &access,
        ReadOnlyBlockContext {
            chain_id: 1,
            genesis_hash: B256::repeat_byte(0x41),
            block_number: 10,
            timestamp: deadline - 1,
            genesis_timestamp: 1_700_000_000,
        },
        LocalTeeRuntimeIdentityV1 {
            reth_p2p_public,
            expected_enclave_id: Some(enclave_id),
            validator: Some(validator),
        },
    )
    .unwrap();
    assert_eq!(
        admission,
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::ValidatorJailed)
    );
}

#[test]
fn only_block_one_validator_may_wait_for_bootstrap_binding() {
    let access = TestStateAccess {
        data: HashMap::new(),
    };
    let validator = Address::with_last_byte(0x41);
    let identity = LocalTeeRuntimeIdentityV1 {
        reth_p2p_public: [0x02; 33],
        expected_enclave_id: None,
        validator: Some(validator),
    };
    let context = |block_number| ReadOnlyBlockContext {
        chain_id: 1,
        genesis_hash: B256::repeat_byte(0x42),
        block_number,
        timestamp: 100,
        genesis_timestamp: 1_700_000_000,
    };

    assert_eq!(
        read_local_tee_runtime_admission_from_state(
            &outbe_runtime::OutbeRuntime::new(),
            &access,
            context(1),
            identity
        )
        .unwrap(),
        LocalTeeRuntimeAdmissionV1::BootstrapPending
    );
    assert_eq!(
        read_local_tee_runtime_admission_from_state(
            &outbe_runtime::OutbeRuntime::new(),
            &access,
            context(1),
            LocalTeeRuntimeIdentityV1 {
                validator: None,
                ..identity
            },
        )
        .unwrap(),
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::MissingBinding)
    );
    assert_eq!(
        read_local_tee_runtime_admission_from_state(
            &outbe_runtime::OutbeRuntime::new(),
            &access,
            context(2),
            identity
        )
        .unwrap(),
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::MissingBinding)
    );
}

#[test]
fn test_read_validators_from_state_handles_empty_known_and_invalid_p2p_entries() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        vs.config_is_initialized.write(true).unwrap();
        vs.set_config_max_validators(128).unwrap();
    });
    let empty_access = TestStateAccess {
        data: provider.storage,
    };
    let empty =
        read_validators_from_state(&outbe_runtime::OutbeRuntime::new(), &empty_access).unwrap();
    assert!(empty.public_keys.is_empty());
    assert!(empty.addresses.is_empty());

    let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 30400);
    let encoded = encode_v1(&P2pAddress::Symmetric(socket));
    let known_access = populated_p2p_state(|vs, validator| {
        vs.set_p2p_address(validator, validator, P2P_ADDRESS_VERSION_V1, &encoded)
            .unwrap();
    });
    let known =
        read_validators_from_state(&outbe_runtime::OutbeRuntime::new(), &known_access).unwrap();
    assert_eq!(
        known.p2p_addresses,
        vec![ValidatorP2pAddress::Known(CommonwareAddress::Symmetric(
            socket
        ))]
    );

    let invalid_access = populated_p2p_state(|vs, validator| {
        vs.test_corrupt_p2p_storage(validator, 99, &[0]).unwrap();
    });
    let invalid =
        read_validators_from_state(&outbe_runtime::OutbeRuntime::new(), &invalid_access).unwrap();
    assert_eq!(invalid.p2p_addresses, vec![ValidatorP2pAddress::Invalid]);
}

#[test]
fn test_read_consensus_validators_includes_exiting_with_share() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.set_block_number(1);
    let active = Address::with_last_byte(0x01);
    let exiting = Address::with_last_byte(0x02);

    StorageHandle::enter(&mut provider, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        vs.config_owner.write(OWNER).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.set_config_max_validators(128).unwrap();
        let minimum = alloy_primitives::U256::from(1_000u64);
        vs.test_seed_authoritative_min_stake(minimum).unwrap();
        vs.register_validator(OWNER, active, &valid_consensus_pubkey(1))
            .unwrap();
        vs.register_validator(OWNER, exiting, &valid_consensus_pubkey(2))
            .unwrap();
        vs.test_seed_authoritative_bonded(active, minimum).unwrap();
        vs.test_seed_authoritative_bonded(exiting, minimum).unwrap();
        vs.activate_validator_via_boundary_for_test(active).unwrap();
        vs.activate_validator_via_boundary_for_test(exiting)
            .unwrap();
        vs.deactivate_validator(OWNER, exiting).unwrap();
    });

    let access = TestStateAccess {
        data: provider.storage.clone(),
    };

    let active_only =
        read_validators_from_state(&outbe_runtime::OutbeRuntime::new(), &access).unwrap();
    assert_eq!(active_only.addresses, vec![active]);

    let consensus =
        read_consensus_validators_from_state(&outbe_runtime::OutbeRuntime::new(), &access).unwrap();
    assert_eq!(consensus.addresses, vec![active, exiting]);
}

#[test]
fn test_has_pending_set_change_reflects_contract_flag() {
    for expected in [false, true] {
        let mut provider = HashMapStorageProvider::new(1);
        StorageHandle::enter(&mut provider, |storage| {
            let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage);
            vs.config_is_initialized.write(true).unwrap();
            vs.set_config_max_validators(128).unwrap();
            if expected {
                vs.test_set_pending_set_change(true).unwrap();
            }
        });
        let access = TestStateAccess {
            data: provider.storage,
        };
        assert_eq!(
            has_pending_set_change(&outbe_runtime::OutbeRuntime::new(), &access).unwrap(),
            expected
        );
    }
}

#[test]
fn jailed_status_byte_matches_validator_set() {
    assert_eq!(
        outbe_engine::protocol_state::VALIDATOR_STATUS_JAILED,
        outbe_validatorset::runtime::status::JAILED
    );
}
