use super::super::tests::{event, private_tempdir, tree_rpc, ASSET, CHAIN, KEY};
use super::*;
use crate::rpc::mock::{
    abi_u64, ExpectedRpcCall as Call, RecordedRpcCall as Request, RecordedRpcResponse as Response,
    RecordingRpc,
};

fn notes(count: usize) -> Vec<Note> {
    (0..count)
        .map(|i| Note::new(CHAIN, ASSET, U256::from(i + 1), Field::from(17 + i as u64)).unwrap())
        .collect()
}
fn snapshot_calls(hash: u8) -> Vec<Call> {
    vec![
        Call::ok(Request::EthChainId, Response::U64(CHAIN)),
        Call::ok(Request::EthBlockNumber, Response::U64(10)),
        block_call(hash),
    ]
}
fn block_call(hash: u8) -> Call {
    Call::ok(
        Request::EthGetBlockByNumber { block: 10 },
        Response::Value(json!({"hash": B256::repeat_byte(hash)})),
    )
}
fn state_calls(note: &Note, present: bool, spent: bool) -> Vec<Call> {
    [
        (
            IPayNote::hasCommitmentCall {
                commitment: note.commitment,
            }
            .abi_encode(),
            present,
        ),
        (
            IPayNote::isSpentCall {
                nullifier: note.nullifier().unwrap(),
            }
            .abi_encode(),
            spent,
        ),
    ]
    .into_iter()
    .map(|(data, value)| {
        Call::ok(
            Request::EthCallAt {
                to: PAYNOTE_ADDRESS,
                data,
                block_tag: "0xa".into(),
            },
            Response::Bytes(abi_u64(u64::from(value))),
        )
    })
    .collect()
}

#[test]
fn bounded_plan_conserves_full_sum_and_persists_every_secret_before_submission() {
    let dir = private_tempdir();
    let op = Operation::new(notes(9)).unwrap();
    assert_eq!(op.stages.len(), 3);
    assert_eq!(
        op.stages.iter().map(|s| s.inputs.len()).collect::<Vec<_>>(),
        [4, 4, 3]
    );
    assert_eq!(op.stages.last().unwrap().output.amount, U256::from(45));
    let path = op.save(dir.path()).unwrap();
    let recovered = load_operation(&path).unwrap();
    for stage in &recovered.stages {
        assert_eq!(sum_inputs(&stage.inputs).unwrap(), stage.output.amount);
        for n in stage.inputs.iter().chain(std::iter::once(&stage.output)) {
            assert!(
                load_note(&resolve_note(dir.path(), &format!("{:#x}", n.commitment))).unwrap()
                    == *n
            );
        }
    }
    assert_eq!(op.save(dir.path()).unwrap(), path);
    let mut bad = recovered;
    bad.stages[1].inputs[0] = notes(1).remove(0);
    assert!(bad.validate().is_err());
}

#[test]
fn selection_rejects_duplicates_mixed_domains_and_overflow_before_saving() {
    let mut input = notes(2);
    input[1] = input[0].clone();
    assert!(Operation::new(input).is_err());
    for kind in 0..3 {
        let mut input = notes(2);
        input[1] = Note::new(
            if kind == 0 { CHAIN + 1 } else { CHAIN },
            if kind == 1 {
                Address::repeat_byte(0x44)
            } else {
                ASSET
            },
            if kind == 2 { U256::MAX } else { U256::ONE },
            Field::from(44),
        )
        .unwrap();
        assert!(Operation::new(input).is_err());
    }
    let mut input = notes(2);
    input[1] = Note::new(CHAIN, ASSET, U256::MAX - U256::ONE, Field::from(44)).unwrap();
    assert_eq!(
        Operation::new(input).unwrap().stages[0].output.amount,
        U256::MAX
    );
}

#[tokio::test]
async fn restart_reconciles_pending_success_conflict_and_reorganization_without_deleting_notes() {
    let dir = private_tempdir();
    let op = Operation::new(notes(2)).unwrap();
    let path = op.save(dir.path()).unwrap();
    let stage = &op.stages[0];
    for (present, spent, pending) in [
        (false, [false, false], 2),
        (true, [true, true], 0),
        (false, [true, false], 0),
    ] {
        let mut calls = snapshot_calls(1);
        for (n, used) in stage.inputs.iter().zip(spent) {
            calls.extend(state_calls(n, true, used));
        }
        calls.extend(state_calls(&stage.output, present, false));
        calls.push(block_call(1));
        let rpc = RecordingRpc::new(calls);
        let snapshot = Snapshot::read(&rpc).await.unwrap();
        let reservations = reservations(&rpc, dir.path(), &snapshot).await.unwrap();
        assert_eq!(reservations.len(), pending);
        snapshot.check(&rpc).await.unwrap();
        rpc.assert_done();
        assert!(load_operation(&path).is_ok());
        for n in stage.inputs.iter().chain(std::iter::once(&stage.output)) {
            assert!(load_note(&resolve_note(dir.path(), &format!("{:#x}", n.commitment))).is_ok());
        }
    }
    let mut calls = snapshot_calls(1);
    calls.push(block_call(2));
    let rpc = RecordingRpc::new(calls);
    assert!(Snapshot::read(&rpc)
        .await
        .unwrap()
        .check(&rpc)
        .await
        .is_err());
    rpc.assert_done();
}

#[tokio::test]
async fn resume_skips_a_confirmed_stage_even_when_receipt_was_lost() {
    let dir = private_tempdir();
    let op = Operation::new(notes(2)).unwrap();
    let path = op.save(dir.path()).unwrap();
    let stage = &op.stages[0];
    let mut calls = snapshot_calls(1);
    calls.extend(state_calls(&stage.output, true, false));
    for n in &stage.inputs {
        calls.extend(state_calls(n, true, true));
    }
    calls.push(block_call(1));
    let rpc = RecordingRpc::new(calls);
    let result = run(
        &rpc,
        &TxSigner::new(KEY).unwrap(),
        dir.path(),
        &[],
        Some(&path),
    )
    .await
    .unwrap();
    assert_eq!(result["commitment"], json!(stage.output.commitment));
    rpc.assert_done(); // No new proof, signature or submission.
}

#[tokio::test]
async fn failed_proof_or_submission_keeps_recoverable_keys() {
    let dir = private_tempdir();
    let op = Operation::new(notes(2)).unwrap();
    let path = op.save(dir.path()).unwrap();
    let stage = &op.stages[0];
    let mut calls = snapshot_calls(1);
    calls.extend(state_calls(&stage.output, false, false));
    for n in &stage.inputs {
        calls.extend(state_calls(n, true, false));
    }
    calls.push(block_call(1));
    calls.push(Call::ok(Request::EthChainId, Response::U64(CHAIN)));
    calls.push(Call::ok(
        Request::EthCall {
            to: PAYNOTE_ADDRESS,
            data: IPayNote::maxMergeInputsCall {}.abi_encode(),
        },
        Response::Bytes(abi_u64(8)),
    ));
    let rpc = RecordingRpc::new(calls);
    assert!(run(
        &rpc,
        &TxSigner::new(KEY).unwrap(),
        dir.path(),
        &[],
        Some(&path)
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("retained"));
    assert!(load_operation(&path).is_ok());
    rpc.assert_done();
}

#[tokio::test]
async fn proof_artifact_has_no_bearer_secrets_amounts_or_source_commitments() {
    let dir = private_tempdir();
    let op = Operation::new(notes(3)).unwrap();
    op.save(dir.path()).unwrap();
    let stage = &op.stages[0];
    let mut tree = new_tree(CHAIN).unwrap();
    let mut logs = Vec::new();
    for (i, n) in stage.inputs.iter().enumerate() {
        tree.append(n.commitment.to_field().unwrap()).unwrap();
        logs.push(event(n, i.try_into().unwrap(), tree.root(), n.amount));
    }
    // Use the normal tree mock for proving, with a recorded prefix for the
    // precise snapshot checks exercised separately above.
    let (proof, public) = prove_stage(stage, &tree).unwrap();
    assert!(verify_circuit::<PaynoteMerge>(&proof).unwrap());
    assert_eq!(public.output_commitment, stage.output.commitment);
    let artifact = proof_artifact(&proof, &public);
    assert_eq!(artifact.as_object().unwrap().len(), 10);
    for private in [
        "amount",
        "note_amounts",
        "spend_key",
        "output_spend_key",
        "inputs",
        "source_commitment",
        "operation_file",
        "output_note",
    ] {
        assert!(
            artifact.get(private).is_none(),
            "private field {private} leaked into relay artifact"
        );
    }
    let mut provider = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(CHAIN);
    outbe_paynote::test_support::seed_pool(&mut provider, CHAIN, tree.leaves());
    provider.enter(|storage| {
        outbe_paynote::precompile::dispatch(
            storage,
            &IPayNote::mergePayNotesCall {
                proof: proof.into(),
            }
            .abi_encode(),
            Address::repeat_byte(0x77),
            U256::ZERO,
        )
        .unwrap()
    });
    // Ordinary spend tooling recognizes the saved output without a new note type.
    tree.append(stage.output.commitment.to_field().unwrap())
        .unwrap();
    logs.push(event(&stage.output, 3, tree.root(), U256::ZERO));
    let rpc = tree_rpc(&tree, logs);
    assert_eq!(read_tree(&rpc, CHAIN).await.unwrap().root(), tree.root());
    let (spend, change, _) = super::super::prove(
        &stage.output,
        U256::from(5),
        B256::from(U256::from(1u64)),
        &tree,
    )
    .unwrap();
    provider.enter(|storage| outbe_paynote::api::consume(&storage, &spend).unwrap());
    assert_eq!(change.unwrap().amount, U256::ONE);
}

#[test]
fn merge_commands_parse_without_exposing_an_owner_argument() {
    use clap::Parser;
    for args in [
        vec!["outbe-cli", "paynote", "merge-proof", "a", "b"],
        vec!["outbe-cli", "paynote", "merge", "a", "b", "c", "d", "e"],
        vec!["outbe-cli", "paynote", "merge", "--resume", "op.json"],
        vec!["outbe-cli", "paynote", "status"],
    ] {
        assert!(crate::Cli::try_parse_from(args).is_ok());
    }
    for args in [
        vec!["outbe-cli", "paynote", "merge", "a"],
        vec![
            "outbe-cli",
            "paynote",
            "merge-proof",
            "a",
            "b",
            "c",
            "d",
            "e",
        ],
        vec![
            "outbe-cli",
            "paynote",
            "merge",
            "a",
            "b",
            "--resume",
            "op.json",
        ],
    ] {
        assert!(crate::Cli::try_parse_from(args).is_err());
    }
}

#[tokio::test]
async fn failed_broadcast_after_real_proving_preserves_operation_and_private_output() {
    let dir = private_tempdir();
    let op = Operation::new(notes(3)).unwrap();
    let path = op.save(dir.path()).unwrap();
    let stage = &op.stages[0];
    let mut tree = new_tree(CHAIN).unwrap();
    let mut logs = Vec::new();
    for (i, n) in stage.inputs.iter().enumerate() {
        tree.append(n.commitment.to_field().unwrap()).unwrap();
        logs.push(event(n, i.try_into().unwrap(), tree.root(), n.amount));
    }
    let mut rpc = tree_rpc(&tree, logs);
    rpc.block_by_number = Ok(json!({"hash":B256::repeat_byte(1)}));
    rpc.eth_call_overrides.insert(
        (
            PAYNOTE_ADDRESS,
            IPayNote::maxMergeInputsCall {}.abi_encode(),
        ),
        abi_u64(4),
    );
    for n in stage.inputs.iter().chain(std::iter::once(&stage.output)) {
        rpc.eth_call_overrides.insert(
            (
                PAYNOTE_ADDRESS,
                IPayNote::hasCommitmentCall {
                    commitment: n.commitment,
                }
                .abi_encode(),
            ),
            abi_u64(u64::from(n.commitment != stage.output.commitment)),
        );
    }
    rpc.tx_count = Ok(0);
    rpc.estimate_gas = Ok(2_000_000);
    rpc.latest_block = Ok(json!({"baseFeePerGas":"0x1"}));
    rpc.send_raw_tx = Err(eyre::eyre!("broadcast response lost"));
    let error = run(
        &rpc,
        &TxSigner::new(KEY).unwrap(),
        dir.path(),
        &[],
        Some(&path),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("broadcast response lost"));
    assert!(load_operation(&path).is_ok());
    assert!(
        load_note(&resolve_note(
            dir.path(),
            &format!("{:#x}", stage.output.commitment)
        ))
        .unwrap()
            == stage.output
    );
    let proofs = json_files(&dir.path().join("proofs")).unwrap();
    assert_eq!(proofs.len(), 1);
    let artifact: Value = serde_json::from_slice(&fs::read(&proofs[0]).unwrap()).unwrap();
    assert!(artifact.get("amount").is_none());
    assert!(artifact.get("inputs").is_none());
    let proof = hex::decode(artifact["proof"].as_str().unwrap().trim_start_matches("0x")).unwrap();
    assert!(verify_circuit::<PaynoteMerge>(&proof).unwrap());
    let report = status(&rpc, dir.path(), &[]).await.unwrap();
    assert_eq!(
        report["notes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["state"] == "pending_merge")
            .count(),
        3
    );
}
