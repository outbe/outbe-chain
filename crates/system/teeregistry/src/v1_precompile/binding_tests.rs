use super::*;
use outbe_primitives::{storage::hashmap::HashMapStorageProvider, tee_attestation_v1::NodeIdV1};

const NODE_PUBLIC: [u8; 33] = [
    0x02, 0x79, 0xbe, 0x66, 0x7e, 0xf9, 0xdc, 0xbb, 0xac, 0x55, 0xa0, 0x62, 0x95, 0xce, 0x87, 0x0b,
    0x07, 0x02, 0x9b, 0xfc, 0xdb, 0x2d, 0xce, 0x28, 0xd9, 0x59, 0xf2, 0x81, 0x5b, 0x16, 0xf8, 0x17,
    0x98,
];

fn binding_calls(public: [u8; 33], validator: Address) -> [Vec<u8>; 2] {
    [
        ITeeRegistryV1::nodeHostEnclaveBindingCall {
            rethP2pPrefix: public[0],
            rethP2pX: B256::from_slice(&public[1..]),
        }
        .abi_encode(),
        ITeeRegistryV1::validatorEnclaveBindingCall { validator }.abi_encode(),
    ]
}

fn seed_hash_fields(registry: &TeeRegistry<'_>, node: B256) -> Result<()> {
    for (field, value) in [
        (&registry.v1_node_enclave_id, B256::repeat_byte(0x42)),
        (&registry.v1_node_binding_id, B256::repeat_byte(0x43)),
        (&registry.v1_node_intent_hash, B256::repeat_byte(0x44)),
        (&registry.v1_node_evidence_hash, B256::repeat_byte(0x45)),
        (&registry.v1_node_policy_hash, B256::repeat_byte(0x46)),
        (&registry.v1_node_recipient_x25519, B256::repeat_byte(0x47)),
        (
            &registry.v1_node_attestation_ed25519,
            B256::repeat_byte(0x48),
        ),
        (
            &registry.v1_node_noise_responder_x25519,
            B256::repeat_byte(0x49),
        ),
        (&registry.v1_node_mrenclave, B256::repeat_byte(0x4a)),
        (&registry.v1_node_mrsigner, B256::repeat_byte(0x4b)),
        (&registry.v1_node_verdict_hash, B256::repeat_byte(0x4c)),
        (
            &registry.v1_node_host_authorization_hash,
            B256::repeat_byte(0x4d),
        ),
    ] {
        field.write(&node, value)?;
    }
    Ok(())
}

fn seed_integer_fields(registry: &TeeRegistry<'_>, node: B256) -> Result<()> {
    for (field, value) in [
        (&registry.v1_node_binding_version, u64::MAX - 1),
        (&registry.v1_node_registration_version, 0x0203040506070809),
        (&registry.v1_node_renewal_nonce, 0x1213141516171819),
        (&registry.v1_node_transition_nonce, 0x2223242526272829),
        (&registry.v1_node_lease_started_at, 0x3233343536373839),
        (&registry.v1_node_valid_until, 0x4243444546474849),
        (&registry.v1_node_collateral_valid_until, 0x5253545556575859),
        (&registry.v1_node_isv_prod_id, 0x6263),
        (&registry.v1_node_isv_svn, 0x7273),
        (&registry.v1_node_platform_tcb_status, 0x82),
    ] {
        field.write(&node, value)?;
    }
    Ok(())
}

fn seeded_provider(node: B256, validator: Address) -> Result<HashMapStorageProvider> {
    let mut provider = HashMapStorageProvider::new(outbe_primitives::chain::TESTNET_CHAIN_ID);
    provider.enter(|storage| {
        let registry = TeeRegistry::new(storage);
        registry.validator_v1_node_hash.write(&validator, node)?;
        seed_hash_fields(&registry, node)?;
        seed_integer_fields(&registry, node)
    })?;
    Ok(provider)
}

// Independent ABI words pin field order and integer widths, without a mapper.
fn expected_binding_bytes(node: B256) -> Vec<u8> {
    let integer = |value: u64| U256::from(value).to_be_bytes::<32>();
    [
        integer(1),
        node.0,
        [0x42; 32],
        [0x43; 32],
        [0x44; 32],
        [0x45; 32],
        [0x46; 32],
        integer(u64::MAX - 1),
        integer(0x0203040506070809),
        integer(0x1213141516171819),
        integer(0x2223242526272829),
        integer(0x3233343536373839),
        integer(0x4243444546474849),
        integer(0x5253545556575859),
        [0x47; 32],
        [0x48; 32],
        [0x49; 32],
        [0x4a; 32],
        [0x4b; 32],
        integer(0x6263),
        integer(0x7273),
        integer(0x82),
        [0x4c; 32],
        [0x4d; 32],
    ]
    .concat()
}

#[test]
fn both_binding_selectors_preserve_all_v1_storage_fields_in_exact_abi_words() {
    let public = NODE_PUBLIC;
    let node = NodeIdV1 {
        reth_p2p_public: public,
    }
    .node_id_hash()
    .unwrap();
    let validator = Address::repeat_byte(0xa1);
    let mut provider = seeded_provider(node, validator).unwrap();
    for calldata in binding_calls(public, validator) {
        let encoded = provider
            .enter(|storage| dispatch(storage, &calldata, Address::ZERO, U256::ZERO))
            .unwrap();
        assert_eq!(encoded.as_ref(), expected_binding_bytes(node));
    }
}

#[test]
fn both_absent_binding_selectors_return_exactly_twenty_four_zero_words() {
    let mut provider = HashMapStorageProvider::new(outbe_primitives::chain::TESTNET_CHAIN_ID);
    for calldata in binding_calls(NODE_PUBLIC, Address::repeat_byte(0xa1)) {
        let encoded = provider
            .enter(|storage| dispatch(storage, &calldata, Address::ZERO, U256::ZERO))
            .unwrap();
        assert_eq!(encoded.as_ref(), vec![0_u8; 24 * 32]);
    }
}

#[test]
fn binding_selectors_reject_stored_measurement_integers_exceeding_abi_widths() {
    let node = NodeIdV1 {
        reth_p2p_public: NODE_PUBLIC,
    }
    .node_id_hash()
    .unwrap();
    let validator = Address::repeat_byte(0xa1);
    let mut provider = seeded_provider(node, validator).unwrap();
    for field in 0..3 {
        provider
            .enter(|storage| {
                let registry = TeeRegistry::new(storage);
                seed_integer_fields(&registry, node)?;
                match field {
                    0 => registry
                        .v1_node_isv_prod_id
                        .write(&node, u64::from(u16::MAX) + 1),
                    1 => registry
                        .v1_node_isv_svn
                        .write(&node, u64::from(u16::MAX) + 1),
                    _ => registry
                        .v1_node_platform_tcb_status
                        .write(&node, u64::from(u8::MAX) + 1),
                }
            })
            .unwrap();
        for calldata in binding_calls(NODE_PUBLIC, validator) {
            assert!(provider
                .enter(|storage| { dispatch(storage, &calldata, Address::ZERO, U256::ZERO) })
                .is_err());
        }
    }
}
