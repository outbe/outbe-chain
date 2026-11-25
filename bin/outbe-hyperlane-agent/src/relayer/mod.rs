//! The relayer role: delivers the messages of the origin chains to their
//! destination chains.

pub mod deliver;
pub mod fetch;
pub mod message;
pub mod report;
pub mod retry;

use std::collections::{BTreeSet, HashMap};
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
use crate::store::{Leaf, Meta, RelayRow, Store};
use crate::sync::{finalized_block, is_range_error, Origin, MAX_LOG_RANGE};
use crate::validator::liveness::VALIDATOR_SET_ADDRESS;
use crate::validator::signer::DomainSigner;

use deliver::{
    announced, delivered, is_contract, metadata, multisig_ism, process, turn_position, Chains,
};
use fetch::{Fetcher, Quorum, Want};
use message::{message_id, Message};
use retry::backoff;

/// Leaves that one take-in reads at most.
const TAKE_IN_BATCH: usize = 256;
/// Older leaves that one take-in reads at most. The take-in of the history
/// also waits while this number of messages is in the queue.
const HISTORY_BATCH: usize = 32;
/// Interval of the delivered check of a dropped row.
const DROPPED_CHECK_SECS: u64 = 3600;

/// Takes the messages of one origin domain into the `Relay` table.
///
/// The validator sync stores each leaf (message id and block) after the
/// endpoint vote. The take-in reads the `Dispatch` logs of those blocks and
/// accepts a message only when its hash is the stored leaf. Thus the log
/// source cannot give a wrong message.
///
/// As the upstream relayer, the take-in starts at the newest leaf and goes
/// in two directions: new leaves first, in index order, then the older
/// leaves from the newest to the oldest. The history of a chain does not
/// delay a new message.
pub struct TakeIn {
    origin: Origin,
    store: Arc<Store>,
    chunk: u64,
    /// The endpoints whose read of the older leaves found a leaf without
    /// its log, and the number of those reads.
    missed: BTreeSet<usize>,
    misses: u32,
}

impl TakeIn {
    pub fn new(origin: Origin, store: Arc<Store>) -> Self {
        Self {
            origin,
            store,
            chunk: MAX_LOG_RANGE,
            missed: BTreeSet::new(),
            misses: 0,
        }
    }

    /// Takes in one batch of leaves and writes their messages as relay rows
    /// with `now` (Unix seconds) as the first-seen time. `synced`: the store
    /// has the newest leaf of the origin chain. `endpoint`: the index of
    /// `provider` and the number of RPC endpoints of the chain. Returns the
    /// number of leaves that a relay cursor passed.
    pub async fn tick(
        &mut self,
        provider: &DynProvider,
        now: u64,
        synced: bool,
        endpoint: (usize, usize),
    ) -> Result<usize> {
        let domain = self.origin.domain;
        let Some(next) = self.store.relay_cursor(domain)? else {
            // The first run starts at the newest leaf. Before the sync is at
            // the head, the newest stored leaf is an old leaf.
            if synced {
                self.store
                    .relay_start(domain, self.store.leaf_count(domain)?)?;
            }
            return Ok(0);
        };
        let new = self.store.leaves_from(domain, next, TAKE_IN_BATCH)?;
        if !new.is_empty() {
            let leaves: Vec<(u32, Leaf)> = new.into_iter().map(|(i, leaf, _)| (i, leaf)).collect();
            // A new leaf without its log stops the take-in: the endpoint
            // must have the logs of new blocks. The next tick reads again.
            let (taken, rows) = self.read(provider, leaves, now, false).await?;
            let value = next + taken as u32;
            self.store
                .relay_take_in(domain, &rows, Meta::RelayCursor, value)?;
            return Ok(taken);
        }
        let back = self.store.relay_back_cursor(domain)?;
        if back == 0 {
            return Ok(0);
        }
        // The history must not delay a new message: the older leaves come in
        // small batches, and only while the queue is short.
        let waiting = self.store.relay_rows(domain)?;
        if waiting.iter().filter(|(_, row)| !row.dropped).count() >= HISTORY_BATCH {
            return Ok(0);
        }
        let from = back.saturating_sub(HISTORY_BATCH as u32);
        let mut older = Vec::new();
        self.store
            .for_each_leaf_from(domain, from, back, |i, leaf| older.push((i, leaf)))?;
        // From the newest to the oldest. An old leaf without its log is
        // passed, but only after every endpoint had a read without it, and
        // after two such reads or more: one endpoint can be a pruned node
        // while another one keeps the old logs.
        older.reverse();
        let (index, endpoints) = endpoint;
        let every_endpoint = (0..endpoints).all(|i| i == index || self.missed.contains(&i));
        let pass_missing = every_endpoint && self.misses >= 1;
        let read = self.read(provider, older, now, pass_missing).await;
        match &read {
            Err(error) if error.is::<RelayError>() => {
                self.missed.insert(index);
                self.misses += 1;
            }
            Ok(_) => {
                self.missed.clear();
                self.misses = 0;
            }
            // An RPC error says nothing about the log.
            Err(_) => {}
        }
        let (taken, rows) = read?;
        let value = back - taken as u32;
        self.store
            .relay_take_in(domain, &rows, Meta::RelayBackCursor, value)?;
        Ok(taken)
    }

    /// Reads the messages of `leaves`, a run of leaves in index order or in
    /// reverse index order. One log request has a block range limit, so the
    /// read can take only the first part of the run. Returns the number of
    /// leaves that it took and their rows. `pass_missing`: a leaf without a
    /// `Dispatch` log is taken with no row; otherwise it is an error.
    async fn read(
        &mut self,
        provider: &DynProvider,
        mut leaves: Vec<(u32, Leaf)>,
        now: u64,
        pass_missing: bool,
    ) -> Result<(usize, Vec<(u32, RelayRow)>)> {
        let domain = self.origin.domain;
        let Some(first) = leaves.first().map(|(_, leaf)| leaf.block) else {
            return Ok((0, Vec::new()));
        };
        let last = leaves.last().map_or(first, |(_, leaf)| leaf.block);
        let descending = last < first;
        loop {
            // The range ends at the block of the last leaf: a node rejects a
            // range that goes past the head of the chain.
            let (from, to) = match descending {
                true => (first.saturating_sub(self.chunk - 1).max(last), first),
                false => (first, first.saturating_add(self.chunk - 1).min(last)),
            };
            let mut messages = match self.dispatched(provider, from, to).await {
                Ok(messages) => messages,
                Err(error) if self.chunk > 1 && is_range_error(&error.to_string()) => {
                    self.chunk = (self.chunk / 2).max(1);
                    debug!(domain, chunk = self.chunk, "smaller log range");
                    continue;
                }
                Err(error) => return Err(error),
            };
            self.chunk = (self.chunk * 2).min(MAX_LOG_RANGE);
            // The run is in block order: the leaves of the range are its start.
            let in_range = leaves
                .iter()
                .take_while(|(_, leaf)| (from..=to).contains(&leaf.block))
                .count();
            leaves.truncate(in_range);
            let mut rows = Vec::with_capacity(leaves.len());
            for (index, leaf) in &leaves {
                let Some(raw) = messages.remove(&leaf.message_id) else {
                    let missing = RelayError::NoDispatch {
                        domain,
                        index: *index,
                        block: leaf.block,
                    };
                    if !pass_missing {
                        return Err(missing.into());
                    }
                    error!(domain, index, %missing, "old message passed: the relayer cannot read it");
                    continue;
                };
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
            debug!(domain, from, to, taken = leaves.len(), "messages taken in");
            return Ok((leaves.len(), rows));
        }
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
    /// The message can never be delivered: no more attempts.
    Drop(String),
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The rows in the order of the upstream queue: a message with no failed
/// attempt first, by leaf index, then the retries by the time of their next
/// attempt. A retry does not delay a new message.
fn in_queue_order(mut rows: Vec<(u32, RelayRow)>) -> Vec<(u32, RelayRow)> {
    rows.sort_by_key(|(index, row)| match row.attempts {
        0 => (0, 0, *index),
        _ => (1, row.next_attempt, *index),
    });
    rows
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
    /// The validator sync of this domain: the take-in starts when the sync
    /// is at the head of the chain.
    pub signer: Arc<DomainSigner>,
}

impl Relayer {
    /// Runs forever. An error of a tick is logged and the next tick tries
    /// again: the relayer must never stop the validator role.
    pub async fn run(self, interval: Duration) {
        let domain = self.origin.domain;
        let mut take_in = TakeIn::new(self.origin.clone(), self.store.clone());
        loop {
            let pause = match self.tick(&mut take_in, unix_now()).await {
                Ok(next_attempt) => self.pause(interval, next_attempt),
                Err(error) => {
                    warn!(domain, error = format!("{error:#}"), "relayer tick failed");
                    interval
                }
            };
            tokio::time::sleep(pause).await;
        }
    }

    /// The time to the next tick: the poll interval, or less when a retry is
    /// due earlier. The fast retries of 2 seconds need this.
    fn pause(&self, interval: Duration, next_attempt: Option<u64>) -> Duration {
        match next_attempt {
            Some(at) => interval.min(Duration::from_secs(at.saturating_sub(unix_now()).max(1))),
            None => interval,
        }
    }

    /// One pass: takes new messages in, then looks at each row that is due.
    /// Returns the earliest time of a retry that is not due yet.
    async fn tick(&self, take_in: &mut TakeIn, now: u64) -> Result<Option<u64>> {
        let domain = self.origin.domain;
        let chain = self.chains.get(domain).ok_or(RelayError::NoChain(domain))?;
        // The operator sees in the status when `H` has no gas on this chain.
        let signer = self.wallet.default_signer().address();
        match chain.provider().get_balance(signer).await {
            Ok(wei) => self.health.set_balance(domain, wei),
            Err(error) => debug!(domain, %error, "balance read failed"),
        }
        // A take-in error must not stop the rows that are already in the queue.
        let endpoint = (chain.endpoint(), chain.providers.len());
        if let Err(error) = take_in
            .tick(chain.provider(), now, self.signer.synced(), endpoint)
            .await
        {
            warn!(
                domain,
                error = format!("{error:#}"),
                "take-in of new messages failed"
            );
            chain.rotate();
        }
        let rows = in_queue_order(self.store.relay_rows(domain)?);
        // A dropped row is due at a slow rate, only for the delivered check.
        let due = |row: &RelayRow| row.next_attempt <= now;
        if rows.iter().any(|(_, row)| due(row)) {
            let out = eth_call(
                &self.outbe,
                VALIDATOR_SET_ADDRESS,
                IValidatorSet::getActiveValidatorsCall {}.abi_encode(),
                BlockId::latest(),
            )
            .await?;
            let active = IValidatorSet::getActiveValidatorsCall::abi_decode_returns(&out)?;
            for (index, row) in rows {
                if !due(&row) {
                    continue;
                }
                let id = message_id(&row.message);
                if row.dropped {
                    self.look_at_dropped(index, row, id, now).await;
                    continue;
                }
                // As upstream, every error counts as a failed attempt, also
                // an RPC error: the relayer does not sort the errors.
                let outcome = match self.look(index, &row, id, now, &active).await {
                    Ok(outcome) => outcome,
                    Err(error) => Outcome::Retry {
                        fast: false,
                        reason: format!("{error:#}"),
                    },
                };
                self.record(index, row, id, outcome)?;
            }
        }
        let now = unix_now();
        Ok(self
            .store
            .relay_rows(domain)?
            .iter()
            .filter(|(_, row)| !row.dropped && row.next_attempt > now)
            .map(|(_, row)| row.next_attempt)
            .min())
    }

    /// A dropped row goes away when someone delivered the message, for
    /// example the operator by hand. The relayer checks this once an hour.
    async fn look_at_dropped(&self, index: u32, mut row: RelayRow, id: B256, now: u64) {
        let domain = self.origin.domain;
        let final_delivery = async {
            let message = Message::decode(&row.message)?;
            let dest = self
                .chains
                .get(message.destination)
                .ok_or(RelayError::NoChain(message.destination))?;
            let last_final = finalized_block(dest.provider(), dest.period).await?;
            let mailbox = dest.origin.mailbox;
            delivered(dest.provider(), mailbox, id, BlockId::number(last_final)).await
        };
        let stored = match final_delivery.await {
            Ok(true) => {
                info!(domain, index, %id, "dropped message is delivered and final");
                self.store.delete_relay(domain, index)
            }
            _ => {
                row.next_attempt = now + DROPPED_CHECK_SECS;
                self.store.put_relay(domain, index, &row)
            }
        };
        if let Err(error) = stored {
            warn!(domain, index, %error, "relay row write failed");
        }
    }

    /// Writes the result of one look into the relay row.
    fn record(&self, index: u32, mut row: RelayRow, id: B256, outcome: Outcome) -> Result<()> {
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
                    // The pause starts when the attempt fails, not when the
                    // tick started: one tick can run for minutes.
                    Some(pause) => {
                        row.next_attempt = unix_now() + pause.as_secs();
                        warn!(domain, index, %id, attempts = row.attempts, pause = pause.as_secs(), reason, "delivery attempt failed");
                    }
                    None => {
                        row.dropped = true;
                        error!(domain, index, %id, attempts = row.attempts, reason, "message dropped after the last attempt; anyone can still deliver it by hand");
                    }
                }
                self.store.put_relay(domain, index, &row)
            }
            Outcome::Drop(reason) => {
                row.dropped = true;
                error!(domain, index, %id, reason, "message dropped: it can never be delivered");
                self.store.put_relay(domain, index, &row)
            }
        }
    }

    /// Checks one message and delivers it when this relayer has the turn.
    /// The steps follow the upstream `prepare`, `submit` and `confirm`.
    async fn look(
        &self,
        index: u32,
        row: &RelayRow,
        id: B256,
        now: u64,
        active: &[Address],
    ) -> Result<Outcome> {
        let message = Message::decode(&row.message)?;
        let retry = |fast, reason: String| Ok(Outcome::Retry { fast, reason });
        let Some(dest) = self.chains.get(message.destination) else {
            return retry(
                false,
                format!("destination domain {} does not run", message.destination),
            );
        };
        let mailbox = dest.origin.mailbox;
        let provider = dest.provider();
        // After an error the next look reads from the next endpoint.
        let rotate = |error| {
            dest.rotate();
            error
        };
        let last_final = finalized_block(provider, dest.period)
            .await
            .map_err(rotate)?;
        // A reorg of the destination can undo a delivery: only a final block
        // ends the work on a message. Upstream waits 10 minutes for this.
        if delivered(provider, mailbox, id, BlockId::number(last_final))
            .await
            .map_err(rotate)?
        {
            return Ok(Outcome::Done);
        }
        if delivered(provider, mailbox, id, BlockId::latest())
            .await
            .map_err(rotate)?
        {
            return Ok(Outcome::Wait);
        }
        // A validator that is not active does not send: it only removes
        // its rows when another relayer delivers.
        let Some(place) = turn_position(active, self.validator, id) else {
            return Ok(Outcome::Wait);
        };
        if now < row.first_seen + place as u64 * dest.turn_secs {
            return Ok(Outcome::Wait);
        }
        if !is_contract(provider, message.recipient)
            .await
            .map_err(rotate)?
        {
            return Ok(Outcome::Drop(format!(
                "the recipient {} has no code on domain {}",
                message.recipient, message.destination
            )));
        }
        // A message whose ISM is not the controller ISM is dropped at once.
        // The controller ISM of a domain is fixed: if it changes, the daemon
        // stops the relayer of that domain before this point.
        let ism = multisig_ism(provider, mailbox, message.recipient, &row.message, dest.ism).await;
        let (validators, threshold) = match ism {
            Ok(set) => set,
            Err(error) if matches!(error.downcast_ref(), Some(RelayError::ForeignIsm { .. })) => {
                return Ok(Outcome::Drop(format!("{error:#}")))
            }
            Err(error) => return Err(rotate(error)),
        };
        // The daemon replaces the chain entry when the controller changes the
        // ValidatorAnnounce of the domain: read the address at each look.
        let origin_chain = self
            .chains
            .get(self.origin.domain)
            .ok_or(RelayError::NoChain(self.origin.domain))?;
        let locations = announced(
            origin_chain.provider(),
            origin_chain.origin.validator_announce,
            &validators,
        )
        .await
        .inspect_err(|_| origin_chain.rotate())?;
        let want = Want {
            domain: self.origin.domain,
            hook: self.origin.hook,
            index,
            message_id: id,
        };
        let (root, signatures) = match self
            .fetcher
            .quorum(&validators, &locations, threshold, &want)
            .await
        {
            Quorum::Found(root, signatures) => (root, signatures),
            Quorum::NotYet => {
                return retry(
                    true,
                    format!("fewer than {threshold} validators serve a signed checkpoint"),
                )
            }
            Quorum::Reorg(validator) => {
                return retry(
                    false,
                    format!(
                        "validator {validator} reports a reorg of domain {}",
                        want.domain
                    ),
                )
            }
        };
        let metadata = metadata(self.origin.hook, root, index, &signatures);
        match process(&dest, &self.wallet, metadata, &row.message)
            .await
            .map_err(rotate)?
        {
            Some(hash) => {
                self.health
                    .record_delivery(self.origin.domain, index, message.destination, hash);
                info!(domain = self.origin.domain, index, %id, destination = message.destination, %hash, "message delivered; waiting for a final block");
                Ok(Outcome::Wait)
            }
            None => Ok(Outcome::Wait),
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

        // Before the validator sync is at the head, the relayer does not start.
        assert_eq!(
            take_in.tick(&provider, 999, false, (0, 1)).await.unwrap(),
            0
        );
        assert_eq!(store.relay_cursor(5).unwrap(), None);
        // The first run starts at the newest leaf: leaves 0 to 2 are older.
        assert_eq!(take_in.tick(&provider, 999, true, (0, 1)).await.unwrap(), 0);
        assert_eq!(store.relay_cursor(5).unwrap(), Some(3));
        assert_eq!(store.relay_back_cursor(5).unwrap(), 3);
        assert!(store.relay_rows(5).unwrap().is_empty());

        // No new leaf: the older leaves come in.
        assert_eq!(
            take_in.tick(&provider, 1_000, true, (0, 1)).await.unwrap(),
            3
        );
        let rows = store.relay_rows(5).unwrap();
        // Leaf 2 has origin 6 in its header: the cursor passes it, no row.
        assert_eq!(rows.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(rows[1].1.message, messages[1]);
        assert_eq!(rows[1].1.first_seen, 1_000);
        assert_eq!(store.relay_back_cursor(5).unwrap(), 0);
        // No new leaf and no older leaf: no request.
        assert_eq!(
            take_in.tick(&provider, 1_001, true, (0, 1)).await.unwrap(),
            0
        );

        // The endpoint does not have the log of the new leaf 3: nothing moves.
        store.commit_batch(5, &[leaf(3, 13)], &[], 12).unwrap();
        let error = take_in
            .tick(&provider, 1_002, true, (0, 1))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no Dispatch log"), "{error}");
        assert_eq!(store.relay_cursor(5).unwrap(), Some(3));

        // An old leaf without its log is passed: it does not stop the others.
        // With two endpoints the first read is an error, so the caller reads
        // from the other endpoint. The second read passes the leaf.
        store.relay_start(5, 4).unwrap();
        let error = take_in
            .tick(&provider, 1_003, true, (0, 2))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no Dispatch log"), "{error}");
        assert_eq!(store.relay_back_cursor(5).unwrap(), 4);
        // The same endpoint again does not pass the leaf.
        assert!(take_in.tick(&provider, 1_003, true, (0, 2)).await.is_err());
        assert_eq!(
            take_in.tick(&provider, 1_003, true, (1, 2)).await.unwrap(),
            4
        );
        assert_eq!(store.relay_back_cursor(5).unwrap(), 0);
        let indexes: Vec<u32> = store
            .relay_rows(5)
            .unwrap()
            .iter()
            .map(|(i, _)| *i)
            .collect();
        assert_eq!(indexes, [0, 1]);
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
                "eth_getLogs" => dispatch.clone(),
                "eth_blockNumber" => json!("0x64"),
                "eth_chainId" => json!("0x61"),
                "eth_getTransactionCount" => json!("0x0"),
                "eth_getCode" => json!("0x60"),
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
        let other_message = raw(5, 1);
        let logs = json!([log(&message, 10), log(&other_message, 11)]);
        let url = chains_rpc(logs, validators, locations, seen.clone()).await;

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
                ISM,
                false,
            )));
        }
        let health = Arc::new(ValidatorHealth::new(store.clone()));
        let signer = Arc::new(
            DomainSigner::new(origin(5, MAILBOX), store.clone(), keys[0].clone()).unwrap(),
        );
        // The relayer of this domain started before leaf 0.
        store.relay_start(5, 0).unwrap();
        let relayer = |validator| Relayer {
            origin: origin(5, MAILBOX),
            store: store.clone(),
            chains: chains.clone(),
            fetcher: Arc::new(Fetcher::new().unwrap()),
            outbe: connect(&url).unwrap(),
            wallet: EthereumWallet::from(keys[0].clone()),
            validator,
            health: health.clone(),
            signer: signer.clone(),
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

        // The recipient of the next message names an ISM that is not the ISM
        // of the controller: the relayer drops the message at once and reads
        // no storage location of that ISM.
        seen.lock().unwrap().delivered = false;
        let next = Leaf {
            message_id: message_id(&other_message),
            block: 11,
        };
        store.commit_batch(5, &[(1, next)], &[], 11).unwrap();
        let foreign = Chains::default();
        for (domain, mailbox) in [(5, MAILBOX), (97, MAILBOX_97)] {
            foreign.insert(Arc::new(Chain::new(
                origin(domain, mailbox),
                vec![connect(&url).unwrap()],
                ReorgPeriod::Blocks(0),
                40,
                Address::repeat_byte(0x16),
                false,
            )));
        }
        let strict = Relayer {
            chains: foreign,
            ..relayer(V)
        };
        strict.tick(&mut take_in, 2_000).await.unwrap();
        strict.tick(&mut take_in, 2_100).await.unwrap();
        let rows = store.relay_rows(5).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].1.dropped && rows[0].1.attempts == 0);
        assert_eq!(seen.lock().unwrap().sent, 1);
    }
}
