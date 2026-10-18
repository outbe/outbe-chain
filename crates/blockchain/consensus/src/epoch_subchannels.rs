//! Pre-registration of vote/cert/res sub-channels for a future
//! consensus epoch.
//!
//! The Outbe consensus stack runs a fresh `simplex::Engine` per epoch
//! and registers a per-epoch sub-channel on each of three Muxers
//! (vote, cert, resolver). Without pre-registration, sub-channel
//! registration on the *receiver side* happens only when the new
//! epoch's `'epoch_loop` iteration starts. That start can lag a faster
//! peer's broadcast for the same epoch. Because production uses
//! `Muxer::new(...)` (no `.with_backup()`), any vote/cert message
//! arriving on an unregistered sub-channel is **dropped silently**
//! and never replayed.
//!
//! This helper closes the cross-node race: at DKG completion (well
//! before any peer fires its activation handler), every honest node
//! pre-registers vote/cert/res sub-channels for the upcoming epoch.
//! By the time a fast peer broadcasts new-epoch traffic, every
//! receiver's Mux already has a route. The activation handler later
//! consumes the stash and hands the (already-receiving) sub-channels
//! to the new Engine.
//!
//! Both production (the `outbe-engine` `stack::epoch` module) and the
//! multi-node test harness (`test_harness.rs`) call `register_epoch_subchannels` and
//! `take_or_register_current` from this module. Tests therefore
//! exercise the same registration code path that production runs.
//!
//! Error path: any failure in this module is fail-fast through
//! `eyre::Result`. There is no silent fallback to lazy registration.
//! A Mux that cannot register a sub-channel is in a state we cannot
//! safely hide from a consensus runtime path.

use crate::errors::EpochSubchannelError;
use commonware_consensus::types::Epoch;
use commonware_p2p::{
    utils::mux::{Error as MuxError, MuxHandle, SubReceiver, SubSender},
    Receiver as P2pReceiver, Sender as P2pSender,
};
use commonware_runtime::Clock;
use eyre::{Result, WrapErr as _};
use std::time::Duration;

/// Pre-registered (or freshly-registered) vote / cert / resolver
/// Mux sub-channels for one consensus epoch. Held in a stash from
/// DKG completion until the activation handler consumes it.
///
/// Each `(SubSender, SubReceiver)` tuple keeps the Mux route alive:
/// dropping the `SubReceiver` triggers a `Control::Deregister`
/// message to the Mux, which removes the route. As long as the
/// stash is held, the new epoch's sub-channel keeps draining the
/// physical channel into its buffered SubReceiver mailbox.
pub struct EpochSubchannels<S, R>
where
    S: P2pSender,
    R: P2pReceiver<PublicKey = S::PublicKey>,
{
    pub epoch: Epoch,
    pub vote: (SubSender<S>, SubReceiver<R>),
    pub cert: (SubSender<S>, SubReceiver<R>),
    pub res: (SubSender<S>, SubReceiver<R>),
}

/// Register vote / cert / res sub-channels for `epoch` on the three
/// provided Mux handles. Used at DKG completion to defeat the
/// cross-node registration race.
///
/// Returns the three `(SubSender, SubReceiver)` tuples bundled into
/// an `EpochSubchannels`. Any underlying Mux error
/// (`AlreadyRegistered`, closed Mux) becomes a wrapped `eyre` error
/// that the function propagates.
pub async fn register_epoch_subchannels<S, R>(
    epoch: Epoch,
    vote_mux: &mut MuxHandle<S, R>,
    cert_mux: &mut MuxHandle<S, R>,
    res_mux: &mut MuxHandle<S, R>,
) -> Result<EpochSubchannels<S, R>>
where
    S: P2pSender,
    R: P2pReceiver<PublicKey = S::PublicKey>,
{
    try_register_epoch_subchannels(epoch, vote_mux, cert_mux, res_mux)
        .await
        .map_err(|(channel, error)| {
            EpochSubchannelError::Register {
                channel,
                epoch,
                reason: format!("{error}"),
            }
            .into()
        })
}

/// Timeout and cadence for waiting for the prior epoch receivers to deregister.
#[derive(Clone, Copy, Debug)]
pub struct SubchannelRetryPolicy {
    pub timeout: Duration,
    pub retry_interval: Duration,
}

/// Exclusive borrowed routes for one vote/cert/res registration attempt.
pub struct EpochMuxHandles<'a, S: P2pSender, R: P2pReceiver<PublicKey = S::PublicKey>> {
    pub vote: &'a mut MuxHandle<S, R>,
    pub cert: &'a mut MuxHandle<S, R>,
    pub res: &'a mut MuxHandle<S, R>,
}

/// Reacquire the three routes for an engine replacement in the same epoch.
///
/// Aborting the old Simplex root recursively aborts its actor descendants, but
/// awaiting that root does not join every descendant. Their `SubReceiver`s can
/// therefore deregister a few scheduler turns later. Retrying only
/// `AlreadyRegistered` closes that teardown race without permitting two live
/// receivers: a successful registration can happen only after the Mux has
/// processed the old receiver's `Deregister` control message. Any closed Mux or
/// bounded local timeout is fatal to the caller.
pub async fn reacquire_epoch_subchannels<S, R, C>(
    epoch: Epoch,
    clock: &C,
    policy: SubchannelRetryPolicy,
    muxes: EpochMuxHandles<'_, S, R>,
) -> Result<EpochSubchannels<S, R>>
where
    S: P2pSender,
    R: P2pReceiver<PublicKey = S::PublicKey>,
    C: Clock,
{
    let SubchannelRetryPolicy {
        timeout,
        retry_interval,
    } = policy;
    let EpochMuxHandles {
        vote: vote_mux,
        cert: cert_mux,
        res: res_mux,
    } = muxes;
    let deadline = clock
        .current()
        .checked_add(timeout)
        .ok_or(EpochSubchannelError::TimeoutOverflow)?;
    loop {
        match try_register_epoch_subchannels(epoch, vote_mux, cert_mux, res_mux).await {
            Ok(channels) => return Ok(channels),
            Err((channel, MuxError::AlreadyRegistered(_))) if clock.current() < deadline => {
                tracing::debug!(
                    %epoch,
                    channel,
                    "waiting for prior same-epoch Mux receiver to deregister"
                );
                clock.sleep(retry_interval).await;
            }
            Err((channel, MuxError::AlreadyRegistered(_))) => {
                return Err(EpochSubchannelError::ReacquireTimeout {
                    timeout,
                    channel,
                    epoch,
                }
                .into());
            }
            Err((channel, error)) => {
                return Err(EpochSubchannelError::Reacquire {
                    channel,
                    epoch,
                    reason: error.to_string(),
                }
                .into());
            }
        }
    }
}

async fn try_register_epoch_subchannels<S, R>(
    epoch: Epoch,
    vote_mux: &mut MuxHandle<S, R>,
    cert_mux: &mut MuxHandle<S, R>,
    res_mux: &mut MuxHandle<S, R>,
) -> std::result::Result<EpochSubchannels<S, R>, (&'static str, MuxError)>
where
    S: P2pSender,
    R: P2pReceiver<PublicKey = S::PublicKey>,
{
    let vote = vote_mux
        .register(epoch.get())
        .await
        .map_err(|error| ("vote", error))?;
    let cert = match cert_mux.register(epoch.get()).await {
        Ok(cert) => cert,
        Err(error) => {
            drop(vote);
            return Err(("cert", error));
        }
    };
    let res = match res_mux.register(epoch.get()).await {
        Ok(res) => res,
        Err(error) => {
            drop(cert);
            drop(vote);
            return Err(("res", error));
        }
    };
    Ok(EpochSubchannels {
        epoch,
        vote,
        cert,
        res,
    })
}

/// Consume the stash if it matches `epoch`, otherwise register
/// fresh.
///
/// This is the entry point at the top of each `'epoch_loop`
/// iteration. Three branches:
///
/// * `Some(stash)` with `stash.epoch == epoch`: the expected case
///   under the production fix. DKG completion pre-registered for
///   this epoch and the activation handler advanced into it.
///   Consume the stash.
/// * `Some(stash)` with `stash.epoch != epoch`: a state-machine
///   bug. Either DKG completion pre-registered for the wrong epoch,
///   or the epoch counter advanced unexpectedly. Silently
///   re-registering would mask the bug. Drop the stash (its
///   `SubReceiver`s' `Drop` deregisters the wrong-epoch routes from
///   the Muxes) and surface the error.
/// * `None`: genesis bootstrap or restart. There was no prior DKG
///   completion to pre-register from. Register fresh.
pub async fn take_or_register_current<S, R>(
    epoch: Epoch,
    stash: &mut Option<EpochSubchannels<S, R>>,
    vote_mux: &mut MuxHandle<S, R>,
    cert_mux: &mut MuxHandle<S, R>,
    res_mux: &mut MuxHandle<S, R>,
) -> Result<EpochSubchannels<S, R>>
where
    S: P2pSender,
    R: P2pReceiver<PublicKey = S::PublicKey>,
{
    match stash.take() {
        Some(s) if s.epoch == epoch => Ok(s),
        Some(s) => {
            let stash_epoch = s.epoch;
            drop(s);
            Err(EpochSubchannelError::StashEpochMismatch { stash_epoch, epoch }.into())
        }
        None => register_epoch_subchannels(epoch, vote_mux, cert_mux, res_mux)
            .await
            .wrap_err_with(|| {
                format!(
                    "lazy register of vote/cert/res subchannels for epoch {epoch} \
                     (no pre-registered stash)"
                )
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_cryptography::{bls12381, Signer as _};
    use commonware_p2p::{
        simulated::{Config as SimConfig, Network, Receiver as SimReceiver, Sender as SimSender},
        utils::mux::Muxer,
    };
    use commonware_runtime::{deterministic, Quota, Runner as _, Spawner as _, Supervisor as _};
    use commonware_utils::{NZUsize, NZU32};
    use std::num::NonZeroUsize;

    type TestMux = MuxHandle<
        SimSender<bls12381::PublicKey, deterministic::Context>,
        SimReceiver<bls12381::PublicKey>,
    >;

    async fn setup_test_muxes(ctx: &deterministic::Context) -> (TestMux, TestMux, TestMux) {
        let (network, oracle) = Network::new(
            ctx.child("network"),
            SimConfig {
                max_size: 64 * 1024,
                disconnect_on_block: true,
                max_peers_per_set: NonZeroUsize::new(1).unwrap(),
                tracked_peer_sets: NZUsize!(1),
            },
        );
        network.start();

        let pk = bls12381::PrivateKey::from_seed(1).public_key();
        let control = oracle.control(pk);
        let quota = Quota::per_second(NZU32!(1_000));
        let (votes_tx, votes_rx) = control.register(0, quota).await.unwrap();
        let (certs_tx, certs_rx) = control.register(1, quota).await.unwrap();
        let (res_tx, res_rx) = control.register(2, quota).await.unwrap();

        let (vote_muxer, vote_mux) = Muxer::new(ctx.child("vote_mux"), votes_tx, votes_rx, 16);
        vote_muxer.start();
        let (cert_muxer, cert_mux) = Muxer::new(ctx.child("cert_mux"), certs_tx, certs_rx, 16);
        cert_muxer.start();
        let (res_muxer, res_mux) = Muxer::new(ctx.child("res_mux"), res_tx, res_rx, 16);
        res_muxer.start();

        (vote_mux, cert_mux, res_mux)
    }

    #[test]
    fn take_or_register_current_handles_stash_hit_lazy_register_and_epoch_mismatch() {
        deterministic::Runner::default().start(|ctx| async move {
            let (mut vote_mux, mut cert_mux, mut res_mux) = setup_test_muxes(&ctx).await;

            // 1. Lazy registration when stash is None.
            let mut stash = None;
            let e1 = take_or_register_current(
                Epoch::new(1),
                &mut stash,
                &mut vote_mux,
                &mut cert_mux,
                &mut res_mux,
            )
            .await
            .expect("lazy registration must succeed");
            assert_eq!(e1.epoch, Epoch::new(1));
            assert!(stash.is_none());

            // 2. Stash hit consumes pre-registered subchannels without re-registering.
            let pre_e2 = register_epoch_subchannels(
                Epoch::new(2),
                &mut vote_mux,
                &mut cert_mux,
                &mut res_mux,
            )
            .await
            .expect("pre-register epoch 2");
            stash = Some(pre_e2);
            let e2 = take_or_register_current(
                Epoch::new(2),
                &mut stash,
                &mut vote_mux,
                &mut cert_mux,
                &mut res_mux,
            )
            .await
            .expect("stash hit must succeed");
            assert_eq!(e2.epoch, Epoch::new(2));
            assert!(stash.is_none());

            // 3. Stash epoch mismatch fails closed and drops the wrong-epoch stash.
            let pre_e3 = register_epoch_subchannels(
                Epoch::new(3),
                &mut vote_mux,
                &mut cert_mux,
                &mut res_mux,
            )
            .await
            .expect("pre-register epoch 3");
            stash = Some(pre_e3);
            let err = match take_or_register_current(
                Epoch::new(4),
                &mut stash,
                &mut vote_mux,
                &mut cert_mux,
                &mut res_mux,
            )
            .await
            {
                Ok(_) => panic!("mismatched stash epoch must fail closed"),
                Err(err) => err,
            };
            assert!(stash.is_none(), "mismatched stash must be consumed/dropped");
            assert!(matches!(
                err.downcast_ref::<EpochSubchannelError>(),
                Some(EpochSubchannelError::StashEpochMismatch {
                    stash_epoch,
                    epoch,
                }) if *stash_epoch == Epoch::new(3) && *epoch == Epoch::new(4)
            ));
        });
    }

    #[test]
    fn partial_registration_rolls_back_and_reacquire_retries_or_times_out() {
        deterministic::Runner::default().start(|ctx| async move {
            let (mut vote_mux, mut cert_mux, mut res_mux) = setup_test_muxes(&ctx).await;
            let epoch = Epoch::new(5);

            // Occupy only `res_mux` for epoch 5 so `vote` and `cert` succeed first,
            // then `res` fails with AlreadyRegistered and triggers rollback of `vote` and `cert`.
            let held_res = res_mux.register(epoch.get()).await.unwrap();

            let err =
                match register_epoch_subchannels(epoch, &mut vote_mux, &mut cert_mux, &mut res_mux)
                    .await
                {
                    Ok(_) => panic!("duplicate res registration must fail"),
                    Err(err) => err,
                };
            assert!(matches!(
                err.downcast_ref::<EpochSubchannelError>(),
                Some(EpochSubchannelError::Register {
                    channel: "res",
                    epoch: ep,
                    ..
                }) if *ep == epoch
            ));

            // While `held_res` remains alive, `reacquire_epoch_subchannels` must time out on "res"
            // (confirming `vote` and `cert` were rolled back on the failed attempt).
            let policy = SubchannelRetryPolicy {
                timeout: Duration::from_millis(20),
                retry_interval: Duration::from_millis(5),
            };
            let timeout_err = match reacquire_epoch_subchannels(
                epoch,
                &ctx,
                policy,
                EpochMuxHandles {
                    vote: &mut vote_mux,
                    cert: &mut cert_mux,
                    res: &mut res_mux,
                },
            )
            .await
            {
                Ok(_) => panic!("reacquire must time out while prior res receiver is held"),
                Err(err) => err,
            };
            assert!(matches!(
                timeout_err.downcast_ref::<EpochSubchannelError>(),
                Some(EpochSubchannelError::ReacquireTimeout {
                    channel: "res",
                    epoch: ep,
                    ..
                }) if *ep == epoch
            ));

            // Drop `held_res` asynchronously after 10ms; `reacquire_epoch_subchannels` must succeed.
            ctx.child("drop_held_res").spawn(move |clock| async move {
                clock.sleep(Duration::from_millis(10)).await;
                drop(held_res);
            });

            let reacquired = reacquire_epoch_subchannels(
                epoch,
                &ctx,
                SubchannelRetryPolicy {
                    timeout: Duration::from_millis(50),
                    retry_interval: Duration::from_millis(5),
                },
                EpochMuxHandles {
                    vote: &mut vote_mux,
                    cert: &mut cert_mux,
                    res: &mut res_mux,
                },
            )
            .await
            .expect("reacquire must succeed once prior receiver drops");
            assert_eq!(reacquired.epoch, epoch);
        });
    }
}
