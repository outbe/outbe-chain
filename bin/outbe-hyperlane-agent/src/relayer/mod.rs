//! The relayer role: delivers the messages of the origin chains to their
//! destination chains.

// The delivery loop uses these items in a later step.
#![allow(dead_code)]

pub mod fetch;
pub mod message;

use std::collections::HashMap;
use std::sync::Arc;

use alloy_primitives::B256;
use alloy_provider::{DynProvider, Provider};
use alloy_rpc_types::Filter;
use alloy_sol_types::SolEvent;
use eyre::Result;
use tracing::{debug, warn};

use crate::abi::IMailbox;
use crate::errors::RelayError;
use crate::store::{RelayRow, Store};
use crate::sync::{is_range_error, Origin, MAX_LOG_RANGE};

use message::{message_id, Message};

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
}
