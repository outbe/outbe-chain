use super::*;
use alloy_consensus::TrieAccount;
use alloy_primitives::{keccak256, Address};
use outbe_primitives::test_utils::state_proof::{account_mpt_with_proofs, storage_trie};
use outbe_primitives::{
    storage::{hashmap::HashMapStorageProvider, proof::StorageSlotProof},
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
};
use std::collections::BTreeMap;

struct Fixture {
    provider: HashMapStorageProvider,
    subject: TributeStateProofSubject,
    records: Vec<TributeRecordStateProof>,
}

fn limits() -> StateProofLimits {
    StateProofLimits {
        max_slots: 32,
        max_nodes: 512,
        max_bytes: 128 * 1024,
    }
}

fn fixture(count: u8, invalid_word: bool) -> Fixture {
    let mut subject = TributeStateProofSubject {
        checkpoint: FinalizedStateCheckpoint {
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
            let tribute_id = derive_poseidon_entity_id(owner, subject.worldwide_day);
            let body = outbe_tee_enclave::tribute_encryption::encrypt_tribute(
                &[0x52; 32],
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
fn every_omitted_slot_or_forged_value_is_rejected() {
    let fixture = fixture(1, false);
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
    let fixture = fixture(1, false);
    let original = &fixture.records[0];
    for mutate in [
        (|body: &mut EncryptedTributeV2| body.context.chain_id += 1) as fn(&mut EncryptedTributeV2),
        |body| body.context.worldwide_day = WorldwideDay::new(20261016),
        |body| body.context.owner = Address::repeat_byte(7),
        |body| body.context.reference_currency = 978,
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
                max_slots: 13,
                ..limits()
            }
        )
        .is_err());
}

#[test]
fn authenticated_noncanonical_integer_is_rejected_without_narrowing() {
    let fixture = fixture(1, true);
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

#[test]
fn retained_source_binding_survives_logical_retirement_and_rejects_index_changes() {
    let mut fixture = fixture(2, false);
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
