use super::*;
use crate::test_support::{change_note, merge_proof, spend_proof, Note};
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::PAYNOTE_ADDRESS;
use outbe_zk_canonical::paynote_merge::{
    decode_public_inputs, encode_combined_proof, MAX_MERGE_INPUTS,
};

fn fixture(count: usize) -> (Vec<Note>, Note, PayNoteTree, Vec<u8>) {
    let inputs = [12, 8, 5, 7]
        .into_iter()
        .take(count)
        .enumerate()
        .map(|(i, amount)| note(CHAIN_ID, 17 + i as u64, USDC, U256::from(amount)))
        .collect::<Vec<_>>();
    let output = note(CHAIN_ID, 99, USDC, inputs.iter().map(|n| n.amount).sum());
    let mut tree = new_tree(CHAIN_ID).unwrap();
    for input in &inputs {
        tree.append(input.commitment).unwrap();
    }
    let proof = merge_proof(CHAIN_ID, &tree, &inputs.iter().collect::<Vec<_>>(), &output);
    (inputs, output, tree, proof)
}

#[test]
fn merge_dispatch_creates_an_ordinary_private_note_and_shared_nullifiers() {
    let (inputs, output, mut tree, proof) = fixture(3);
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    seed_pool(&mut provider, CHAIN_ID, tree.leaves());
    provider.enter(|storage| {
        let data = IPayNote::mergePayNotesCall {
            proof: proof.clone().into(),
        }
        .abi_encode();
        assert_eq!(base_gas(&data), crate::precompile::PAYNOTE_MERGE_BASE_GAS);
        dispatch(storage.clone(), &data, WBTC, U256::ZERO).unwrap(); // unrelated relayer
        let c: PayNoteContract<'_> = storage.contract();
        for n in &inputs {
            assert!(c.spent_nullifiers.read(&b256(n.nullifier)).unwrap());
        }
        assert_eq!(c.leaf_count.read().unwrap(), 4);
        assert!(c.commitments.read(&b256(output.commitment)).unwrap());
        assert!(runtime::merge_pay_notes(&storage, &proof).is_err());
        let max = dispatch(
            storage,
            &IPayNote::maxMergeInputsCall {}.abi_encode(),
            ALICE,
            U256::ZERO,
        )
        .unwrap();
        assert_eq!(
            IPayNote::maxMergeInputsCall::abi_decode_returns(&max).unwrap(),
            4
        );
    });
    let events = &provider.events[&PAYNOTE_ADDRESS];
    let merged = IPayNote::NotesMerged::decode_log_data_validate(&events[0]).unwrap();
    assert_eq!(
        merged.nullifiers,
        inputs.iter().map(|n| b256(n.nullifier)).collect::<Vec<_>>()
    );
    let new = IPayNote::NewNote::decode_log_data_validate(&events[1]).unwrap();
    assert_eq!(new.noteAmount, U256::ZERO);
    assert_eq!(output.amount, U256::from(25));
    tree.append(output.commitment).unwrap();
    assert_eq!(new.rootAfter, b256(tree.root()));
    let spend = spend_proof(CHAIN_ID, &tree, 3, &output, OWNER, U256::from(20));
    provider.enter(|storage| {
        assert_eq!(
            runtime::consume(&storage, &spend).unwrap().spend_amount,
            U256::from(20)
        );
    });
    let change = change_note(CHAIN_ID, &output, U256::from(20)).unwrap();
    assert_eq!(change.amount, U256::from(5));
    tree.append(change.commitment).unwrap();
    let extra = note(CHAIN_ID, 100, USDC, U256::from(7));
    // Seed one additional already-funded note while retaining the real merge/spend history.
    provider.enter(|storage| {
        let c: PayNoteContract<'_> = storage.contract();
        runtime::append(
            &c,
            &empty_subtrees(CHAIN_ID, PAYNOTE_TREE_DEPTH).unwrap(),
            extra.commitment,
        )
        .unwrap();
        c.commitments.write(&b256(extra.commitment), true).unwrap();
    });
    tree.append(extra.commitment).unwrap();
    let merged_again = note(CHAIN_ID, 101, USDC, U256::from(12));
    let again = merge_proof(CHAIN_ID, &tree, &[&change, &extra], &merged_again);
    provider.enter(|storage| runtime::merge_pay_notes(&storage, &again).unwrap());
}

#[test]
fn merge_and_settlement_compete_in_both_orders_and_merge_competes_with_merge() {
    let (inputs, output, tree, proof) = fixture(2);
    let spend = spend_proof(CHAIN_ID, &tree, 0, &inputs[0], OWNER, inputs[0].amount);
    let alternate = note(CHAIN_ID, 100, USDC, output.amount);
    let competing = merge_proof(
        CHAIN_ID,
        &tree,
        &inputs.iter().collect::<Vec<_>>(),
        &alternate,
    );
    for merge_first in [false, true] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed_pool(&mut provider, CHAIN_ID, tree.leaves());
        provider.enter(|storage| {
            if merge_first {
                runtime::merge_pay_notes(&storage, &proof).unwrap();
                assert!(runtime::consume(&storage, &spend).is_err());
                assert!(runtime::merge_pay_notes(&storage, &competing).is_err());
            } else {
                runtime::consume(&storage, &spend).unwrap();
                assert!(runtime::merge_pay_notes(&storage, &proof).is_err());
                let c: PayNoteContract<'_> = storage.contract();
                assert!(!c.spent_nullifiers.read(&b256(inputs[1].nullifier)).unwrap());
                assert!(!c.commitments.read(&b256(output.commitment)).unwrap());
            }
        });
    }
}

#[test]
fn every_merge_mutation_and_event_rolls_back_on_failure() {
    let (_, _, tree, proof) = fixture(MAX_MERGE_INPUTS);
    let mut baseline = HashMapStorageProvider::new(CHAIN_ID);
    seed_pool(&mut baseline, CHAIN_ID, tree.leaves());
    baseline.clear_mutation_failure();
    baseline.enter(|storage| runtime::merge_pay_notes(&storage, &proof).unwrap());
    let mutations = baseline.clear_mutation_failure();
    assert!(mutations > PAYNOTE_TREE_DEPTH + MAX_MERGE_INPUTS);
    for point in 0..mutations {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed_pool(&mut provider, CHAIN_ID, tree.leaves());
        let before = provider.storage.clone();
        let events = provider.events.clone();
        provider.fail_after_mutation_at(point);
        provider.enter(|storage| {
            assert!(
                runtime::merge_pay_notes(&storage, &proof).is_err(),
                "mutation {point}"
            )
        });
        assert_eq!(provider.storage, before, "mutation {point}");
        assert_eq!(provider.events, events, "event at mutation {point}");
    }
}

#[test]
fn malformed_statements_tree_capacity_and_duplicate_output_leave_inputs_unspent() {
    let (_, output, tree, proof) = fixture(3);
    let public = decode_public_inputs(&proof).unwrap();
    let offset = 4 + outbe_zk_canonical::paynote_merge::PUBLIC_INPUT_COUNT * 32;
    let proof_words = proof[offset..]
        .chunks_exact(32)
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    let mut cases = vec![vec![], proof[..proof.len() - 1].to_vec()];
    for count in [0, 1, 5] {
        let mut bad = public.clone();
        bad.input_count = count;
        cases.push(encode_combined_proof(bad, proof_words.clone()).unwrap());
    }
    for kind in 0..8 {
        let mut bad = public.clone();
        match kind {
            0 => bad.chain_id += 1,
            1 => bad.pool = Field::from(0x1018),
            2 => bad.asset = Field::from(0),
            3 => bad.nullifiers[1] = bad.nullifiers[0],
            4 => bad.nullifiers[3] = Field::from(1),
            5 => bad.root = Field::from(1),
            6 => bad.output_commitment += Field::from(1),
            _ => bad.nullifiers[0] = Field::from(0),
        }
        cases.push(encode_combined_proof(bad, proof_words.clone()).unwrap());
    }
    for candidate in cases {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed_pool(&mut provider, CHAIN_ID, tree.leaves());
        let before = provider.storage.clone();
        provider.enter(|storage| assert!(runtime::merge_pay_notes(&storage, &candidate).is_err()));
        assert_eq!(before, provider.storage);
    }
    for tree_full in [false, true] {
        let mut provider = HashMapStorageProvider::new(CHAIN_ID);
        seed_pool(&mut provider, CHAIN_ID, tree.leaves());
        provider.enter(|storage| {
            let c: PayNoteContract<'_> = storage.contract();
            if tree_full {
                c.leaf_count.write(PAYNOTE_TREE_CAPACITY).unwrap();
            } else {
                c.commitments.write(&b256(output.commitment), true).unwrap();
            }
        });
        let before = provider.storage.clone();
        provider.enter(|storage| assert!(runtime::merge_pay_notes(&storage, &proof).is_err()));
        assert_eq!(before, provider.storage);
    }
}
