use super::*;
use crate::{payload::PatchVotePayloadV1, publication::PublicationProgress};
use commonware_codec::Encode as _;
use commonware_cryptography::{bls12381, Hasher as _, Sha256, Signer as _};
use commonware_utils::TryCollect as _;
use outbe_primitives::{
    addresses::RADICLE_REGISTRY_ADDRESS,
    radicle_publication::{
        players_commitment, PublicationAction, PublicationAttestation, PublicationBatch,
        PublicationBody, PUBLICATION_NAMESPACE,
    },
    storage::PrecompileStorageProvider as _,
};
use outbe_validatorset::{
    contract::ValidatorSet,
    state::{write_committee_snapshot, CommitteeEntry, CommitteeSnapshot},
};

struct Fixture {
    provider: HashMapStorageProvider,
    keys: Vec<bls12381::PrivateKey>,
    committee: CommitteeSnapshot,
    body: PublicationBody,
}

impl Fixture {
    fn new(n: usize) -> Self {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/governments-genesis-storage.json"
        ))
        .unwrap();
        let mut provider =
            HashMapStorageProvider::new_with_chain_identity(2026, B256::repeat_byte(7));
        provider.set_block_number(1);
        for (key, value) in fixture["storage"].as_object().unwrap() {
            provider.storage.insert(
                (RADICLE_REGISTRY_ADDRESS, key.parse().unwrap()),
                value.as_str().unwrap().parse().unwrap(),
            );
        }
        let mut keys: Vec<_> = (101..101 + n as u64)
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
                    address: Address::from_word(B256::from(
                        U256::from(index + 1).to_be_bytes::<32>(),
                    )),
                    consensus_pubkey: key.public_key().encode().to_vec().try_into().unwrap(),
                })
                .collect(),
            vrf_material_version: 1,
            vrf_group_public_key_bytes: dkg.polynomial.public().encode().to_vec(),
            vrf_public_polynomial_hash: outbe_consensus::dkg_manager::public_polynomial_hash(
                &dkg.polynomial,
            ),
        };
        // The Vote fixtures use repeated-byte addresses A/B/C.
        let mut committee = committee;
        if n <= 255 {
            for (index, member) in committee.committee.iter_mut().enumerate() {
                member.address = Address::repeat_byte(index as u8 + 1);
            }
        }
        provider.enter(|storage| {
            let mut validators = ValidatorSet::new(storage.clone());
            validators
                .config_owner
                .write(Address::repeat_byte(99))
                .unwrap();
            validators.set_config_max_validators(n as u32).unwrap();
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
        let players = players_commitment(
            committee
                .committee
                .iter()
                .map(|member| (member.address, &member.consensus_pubkey)),
        )
        .unwrap();
        let output = dkg.output.encode();
        let material_hash = Sha256::hash(&[
            b"ODM1",
            &0_u64.to_be_bytes(),
            &1_u64.to_be_bytes(),
            players.as_slice(),
            &(output.len() as u32).to_be_bytes(),
            output.as_ref(),
        ])
        .0
        .into();
        let body = PublicationBody {
            chain_id: 2026,
            genesis_hash: B256::repeat_byte(7),
            repo_id: repo(0xaa),
            descriptor_hash: provider.enter(|s| {
                RadicleRegistry::new(s)
                    .config_descriptor_hash
                    .read()
                    .unwrap()
            }),
            owner_bls_public: committee
                .vrf_group_public_key_bytes
                .as_slice()
                .try_into()
                .unwrap(),
            players_commitment: players,
            epoch: 0,
            material_version: 1,
            material_hash,
            finalized_anchor_height: 0,
            finalized_anchor_hash: B256::repeat_byte(7),
            sequence: 1,
            previous_receipt_hash: B256::ZERO,
            action: PublicationAction::Bootstrap,
            proposal_id: U256::ZERO,
            patch_id: FixedBytes::ZERO,
            revision_id: FixedBytes::ZERO,
            expected_target_head: FixedBytes::ZERO,
            package_manifest_hash: B256::repeat_byte(8),
        };
        Self {
            provider,
            keys,
            committee,
            body,
        }
    }

    fn signed(&self, body: &PublicationBody, indices: &[usize]) -> PublicationBatch {
        PublicationBatch {
            body: body.clone(),
            attestations: indices
                .iter()
                .map(|&index| PublicationAttestation {
                    participant_index: index as u32,
                    signature: FixedBytes::from_slice(
                        self.keys[index]
                            .sign(PUBLICATION_NAMESPACE, &body.to_bytes())
                            .encode()
                            .as_ref(),
                    ),
                })
                .collect(),
        }
    }

    fn apply(
        &mut self,
        body: &PublicationBody,
        indices: &[usize],
    ) -> outbe_primitives::error::Result<PublicationProgress> {
        let batch = self.signed(body, indices);
        self.provider
            .enter(|storage| RadicleRegistry::new(storage).apply_publication_batch(&batch))
    }

    fn bootstrap(&mut self) {
        let body = self.body.clone();
        let quorum = self.keys.len() - (self.keys.len() - 1) / 3;
        let mut completed = 0;
        for indices in (0..quorum).collect::<Vec<_>>().chunks(32) {
            let progress = self.apply(&body, indices).unwrap();
            completed += usize::from(progress.receipt.is_some());
        }
        assert_eq!(completed, 1);
    }

    fn advance_material(&mut self) {
        self.committee.vrf_material_version += 1;
        self.provider.set_block_number(5);
        self.provider.enter(|storage| {
            ValidatorSet::new(storage.clone())
                .advance_epoch(1005, 5)
                .unwrap();
            write_committee_snapshot(storage, 1, &self.committee).unwrap();
        });
        self.provider.set_block_number(6);
        self.provider
            .set_canonical_block_hash(5, B256::repeat_byte(5));
    }
}

#[test]
fn publication_generation_drops_pending_slots_and_preserves_accepted_history() {
    let mut fixture = Fixture::new(4);
    let original = fixture.body.clone();
    assert_eq!(fixture.apply(&original, &[0]).unwrap().attestations, 1);
    fixture.advance_material();
    assert!(fixture.apply(&original, &[1, 2, 3]).is_err());
    let mut current = original;
    current.epoch = 1;
    current.material_version = 2;
    current.material_hash = B256::repeat_byte(42);
    assert_eq!(fixture.apply(&current, &[1, 2]).unwrap().attestations, 2);
    assert!(fixture.apply(&current, &[3]).unwrap().receipt.is_some());

    let mut fixture = Fixture::new(4);
    fixture.bootstrap();
    let accepted = fixture.body.clone();
    fixture.advance_material();
    let mut binding = accepted.clone();
    binding.action = PublicationAction::BindMaterial;
    binding.epoch = 1;
    binding.material_version = 2;
    binding.material_hash = B256::repeat_byte(42);
    binding.sequence = 2;
    binding.previous_receipt_hash = accepted.receipt_hash();
    binding.finalized_anchor_height = 5;
    binding.finalized_anchor_hash = B256::repeat_byte(5);
    assert!(fixture
        .apply(&binding, &[0, 1, 2])
        .unwrap()
        .receipt
        .is_some());
    fixture.provider.enter(|storage| {
        let registry = RadicleRegistry::new(storage);
        assert_eq!(
            registry.publication_receipt(1).unwrap().unwrap().body,
            accepted
        );
        assert_eq!(registry.publication_material_sequence.read().unwrap(), 2);
    });
    let previous_hash = binding.receipt_hash();
    binding.sequence = 3;
    binding.previous_receipt_hash = previous_hash;
    assert!(
        revert(fixture.apply(&binding, &[0, 1, 2]).unwrap_err()).contains("material already bound")
    );
}

#[test]
fn publication_every_write_failure_rolls_back_pending_binding_receipt_and_event() {
    let fixture = Fixture::new(4);
    let batch = fixture.signed(&fixture.body, &[0, 1, 2]);
    let fork = || {
        let mut provider =
            HashMapStorageProvider::new_with_chain_identity(2026, B256::repeat_byte(7));
        provider.storage = fixture.provider.storage.clone();
        provider.set_block_number(1);
        provider
    };
    let mut completed = fork();
    completed.clear_mutation_failure();
    completed
        .enter(|s| RadicleRegistry::new(s).apply_publication_batch(&batch))
        .unwrap();
    let mutations = completed.clear_mutation_failure();
    assert!(mutations > 0);
    for operation in 0..mutations {
        for after in [false, true] {
            let mut provider = fork();
            let before = provider.storage.clone();
            if after {
                provider.fail_after_mutation_at(operation);
            } else {
                provider.fail_mutation_at(operation);
            }
            assert!(
                provider
                    .enter(|s| RadicleRegistry::new(s).apply_publication_batch(&batch))
                    .is_err(),
                "operation={operation},after={after}"
            );
            assert_eq!(
                provider.storage, before,
                "operation={operation},after={after}"
            );
            assert!(provider.get_ordered_events().is_empty());
        }
    }
    println!(
        "publication atomic bootstrap mutation sites={mutations}, before+after injection PASS"
    );
}

#[test]
fn publication_maximum_committee_reaches_quorum_with_six_bounded_batches() {
    use outbe_primitives::{
        radicle_publication::MAX_PUBLICATION_ATTESTATIONS, system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT,
    };
    let mut fixture = Fixture::new(outbe_consensus::bls::MAX_VALIDATORS as usize);
    let body = fixture.body.clone();
    let required = fixture.keys.len() - (fixture.keys.len() - 1) / 3;
    fixture.provider.enable_production_storage_gas_metering();
    let mut max_reads = 0;
    let mut max_writes = 0;
    let mut max_storage_gas = 0;
    let mut completed = 0;
    let mut batches = 0;
    let oversized = fixture.signed(&body, &(0..33).collect::<Vec<_>>());
    let before = fixture.provider.storage.clone();
    assert!(fixture
        .provider
        .enter(|s| RadicleRegistry::new(s).apply_publication_batch(&oversized))
        .is_err());
    assert_eq!(fixture.provider.storage, before);
    let start = std::time::Instant::now();
    for indices in (0..required)
        .collect::<Vec<_>>()
        .chunks(MAX_PUBLICATION_ATTESTATIONS)
    {
        if batches == 0 {
            let mut invalid = fixture.signed(&body, indices);
            invalid.attestations.last_mut().unwrap().signature = invalid.attestations[0].signature;
            let before = fixture.provider.storage.clone();
            assert!(fixture
                .provider
                .enter(|s| RadicleRegistry::new(s).apply_publication_batch(&invalid))
                .is_err());
            assert_eq!(fixture.provider.storage, before);
        }
        fixture.provider.set_gas_limit(SYSTEM_TX_ARTIFACT_GAS_LIMIT);
        let progress = fixture.apply(&body, indices).unwrap();
        completed += usize::from(progress.receipt.is_some());
        batches += 1;
        let (reads, writes) = fixture.provider.metered_storage_operations();
        max_reads = max_reads.max(reads);
        max_writes = max_writes.max(writes);
        max_storage_gas = max_storage_gas.max(fixture.provider.gas_used());
    }
    assert_eq!(required, 171);
    assert_eq!(batches, 6);
    assert_eq!(completed, 1);
    assert!(max_storage_gas > 0 && max_storage_gas < SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    println!("publication n=256 q=171 batches={batches} per_batch_identity_checks<=32 max_storage_reads={max_reads} max_storage_writes={max_writes} max_storage_gas={max_storage_gas} valid_and_negative_calls_elapsed_us={}", start.elapsed().as_micros());
}

#[test]
fn publication_receipt_reader_bounds_the_raw_dynamic_body_before_loading() {
    use outbe_primitives::storage::types::StorageKey as _;
    let mut fixture = Fixture::new(4);
    fixture.bootstrap();
    let slot = fixture
        .provider
        .enter(|s| 1_u64.mapping_slot(RadicleRegistry::new(s).publication_receipts.base_slot()));
    fixture
        .provider
        .storage
        .insert((RADICLE_REGISTRY_ADDRESS, slot), U256::MAX);
    assert!(fixture
        .provider
        .enter(|s| RadicleRegistry::new(s).publication_receipt(1))
        .is_err());
}

#[test]
fn publication_q_crossed_inside_batch_commits_once_and_bad_last_is_atomic() {
    let mut fixture = Fixture::new(4);
    let body = fixture.body.clone();
    assert_eq!(fixture.apply(&body, &[0, 1]).unwrap().attestations, 2);
    let before = fixture.provider.storage.clone();
    let events = fixture.provider.get_ordered_events().to_vec();
    let mut bad = fixture.signed(&body, &[2, 3]);
    bad.attestations[1].signature = FixedBytes::from_slice(
        bls12381::PrivateKey::from_seed(999)
            .sign(PUBLICATION_NAMESPACE, &body.to_bytes())
            .encode()
            .as_ref(),
    );
    assert!(fixture
        .provider
        .enter(|s| RadicleRegistry::new(s).apply_publication_batch(&bad))
        .is_err());
    assert_eq!(fixture.provider.storage, before);
    assert_eq!(fixture.provider.get_ordered_events(), events);
    let result = fixture.apply(&body, &[2, 3]).unwrap();
    assert_eq!(result.attestations, 4);
    assert_eq!(result.receipt.unwrap().receipt_hash, body.receipt_hash());
    fixture.provider.enter(|storage| {
        let registry = RadicleRegistry::new(storage);
        assert_eq!(registry.publication_sequence.read().unwrap(), 1);
        assert_eq!(registry.publication_receipt(1).unwrap().unwrap().body, body);
        assert!(registry.publication_receipt(2).unwrap().is_none());
        assert_eq!(registry.repository_at(0).unwrap(), body.repo_id);
    });
    assert!(fixture.apply(&body, &[0, 1, 2]).is_err());
}

#[test]
fn publication_duplicate_slots_cannot_inflate_quorum_or_switch_body() {
    let mut fixture = Fixture::new(4);
    let body = fixture.body.clone();
    assert_eq!(fixture.apply(&body, &[0]).unwrap().attestations, 1);
    assert_eq!(fixture.apply(&body, &[0]).unwrap().attestations, 1);
    let mut other = body.clone();
    other.material_hash = B256::repeat_byte(23);
    let before = fixture.provider.storage.clone();
    assert!(fixture.apply(&other, &[0]).is_err());
    assert_eq!(fixture.provider.storage, before);
    assert_eq!(fixture.apply(&other, &[1]).unwrap().attestations, 1);
    let complete = fixture.apply(&body, &[2, 3]).unwrap();
    assert_eq!(complete.attestations, 3);
    assert!(complete.receipt.is_some());
}

#[test]
fn publication_invalid_context_or_typed_contributions_never_change_state() {
    let mut fixture = Fixture::new(4);
    let original = fixture.body.clone();
    let changes: Vec<fn(&mut PublicationBody)> = vec![
        |b| b.chain_id += 1,
        |b| b.genesis_hash = B256::repeat_byte(9),
        |b| b.descriptor_hash = B256::repeat_byte(9),
        |b| b.players_commitment = B256::repeat_byte(9),
        |b| b.epoch += 1,
        |b| b.material_version += 1,
        |b| b.owner_bls_public = FixedBytes::ZERO,
        |b| b.repo_id = FixedBytes::ZERO,
        |b| b.material_hash = B256::ZERO,
        |b| b.package_manifest_hash = B256::ZERO,
        |b| b.sequence += 1,
        |b| b.previous_receipt_hash = B256::repeat_byte(9),
        |b| b.patch_id = repo(9),
        |b| b.finalized_anchor_height = 1,
        |b| b.finalized_anchor_hash = B256::repeat_byte(9),
    ];
    for change in changes {
        let mut body = original.clone();
        change(&mut body);
        let before = fixture.provider.storage.clone();
        assert!(fixture.apply(&body, &[0, 1, 2]).is_err());
        assert_eq!(fixture.provider.storage, before);
    }
    let valid = fixture.signed(&original, &[0, 1, 2]);
    let mut invalid = vec![];
    let mut empty = valid.clone();
    empty.attestations.clear();
    invalid.push(empty);
    let mut duplicate = valid.clone();
    duplicate.attestations[2] = duplicate.attestations[1].clone();
    invalid.push(duplicate);
    let mut outside = valid.clone();
    outside.attestations[2].participant_index = 256;
    invalid.push(outside);
    let mut wrong_key = valid.clone();
    wrong_key.attestations[2].signature = wrong_key.attestations[1].signature;
    invalid.push(wrong_key);
    let mut wrong_domain = valid.clone();
    wrong_domain.attestations[2].signature = FixedBytes::from_slice(
        fixture.keys[2]
            .sign(b"outbe.radicle.partial.v1", &original.to_bytes())
            .encode()
            .as_ref(),
    );
    invalid.push(wrong_domain);
    for batch in invalid {
        let before = fixture.provider.storage.clone();
        assert!(fixture
            .provider
            .enter(|s| RadicleRegistry::new(s).apply_publication_batch(&batch))
            .is_err());
        assert_eq!(fixture.provider.storage, before);
    }
}

#[test]
fn publication_bootstrap_reserves_capacity_and_adopts_an_existing_rid() {
    for pre_registered in [false, true] {
        let mut fixture = Fixture::new(4);
        let body = fixture.body.clone();
        fixture.provider.enter(|storage| {
            let mut registry = RadicleRegistry::new(storage);
            registry.config_max_repositories.write(2).unwrap();
            registry
                .register_repository(
                    if pre_registered {
                        body.repo_id
                    } else {
                        repo(1)
                    },
                    Address::repeat_byte(44),
                )
                .unwrap();
            assert!(registry
                .register_repository(repo(2), Address::ZERO)
                .is_err());
        });
        fixture.bootstrap();
        fixture.provider.enter(|storage| {
            let registry = RadicleRegistry::new(storage);
            assert_eq!(
                registry.network_repository.read().unwrap(),
                repo_id_storage_key(body.repo_id)
            );
            assert_eq!(
                registry
                    .repository_index
                    .read(&repo_id_storage_key(body.repo_id))
                    .unwrap(),
                if pre_registered { 1 } else { 2 }
            );
            assert_eq!(
                registry.repository_count.read().unwrap(),
                if pre_registered { 1 } else { 2 }
            );
            if pre_registered {
                assert_eq!(
                    registry.registrant_of(body.repo_id).unwrap(),
                    Address::repeat_byte(44)
                );
            }
        });
    }
}

#[test]
fn publication_review_requires_real_terminal_vote_and_merge_is_a_separate_checkpoint() {
    let mut fixture = Fixture::new(4);
    fixture.bootstrap();
    let bootstrap = fixture.body.clone();
    fixture.provider.set_block_number(10);
    let payload = PatchVotePayloadV1::accept(bootstrap.repo_id, repo(0xbb), repo(0xcc), repo(0xdd));
    let proposal = super::patch_vote::create(
        &mut fixture.provider,
        String::from_utf8(payload.encode().unwrap()).unwrap(),
    )
    .unwrap();
    let mut review = bootstrap.clone();
    review.action = PublicationAction::Review;
    review.proposal_id = proposal;
    review.patch_id = payload.patch_id;
    review.revision_id = payload.revision_id;
    review.expected_target_head = payload.expected_target_head;
    review.sequence = 2;
    review.previous_receipt_hash = bootstrap.receipt_hash();
    review.finalized_anchor_height = 13;
    review.finalized_anchor_hash = B256::repeat_byte(13);
    assert!(fixture.apply(&review, &[0, 1, 2]).is_err());
    for address in [1, 2, 3] {
        super::patch_vote::cast(
            &mut fixture.provider,
            proposal,
            Address::repeat_byte(address),
            true,
        )
        .unwrap();
    }
    super::patch_vote::finalize(&mut fixture.provider, 13).unwrap();
    fixture.provider.set_block_number(14);
    fixture
        .provider
        .set_canonical_block_hash(13, B256::repeat_byte(13));
    let mut wrong = review.clone();
    wrong.revision_id = repo(0xee);
    let before = fixture.provider.storage.clone();
    assert!(fixture.apply(&wrong, &[0, 1, 2]).is_err());
    assert_eq!(fixture.provider.storage, before);
    let mut premature_merge = review.clone();
    premature_merge.action = PublicationAction::Merge;
    assert!(fixture.apply(&premature_merge, &[0, 1, 2]).is_err());
    assert!(fixture
        .apply(&review, &[0, 1, 2])
        .unwrap()
        .receipt
        .is_some());
    fixture.provider.enter(|storage| {
        assert_eq!(
            RadicleRegistry::new(storage)
                .patch_execution_stage
                .read(&proposal)
                .unwrap(),
            crate::schema::PATCH_EXECUTION_REVIEWED
        )
    });
    let mut merge = review.clone();
    merge.action = PublicationAction::Merge;
    merge.sequence = 3;
    merge.previous_receipt_hash = review.receipt_hash();
    merge.finalized_anchor_height = 14;
    merge.finalized_anchor_hash = B256::repeat_byte(14);
    merge.package_manifest_hash = B256::repeat_byte(15);
    fixture.provider.set_block_number(15);
    fixture
        .provider
        .set_canonical_block_hash(14, B256::repeat_byte(14));
    assert!(fixture.apply(&merge, &[0, 1, 2]).unwrap().receipt.is_some());
    fixture.provider.enter(|storage| {
        let registry = RadicleRegistry::new(storage);
        assert_eq!(
            registry.patch_execution_stage.read(&proposal).unwrap(),
            crate::schema::PATCH_EXECUTION_MERGED
        );
        assert_eq!(
            registry.patch_approval(proposal).unwrap().unwrap().payload,
            String::from_utf8(payload.encode().unwrap()).unwrap()
        );
        assert_eq!(
            registry.publication_receipt(1).unwrap().unwrap().body,
            bootstrap
        );
        assert_eq!(
            registry.publication_receipt(2).unwrap().unwrap().body,
            review
        );
    });
}
