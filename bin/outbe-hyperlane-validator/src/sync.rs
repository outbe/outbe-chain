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

use std::collections::HashSet;
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

use crate::abi::{IHyperlaneController, IMerkleTreeHook, IValidatorAnnounce};
use crate::client::eth_call;
use crate::config::{OriginConfig, ReorgPeriod, RpcConsensus};
use crate::errors::ValidatorError;
use crate::liveness::HYPERLANE_CONTROLLER_ADDRESS;
use crate::merkle::IncrementalMerkle;
use crate::store::Store;

/// Time limit of one endpoint read in the vote.
pub const RPC_TIMEOUT: Duration = Duration::from_secs(20);
/// The largest `eth_getLogs` block range. The Outbe RPC rejects larger ranges.
pub const MAX_LOG_RANGE: u64 = 100_000;
const MAX_WS_BACKOFF: Duration = Duration::from_secs(60);

/// Removes exact duplicate URLs and keeps the order. A duplicate endpoint
/// would vote twice.
pub fn dedup_urls(urls: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let deduped: Vec<String> = urls
        .iter()
        .filter(|url| seen.insert(url.as_str()))
        .cloned()
        .collect();
    if deduped.len() < urls.len() {
        warn!(
            configured = urls.len(),
            kept = deduped.len(),
            "duplicate rpc_urls removed: one endpoint has one vote"
        );
    }
    deduped
}

/// The block range of the next `eth_getLogs` request.
fn log_range(start: u64, end: u64, chunk: u64) -> (u64, u64) {
    (start, end.min(start.saturating_add(chunk - 1)))
}

/// True when the RPC rejected a log request because of its size.
fn is_range_error(message: &str) -> bool {
    let message = message.to_lowercase();
    ["range", "limit", "too many", "exceed"]
        .iter()
        .any(|word| message.contains(word))
}

/// A hook `latestCheckpoint()` that one endpoint returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observed {
    pub index: u32,
    pub root: B256,
}

/// A leaf with the tree root after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncedLeaf {
    pub index: u32,
    pub message_id: B256,
    pub block: u64,
    pub root: B256,
}

/// The result of one vote over the local tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vote {
    /// Enough endpoints agree with the local root at this index. Every leaf up
    /// to it is verified.
    Verified(u32),
    /// Endpoints are ahead of the local leaves. Read more logs.
    WaitingForLeaves,
    /// Not enough endpoints answered or agreed. Try again later.
    WaitingForRpc,
    /// Enough endpoints agree on a different root at an index that this
    /// validator already committed. Stop signing.
    Reorg {
        index: u32,
        local: B256,
        observed: B256,
    },
    /// Enough endpoints agree on a different root inside the unverified
    /// leaves. The log source was wrong: drop those leaves and read again.
    Rewind,
}

/// Decides what the endpoint answers mean for the local tree.
///
/// `committed` is the index and root of the last stored leaf. `pending` holds
/// the unverified leaves after it, in index order. Failed endpoints keep their
/// slot in `observed`, so a failure never lowers the threshold.
pub fn evaluate(
    observed: &[Option<Observed>],
    required: usize,
    committed: Option<(u32, B256)>,
    pending: &[SyncedLeaf],
) -> Vote {
    let agreeing = |o: &Observed| observed.iter().flatten().filter(|x| *x == o).count();
    if let Some((index, local)) = committed {
        for o in observed.iter().flatten() {
            if o.index == index && o.root != local && agreeing(o) >= required {
                return Vote::Reorg {
                    index,
                    local,
                    observed: o.root,
                };
            }
        }
    }
    let root_at = |index: u32| match committed {
        Some((c, root)) if c == index => Some(root),
        _ => pending.iter().find(|l| l.index == index).map(|l| l.root),
    };
    let mut matching = Vec::new();
    let mut missing = 0;
    let mut rewind = false;
    for o in observed.iter().flatten() {
        // An endpoint behind the committed tree has nothing to say.
        if committed.is_some_and(|(c, _)| o.index < c) {
            continue;
        }
        match root_at(o.index) {
            Some(root) if root == o.root => matching.push(o.index),
            Some(_) => {
                warn!(
                    index = o.index,
                    "endpoint root differs from the local leaves"
                );
                rewind |= agreeing(o) >= required;
            }
            None => missing += 1,
        }
    }
    if matching.len() < required {
        if rewind {
            return Vote::Rewind;
        }
        if matching.len() + missing >= required {
            return Vote::WaitingForLeaves;
        }
        return Vote::WaitingForRpc;
    }
    // A matching later root authenticates every earlier leaf. The
    // required-th highest matching index is the verified boundary.
    matching.sort_unstable_by(|a, b| b.cmp(a));
    Vote::Verified(matching[required - 1])
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
            return Err(ValidatorError::LeafGap {
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

/// One domain of the HyperlaneController.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControllerDomain {
    pub domain: u32,
    pub hook: Address,
    pub validator_announce: Address,
}

/// Reads the domains and their contracts from the HyperlaneController.
/// Returns `None` while `icaRouter()` is zero: the controller is not
/// initialized yet.
pub async fn controller_domains(outbe: &impl Provider) -> Result<Option<Vec<ControllerDomain>>> {
    let call =
        |data: Vec<u8>| eth_call(outbe, HYPERLANE_CONTROLLER_ADDRESS, data, BlockId::latest());
    let out = call(IHyperlaneController::icaRouterCall {}.abi_encode()).await?;
    if IHyperlaneController::icaRouterCall::abi_decode_returns(&out)? == Address::ZERO {
        return Ok(None);
    }
    let out = call(IHyperlaneController::domainsCall {}.abi_encode()).await?;
    let mut domains = Vec::new();
    for domain in IHyperlaneController::domainsCall::abi_decode_returns(&out)? {
        let out = call(IHyperlaneController::hookByDomainCall { domain }.abi_encode()).await?;
        let hook = IHyperlaneController::hookByDomainCall::abi_decode_returns(&out)?;
        let out = call(IHyperlaneController::validatorAnnounceByDomainCall { domain }.abi_encode())
            .await?;
        let validator_announce =
            IHyperlaneController::validatorAnnounceByDomainCall::abi_decode_returns(&out)?;
        domains.push(ControllerDomain {
            domain,
            hook,
            validator_announce,
        });
    }
    Ok(Some(domains))
}

/// Checks the contracts of a controller domain on its origin chain and
/// returns the origin.
pub async fn check_origin(d: &ControllerDomain, config: &OriginConfig) -> Result<Origin> {
    let (domain, hook, va) = (d.domain, d.hook, d.validator_announce);
    let provider = connect(&config.rpc_urls[0])?;
    let call = |to: Address, data: Vec<u8>| eth_call(&provider, to, data, BlockId::latest());
    let mismatch = |what: String| ValidatorError::OriginMismatch { domain, what };
    let out = call(va, IValidatorAnnounce::localDomainCall {}.abi_encode()).await?;
    let local = IValidatorAnnounce::localDomainCall::abi_decode_returns(&out)?;
    if local != domain {
        return Err(mismatch(format!("ValidatorAnnounce localDomain() is {local}")).into());
    }
    let out = call(va, IValidatorAnnounce::mailboxCall {}.abi_encode()).await?;
    let mailbox = IValidatorAnnounce::mailboxCall::abi_decode_returns(&out)?;
    let out = call(hook, IMerkleTreeHook::mailboxCall {}.abi_encode()).await?;
    let hook_mailbox = IMerkleTreeHook::mailboxCall::abi_decode_returns(&out)?;
    if hook_mailbox != mailbox {
        return Err(mismatch(format!(
            "hook mailbox {hook_mailbox} differs from ValidatorAnnounce mailbox {mailbox}"
        ))
        .into());
    }
    let start_block = match config.index_from {
        Some(block) => block,
        None => {
            let out = call(hook, IMerkleTreeHook::deployedBlockCall {}.abi_encode()).await?;
            let block = IMerkleTreeHook::deployedBlockCall::abi_decode_returns(&out)?;
            u64::try_from(block).map_err(|_| mismatch("deployedBlock() exceeds u64".into()))?
        }
    };
    Ok(Origin {
        domain,
        hook,
        validator_announce: va,
        mailbox,
        start_block,
    })
}

pub fn connect(url: &str) -> Result<DynProvider> {
    let url = url
        .parse()
        .with_context(|| format!("invalid RPC URL {url}"))?;
    Ok(ProviderBuilder::new().connect_http(url).erased())
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
            .ok_or(ValidatorError::NoFinalizedBlock)?
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
    /// The endpoint that gives finality and logs. It rotates after an error.
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
}

impl DomainSync {
    /// Rebuilds the tree from the stored leaves and continues after the
    /// stored cursor. After a store error, build a new `DomainSync`: the
    /// in-memory state may be ahead of the store.
    pub fn new(origin: Origin, config: &OriginConfig, store: &Store) -> Result<Self> {
        let mut committed = IncrementalMerkle::default();
        store.for_each_leaf(origin.domain, |_, leaf| committed.ingest(leaf.message_id))?;
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
        })
    }

    pub fn domain(&self) -> u32 {
        self.origin.domain
    }

    /// Reads new finalized leaves, runs the endpoint vote and returns the
    /// leaves that the vote verified.
    pub async fn tick(&mut self) -> Result<Tick> {
        if let Err(error) = self.read_leaves().await {
            self.source = (self.source + 1) % self.endpoints.len();
            return Err(error);
        }
        let observed = self.vote().await;
        let committed = self.committed.index().map(|i| (i, self.committed.root()));
        let required = self.consensus.required(self.endpoints.len());
        match evaluate(&observed, required, committed, &self.pending) {
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
                self.source = (self.source + 1) % self.endpoints.len();
                Ok(Tick::Idle)
            }
            vote => {
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
            let mut leaves = Vec::with_capacity(logs.len());
            for log in logs {
                let event = IMerkleTreeHook::InsertedIntoTree::decode_log_data(log.data())?;
                let block = log.block_number.ok_or(ValidatorError::LogWithoutBlock)?;
                leaves.push((event.index, event.messageId, block));
            }
            append(self.domain(), &mut self.tip, &mut self.pending, leaves)?;
            self.scanned = to;
            start = to + 1;
        }
        Ok(())
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::Leaf;

    fn b(n: u8) -> B256 {
        B256::repeat_byte(n)
    }

    fn pending(from: u32, count: u32) -> (IncrementalMerkle, Vec<SyncedLeaf>) {
        let mut tip = IncrementalMerkle::default();
        let mut leaves = Vec::new();
        let raw = (0..from + count).map(|i| (i, b(i as u8 + 1), 100 + u64::from(i)));
        append(1, &mut tip, &mut leaves, raw.collect()).unwrap();
        (tip, leaves.split_off(from as usize))
    }

    fn obs(index: u32, root: B256) -> Option<Observed> {
        Some(Observed { index, root })
    }

    #[test]
    fn dedup_keeps_order_and_one_vote_per_url() {
        let urls = ["a", "b", "a", "c", "b"].map(String::from);
        assert_eq!(dedup_urls(&urls), ["a", "b", "c"].map(String::from));
    }

    #[test]
    fn log_ranges_and_range_errors() {
        assert_eq!(log_range(1, 250_000, MAX_LOG_RANGE), (1, 100_000));
        assert_eq!(
            log_range(100_001, 150_000, MAX_LOG_RANGE),
            (100_001, 150_000)
        );
        assert_eq!(log_range(5, 5, 1), (5, 5));
        assert!(is_range_error("query exceeds max block range 100000"));
        assert!(is_range_error("Log response size exceeded"));
        assert!(is_range_error(
            "query returned more than 10000 results; limit"
        ));
        assert!(!is_range_error("connection refused"));
    }

    #[test]
    fn append_orders_skips_replays_and_rejects_gaps() {
        let mut tip = IncrementalMerkle::default();
        let mut leaves = Vec::new();
        append(1, &mut tip, &mut leaves, vec![(1, b(2), 10), (0, b(1), 10)]).unwrap();
        assert_eq!(leaves.iter().map(|l| l.index).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(leaves[1].root, tip.root());

        // A replay of known leaves changes nothing.
        append(1, &mut tip, &mut leaves, vec![(0, b(1), 10), (1, b(2), 10)]).unwrap();
        assert_eq!(leaves.len(), 2);

        // A gap leaves the state as it was.
        let before = tip.clone();
        assert!(append(1, &mut tip, &mut leaves, vec![(2, b(3), 11), (4, b(5), 12)]).is_err());
        assert_eq!(tip, before);
        assert_eq!(leaves.len(), 2);
    }

    #[test]
    fn vote_verifies_the_required_highest_matching_index() {
        let (_, leaves) = pending(0, 5);
        let at = |i: usize| obs(leaves[i].index, leaves[i].root);
        // 2 of 3 match: index 4 and 2. Required 2 → boundary 2.
        let vote = evaluate(&[at(4), at(2), None], 2, None, &leaves);
        assert_eq!(vote, Vote::Verified(2));
        // Single: one match is sufficient.
        assert_eq!(
            evaluate(&[None, at(3)], 1, None, &leaves),
            Vote::Verified(3)
        );
    }

    #[test]
    fn vote_waits_for_leaves_or_for_rpc() {
        let (_, leaves) = pending(0, 2);
        let ahead = obs(9, b(9));
        let at1 = obs(1, leaves[1].root);
        // One match and one endpoint ahead of the local leaves: read more logs.
        assert_eq!(
            evaluate(&[at1, ahead, None], 2, None, &leaves),
            Vote::WaitingForLeaves
        );
        // Two failed endpoints of three: do not shrink the pool.
        assert_eq!(
            evaluate(&[at1, None, None], 2, None, &leaves),
            Vote::WaitingForRpc
        );
    }

    #[test]
    fn vote_detects_a_reorg_of_committed_leaves() {
        let (tip, _) = pending(0, 3);
        let committed = Some((2, tip.root()));
        let other = obs(2, b(0xee));
        assert_eq!(
            evaluate(&[other, other, None], 2, committed, &[]),
            Vote::Reorg {
                index: 2,
                local: tip.root(),
                observed: b(0xee)
            }
        );
        // One endpoint is not enough to call a reorg.
        assert_eq!(
            evaluate(&[other, None, None], 2, committed, &[]),
            Vote::WaitingForRpc
        );
        // A stale endpoint below the committed tree is ignored.
        let stale = obs(1, b(0xee));
        let at2 = obs(2, tip.root());
        assert_eq!(
            evaluate(&[stale, at2, at2], 2, committed, &[]),
            Vote::Verified(2)
        );
    }

    #[test]
    fn vote_rewinds_wrong_pending_leaves() {
        let (_, leaves) = pending(3, 2);
        let committed = Some((2, leaves[0].root));
        let other = obs(4, b(0xee));
        assert_eq!(
            evaluate(&[other, other, None], 2, committed, &leaves),
            Vote::Rewind
        );
    }

    #[test]
    fn domain_sync_starts_from_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let leaves: Vec<(u32, Leaf)> = (0..3)
            .map(|i| {
                (
                    i,
                    Leaf {
                        message_id: b(i as u8 + 1),
                        block: 50,
                    },
                )
            })
            .collect();
        store.commit_batch(7, &leaves, &[], 60).unwrap();
        let origin = Origin {
            domain: 7,
            hook: Address::ZERO,
            validator_announce: Address::ZERO,
            mailbox: Address::ZERO,
            start_block: 40,
        };
        let config: OriginConfig = toml::from_str(
            "domain = 7\nrpc_urls = [\"http://127.0.0.1:1\", \"http://127.0.0.1:1\"]\nreorg_period = 0",
        )
        .unwrap();
        let sync = DomainSync::new(origin.clone(), &config, &store).unwrap();
        let (tip, _) = pending(0, 3);
        assert_eq!(sync.committed.root(), tip.root());
        assert_eq!(sync.cursor, 60);
        assert_eq!(sync.endpoints.len(), 1);

        let empty = tempfile::tempdir().unwrap();
        let store = Store::open(empty.path()).unwrap();
        let sync = DomainSync::new(origin, &config, &store).unwrap();
        assert_eq!(sync.cursor, 39);
        assert_eq!(sync.committed.count(), 0);
    }

    /// A JSON-RPC server for one hook: leaves at fixed blocks, head at block
    /// 100, `eth_getLogs` rejects ranges above 50 blocks, and
    /// `latestCheckpoint()` returns `root` (or the real root when `None`).
    pub(crate) async fn mock_rpc(
        leaves: Vec<(B256, u64)>,
        root: Arc<std::sync::Mutex<Option<B256>>>,
    ) -> String {
        use serde_json::{json, Value};

        let respond = move |req: &Value| -> Value {
            let id = req["id"].clone();
            let hex = |v: &Value| {
                u64::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
            };
            let result = match req["method"].as_str().unwrap() {
                "eth_blockNumber" => json!("0x64"),
                "eth_getLogs" => {
                    let (from, to) = (
                        hex(&req["params"][0]["fromBlock"]),
                        hex(&req["params"][0]["toBlock"]),
                    );
                    if to - from + 1 > 50 {
                        return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "query exceeds max block range 50"}});
                    }
                    let logs: Vec<Value> = leaves
                        .iter()
                        .enumerate()
                        .filter(|(_, (_, block))| (from..=to).contains(block))
                        .map(|(i, (id, block))| {
                            let data = IMerkleTreeHook::InsertedIntoTree {
                                messageId: *id,
                                index: i as u32,
                            }
                            .encode_data();
                            json!({
                                "address": Address::ZERO,
                                "topics": [IMerkleTreeHook::InsertedIntoTree::SIGNATURE_HASH],
                                "data": alloy_primitives::Bytes::from(data),
                                "blockNumber": format!("{block:#x}"),
                                "blockHash": B256::repeat_byte(1),
                                "transactionHash": B256::repeat_byte(2),
                                "transactionIndex": "0x0",
                                "logIndex": format!("{i:#x}"),
                                "removed": false,
                            })
                        })
                        .collect();
                    json!(logs)
                }
                "eth_call" => {
                    let mut tree = IncrementalMerkle::default();
                    leaves.iter().for_each(|(id, _)| tree.ingest(*id));
                    let ret = IMerkleTreeHook::latestCheckpointReturn {
                        root: root.lock().unwrap().unwrap_or(tree.root()),
                        index: tree.index().unwrap(),
                    };
                    json!(alloy_primitives::Bytes::from(
                        IMerkleTreeHook::latestCheckpointCall::abi_encode_returns(&ret)
                    ))
                }
                other => panic!("unexpected method {other}"),
            };
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        };
        serve_json_rpc(respond).await
    }

    /// A JSON-RPC server over HTTP. `respond` maps a request to a response.
    pub(crate) async fn serve_json_rpc(
        respond: impl Fn(&serde_json::Value) -> serde_json::Value + Send + Sync + 'static,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let respond = Arc::new(respond);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let respond = respond.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    loop {
                        let mut chunk = [0u8; 4096];
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let headers = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                        let len: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .map(|v| v.trim().parse().unwrap())
                            .unwrap_or(0);
                        if buf.len() < end + 4 + len {
                            continue;
                        }
                        let body: serde_json::Value =
                            serde_json::from_slice(&buf[end + 4..end + 4 + len]).unwrap();
                        buf.drain(..end + 4 + len);
                        let reply = respond(&body).to_string();
                        let head = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n", reply.len());
                        stream.write_all(head.as_bytes()).await.unwrap();
                        stream.write_all(reply.as_bytes()).await.unwrap();
                    }
                });
            }
        });
        url
    }

    #[tokio::test]
    async fn tick_reads_votes_and_detects_a_reorg() {
        let leaves = vec![(b(1), 10), (b(2), 60), (b(3), 90)];
        let fake_root = Arc::new(std::sync::Mutex::new(None));
        let a = mock_rpc(leaves.clone(), fake_root.clone()).await;
        let c = mock_rpc(leaves.clone(), fake_root.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let origin = Origin {
            domain: 5,
            hook: Address::ZERO,
            validator_announce: Address::ZERO,
            mailbox: Address::ZERO,
            start_block: 1,
        };
        let config: OriginConfig = toml::from_str(&format!(
            "domain = 5\nrpc_urls = [\"{a}\", \"{c}\"]\nreorg_period = 0"
        ))
        .unwrap();
        let mut sync = DomainSync::new(origin, &config, &store).unwrap();

        let Tick::Batch(batch) = sync.tick().await.unwrap() else {
            panic!("batch")
        };
        assert!(sync.chunk <= 50, "range errors shrink the chunk");
        assert_eq!(batch.leaves.len(), 3);
        assert_eq!(batch.cursor, 100);
        let mut tree = IncrementalMerkle::default();
        for (leaf, (id, block)) in batch.leaves.iter().zip(&leaves) {
            tree.ingest(*id);
            assert_eq!(
                (leaf.message_id, leaf.block, leaf.root),
                (*id, *block, tree.root())
            );
        }
        assert_eq!(sync.tick().await.unwrap(), Tick::Idle);

        *fake_root.lock().unwrap() = Some(b(0xee));
        assert_eq!(
            sync.tick().await.unwrap(),
            Tick::Reorg {
                index: 2,
                local: tree.root(),
                observed: b(0xee)
            }
        );
    }

    #[test]
    fn take_verified_stops_the_cursor_before_unverified_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let origin = Origin {
            domain: 1,
            hook: Address::ZERO,
            validator_announce: Address::ZERO,
            mailbox: Address::ZERO,
            start_block: 100,
        };
        let config: OriginConfig =
            toml::from_str("domain = 1\nrpc_urls = [\"http://127.0.0.1:1\"]\nreorg_period = 0")
                .unwrap();
        let mut sync = DomainSync::new(origin, &config, &store).unwrap();
        let (tip, leaves) = pending(0, 4);
        sync.tip = tip;
        sync.pending = leaves;
        sync.scanned = 200;
        let Tick::Batch(batch) = sync.take_verified(1) else {
            panic!("batch")
        };
        assert_eq!(
            batch.leaves.iter().map(|l| l.index).collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(batch.cursor, 101, "leaf 2 is in block 102");
        assert_eq!(sync.committed.root(), batch.leaves[1].root);
        let Tick::Batch(batch) = sync.take_verified(3) else {
            panic!("batch")
        };
        assert_eq!(batch.cursor, 200);
        assert_eq!(sync.take_verified(3), Tick::Idle);
    }
}
