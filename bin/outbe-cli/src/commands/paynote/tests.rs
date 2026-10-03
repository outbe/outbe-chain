use super::*;
use crate::rpc::mock::{
    abi_u256, abi_u64, call_map, ExpectedRpcCall, MockRpc, RecordedRpcCall as Request,
    RecordedRpcResponse as Response, RecordingRpc,
};
use outbe_paynote::{schema::PayNoteContract, test_support::seed_pool};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_protocol::codec::field_to_b256;
use std::collections::HashMap;

pub(super) const KEY: &str = "0000000000000000000000000000000000000000000000000000000000000001";
pub(super) const CHAIN: u64 = 1337;
pub(super) const ASSET: Address = Address::new([0x33; 20]);

pub(super) fn private_tempdir() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Existing note directories must be private regardless of the caller's umask.
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

fn note() -> Note {
    Note::new(
        CHAIN,
        ASSET,
        (U256::ONE << 200) + U256::from(100),
        Field::from(17),
    )
    .unwrap()
}

pub(super) fn event(note: &Note, index: u32, root: Field, amount: U256) -> Value {
    let log = IPayNote::NewNote {
        commitment: note.commitment,
        leafIndex: index,
        rootAfter: field_to_b256(&root).unwrap(),
        asset: note.asset,
        noteAmount: amount,
    }
    .encode_log_data();
    json!({ "address": PAYNOTE_ADDRESS, "topics": log.topics(), "data": log.data, "removed": false })
}

fn read_call(request: impl SolCall, result: Vec<u8>) -> ExpectedRpcCall {
    ExpectedRpcCall::ok(
        Request::EthCall {
            to: ASSET,
            data: request.abi_encode(),
        },
        Response::Bytes(result),
    )
}

fn allowance_call(amount: U256) -> ExpectedRpcCall {
    read_call(
        IERC20::allowanceCall {
            owner: TxSigner::new(KEY).unwrap().address(),
            spender: PAYNOTE_ADDRESS,
        },
        abi_u256(amount),
    )
}

fn tx_calls(to: Address, data: Vec<u8>, receipt: Value) -> Vec<ExpectedRpcCall> {
    let signer = TxSigner::new(KEY).unwrap();
    let base_fee = U256::from(alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE);
    let raw_tx = signer
        .sign_legacy_tx_for_test(
            0,
            crate::tx::buffered_gas_price(base_fee),
            25_200,
            to,
            U256::ZERO,
            &data,
            CHAIN,
        )
        .unwrap();
    vec![
        ExpectedRpcCall::ok(Request::EthChainId, Response::U64(CHAIN)),
        ExpectedRpcCall::ok(
            Request::EthGetTransactionCount {
                address: signer.address(),
            },
            Response::U64(0),
        ),
        ExpectedRpcCall::ok(
            Request::EthGetLatestBlock,
            Response::Value(json!({ "baseFeePerGas": format!("{base_fee:#x}") })),
        ),
        ExpectedRpcCall::ok(
            Request::EthEstimateGas {
                from: signer.address(),
                to,
                data,
                value: U256::ZERO,
            },
            Response::U64(21_000),
        ),
        ExpectedRpcCall::ok(
            Request::EthSendRawTransaction { raw_tx },
            Response::Text("0x1234".into()),
        ),
        ExpectedRpcCall::ok(
            Request::EthGetTransactionReceipt {
                transaction_hash: "0x1234".into(),
            },
            Response::OptionalValue(Some(receipt)),
        ),
    ]
}

fn receipt(logs: Vec<Value>) -> Value {
    json!({ "transactionHash": "0x1234", "status": "0x1", "logs": logs })
}
fn deposit_data(note: &Note) -> Vec<u8> {
    IPayNote::depositCall {
        asset: note.asset,
        amount: note.amount,
        noteSn: field_to_b256(&note_sn(note.key().unwrap()).unwrap()).unwrap(),
    }
    .abi_encode()
}

#[test]
fn command_inputs_and_note_validation() {
    use clap::Parser;
    let asset = format!("{ASSET:#x}");
    assert!(crate::Cli::try_parse_from([
        "outbe-cli",
        "paynote",
        "deposit",
        &asset,
        &U256::MAX.to_string()
    ])
    .is_ok());
    assert!(crate::Cli::try_parse_from([
        "outbe-cli",
        "paynote",
        "spend-proof",
        "note.json",
        "1",
        "--nod",
        "7"
    ])
    .is_ok());
    assert!(crate::Cli::try_parse_from([
        "outbe-cli",
        "paynote",
        "spend-proof",
        "note.json",
        "1",
        "--nod",
        "7",
        "--gem",
        "8"
    ])
    .is_err());
    for invalid in [
        "0",
        "-1",
        "1.5",
        "0x10",
        "",
        " 1",
        "+1",
        &format!("{}0", U256::MAX),
    ] {
        assert!(parse_amount(invalid).is_err(), "{invalid}");
    }
    let mut n = note();
    assert!(n.change(U256::ZERO).is_err());
    assert!(n.change(n.amount + U256::ONE).is_err());
    assert!(n.change(n.amount).unwrap().is_none());
    n.amount += U256::ONE;
    assert!(n.validate().is_err());
    n = note();
    n.spend_key = B256::repeat_byte(0xff);
    assert!(n.validate().is_err());
    n = note();
    n.version = 2;
    assert!(n.validate().is_err());
    assert!(Note::new(CHAIN, Address::ZERO, U256::ONE, Field::from(1)).is_err());
}

#[test]
fn note_files_are_private_immutable_and_roundtrip_full_u256() {
    let temp = private_tempdir();
    let dir = temp.path().join("paynotes");
    let n = note();
    let path = save_note(&dir, &n).unwrap();
    assert!(load_note(&path).unwrap() == n);
    assert_eq!(save_note(&dir, &n).unwrap(), path);
    assert_eq!(resolve_note(&dir, &format!("{:#x}", n.commitment)), path);
    let original = fs::read(&path).unwrap();
    assert!(save_json(
        &dir,
        path.file_name().unwrap().to_str().unwrap(),
        &json!({ "different": true })
    )
    .is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let link = temp.path().join("linked");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(save_note(&link, &n).is_err());
    }
    let first = Note::random(CHAIN, ASSET, U256::ONE).unwrap();
    let second = Note::random(CHAIN, ASSET, U256::ONE).unwrap();
    assert_ne!(first.commitment, second.commitment);
    let maximum = Note::new(CHAIN, ASSET, U256::MAX, Field::from(19)).unwrap();
    assert_eq!(
        load_note(&save_note(&dir, &maximum).unwrap())
            .unwrap()
            .amount,
        U256::MAX
    );
}

#[tokio::test]
async fn deposit_approves_only_when_needed_and_preserves_note_on_failure() {
    let n = note();
    let mut tree = new_tree(CHAIN).unwrap();
    tree.append(n.commitment.to_field().unwrap()).unwrap();
    for initial in [U256::ZERO, U256::ONE, n.amount] {
        let mut calls = vec![allowance_call(initial)];
        if initial < n.amount {
            if !initial.is_zero() {
                calls.extend(tx_calls(
                    ASSET,
                    IERC20::approveCall {
                        spender: PAYNOTE_ADDRESS,
                        amount: U256::ZERO,
                    }
                    .abi_encode(),
                    receipt(vec![]),
                ));
                calls.push(allowance_call(U256::ZERO));
            }
            calls.extend(tx_calls(
                ASSET,
                IERC20::approveCall {
                    spender: PAYNOTE_ADDRESS,
                    amount: n.amount,
                }
                .abi_encode(),
                receipt(vec![]),
            ));
            calls.push(allowance_call(n.amount));
        }
        calls.extend(tx_calls(
            PAYNOTE_ADDRESS,
            deposit_data(&n),
            receipt(vec![event(&n, 0, tree.root(), n.amount)]),
        ));
        let rpc = RecordingRpc::new(calls);
        let temp = private_tempdir();
        let output = deposit(&rpc, &TxSigner::new(KEY).unwrap(), temp.path(), &n)
            .await
            .unwrap();
        assert!(load_note(Path::new(output["note"].as_str().unwrap())).unwrap() == n);
        rpc.assert_done();
    }
    // ERC20 returns a successful receipt but never changes its allowance.
    let mut calls = vec![allowance_call(U256::ZERO)];
    calls.extend(tx_calls(
        ASSET,
        IERC20::approveCall {
            spender: PAYNOTE_ADDRESS,
            amount: n.amount,
        }
        .abi_encode(),
        receipt(vec![]),
    ));
    calls.push(allowance_call(U256::ZERO));
    let rpc = RecordingRpc::new(calls);
    let temp = private_tempdir();
    assert!(deposit(&rpc, &TxSigner::new(KEY).unwrap(), temp.path(), &n)
        .await
        .is_err());
    rpc.assert_done(); // No deposit transaction was sent.
    assert!(load_note(&resolve_note(temp.path(), &format!("{:#x}", n.commitment))).unwrap() == n);
}

#[tokio::test]
async fn deposit_revert_or_lost_response_keeps_the_secret() {
    let n = note();
    for failed_receipt in [
        Err(eyre::eyre!("connection lost")),
        Ok(Some(
            json!({ "transactionHash": "0x1234", "status": "0x0" }),
        )),
    ] {
        let rpc = MockRpc {
            chain_id: Ok(CHAIN),
            tx_count: Ok(0),
            estimate_gas: Ok(1_000_000),
            latest_block: Ok(json!({})),
            send_raw_tx: Ok("0x1234".into()),
            transaction_receipt: failed_receipt,
            eth_call_map: Some(call_map(HashMap::from([(
                (ASSET, IERC20::allowanceCall::SELECTOR),
                abi_u256(n.amount),
            )]))),
            ..Default::default()
        };
        let temp = private_tempdir();
        assert!(deposit(&rpc, &TxSigner::new(KEY).unwrap(), temp.path(), &n)
            .await
            .is_err());
        assert!(
            load_note(&resolve_note(temp.path(), &format!("{:#x}", n.commitment))).unwrap() == n
        );
    }
    let rpc = MockRpc {
        transaction_receipt: Ok(None),
        ..Default::default()
    };
    assert!(wait_receipt(&rpc, "0x1234", Duration::from_millis(1))
        .await
        .unwrap_err()
        .to_string()
        .contains("pending"));
}

fn nod_target() -> SettlementTarget {
    SettlementTarget::Nod(U256::from(7u64))
}

fn quote_return(payable: U256) -> Vec<u8> {
    INodFactory::quoteSettlementCall::abi_encode_returns(&INodFactory::quoteSettlementReturn {
        settlementCurrency: 840,
        paymentMinor: payable,
        snapshotId: U256::ZERO,
    })
}

pub(super) fn tree_rpc(tree: &PayNoteTree, logs: Vec<Value>) -> MockRpc {
    tree_rpc_quoted(tree, logs, U256::ZERO)
}

pub(super) fn tree_rpc_quoted(tree: &PayNoteTree, logs: Vec<Value>, payable: U256) -> MockRpc {
    MockRpc {
        chain_id: Ok(CHAIN),
        block_number: Ok(10),
        logs: Ok(logs),
        eth_call_map: Some(call_map(HashMap::from([
            (
                (PAYNOTE_ADDRESS, IPayNote::leafCountCall::SELECTOR),
                abi_u64(tree.leaves().len().try_into().unwrap()),
            ),
            (
                (PAYNOTE_ADDRESS, IPayNote::currentRootCall::SELECTOR),
                field_to_b256(&tree.root()).unwrap().to_vec(),
            ),
            (
                (PAYNOTE_ADDRESS, IPayNote::isSpentCall::SELECTOR),
                abi_u64(0),
            ),
            (
                (PAYNOTE_ADDRESS, IPayNote::isKnownRootCall::SELECTOR),
                abi_u64(1),
            ),
            (
                (
                    NOD_FACTORY_ADDRESS,
                    INodFactory::quoteSettlementCall::SELECTOR,
                ),
                quote_return(payable),
            ),
        ]))),
        ..Default::default()
    }
}

#[tokio::test]
async fn tree_history_requires_dense_indexes_and_matching_roots() {
    let n = note();
    let mut tree = new_tree(CHAIN).unwrap();
    tree.append(n.commitment.to_field().unwrap()).unwrap();
    let valid = event(&n, 0, tree.root(), n.amount);
    assert_eq!(
        read_tree(&tree_rpc(&tree, vec![valid.clone()]), CHAIN)
            .await
            .unwrap()
            .root(),
        tree.root()
    );
    for logs in [
        vec![],
        vec![valid.clone(), valid.clone()],
        vec![event(&n, 1, tree.root(), n.amount)],
        vec![event(&n, 0, Field::from(1), n.amount)],
        vec![json!({})],
    ] {
        assert!(read_tree(&tree_rpc(&tree, logs), CHAIN).await.is_err());
    }
    let mut removed = valid.clone();
    removed["removed"] = json!(true);
    assert!(decode_note(&removed).is_err());
    let mut noncanonical = valid;
    noncanonical["topics"][1] = json!(B256::repeat_byte(0xff));
    assert!(decode_note(&noncanonical).is_err());
}

#[tokio::test]
async fn wrong_chain_overspend_and_spent_notes_fail_before_proving() {
    let temp = private_tempdir();
    let n = note();
    let rpc = MockRpc {
        chain_id: Ok(CHAIN + 1),
        ..Default::default()
    };
    assert!(spend_proof(&rpc, temp.path(), &n, U256::ONE, nod_target())
        .await
        .unwrap_err()
        .to_string()
        .contains("chain ID"));
    let rpc = MockRpc {
        chain_id: Ok(CHAIN),
        ..Default::default()
    };
    assert!(
        spend_proof(&rpc, temp.path(), &n, n.amount + U256::ONE, nod_target())
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds")
    );
    let rpc = MockRpc {
        chain_id: Ok(CHAIN),
        eth_call_map: Some(call_map(HashMap::from([(
            (PAYNOTE_ADDRESS, IPayNote::isSpentCall::SELECTOR),
            abi_u64(1),
        )]))),
        ..Default::default()
    };
    assert!(spend_proof(&rpc, temp.path(), &n, U256::ONE, nod_target())
        .await
        .unwrap_err()
        .to_string()
        .contains("already spent"));
}

#[tokio::test]
async fn expired_proof_does_not_publish_artifacts_or_change_state() {
    let n = note();
    let temp = private_tempdir();
    let path = save_note(temp.path(), &n).unwrap();
    let before = fs::read(&path).unwrap();
    let mut tree = new_tree(CHAIN).unwrap();
    tree.append(n.commitment.to_field().unwrap()).unwrap();
    let mut rpc = tree_rpc(&tree, vec![event(&n, 0, tree.root(), n.amount)]);
    rpc.eth_call_map = Some(call_map(HashMap::from([
        (
            (PAYNOTE_ADDRESS, IPayNote::leafCountCall::SELECTOR),
            abi_u64(1),
        ),
        (
            (PAYNOTE_ADDRESS, IPayNote::currentRootCall::SELECTOR),
            field_to_b256(&tree.root()).unwrap().to_vec(),
        ),
        (
            (PAYNOTE_ADDRESS, IPayNote::isSpentCall::SELECTOR),
            abi_u64(0),
        ),
        (
            (PAYNOTE_ADDRESS, IPayNote::isKnownRootCall::SELECTOR),
            abi_u64(0),
        ),
        (
            (
                NOD_FACTORY_ADDRESS,
                INodFactory::quoteSettlementCall::SELECTOR,
            ),
            quote_return(U256::ONE),
        ),
    ])));
    let error = spend_proof(&rpc, temp.path(), &n, U256::ONE, nod_target())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("root expired"));
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn deposited_note_partial_spend_and_saved_change_consume_real_proofs() {
    let n = note();
    let temp = private_tempdir();
    let mut tree = new_tree(CHAIN).unwrap();
    tree.append(n.commitment.to_field().unwrap()).unwrap();
    let origin_log = event(&n, 0, tree.root(), n.amount);
    let mut calls = vec![allowance_call(n.amount)];
    calls.extend(tx_calls(
        PAYNOTE_ADDRESS,
        deposit_data(&n),
        receipt(vec![origin_log.clone()]),
    ));
    let rpc = RecordingRpc::new(calls);
    let deposited = deposit(&rpc, &TxSigner::new(KEY).unwrap(), temp.path(), &n)
        .await
        .unwrap();
    rpc.assert_done();
    let saved = load_note(Path::new(deposited["note"].as_str().unwrap())).unwrap();
    let amount = (U256::ONE << 199) + U256::from(40);
    let context = outbe_paynote::api::settlement_context(
        outbe_paynote::api::SettlementDomain::Nod,
        B256::from(U256::from(7u64)),
        U256::ONE,
        U256::ZERO,
    )
    .unwrap();
    let output = spend_proof(
        &tree_rpc_quoted(&tree, vec![origin_log.clone()], amount),
        temp.path(),
        &saved,
        amount,
        nod_target(),
    )
    .await
    .unwrap();
    let combined = hex::decode(output["proof"].as_str().unwrap().trim_start_matches("0x")).unwrap();
    let change = load_note(Path::new(output["change_note"].as_str().unwrap())).unwrap();
    assert_eq!(output["context"], json!(context));
    assert_eq!(output["domain"], json!("nod"));
    assert_eq!(output["units"], json!("1"));
    assert_eq!(output["snapshotId"], json!("0"));
    assert_eq!(change.amount, n.amount - amount);
    assert!(witness(&tree, change.commitment.to_field().unwrap()).is_err());
    assert!(load_note(Path::new(deposited["note"].as_str().unwrap())).unwrap() == n);
    let artifact = fs::read_to_string(output["proof_file"].as_str().unwrap()).unwrap();
    assert!(!artifact.contains(&format!("{:#x}", change.spend_key)));
    assert!(!artifact.contains(&format!("{:#x}", n.spend_key)));

    // The command's deposit receipt is mocked; consumption runs the production
    // cross-module API and real frozen verifier against the corresponding pool.
    let mut provider = HashMapStorageProvider::new(CHAIN);
    seed_pool(&mut provider, CHAIN, tree.leaves());
    provider.enter(|storage| {
        let claim = outbe_paynote::api::consume(&storage, &combined).unwrap();
        assert_eq!(claim.spend_amount, amount);
        assert_eq!(claim.context, context);
        assert!(outbe_paynote::api::is_spent(&storage, n.nullifier().unwrap()).unwrap());
    });
    tree.append(change.commitment.to_field().unwrap()).unwrap();
    let change_log = provider
        .get_ordered_events()
        .iter()
        .find(|log| log.data.topics().first() == Some(&IPayNote::NewNote::SIGNATURE_HASH))
        .unwrap();
    let change_log = json!({ "address": change_log.address, "topics": change_log.data.topics(), "data": change_log.data.data });
    assert_eq!(
        decode_note(&change_log).unwrap().rootAfter,
        field_to_b256(&tree.root()).unwrap()
    );
    let output = spend_proof(
        &tree_rpc_quoted(&tree, vec![origin_log, change_log], change.amount),
        temp.path(),
        &change,
        change.amount,
        nod_target(),
    )
    .await
    .unwrap();
    assert!(output["change_note"].is_null());
    assert_eq!(output["change_commitment"], json!(B256::ZERO));
    let combined = hex::decode(output["proof"].as_str().unwrap().trim_start_matches("0x")).unwrap();
    provider.enter(|storage| {
        assert_eq!(
            outbe_paynote::api::consume(&storage, &combined)
                .unwrap()
                .spend_amount,
            change.amount
        );
        let pool: PayNoteContract<'_> = storage.contract();
        assert_eq!(pool.leaf_count.read().unwrap(), 2);
        assert!(outbe_paynote::api::is_spent(&storage, change.nullifier().unwrap()).unwrap());
        assert!(outbe_paynote::api::consume(&storage, &combined).is_err());
    });
}

#[tokio::test]
async fn a_quote_whose_units_differ_from_the_spend_never_proves() {
    let n = note();
    let temp = private_tempdir();
    save_note(temp.path(), &n).unwrap();
    let mut tree = new_tree(CHAIN).unwrap();
    tree.append(n.commitment.to_field().unwrap()).unwrap();
    let error = spend_proof(
        &tree_rpc_quoted(
            &tree,
            vec![event(&n, 0, tree.root(), n.amount)],
            U256::from(2u64),
        ),
        temp.path(),
        &n,
        U256::ONE,
        nod_target(),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("do not match spend amount"),
        "{error}"
    );
    assert!(temp.path().join("proofs").read_dir().is_err());
}

#[test]
fn spend_proof_requires_exactly_one_settlement_target() {
    use clap::Parser;

    #[derive(Parser)]
    struct Probe {
        #[command(subcommand)]
        cmd: PaynoteCmd,
    }

    assert!(Probe::try_parse_from(["outbe-cli", "spend-proof", "note", "1"]).is_err());
    assert!(Probe::try_parse_from([
        "outbe-cli",
        "spend-proof",
        "note",
        "1",
        "--nod",
        "1",
        "--gem",
        "2"
    ])
    .is_err());
    assert!(Probe::try_parse_from([
        "outbe-cli",
        "spend-proof",
        "note",
        "1",
        "--intex",
        "20260212-TRY-U"
    ])
    .is_err());
    assert!(Probe::try_parse_from(["outbe-cli", "spend-proof", "note", "1", "--nod", "7"]).is_ok());
    assert!(Probe::try_parse_from([
        "outbe-cli",
        "spend-proof",
        "note",
        "1",
        "--intex",
        "20260212-TRY-U",
        "--units",
        "2"
    ])
    .is_err());
    assert!(Probe::try_parse_from([
        "outbe-cli",
        "spend-proof",
        "note",
        "1",
        "--intex",
        "20260212-TRY-U",
        "--units",
        "2",
        "--holder",
        "0x1111111111111111111111111111111111111111"
    ])
    .is_ok());
    assert!(Probe::try_parse_from([
        "outbe-cli",
        "spend-proof",
        "note",
        "1",
        "--nod",
        "7",
        "--holder",
        "0x1111111111111111111111111111111111111111"
    ])
    .is_err());
}

/// A series has many holders, so an Intex proof binds the one whose units it pays.
#[tokio::test]
async fn an_intex_quote_binds_the_holder() {
    let series = *b"20260212-TRY-U";
    let holder = Address::repeat_byte(0x11);
    let units = U256::from(2u64);
    let payable = U256::from(5u64);
    let rpc = MockRpc {
        eth_call_map: Some(call_map(HashMap::from([(
            (
                INTEX_FACTORY_ADDRESS,
                IIntexFactory::quoteSettlementCall::SELECTOR,
            ),
            IIntexFactory::quoteSettlementCall::abi_encode_returns(
                &IIntexFactory::quoteSettlementReturn {
                    settlementCurrency: 840,
                    paymentMinor: payable,
                    snapshotId: U256::ZERO,
                },
            ),
        )]))),
        ..Default::default()
    };
    let quoted = quote_settlement(
        &rpc,
        &note(),
        payable,
        SettlementTarget::Intex {
            series,
            holder,
            units,
        },
    )
    .await
    .unwrap();
    assert_eq!(quoted.holder, Some(holder));
    assert_eq!(quoted.series.as_deref(), Some("20260212-TRY-U"));
    assert_eq!(
        quoted.context,
        settlement_context(
            SettlementDomain::Intex,
            outbe_paynote::api::intex_holding_target(&series, holder),
            units,
            U256::ZERO,
        )
        .unwrap()
    );
}

#[test]
fn a_zero_holder_is_refused_before_proving() {
    let target = SpendTarget {
        nod: None,
        gem: None,
        intex: Some("20260212-TRY-U".into()),
    };
    let error = target
        .into_settlement(Some(U256::ONE), Some(Address::ZERO))
        .unwrap_err();
    assert!(error.to_string().contains("zero address"), "{error}");
}
