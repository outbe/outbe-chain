//! Endpoint vote: URL dedup, consensus thresholds and the decision over the
//! local tree.
//!
//! Ported from hyperlane-monorepo `agents-v3.0.0`,
//! `rust/main/agents/validator/src/` (Apache-2.0, see `LICENSE-APACHE`):
//! `checkpoint_consensus.rs` (endpoint vote), `rpc.rs` (URL dedup),
//! `submit.rs` (`evaluate_checkpoint_batch`, the conflict check) and the RPC
//! path of `merkle_tree_hook_sync.rs`. Changes: alloy types, one log source per
//! tick, unverified leaves stay in memory, no RocksDB, a WebSocket event only
//! wakes the poll.

use std::collections::HashSet;

use alloy_primitives::B256;
use tracing::warn;

use super::SyncedLeaf;
use crate::config::RpcConsensus;

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

/// A hook `latestCheckpoint()` that one endpoint returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observed {
    pub index: u32,
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
    if let Some(reorg) = confirmed_reorg(observed, required, committed) {
        return reorg;
    }
    let Tally {
        mut matching,
        missing,
        rewind,
    } = tally(observed, required, committed, pending);
    if matching.len() < required {
        return if rewind {
            Vote::Rewind
        } else if matching.len() + missing >= required {
            Vote::WaitingForLeaves
        } else {
            Vote::WaitingForRpc
        };
    }
    // A matching later root authenticates every earlier leaf. The
    // required-th highest matching index is the verified boundary.
    matching.sort_unstable_by(|a, b| b.cmp(a));
    Vote::Verified(matching[required - 1])
}

/// Number of endpoints that returned the same checkpoint as `o`.
fn agreeing(observed: &[Option<Observed>], o: &Observed) -> usize {
    observed.iter().flatten().filter(|x| *x == o).count()
}

/// A different root at the committed index that `required` endpoints return.
fn confirmed_reorg(
    observed: &[Option<Observed>],
    required: usize,
    committed: Option<(u32, B256)>,
) -> Option<Vote> {
    let (index, local) = committed?;
    observed
        .iter()
        .flatten()
        .find(|o| o.index == index && o.root != local && agreeing(observed, o) >= required)
        .map(|o| Vote::Reorg {
            index,
            local,
            observed: o.root,
        })
}

/// Endpoint answers compared with the local leaves.
struct Tally {
    /// Indexes where the endpoint root equals the local root.
    matching: Vec<u32>,
    /// Endpoints ahead of the local leaves.
    missing: usize,
    /// `required` endpoints agree on a root that differs from a local leaf.
    rewind: bool,
}

fn tally(
    observed: &[Option<Observed>],
    required: usize,
    committed: Option<(u32, B256)>,
    pending: &[SyncedLeaf],
) -> Tally {
    let root_at = |index: u32| match committed {
        Some((c, root)) if c == index => Some(root),
        _ => pending.iter().find(|l| l.index == index).map(|l| l.root),
    };
    let mut tally = Tally {
        matching: Vec::new(),
        missing: 0,
        rewind: false,
    };
    // An endpoint behind the committed tree has nothing to say.
    let current = observed
        .iter()
        .flatten()
        .filter(|o| committed.is_none_or(|(c, _)| o.index >= c));
    for o in current {
        match root_at(o.index) {
            Some(root) if root == o.root => tally.matching.push(o.index),
            Some(_) => {
                warn!(
                    index = o.index,
                    "endpoint root differs from the local leaves"
                );
                tally.rewind |= agreeing(observed, o) >= required;
            }
            None => tally.missing += 1,
        }
    }
    tally
}

impl RpcConsensus {
    /// Agreeing endpoints that a pool of `endpoints` needs.
    pub fn required(self, endpoints: usize) -> usize {
        match self {
            Self::Majority => endpoints - endpoints / 3,
            Self::Quorum => endpoints.div_ceil(2),
            Self::Single => 1,
        }
    }
}
