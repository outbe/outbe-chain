//! The take-in: reads the dispatched messages of the stored leaves into the
//! `Relay` table.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use alloy_primitives::{Address, B256};
use alloy_provider::{DynProvider, Provider};
use alloy_rpc_types::Filter;
use alloy_sol_types::SolEvent;
use eyre::Result;
use tracing::{debug, error, warn};

use crate::abi::IMailbox;
use crate::errors::RelayError;
use crate::relayer::message::{message_id, Message};
use crate::store::{Leaf, Meta, RelayRow, Store};
use crate::sync::{is_range_error, Origin, MAX_LOG_RANGE};

/// Leaves that one take-in reads at most.
const TAKE_IN_BATCH: usize = 256;
/// Older leaves that one take-in reads at most. The take-in of the history
/// also waits while this number of messages is in the queue.
const HISTORY_BATCH: usize = 32;
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
            let mut messages = match dispatched(provider, self.origin.mailbox, from, to).await {
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
}

/// The messages that `mailbox` dispatched in the block range, by message id.
pub(crate) async fn dispatched(
    provider: &DynProvider,
    mailbox: Address,
    from: u64,
    to: u64,
) -> Result<HashMap<B256, Vec<u8>>> {
    let filter = Filter::new()
        .address(mailbox)
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
