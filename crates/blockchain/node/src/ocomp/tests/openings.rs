use outbe_ocomp_protocol::test_utils::{account_mpt_with_proofs as account_trie, storage_trie};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicUsize, Arc},
};

use alloy_eips::BlockNumHash;
use alloy_primitives::{Address, B256, U256};
use alloy_trie::{TrieAccount, KECCAK_EMPTY};
use outbe_metadosis::config::poc_schema_limits;
use outbe_nod::openings::entry_price_slots;
use outbe_ocomp_protocol::{
    league_snapshot::{league_snapshot_slot, ordered_league_snapshot_slots},
    opening::OpeningSubjectsV1,
};
use outbe_primitives::addresses::{FIDELITY_ADDRESS, METADOSIS_ADDRESS, NOD_ADDRESS};
use outbe_primitives::time::WorldwideDay;
use reth_primitives_traits::Account;
use reth_trie::{AccountProof, StorageProof};

use crate::ocomp::{
    finality::{build_verified_raw_contract_opening, verify_raw_contract_opening},
    openings::build_lysis_openings,
    retention::CandidatePinV1,
};

type OpeningStateProvider = crate::test_utils::OpeningStateFixture<Arc<AtomicUsize>>;

type OpeningProvider = crate::test_utils::OpeningProviderFixture<Arc<AtomicUsize>>;

fn opening_state(contracts: &[(Address, Vec<(B256, U256)>)]) -> (OpeningStateProvider, B256) {
    let mut contract_tries = Vec::with_capacity(contracts.len());
    for (address, slots) in contracts {
        let words = slots
            .iter()
            .map(|(slot, value)| (U256::from_be_bytes(slot.0), *value))
            .collect::<Vec<_>>();
        let (storage_root, storage_proofs) = storage_trie(&words);
        contract_tries.push((
            *address,
            slots.clone(),
            TrieAccount {
                nonce: 1,
                balance: U256::from(10),
                storage_root,
                code_hash: KECCAK_EMPTY,
            },
            storage_proofs,
        ));
    }

    let trie_accounts = contract_tries
        .iter()
        .map(|(address, _, account, _)| (*address, *account))
        .collect::<Vec<_>>();
    let (state_root, account_proofs) = account_trie(&trie_accounts);
    let mut accounts = BTreeMap::new();
    let mut storage = BTreeMap::new();
    let mut proofs = BTreeMap::new();
    for (address, slots, trie_account, slot_proofs) in contract_tries {
        let account = Account {
            nonce: trie_account.nonce,
            balance: trie_account.balance,
            bytecode_hash: None,
        };
        accounts.insert(address, account);
        storage.extend(slots.iter().map(|(slot, value)| ((address, *slot), *value)));
        let storage_proofs = slots
            .iter()
            .zip(slot_proofs)
            .map(|((slot, value), nodes)| {
                StorageProof {
                    key: *slot,
                    value: *value,
                    ..StorageProof::new(*slot)
                }
                .with_proof(nodes)
            })
            .collect();
        proofs.insert(
            address,
            AccountProof {
                address,
                info: Some(account),
                proof: account_proofs[&address].clone(),
                storage_root: trie_account.storage_root,
                storage_proofs,
            },
        );
    }

    (
        OpeningStateProvider {
            state_root,
            accounts,
            storage,
            proofs,
            storage_reads: Arc::new(AtomicUsize::new(0)),
            block_lookup: crate::test_utils::NoBlockLookup,
        },
        state_root,
    )
}

#[test]
fn fidelity_and_oracle_openings_reject_mutated_values_shape_and_mpt_nodes() {
    let limits = poc_schema_limits();
    let fidelity_slots = vec![
        (B256::repeat_byte(0x11), U256::from(7)),
        (B256::repeat_byte(0x12), U256::ZERO),
    ];
    let oracle_slots = vec![
        (B256::repeat_byte(0x21), U256::from(840)),
        (B256::repeat_byte(0x22), U256::from(1_000)),
    ];
    let (state, state_root) = opening_state(&[
        (FIDELITY_ADDRESS, fidelity_slots.clone()),
        (NOD_ADDRESS, oracle_slots.clone()),
    ]);

    for (address, slots) in [
        (FIDELITY_ADDRESS, fidelity_slots),
        (NOD_ADDRESS, oracle_slots),
    ] {
        let ordered_slots = slots.iter().map(|(slot, _)| *slot).collect::<Vec<_>>();
        let opening = build_verified_raw_contract_opening(
            &state,
            state_root,
            address,
            &ordered_slots,
            &limits,
        )
        .expect("real account/storage MPT opening must build and self-verify");
        verify_raw_contract_opening(&opening, address, state_root, &ordered_slots, &limits)
            .expect("unaltered raw opening must verify");

        let mut wrong_value = opening.clone();
        wrong_value.ordered_slots[0].value += U256::from(1);
        assert!(
            verify_raw_contract_opening(
                &wrong_value,
                address,
                state_root,
                &ordered_slots,
                &limits,
            )
            .is_err(),
            "a changed raw value must invalidate its storage proof"
        );

        let mut missing_slot = opening.clone();
        missing_slot.ordered_slots.pop();
        assert!(
            verify_raw_contract_opening(
                &missing_slot,
                address,
                state_root,
                &ordered_slots,
                &limits,
            )
            .is_err(),
            "an omitted slot must invalidate the exact opening shape"
        );

        let mut reordered = opening.clone();
        reordered.ordered_slots.swap(0, 1);
        assert!(
            verify_raw_contract_opening(&reordered, address, state_root, &ordered_slots, &limits,)
                .is_err(),
            "slot order is part of the authenticated opening"
        );

        let mut corrupt_proof = opening;
        let byte = corrupt_proof
            .storage_proof
            .0
            .last_mut()
            .expect("fixture storage proof is non-empty");
        *byte ^= 1;
        assert!(
            verify_raw_contract_opening(
                &corrupt_proof,
                address,
                state_root,
                &ordered_slots,
                &limits,
            )
            .is_err(),
            "mutated MPT proof bytes must not verify"
        );
    }
}

#[test]
fn lysis_opening_builder_returns_league_snapshot_and_oracle_proofs() {
    let limits = poc_schema_limits();
    let owner = Address::repeat_byte(0x41);
    let subjects = OpeningSubjectsV1 {
        owners: vec![owner],
        reference_isos: vec![840],
    };
    let day = WorldwideDay::new(20260724);

    // The Fidelity opening is now a single per-owner league word in Metadosis
    // storage. Any value in [1, 4096] is a valid league.
    let fidelity_slots = vec![(league_snapshot_slot(day.value(), owner), U256::from(7))];

    let oracle_plan = entry_price_slots(day, &subjects.reference_isos).unwrap();
    let oracle_slots = oracle_plan
        .iter()
        .copied()
        .zip([U256::from(1), U256::from(320_000)])
        .collect();

    let (state, state_root) = opening_state(&[
        (METADOSIS_ADDRESS, fidelity_slots),
        (NOD_ADDRESS, oracle_slots),
    ]);
    let candidate = CandidatePinV1 {
        block_number: 100,
        block_hash: B256::repeat_byte(0x61),
        state_root,
        intent_id: B256::repeat_byte(0x62),
        wwd: day.value(),
        ce_sealed_root: B256::repeat_byte(0x63),
        protocol_bundle_hash: B256::repeat_byte(0x64),
        input_lease_id: B256::repeat_byte(0x65),
    };
    let openings = build_lysis_openings(
        &OpeningProvider {
            state,
            block: crate::test_utils::ExactBlockLookup {
                identity: BlockNumHash::new(candidate.block_number, candidate.block_hash),
            },
            exact_hash_message:
                "opening builder must request the candidate's exact finalized block hash",
        },
        &limits,
        candidate,
        subjects.clone(),
    )
    .expect("exact historical Fidelity and Oracle openings");

    let expected_fidelity_slots = ordered_league_snapshot_slots(day.value(), &subjects.owners);
    assert_eq!(openings.subjects, subjects);
    assert_eq!(openings.finalized_block_hash, candidate.block_hash);
    assert_eq!(openings.finalized_state_root, candidate.state_root);
    assert_eq!(openings.wwd, candidate.wwd);
    assert_eq!(
        openings
            .fidelity
            .ordered_slots
            .iter()
            .map(|raw| raw.slot)
            .collect::<Vec<_>>(),
        expected_fidelity_slots
    );
    assert_eq!(
        openings
            .oracle
            .ordered_slots
            .iter()
            .map(|raw| raw.slot)
            .collect::<Vec<_>>(),
        oracle_plan
    );
    verify_raw_contract_opening(
        &openings.fidelity,
        METADOSIS_ADDRESS,
        state_root,
        &expected_fidelity_slots,
        &limits,
    )
    .expect("Fidelity league proof must verify against the finalized root");
    verify_raw_contract_opening(
        &openings.oracle,
        NOD_ADDRESS,
        state_root,
        &oracle_plan,
        &limits,
    )
    .expect("Oracle proof must verify against the finalized root");
}
