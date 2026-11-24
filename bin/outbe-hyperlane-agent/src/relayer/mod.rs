//! The relayer role: delivers the messages of the origin chains to their
//! destination chains.

pub mod deliver;
pub mod fetch;
pub mod message;
pub mod retry;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy_eips::BlockId;
use alloy_network::EthereumWallet;
use alloy_primitives::{Address, B256};
use alloy_provider::{DynProvider, Provider};
use alloy_rpc_types::Filter;
use alloy_sol_types::{SolCall, SolEvent};
use eyre::Result;
use tracing::{debug, error, info, warn};

use crate::abi::{IMailbox, IValidatorSet};
use crate::client::eth_call;
use crate::errors::RelayError;
use crate::http::health::ValidatorHealth;
use crate::store::{RelayRow, Store};
use crate::sync::{finalized_block, is_range_error, Origin, MAX_LOG_RANGE};
use crate::validator::liveness::VALIDATOR_SET_ADDRESS;

use deliver::{announced, delivered, metadata, multisig_ism, process, turn_position, Chains};
use fetch::{Fetcher, Want};
use message::{message_id, Message};
use retry::backoff;

/// Leaves that one take-in reads at most.
const TAKE_IN_BATCH: usize = 256;

/// Takes the messages of one origin domain into the `Relay` table.
///
/// The validator sync stores each leaf (message id and block) after the
/// endpoint vote. The take-in reads the `Dispatch` logs of those blocks and
/// accepts a message only when its hash is the stored leaf. Thus the log
/// source cannot give a wrong message.
pub struct TakeIn {
    origin: Origin,
    store: Arc<Store>,
    chunk: u64,
}

impl TakeIn {
    pub fn new(origin: Origin, store: Arc<Store>) -> Self {
        Self {
            origin,
            store,
            chunk: MAX_LOG_RANGE,
        }
    }

    /// Reads the messages of the next stored leaves and writes them as relay
    /// rows with `now` (Unix seconds) as the first-seen time. Returns the
    /// number of leaves that the relay cursor passed.
    // ponytail: the first run takes in the full leaf history, one row and one `delivered` call per old message; start at the latest leaf if a new validator joins a chain with millions of messages
    pub async fn tick(&mut self, provider: &DynProvider, now: u64) -> Result<usize> {
        let domain = self.origin.domain;
        let next = self.store.relay_cursor(domain)?;
        let mut leaves = self.store.leaves_from(domain, next, TAKE_IN_BATCH)?;
        let Some(from) = leaves.first().map(|(_, leaf, _)| leaf.block) else {
            return Ok(0);
        };
        let to = loop {
            let to = from.saturating_add(self.chunk - 1);
            match self.dispatched(provider, from, to).await {
                Ok(mut messages) => {
                    self.chunk = (self.chunk * 2).min(MAX_LOG_RANGE);
                    leaves.retain(|(_, leaf, _)| leaf.block <= to);
                    let mut rows = Vec::with_capacity(leaves.len());
                    for (index, leaf, _) in &leaves {
                        let raw =
                            messages
                                .remove(&leaf.message_id)
                                .ok_or(RelayError::NoDispatch {
                                    domain,
                                    index: *index,
                                    block: leaf.block,
                                })?;
                        match Message::decode(&raw) {
                            Ok(message) if message.origin == domain => rows.push((
                                *index,
                                RelayRow {
                                    first_seen: now,
                                    attempts: 0,
                                    next_attempt: now,
                                    dropped: false,
                                    message: raw,
                                },
                            )),
                            other => {
                                warn!(
                                    domain,
                                    index,
                                    ?other,
                                    "message skipped: not a message of this domain"
                                )
                            }
                        }
                    }
                    self.store
                        .relay_take_in(domain, &rows, next + leaves.len() as u32)?;
                    break to;
                }
                Err(error) if self.chunk > 1 && is_range_error(&error.to_string()) => {
                    self.chunk = (self.chunk / 2).max(1);
                    debug!(domain, chunk = self.chunk, "smaller log range");
                }
                Err(error) => return Err(error),
            }
        };
        debug!(domain, from, to, taken = leaves.len(), "messages taken in");
        Ok(leaves.len())
    }

    /// The messages that the origin Mailbox dispatched in the block range, by
    /// message id.
    async fn dispatched(
        &self,
        provider: &DynProvider,
        from: u64,
        to: u64,
    ) -> Result<HashMap<B256, Vec<u8>>> {
        let filter = Filter::new()
            .address(self.origin.mailbox)
            .event_signature(IMailbox::Dispatch::SIGNATURE_HASH)
            .from_block(from)
            .to_block(to);
        let mut messages = HashMap::new();
        for log in provider.get_logs(&filter).await? {
            let event = IMailbox::Dispatch::decode_log_data(log.data())?;
            messages.insert(message_id(&event.message), event.message.to_vec());
        }
        Ok(messages)
    }
}

/// What the relayer does with a relay row after one look at it.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The message is delivered in a final block: the row goes away.
    Done,
    /// Nothing to do now: another relayer has the turn, or the delivery
    /// waits for a final block.
    Wait,
    /// The attempt failed. `fast`: only the validator signatures are missing.
    Retry { fast: bool, reason: String },
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The relayer of one origin domain: takes the messages in and delivers them
/// to their destination chains.
pub struct Relayer {
    pub origin: Origin,
    pub store: Arc<Store>,
    pub chains: Chains,
    pub fetcher: Arc<Fetcher>,
    /// Outbe RPC: the active validators give the delivery order.
    pub outbe: DynProvider,
    /// The key `H`. It pays the gas on each destination chain.
    pub wallet: EthereumWallet,
    /// Validator `V`: the place of this relayer in the delivery order. It
    /// sends only while `V` is an active validator.
    pub validator: Address,
    pub health: Arc<ValidatorHealth>,
}

impl Relayer {
    /// Runs forever. An error of a tick is logged and the next tick tries
    /// again: the relayer must never stop the validator role.
    pub async fn run(self, interval: Duration) {
        let domain = self.origin.domain;
        let mut take_in = TakeIn::new(self.origin.clone(), self.store.clone());
        loop {
            if let Err(error) = self.tick(&mut take_in, unix_now()).await {
                warn!(domain, error = format!("{error:#}"), "relayer tick failed");
                if let Some(chain) = self.chains.get(domain) {
                    chain.rotate();
                }
            }
            tokio::time::sleep(interval).await;
        }
    }

    async fn tick(&self, take_in: &mut TakeIn, now: u64) -> Result<()> {
        let domain = self.origin.domain;
        let chain = self.chains.get(domain).ok_or(RelayError::NoChain(domain))?;
        // The operator sees in the status when `H` has no gas on this chain.
        let signer = self.wallet.default_signer().address();
        match chain.provider().get_balance(signer).await {
            Ok(wei) => self.health.set_balance(domain, wei),
            Err(error) => debug!(domain, %error, "balance read failed"),
        }
        take_in.tick(chain.provider(), now).await?;
        let rows = self.store.relay_rows(domain)?;
        if rows
            .iter()
            .all(|(_, row)| row.dropped || row.next_attempt > now)
        {
            return Ok(());
        }
        let out = eth_call(
            &self.outbe,
            VALIDATOR_SET_ADDRESS,
            IValidatorSet::getActiveValidatorsCall {}.abi_encode(),
            BlockId::latest(),
        )
        .await?;
        let active = IValidatorSet::getActiveValidatorsCall::abi_decode_returns(&out)?;
        for (index, row) in rows {
            if row.dropped || row.next_attempt > now {
                continue;
            }
            let id = message_id(&row.message);
            let outcome = match self.look(index, &row, id, now, &active).await {
                Ok(outcome) => outcome,
                // An RPC error is not a fault of the message: no attempt counts.
                Err(error) => {
                    warn!(domain, index, %id, error = format!("{error:#}"), "message check failed");
                    continue;
                }
            };
            self.record(index, row, id, outcome, now)?;
        }
        Ok(())
    }

    /// Writes the result of one look into the relay row.
    fn record(
        &self,
        index: u32,
        mut row: RelayRow,
        id: B256,
        outcome: Outcome,
        now: u64,
    ) -> Result<()> {
        let domain = self.origin.domain;
        match outcome {
            Outcome::Done => {
                info!(domain, index, %id, "message is delivered and final");
                self.store.delete_relay(domain, index)
            }
            Outcome::Wait => Ok(()),
            Outcome::Retry { fast, reason } => {
                row.attempts += 1;
                match backoff(row.attempts, fast, id) {
                    Some(pause) => {
                        row.next_attempt = now + pause.as_secs();
                        warn!(domain, index, %id, attempts = row.attempts, pause = pause.as_secs(), reason, "delivery attempt failed");
                    }
                    None => {
                        row.dropped = true;
                        error!(domain, index, %id, attempts = row.attempts, reason, "message dropped after the last attempt; anyone can still deliver it by hand");
                    }
                }
                self.store.put_relay(domain, index, &row)
            }
        }
    }

    /// Checks one message and delivers it when this relayer has the turn.
    async fn look(
        &self,
        index: u32,
        row: &RelayRow,
        id: B256,
        now: u64,
        active: &[Address],
    ) -> Result<Outcome> {
        let message = Message::decode(&row.message)?;
        let Some(dest) = self.chains.get(message.destination) else {
            return Ok(Outcome::Retry {
                fast: false,
                reason: format!("destination domain {} does not run", message.destination),
            });
        };
        let mailbox = dest.origin.mailbox;
        let reads = async {
            let provider = dest.provider();
            let last_final = finalized_block(provider, dest.period).await?;
            // A reorg of the destination can undo a delivery: only a final
            // block ends the work on a message.
            if delivered(provider, mailbox, id, BlockId::number(last_final)).await? {
                return Ok(Some(Outcome::Done));
            }
            if delivered(provider, mailbox, id, BlockId::latest()).await? {
                return Ok(Some(Outcome::Wait));
            }
            eyre::Ok(None)
        };
        match reads.await {
            Ok(Some(outcome)) => return Ok(outcome),
            Ok(None) => {}
            Err(error) => {
                dest.rotate();
                return Err(error);
            }
        }
        // A validator that is not active does not send: it only removes
        // its rows when another relayer delivers.
        let Some(place) = turn_position(active, self.validator, id) else {
            return Ok(Outcome::Wait);
        };
        if now < row.first_seen + place as u64 * dest.turn_secs {
            return Ok(Outcome::Wait);
        }
        let retry = |fast, reason: String| Ok(Outcome::Retry { fast, reason });
        let (validators, threshold) =
            match multisig_ism(dest.provider(), mailbox, message.recipient, &row.message).await {
                Ok(set) => set,
                Err(error) => return retry(false, format!("ISM read failed: {error:#}")),
            };
        let origin_chain = self
            .chains
            .get(self.origin.domain)
            .ok_or(RelayError::NoChain(self.origin.domain))?;
        let locations = announced(
            origin_chain.provider(),
            self.origin.validator_announce,
            &validators,
        )
        .await?;
        let want = Want {
            domain: self.origin.domain,
            hook: self.origin.hook,
            index,
            message_id: id,
        };
        let Some((root, signatures)) = self
            .fetcher
            .quorum(&validators, &locations, threshold, &want)
            .await
        else {
            return retry(
                true,
                format!("fewer than {threshold} validators serve a signed checkpoint"),
            );
        };
        let metadata = metadata(self.origin.hook, root, index, &signatures);
        match process(&dest, &self.wallet, metadata, &row.message).await {
            Ok(Some(hash)) => {
                self.health
                    .record_delivery(self.origin.domain, index, message.destination, hash);
                info!(domain = self.origin.domain, index, %id, destination = message.destination, %hash, "message delivered; waiting for a final block");
                Ok(Outcome::Wait)
            }
            Ok(None) => Ok(Outcome::Wait),
            Err(error) => retry(false, format!("process failed: {error:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, Bytes, LogData};
    use serde_json::{json, Value};

    use super::*;
    use crate::store::Leaf;
    use crate::sync::connect;
    use crate::sync::tests::serve_json_rpc;

    const MAILBOX: Address = Address::repeat_byte(0xb2);

    fn raw(origin: u32, nonce: u32) -> Vec<u8> {
        let mut raw = vec![3u8];
        raw.extend_from_slice(&nonce.to_be_bytes());
        raw.extend_from_slice(&origin.to_be_bytes());
        raw.extend_from_slice(&[0xaa; 32]);
        raw.extend_from_slice(&97u32.to_be_bytes());
        raw.extend_from_slice(&[0xbb; 32]);
        raw.extend_from_slice(b"body");
        raw
    }

    fn log(message: &[u8], block: u64) -> Value {
        let event = IMailbox::Dispatch {
            sender: Address::ZERO,
            destination: 97,
            recipient: B256::repeat_byte(0xbb),
            message: Bytes::copy_from_slice(message),
        };
        let data: LogData = event.encode_log_data();
        json!({
            "address": MAILBOX,
            "topics": data.topics(),
            "data": data.data,
            "blockNumber": format!("{block:#x}"),
            "blockHash": B256::ZERO,
            "transactionHash": B256::ZERO,
            "transactionIndex": "0x0",
            "logIndex": "0x0",
            "removed": false,
        })
    }

    #[tokio::test]
    async fn take_in_accepts_only_messages_that_hash_to_the_leaf() {
        let messages = [raw(5, 0), raw(5, 1), raw(6, 2), raw(5, 3)];
        // The endpoint also returns a message that is not a stored leaf.
        let logs = vec![
            log(&messages[0], 10),
            log(&messages[1], 10),
            log(&raw(5, 99), 11),
            log(&messages[2], 12),
        ];
        let url = serve_json_rpc(move |req: &Value| {
            assert_eq!(req["method"], "eth_getLogs");
            assert_eq!(req["params"][0]["fromBlock"], "0xa");
            json!({"jsonrpc": "2.0", "id": req["id"], "result": logs})
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let leaf = |i: usize, block| {
            (
                i as u32,
                Leaf {
                    message_id: message_id(&messages[i]),
                    block,
                },
            )
        };
        store
            .commit_batch(5, &[leaf(0, 10), leaf(1, 10), leaf(2, 12)], &[], 12)
            .unwrap();
        let origin = Origin {
            domain: 5,
            hook: Address::ZERO,
            validator_announce: Address::ZERO,
            mailbox: MAILBOX,
            start_block: 1,
        };
        let mut take_in = TakeIn::new(origin, store.clone());
        let provider = connect(&url).unwrap();

        assert_eq!(take_in.tick(&provider, 1_000).await.unwrap(), 3);
        let rows = store.relay_rows(5).unwrap();
        // Leaf 2 has origin 6 in its header: the cursor passes it, no row.
        assert_eq!(rows.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(rows[1].1.message, messages[1]);
        assert_eq!(rows[1].1.first_seen, 1_000);
        assert_eq!(store.relay_cursor(5).unwrap(), 3);
        // No new leaf: no request.
        assert_eq!(take_in.tick(&provider, 1_001).await.unwrap(), 0);

        // The endpoint does not have the log of leaf 3: nothing moves.
        store.commit_batch(5, &[leaf(3, 10)], &[], 12).unwrap();
        let error = take_in.tick(&provider, 1_002).await.unwrap_err();
        assert!(error.to_string().contains("no Dispatch log"), "{error}");
        assert_eq!(store.relay_cursor(5).unwrap(), 3);
    }

    const VA: Address = Address::repeat_byte(0xa1);
    const MAILBOX_97: Address = Address::repeat_byte(0xb9);
    const ISM: Address = Address::repeat_byte(0x15);
    const V: Address = Address::repeat_byte(0x77);
    const W: Address = Address::repeat_byte(0x78);

    /// What the test reads back from the mock chain.
    #[derive(Default)]
    struct Seen {
        /// The calldata of the last `eth_estimateGas`.
        estimate: Option<Bytes>,
        sent: u32,
        delivered: bool,
    }

    /// One endpoint for the origin chain 5, the destination chain 97 and
    /// Outbe. The destination has a message-id multisig ISM with `validators`
    /// and threshold 2. A sent transaction delivers the message.
    async fn chains_rpc(
        dispatch: Value,
        validators: Vec<Address>,
        locations: Vec<Vec<String>>,
        seen: Arc<std::sync::Mutex<Seen>>,
    ) -> String {
        use alloy_sol_types::SolValue;

        use crate::abi::{IIsm, IValidatorAnnounce};

        serve_json_rpc(move |req: &Value| {
            let mut seen = seen.lock().unwrap();
            let result = match req["method"].as_str().unwrap() {
                "eth_getLogs" => json!([dispatch]),
                "eth_blockNumber" => json!("0x64"),
                "eth_chainId" => json!("0x61"),
                "eth_getTransactionCount" => json!("0x0"),
                // 1.5 native tokens.
                "eth_getBalance" => json!("0x14d1120d7b160000"),
                "eth_gasPrice" | "eth_maxPriorityFeePerGas" => json!("0x3b9aca00"),
                "eth_feeHistory" => json!({
                    "oldestBlock": "0x63",
                    "baseFeePerGas": ["0x3b9aca00", "0x3b9aca00"],
                    "gasUsedRatio": [0.5],
                    "reward": [["0x3b9aca00"]],
                }),
                "eth_estimateGas" => {
                    seen.estimate =
                        serde_json::from_value(req["params"][0]["input"].clone()).unwrap();
                    json!("0x186a0")
                }
                "eth_sendRawTransaction" => {
                    seen.sent += 1;
                    seen.delivered = true;
                    json!(B256::repeat_byte(0xab))
                }
                "eth_getTransactionReceipt" => json!({
                    "transactionHash": B256::repeat_byte(0xab),
                    "transactionIndex": "0x0",
                    "blockHash": B256::repeat_byte(3),
                    "blockNumber": "0x64",
                    "from": V,
                    "to": MAILBOX_97,
                    "cumulativeGasUsed": "0x5208",
                    "gasUsed": "0x5208",
                    "effectiveGasPrice": "0x1",
                    "contractAddress": null,
                    "logs": [],
                    "logsBloom": format!("0x{}", "00".repeat(256)),
                    "status": "0x1",
                    "type": "0x2",
                }),
                "eth_call" => {
                    let call = &req["params"][0];
                    let to: Address = serde_json::from_value(call["to"].clone()).unwrap();
                    let input = if call["input"].is_null() {
                        &call["data"]
                    } else {
                        &call["input"]
                    };
                    let data: Bytes = serde_json::from_value(input.clone()).unwrap();
                    let selector: [u8; 4] = data[..4].try_into().unwrap();
                    let out = match (to, selector) {
                        (VALIDATOR_SET_ADDRESS, _) => vec![V, W].abi_encode(),
                        (MAILBOX_97, IMailbox::deliveredCall::SELECTOR) => {
                            seen.delivered.abi_encode()
                        }
                        (MAILBOX_97, IMailbox::recipientIsmCall::SELECTOR) => ISM.abi_encode(),
                        (ISM, IIsm::moduleTypeCall::SELECTOR) => 5u32.abi_encode(),
                        (ISM, IIsm::validatorsAndThresholdCall::SELECTOR) => {
                            (validators.clone(), 2u32).abi_encode_params()
                        }
                        (VA, IValidatorAnnounce::getAnnouncedStorageLocationsCall::SELECTOR) => {
                            locations.abi_encode()
                        }
                        other => panic!("unexpected call {other:?}"),
                    };
                    json!(Bytes::from(out))
                }
                other => panic!("unexpected method {other}"),
            };
            json!({"jsonrpc": "2.0", "id": req["id"], "result": result})
        })
        .await
    }

    /// Serves the checkpoint of leaf 0 that `key` signed. Returns the location.
    async fn bucket(key: &alloy_signer_local::PrivateKeySigner, want: &Want) -> String {
        use crate::checkpoint::{Checkpoint, CheckpointWithMessageId, Signed};

        let value = CheckpointWithMessageId {
            checkpoint: Checkpoint {
                merkle_tree_hook_address: want.hook.into_word(),
                mailbox_domain: want.domain,
                root: B256::repeat_byte(0x44),
                index: want.index,
            },
            message_id: want.message_id,
        };
        let body = serde_json::to_string(&Signed::sign(value, key).unwrap()).unwrap();
        let app = axum::Router::new().route(
            "/b/5/checkpoint_0_with_id.json",
            axum::routing::get(move || {
                let body = body.clone();
                async move { body }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let location = format!("s3+http://{}/b/5", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        location
    }

    #[tokio::test]
    async fn a_message_goes_from_the_leaf_to_a_final_delivery() {
        use alloy_signer_local::PrivateKeySigner;

        use crate::config::ReorgPeriod;
        use deliver::Chain;

        let message = raw(5, 0);
        let id = message_id(&message);
        let hook = Address::repeat_byte(0x11);
        let want = Want {
            domain: 5,
            hook,
            index: 0,
            message_id: id,
        };
        // Three ISM validators. The second one serves no checkpoint.
        let keys: Vec<_> = (0..3).map(|_| PrivateKeySigner::random()).collect();
        let validators: Vec<Address> = keys.iter().map(|k| k.address()).collect();
        let locations = vec![
            vec![bucket(&keys[0], &want).await],
            vec!["s3+http://127.0.0.1:1/b/5".to_owned()],
            vec![bucket(&keys[2], &want).await],
        ];
        let seen = Arc::new(std::sync::Mutex::new(Seen::default()));
        let url = chains_rpc(log(&message, 10), validators, locations, seen.clone()).await;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let leaf = Leaf {
            message_id: id,
            block: 10,
        };
        store.commit_batch(5, &[(0, leaf)], &[], 10).unwrap();
        let origin = |domain, mailbox| Origin {
            domain,
            hook,
            validator_announce: VA,
            mailbox,
            start_block: 1,
        };
        let chains = Chains::default();
        for (domain, mailbox) in [(5, MAILBOX), (97, MAILBOX_97)] {
            chains.insert(Arc::new(Chain::new(
                origin(domain, mailbox),
                vec![connect(&url).unwrap()],
                ReorgPeriod::Blocks(0),
                40,
            )));
        }
        let health = Arc::new(ValidatorHealth::new(store.clone()));
        health.set_relayer(true);
        let relayer = |validator| Relayer {
            origin: origin(5, MAILBOX),
            store: store.clone(),
            chains: chains.clone(),
            fetcher: Arc::new(Fetcher::new().unwrap()),
            outbe: connect(&url).unwrap(),
            wallet: EthereumWallet::from(keys[0].clone()),
            validator,
            health: health.clone(),
        };
        let mut take_in = TakeIn::new(origin(5, MAILBOX), store.clone());

        // A relayer that is not an active validator never sends.
        let outsider = relayer(Address::repeat_byte(0x99));
        outsider.tick(&mut take_in, 1_000).await.unwrap();
        outsider.tick(&mut take_in, 9_000).await.unwrap();
        assert_eq!(seen.lock().unwrap().sent, 0);
        assert_eq!(store.relay_rows(5).unwrap()[0].1.first_seen, 1_000);

        // Two active validators: one has the first turn, the other waits one
        // turn of 40 seconds.
        let (first, second) = match turn_position(&[V, W], V, id) {
            Some(0) => (relayer(V), relayer(W)),
            _ => (relayer(W), relayer(V)),
        };
        second.tick(&mut take_in, 1_039).await.unwrap();
        assert_eq!(seen.lock().unwrap().sent, 0);

        first.tick(&mut take_in, 1_001).await.unwrap();
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.sent, 1);
            let call = IMailbox::processCall::abi_decode(seen.estimate.as_ref().unwrap()).unwrap();
            assert_eq!(call.message.as_ref(), message.as_slice());
            // hook, root, index, then the signatures of validators 0 and 2.
            assert_eq!(call.metadata.len(), 68 + 2 * 65);
            assert_eq!(&call.metadata[12..32], hook.as_slice());
            assert_eq!(&call.metadata[32..64], &[0x44; 32]);
            assert_eq!(&call.metadata[64..68], &[0; 4]);
        }
        // The row stays until the delivery is in a final block.
        assert_eq!(store.relay_rows(5).unwrap().len(), 1);
        let status = health.status_json();
        assert_eq!(status["relayer"]["delivered"], 1);
        assert_eq!(status["relayer"]["last_delivery"]["destination"], 97);
        assert_eq!(
            status["domains"],
            json!({}),
            "no started domain in this test"
        );

        // The next tick sees the final delivery: the row goes away. The
        // second relayer does not send a transaction after its wait.
        second.tick(&mut take_in, 1_050).await.unwrap();
        assert!(store.relay_rows(5).unwrap().is_empty());
        assert_eq!(seen.lock().unwrap().sent, 1);
    }
}
