use super::*;
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Hasher as _, Sha256, Signer as _};
use commonware_utils::TryCollect as _;
use outbe_coderegistry::CodeRegistry;
use outbe_primitives::{
    addresses::CODE_REGISTRY_ADDRESS,
    radicle_publication::{
        players_commitment, PublicationAction, PublicationAttestation, PublicationBatch,
        PUBLICATION_NAMESPACE,
    },
};
use outbe_validatorset::{
    contract::ValidatorSet,
    state::{write_committee_snapshot, CommitteeEntry, CommitteeSnapshot},
};

#[test]
fn publication_system_dispatch_requires_system_caller_and_current_quorum_atomically() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../system/coderegistry/tests/fixtures/governments-genesis-storage.json"
    )))
    .unwrap();
    let genesis = B256::repeat_byte(7);
    let mut provider = HashMapStorageProvider::new_with_chain_identity(2026, genesis);
    provider.set_block_number(1);
    for (slot, value) in fixture["storage"].as_object().unwrap() {
        provider.storage.insert(
            (CODE_REGISTRY_ADDRESS, slot.parse().unwrap()),
            value.as_str().unwrap().parse().unwrap(),
        );
    }
    let mut keys: Vec<_> = (101..105).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let players = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = outbe_consensus::bls::bootstrap_dkg_for_participants(players).unwrap();
    let committee = CommitteeSnapshot {
        committee: keys
            .iter()
            .enumerate()
            .map(|(index, key)| CommitteeEntry {
                address: Address::repeat_byte(index as u8 + 1),
                consensus_pubkey: key.public_key().encode().to_vec().try_into().unwrap(),
            })
            .collect(),
        vrf_material_version: 1,
        vrf_group_public_key_bytes: dkg.polynomial.public().encode().to_vec(),
        vrf_public_polynomial_hash: outbe_consensus::dkg_manager::public_polynomial_hash(
            &dkg.polynomial,
        ),
    };
    provider.enter(|storage| {
        let mut validators = ValidatorSet::new(storage.clone());
        validators.config_owner.write(OWNER).unwrap();
        validators.set_config_max_validators(4).unwrap();
        for member in &committee.committee {
            validators
                .test_register_validator_without_pop(member.address, &member.consensus_pubkey)
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(member.address)
                .unwrap();
        }
        write_committee_snapshot(storage, 0, &committee).unwrap();
    });
    let mut body = outbe_primitives::test_utils::sample_radicle_publication_body();
    body.chain_id = 2026;
    body.genesis_hash = genesis;
    body.descriptor_hash =
        provider.enter(|s| CodeRegistry::new(s).config_descriptor_hash.read().unwrap());
    body.epoch = 0;
    body.material_version = 1;
    body.owner_bls_public =
        alloy_primitives::FixedBytes::from_slice(&committee.vrf_group_public_key_bytes);
    body.players_commitment = players_commitment(
        committee
            .committee
            .iter()
            .map(|member| (member.address, &member.consensus_pubkey)),
    )
    .unwrap();
    let output = dkg.output.encode();
    body.material_hash = Sha256::hash(&[
        b"ODM1",
        &0_u64.to_be_bytes(),
        &1_u64.to_be_bytes(),
        body.players_commitment.as_slice(),
        &(output.len() as u32).to_be_bytes(),
        output.as_ref(),
    ])
    .0
    .into();
    body.action = PublicationAction::Bootstrap;
    body.sequence = 1;
    body.previous_receipt_hash = B256::ZERO;
    body.finalized_anchor_height = 0;
    body.finalized_anchor_hash = genesis;
    body.proposal_id = U256::ZERO;
    body.patch_id = Default::default();
    body.revision_id = Default::default();
    body.expected_target_head = Default::default();
    let batch = PublicationBatch {
        body: body.clone(),
        attestations: keys
            .iter()
            .enumerate()
            .map(|(index, key)| PublicationAttestation {
                participant_index: index as u32,
                signature: alloy_primitives::FixedBytes::from_slice(
                    key.sign(PUBLICATION_NAMESPACE, &body.to_bytes())
                        .encode()
                        .as_ref(),
                ),
            })
            .collect(),
    };
    let input = SystemTxInputV2::RadiclePublication {
        batch: batch.clone(),
    }
    .encode()
    .unwrap();
    let before = provider.storage.clone();
    assert!(provider
        .enter(|s| dispatch(s, &input, VALIDATOR, U256::ZERO))
        .is_err());
    assert!(provider
        .enter(|s| dispatch(s, &input, SYSTEM_ADDRESS, U256::from(1)))
        .is_err());
    let mut invalid = batch;
    invalid.attestations[3].signature = invalid.attestations[0].signature;
    let invalid = SystemTxInputV2::RadiclePublication { batch: invalid }
        .encode()
        .unwrap();
    assert!(provider
        .enter(|s| dispatch(s, &invalid, SYSTEM_ADDRESS, U256::ZERO))
        .is_err());
    assert_eq!(provider.storage, before);
    provider
        .enter(|s| dispatch(s, &input, SYSTEM_ADDRESS, U256::ZERO))
        .unwrap();
    provider.enter(|s| {
        let registry = CodeRegistry::new(s);
        assert_eq!(registry.publication_sequence.read().unwrap(), 1);
        assert_eq!(registry.publication_receipt(1).unwrap().unwrap().body, body);
    });
}
