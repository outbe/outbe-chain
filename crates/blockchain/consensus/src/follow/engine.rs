//! Follower engine assembly: marshal + resolver + driver + committee chain.
//!
//! The follower reuses the same marshal and executor as the validator path. The
//! marshal actor (with its two immutable archives) and the executor mailbox are
//! built by the caller — they need the reth node handle and the engine-crate
//! storage config, which `outbe-consensus` does not have — and handed in here.
//! This function then:
//!
//! 1. bootstraps the [`CommitteeChain`] at the anchor epoch (fetching the anchor
//!    epoch's first block from the upstream and registering its committee, which
//!    runs the anchor group-key trust check);
//! 2. builds the marshal's resolver handler pair and the [`FollowResolver`] over
//!    the upstream + local block sources;
//! 3. starts the marshal with the executor mailbox as its application reporter,
//!    a null broadcast, and the follow resolver;
//! 4. starts the [`Driver`] which walks the marshal forward to the upstream tip.
//!
//! It never returns under normal operation (the actors run forever); it returns
//! `Err` only if the marshal exits or the anchor bootstrap fails.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use commonware_consensus::marshal::resolver::handler;
use commonware_consensus::types::{Epoch, Epocher, FixedEpocher};
use commonware_runtime::{BufferPooler, Clock, Metrics, Spawner, Storage};
use eyre::{eyre, Result};
use rand::{CryptoRng, RngCore};
use tracing::info;

use crate::digest::Digest;
use crate::follow::driver::{self, Driver};
use crate::follow::resolver;
use crate::follow::upstream::{FinalizedSource, LocalBlockSource, TipSource};
use crate::follow::{stubs, CommitteeChain};
use crate::marshal_types::{MarshalActor, MarshalMailbox};

/// Inputs to [`run_follow_engine`].
pub struct FollowEngineConfig<E, F, L, T, R>
where
    E: Spawner
        + Clock
        + Metrics
        + BufferPooler
        + Storage
        + RngCore
        + CryptoRng
        + Send
        + Sync
        + 'static,
{
    /// The marshal actor (already initialized over the committee-chain scheme
    /// provider), ready to `start`.
    pub marshal_actor: MarshalActor<E>,
    /// The marshal mailbox (for the driver's `hint_finalized`).
    pub marshal_mailbox: MarshalMailbox,
    /// The executor mailbox, used as the marshal's application reporter. It must
    /// implement `Reporter<Activity = MarshalUpdate>` (the outbe executor does).
    pub executor_reporter: R,
    /// Upstream finalized-block source (the transport seam).
    pub upstream: F,
    /// Local execution-layer block source (for `Request::Block`).
    pub local: L,
    /// Upstream tip discovery.
    pub tip: T,
    /// Height→epoch strategy, shared with the marshal.
    pub epocher: FixedEpocher,
    /// The shared committee chain. Its `scheme_provider()` MUST be the same
    /// provider the `marshal_actor` was initialized with, so committee
    /// registrations are visible to the marshal's certificate verification.
    /// It is bootstrapped at the anchor epoch by this function.
    pub chain: Arc<Mutex<CommitteeChain>>,
    /// The trust anchor's start epoch (for the bootstrap + driver). Equal to
    /// `chain.anchor_epoch()`.
    pub anchor_epoch: Epoch,
    /// Mailbox capacity for the resolver handler.
    pub mailbox_size: NonZeroUsize,
}

/// Assemble and run the follower engine. Returns only on fatal exit.
pub async fn run_follow_engine<E, F, L, T, R>(
    context: E,
    config: FollowEngineConfig<E, F, L, T, R>,
) -> Result<()>
where
    E: Spawner
        + Clock
        + Metrics
        + BufferPooler
        + Storage
        + RngCore
        + CryptoRng
        + Send
        + Sync
        + 'static,
    F: FinalizedSource,
    L: LocalBlockSource,
    T: TipSource,
    R: commonware_consensus::Reporter<Activity = crate::marshal_types::MarshalUpdate>
        + Send
        + 'static,
{
    let FollowEngineConfig {
        marshal_actor,
        marshal_mailbox,
        executor_reporter,
        upstream,
        local,
        tip,
        epocher,
        chain,
        anchor_epoch,
        mailbox_size,
    } = config;

    // ── 1. Bootstrap the committee chain at the anchor epoch ────────────────
    bootstrap_anchor(&chain, &upstream, &epocher, anchor_epoch).await?;

    // ── 2. Build the marshal resolver handler pair + follow resolver ────────
    let (handler_receiver, handler): (handler::Receiver<Digest>, handler::Handler<Digest>) =
        handler::init(context.child("follow_resolver_handler"), mailbox_size);

    let (resolver_actor, follow_resolver) = resolver::init(
        context.child("follow_resolver"),
        handler,
        upstream.clone(),
        local,
        chain.clone(),
    );
    let _resolver_handle = resolver_actor.start();

    // ── 3. Null broadcast (the follower never disseminates) ─────────────────
    let broadcast = stubs::null_broadcast(context.child("follow_broadcast"), mailbox_size);

    // ── 4. Start the marshal: executor as application reporter, follow
    //        resolver for gap repair. ────────────────────────────────────────
    let marshal_handle = marshal_actor.start(
        executor_reporter,
        broadcast,
        (handler_receiver, follow_resolver),
    );

    // ── 5. Start the driver: walk the marshal forward to the upstream tip ───
    let driver = Driver::new(
        context.child("follow_driver"),
        driver::Config {
            marshal: marshal_mailbox,
            chain,
            upstream,
            tip,
            epocher,
            anchor_epoch,
        },
    );
    let _driver_handle = driver.start();

    info!("follower engine started; syncing from upstream");

    // The marshal is the critical subsystem: if it exits, the follower is done.
    marshal_handle
        .await
        .map_err(|e| eyre!("follower marshal exited: {e:?}"))?;
    Err(eyre!("follower marshal exited unexpectedly"))
}

/// Fetch the anchor epoch's first block from the upstream and register its
/// committee, running the anchor group-key trust check. The anchor epoch's
/// first block is `epocher.first(anchor_epoch)` and carries that epoch's
/// boundary outcome (for epoch 0 that is BLOCK 1, since genesis `extra_data` is
/// empty — but the epocher's `first(0)` resolves to the boundary-carrying block
/// the same way the live stack commits it).
async fn bootstrap_anchor<F>(
    chain: &Arc<Mutex<CommitteeChain>>,
    upstream: &F,
    epocher: &FixedEpocher,
    anchor_epoch: Epoch,
) -> Result<()>
where
    F: FinalizedSource,
{
    use alloy_consensus::BlockHeader as _;

    let first = epocher
        .first(anchor_epoch)
        .ok_or_else(|| eyre!("epocher has no first height for anchor epoch {anchor_epoch}"))?;

    // For epoch 0 the boundary outcome rides BLOCK 1, not genesis (genesis
    // extra_data is empty in outbe). `epocher.first(0)` is height 0 (genesis),
    // so for the anchor-0 case we look at height 1; for any other anchor epoch
    // the first height already carries the boundary outcome.
    let candidate = if anchor_epoch.get() == 0 && first.get() == 0 {
        commonware_consensus::types::Height::new(1)
    } else {
        first
    };

    let certified = upstream
        .get_finalization(candidate)
        .await
        .ok_or_else(|| {
            eyre!(
                "upstream did not return the anchor epoch's boundary block at height {candidate}; \
                 cannot bootstrap the committee chain"
            )
        })?;

    let extra = certified.block.header().extra_data().clone();
    let registered = chain
        .lock()
        .expect("committee chain mutex poisoned")
        .advance_from_block_extra_data(extra.as_ref())?
        .ok_or_else(|| {
            eyre!(
                "anchor epoch {anchor_epoch}'s first block at height {candidate} carried no \
                 boundary outcome; cannot establish the trust root"
            )
        })?;

    info!(
        anchor_epoch = anchor_epoch.get(),
        registered = registered.get(),
        height = candidate.get(),
        "committee chain anchored at trusted network identity"
    );
    Ok(())
}
