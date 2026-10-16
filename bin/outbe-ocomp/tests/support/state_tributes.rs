//! Encrypted Tribute populations with actual EVM account and storage proofs.

use std::{collections::BTreeMap, path::Path};

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_ocomp::input_inventory::{
    StateTributeProofArchive, StateTributeProofArchiveBuilder, TributeInventoryRecordV1,
    TributeInventorySubjectV1, SOURCE_PROOF_ARCHIVE_DIRECTORY,
};
use outbe_ocomp_protocol::{
    input::CheckpointIdentityV1,
    test_utils::{account_mpt_with_proofs, storage_trie},
};
use outbe_primitives::{
    addresses::TRIBUTE_ADDRESS,
    storage::{
        hashmap::HashMapStorageProvider,
        proof::{AccountStorageProof, StorageSlotProof},
        StorageHandle,
    },
    time::WorldwideDay,
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
    wwd_entity_id::derive_poseidon_entity_id,
};
use outbe_tee::tribute_source::{TributeSourceCheckpoint, TributeSourceSubject};
use outbe_tribute::{
    state_proof::{expected_record_words, TributeRecordStateProof},
    storage_records::StoredTribute,
    TributeContract,
};

pub struct StateTributes {
    pub subject: TributeInventorySubjectV1,
    pub sources: StateTributeProofArchive,
    pub records: Vec<TributeInventoryRecordV1>,
}

pub fn state_tributes(root: &Path, count: u32, reference_iso: Option<u16>) -> StateTributes {
    let mut subject = TributeSourceSubject {
        checkpoint: TributeSourceCheckpoint {
            block_number: 100,
            block_hash: B256::repeat_byte(3),
            state_root: B256::repeat_byte(4),
        },
        chain_id: 1,
        worldwide_day: WorldwideDay::new(20260901),
        tribute_count: count,
    };
    let mut provider = HashMapStorageProvider::new(subject.chain_id);
    let (bodies, selected, length_slot) = StorageHandle::enter(&mut provider, |storage| {
        let contract = TributeContract::new(storage);
        let mut bodies = Vec::new();
        for index in 0..count {
            let owner = Address::from_word(B256::from(U256::from(index + 1)));
            let body = outbe_tee_enclave::tribute_encryption::encrypt_tribute(
                &outbe_tribute::enclave_client::test_enclave::NETWORK_SECRET,
                &outbe_tee_enclave::crypto::x25519_public(&[0x77; 32]),
                TributeContextV2 {
                    chain_id: subject.chain_id,
                    tribute_id: derive_poseidon_entity_id(owner, subject.worldwide_day).unwrap(),
                    owner,
                    worldwide_day: subject.worldwide_day,
                    issuance_currency: 840,
                    reference_currency: reference_iso.unwrap_or(if index % 2 == 0 {
                        978
                    } else {
                        840
                    }),
                    tribute_price_minor: U256::from(index + 2),
                    offer_input_hash: keccak256(index.to_be_bytes()),
                },
                &TributeAmountsV2 {
                    issuance_amount_minor: U256::from(index + 1),
                    nominal_amount_minor: U256::ONE,
                },
            )
            .unwrap();
            contract
                .insert_state_record(&StoredTribute::from(&body))
                .unwrap();
            bodies.push(body);
        }
        let selected = bodies
            .iter()
            .enumerate()
            .map(|(index, body)| {
                expected_record_words(&contract, &subject.into(), index as u32, body)
                    .unwrap()
                    .into_iter()
                    .map(|(slot, _)| slot)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        (
            bodies,
            selected,
            contract.day_ids.list(&subject.worldwide_day).base_slot(),
        )
    });
    let mut words = provider
        .storage
        .iter()
        .filter(|((address, _), _)| *address == TRIBUTE_ADDRESS)
        .map(|((_, slot), value)| (*slot, *value))
        .collect::<BTreeMap<_, _>>();
    // An empty day still needs an authenticated absence proof for its length.
    words.entry(length_slot).or_insert(U256::ZERO);
    let words = words.into_iter().collect::<Vec<_>>();
    let (storage_root, nodes) = storage_trie(&words);
    let account = alloy_consensus::TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root,
        code_hash: keccak256([]),
    };
    let (root_hash, accounts) = account_mpt_with_proofs(&[(TRIBUTE_ADDRESS, account)]);
    subject.checkpoint.state_root = root_hash;
    let slots = words
        .into_iter()
        .zip(nodes)
        .map(|((slot, value), nodes)| {
            let slot = B256::from(slot.to_be_bytes::<32>());
            (slot, StorageSlotProof { slot, value, nodes })
        })
        .collect::<BTreeMap<_, _>>();
    let proof = |selected: &[B256]| AccountStorageProof {
        address: TRIBUTE_ADDRESS,
        nonce: account.nonce,
        balance: account.balance,
        storage_root,
        code_hash: account.code_hash,
        account_nodes: accounts[&TRIBUTE_ADDRESS].clone(),
        slots: selected.iter().map(|slot| slots[slot].clone()).collect(),
    };
    let mut archive = StateTributeProofArchiveBuilder::create(
        root.join(SOURCE_PROOF_ARCHIVE_DIRECTORY),
        subject,
        proof(&[B256::from(length_slot.to_be_bytes::<32>())]),
    )
    .unwrap();
    let mut records = Vec::new();
    for (index, (body, selected)) in bodies.into_iter().zip(selected).enumerate() {
        let source = TributeRecordStateProof {
            day_index: index as u32,
            tribute: body.clone(),
            account: proof(&selected),
        };
        archive.push(&source).unwrap();
        records.push(TributeInventoryRecordV1 {
            tribute_id: body.context.tribute_id,
            owner: body.context.owner,
            reference_iso: body.context.reference_currency,
            canonical_body: outbe_primitives::entity_codec::encode_tribute_v2(&body).unwrap(),
        });
    }
    records.sort_by_key(|record| record.tribute_id);
    StateTributes {
        subject: TributeInventorySubjectV1 {
            protocol_bundle_hash: B256::repeat_byte(1),
            job_id: B256::repeat_byte(2),
            attempt: 0,
            checkpoint: CheckpointIdentityV1 {
                finalized_block_number: subject.checkpoint.block_number,
                finalized_block_hash: subject.checkpoint.block_hash,
                finalized_state_root: subject.checkpoint.state_root,
            },
            chain_id: subject.chain_id,
            worldwide_day: subject.worldwide_day,
            expected_tribute_count: count,
            expected_nominal_total: U256::from(count),
        },
        sources: archive.finish().unwrap(),
        records,
    }
}
