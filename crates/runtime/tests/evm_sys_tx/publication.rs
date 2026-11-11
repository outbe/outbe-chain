use super::*;
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Hasher as _, Sha256, Signer as _};
use commonware_utils::TryCollect as _;
use outbe_coderegistry::CodeRegistry;
use outbe_primitives::system_tx::CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX;
use outbe_primitives::{
    addresses::CODE_REGISTRY_ADDRESS,
    radicle_publication::{
        players_commitment, PublicationAction, PublicationAttestation, PublicationBatch,
        PublicationBody, MAX_PUBLICATION_ATTESTATIONS, PUBLICATION_NAMESPACE,
    },
};
use outbe_validatorset::{
    contract::ValidatorSet,
    state::{write_committee_snapshot, CommitteeEntry, CommitteeSnapshot},
};

struct PublicationFixture {
    provider: HashMapStorageProvider,
    keys: Vec<bls12381::PrivateKey>,
    committee: CommitteeSnapshot,
    body: PublicationBody,
}

fn publication_fixture(size: usize, maximum_descriptor: bool) -> PublicationFixture {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../system/coderegistry/tests/fixtures/governments-genesis-storage.json"
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
    let mut keys: Vec<_> = (101..101 + size as u64)
        .map(bls12381::PrivateKey::from_seed)
        .collect();
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
                address: Address::from_word(B256::from(U256::from(index + 1).to_be_bytes::<32>())),
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
        validators.set_config_max_validators(size as u32).unwrap();
        let minimum = U256::from(1_000u64);
        validators
            .test_seed_authoritative_min_stake(minimum)
            .unwrap();
        for member in &committee.committee {
            validators
                .test_register_validator_without_pop(member.address, &member.consensus_pubkey)
                .unwrap();
            validators
                .test_seed_authoritative_bonded(member.address, minimum)
                .unwrap();
            validators
                .activate_validator_via_boundary_for_test(member.address)
                .unwrap();
        }
        write_committee_snapshot(storage, 0, &committee).unwrap();
    });
    if maximum_descriptor {
        provider.enter(|storage| {
            let registry = CodeRegistry::new(storage);
            let mut descriptor = registry.governments_descriptor().unwrap().unwrap();
            descriptor.description =
                "x".repeat(outbe_primitives::radicle_genesis::MAX_GOVERNMENTS_DESCRIPTION_BYTES);
            let encoded = descriptor.to_bytes().unwrap();
            assert_eq!(
                encoded.len(),
                outbe_primitives::radicle_genesis::MAX_GOVERNMENTS_DESCRIPTOR_BYTES
            );
            registry.config_descriptor_bytes.write(&encoded).unwrap();
            registry
                .config_descriptor_hash
                .write(descriptor.hash().unwrap())
                .unwrap();
        });
    }
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
    PublicationFixture {
        provider,
        keys,
        committee,
        body,
    }
}

fn signed_batch(
    keys: &[bls12381::PrivateKey],
    body: &PublicationBody,
    indices: std::ops::Range<usize>,
) -> PublicationBatch {
    PublicationBatch {
        body: body.clone(),
        attestations: indices
            .map(|index| PublicationAttestation {
                participant_index: index as u32,
                signature: alloy_primitives::FixedBytes::from_slice(
                    keys[index]
                        .sign(PUBLICATION_NAMESPACE, &body.to_bytes())
                        .encode()
                        .as_ref(),
                ),
            })
            .collect(),
    }
}

#[test]
fn publication_system_dispatch_requires_system_caller_and_current_quorum_atomically() {
    let PublicationFixture {
        mut provider,
        keys,
        body,
        ..
    } = publication_fixture(4, false);
    let batch = signed_batch(&keys, &body, 0..keys.len());
    let input = SystemTx::Consensus {
        step_index: CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX,
        payload: Bytes::from(batch.to_bytes().unwrap()),
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
    let invalid = SystemTx::Consensus {
        step_index: CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX,
        payload: Bytes::from(invalid.to_bytes().unwrap()),
    }
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

/// Every measured batch runs through the real system-call EVM. The independent
/// HashMap meter must match its gross charge and changed storage exactly.
fn measure_publication_batch(fixture: &mut PublicationFixture, batch: PublicationBatch) -> u64 {
    use alloy_evm::{Evm as _, EvmEnv, EvmFactory as _};
    use outbe_primitives::storage::{gas::PRECOMPILE_BASE_GAS, PrecompileStorageProvider as _};
    use revm::{
        context::{BlockEnv, CfgEnv},
        database::{CacheDB, EmptyDB},
        primitives::hardfork::SpecId,
        state::AccountInfo,
    };

    let input = SystemTx::Consensus {
        step_index: CONSENSUS_RADICLE_PUBLICATION_STEP_INDEX,
        payload: Bytes::from(batch.to_bytes().unwrap()),
    }
    .encode()
    .unwrap();
    let mut db = CacheDB::new(EmptyDB::default());
    for (&(address, slot), &value) in &fixture.provider.storage {
        // Retain storage-only seeded accounts under EIP-161, as genesis does.
        db.insert_account_info(
            address,
            AccountInfo {
                nonce: 1,
                ..Default::default()
            },
        );
        db.insert_account_storage(address, slot, value).unwrap();
    }
    db.cache.block_hashes.insert(
        U256::from(batch.body.finalized_anchor_height),
        batch.body.finalized_anchor_hash,
    );
    let factory = outbe_evm::factory::OutbeEvmFactory::new(std::sync::Arc::new(
        outbe_runtime::OutbeRuntime::new(),
    ))
    .with_genesis_hash(fixture.provider.genesis_hash())
    .with_genesis_timestamp(fixture.provider.genesis_timestamp());
    let env = EvmEnv {
        cfg_env: CfgEnv::new()
            .with_chain_id(2026)
            .with_spec_and_mainnet_gas_params(SpecId::PRAGUE),
        block_env: BlockEnv {
            number: U256::from(fixture.provider.block_number()),
            gas_limit: 32_000_000,
            ..Default::default()
        },
    };
    let mut evm = factory.create_evm(db, env);
    let output = evm
        .transact_system_call(
            SYSTEM_ADDRESS,
            outbe_primitives::addresses::OUTBE_SYSTEM_TX_ADDRESS,
            input.clone(),
        )
        .unwrap();
    assert!(
        output.result.is_success(),
        "publication EVM failure: {:?}",
        output.result
    );
    let gross = output.result.gas().total_gas_spent();

    fixture
        .provider
        .set_gas_limit(outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    fixture
        .provider
        .enter(|storage| dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO))
        .unwrap();
    let (reads, writes) = fixture.provider.metered_storage_operations();
    assert_eq!(fixture.provider.gas_used(), reads * 100 + writes * 5000);
    assert_eq!(gross, fixture.provider.gas_used() + PRECOMPILE_BASE_GAS);
    assert_eq!(fixture.provider.gas_refunded(), 0);
    for (address, account) in output.state {
        for (slot, value) in account.storage {
            if value.is_changed() {
                assert_eq!(
                    value.present_value(),
                    fixture
                        .provider
                        .storage
                        .get(&(address, slot))
                        .copied()
                        .unwrap_or_default()
                );
            }
        }
    }
    println!(
        "publication action={:?} sequence={} batch={} reads={reads} writes={writes} gross={gross}",
        batch.body.action,
        batch.body.sequence,
        batch.attestations.len()
    );
    gross
}

fn measure_publication_action(fixture: &mut PublicationFixture, body: &PublicationBody) -> u64 {
    assert_eq!(fixture.keys.len(), 256);
    assert_eq!(
        body.to_bytes().len(),
        outbe_primitives::radicle_publication::PUBLICATION_BODY_LEN
    );
    let mut max_gross = 0;
    // 170 correct contributions stay one below quorum. Another 54 valid
    // contributions bind a competing body without reaching its own quorum.
    // The final full 32-entry batch commits while scanning all 256 occupied slots.
    for (start, end, conflicting) in [(0, 170, false), (170, 224, true), (224, 256, false)] {
        let mut candidate = body.clone();
        if conflicting {
            candidate.package_manifest_hash = B256::repeat_byte(0x66);
        }
        for first in (start..end).step_by(MAX_PUBLICATION_ATTESTATIONS) {
            let last = (first + MAX_PUBLICATION_ATTESTATIONS).min(end);
            let batch = signed_batch(&fixture.keys, &candidate, first..last);
            if last - first == MAX_PUBLICATION_ATTESTATIONS {
                assert_eq!(
                    batch.to_bytes().unwrap().len(),
                    outbe_primitives::radicle_publication::MAX_PUBLICATION_BATCH_LEN
                );
            }
            max_gross = max_gross.max(measure_publication_batch(fixture, batch));
            fixture.provider.enter(|storage| {
                let registry = CodeRegistry::new(storage);
                assert_eq!(
                    registry.publication_sequence.read().unwrap(),
                    if last == 256 {
                        body.sequence
                    } else {
                        body.sequence - 1
                    }
                );
            });
        }
    }
    fixture.provider.enter(|storage| {
        assert_eq!(
            CodeRegistry::new(storage)
                .publication_receipt(body.sequence)
                .unwrap()
                .unwrap()
                .body,
            *body
        )
    });
    max_gross
}

#[test]
fn publication_maximum_committee_and_wire_batches_calibrate_all_actions() {
    use alloy_primitives::FixedBytes;
    use outbe_coderegistry::{payload::PatchVotePayloadV1, PatchApprovalRecord};
    use outbe_vote::{schema::ProposalRecord, ProposalStatus, Vote};

    assert_eq!(outbe_consensus::bls::MAX_VALIDATORS, 256);
    assert_eq!(MAX_PUBLICATION_ATTESTATIONS, 32);
    let mut fixture = publication_fixture(256, true);
    let oversized = signed_batch(&fixture.keys, &fixture.body, 0..33);
    assert!(
        oversized.to_bytes().is_err(),
        "33 identity checks cannot enter one publication call"
    );
    fixture.provider.enable_production_storage_gas_metering();
    let bootstrap = fixture.body.clone();
    let bootstrap_gas = measure_publication_action(&mut fixture, &bootstrap);

    // Move to a real new epoch/material snapshot before binding its receipt.
    fixture.committee.vrf_material_version = 2;
    fixture.provider.set_block_number(5);
    fixture.provider.enter(|storage| {
        ValidatorSet::new(storage.clone())
            .advance_epoch(1005, 5)
            .unwrap();
        write_committee_snapshot(storage, 1, &fixture.committee).unwrap();
    });
    fixture.provider.set_block_number(6);
    fixture
        .provider
        .set_canonical_block_hash(5, B256::repeat_byte(5));
    let mut material = bootstrap.clone();
    material.action = PublicationAction::BindMaterial;
    material.epoch = 1;
    material.material_version = 2;
    material.material_hash = B256::repeat_byte(0x33);
    material.sequence = 2;
    material.previous_receipt_hash = bootstrap.receipt_hash();
    material.finalized_anchor_height = 5;
    material.finalized_anchor_hash = B256::repeat_byte(5);
    let material_gas = measure_publication_action(&mut fixture, &material);

    let subject = PatchVotePayloadV1::accept(
        material.repo_id,
        FixedBytes::repeat_byte(0xbb),
        FixedBytes::repeat_byte(0xcc),
        FixedBytes::repeat_byte(0xdd),
    );
    let payload = String::from_utf8(subject.encode().unwrap()).unwrap();
    // Seed the canonical terminal-approved records normally produced by Vote.
    // Publication validates both records and the exact bounded subject itself.
    fixture.provider.enter(|storage| {
        Vote::new(storage.clone())
            .proposals
            .create(&ProposalRecord {
                id: U256::ONE,
                proposer: OWNER,
                target_module: CODE_REGISTRY_ADDRESS,
                payload: payload.clone(),
                created_height: 4,
                voting_deadline_height: 6,
                status: ProposalStatus::Approved.to_u8(),
            })
            .unwrap();
        let registry = CodeRegistry::new(storage);
        registry
            .patch_approvals
            .create(&PatchApprovalRecord {
                proposal_id: U256::ONE,
                payload,
                approved_height: 7,
                approved_timestamp: 7,
            })
            .unwrap();
        registry
            .patch_execution_stage
            .write(
                &U256::ONE,
                outbe_coderegistry::schema::PATCH_EXECUTION_PENDING,
            )
            .unwrap();
    });
    fixture.provider.set_block_number(8);
    fixture
        .provider
        .set_canonical_block_hash(7, B256::repeat_byte(7));
    let mut review = material.clone();
    review.action = PublicationAction::Review;
    review.sequence = 3;
    review.previous_receipt_hash = material.receipt_hash();
    review.proposal_id = U256::ONE;
    review.patch_id = subject.patch_id;
    review.revision_id = subject.revision_id;
    review.expected_target_head = subject.expected_target_head;
    review.finalized_anchor_height = 7;
    review.finalized_anchor_hash = B256::repeat_byte(7);
    let review_gas = measure_publication_action(&mut fixture, &review);

    fixture.provider.set_block_number(9);
    fixture
        .provider
        .set_canonical_block_hash(8, B256::repeat_byte(8));
    let mut merge = review.clone();
    merge.action = PublicationAction::Merge;
    merge.sequence = 4;
    merge.previous_receipt_hash = review.receipt_hash();
    merge.finalized_anchor_height = 8;
    merge.finalized_anchor_hash = B256::repeat_byte(8);
    let merge_gas = measure_publication_action(&mut fixture, &merge);
    // Pin the calibration inputs used by the production four-times headroom rule.
    assert_eq!(
        [bootstrap_gas, material_gas, review_gas, merge_gas],
        [1_691_100, 1_667_200, 1_669_800, 1_669_800]
    );
    let max_gross = bootstrap_gas
        .max(material_gas)
        .max(review_gas)
        .max(merge_gas);
    let publication_reserve = max_gross.checked_mul(4).unwrap().div_ceil(1_000_000) * 1_000_000;
    // Oracle/Boundary/TEE maxima from executor/tests/receipts_and_gas/calibration.rs.
    let combined_later_reserve = 397_996_602_u64
        .checked_add(max_gross)
        .unwrap()
        .checked_mul(4)
        .unwrap()
        .div_ceil(1_000_000)
        * 1_000_000;
    assert!(publication_reserve > max_gross);
    assert_eq!(publication_reserve, 7_000_000);
    assert_eq!(combined_later_reserve, 1_599_000_000);
    assert!(combined_later_reserve < outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    println!("publication n=256 quorum=171 max_batch=32 max_descriptor=353 fixed_body=477 bootstrap={bootstrap_gas} material={material_gas} review={review_gas} merge={merge_gas} max_gross={max_gross} publication_reserve_4x_rounded_1m={publication_reserve} combined_later_reserve={combined_later_reserve}");
}
