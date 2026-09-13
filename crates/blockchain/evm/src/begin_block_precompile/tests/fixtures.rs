use super::*;

pub(super) const CHAIN_ID: u64 = outbe_primitives::tee_genesis_v1::GRAMINE_DIRECT_DEV_CHAIN_ID;

pub(super) const GENESIS_HASH: B256 = B256::repeat_byte(0x11);

pub(super) const OWNER: Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

pub(super) const VALIDATOR: Address = address!("0x1111111111111111111111111111111111111111");

pub(super) fn metadata() -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        finalized_block_number: 1,
        finalized_block_hash: B256::repeat_byte(0x11),
        finalized_epoch: 1,
        finalized_view: 2,
        parent_view: 1,
        ordered_committee: vec![VALIDATOR],
        signer_bitmap: vec![1],
        proof: Bytes::from_static(b"cert"),
        committee_set_hash: B256::ZERO,
        vrf_material_version: 0,
        vrf_group_public_key_hash: B256::ZERO,
        proof_kind: outbe_primitives::consensus_metadata::ParentParticipationProof::Finalization,
        missed_proposers: Vec::new(),
    }
}

fn active_set_hash(addresses: &[Address]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    keccak256(bytes)
}

pub(super) fn boundary_noop() -> DkgBoundaryArtifact {
    let vrf_group_public_key_bytes = vec![0x42u8; 96];
    let snapshot = outbe_validatorset::CommitteeSnapshot {
        committee: vec![outbe_validatorset::CommitteeEntry {
            address: VALIDATOR,
            consensus_pubkey: [7u8; 48],
        }],
        vrf_material_version: 1,
        vrf_group_public_key_bytes: vrf_group_public_key_bytes.clone(),
        vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
    };
    DkgBoundaryArtifact {
        epoch: 1,
        dkg_cycle: 1,
        freeze_height: 1,
        planned_activation_height: 2,
        target_set_hash: B256::ZERO,
        vrf_material_version: 1,
        vrf_group_public_key: keccak256(&vrf_group_public_key_bytes),
        vrf_group_public_key_bytes: Bytes::from(vrf_group_public_key_bytes),
        committee_set_hash: outbe_validatorset::committee_set_hash_v2(1, &snapshot),
        is_validator_set_change: false,
        outcome: Bytes::new(),
        is_full_dkg: false,
        tee_recipient_pubkeys: Vec::new(),
        tee_expired_target_exclusions: Vec::new(),
        tee_expired_target_exclusions_hash: B256::ZERO,
        reshare: ReshareResult {
            new_active_set: vec![VALIDATOR],
            active_set_hash: active_set_hash(&[VALIDATOR]),
        },
    }
}

pub(super) fn configured_storage(block_number: u64, timestamp: u64) -> HashMapStorageProvider {
    let consensus_key = [7u8; 48];
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS_HASH);
    provider.set_block_number(block_number);
    provider.set_timestamp(U256::from(timestamp));
    provider.set_beneficiary(VALIDATOR);
    let install = outbe_metadosis::test_support::ForkInstallScenario::measurement_at(
        1,
        CHAIN_ID,
        GENESIS_HASH,
    )
    .unwrap()
    .with_founder_validators(&[(VALIDATOR, consensus_key)])
    .unwrap()
    .into_install();
    provider.enter(|storage| {
        let root = outbe_compressed_entities::sealed_root(B256::ZERO).unwrap();
        storage
            .sstore(
                outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                U256::ZERO,
                U256::from(4),
            )
            .unwrap();
        storage
            .sstore(
                outbe_primitives::addresses::COMPRESSED_ENTITIES_ADDRESS,
                U256::from(1),
                U256::from_be_slice(root.as_slice()),
            )
            .unwrap();
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_epoch_length_blocks.write(10).unwrap();
        vs.register_validator(OWNER, VALIDATOR, &consensus_key)
            .unwrap();
        vs.mark_pending(VALIDATOR).unwrap();
        let registration = install.founder_registrations[0]
            .encode_canonical(&outbe_metadosis::config::poc_schema_limits())
            .unwrap();
        vs.confirm_validator_ready(VALIDATOR, &registration)
            .unwrap();
        vs.activate_validator_via_boundary_for_test(VALIDATOR)
            .unwrap();

        outbe_oracle::api::register_pair(storage.clone(), outbe_oracle::api::DAY_TYPE_PAIR)
            .unwrap();
    });
    provider.set_block_number(1);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::ForkProfile,
    );
    provider.enter(|storage| {
        let ctx = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(1, timestamp, CHAIN_ID),
            storage,
        );
        outbe_metadosis::commands::install_fork_profile(&ctx, &install).unwrap();
    });
    provider.set_block_number(block_number);
    provider
}

pub(super) fn provider_from_storage(
    block_number: u64,
    timestamp: u64,
    storage: std::collections::HashMap<(Address, U256), U256>,
) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, GENESIS_HASH);
    provider.set_block_number(block_number);
    provider.set_timestamp(U256::from(timestamp));
    provider.set_beneficiary(VALIDATOR);
    provider.storage = storage;
    provider
}

pub(super) fn runtime_ctx(storage: StorageHandle<'_>) -> BlockRuntimeContext<'_> {
    BlockRuntimeContext::new(
        BlockContext::new(
            storage.block_number().unwrap(),
            storage.timestamp().unwrap().to::<u64>(),
            storage.chain_id().unwrap(),
            VALIDATOR,
            vec![VALIDATOR],
        ),
        storage,
    )
}
