//! Follower sync driver.
//!
//! Pulls the upstream's finalized tip and walks the follower's marshal forward
//! to it, one height at a time, by `hint_finalized`-ing each height. The
//! marshal's gap-repair then fetches `Finalized { height }` through the
//! [`FollowResolver`](super::resolver), which verifies the certificate against
//! the epoch committee and hands the block to the executor.
//!
//! **Why the driver registers epochs ahead of hinting.** The marshal silently
//! drops a `hint_finalized` for a height whose epoch has no registered verifier
//! (see `marshal::core::mailbox::hint_finalized`). Each new epoch's committee is
//! only known from the boundary block that activates it. So before hinting any
//! height in epoch N, the driver fetches epoch N's first block from the upstream
//! and [`advance_from_block_extra_data`](super::CommitteeChain::advance_from_block_extra_data)s
//! the shared committee chain. This is the same registration the resolver does
//! on the fetch path; doing it here too keeps the *hint* from being dropped. The
//! marshal still re-verifies every certificate against the registered committee,
//! so registration is never a trust shortcut.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_consensus::BlockHeader as _;
use commonware_consensus::types::{Epoch, Epocher, FixedEpocher, Height};
use commonware_cryptography::bls12381;
use commonware_runtime::{Clock, Spawner};
use commonware_utils::vec::NonEmptyVec;
use tracing::{debug, info, warn};

use crate::follow::upstream::{FinalizedSource, TipSource};
use crate::follow::CommitteeChain;
use crate::marshal_types::MarshalMailbox;

/// How often to poll the upstream for a new finalized tip when caught up.
const TIP_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A stub target peer for `hint_finalized`. The follower has no real consensus
/// peers; the resolver ignores targets and serves from the upstream regardless,
/// but `hint_finalized` requires a non-empty target set.
fn stub_targets() -> NonEmptyVec<bls12381::PublicKey> {
    use commonware_cryptography::Signer as _;
    NonEmptyVec::new(bls12381::PrivateKey::from_seed(0).public_key())
}

/// Configuration for the follow driver.
pub(super) struct Config<F, T> {
    /// Marshal mailbox — receives `hint_finalized` for each height to pull.
    pub(super) marshal: MarshalMailbox,
    /// Shared committee chain — epochs are registered here before hinting.
    pub(super) chain: Arc<Mutex<CommitteeChain>>,
    /// Upstream finalized-block source — used to fetch each epoch's first block
    /// for committee registration.
    pub(super) upstream: F,
    /// Upstream tip discovery.
    pub(super) tip: T,
    /// Height→epoch strategy (shared with the marshal).
    pub(super) epocher: FixedEpocher,
    /// The anchor epoch (the first epoch the follower can verify); the driver
    /// begins pulling from this epoch's first height.
    pub(super) anchor_epoch: Epoch,
}

/// The follow driver actor.
pub(super) struct Driver<E, F, T> {
    context: E,
    config: Config<F, T>,
    /// Highest epoch whose committee the driver has ensured is registered.
    registered_epoch: Option<Epoch>,
    /// Next height to hint to the marshal.
    next_height: Height,
}

impl<E, F, T> Driver<E, F, T>
where
    E: Spawner + Clock + Clone + Send + Sync + 'static,
    F: FinalizedSource,
    T: TipSource,
{
    pub(super) fn new(context: E, config: Config<F, T>) -> Self {
        let first = config
            .epocher
            .first(config.anchor_epoch)
            .unwrap_or_else(Height::zero);
        Self {
            context,
            config,
            registered_epoch: None,
            next_height: first,
        }
    }

    pub(super) fn start(self) -> commonware_runtime::Handle<()> {
        let context = self.context.clone();
        context.spawn(move |_| self.run())
    }

    async fn run(mut self) {
        info!(
            anchor_epoch = self.config.anchor_epoch.get(),
            start_height = self.next_height.get(),
            "follow driver started"
        );
        loop {
            match self.config.tip.finalized_tip().await {
                Some(tip) => self.pull_to(tip).await,
                None => {
                    debug!("upstream tip unavailable; will retry");
                }
            }
            self.context.sleep(TIP_POLL_INTERVAL).await;
        }
    }

    /// Hint every height from `next_height` up to (and including) `tip`,
    /// ensuring each height's epoch committee is registered first.
    async fn pull_to(&mut self, tip: Height) {
        while self.next_height <= tip {
            let height = self.next_height;
            let Some(info) = self.config.epocher.containing(height) else {
                warn!(%height, "epocher cannot map height; stopping pull");
                return;
            };
            if !self.ensure_epoch_registered(info.epoch()).await {
                // Could not register this epoch's committee yet (upstream gap or
                // verification failure). Stop; retry on the next poll.
                return;
            }
            let targets = stub_targets();
            self.config.marshal.hint_finalized(height, targets);
            self.next_height = Height::new(height.get().saturating_add(1));
        }
    }

    /// Ensure epoch `epoch`'s committee verifier is registered in the shared
    /// chain. Returns `false` if it could not be registered (so the caller
    /// stops and retries later).
    async fn ensure_epoch_registered(&mut self, epoch: Epoch) -> bool {
        if self.registered_epoch.is_some_and(|r| r >= epoch) {
            return true;
        }
        // Already registered (e.g. anchor bootstrap or resolver fetch)?
        if self
            .config
            .chain
            .lock()
            .expect("committee chain mutex poisoned")
            .highest_registered()
            .is_some_and(|h| h >= epoch)
        {
            self.registered_epoch = Some(epoch);
            return true;
        }

        // Epoch N's committee rides epoch N's first finalized block.
        let Some(first) = self.config.epocher.first(epoch) else {
            warn!(epoch = epoch.get(), "epocher has no first height for epoch");
            return false;
        };
        let Some(certified) = self.config.upstream.get_finalization(first).await else {
            debug!(
                epoch = epoch.get(),
                first = first.get(),
                "upstream has no boundary block yet for epoch; will retry"
            );
            return false;
        };
        let extra = certified.block.header().extra_data().clone();
        match self
            .config
            .chain
            .lock()
            .expect("committee chain mutex poisoned")
            .advance_from_block_extra_data(extra.as_ref())
        {
            Ok(Some(registered)) => {
                debug!(
                    epoch = epoch.get(),
                    registered = registered.get(),
                    "registered epoch committee from boundary block"
                );
                self.registered_epoch = Some(epoch);
                true
            }
            Ok(None) => {
                // The first block of an epoch must carry that epoch's boundary
                // outcome. If it does not, the upstream is inconsistent with our
                // epoch model; refuse rather than silently skip.
                warn!(
                    epoch = epoch.get(),
                    first = first.get(),
                    "epoch's first block carried no boundary outcome; refusing to advance"
                );
                false
            }
            Err(error) => {
                warn!(
                    epoch = epoch.get(),
                    %error,
                    "failed to register epoch committee from boundary block"
                );
                false
            }
        }
    }
}
