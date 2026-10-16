use super::*;
use alloy_consensus::TrieAccount;
use alloy_primitives::{keccak256, Address};
use outbe_ocomp_protocol::test_utils::{account_mpt_with_proofs, storage_trie};
use outbe_primitives::{
    storage::{hashmap::HashMapStorageProvider, proof::StorageSlotProof},
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
};
use std::collections::BTreeMap;

struct Fixture {
    provider: HashMapStorageProvider,
    subject: TributeStateProofSubject,
    records: Vec<TributeRecordStateProof>,
    day_proof: AccountStorageProof,
}

fn limits() -> StateProofLimits {
    StateProofLimits {
        max_slots: 32,
        max_nodes: 512,
        max_bytes: 128 * 1024,
    }
}

fn fixture(count: u8, duplicate: bool, invalid_word: bool) -> Fixture {
    let mut subject = TributeStateProofSubject {
        checkpoint: FinalizedRequestBindingV1 {
            block_number: 42,
            block_hash: B256::repeat_byte(9),
            state_root: B256::repeat_byte(1),
        },
        chain_id: 54322345,
        worldwide_day: WorldwideDay::new(20261015),
        tribute_count: u32::from(count),
    };
    let mut provider = HashMapStorageProvider::new(subject.chain_id);
    let mut bodies = Vec::new();
    let mut selected = Vec::new();
    let day_length_slot = StorageHandle::enter(&mut provider, |storage| {
        let contract = TributeContract::new(storage.clone());
        for index in 0..count {
            let owner = Address::repeat_byte(index + 1);
            let tribute_id = derive_poseidon_entity_id(owner, subject.worldwide_day).unwrap();
            let body = outbe_tee_enclave::tribute_encryption::encrypt_tribute(
                &[0x52; 32],
                &[index + 0x35; 32],
                TributeContextV2 {
                    chain_id: subject.chain_id,
                    tribute_id,
                    owner,
                    worldwide_day: subject.worldwide_day,
                    issuance_currency: 840,
                    reference_currency: 840,
                    tribute_price_minor: U256::from(1_000_000),
                    offer_input_hash: B256::repeat_byte(index + 11),
                },
                &TributeAmountsV2 {
                    issuance_amount_minor: U256::from(123_000_000),
                    nominal_amount_minor: U256::from(100_000_000),
                },
            )
            .unwrap();
            contract
                .insert_state_record(&crate::storage_records::StoredTribute::from(&body))
                .unwrap();
            bodies.push(body);
        }
        if duplicate {
            contract
                .day_ids
                .list(&subject.worldwide_day)
                .set(1, bodies[0].context.tribute_id)
                .unwrap();
            bodies[1] = bodies[0].clone();
        }
        if invalid_word {
            let slot = contract
                .records
                .entry(bodies[0].context.tribute_id)
                .issuance_currency()
                .slot();
            storage.sstore(TRIBUTE_ADDRESS, slot, U256::MAX).unwrap();
        }
        for (index, body) in bodies.iter().enumerate() {
            selected.push(
                expected_record_words(&contract, &subject, index as u32, body)
                    .unwrap()
                    .into_iter()
                    .map(|(slot, _)| slot)
                    .collect::<Vec<_>>(),
            );
        }
        contract.day_ids.list(&subject.worldwide_day).base_slot()
    });
    let mut words = provider
        .storage
        .iter()
        .filter(|((address, _), _)| *address == TRIBUTE_ADDRESS)
        .map(|((_, slot), value)| (*slot, *value))
        .collect::<BTreeMap<_, _>>();
    words.entry(day_length_slot).or_insert(U256::ZERO);
    let words = words.into_iter().collect::<Vec<_>>();
    let (storage_root, storage_nodes) = storage_trie(&words);
    let account = TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root,
        code_hash: keccak256([]),
    };
    let (state_root, accounts) = account_mpt_with_proofs(&[(TRIBUTE_ADDRESS, account)]);
    subject.checkpoint.state_root = state_root;
    let openings = words
        .into_iter()
        .zip(storage_nodes)
        .map(|((slot, value), nodes)| {
            let slot = B256::from(slot.to_be_bytes::<32>());
            (slot, StorageSlotProof { slot, value, nodes })
        })
        .collect::<BTreeMap<_, _>>();
    let proof_for = |slots: &[B256]| AccountStorageProof {
        address: TRIBUTE_ADDRESS,
        nonce: account.nonce,
        balance: account.balance,
        code_hash: account.code_hash,
        storage_root,
        account_nodes: accounts[&TRIBUTE_ADDRESS].clone(),
        slots: slots.iter().map(|slot| openings[slot].clone()).collect(),
    };
    Fixture {
        provider,
        subject,
        day_proof: proof_for(&[B256::from(day_length_slot.to_be_bytes::<32>())]),
        records: bodies
            .into_iter()
            .zip(selected)
            .enumerate()
            .map(|(index, (tribute, slots))| TributeRecordStateProof {
                day_index: index as u32,
                tribute,
                account: proof_for(&slots),
            })
            .collect(),
    }
}

#[test]
fn complete_day_opens_real_state_without_ce_and_includes_every_ciphertext_word() {
    let fixture = fixture(2, false, false);
    let mut verifier =
        TributeDayProofVerifier::new(fixture.subject, &fixture.day_proof, limits()).unwrap();
    assert!(verifier.finish().is_err());
    for proof in &fixture.records {
        assert_eq!(proof.account.slots.len(), 17);
        verifier.verify_next(proof, limits()).unwrap();
    }
    verifier.finish().unwrap();
    assert!(verifier.verify_next(&fixture.records[0], limits()).is_err());
}

#[test]
fn empty_day_requires_a_valid_length_opening() {
    let fixture = fixture(0, false, false);
    TributeDayProofVerifier::new(fixture.subject, &fixture.day_proof, limits())
        .unwrap()
        .finish()
        .unwrap();
    let mut omitted = fixture.day_proof.clone();
    omitted.slots.clear();
    assert!(TributeDayProofVerifier::new(fixture.subject, &omitted, limits()).is_err());
    let mut wrong_count = fixture.subject;
    wrong_count.tribute_count = 1;
    assert!(TributeDayProofVerifier::new(wrong_count, &fixture.day_proof, limits()).is_err());
}

#[test]
fn omissions_reordering_and_duplicate_identities_do_not_advance_the_cursor() {
    let fixture = fixture(2, false, false);
    let mut verifier =
        TributeDayProofVerifier::new(fixture.subject, &fixture.day_proof, limits()).unwrap();
    assert!(verifier.verify_next(&fixture.records[1], limits()).is_err());
    verifier.verify_next(&fixture.records[0], limits()).unwrap();
    assert!(verifier.verify_next(&fixture.records[0], limits()).is_err());
    assert!(verifier.finish().is_err());
    verifier.verify_next(&fixture.records[1], limits()).unwrap();
    verifier.finish().unwrap();

    let duplicate = self::fixture(2, true, false);
    let mut verifier =
        TributeDayProofVerifier::new(duplicate.subject, &duplicate.day_proof, limits()).unwrap();
    verifier
        .verify_next(&duplicate.records[0], limits())
        .unwrap();
    assert!(verifier
        .verify_next(&duplicate.records[1], limits())
        .is_err());
    assert!(verifier.finish().is_err());
}

#[test]
fn every_omitted_slot_or_forged_value_is_rejected() {
    let fixture = fixture(1, false, false);
    let original = &fixture.records[0];
    for index in 0..original.account.slots.len() {
        let mut omitted = original.clone();
        omitted.account.slots.remove(index);
        assert!(omitted.verify(&fixture.subject, limits()).is_err());
        let mut forged = original.clone();
        forged.account.slots[index].value ^= U256::from(1);
        assert!(forged.verify(&fixture.subject, limits()).is_err());
    }
    let mut truncated = original.clone();
    truncated.account.account_nodes.clear();
    assert!(truncated.verify(&fixture.subject, limits()).is_err());
}

#[test]
fn wrong_context_ciphertext_checkpoint_and_limits_are_rejected() {
    let fixture = fixture(1, false, false);
    let original = &fixture.records[0];
    for mutate in [
        (|body: &mut EncryptedTributeV2| body.context.chain_id += 1) as fn(&mut EncryptedTributeV2),
        |body| body.context.worldwide_day = WorldwideDay::new(20261016),
        |body| body.context.owner = Address::repeat_byte(7),
        |body| body.context.reference_currency = 978,
        |body| body.encrypted_creator_public_key[20] ^= 1,
        |body| body.encrypted_amounts[40] ^= 1,
        |body| body.encrypted_amounts.pop().map(|_| ()).unwrap(),
    ] {
        let mut forged = original.clone();
        mutate(&mut forged.tribute);
        assert!(forged.verify(&fixture.subject, limits()).is_err());
    }
    let mut wrong_root = fixture.subject;
    wrong_root.checkpoint.state_root = B256::repeat_byte(0x55);
    assert!(original.verify(&wrong_root, limits()).is_err());
    let mut wrong_count = fixture.subject;
    wrong_count.tribute_count += 1;
    assert!(original.verify(&wrong_count, limits()).is_err());
    assert!(original
        .verify(
            &fixture.subject,
            StateProofLimits {
                max_slots: 16,
                ..limits()
            }
        )
        .is_err());
}

#[test]
fn authenticated_noncanonical_integer_is_rejected_without_narrowing() {
    let fixture = fixture(1, false, true);
    fixture.records[0]
        .account
        .verify(
            TRIBUTE_ADDRESS,
            fixture.subject.checkpoint.state_root,
            limits(),
        )
        .unwrap();
    assert!(fixture.records[0]
        .verify(&fixture.subject, limits())
        .is_err());
}

fn prepared_request(
    fixture: &Fixture,
) -> outbe_tee::nod_state_materialization::PrepareStateNodsRequest {
    use outbe_ocomp_protocol::{
        nod_materialization::{NodMaterializationBatchV1, NodMaterializationHeadV1},
        profile::poc_schema_limits,
        result::NodActionV1,
        test_utils::nod_action_population,
    };
    use outbe_tee::{
        nod_materialization::LeagueFractionInputV2,
        nod_state_materialization::{PrepareStateNodsRequest, StateNodAuthority},
        tribute_source::{TributeSourceCheckpoint, TributeSourceSubject},
    };
    let limits = poc_schema_limits();
    let actions = fixture
        .records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let context = &record.tribute.context;
            NodActionV1 {
                raw_ordinal: index as u32,
                tribute_id: B256::from_slice(context.tribute_id.as_slice()),
                nod_id: B256::from_slice(context.tribute_id.as_slice()),
                owner: context.owner,
                wwd: context.worldwide_day.value(),
                league_id: 1,
                entry_price_minor: U256::from(1_000_000),
                issuance_currency: context.issuance_currency,
                reference_currency: context.reference_currency,
            }
        })
        .collect::<Vec<_>>();
    let (nod_root, _) = nod_action_population(&actions);
    let head = NodMaterializationHeadV1 {
        queue_sequence: 1,
        job_id: B256::repeat_byte(0x11),
        program_semantics_hash: B256::repeat_byte(0x22),
        worldwide_day: fixture.subject.worldwide_day.value(),
        generation: 1,
        nod_root,
        nod_count: actions.len() as u32,
        next_nod_ordinal: 0,
        last_progress_height: 50,
    };
    let mut provider = HashMapStorageProvider::new(fixture.subject.chain_id);
    let sources = StorageHandle::enter(&mut provider, |storage| {
        let contract = TributeContract::new(storage);
        fixture
            .records
            .iter()
            .map(|proof| proof.source_opening(&contract, &fixture.subject).unwrap())
            .collect::<Vec<_>>()
    });
    let source_bindings = sources
        .iter()
        .map(|source| source.binding().unwrap())
        .collect();
    PrepareStateNodsRequest {
        authority: StateNodAuthority {
            source: TributeSourceSubject {
                checkpoint: TributeSourceCheckpoint {
                    block_number: fixture.subject.checkpoint.block_number,
                    block_hash: fixture.subject.checkpoint.block_hash,
                    state_root: fixture.subject.checkpoint.state_root,
                },
                chain_id: fixture.subject.chain_id,
                worldwide_day: fixture.subject.worldwide_day,
                tribute_count: fixture.subject.tribute_count,
            },
            head: head.encode_canonical(&limits).unwrap(),
            subtree_height: 1,
            source_bindings,
        },
        batch: NodMaterializationBatchV1 {
            queue_sequence: 1,
            first_nod_ordinal: 0,
            actions,
            root_path: vec![],
        }
        .encode_canonical(&limits)
        .unwrap(),
        league_fractions: vec![LeagueFractionInputV2 {
            league_id: 1,
            fraction_fp: U256::from(1_000_000),
        }],
        sources,
    }
}

#[test]
fn enclave_requests_sources_and_opens_mpt_backed_nods_without_a_reader() {
    let fixture = fixture(2, false, false);
    let request = prepared_request(&fixture);
    let mut query = request.clone();
    query.sources.clear();
    query.authority.source_bindings.clear();
    let lookup = outbe_tee_enclave::nod_state_materialization::request_sources(&query).unwrap();
    assert_eq!(lookup.subject, request.authority.source);
    assert_eq!(
        lookup.tribute_ids,
        fixture
            .records
            .iter()
            .map(|record| { record.tribute.context.tribute_id })
            .collect::<Vec<_>>()
    );
    let carrier =
        outbe_tee_enclave::nod_state_materialization::prepare(&[0x52; 32], &request).unwrap();
    let opened = outbe_tee_enclave::nod_state_materialization::open(
        &[0x52; 32],
        &request.authority,
        &carrier,
    )
    .unwrap();
    assert_eq!(opened.len(), 2);
    for nod in opened {
        assert_eq!(
            outbe_tee_enclave::nod_encryption::decrypt_nod(&[0x52; 32], &nod).unwrap(),
            U256::from(100_000_000)
        );
    }
    let mut wrong_authority = request.authority.clone();
    wrong_authority.source_bindings[0] = B256::repeat_byte(0x77);
    assert!(outbe_tee_enclave::nod_state_materialization::open(
        &[0x52; 32],
        &wrong_authority,
        &carrier
    )
    .is_err());
    let mut changed = carrier.clone();
    changed.encrypted_nods.swap(0, 1);
    assert!(outbe_tee_enclave::nod_state_materialization::open(
        &[0x52; 32],
        &request.authority,
        &changed
    )
    .is_err());
}

#[test]
fn enclave_rejects_altered_state_source_and_checkpoint() {
    let fixture = fixture(1, false, false);
    let original = prepared_request(&fixture);
    for mutate in [
        (|request: &mut outbe_tee::nod_state_materialization::PrepareStateNodsRequest| {
            request.sources[0].expected_words[0].1 ^= U256::from(1);
        }) as fn(&mut outbe_tee::nod_state_materialization::PrepareStateNodsRequest),
        |request| {
            request.sources[0].proof.slots.remove(0);
        },
        |request| {
            request.sources[0].tribute.encrypted_amounts[20] ^= 1;
        },
        |request| {
            request.authority.source.checkpoint.state_root = B256::repeat_byte(0x44);
        },
        |request| {
            request.authority.source_bindings.clear();
        },
    ] {
        let mut changed = original.clone();
        mutate(&mut changed);
        assert!(
            outbe_tee_enclave::nod_state_materialization::prepare(&[0x52; 32], &changed).is_err()
        );
    }
}

#[test]
fn retained_source_binding_survives_logical_retirement_and_rejects_index_changes() {
    let mut fixture = fixture(2, false, false);
    let record = &fixture.records[0];
    let id = record.tribute.context.tribute_id;
    StorageHandle::enter(&mut fixture.provider, |storage| {
        let contract = TributeContract::new(storage.clone());
        let opening = record.source_opening(&contract, &fixture.subject).unwrap();
        let expected = opening.binding().unwrap();
        assert_eq!(
            retained_source_binding(&storage, &fixture.subject, id).unwrap(),
            expected
        );
        contract
            .retired_days
            .write(&fixture.subject.worldwide_day, true)
            .unwrap();
        assert!(crate::state_api::get_record(&storage, id)
            .unwrap()
            .is_none());
        assert_eq!(
            retained_source_binding(&storage, &fixture.subject, id).unwrap(),
            expected
        );
        contract.day_id_position.write(&id, 2).unwrap();
        assert!(retained_source_binding(&storage, &fixture.subject, id).is_err());
        contract.day_id_position.write(&id, 1).unwrap();
        contract
            .day_ids
            .list(&fixture.subject.worldwide_day)
            .set(0, fixture.records[1].tribute.context.tribute_id)
            .unwrap();
        assert!(retained_source_binding(&storage, &fixture.subject, id).is_err());
    });
}
