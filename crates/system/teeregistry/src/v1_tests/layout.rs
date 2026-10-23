use super::*;
use outbe_primitives::zero_fee::VALIDATOR_V1_NODE_HASH_SLOT;

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
            registry.validator_v1_node_hash.base_slot(),
            U256::from(VALIDATOR_V1_NODE_HASH_SLOT)
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

        let context = DcapOnboardingContextV1 {
            chain_id: [0; 32],
            genesis_hash: B256::ZERO,
            intent_hash: B256::ZERO,
            node_id_hash: B256::repeat_byte(0x44),
            enclave_id: B256::ZERO,
            binding_id: B256::repeat_byte(0x01),
            policy_hash: B256::repeat_byte(0x02),
            recipient_x25519: [0; 32],
            tribute_offer_public: [0; 32],
            key_epoch: 0,
            tribute_offer_epoch: 0,
        };
        let direct = |slot: U256| B256::from(slot.to_be_bytes::<32>());
        let mapped = |slot: U256| {
            let mut preimage = [0_u8; 64];
            preimage[..32].copy_from_slice(context.node_id_hash.as_slice());
            preimage[32..].copy_from_slice(&slot.to_be_bytes::<32>());
            alloy_primitives::keccak256(preimage)
        };
        assert_eq!(
            outbe_tee::finalized_admission::upgrade_registry_slots_v1(&context),
            [
                direct(registry.tribute_offer_public_key.slot()),
                direct(registry.key_epoch.slot()),
                direct(registry.tribute_offer_epoch.slot()),
                mapped(registry.upgrade_candidate_context.base_slot()),
                mapped(registry.upgrade_candidate_expiry.base_slot()),
                mapped(registry.upgrade_candidate_source.base_slot()),
                mapped(registry.v1_node_binding_id.base_slot()),
                direct(registry.strict_upgrade_successor.slot()),
                mapped(registry.upgrade_candidate_nonce.base_slot()),
            ]
        );
        assert_eq!(
            registry.enclave_upgrade_storage_slots_v1(),
            [
                registry.strict_upgrade_proposal.slot(),
                registry.strict_upgrade_height.slot(),
                registry.strict_upgrade_mrenclave.slot(),
                registry.strict_upgrade_successor.slot(),
                registry.strict_upgrade_predecessor.slot(),
                registry.last_enclave_retirement_height.slot(),
            ]
            .map(|slot| B256::from(slot.to_be_bytes::<32>()))
            .to_vec()
        );
        Ok::<(), PrecompileError>(())
    })
    .unwrap();
}
