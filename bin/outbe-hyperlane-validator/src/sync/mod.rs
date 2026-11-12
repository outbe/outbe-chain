//! Origin chain sync: verified, ordered `MerkleTreeHook` leaves up to a
//! finalized block.
//!
//! Ported from hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/agents/validator/src/` (Apache-2.0, see `LICENSE-APACHE`):
//! `checkpoint_consensus.rs` (endpoint vote), `rpc.rs` (URL dedup),
//! `submit.rs` (`evaluate_checkpoint_batch`, the conflict check) and the RPC
//! path of `merkle_tree_hook_sync.rs`. Changes: alloy types, one log source per
//! tick, unverified leaves stay in memory, no RocksDB, a WebSocket event only
//! wakes the poll.
//!
//! The log source never authorizes a root. A leaf becomes verified only when
//! enough endpoints return the same `latestCheckpoint()` as the local tree.

mod discover;
#[cfg(test)]
pub(crate) mod tests;
mod vote;

pub use discover::{check_origin, controller_domains, ControllerDomain};
pub use vote::{dedup_urls, evaluate, Observed, Vote};

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, B256};
use alloy_provider::{DynProvider, Provider, ProviderBuilder, WsConnect};
use alloy_rpc_types::Filter;
use alloy_sol_types::{SolCall, SolEvent};
use eyre::{Context, Result};
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tracing::{debug, error, warn};

use crate::abi::IMerkleTreeHook;
use crate::client::eth_call;
use crate::config::{OriginConfig, ReorgPeriod, RpcConsensus};
use crate::errors::SyncError;
use crate::merkle::IncrementalMerkle;
use crate::store::Store;

/// Time limit of one endpoint read in the vote.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest `eth_getLogs` block range. The Outbe RPC rejects larger ranges.
pub const MAX_LOG_RANGE: u64 = 100_000;
const MAX_WS_BACKOFF: Duration = Duration::from_secs(60);
/// Ticks in a row that wait for leaves before the log source rotates.
const STALE_SOURCE_TICKS: u32 = 3;
/// Rewinds without a new verified leaf before the sync shows as stalled.
const MAX_REWINDS: u32 = 5;
/// Time limits of each HTTP JSON-RPC request.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The block range of the next `eth_getLogs` request.
fn log_range(start: u64, end: u64, chunk: u64) -> (u64, u64) {
    (start, end.min(start.saturating_add(chunk - 1)))
}

/// True when the RPC rejected a log request because of its size. A rate
/// limit is not a range error: the next tick reads the same range again.
fn is_range_error(message: &str) -> bool {
    let message = message.to_lowercase();
    let rate_limit = ["429", "rate limit", "too many requests"]
        .iter()
        .any(|word| message.contains(word));
    !rate_limit
        && ["range", "limit", "too many", "exceed"]
            .iter()
            .any(|word| message.contains(word))
}

/// A leaf with the tree root after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncedLeaf {
    pub index: u32,
    pub message_id: B256,
    pub block: u64,
    pub root: B256,
}

/// Adds leaves to `tip` and `pending` in index order. A leaf below `next`
/// is a replay of a known leaf and is skipped. A leaf above `next` is a gap.
fn append(
    domain: u32,
    tip: &mut IncrementalMerkle,
    pending: &mut Vec<SyncedLeaf>,
    mut leaves: Vec<(u32, B256, u64)>,
) -> Result<()> {
    leaves.sort_unstable_by_key(|(index, ..)| *index);
    let mut new_tip = tip.clone();
    let mut new = Vec::new();
    for (index, message_id, block) in leaves {
        let expected = new_tip.count() as u32;
        if index < expected {
            continue;
        }
        if index > expected {
            return Err(SyncError::LeafGap {
                domain,
                expected,
                got: index,
            }
            .into());
        }
        new_tip.ingest(message_id);
        new.push(SyncedLeaf {
            index,
            message_id,
            block,
            root: new_tip.root(),
        });
    }
    *tip = new_tip;
    pending.extend(new);
    Ok(())
}

/// Leaves that the vote verified. The signer commits the leaves, the signed
/// root of the last leaf and `cursor` in one store transaction. The backfill
/// signs the other leaves.
#[derive(Debug, PartialEq, Eq)]
pub struct VerifiedBatch {
    pub domain: u32,
    pub leaves: Vec<SyncedLeaf>,
    /// Every leaf at or below this block is in the store after the commit.
    pub cursor: u64,
}

/// What one sync tick found.
#[derive(Debug, PartialEq, Eq)]
pub enum Tick {
    Batch(VerifiedBatch),
    Idle,
    Reorg {
        index: u32,
        local: B256,
        observed: B256,
    },
}

/// The contracts of one origin domain, read from the chain.
#[derive(Clone, Debug)]
pub struct Origin {
    pub domain: u32,
    pub hook: Address,
    pub validator_announce: Address,
    pub mailbox: Address,
    pub start_block: u64,
}

/// An HTTP provider with connect and request time limits. Without them a
/// silent endpoint blocks its caller forever.
pub fn connect(url: &str) -> Result<DynProvider> {
    let url = url
        .parse()
        .with_context(|| format!("invalid RPC URL {url}"))?;
    let provider = ProviderBuilder::new().with_reqwest(url, |builder| {
        builder
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("HTTP client")
    });
    Ok(provider.erased())
}

/// The finalized block of one endpoint.
async fn finalized_block(provider: &DynProvider, period: ReorgPeriod) -> Result<u64> {
    match period {
        ReorgPeriod::Blocks(blocks) => {
            Ok(provider.get_block_number().await?.saturating_sub(blocks))
        }
        ReorgPeriod::Tag(_) => Ok(provider
            .get_block_by_number(BlockNumberOrTag::Finalized)
            .await?
            .ok_or(SyncError::NoFinalizedBlock)?
            .header
            .number),
    }
}

/// One endpoint's `latestCheckpoint()` at its own finalized block.
async fn read_checkpoint(
    provider: &DynProvider,
    hook: Address,
    period: ReorgPeriod,
) -> Result<Observed> {
    let block = finalized_block(provider, period).await?;
    let out = eth_call(
        provider,
        hook,
        IMerkleTreeHook::latestCheckpointCall {}.abi_encode(),
        BlockId::number(block),
    )
    .await?;
    let checkpoint = IMerkleTreeHook::latestCheckpointCall::abi_decode_returns(&out)?;
    Ok(Observed {
        index: checkpoint.index,
        root: checkpoint.root,
    })
}

/// Sync state of one origin domain.
pub struct DomainSync {
    origin: Origin,
    endpoints: Vec<DynProvider>,
    consensus: RpcConsensus,
    period: ReorgPeriod,
    /// The endpoint that gives finality and logs. It rotates after an error,
    /// a rewind, or when the endpoints stay ahead of its finality.
    source: usize,
    /// The tree of the stored leaves.
    committed: IncrementalMerkle,
    /// The tree of the stored and the pending leaves.
    tip: IncrementalMerkle,
    pending: Vec<SyncedLeaf>,
    /// Every leaf at or below this block is stored.
    cursor: u64,
    /// The last block that the log reads covered.
    scanned: u64,
    chunk: u64,
    /// Compares replayed leaves with the stored leaves.
    store: Arc<Store>,
    /// Ticks in a row that waited for leaves.
    waiting: u32,
    /// Rewinds since the last verified leaf.
    rewinds: u32,
    /// Endpoints whose logs contradict a stored leaf. One endpoint cannot
    /// halt the domain: the conflict needs the agreement count of the vote.
    replay_conflicts: BTreeSet<usize>,
}

impl DomainSync {
    /// Rebuilds the tree from the stored leaves and continues after the
    /// stored cursor. After a store error, build a new `DomainSync`: the
    /// in-memory state may be ahead of the store.
    pub fn new(origin: Origin, config: &OriginConfig, store: Arc<Store>) -> Result<Self> {
        let mut committed = IncrementalMerkle::default();
        store.for_each_leaf(origin.domain, u32::MAX, |_, leaf| {
            committed.ingest(leaf.message_id)
        })?;
        let cursor = match store.cursor(origin.domain)? {
            Some(cursor) => cursor,
            None => origin.start_block.saturating_sub(1),
        };
        let endpoints = dedup_urls(&config.rpc_urls)
            .iter()
            .map(|url| connect(url))
            .collect::<Result<_>>()?;
        Ok(Self {
            origin,
            endpoints,
            consensus: config.rpc_consensus,
            period: config.reorg_period,
            source: 0,
            tip: committed.clone(),
            committed,
            pending: Vec::new(),
            cursor,
            scanned: cursor,
            chunk: MAX_LOG_RANGE,
            store,
            waiting: 0,
            rewinds: 0,
            replay_conflicts: BTreeSet::new(),
        })
    }

    pub fn domain(&self) -> u32 {
        self.origin.domain
    }

    /// True when the endpoints reject the read leaves again and again. A
    /// reorg below the stored cursor gives this: the replay does not see it.
    pub fn stalled(&self) -> bool {
        self.rewinds >= MAX_REWINDS
    }

    fn rotate_source(&mut self) {
        self.source = (self.source + 1) % self.endpoints.len();
    }

    /// Reads new finalized leaves, runs the endpoint vote and returns the
    /// leaves that the vote verified.
    pub async fn tick(&mut self) -> Result<Tick> {
        if let Err(error) = self.read_leaves().await {
            if let Some(&SyncError::ReplayMismatch {
                index,
                stored,
                read,
                ..
            }) = error.downcast_ref()
            {
                self.replay_conflicts.insert(self.source);
                let required = self.consensus.required(self.endpoints.len());
                if self.replay_conflicts.len() >= required {
                    return Ok(Tick::Reorg {
                        index,
                        local: stored,
                        observed: read,
                    });
                }
                warn!(
                    domain = self.domain(),
                    source = self.source,
                    index,
                    %stored,
                    %read,
                    "log source contradicts a stored leaf; reading from the next endpoint"
                );
            }
            self.rotate_source();
            return Err(error);
        }
        let observed = self.vote().await;
        let committed = self.committed.index().map(|i| (i, self.committed.root()));
        let required = self.consensus.required(self.endpoints.len());
        let vote = evaluate(&observed, required, committed, &self.pending);
        self.waiting = match vote {
            Vote::WaitingForLeaves => self.waiting + 1,
            _ => 0,
        };
        match vote {
            Vote::Verified(target) => Ok(self.take_verified(target)),
            Vote::Reorg {
                index,
                local,
                observed: root,
            } => {
                // This replaces the upstream reorg reporter: log only.
                error!(
                    domain = self.domain(),
                    index,
                    %local,
                    endpoints = ?observed,
                    "endpoints agree on a different root at a committed index"
                );
                Ok(Tick::Reorg {
                    index,
                    local,
                    observed: root,
                })
            }
            Vote::Rewind => {
                warn!(
                    domain = self.domain(),
                    "endpoints reject the read leaves; reading again"
                );
                self.pending.clear();
                self.tip = self.committed.clone();
                self.scanned = self.cursor;
                self.rotate_source();
                self.rewinds += 1;
                if self.stalled() {
                    error!(
                        domain = self.domain(),
                        rewinds = self.rewinds,
                        "no verified leaf after many rewinds: check for a reorg below the stored cursor"
                    );
                }
                Ok(Tick::Idle)
            }
            vote => {
                // The source finality is behind the endpoints: read the logs
                // from the next endpoint.
                if self.waiting >= STALE_SOURCE_TICKS {
                    warn!(
                        domain = self.domain(),
                        "endpoints stay ahead of the log source; the source rotates"
                    );
                    self.waiting = 0;
                    self.rotate_source();
                }
                if !self.pending.is_empty() {
                    warn!(
                        domain = self.domain(),
                        ?vote,
                        ?observed,
                        "leaves not verified yet"
                    );
                }
                Ok(Tick::Idle)
            }
        }
    }

    fn take_verified(&mut self, target: u32) -> Tick {
        let split = self.pending.partition_point(|leaf| leaf.index <= target);
        if split == 0 {
            return Tick::Idle;
        }
        self.rewinds = 0;
        self.replay_conflicts.clear();
        // ponytail: the first sync holds all unverified leaves in memory (~1 GB per 5M leaves); a vote at intermediate blocks needs archive RPC state
        let leaves = if split == self.pending.len() {
            std::mem::take(&mut self.pending)
        } else {
            self.pending.drain(..split).collect()
        };
        for leaf in &leaves {
            self.committed.ingest(leaf.message_id);
        }
        // A remaining leaf must be read again after a restart: stop the
        // cursor before its block. The replay skips the stored leaves.
        self.cursor = match self.pending.first() {
            Some(next) => next.block.saturating_sub(1),
            None => self.scanned,
        };
        Tick::Batch(VerifiedBatch {
            domain: self.domain(),
            leaves,
            cursor: self.cursor,
        })
    }

    async fn read_leaves(&mut self) -> Result<()> {
        let source = self.endpoints[self.source].clone();
        let finalized = finalized_block(&source, self.period).await?;
        let mut start = self.scanned + 1;
        while start <= finalized {
            let (from, to) = log_range(start, finalized, self.chunk);
            let filter = Filter::new()
                .address(self.origin.hook)
                .event_signature(IMerkleTreeHook::InsertedIntoTree::SIGNATURE_HASH)
                .from_block(from)
                .to_block(to);
            let logs = match source.get_logs(&filter).await {
                Ok(logs) => logs,
                Err(error) if self.chunk > 1 && is_range_error(&error.to_string()) => {
                    self.chunk = (self.chunk / 2).max(1);
                    debug!(
                        domain = self.domain(),
                        chunk = self.chunk,
                        "smaller log range"
                    );
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            self.chunk = (self.chunk * 2).min(MAX_LOG_RANGE);
            let mut leaves = Vec::with_capacity(logs.len());
            for log in logs {
                let event = IMerkleTreeHook::InsertedIntoTree::decode_log_data(log.data())?;
                let block = log.block_number.ok_or(SyncError::LogWithoutBlock)?;
                self.check_replay(event.index, event.messageId)?;
                leaves.push((event.index, event.messageId, block));
            }
            append(self.domain(), &mut self.tip, &mut self.pending, leaves)?;
            self.scanned = to;
            start = to + 1;
        }
        Ok(())
    }

    /// A leaf below the committed count is a replay. Another message id than
    /// the stored one is a reorg of a verified leaf.
    fn check_replay(&self, index: u32, read: B256) -> Result<()> {
        if index as usize >= self.committed.count() {
            return Ok(());
        }
        match self.store.leaf(self.domain(), index)? {
            Some(leaf) if leaf.message_id != read => Err(SyncError::ReplayMismatch {
                domain: self.domain(),
                index,
                stored: leaf.message_id,
                read,
            }
            .into()),
            _ => Ok(()),
        }
    }

    /// Every endpoint reads in parallel. A failed or slow endpoint keeps its
    /// slot as `None`.
    async fn vote(&self) -> Vec<Option<Observed>> {
        let reads = self.endpoints.iter().enumerate().map(|(slot, provider)| async move {
            match tokio::time::timeout(
                RPC_TIMEOUT,
                read_checkpoint(provider, self.origin.hook, self.period),
            )
            .await
            {
                Ok(Ok(observed)) => Some(observed),
                Ok(Err(error)) => {
                    debug!(domain = self.domain(), slot, %error, "endpoint checkpoint read failed");
                    None
                }
                Err(_) => {
                    debug!(domain = self.domain(), slot, "endpoint checkpoint read timed out");
                    None
                }
            }
        });
        futures::future::join_all(reads).await
    }
}

/// Starts the WebSocket waker of an origin in `tasks` when `ws_url` is set.
/// Dropping `tasks` stops the subscription.
pub fn ws_wake(
    config: &OriginConfig,
    hook: Address,
    tasks: &mut JoinSet<()>,
) -> Option<Arc<Notify>> {
    let url = config.ws_url.clone()?;
    let wake = Arc::new(Notify::new());
    tasks.spawn(ws_waker(url, hook, wake.clone()));
    Some(wake)
}

/// Wakes `wake` on every `InsertedIntoTree` event of `hook`. The event does
/// not carry trust: the poll reads and verifies as usual. After a drop the
/// task connects again with a growing pause.
async fn ws_waker(url: String, hook: Address, wake: Arc<Notify>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match subscribe(&url, hook).await {
            Ok(mut stream) => {
                use futures::StreamExt;
                backoff = Duration::from_secs(1);
                while stream.next().await.is_some() {
                    wake.notify_one();
                }
                warn!(url, "WebSocket subscription ended; HTTP polling continues");
            }
            Err(error) => warn!(url, %error, "WebSocket subscription failed"),
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_WS_BACKOFF);
    }
}

async fn subscribe(
    url: &str,
    hook: Address,
) -> Result<impl futures::Stream<Item = alloy_rpc_types::Log>> {
    let provider = ProviderBuilder::new()
        .connect_ws(WsConnect::new(url))
        .await?;
    let filter = Filter::new()
        .address(hook)
        .event_signature(IMerkleTreeHook::InsertedIntoTree::SIGNATURE_HASH);
    let subscription = provider.subscribe_logs(&filter).await?;
    // Keep the provider alive while the stream runs.
    Ok(futures::StreamExt::map(
        subscription.into_stream(),
        move |log| {
            let _ = &provider;
            log
        },
    ))
}

/// Waits `interval`, or less when the WebSocket task wakes the loop.
pub async fn wait_next(interval: Duration, wake: Option<&Notify>) {
    match wake {
        Some(wake) => {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                _ = wake.notified() => {}
            }
        }
        None => tokio::time::sleep(interval).await,
    }
}
