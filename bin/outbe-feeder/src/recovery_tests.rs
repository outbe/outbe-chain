//! Exercise the production tick, signing and durable recovery against a local
//! JSON-RPC transport. This fixture models RPC outcomes, not chain execution;
//! real quorum/tally behavior is exercised separately on a local network.
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use alloy_primitives::{keccak256, Address, Bytes, U256};
use alloy_sol_types::SolCall;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

use super::*;
use crate::abi::{IOracle, IValidatorSet};

fn block_hash(height: u64) -> String {
    format!("0x{:064x}", u128::from(height) + 65_536)
}
fn words(values: &[U256]) -> Value {
    let bytes: Vec<u8> = values
        .iter()
        .flat_map(|word| word.to_be_bytes::<32>())
        .collect();
    json!(format!("{:#x}", Bytes::from(bytes)))
}
fn ints(values: &[u64]) -> Value {
    words(&values.iter().copied().map(U256::from).collect::<Vec<_>>())
}

struct RpcState {
    height: u64,
    nonce: u64,
    pending_nonce: u64,
    validator: Address,
    resolved_validator: Address,
    oracle_enabled: bool,
    vote_period: u64,
    validator_status: u64,
    exists: bool,
    fail_params_once: bool,
    lose_send_response_once: bool,
    broadcast_error: Option<String>,
    receipts: BTreeMap<String, Value>,
    requests: Vec<Value>,
    broadcasts: Vec<String>,
}

impl RpcState {
    fn reply(&mut self, request: &Value) -> Option<Result<Value, String>> {
        self.requests.push(request.clone());
        let params = &request["params"];
        let result = match request["method"].as_str().unwrap() {
            "eth_getBlockByNumber" => {
                let height = if params[0] == "latest" {
                    self.height
                } else {
                    u64::from_str_radix(params[0].as_str().unwrap().trim_start_matches("0x"), 16)
                        .unwrap()
                };
                json!({"number":format!("{height:#x}"),"hash":block_hash(height)})
            }
            "eth_chainId" => json!("0x1"),
            "eth_getTransactionCount" => json!(format!(
                "{:#x}",
                if params[1] == "pending" {
                    self.pending_nonce
                } else {
                    self.nonce
                }
            )),
            "eth_gasPrice" => json!("0x3b9aca00"),
            "eth_getTransactionReceipt" => self
                .receipts
                .get(params[0].as_str().unwrap())
                .cloned()
                .unwrap_or(Value::Null),
            "eth_sendRawTransaction" => {
                let raw = params[0].as_str().unwrap().to_string();
                let bytes: Bytes = raw.parse().unwrap();
                self.broadcasts.push(raw);
                self.pending_nonce = self.nonce + 1;
                if self.lose_send_response_once {
                    self.lose_send_response_once = false;
                    return None; // Server accepted bytes, then the connection was lost.
                }
                if let Some(error) = &self.broadcast_error {
                    return Some(Err(error.clone()));
                }
                json!(format!("{:#x}", keccak256(bytes)))
            }
            "eth_call" => {
                let bytes: Bytes = serde_json::from_value(params[0]["data"].clone()).unwrap();
                let selector = &bytes[..4];
                if selector == IOracle::getParamsCall::SELECTOR {
                    if self.fail_params_once {
                        self.fail_params_once = false;
                        return Some(Err("temporary preflight outage".into()));
                    }
                    ints(&[
                        self.vote_period,
                        0,
                        1000,
                        0,
                        0,
                        60,
                        u64::from(self.oracle_enabled),
                    ])
                } else if selector == IValidatorSet::validatorByAddressCall::SELECTOR {
                    // Twelve tuple heads; dynamic consensusPubkey is empty.
                    let mut values = vec![U256::ZERO; 13];
                    values[0] = U256::from_be_slice(self.validator.as_slice());
                    values[1] = U256::from(12u64 * 32);
                    values[3] = U256::from(self.validator_status);
                    words(&values)
                } else if selector == IValidatorSet::resolveValidatorCall::SELECTOR {
                    words(&[U256::from_be_slice(self.resolved_validator.as_slice())])
                } else if selector == IOracle::getAggregateVoteCall::SELECTOR {
                    // exists plus four dynamic empty arrays. Presence is the
                    // scheduler input; tuple contents are immaterial here.
                    ints(&[u64::from(self.exists), 160, 192, 224, 256, 0, 0, 0, 0])
                } else if selector == IOracle::getExchangeRateDataCall::SELECTOR {
                    ints(&[1_000_000, self.height.max(1), 1_790_000_000])
                } else {
                    return Some(Err(format!("unexpected eth_call selector {selector:?}")));
                }
            }
            method => return Some(Err(format!("unexpected method {method}"))),
        };
        Some(Ok(result))
    }

    fn include(&mut self, pending: &journal::PendingVote, height: u64, success: bool) {
        self.receipts.insert(
            pending.hash.clone(),
            json!({
                "transactionHash":pending.hash,"blockNumber":format!("{height:#x}"),
                "blockHash":block_hash(height),"status":if success {"0x1"} else {"0x0"},
            }),
        );
        if success {
            self.nonce = pending.nonce + 1;
            self.pending_nonce = self.nonce;
            self.exists = true;
        }
    }
}

struct FakeRpc {
    endpoint: String,
    state: Arc<Mutex<RpcState>>,
    task: JoinHandle<()>,
}
impl Drop for FakeRpc {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl FakeRpc {
    async fn start(height: u64, validator: Address) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(RpcState {
            height,
            nonce: 0,
            pending_nonce: 0,
            validator,
            resolved_validator: validator,
            oracle_enabled: true,
            vote_period: 8,
            validator_status: 2,
            exists: false,
            fail_params_once: false,
            lose_send_response_once: false,
            broadcast_error: None,
            receipts: BTreeMap::new(),
            requests: vec![],
            broadcasts: vec![],
        }));
        let shared = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let (header_end, body_len) = loop {
                    let mut chunk = [0u8; 4096];
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "incomplete HTTP request");
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(pos) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        let header = std::str::from_utf8(&bytes[..pos]).unwrap();
                        let length = header
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .expect("JSON-RPC request must have content-length");
                        break (pos + 4, length);
                    }
                };
                while bytes.len() < header_end + body_len {
                    let mut chunk = [0u8; 4096];
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert!(n > 0, "incomplete JSON body");
                    bytes.extend_from_slice(&chunk[..n]);
                }
                let request: Value =
                    serde_json::from_slice(&bytes[header_end..header_end + body_len]).unwrap();
                let reply = shared.lock().unwrap().reply(&request);
                let Some(reply) = reply else { continue };
                let body = match reply {
                    Ok(result) => json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
                    Err(message) => json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":message}}),
                }.to_string();
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        Self {
            endpoint,
            state,
            task,
        }
    }
}

struct Fixture {
    rpc: FakeRpc,
    config: FeederConfig,
    providers: Vec<Box<dyn provider::Provider>>,
    wallet: alloy_network::EthereumWallet,
    signer: Address,
    validator: Address,
    client: oracle_client::OracleClient,
    journal: Option<journal::Journal>,
    path: PathBuf,
    identity: String,
    health: FeederHealth,
    last_attempt: Option<u64>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.journal.take();
        let _ = std::fs::remove_dir_all(self.path.parent().unwrap());
    }
}
impl Fixture {
    async fn new(height: u64) -> Self {
        let validator = Address::repeat_byte(0x11);
        let rpc = FakeRpc::start(height, validator).await;
        let config: FeederConfig = toml::from_str(&format!(
            r#"
[chain]
rpc_endpoint = "{}"
chain_id = 1
gasless_oracle_votes = true
[account]
private_key = "0x0000000000000000000000000000000000000000000000000000000000000001"
validator_address = "{validator:#x}"
[oracle]
vote_period = 8
poll_interval_secs = 1
[[currency_pairs]]
base = "COEN"
quote = "840"
[[currency_pairs.sources]]
provider = "mock"
base = "COEN"
quote = "840"
"#,
            rpc.endpoint
        ))
        .unwrap();
        config.validate().unwrap();
        let providers = provider::create_providers(&config).unwrap();
        let (wallet, signer) = oracle_client::create_wallet(&config.account).unwrap();
        let client = oracle_client::OracleClient::new(&rpc.endpoint).unwrap();
        let identity = client.identity(1, signer, validator).await.unwrap();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "outbe-feeder-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("pending.json");
        let journal = Some(journal::Journal::open(&path, &identity).unwrap());
        let health = FeederHealth::new(8);
        health.expect_pairs(&["COEN/840".into()]);
        Self {
            rpc,
            config,
            providers,
            wallet,
            signer,
            validator,
            client,
            journal,
            path,
            identity,
            health,
            last_attempt: None,
        }
    }
    async fn tick(&mut self) -> Result<()> {
        feeder_tick(
            &self.config,
            &self.providers,
            &self.client,
            &self.wallet,
            self.signer,
            self.validator,
            self.journal.as_mut().unwrap(),
            &self.health,
            &mut self.last_attempt,
        )
        .await
    }
    fn pending(&self) -> journal::PendingVote {
        self.journal.as_ref().unwrap().pending().unwrap().clone()
    }
    fn restart(&mut self) {
        self.journal.take();
        self.journal = Some(journal::Journal::open(&self.path, &self.identity).unwrap());
        self.last_attempt = None;
        self.health = FeederHealth::new(8);
        self.health.expect_pairs(&["COEN/840".into()]);
    }
}

#[tokio::test]
async fn period_zero_submits_and_all_state_reads_are_pinned_to_committed_hash() {
    let mut f = Fixture::new(1).await;
    f.tick().await.unwrap();
    assert_eq!(f.pending().observed_height, 1);
    assert_eq!(f.health.current_period.load(Ordering::Relaxed), 0);
    let state = f.rpc.state.lock().unwrap();
    assert_eq!(state.broadcasts.len(), 1);
    let calls: Vec<_> = state
        .requests
        .iter()
        .filter(|r| r["method"] == "eth_call")
        .collect();
    assert_eq!(
        calls.len(),
        9,
        "freshness plus two complete four-call preflights"
    );
    for call in calls {
        assert_eq!(
            call["params"][1],
            json!({"blockHash":block_hash(1),"requireCanonical":true})
        );
    }
    let canonical_nonce = state
        .requests
        .iter()
        .find(|r| r["method"] == "eth_getTransactionCount" && r["params"][1] != "pending")
        .unwrap();
    assert_eq!(canonical_nonce["params"][1]["blockHash"], block_hash(1));
}

#[tokio::test]
async fn transient_preflight_error_does_not_consume_current_head_or_period() {
    let mut f = Fixture::new(9).await;
    f.rpc.state.lock().unwrap().fail_params_once = true;
    assert!(f.tick().await.is_err());
    assert_eq!(f.last_attempt, None);
    assert!(f.journal.as_ref().unwrap().pending().is_none());
    f.tick().await.unwrap();
    assert_eq!(f.rpc.state.lock().unwrap().broadcasts.len(), 1);
    assert_eq!(f.pending().observed_height, 9);
}

#[tokio::test]
async fn lost_send_response_restart_rebroadcasts_identical_bytes_hash_and_nonce() {
    let mut f = Fixture::new(9).await;
    f.rpc.state.lock().unwrap().lose_send_response_once = true;
    assert!(f.tick().await.is_err());
    let original = f.pending();
    f.restart();
    assert_eq!(
        f.pending(),
        original,
        "unknown outcome survived process restart"
    );
    f.tick().await.unwrap();
    assert_eq!(f.pending(), original);
    let state = f.rpc.state.lock().unwrap();
    assert_eq!(state.broadcasts, vec![original.raw.clone(), original.raw]);
    assert_eq!(
        state.pending_nonce,
        original.nonce + 1,
        "one occupied nonce only"
    );
}

#[tokio::test]
async fn delayed_receipt_records_actual_inclusion_block_and_period() {
    let mut f = Fixture::new(7).await;
    f.tick().await.unwrap();
    let pending = f.pending();
    {
        let mut state = f.rpc.state.lock().unwrap();
        state.height = 10;
        state.include(&pending, 9, true);
    }
    f.tick().await.unwrap();
    assert!(f.journal.as_ref().unwrap().pending().is_none());
    let included = f.health.last_vote_block.load(Ordering::Relaxed);
    assert_eq!(included, 9);
    assert_eq!(included / 8, 1);
    assert_ne!(included / 8, pending.observed_height / 8);
    assert_eq!(f.health.votes_submitted.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn synthetic_failure_without_nonce_consumption_replaces_at_same_nonce() {
    let mut f = Fixture::new(9).await;
    f.tick().await.unwrap();
    let original = f.pending();
    {
        let mut state = f.rpc.state.lock().unwrap();
        state.height = 10;
        state.include(&original, 10, false);
    }
    f.tick().await.unwrap();
    let replacement = f.pending();
    assert_eq!(replacement.nonce, original.nonce);
    assert_eq!(replacement.created_at, original.created_at);
    assert!(replacement.max_fee_per_gas > original.max_fee_per_gas);
    assert_ne!(replacement.hash, original.hash);
    assert_ne!(replacement.raw, original.raw);
    assert_eq!(f.rpc.state.lock().unwrap().broadcasts.len(), 2);
    f.restart();
    assert_eq!(
        f.pending(),
        replacement,
        "replacement was durably journaled"
    );
}

#[tokio::test]
async fn consumed_nonce_without_receipt_clears_journal_before_resubmitting() {
    let mut f = Fixture::new(9).await;
    f.tick().await.unwrap();
    let original = f.pending();
    {
        let mut state = f.rpc.state.lock().unwrap();
        state.height = 10;
        state.nonce = original.nonce + 1;
        state.pending_nonce = state.nonce;
    }
    f.tick().await.unwrap();
    assert!(f.journal.as_ref().unwrap().pending().is_none());
    assert_eq!(
        f.rpc.state.lock().unwrap().broadcasts.len(),
        1,
        "reconcile before another send"
    );
    f.tick().await.unwrap();
    assert_eq!(f.pending().nonce, original.nonce + 1);
}

#[tokio::test]
async fn old_vote_at_preboundary_head_does_not_consume_following_period() {
    let mut f = Fixture::new(7).await;
    f.rpc.state.lock().unwrap().exists = true;
    f.tick().await.unwrap();
    assert!(f.rpc.state.lock().unwrap().broadcasts.is_empty());
    assert_eq!(f.health.current_period.load(Ordering::Relaxed), 0);
    {
        let mut state = f.rpc.state.lock().unwrap();
        state.height = 8;
        state.exists = false; // Beginning-of-block tally clears prior ballots.
    }
    f.tick().await.unwrap();
    assert_eq!(f.pending().observed_height, 8);
    assert_eq!(f.rpc.state.lock().unwrap().broadcasts.len(), 1);
}

#[derive(Clone, Copy, Debug)]
enum BlockedCase {
    Disabled,
    PeriodMismatch,
    InactiveValidator,
    UnauthorizedSigner,
}

async fn blocked_case_recovers(case: BlockedCase) {
    let mut f = Fixture::new(9).await;
    {
        let mut state = f.rpc.state.lock().unwrap();
        match case {
            BlockedCase::Disabled => state.oracle_enabled = false,
            BlockedCase::PeriodMismatch => state.vote_period = 16,
            BlockedCase::InactiveValidator => state.validator_status = 1,
            BlockedCase::UnauthorizedSigner => state.resolved_validator = Address::ZERO,
        }
    }
    f.tick().await.unwrap();
    assert!(
        f.journal.as_ref().unwrap().pending().is_none(),
        "{case:?} must not sign"
    );
    assert!(
        f.rpc.state.lock().unwrap().broadcasts.is_empty(),
        "{case:?} must not broadcast"
    );
    assert_eq!(f.last_attempt, None, "{case:?} must be reconsidered");
    {
        let mut state = f.rpc.state.lock().unwrap();
        state.height = 10;
        state.oracle_enabled = true;
        state.vote_period = 8;
        state.validator_status = 2;
        state.resolved_validator = state.validator;
    }
    // A corrected state on the next committed head is reconsidered within
    // the same eight-block period; a blocked check cannot consume that period.
    f.tick().await.unwrap();
    assert_eq!(f.rpc.state.lock().unwrap().broadcasts.len(), 1);
    assert_eq!(f.pending().observed_height, 10);
}

#[tokio::test]
async fn disabled_oracle_blocks_signing_and_can_recover() {
    blocked_case_recovers(BlockedCase::Disabled).await;
}
#[tokio::test]
async fn period_mismatch_blocks_signing_and_can_recover() {
    blocked_case_recovers(BlockedCase::PeriodMismatch).await;
}
#[tokio::test]
async fn inactive_validator_blocks_signing_and_can_recover() {
    blocked_case_recovers(BlockedCase::InactiveValidator).await;
}
#[tokio::test]
async fn unauthorized_signer_blocks_signing_and_can_recover() {
    blocked_case_recovers(BlockedCase::UnauthorizedSigner).await;
}

#[tokio::test]
async fn stalled_pending_replaces_only_after_two_periods_at_the_same_nonce() {
    let mut f = Fixture::new(9).await;
    f.tick().await.unwrap();
    let original = f.pending();
    f.rpc.state.lock().unwrap().height = 25;
    f.tick().await.unwrap();
    assert_eq!(
        f.pending(),
        original,
        "exactly two periods rebroadcasts original"
    );
    f.rpc.state.lock().unwrap().height = 26;
    f.tick().await.unwrap();
    let replacement = f.pending();
    assert_eq!(replacement.nonce, original.nonce);
    assert_eq!(replacement.created_at, original.created_at);
    assert_eq!(replacement.observed_height, 26);
    assert_ne!(replacement.hash, original.hash);
    assert!(replacement.max_fee_per_gas > original.max_fee_per_gas);
    assert_eq!(f.rpc.state.lock().unwrap().broadcasts.len(), 3);
    f.restart();
    assert_eq!(f.pending(), replacement);
}

#[tokio::test]
async fn already_known_rebroadcast_is_acknowledged_but_not_counted_as_inclusion() {
    let mut f = Fixture::new(9).await;
    f.tick().await.unwrap();
    let pending = f.pending();
    {
        let mut state = f.rpc.state.lock().unwrap();
        state.height = 10;
        state.broadcast_error = Some("already known".into());
    }
    f.tick().await.unwrap();
    assert_eq!(f.pending(), pending);
    assert_eq!(f.health.votes_submitted.load(Ordering::Relaxed), 0);
    {
        let state = f.rpc.state.lock().unwrap();
        assert_eq!(
            state.broadcasts,
            vec![pending.raw.clone(), pending.raw.clone()]
        );
    }
    f.rpc.state.lock().unwrap().include(&pending, 10, true);
    f.tick().await.unwrap();
    assert!(f.journal.as_ref().unwrap().pending().is_none());
    assert_eq!(f.health.votes_submitted.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn admission_rejection_is_not_confused_with_already_known_acknowledgement() {
    let mut f = Fixture::new(9).await;
    f.rpc.state.lock().unwrap().broadcast_error =
        Some("zero-fee oracle vote already exists for validator".into());
    assert!(f.tick().await.is_err());
    assert!(f.journal.as_ref().unwrap().pending().is_some());
    assert_eq!(f.health.votes_submitted.load(Ordering::Relaxed), 0);
}
