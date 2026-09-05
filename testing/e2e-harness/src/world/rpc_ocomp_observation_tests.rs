//! Exercise public observations across the HTTP/ABI/canonical-record boundary.
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};

use alloy_sol_types::SolValue as _;
use outbe_ocomp_protocol::intent::{
    ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType, FrozenMetadosisValuesV1,
    JobIntentV1, MetadosisAttemptPreconditionV1, MetadosisExpectedStatus, NodTargetPreconditionV1,
    TributeInputBindingV1,
};
use outbe_ocomp_protocol::state::{LysisTerminalV1, OcompFinalizedJobV1, OcompTerminalOutcome};
use serde_json::{json, Value};

use super::*;

fn pending_record() -> OcompJobRecordV1 {
    let hash = B256::repeat_byte;
    OcompJobRecordV1 {
        intent: JobIntentV1 {
            chain_id: 42,
            genesis_hash: hash(40),
            fork_id: hash(1),
            wwd: 7,
            pending_nonce: 0,
            attempt: 0,
            protocol_bundle_hash: hash(41),
            ce_sealed_root: hash(42),
            sealed_tribute_collection_key: hash(30),
            sealed_tribute_collection_root: hash(31),
            authenticated_day_count: 1,
            authenticated_day_nominal: U256::ZERO,
            pre_admission_envelope_hash: hash(43),
            source_availability_policy_id: hash(44),
            frozen_metadosis_values: FrozenMetadosisValuesV1 {
                day_type: DayType::Green,
                day_limit: U256::ZERO,
                previous_vwap: U256::ZERO,
                current_vwap: U256::ZERO,
                gratis_demand: U256::ZERO,
                gratis_supply: U256::ZERO,
                lysis_budget: U256::ZERO,
                auction_base: U256::ZERO,
                auction_entry_prices: vec![],
                request_budget_split_receipt_hash: hash(113),
            },
            logical_evaluation_height: 100,
            logical_evaluation_time: 1_000,
            activation_preconditions: ActivationPreconditionsV1 {
                tribute: TributeInputBindingV1 {
                    wwd: 7,
                    source_generation: 3,
                    collection_key: hash(30),
                    sealed_collection_root: hash(31),
                    exact_count: 1,
                    exact_nominal_total: U256::ZERO,
                },
                nod: NodTargetPreconditionV1 {
                    wwd: 7,
                    target_generation: 5,
                    namespace_root_before: hash(32),
                    max_nod_count: 1,
                },
                contributors: ContributorTargetPreconditionV1 {
                    worldwide_day: 7,
                    expected_series_version: 8,
                    max_contributor_count: 1,
                    max_eligible_nominal_total: U256::ZERO,
                },
                metadosis: MetadosisAttemptPreconditionV1 {
                    wwd: 7,
                    pending_nonce: 0,
                    expected_status: MetadosisExpectedStatus::OffchainPending,
                    state_version: 12,
                },
            },
            result_validator_set_epoch: 1,
            result_committee_set_hash: hash(45),
            result_ocomp_binding_hash: hash(46),
            result_member_count: 4,
            result_quorum_threshold: 3,
            custody_committee_epoch_hash: None,
        },
        intent_height: 100,
        status: OcompJobStatus::AwaitingFinality,
        finalized: None,
        terminal: None,
    }
}

#[derive(Clone)]
struct Replies {
    head: Value,
    logs: Value,
    block: Value,
    call: Value,
    pool: Value,
    error_method: Option<&'static str>,
    transient_finalized_errors: usize,
}

impl Replies {
    fn new(record: &OcompJobRecordV1) -> Self {
        let limits = poc_schema_limits();
        let intent_id = record.intent.intent_id(&limits).unwrap();
        let preconditions = record
            .intent
            .activation_preconditions
            .activation_preconditions_hash(&limits)
            .unwrap();
        let mut data = vec![0; 64];
        data.extend_from_slice(preconditions.as_slice());
        let mut block =
            serde_json::to_value(alloy_rpc_types::Block::<alloy_rpc_types::Transaction>::default())
                .unwrap();
        block["number"] = json!("0x64");
        block["hash"] = json!(B256::repeat_byte(70));
        block["stateRoot"] = json!(B256::repeat_byte(71));
        Self {
            head: json!({"number": "0x68"}),
            logs: json!([{
                "address": addresses::WWD_ADDR,
                "topics": [
                    keccak256(b"OffchainJobRequested(bytes32,uint32,uint64,uint32,bytes32)"),
                    intent_id, format!("0x{:064x}", record.intent.wwd)
                ],
                "data": format!("0x{}", hex::encode(data)),
                "blockNumber": "0x64", "blockHash": B256::repeat_byte(70),
                "transactionHash": B256::repeat_byte(72)
            }]),
            block,
            call: json!(format!(
                "0x{}",
                hex::encode(Bytes::from(record.encode_canonical(&limits).unwrap()).abi_encode())
            )),
            error_method: None,
            transient_finalized_errors: 0,
            pool: json!({"pending": {}, "queued": {}}),
        }
    }
}

/// Only the external JSON-RPC boundary is scripted; the actual observer and
/// production codecs run unchanged. Shutdown is bounded even on an early error.
struct RpcServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RpcServer {
    fn start(replies: Replies) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = thread::spawn(move || {
            let mut transient_finalized_errors = replies.transient_finalized_errors;
            while !stopped.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("accept RPC: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut length = None;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = Some(value.trim().parse::<usize>().unwrap());
                        }
                    }
                }
                let mut body = vec![0; length.expect("RPC content length")];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                let method = request["method"].as_str().unwrap();
                let result = match method {
                    "eth_getBlockByNumber" if request["params"][0] == "finalized" => &replies.head,
                    "eth_getBlockByNumber" => &replies.block,
                    "eth_getLogs" => &replies.logs,
                    "txpool_content" => &replies.pool,
                    "eth_call" => {
                        assert_eq!(
                            request["params"][1], "0x68",
                            "record must be read at sampled finalized height"
                        );
                        &replies.call
                    }
                    _ => panic!("unexpected RPC method {method}"),
                };
                let mut response = json!({"jsonrpc": "2.0", "id": request["id"]});
                let transient_error = method == "eth_getBlockByNumber"
                    && request["params"][0] == "finalized"
                    && transient_finalized_errors > 0;
                if transient_error {
                    transient_finalized_errors -= 1;
                }
                if replies.error_method == Some(method) || transient_error {
                    response["error"] = json!({"code": -32603, "message": "injected RPC failure"});
                } else {
                    response["result"] = result.clone();
                }
                let body = serde_json::to_vec(&response).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }

    fn rpc(&self) -> Rpc {
        let mut cfg = Config::resolve(&crate::env::Environment::default());
        cfg.rpc0 = format!("http://127.0.0.1:{}", self.port);
        Rpc { cfg }
    }
}

impl Drop for RpcServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let result = self.thread.take().unwrap().join();
        if !thread::panicking() {
            result.expect("RPC server did not panic");
        }
    }
}

#[test]
fn pending_request_is_observable_before_its_canonical_job_binding() {
    let server = RpcServer::start(Replies::new(&pending_record()));
    let observation = server
        .rpc()
        .finalized_ocomp_job_request_for_worldwide_day_result_on(server.port, 0, 7);
    assert!(
        observation.is_ok(),
        "a canonical pending request is not a transport/protocol failure: {observation:?}"
    );
    let observation = observation.unwrap();
    assert!(
        !observation.is_absent(),
        "neither no-successor nor no-preseed may accept pending"
    );
    assert!(observation.into_bound_request().unwrap().is_none());
    assert!(server
        .rpc()
        .finalized_ocomp_job_request_on(server.port, 0)
        .unwrap()
        .is_none());
    assert!(server
        .rpc()
        .finalized_ocomp_job_request_for_worldwide_day_on(server.port, 0, 7)
        .unwrap()
        .is_none());
    assert!(server
        .rpc()
        .finalized_ocomp_job_request(0)
        .unwrap()
        .is_none());
}

fn bound_record() -> OcompJobRecordV1 {
    let mut record = pending_record();
    let request_hash = B256::repeat_byte(70);
    let request_root = B256::repeat_byte(71);
    record.finalized = Some(OcompFinalizedJobV1 {
        job_id: record
            .intent
            .job_id(request_hash, request_root, &poc_schema_limits())
            .unwrap(),
        finalized_request_block_hash: request_hash,
        finalized_request_state_root: request_root,
        finality_recorded_height: 104,
        open_height: 108,
        deadline_height: 211,
        quorum: None,
    });
    record
}

fn observe(replies: Replies) -> Result<OcompRequestObservation> {
    let server = RpcServer::start(replies);
    server
        .rpc()
        .finalized_ocomp_job_request_for_worldwide_day_result_on(server.port, 0, 7)
}

#[test]
fn absence_requires_a_successful_read_without_a_matching_request() {
    let mut replies = Replies::new(&pending_record());
    replies.logs = json!([]);
    let absent = observe(replies).unwrap();
    assert!(absent.is_absent());
    assert!(absent.into_bound_request().unwrap().is_none());

    let server = RpcServer::start(Replies::new(&pending_record()));
    assert!(server
        .rpc()
        .finalized_ocomp_job_request_for_worldwide_day_result_on(server.port, 0, 8)
        .unwrap()
        .is_absent());
    assert!(server
        .rpc()
        .finalized_ocomp_job_request_for_worldwide_day_result_on(server.port, 105, 7)
        .unwrap()
        .is_absent());
}

#[test]
fn staggered_validators_wait_for_the_exact_canonical_binding() {
    let pending = observe(Replies::new(&pending_record())).unwrap();
    let record = bound_record();
    let binding = record.finalized.as_ref().unwrap();
    let bound = observe(Replies::new(&record)).unwrap();
    assert!(!bound.is_absent());
    let request = bound.clone().into_bound_request().unwrap().unwrap();
    assert_eq!(request.job_id, binding.job_id);
    assert_eq!(
        request.intent_id,
        record.intent.intent_id(&poc_schema_limits()).unwrap()
    );
    assert_eq!(request.finality_recorded_height, 104);
    assert_eq!(request.open_height, 108);
    assert_eq!(request.deadline_height, 211);
    assert_eq!(request.request_height, 100);
    assert_eq!(request.request_block_hash, B256::repeat_byte(70));
    assert_eq!(request.transaction_hash, B256::repeat_byte(72));
    // The same positive-poll conversion used by the scenario permits staggered
    // readiness, but only a full set of exact bindings satisfies all(Some).
    for ready in 0..=4 {
        let observations = (0..4)
            .map(|index| {
                if index < ready {
                    bound.clone()
                } else {
                    pending.clone()
                }
            })
            .map(|observation| observation.into_bound_request().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(observations.iter().all(Option::is_some), ready == 4);
        assert!(observations
            .iter()
            .flatten()
            .all(|observed| observed == &request));
    }
}

#[test]
fn terminal_without_binding_is_present_but_never_pending() {
    for (status, outcome) in [
        (OcompJobStatus::Expired, OcompTerminalOutcome::Expired),
        (OcompJobStatus::Failed, OcompTerminalOutcome::Failed),
    ] {
        let mut record = pending_record();
        record.status = status;
        record.terminal = Some(LysisTerminalV1 {
            outcome,
            terminal_height: 104,
            terminal_time: 1_100,
            completed_binding: None,
        });
        let observation = observe(Replies::new(&record)).unwrap();
        assert!(!observation.is_absent());
        let error = observation.into_bound_request().unwrap_err();
        assert!(error.to_string().contains("terminated as"), "{error:#}");
    }
}

#[test]
fn rpc_errors_are_not_absence_or_pending_on_any_observer_interface() {
    for method in ["eth_getBlockByNumber", "eth_getLogs", "eth_call"] {
        let mut replies = Replies::new(&pending_record());
        replies.error_method = Some(method);
        let server = RpcServer::start(replies);
        let rpc = server.rpc();
        assert!(rpc
            .finalized_ocomp_job_request_for_worldwide_day_result_on(server.port, 0, 7)
            .is_err());
        assert!(rpc.finalized_ocomp_job_request_on(server.port, 0).is_err());
        assert!(rpc
            .finalized_ocomp_job_request_for_worldwide_day_on(server.port, 0, 7)
            .is_err());
        assert!(rpc.finalized_ocomp_job_request(0).is_err());
    }
}

#[test]
fn missing_rpc_cannot_prove_absence() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let rpc = Rpc {
        cfg: Config::resolve(&crate::env::Environment::default()),
    };
    assert!(rpc
        .finalized_ocomp_job_request_for_worldwide_day_result_on(port, 0, 7)
        .is_err());
}

#[test]
fn malformed_rpc_abi_and_canonical_records_remain_errors() {
    for mutation in 0..7 {
        let mut replies = Replies::new(&pending_record());
        match mutation {
            0 => replies.head = json!({}),
            1 => replies.logs = json!({}),
            2 => replies.block = Value::Null,
            3 => replies.call = json!("0x00"),
            4 => {
                replies.call = json!(format!(
                    "0x{}",
                    hex::encode(Bytes::from(vec![0; 10]).abi_encode())
                ))
            }
            5 => replies.logs[0]["transactionHash"] = json!("bad hash"),
            6 => replies.logs[0]["data"] = json!("0x00"),
            _ => unreachable!(),
        }
        assert!(
            observe(replies).is_err(),
            "malformed case {mutation} was swallowed"
        );
    }
}

#[test]
fn pending_and_bound_requests_validate_all_event_and_header_bindings() {
    for record in [pending_record(), bound_record()] {
        for mutation in 0..5 {
            let mut replies = Replies::new(&record);
            match mutation {
                0 => replies.logs[0]["topics"][1] = json!(B256::repeat_byte(90)),
                1 => replies.logs[0]["blockHash"] = json!(B256::repeat_byte(90)),
                2 => replies.logs[0]["topics"][0] = json!(B256::repeat_byte(90)),
                3 => replies.logs[0]["blockNumber"] = json!("0x69"),
                4 => {
                    let mut changed = record.clone();
                    changed.intent_height += 1;
                    replies.call = Replies::new(&changed).call;
                }
                _ => unreachable!(),
            }
            assert!(
                observe(replies).is_err(),
                "binding mismatch {mutation} was swallowed"
            );
        }
    }
    for field in ["hash", "stateRoot"] {
        let mut replies = Replies::new(&bound_record());
        replies.block[field] = json!(B256::repeat_byte(99));
        if field == "hash" {
            replies.logs[0]["blockHash"] = replies.block[field].clone();
        }
        let error = observe(replies).unwrap_err();
        assert!(
            format!("{error:#}").contains("finalized job request commitment mismatch"),
            "{error:#}"
        );
    }
}

#[test]
fn pr4_malformed_pool_cannot_certify_transaction_absence() {
    let hash = B256::repeat_byte(12).to_string();
    let sender = Address::repeat_byte(11).to_string();
    for pool in [
        Value::Null,
        json!({"pending": {}, "queued": null}),
        json!({"pending": {}, "queued": []}),
        json!({"pending": {}}),
        json!({"pending": {&sender: null}, "queued": {}}),
        json!({"pending": {&sender: {"0": null}}, "queued": {}}),
    ] {
        let mut replies = Replies::new(&pending_record());
        replies.pool = pool.clone();
        let server = RpcServer::start(replies);
        assert!(
            server.rpc().txpool_has(server.port, &hash).is_err(),
            "{pool}"
        );
        assert!(
            server.rpc().txpool_location(server.port, &hash).is_err(),
            "{pool}"
        );
    }
}

fn pool_transaction(hash: B256, sender: Address) -> Value {
    json!({"hash": hash, "from": sender, "nonce": "0x0", "input": "0x"})
}

#[test]
fn pr4_pool_lookup_uses_exact_hash_and_validates_both_complete_sections() {
    let hash = B256::repeat_byte(12);
    let sender = Address::repeat_byte(11);
    let mut replies = Replies::new(&pending_record());
    let empty = RpcServer::start(replies.clone());
    assert!(!empty
        .rpc()
        .txpool_has(empty.port, &hash.to_string())
        .unwrap());
    for kind in ["pending", "queued"] {
        replies.pool = json!({"pending": {}, "queued": {}});
        replies.pool[kind] = json!({sender.to_string(): {"0": pool_transaction(hash, sender)}});
        let server = RpcServer::start(replies.clone());
        assert_eq!(
            server
                .rpc()
                .txpool_location(server.port, &hash.to_string())
                .unwrap(),
            Some(kind)
        );
        assert!(server
            .rpc()
            .txpool_has(server.port, &hash.to_string())
            .unwrap());
    }

    let mut transaction = pool_transaction(B256::repeat_byte(13), sender);
    transaction["input"] = json!(hash);
    replies.pool = json!({"pending": {sender.to_string(): {"0": transaction}}, "queued": {}});
    let server = RpcServer::start(replies.clone());
    assert!(!server
        .rpc()
        .txpool_has(server.port, &hash.to_string())
        .unwrap());

    let valid = pool_transaction(hash, sender);
    for invalid in [
        Value::Null,
        json!({sender.to_string(): {"0": valid.clone()}}),
    ] {
        replies.pool =
            json!({"pending": {sender.to_string(): {"0": valid.clone()}}, "queued": invalid});
        let server = RpcServer::start(replies.clone());
        assert!(server
            .rpc()
            .txpool_location(server.port, &hash.to_string())
            .is_err());
    }
}

#[test]
fn pr4_pool_lookup_rejects_inconsistent_entries_even_for_an_absent_hash() {
    let sender = Address::repeat_byte(11);
    let hash = B256::repeat_byte(12);
    for mutation in 0..5 {
        let mut transaction = pool_transaction(hash, sender);
        match mutation {
            0 => transaction["from"] = json!(Address::repeat_byte(10)),
            1 => transaction["nonce"] = json!("0x1"),
            2 => transaction["hash"] = json!("0x1234"),
            3 => transaction["nonce"] = json!("invalid"),
            4 => transaction["from"] = Value::Null,
            _ => unreachable!(),
        }
        let mut replies = Replies::new(&pending_record());
        replies.pool = json!({"pending": {sender.to_string(): {"0": transaction}}, "queued": {}});
        let server = RpcServer::start(replies);
        assert!(
            server
                .rpc()
                .txpool_has(server.port, &B256::repeat_byte(99).to_string())
                .is_err(),
            "mutation {mutation}"
        );
    }
}

#[test]
fn pr4_pool_rpc_failure_is_not_transaction_absence() {
    let mut replies = Replies::new(&pending_record());
    replies.error_method = Some("txpool_content");
    let server = RpcServer::start(replies);
    assert!(server
        .rpc()
        .txpool_has(server.port, &B256::repeat_byte(12).to_string())
        .is_err());
}

#[test]
fn pr4_pool_rejects_conflicting_hashes_at_the_same_sender_nonce() {
    let sender = Address::repeat_byte(11);
    let mut replies = Replies::new(&pending_record());
    replies.pool = json!({
        "pending": {sender.to_string(): {"0": pool_transaction(B256::repeat_byte(12), sender)}},
        "queued": {sender.to_string(): {"0": pool_transaction(B256::repeat_byte(13), sender)}}
    });
    let server = RpcServer::start(replies);
    assert!(server
        .rpc()
        .txpool_has(server.port, &B256::repeat_byte(99).to_string())
        .is_err());
}

#[test]
fn pr4_pool_rejects_noncanonical_nonce_encodings() {
    let sender = Address::repeat_byte(11);
    for (key, encoded) in [("0", "0x+0"), ("0", "0x00"), ("+0", "0x0"), ("00", "0x0")] {
        let mut transaction = pool_transaction(B256::repeat_byte(12), sender);
        transaction["nonce"] = json!(encoded);
        let mut replies = Replies::new(&pending_record());
        replies.pool = json!({"pending": {sender.to_string(): {key: transaction}}, "queued": {}});
        let server = RpcServer::start(replies);
        assert!(
            server
                .rpc()
                .txpool_has(server.port, &B256::repeat_byte(99).to_string())
                .is_err(),
            "{key}/{encoded}"
        );
    }
}

#[test]
fn pr4_checkpoint_rejects_a_response_for_another_height() {
    let server = RpcServer::start(Replies::new(&pending_record()));
    assert!(server.rpc().checkpoint_at(server.port, 100).is_ok());
    assert!(server.rpc().checkpoint_at(server.port, 101).is_err());
}

fn receipt_outcome() -> TxOutcome {
    TxOutcome {
        transaction_hash: B256::repeat_byte(72).to_string(),
        success: true,
        receipt: json!({
            "transactionHash": B256::repeat_byte(72), "status": "0x1",
            "blockNumber": "0x64", "blockHash": B256::repeat_byte(70)
        }),
    }
}

#[test]
fn pr4_finalized_receipt_cannot_contradict_its_claimed_status_or_transaction() {
    let mut replies = Replies::new(&pending_record());
    replies.head = replies.block.clone();
    let server = RpcServer::start(replies);
    let rpc = server.rpc();
    let valid = receipt_outcome();
    assert!(rpc.finalize_outcome(&valid, &[server.port], 1).is_ok());
    for mutation in 0..8 {
        let mut outcome = valid.clone();
        match mutation {
            0 => outcome.receipt["status"] = json!("0x0"),
            1 => outcome.receipt["transactionHash"] = json!(B256::repeat_byte(73)),
            2 => outcome.receipt["status"] = Value::Null,
            3 => outcome.receipt["status"] = json!("0x2"),
            4 => outcome.receipt["transactionHash"] = Value::Null,
            5 => outcome.success = false,
            6 => outcome.receipt["blockHash"] = json!(B256::repeat_byte(74)),
            7 => outcome.receipt["blockNumber"] = json!("0x63"),
            _ => unreachable!(),
        }
        assert!(
            rpc.finalize_outcome(&outcome, &[server.port], 1).is_err(),
            "receipt contradiction {mutation} was accepted"
        );
    }
}

#[test]
fn pr4_receipt_requires_matching_checkpoint_on_every_node() {
    let mut replies = Replies::new(&pending_record());
    replies.head = replies.block.clone();
    let first = RpcServer::start(replies.clone());
    let second = RpcServer::start(replies.clone());
    let rpc = first.rpc();
    assert!(rpc
        .finalize_outcome(&receipt_outcome(), &[first.port, second.port], 1)
        .is_ok());
    replies.block["stateRoot"] = json!(B256::repeat_byte(75));
    let divergent = RpcServer::start(replies);
    assert!(rpc
        .finalize_outcome(&receipt_outcome(), &[first.port, divergent.port], 1)
        .is_err());
}

#[test]
fn pr4_fresh_finality_target_includes_the_fastest_peer_and_requires_every_rpc() {
    let mut replies = Replies::new(&pending_record());
    replies.head = replies.block.clone();
    let first = RpcServer::start(replies.clone());
    replies.head["number"] = json!("0x70");
    let faster = RpcServer::start(replies.clone());
    let rpc = first.rpc();
    assert_eq!(
        rpc.fresh_finality_target(&[first.port, faster.port])
            .unwrap(),
        114
    );
    assert_eq!(
        rpc.fresh_finality_target(&[faster.port, first.port])
            .unwrap(),
        114
    );
    replies.error_method = Some("eth_getBlockByNumber");
    let unavailable = RpcServer::start(replies);
    assert!(rpc
        .fresh_finality_target(&[first.port, unavailable.port])
        .is_err());
    assert!(rpc.fresh_finality_target(&[]).is_err());
}

#[test]
fn pr4_recovery_waits_for_rpc_readiness_before_sampling_fresh_finality() {
    let mut replies = Replies::new(&pending_record());
    replies.head = replies.block.clone();
    let ready = RpcServer::start(replies.clone());
    replies.transient_finalized_errors = 1;
    let restarting = RpcServer::start(replies);
    let rpc = ready.rpc();
    let ports = [ready.port, restarting.port];
    let checkpoint = rpc.wait_finalized_checkpoint(&ports, 1, 2).unwrap();
    assert_eq!(checkpoint.height, 100);
    assert_eq!(rpc.fresh_finality_target(&ports).unwrap(), 102);
}
