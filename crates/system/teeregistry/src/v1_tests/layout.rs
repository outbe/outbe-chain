use super::*;

#[test]
fn finalized_admission_slot_contract_matches_the_physical_registry_layout() {
    let mut provider = storage(B256::repeat_byte(0x44));
    StorageHandle::enter(&mut provider, |storage| {
        let registry = TeeRegistry::new(storage);
        assert_eq!(
            registry.tribute_offer_public_key.slot(),
            U256::from(TEE_REGISTRY_OFFER_PUBLIC_SLOT_V1)
        );
        assert_eq!(
            registry.key_epoch.slot(),
            U256::from(TEE_REGISTRY_KEY_EPOCH_SLOT_V1)
        );
        assert_eq!(
            registry.tribute_offer_epoch.slot(),
            U256::from(TEE_REGISTRY_OFFER_EPOCH_SLOT_V1)
        );
        assert_eq!(
            registry.v1_node_enclave_id.base_slot(),
            U256::from(TEE_REGISTRY_NODE_ENCLAVE_ID_SLOT_V1)
        );
        assert_eq!(
            registry.v1_node_binding_id.base_slot(),
            U256::from(TEE_REGISTRY_NODE_BINDING_ID_SLOT_V1)
        );
        assert_eq!(
            registry.v1_node_intent_hash.base_slot(),
            U256::from(TEE_REGISTRY_NODE_INTENT_HASH_SLOT_V1)
        );
        assert_eq!(
            registry.v1_node_policy_hash.base_slot(),
            U256::from(TEE_REGISTRY_NODE_POLICY_HASH_SLOT_V1)
        );
        assert_eq!(
            registry.v1_node_valid_until.base_slot(),
            U256::from(TEE_REGISTRY_NODE_VALID_UNTIL_SLOT_V1)
        );
        assert_eq!(
            registry.v1_node_recipient_x25519.base_slot(),
            U256::from(TEE_REGISTRY_NODE_RECIPIENT_X25519_SLOT_V1)
        );
        Ok::<(), PrecompileError>(())
    })
    .unwrap();
}
