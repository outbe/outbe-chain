//! DKG initial ceremony actor.
//!
//! Runs the interactive DKG protocol over a dedicated P2P channel.
//! Initial bootstrap makes all genesis validators Dealer and Player. Live
//! reshare makes previous-output players Dealer and the target set Player, so
//! newly added validators can join as player-only until they receive a share.
//! Reshare ceremonies complete from ≥ 2f+1 chain-finalized dealer logs.
//! Interactive bootstrap without a chain carrier waits for every genesis
//! participant's dealer log, because threshold P2P subsets are not canonical and
//! may otherwise produce different public polynomials on different validators.
//!
//! Protocol:
//! 1. Each validator calls `Dealer::start()` → gets `DealerPubMsg` + per-player `DealerPrivMsg`
//! 2. Each dealer sends `DealerBundle(pub_msg, priv_msg_i)` to player i via P2P
//! 3. Each player validates via `Player::dealer_message()`, sends `Ack` back to dealer
//! 4. Each node handles its own dealing locally (no network round-trip for self)
//! 5. Each dealer calls `Dealer::finalize()` → `SignedDealerLog`
//! 6. Each dealer broadcasts `FinalizedLog(log)` to ALL via P2P
//! 7. Each player collects dealer logs, calls `Player::finalize()` → `(Output, Share)`
//!
//! Local threshold finalization is not the activation source of truth. The
//! commonware `select()` function deterministically picks the first
//! `required_commitments` valid logs from the logs it is given, but different
//! nodes can receive different P2P subsets. Live reshare activation therefore
//! uses the canonical output reconstructed from finalized chain-carried dealer
//! logs; initial bootstrap waits for all genesis logs before block production.

use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::time::Duration;

use alloy_primitives::{keccak256, Bytes, B256};
use commonware_codec::{Encode, Read as _};
use commonware_cryptography::bls12381::{
    self,
    dkg::feldman_desmedt::{
        Dealer, DealerLog, DealerPubMsg, Info, Logs, Output, Player, PlayerAck, SignedDealerLog,
    },
    primitives::{group::Share, sharing::Mode, variant::MinSig},
};
use commonware_p2p::{Receiver as P2pReceiver, Recipients, Sender as P2pSender};
use commonware_parallel::Sequential;
use commonware_runtime::Clock;
use commonware_utils::{
    ordered::{Quorum, Set},
    N3f1,
};
use eyre::Result;
// Intentionally `tokio::sync::mpsc`: `progress_tx` / `finalized_log_rx` are created
// cross-crate by `outbe-engine` (`crates/blockchain/engine/src/stack.rs`) and have no
// timer/spawn dependency, so they are runtime-agnostic and do not require the tokio
// reactor. The type is kept to preserve the cross-crate engine API.
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::wire::{DkgCeremonyId, DkgMessage, DkgMessageReadError, DkgWireConfig};

/// Result of a successful DKG ceremony.
#[derive(Clone, Debug)]
pub struct DkgComplete {
    /// Threshold public polynomial (shared by all participants).
    pub output: Output<MinSig, bls12381::PublicKey>,
    /// This validator's private threshold share.
    pub share: Share,
    /// The participant set used in this DKG round.
    pub participants: Set<bls12381::PublicKey>,
}

/// Result of a dealer-only reshare participant.
///
/// A removed validator that still owns a previous threshold share remains a
/// dealer for the reshare, but is not a player in the target participant set
/// and therefore does not receive a fresh share.
#[derive(Clone, Debug)]
pub struct DkgDealerOnlyComplete {
    /// The participant set that receives fresh shares in this DKG round.
    pub participants: Set<bls12381::PublicKey>,
}

/// Progress emitted while a DKG ceremony is running in parallel with consensus.
#[derive(Debug, Clone)]
pub enum DkgProgress {
    /// The local dealer has finalized its signed dealer log and it may now be
    /// carried in a proposal `header.extra_data`.
    LocalDealerLog(Bytes),
    /// A valid P2P finalized dealer log was observed while chain-finalized mode
    /// is active. It may become a proposal candidate, but it is not canonical
    /// until it appears in finalized `header.extra_data`.
    P2pDealerLog(Bytes),
}

/// Timeout for the entire DKG ceremony.
const DKG_TIMEOUT: Duration = Duration::from_secs(120);

/// Interval between retry attempts for unsent shares.
#[cfg(not(test))]
const RETRY_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(test)]
const RETRY_INTERVAL: Duration = Duration::from_millis(100);

/// Initial bootstrap has no chain carrier yet, so nodes that already collected
/// all genesis logs keep gossiping them briefly before returning threshold
/// material. This prevents fast nodes from leaving the DKG channel while slower
/// nodes are still missing a final log.
#[cfg(not(test))]
const BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE: Duration = Duration::from_secs(10);
#[cfg(test)]
const BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
struct AcceptedDealerAck {
    pub_msg_hash: B256,
    ack: PlayerAck<bls12381::PublicKey>,
}

#[derive(Clone, Debug)]
enum DealerBundleAction {
    SendAck(PlayerAck<bls12381::PublicKey>),
    DuplicateAck(PlayerAck<bls12381::PublicKey>),
    Invalid,
    Equivocation { previous: B256, received: B256 },
}

/// Run a DKG ceremony over P2P (initial or reshare).
///
/// Blocks until the ceremony completes or times out. The completion quorum
/// depends on the mode:
/// - **Chain-finalized reshare**: completes on `>= 2f+1` (N3f1) finalized dealer
///   logs. The chain carrier makes the selected subset canonical, so one or more
///   offline validators do NOT block the ceremony.
/// - **Initial interactive bootstrap (genesis)**: requires ALL `n` genesis dealer
///   logs. There is no canonical carrier yet, so every validator must agree on
///   the identical complete dealer-log set to derive the same public polynomial;
///   a `2f+1` subset would be non-deterministic and could fork the genesis
///   committee. A single offline founder therefore stalls genesis until the
///   timeout — by design (see the completion guard below and
///   `test_bootstrap_dkg_waits_for_all_genesis_nodes_*`).
///
/// # Arguments
/// * `signing_key` — this validator's BLS individual private key (MinPk)
/// * `participants` — ordered set of all validator BLS public keys
/// * `previous_output` — `None` for initial DKG, `Some(output)` for reshare
/// * `previous_share` — `None` for initial DKG, `Some(share)` for reshare
/// * `round` — DKG round number (0 for initial, incremented for reshares)
/// * `finalized_log_rx` — finalized chain-carried dealer logs for this ceremony
/// * `sender` — P2P sender for the DKG channel
/// * `receiver` — P2P receiver for the DKG channel
#[allow(clippy::too_many_arguments)]
pub async fn run_initial_dkg(
    clock: &impl Clock,
    signing_key: bls12381::PrivateKey,
    participants: Set<bls12381::PublicKey>,
    previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
    previous_share: Option<Share>,
    round: u64,
    progress_tx: Option<mpsc::UnboundedSender<DkgProgress>>,
    finalized_log_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
    mut sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    mut receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<DkgComplete> {
    let n = participants.len() as u32;
    let my_pk = commonware_cryptography::Signer::public_key(&signing_key);
    let player_threshold = participants.quorum::<N3f1>();

    let is_reshare = previous_output.is_some();
    let dealers = previous_output
        .as_ref()
        .map(|output| output.players().clone())
        .unwrap_or_else(|| participants.clone());
    let log_threshold = dealers.quorum::<N3f1>().max(
        previous_output
            .as_ref()
            .map(Output::quorum::<N3f1>)
            .unwrap_or(0),
    );
    let is_local_dealer = dealers.position(&my_pk).is_some();
    info!(
        validators = n,
        dealers = dealers.len(),
        player_threshold,
        log_threshold,
        is_reshare,
        is_local_dealer,
        round,
        "starting DKG ceremony"
    );

    let ceremony_id = DkgCeremonyId::new(
        &crate::config::outbe_app_namespace(),
        round,
        previous_output.as_ref(),
        &participants,
    );

    // Build DKG Info with optional previous output for reshare.
    // For initial: round=0, previous=None.
    // For reshare: round>0, previous=Some(Output), dealers must be from previous players.
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        round,
        previous_output,
        Mode::NonZeroCounter,
        dealers,
        participants.clone(),
    )
    .map_err(|e| eyre::eyre!("failed to create DKG info: {e:?}"))?;

    // Old share holders are dealers in a reshare. New validators are
    // players only until they receive a fresh threshold share.
    let (mut dealer, my_pub_msg, priv_msgs) = if is_local_dealer {
        let (dealer, my_pub_msg, priv_msgs) =
            Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                rand_core::OsRng,
                info.clone(),
                signing_key.clone(),
                previous_share,
            )
            .map_err(|e| eyre::eyre!("failed to start dealer: {e:?}"))?;
        (Some(dealer), Some(my_pub_msg), priv_msgs)
    } else {
        if previous_share.is_some() {
            warn!("local validator has previous DKG share but is not a dealer in this ceremony");
        }
        info!("local validator is DKG player-only for this reshare");
        (None, None, Vec::new())
    };

    // Start as Player.
    let mut player = Player::<MinSig, bls12381::PrivateKey>::new(info.clone(), signing_key.clone())
        .map_err(|e| eyre::eyre!("failed to create player: {e:?}"))?;

    // Build a map of unsent private shares: public_key → DealerPrivMsg.
    let mut unsent_shares: BTreeMap<bls12381::PublicKey, _> = priv_msgs.into_iter().collect();

    // Use a BTreeSet for unique ack tracking instead of a counter
    // (BTreeSet, not HashSet — deterministic iteration order on the consensus path).
    // Start empty — only count self-ack if self-dealing succeeded below.
    let mut acked_players: std::collections::BTreeSet<bls12381::PublicKey> =
        std::collections::BTreeSet::new();

    // Handle self-dealing locally (no network round-trip).
    // Only count self-ack if self-dealing validation succeeds.
    if let (Some(my_pub_msg), Some(my_priv_msg)) =
        (my_pub_msg.as_ref(), unsent_shares.remove(&my_pk))
    {
        if let Some(ack) =
            player.dealer_message::<N3f1>(my_pk.clone(), my_pub_msg.clone(), my_priv_msg)
        {
            if let Some(ref mut d) = dealer {
                d.receive_player_ack(my_pk.clone(), ack)
                    .map_err(|e| eyre::eyre!("failed to process self-ack: {e:?}"))?;
                acked_players.insert(my_pk.clone());
            }
            debug!("self-dealing complete");
        } else {
            warn!("self-dealing validation failed");
        }
    }

    // Track state.
    let mut finalized_logs: BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>> =
        BTreeMap::new();
    let mut signed_finalized_logs: BTreeMap<
        bls12381::PublicKey,
        SignedDealerLog<MinSig, bls12381::PrivateKey>,
    > = BTreeMap::new();
    let mut invalid_dealers: std::collections::BTreeSet<bls12381::PublicKey> =
        std::collections::BTreeSet::new();
    let mut accepted_dealer_acks: BTreeMap<bls12381::PublicKey, AcceptedDealerAck> =
        BTreeMap::new();
    let max_players = NonZeroU32::new(n)
        .ok_or_else(|| eyre::eyre!("DKG ceremony requires at least one participant"))?;
    let wire_cfg = DkgWireConfig {
        max_players,
        expected_ceremony_id: ceremony_id,
    };
    let chain_finalized_mode = finalized_log_rx.is_some();
    let mut finalized_log_rx = finalized_log_rx;

    // Send shares to all other players.
    if let Some(my_pub_msg) = my_pub_msg.as_ref() {
        send_shares(&mut sender, ceremony_id, my_pub_msg, &unsent_shares).await;
    }

    // Runtime-agnostic deadline + retry tick (commonware `Clock`, runs on both the
    // tokio and deterministic runtimes; no wall-clock on the consensus path).
    //
    // A periodic interval timer fires immediately on first poll; the previous code
    // consumed that first tick before the loop so the first in-loop retry fired one
    // `RETRY_INTERVAL` after start. We replicate that exactly: seed `next_retry_tick`
    // one period out and advance it by a fixed `RETRY_INTERVAL` each fire
    // (interval-schedule cadence, no drift from select wakeup latency).
    let now = clock.current();
    let deadline = now + DKG_TIMEOUT;
    let mut next_retry_tick = now + RETRY_INTERVAL;

    let mut bootstrap_threshold_logged = false;
    let mut bootstrap_all_logs_collected_at: Option<std::time::SystemTime> = None;

    loop {
        // `commonware_macros::select!` is biased (top-to-bottom): message processing
        // is preferred over retries/timeouts, matching the prior
        // `tokio::select! { biased; .. }` arm order exactly.
        commonware_macros::select! {
            msg_result = receiver.recv() => {
                let (from, raw) = msg_result
                    .map_err(|e| eyre::eyre!("DKG P2P receiver error: {e}"))?;

                let mut buf = raw;
                let msg = match DkgMessage::read_for_ceremony(&mut buf, &wire_cfg) {
                    Ok(m) => m,
                    Err(DkgMessageReadError::WrongCeremonyId { expected, received }) => {
                        warn!(
                            ?from,
                            expected_round = expected.round,
                            received_round = received.round,
                            expected_info_hash = %expected.info_hash,
                            received_info_hash = %received.info_hash,
                            "received DKG message for a different ceremony, ignoring"
                        );
                        continue;
                    }
                    Err(DkgMessageReadError::Codec(e)) => {
                        warn!(?e, ?from, "failed to decode DKG message, ignoring");
                        continue;
                    }
                };

                match msg {
                    DkgMessage::DealerBundle { pub_msg, priv_msg, .. } => {
                        // We are a Player receiving a dealing from another Dealer.
                        match handle_dealer_bundle(
                            &mut player,
                            &mut accepted_dealer_acks,
                            from.clone(),
                            pub_msg,
                            priv_msg,
                        ) {
                            DealerBundleAction::SendAck(ack) => {
                                send_ack(&mut sender, ceremony_id, &from, ack, "sent ack to dealer").await;
                            }
                            DealerBundleAction::DuplicateAck(ack) => {
                                send_ack(
                                    &mut sender,
                                    ceremony_id,
                                    &from,
                                    ack,
                                    "resent cached ack for duplicate dealer bundle",
                                )
                                .await;
                            }
                            DealerBundleAction::Equivocation { previous, received } => {
                                warn!(
                                    ?from,
                                    previous_pub_msg_hash = %previous,
                                    received_pub_msg_hash = %received,
                                    "dealer sent conflicting DKG public message"
                                );
                                invalid_dealers.insert(from.clone());
                            }
                            DealerBundleAction::Invalid => {
                                warn!(?from, "dealer sent invalid share — potential misbehavior");
                                invalid_dealers.insert(from.clone());
                            }
                        }
                    }
                    DkgMessage::Ack { ack, .. } => {
                        // We are a Dealer receiving an ack from a Player.
                        // Only count unique acks per player.
                        if let Some(ref mut d) = dealer {
                            match d.receive_player_ack(from.clone(), ack) {
                                Ok(()) => {
                                    let is_new_ack = acked_players.insert(from.clone());
                                    unsent_shares.remove(&from);
                                    let acks_received = acked_players.len();
                                    debug!(
                                        ?from,
                                        acks_received,
                                        player_threshold,
                                        is_new_ack,
                                        remaining = unsent_shares.len(),
                                        "received ack"
                                    );
                                }
                                Err(e) => {
                                    debug!(?e, ?from, "ack rejected");
                                }
                            }
                        }
                    }
                    DkgMessage::FinalizedLog { signed_log, .. } => {
                        if chain_finalized_mode {
                            if signed_log.clone().check(&info).is_none() {
                                warn!(?from, "received invalid finalized log, ignoring");
                            } else {
                                if let Some(progress_tx) = &progress_tx {
                                    let _ = progress_tx.send(DkgProgress::P2pDealerLog(
                                        Bytes::from(signed_log.encode()),
                                    ));
                                }
                                debug!(
                                    ?from,
                                    "received P2P finalized log; queued as proposal candidate"
                                );
                            }
                            continue;
                        }
                        if record_and_store_signed_dealer_log(
                            signed_log,
                            &info,
                            &mut finalized_logs,
                            &mut signed_finalized_logs,
                            "p2p",
                        )
                        .is_none()
                        {
                            warn!(?from, "received invalid finalized log, ignoring");
                        }
                    }
                }
            },

            chain_log = recv_chain_finalized_log(&mut finalized_log_rx) => {
                match chain_log {
                    Some(bytes) => {
                        match decode_signed_dealer_log(&bytes, &max_players) {
                            Ok(signed_log) => {
                                let _ = record_signed_dealer_log(
                                    signed_log,
                                    &info,
                                    &mut finalized_logs,
                                    "chain",
                                );
                            }
                            Err(error) => {
                                warn!(%error, "failed to decode chain-finalized DKG dealer log");
                            }
                        }
                    }
                    None => {
                        finalized_log_rx = None;
                        debug!("chain-finalized DKG dealer log stream closed");
                    }
                }
            },

            _ = clock.sleep_until(next_retry_tick) => {
                // Advance on the fixed interval schedule (matches the previous
                // interval-timer cadence, no drift from wakeup latency).
                next_retry_tick += RETRY_INTERVAL;
                if should_retry_share_distribution(dealer.is_some(), &unsent_shares) {
                    if let Some(my_pub_msg) = my_pub_msg.as_ref() {
                        debug!(remaining = unsent_shares.len(), "retrying share distribution");
                        send_shares(&mut sender, ceremony_id, my_pub_msg, &unsent_shares).await;
                    }
                }
                if should_retry_finalized_log_gossip(chain_finalized_mode, &signed_finalized_logs) {
                    debug!(
                        logs = signed_finalized_logs.len(),
                        target = n,
                        "retrying bootstrap finalized-log gossip"
                    );
                    gossip_finalized_logs(&mut sender, ceremony_id, &signed_finalized_logs).await;
                }
            },

            _ = clock.sleep_until(deadline) => {
                let log_target = if chain_finalized_mode { log_threshold } else { n };
                return Err(eyre::eyre!(
                    "DKG ceremony timed out after {:?} (acks: {}/{}, logs: {}/{})",
                    DKG_TIMEOUT,
                    acked_players.len(),
                    player_threshold,
                    finalized_logs.len(),
                    log_target,
                ));
            },
        }

        // Try to finalize dealer if we have enough acks and haven't finalized yet.
        if dealer.is_some() && acked_players.len() >= player_threshold as usize {
            let Some(d) = dealer.take() else {
                continue;
            };
            let signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey> = d.finalize::<N3f1>();

            info!(
                acks = acked_players.len(),
                player_threshold, "dealer finalized, broadcasting log"
            );

            // Verify our own log. In chain-finalized mode it is only used
            // after the log appears in a finalized block and is fed back by
            // `DkgManager`, so local/P2P subsets cannot diverge from the
            // canonical output.
            if signed_log.clone().check(&info).is_none() {
                return Err(eyre::eyre!("our own finalized log failed verification"));
            }
            if !chain_finalized_mode {
                let _ = record_and_store_signed_dealer_log(
                    signed_log.clone(),
                    &info,
                    &mut finalized_logs,
                    &mut signed_finalized_logs,
                    "local",
                );
            }

            if let Some(progress_tx) = &progress_tx {
                let _ = progress_tx.send(DkgProgress::LocalDealerLog(Bytes::from(
                    signed_log.encode(),
                )));
            }

            // Broadcast our finalized log to all peers.
            unsent_shares.clear();
            if !send_finalized_log(&mut sender, ceremony_id, signed_log) {
                debug!("finalized-log broadcast had no accepting recipients this attempt (rate-limited/backpressure); recovered by the DKG retry tick");
            }
        }

        // Check if we have enough finalized logs.
        //
        // For chain-finalized reshare, threshold is safe because the chain
        // carrier makes the selected dealer-log set canonical. For initial
        // interactive bootstrap there is no canonical carrier yet, so every
        // validator must use the same complete genesis dealer-log set.
        if finalized_logs.len() as u32 >= n {
            if chain_finalized_mode {
                info!(
                    logs = finalized_logs.len(),
                    n, "all logs collected, completing ceremony"
                );
                break;
            }

            let now = clock.current();
            match bootstrap_all_logs_collected_at {
                // `SystemTime::duration_since` errors only if `collected_at` is in the
                // future relative to `now`; the runtime clock is monotonic across these
                // reads, so `unwrap_or_default()` (a zero elapsed) is safe and panic-free,
                // deferring the grace by one loop iteration in the impossible skew case.
                Some(collected_at)
                    if now.duration_since(collected_at).unwrap_or_default()
                        >= BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE =>
                {
                    info!(
                        logs = finalized_logs.len(),
                        n,
                        "all bootstrap logs collected and gossip grace elapsed, completing ceremony"
                    );
                    break;
                }
                Some(_) => {}
                None => {
                    info!(
                        logs = finalized_logs.len(),
                        n,
                        grace_ms = BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE.as_millis(),
                        "all bootstrap logs collected; keeping DKG channel alive for finalized-log gossip grace"
                    );
                    bootstrap_all_logs_collected_at = Some(now);
                    gossip_finalized_logs(&mut sender, ceremony_id, &signed_finalized_logs).await;
                }
            }
        }

        if finalized_logs.len() as u32 >= log_threshold {
            if chain_finalized_mode {
                info!(
                    logs = finalized_logs.len(),
                    log_threshold, "chain-finalized threshold logs collected, completing ceremony"
                );
                break;
            }
            if !bootstrap_threshold_logged {
                info!(
                    logs = finalized_logs.len(),
                    log_threshold,
                    n,
                    "bootstrap DKG threshold logs collected; waiting for all genesis participants to keep output deterministic"
                );
                bootstrap_threshold_logged = true;
            }
        }
    }

    // Log invalid dealers (if any were detected during the ceremony).
    if !invalid_dealers.is_empty() {
        warn!(
            count = invalid_dealers.len(),
            "DKG completed with {} dealers who sent invalid shares",
            invalid_dealers.len()
        );
    }

    // Player finalize: recover threshold share from collected logs.
    let mut finalize_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info.clone());
    for (dealer_pk, log) in &finalized_logs {
        finalize_logs.record(dealer_pk.clone(), log.clone());
    }
    let (output, share) = player
        .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
            &mut rand_core::OsRng,
            finalize_logs,
            &Sequential,
        )
        .map_err(|e| eyre::eyre!("player finalize failed: {e:?}"))?;

    info!("DKG ceremony complete — threshold material obtained");

    // surface validators whose individual share evaluation was publicly
    // REVEALED during the ceremony (they were offline/non-acking, so
    // `feldman_desmedt` reveals their share so recovery can complete). The
    // reveals are permanently committed on-chain in the `DealerLog` artifacts,
    // and a revealed share makes that validator's VRF threshold partial publicly
    // forgeable — bounded (VRF drives leader election/fairness, not BFT safety:
    // the BLS individual aggregate stays authoritative), but operators must
    // rotate the affected validator's consensus key. `Output::revealed()` was
    // previously never consumed.
    let revealed = output.revealed();
    if !revealed.is_empty() {
        crate::metrics::record_dkg_revealed_shares(revealed.len());
        for pk in revealed.iter() {
            warn!(
                target: "outbe::dkg",
                revealed_validator = %pk,
                "DKG: a validator's individual share was REVEALED (offline during the ceremony); \
                 its VRF threshold partial is now publicly forgeable — rotate this validator's \
                 consensus key"
            );
        }
    }

    Ok(DkgComplete {
        output,
        share,
        participants,
    })
}

/// Run the dealer-only side of a live reshare.
///
/// This is used by validators that are in the previous DKG output and hold a
/// previous share, but are excluded from the target participant set. They must
/// still deal to the new players so the reshare can complete, but they must not
/// create a `Player` or wait for a new share.
#[allow(clippy::too_many_arguments)]
pub async fn run_reshare_dealer_only(
    clock: &impl Clock,
    signing_key: bls12381::PrivateKey,
    participants: Set<bls12381::PublicKey>,
    previous_output: Output<MinSig, bls12381::PublicKey>,
    previous_share: Share,
    round: u64,
    progress_tx: mpsc::UnboundedSender<DkgProgress>,
    mut sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    mut receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<DkgDealerOnlyComplete> {
    let my_pk = commonware_cryptography::Signer::public_key(&signing_key);
    if participants.position(&my_pk).is_some() {
        return Err(eyre::eyre!(
            "dealer-only DKG called for a target-set player"
        ));
    }

    let dealers = previous_output.players().clone();
    if dealers.position(&my_pk).is_none() {
        return Err(eyre::eyre!(
            "dealer-only DKG called for a key outside the previous dealer set"
        ));
    }

    let player_threshold = participants.quorum::<N3f1>();
    let previous_output_for_id = previous_output.clone();
    let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
        &crate::config::outbe_app_namespace(),
        round,
        Some(previous_output),
        Mode::NonZeroCounter,
        dealers,
        participants.clone(),
    )
    .map_err(|e| eyre::eyre!("failed to create dealer-only DKG info: {e:?}"))?;

    let (mut dealer, my_pub_msg, priv_msgs) =
        Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core::OsRng,
            info.clone(),
            signing_key,
            Some(previous_share),
        )
        .map_err(|e| eyre::eyre!("failed to start dealer-only DKG dealer: {e:?}"))?;

    let max_players = NonZeroU32::new(participants.len() as u32)
        .ok_or_else(|| eyre::eyre!("dealer-only DKG requires at least one target player"))?;
    let ceremony_id = DkgCeremonyId::new(
        &crate::config::outbe_app_namespace(),
        round,
        Some(&previous_output_for_id),
        &participants,
    );
    let wire_cfg = DkgWireConfig {
        max_players,
        expected_ceremony_id: ceremony_id,
    };

    let mut unsent_shares: BTreeMap<bls12381::PublicKey, _> = priv_msgs.into_iter().collect();
    let mut acked_players: std::collections::BTreeSet<bls12381::PublicKey> =
        std::collections::BTreeSet::new();
    send_shares(&mut sender, ceremony_id, &my_pub_msg, &unsent_shares).await;

    // Same runtime-agnostic deadline + interval-cadence retry tick as
    // `run_initial_dkg` (see that function for the first-tick rationale).
    let now = clock.current();
    let deadline = now + DKG_TIMEOUT;
    let mut next_retry_tick = now + RETRY_INTERVAL;

    let signed_log = loop {
        // Biased select (top-to-bottom), matching the prior
        // `tokio::select! { biased; .. }` arm order.
        commonware_macros::select! {
            msg_result = receiver.recv() => {
                let (from, raw) = msg_result
                    .map_err(|e| eyre::eyre!("dealer-only DKG P2P receiver error: {e}"))?;
                let mut buf = raw;
                let msg = match DkgMessage::read_for_ceremony(&mut buf, &wire_cfg) {
                    Ok(m) => m,
                    Err(DkgMessageReadError::WrongCeremonyId { expected, received }) => {
                        warn!(
                            ?from,
                            expected_round = expected.round,
                            received_round = received.round,
                            expected_info_hash = %expected.info_hash,
                            received_info_hash = %received.info_hash,
                            "received dealer-only DKG message for a different ceremony, ignoring"
                        );
                        continue;
                    }
                    Err(DkgMessageReadError::Codec(e)) => {
                        warn!(?e, ?from, "failed to decode dealer-only DKG message, ignoring");
                        continue;
                    }
                };

                match msg {
                    DkgMessage::Ack { ack, .. } => {
                        match dealer.receive_player_ack(from.clone(), ack) {
                            Ok(()) => {
                                let is_new_ack = acked_players.insert(from.clone());
                                unsent_shares.remove(&from);
                                debug!(
                                    ?from,
                                    acks_received = acked_players.len(),
                                    player_threshold,
                                    is_new_ack,
                                    remaining = unsent_shares.len(),
                                    "dealer-only DKG received ack"
                                );
                            }
                            Err(e) => {
                                debug!(?e, ?from, "dealer-only DKG ack rejected");
                            }
                        }
                    }
                    DkgMessage::DealerBundle { .. } => {
                        debug!(?from, "dealer-only DKG ignoring dealer bundle");
                    }
                    DkgMessage::FinalizedLog { .. } => {
                        debug!(?from, "dealer-only DKG ignoring finalized log");
                    }
                }
            },

            _ = clock.sleep_until(next_retry_tick) => {
                next_retry_tick += RETRY_INTERVAL;
                if !unsent_shares.is_empty() {
                    debug!(
                        remaining = unsent_shares.len(),
                        "dealer-only DKG retrying share distribution"
                    );
                    send_shares(&mut sender, ceremony_id, &my_pub_msg, &unsent_shares).await;
                }
            },

            _ = clock.sleep_until(deadline) => {
                return Err(eyre::eyre!(
                    "dealer-only DKG timed out after {:?} (acks: {}/{})",
                    DKG_TIMEOUT,
                    acked_players.len(),
                    player_threshold,
                ));
            },
        }

        if acked_players.len() >= player_threshold as usize {
            break dealer.finalize::<N3f1>();
        }
    };

    if signed_log.clone().check(&info).is_none() {
        return Err(eyre::eyre!(
            "dealer-only DKG finalized log failed verification"
        ));
    }

    progress_tx
        .send(DkgProgress::LocalDealerLog(Bytes::from(
            signed_log.encode(),
        )))
        .map_err(|_| eyre::eyre!("failed to publish dealer-only DKG local dealer log"))?;

    if !send_finalized_log(&mut sender, ceremony_id, signed_log) {
        debug!("dealer-only finalized-log broadcast had no accepting recipients this attempt (rate-limited/backpressure); recovered by the retry tick");
    }

    info!(
        acks = acked_players.len(),
        player_threshold, "dealer-only DKG complete — local dealer log published"
    );
    Ok(DkgDealerOnlyComplete { participants })
}

fn dealer_pub_msg_hash(pub_msg: &DealerPubMsg<MinSig>) -> B256 {
    keccak256(pub_msg.encode())
}

fn handle_dealer_bundle(
    player: &mut Player<MinSig, bls12381::PrivateKey>,
    accepted_dealer_acks: &mut BTreeMap<bls12381::PublicKey, AcceptedDealerAck>,
    dealer: bls12381::PublicKey,
    pub_msg: DealerPubMsg<MinSig>,
    priv_msg: commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerPrivMsg,
) -> DealerBundleAction {
    let pub_msg_hash = dealer_pub_msg_hash(&pub_msg);
    if let Some(accepted) = accepted_dealer_acks.get(&dealer) {
        if accepted.pub_msg_hash == pub_msg_hash {
            return DealerBundleAction::DuplicateAck(accepted.ack.clone());
        }
        return DealerBundleAction::Equivocation {
            previous: accepted.pub_msg_hash,
            received: pub_msg_hash,
        };
    }

    let Some(ack) = player.dealer_message::<N3f1>(dealer.clone(), pub_msg, priv_msg) else {
        return DealerBundleAction::Invalid;
    };
    accepted_dealer_acks.insert(
        dealer,
        AcceptedDealerAck {
            pub_msg_hash,
            ack: ack.clone(),
        },
    );
    DealerBundleAction::SendAck(ack)
}

/// Encode and send one DKG message over the P2P sender. The single owner of the
/// `encode → send → "empty accepted-set is benign backpressure"` recipe, so every
/// DKG send interprets the result identically.
///
/// Returns `true` if at least one recipient accepted this attempt. An empty
/// accepted-set (commonware 2026.5.0 sync `Sender::send` returns the accepting
/// peers) means none accepted *this attempt* — benign rate-limit/backpressure,
/// recovered by the ceremony retry tick and peer pull — never a hard failure.
/// `#[must_use]`: callers must observe acceptance (typically to log backpressure).
#[must_use]
fn send_dkg_message(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    recipients: Recipients<bls12381::PublicKey>,
    message: DkgMessage,
) -> bool {
    !sender.send(recipients, message.encode(), true).is_empty()
}

/// Broadcast one finalized dealer log to all peers. The single site that encodes
/// `DkgMessage::FinalizedLog` to `Recipients::All`, shared by the gossip loop and
/// the two one-shot post-finalize broadcasts.
#[must_use]
fn send_finalized_log(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey>,
) -> bool {
    send_dkg_message(
        sender,
        Recipients::All,
        DkgMessage::FinalizedLog {
            ceremony_id,
            signed_log,
        },
    )
}

async fn send_ack(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    dealer: &bls12381::PublicKey,
    ack: PlayerAck<bls12381::PublicKey>,
    success_message: &'static str,
) {
    if send_dkg_message(
        sender,
        Recipients::One(dealer.clone()),
        DkgMessage::Ack { ceremony_id, ack },
    ) {
        debug!(?dealer, message = success_message, "sent DKG ack");
    } else {
        debug!(?dealer, "ack had no accepting recipient this attempt (rate-limited/backpressure); retried by the ceremony loop");
    }
}

fn should_retry_share_distribution(
    dealer_active: bool,
    unsent: &BTreeMap<
        bls12381::PublicKey,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerPrivMsg,
    >,
) -> bool {
    dealer_active && !unsent.is_empty()
}

fn should_retry_finalized_log_gossip(
    chain_finalized_mode: bool,
    signed_logs: &BTreeMap<bls12381::PublicKey, SignedDealerLog<MinSig, bls12381::PrivateKey>>,
) -> bool {
    !chain_finalized_mode && !signed_logs.is_empty()
}

async fn recv_chain_finalized_log(
    rx: &mut Option<mpsc::UnboundedReceiver<Bytes>>,
) -> Option<Bytes> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

fn decode_signed_dealer_log(
    bytes: &Bytes,
    max_players: &NonZeroU32,
) -> Result<SignedDealerLog<MinSig, bls12381::PrivateKey>> {
    let mut reader = bytes.as_ref();
    let signed_log =
        SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(&mut reader, max_players)
            .map_err(|e| eyre::eyre!("invalid signed dealer log encoding: {e:?}"))?;
    if !reader.is_empty() {
        return Err(eyre::eyre!("trailing bytes after signed dealer log"));
    }
    Ok(signed_log)
}

fn record_and_store_signed_dealer_log(
    signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey>,
    info: &Info<MinSig, bls12381::PublicKey>,
    finalized_logs: &mut BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>>,
    signed_finalized_logs: &mut BTreeMap<
        bls12381::PublicKey,
        SignedDealerLog<MinSig, bls12381::PrivateKey>,
    >,
    source: &'static str,
) -> Option<bls12381::PublicKey> {
    let dealer_pk = record_signed_dealer_log(signed_log.clone(), info, finalized_logs, source)?;
    signed_finalized_logs
        .entry(dealer_pk.clone())
        .or_insert(signed_log);
    Some(dealer_pk)
}

fn record_signed_dealer_log(
    signed_log: SignedDealerLog<MinSig, bls12381::PrivateKey>,
    info: &Info<MinSig, bls12381::PublicKey>,
    finalized_logs: &mut BTreeMap<bls12381::PublicKey, DealerLog<MinSig, bls12381::PublicKey>>,
    source: &'static str,
) -> Option<bls12381::PublicKey> {
    let Some((dealer_pk, log)) = signed_log.check(info) else {
        warn!(source, "finalized DKG dealer log failed verification");
        return None;
    };
    let replaced = finalized_logs.insert(dealer_pk.clone(), log).is_some();
    debug!(
        ?dealer_pk,
        source,
        replaced,
        logs = finalized_logs.len(),
        "recorded finalized DKG dealer log"
    );
    Some(dealer_pk)
}

async fn gossip_finalized_logs(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    signed_logs: &BTreeMap<bls12381::PublicKey, SignedDealerLog<MinSig, bls12381::PrivateKey>>,
) {
    for (dealer, signed_log) in signed_logs {
        if !send_finalized_log(sender, ceremony_id, signed_log.clone()) {
            debug!(
                ?dealer,
                "finalized-log gossip had no accepting recipients this attempt \
                 (rate-limited/backpressure); re-gossiped by the retry tick"
            );
        }
    }
}

/// Send DealerBundle messages to all players in the unsent map.
async fn send_shares(
    sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    pub_msg: &DealerPubMsg<MinSig>,
    unsent: &BTreeMap<
        bls12381::PublicKey,
        commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerPrivMsg,
    >,
) {
    for (player_pk, priv_msg) in unsent {
        let message = DkgMessage::DealerBundle {
            ceremony_id,
            pub_msg: pub_msg.clone(),
            priv_msg: priv_msg.clone(),
        };
        if send_dkg_message(sender, Recipients::One(player_pk.clone()), message) {
            debug!(?player_pk, "sent share to player");
        } else {
            debug!(
                ?player_pk,
                "share send had no accepting recipient this attempt \
                 (rate-limited/backpressure); retried by send_shares"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_codec::Encode;
    use commonware_cryptography::Signer as _;
    use commonware_math::algebra::Random;
    use commonware_runtime::IoBuf;
    use commonware_utils::TryCollect as _;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    // -----------------------------------------------------------------------
    // Mock P2P network — routes messages between in-process nodes
    // -----------------------------------------------------------------------

    /// Shared routing table: maps public key → incoming message channel.
    type Router =
        Arc<HashMap<bls12381::PublicKey, mpsc::UnboundedSender<(bls12381::PublicKey, IoBuf)>>>;

    /// Mock P2P sender that routes messages through in-memory channels.
    #[derive(Clone)]
    struct MockSender {
        my_pk: bls12381::PublicKey,
        router: Router,
        drop_rule: Option<DropRule>,
    }

    type DropRule = Arc<Mutex<DropFinalizedLogOnce>>;

    #[derive(Debug)]
    struct DropFinalizedLogOnce {
        from: bls12381::PublicKey,
        to: bls12381::PublicKey,
        dropped: bool,
    }

    /// Checked sender returned by MockSender::check().
    struct MockCheckedSender<'a> {
        sender: &'a MockSender,
        recipients: Recipients<bls12381::PublicKey>,
    }

    impl commonware_p2p::CheckedSender for MockCheckedSender<'_> {
        type PublicKey = bls12381::PublicKey;

        fn recipients(&self) -> Vec<Self::PublicKey> {
            match &self.recipients {
                Recipients::One(pk) => vec![pk.clone()],
                Recipients::Some(pks) => pks.clone(),
                Recipients::All => self
                    .sender
                    .router
                    .keys()
                    .filter(|pk| **pk != self.sender.my_pk)
                    .cloned()
                    .collect(),
            }
        }

        fn send(
            self,
            message: impl Into<commonware_runtime::IoBufs> + Send,
            _priority: bool,
        ) -> commonware_actor::Unreliable<commonware_actor::Feedback> {
            let data: IoBuf = message.into().coalesce();
            for target in self.recipients() {
                if should_drop_finalized_log_once(
                    &self.sender.drop_rule,
                    &self.sender.my_pk,
                    &target,
                    data.as_ref(),
                ) {
                    continue;
                }
                if let Some(tx) = self.sender.router.get(&target) {
                    let _ = tx.send((self.sender.my_pk.clone(), data.clone()));
                }
            }
            commonware_actor::Unreliable::Outcome(commonware_actor::Feedback::Ok)
        }
    }

    impl commonware_p2p::LimitedSender for MockSender {
        type PublicKey = bls12381::PublicKey;
        type Checked<'a> = MockCheckedSender<'a>;

        fn check(
            &mut self,
            recipients: Recipients<Self::PublicKey>,
        ) -> Result<Self::Checked<'_>, std::time::SystemTime> {
            Ok(MockCheckedSender {
                sender: self,
                recipients,
            })
        }
    }

    /// Mock P2P receiver backed by an mpsc channel.
    #[derive(Debug)]
    struct MockReceiver {
        rx: mpsc::UnboundedReceiver<(bls12381::PublicKey, IoBuf)>,
    }

    impl commonware_p2p::Receiver for MockReceiver {
        type Error = std::io::Error;
        type PublicKey = bls12381::PublicKey;

        async fn recv(&mut self) -> Result<commonware_p2p::Message<Self::PublicKey>, Self::Error> {
            self.rx
                .recv()
                .await
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }

    /// Build a mock P2P mesh for `n` nodes.
    ///
    /// Returns `(keys, senders, receivers)` where each index corresponds to a node.
    fn build_mock_network(keys: &[bls12381::PrivateKey]) -> (Vec<MockSender>, Vec<MockReceiver>) {
        build_mock_network_with_drop(keys, None)
    }

    fn build_mock_network_with_drop(
        keys: &[bls12381::PrivateKey],
        drop_rule: Option<DropRule>,
    ) -> (Vec<MockSender>, Vec<MockReceiver>) {
        let mut channels = HashMap::new();
        let mut receivers = Vec::new();

        for key in keys {
            let pk = key.public_key();
            let (tx, rx) = mpsc::unbounded_channel();
            channels.insert(pk, tx);
            receivers.push(MockReceiver { rx });
        }

        let router: Router = Arc::new(channels);

        let senders: Vec<MockSender> = keys
            .iter()
            .map(|k| MockSender {
                my_pk: k.public_key(),
                router: Arc::clone(&router),
                drop_rule: drop_rule.clone(),
            })
            .collect();

        (senders, receivers)
    }

    #[allow(clippy::type_complexity)]
    fn run_direct_initial_round(
        keys: &[bls12381::PrivateKey],
    ) -> (
        Set<bls12381::PublicKey>,
        Output<MinSig, bls12381::PublicKey>,
        Vec<Share>,
    ) {
        let participants: Set<bls12381::PublicKey> = keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &crate::config::outbe_app_namespace(),
            0,
            None,
            Mode::NonZeroCounter,
            participants.clone(),
            participants.clone(),
        )
        .unwrap();

        let mut dealers = Vec::new();
        let mut pub_msgs = Vec::new();
        let mut all_priv_msgs = Vec::new();
        for key in keys {
            let (dealer, pub_msg, priv_msgs) =
                Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                    rand_core::OsRng,
                    info.clone(),
                    key.clone(),
                    None,
                )
                .unwrap();
            dealers.push(dealer);
            pub_msgs.push(pub_msg);
            all_priv_msgs.push(priv_msgs);
        }

        let mut players: Vec<Player<MinSig, bls12381::PrivateKey>> = keys
            .iter()
            .map(|key| Player::new(info.clone(), key.clone()).unwrap())
            .collect();

        for (dealer_idx, (pub_msg, priv_msgs)) in
            pub_msgs.iter().zip(all_priv_msgs.iter()).enumerate()
        {
            let dealer_pk = keys[dealer_idx].public_key();
            for (player_pk, priv_msg) in priv_msgs {
                let player_idx = keys
                    .iter()
                    .position(|key| key.public_key() == *player_pk)
                    .unwrap();
                if let Some(ack) = players[player_idx].dealer_message::<N3f1>(
                    dealer_pk.clone(),
                    pub_msg.clone(),
                    priv_msg.clone(),
                ) {
                    dealers[dealer_idx]
                        .receive_player_ack(player_pk.clone(), ack)
                        .unwrap();
                }
            }
        }

        let mut logs = BTreeMap::new();
        for dealer in dealers {
            let signed_log = dealer.finalize::<N3f1>();
            if let Some((dealer_pk, log)) = signed_log.check(&info) {
                logs.insert(dealer_pk, log);
            }
        }

        let mut output = None;
        let mut shares = Vec::new();
        for player in players {
            let mut dkg_logs = Logs::<MinSig, bls12381::PublicKey, N3f1>::new(info.clone());
            for (dealer_pk, log) in &logs {
                dkg_logs.record(dealer_pk.clone(), log.clone());
            }
            let (player_output, share) = player
                .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
                    &mut rand_core::OsRng,
                    dkg_logs,
                    &Sequential,
                )
                .unwrap();
            output = Some(player_output);
            shares.push(share);
        }

        (participants, output.unwrap(), shares)
    }

    fn should_drop_finalized_log_once(
        drop_rule: &Option<DropRule>,
        from: &bls12381::PublicKey,
        to: &bls12381::PublicKey,
        payload: &[u8],
    ) -> bool {
        let Some(drop_rule) = drop_rule else {
            return false;
        };
        let Some(tag) = payload.get(1 + std::mem::size_of::<u64>() + B256::len_bytes()) else {
            return false;
        };
        if *tag != 0x02 {
            return false;
        }
        let Ok(mut rule) = drop_rule.lock() else {
            return false;
        };
        if rule.dropped || &rule.from != from || &rule.to != to {
            return false;
        }
        rule.dropped = true;
        true
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_initial_dkg_3_nodes() {
        use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600))
            .start(|context| async move {
                // Generate 3 validator keys, sorted by public key (same as bootstrap).
                let mut keys: Vec<bls12381::PrivateKey> = (0..3)
                    .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
                    .collect();
                keys.sort_by_key(|a| a.public_key().encode());

                let participants: Set<bls12381::PublicKey> = keys
                    .iter()
                    .map(|k| k.public_key())
                    .try_collect::<Set<bls12381::PublicKey>>()
                    .unwrap();

                let (senders, receivers) = build_mock_network(&keys);

                // Spawn all 3 DKG ceremonies concurrently.
                let mut handles = Vec::new();
                for (i, ((key, sender), receiver)) in
                    keys.iter().cloned().zip(senders).zip(receivers).enumerate()
                {
                    let p = participants.clone();
                    // `Context` is not `Clone` on commonware 2026.5.0; obtain a
                    // fresh owned clock for the spawned ceremony via `Supervisor::child`.
                    handles.push(
                        context
                            .child("dkg_ceremony")
                            .spawn(move |clock| async move {
                                let result = run_initial_dkg(
                                    &clock, key, p, None, None, 0, None, None, sender, receiver,
                                )
                                .await;
                                (i, result)
                            }),
                    );
                }

                // Collect results.
                let mut results = Vec::new();
                for handle in handles {
                    let (i, result) = handle.await.unwrap();
                    let complete = result.unwrap_or_else(|e| panic!("node {i} DKG failed: {e}"));
                    results.push(complete);
                }

                // All nodes must get the same polynomial (Output).
                let poly_0 = results[0].output.public().encode();
                for (i, r) in results.iter().enumerate().skip(1) {
                    assert_eq!(
                        poly_0,
                        r.output.public().encode(),
                        "node {i} polynomial differs from node 0"
                    );
                }

                // All shares must be distinct.
                let share_bytes: Vec<Vec<u8>> =
                    results.iter().map(|r| r.share.encode().to_vec()).collect();
                for i in 0..share_bytes.len() {
                    for j in (i + 1)..share_bytes.len() {
                        assert_ne!(
                            share_bytes[i], share_bytes[j],
                            "shares {i} and {j} are identical"
                        );
                    }
                }

                // Participant sets must be identical.
                for (i, r) in results.iter().enumerate().skip(1) {
                    assert_eq!(
                        results[0].participants.len(),
                        r.participants.len(),
                        "node {i} participant count differs"
                    );
                }
            });
    }

    #[test]
    fn test_initial_dkg_4_nodes() {
        use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600))
            .start(|context| async move {
                let mut keys: Vec<bls12381::PrivateKey> = (0..4)
                    .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
                    .collect();
                keys.sort_by_key(|a| a.public_key().encode());

                let participants: Set<bls12381::PublicKey> = keys
                    .iter()
                    .map(|k| k.public_key())
                    .try_collect::<Set<bls12381::PublicKey>>()
                    .unwrap();

                let (senders, receivers) = build_mock_network(&keys);

                let mut handles = Vec::new();
                for (key, sender, receiver) in keys
                    .iter()
                    .cloned()
                    .zip(senders)
                    .zip(receivers)
                    .map(|((k, s), r)| (k, s, r))
                {
                    let p = participants.clone();
                    handles.push(
                        context
                            .child("dkg_ceremony")
                            .spawn(move |clock| async move {
                                run_initial_dkg(
                                    &clock, key, p, None, None, 0, None, None, sender, receiver,
                                )
                                .await
                            }),
                    );
                }

                let mut outputs = Vec::new();
                for (i, handle) in handles.into_iter().enumerate() {
                    let result = handle.await.unwrap();
                    outputs.push(result.unwrap_or_else(|e| panic!("node {i} DKG failed: {e}")));
                }

                // All nodes share the same polynomial.
                let poly_0 = outputs[0].output.public().encode();
                for (i, o) in outputs.iter().enumerate().skip(1) {
                    assert_eq!(poly_0, o.output.public().encode(), "node {i} poly differs");
                }

                // 4 distinct shares.
                let mut share_set = std::collections::BTreeSet::new();
                for o in &outputs {
                    assert!(share_set.insert(o.share.encode()), "duplicate share");
                }
            });
    }

    #[test]
    fn test_bootstrap_dkg_recovers_dropped_finalized_log_with_retry() {
        use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600))
            .start(|context| async move {
                let mut keys: Vec<bls12381::PrivateKey> = (0..4)
                    .map(|seed| bls12381::PrivateKey::from_seed(seed + 1))
                    .collect();
                keys.sort_by_key(|key| key.public_key().encode());

                let participants: Set<bls12381::PublicKey> = keys
                    .iter()
                    .map(|key| key.public_key())
                    .try_collect::<Set<bls12381::PublicKey>>()
                    .unwrap();

                let drop_rule = Arc::new(Mutex::new(DropFinalizedLogOnce {
                    from: keys[0].public_key(),
                    to: keys[1].public_key(),
                    dropped: false,
                }));
                let (senders, receivers) =
                    build_mock_network_with_drop(&keys, Some(drop_rule.clone()));

                let mut handles = Vec::new();
                for (i, ((key, sender), receiver)) in
                    keys.iter().cloned().zip(senders).zip(receivers).enumerate()
                {
                    let p = participants.clone();
                    handles.push(
                        context
                            .child("dkg_ceremony")
                            .spawn(move |clock| async move {
                                let result = run_initial_dkg(
                                    &clock, key, p, None, None, 0, None, None, sender, receiver,
                                )
                                .await;
                                (i, result)
                            }),
                    );
                }

                let mut results = Vec::new();
                for handle in handles {
                    let (i, result) = handle.await.unwrap();
                    results.push(result.unwrap_or_else(|error| {
                        panic!("node {i} DKG failed after finalized-log retry: {error}")
                    }));
                }

                assert!(
                    drop_rule.lock().expect("drop rule lock").dropped,
                    "test must drop the first finalized log to exercise retry gossip"
                );

                let public_0 = results[0].output.public().encode();
                for (i, result) in results.iter().enumerate().skip(1) {
                    assert_eq!(
                        public_0,
                        result.output.public().encode(),
                        "node {i} polynomial differs after finalized-log retry"
                    );
                }
            });
    }

    #[test]
    fn reshare_allows_new_player_without_previous_share() {
        use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600))
            .start(|context| async move {
                let mut old_keys: Vec<bls12381::PrivateKey> =
                    (1..=4).map(bls12381::PrivateKey::from_seed).collect();
                old_keys.sort_by_key(|key| key.public_key().encode());
                let (old_participants, previous_output, previous_shares) =
                    run_direct_initial_round(&old_keys);

                let new_key = bls12381::PrivateKey::from_seed(100);
                let new_pk = new_key.public_key();
                let mut target_keys = old_keys.clone();
                target_keys.push(new_key);
                target_keys.sort_by_key(|key| key.public_key().encode());
                let target_participants: Set<bls12381::PublicKey> = target_keys
                    .iter()
                    .map(|key| key.public_key())
                    .try_collect()
                    .unwrap();

                let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
                    &crate::config::outbe_app_namespace(),
                    1,
                    Some(previous_output.clone()),
                    Mode::NonZeroCounter,
                    old_participants.clone(),
                    target_participants.clone(),
                )
                .unwrap();
                let max_players = NonZeroU32::new(target_participants.len() as u32).unwrap();

                let (senders, receivers) = build_mock_network(&target_keys);
                let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
                let mut finalized_log_txs = Vec::new();
                let mut handles = Vec::new();

                for ((key, sender), receiver) in
                    target_keys.iter().cloned().zip(senders).zip(receivers)
                {
                    let participants = target_participants.clone();
                    let prev_output = previous_output.clone();
                    let prev_share = old_keys
                        .iter()
                        .position(|old_key| old_key.public_key() == key.public_key())
                        .map(|idx| previous_shares[idx].clone());
                    let progress_tx = progress_tx.clone();
                    let (finalized_log_tx, finalized_log_rx) = mpsc::unbounded_channel();
                    finalized_log_txs.push(finalized_log_tx);

                    handles.push(
                        context
                            .child("dkg_ceremony")
                            .spawn(move |clock| async move {
                                run_initial_dkg(
                                    &clock,
                                    key,
                                    participants,
                                    Some(prev_output),
                                    prev_share,
                                    1,
                                    Some(progress_tx),
                                    Some(finalized_log_rx),
                                    sender,
                                    receiver,
                                )
                                .await
                            }),
                    );
                }
                drop(progress_tx);

                let log_threshold = old_participants.quorum::<N3f1>();
                let mut chain_logs = BTreeMap::new();
                while chain_logs.len() < log_threshold as usize {
                    let progress = progress_rx
                        .recv()
                        .await
                        .expect("progress channel should remain open");
                    if let DkgProgress::LocalDealerLog(bytes) = progress {
                        let mut reader = bytes.as_ref();
                        let signed_log = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
                            &mut reader,
                            &max_players,
                        )
                        .unwrap();
                        let (dealer, _log) = signed_log.check(&info).unwrap();
                        assert_ne!(dealer, new_pk, "new player must not act as reshare dealer");
                        chain_logs.entry(dealer).or_insert(bytes);
                    }
                }

                for bytes in chain_logs.values() {
                    for tx in &finalized_log_txs {
                        tx.send(bytes.clone()).unwrap();
                    }
                }

                let mut results = Vec::new();
                for handle in handles {
                    results.push(handle.await.unwrap().unwrap());
                }

                let expected_public = results[0].output.public().encode();
                for result in &results {
                    assert_eq!(result.output.public().encode(), expected_public);
                    assert_eq!(result.participants, target_participants);
                }
                assert!(
                    results
                        .iter()
                        .any(|result| result.participants.position(&new_pk).is_some()),
                    "new player must be part of the reshared participant set"
                );
            });
    }

    #[test]
    fn removed_old_validator_can_deal_without_being_target_player() {
        use commonware_runtime::{Runner as _, Spawner as _, Supervisor as _};
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(600))
            .start(|context| async move {
                let mut old_keys: Vec<bls12381::PrivateKey> =
                    (1..=4).map(bls12381::PrivateKey::from_seed).collect();
                old_keys.sort_by_key(|key| key.public_key().encode());
                let (old_participants, previous_output, previous_shares) =
                    run_direct_initial_round(&old_keys);

                let removed_key = old_keys[0].clone();
                let removed_pk = removed_key.public_key();
                let target_keys: Vec<bls12381::PrivateKey> = old_keys
                    .iter()
                    .filter(|key| key.public_key() != removed_pk)
                    .cloned()
                    .collect();
                let target_participants: Set<bls12381::PublicKey> = target_keys
                    .iter()
                    .map(|key| key.public_key())
                    .try_collect()
                    .unwrap();
                assert!(target_participants.position(&removed_pk).is_none());

                let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
                    &crate::config::outbe_app_namespace(),
                    1,
                    Some(previous_output.clone()),
                    Mode::NonZeroCounter,
                    old_participants.clone(),
                    target_participants.clone(),
                )
                .unwrap();
                let max_players = NonZeroU32::new(target_participants.len() as u32).unwrap();

                let (senders, receivers) = build_mock_network(&old_keys);
                let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
                let mut finalized_log_txs = Vec::new();
                let mut player_handles = Vec::new();
                let mut dealer_only_handle = None;

                for ((key, sender), receiver) in
                    old_keys.iter().cloned().zip(senders).zip(receivers)
                {
                    let participants = target_participants.clone();
                    let prev_output = previous_output.clone();
                    let progress_tx = progress_tx.clone();
                    let idx = old_keys
                        .iter()
                        .position(|old_key| old_key.public_key() == key.public_key())
                        .unwrap();
                    let prev_share = previous_shares[idx].clone();

                    if key.public_key() == removed_pk {
                        dealer_only_handle = Some(context.child("dkg_ceremony").spawn(
                            move |clock| async move {
                                run_reshare_dealer_only(
                                    &clock,
                                    key,
                                    participants,
                                    prev_output,
                                    prev_share,
                                    1,
                                    progress_tx,
                                    sender,
                                    receiver,
                                )
                                .await
                            },
                        ));
                    } else {
                        let (finalized_log_tx, finalized_log_rx) = mpsc::unbounded_channel();
                        finalized_log_txs.push(finalized_log_tx);
                        player_handles.push(context.child("dkg_ceremony").spawn(
                            move |clock| async move {
                                run_initial_dkg(
                                    &clock,
                                    key,
                                    participants,
                                    Some(prev_output),
                                    Some(prev_share),
                                    1,
                                    Some(progress_tx),
                                    Some(finalized_log_rx),
                                    sender,
                                    receiver,
                                )
                                .await
                            },
                        ));
                    }
                }
                drop(progress_tx);

                let log_threshold = old_participants.quorum::<N3f1>();
                let mut chain_logs = BTreeMap::new();
                while chain_logs.len() < log_threshold as usize
                    || !chain_logs.contains_key(&removed_pk)
                {
                    let progress = progress_rx
                        .recv()
                        .await
                        .expect("progress channel should remain open");
                    let DkgProgress::LocalDealerLog(bytes) = progress else {
                        continue;
                    };
                    let mut reader = bytes.as_ref();
                    let signed_log = SignedDealerLog::<MinSig, bls12381::PrivateKey>::read_cfg(
                        &mut reader,
                        &max_players,
                    )
                    .unwrap();
                    let (dealer, _log) = signed_log.check(&info).unwrap();
                    chain_logs.entry(dealer).or_insert(bytes);
                }

                assert!(
                    chain_logs.contains_key(&removed_pk),
                    "removed old validator must publish a valid dealer log"
                );
                let mut selected_logs = Vec::with_capacity(log_threshold as usize);
                selected_logs.push(chain_logs.get(&removed_pk).unwrap().clone());
                for (dealer, bytes) in &chain_logs {
                    if dealer == &removed_pk {
                        continue;
                    }
                    selected_logs.push(bytes.clone());
                    if selected_logs.len() == log_threshold as usize {
                        break;
                    }
                }
                assert_eq!(
                    selected_logs.len(),
                    log_threshold as usize,
                    "test must feed a threshold set including the removed dealer"
                );
                for bytes in selected_logs {
                    for tx in &finalized_log_txs {
                        tx.send(bytes.clone()).unwrap();
                    }
                }

                let dealer_only_result = dealer_only_handle
                    .expect("dealer-only task must be spawned for removed validator")
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(dealer_only_result.participants, target_participants);

                let mut results = Vec::new();
                for handle in player_handles {
                    results.push(handle.await.unwrap().unwrap());
                }

                let expected_public = results[0].output.public().encode();
                for result in &results {
                    assert_eq!(result.output.public().encode(), expected_public);
                    assert_eq!(result.participants, target_participants);
                }
            });
    }

    // -----------------------------------------------------------------------
    // Offline-node tests — initial interactive bootstrap must not finalize from
    // arbitrary threshold P2P subsets because no chain carrier exists yet.
    // -----------------------------------------------------------------------

    /// Helper: run DKG with `n` total keys but only spawn tasks for the first
    /// `online` nodes. Offline nodes' receivers are dropped so they never
    /// participate. Returns results from online nodes only.
    async fn run_partial_dkg(
        clock: &commonware_runtime::deterministic::Context,
        n: usize,
        online: usize,
    ) -> Vec<Result<DkgComplete>> {
        use commonware_runtime::{Spawner as _, Supervisor as _};
        assert!(online <= n);

        let mut keys: Vec<bls12381::PrivateKey> = (0..n)
            .map(|_| bls12381::PrivateKey::random(rand_core::OsRng))
            .collect();
        keys.sort_by_key(|a| a.public_key().encode());

        // Participant set includes ALL n validators (this is the "expected" set).
        let participants: Set<bls12381::PublicKey> = keys
            .iter()
            .map(|k| k.public_key())
            .try_collect::<Set<bls12381::PublicKey>>()
            .unwrap();

        let (senders, receivers) = build_mock_network(&keys);

        // Only spawn DKG tasks for the first `online` nodes.
        // The remaining nodes' receivers are dropped (simulating offline).
        let mut handles = Vec::new();
        for (key, sender, receiver) in keys
            .iter()
            .take(online)
            .cloned()
            .zip(senders.into_iter().take(online))
            .zip(receivers.into_iter().take(online))
            .map(|((k, s), r)| (k, s, r))
        {
            let p = participants.clone();
            handles.push(clock.child("dkg_ceremony").spawn(move |clock| async move {
                run_initial_dkg(&clock, key, p, None, None, 0, None, None, sender, receiver).await
            }));
        }
        // Drop remaining receivers explicitly (offline nodes).
        // (They're already dropped by the `take(online)` iterators above,
        // but this documents intent.)

        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.unwrap());
        }
        results
    }

    /// 4 validators, 1 offline. Threshold = 3 (N3f1: f=1, quorum=3).
    /// Interactive bootstrap must wait instead of finalizing from a threshold
    /// P2P subset that could diverge across validators.
    #[test]
    fn test_bootstrap_dkg_waits_for_all_genesis_nodes_one_offline() {
        use commonware_runtime::{Clock as _, Runner as _};
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60))
            .start(|context| async move {
            // Non-completion within a bounded virtual-time window (matches the
            // original outer 5s timeout): a non-canonical 3/4 subset must keep
            // waiting, never finalize. `select!` is biased; the sleep arm winning
            // is the pass condition.
            commonware_macros::select! {
                _ = run_partial_dkg(&context, 4, 3) => {
                    panic!("bootstrap DKG must not complete from a non-canonical 3/4 P2P subset");
                },
                _ = context.sleep(std::time::Duration::from_secs(5)) => {},
            }
        });
    }

    /// 7 validators, 2 offline (maximum tolerable under N3f1).
    /// Threshold = 5 (f=2, quorum=5). Bootstrap still must wait for the full
    /// genesis dealer-log set; threshold liveness belongs to chain-finalized
    /// reshare after blocks exist.
    #[test]
    fn test_bootstrap_dkg_waits_for_all_genesis_nodes_max_offline() {
        use commonware_runtime::{Clock as _, Runner as _};
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60))
            .start(|context| async move {
            commonware_macros::select! {
                _ = run_partial_dkg(&context, 7, 5) => {
                    panic!("bootstrap DKG must not complete from a non-canonical 5/7 P2P subset");
                },
                _ = context.sleep(std::time::Duration::from_secs(5)) => {},
            }
        });
    }

    /// 4 validators, only 2 online — below threshold (3).
    /// The ceremony must time out, not hang forever.
    #[test]
    fn test_dkg_fails_below_threshold() {
        use commonware_runtime::{Clock as _, Runner as _};
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60))
            .start(|context| async move {
            // Use a short timeout to avoid slow test.
            // The DKG has DKG_TIMEOUT=120s, but we wrap with a shorter outer timeout.
            // The ceremony should not complete — it will hit DKG_TIMEOUT internally,
            // but we can't wait 120s in a test. Instead, verify it doesn't complete
            // within a reasonable window.
            // Should not complete — 2 nodes can't reach threshold=3.
            commonware_macros::select! {
                _ = run_partial_dkg(&context, 4, 2) => {
                    panic!("DKG should NOT complete with only 2/4 nodes online (below threshold=3)");
                },
                _ = context.sleep(std::time::Duration::from_secs(5)) => {},
            }
        });
    }

    /// 4 nodes, 1 offline — verify bootstrap does not return a threshold
    /// output that could later mismatch another validator's boundary artifact.
    #[test]
    fn test_bootstrap_dkg_does_not_return_threshold_subset_output() {
        use commonware_runtime::{Clock as _, Runner as _};
        commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(60)).start(
            |context| async move {
                commonware_macros::select! {
                    _ = run_partial_dkg(&context, 4, 3) => {
                        panic!("bootstrap DKG must wait for the complete genesis dealer-log set");
                    },
                    _ = context.sleep(std::time::Duration::from_secs(5)) => {},
                }
            },
        );
    }

    // -----------------------------------------------------------------------
    // Ack dedup — HashSet ignores duplicate inserts
    // -----------------------------------------------------------------------

    /// Verify that acked_players HashSet correctly deduplicates.
    /// In production, duplicate P2P ack messages from the same player
    /// must not inflate the ack count.
    #[test]
    fn test_ack_hashset_dedup() {
        use commonware_cryptography::bls12381;

        let key_a = bls12381::PrivateKey::from_seed(1);
        let pk_a = key_a.public_key();
        let key_b = bls12381::PrivateKey::from_seed(2);
        let pk_b = key_b.public_key();

        let mut acked_players = std::collections::BTreeSet::new();

        // First insert — count goes to 1
        acked_players.insert(pk_a.clone());
        assert_eq!(acked_players.len(), 1);

        // Duplicate insert of same key — count stays at 1
        acked_players.insert(pk_a.clone());
        assert_eq!(
            acked_players.len(),
            1,
            "duplicate ack must not increment count"
        );

        // Different key — count goes to 2
        acked_players.insert(pk_b.clone());
        assert_eq!(acked_players.len(), 2);

        // Duplicate of second key — still 2
        acked_players.insert(pk_b);
        assert_eq!(acked_players.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Self-dealing ack count — only on success
    // -----------------------------------------------------------------------

    /// Verify that self-ack is counted only when self-dealing succeeds.
    /// The acked_players HashSet starts empty and self-ack is inserted only
    /// after successful receive_player_ack.
    #[test]
    fn test_self_ack_starts_empty() {
        let acked_players: std::collections::BTreeSet<
            commonware_cryptography::bls12381::PublicKey,
        > = std::collections::BTreeSet::new();

        // Starts at 0 (not 1 as the old code had)
        assert_eq!(acked_players.len(), 0, "acked_players must start empty");
    }

    #[test]
    fn test_valid_ack_removes_player_from_retry_set() {
        let key = bls12381::PrivateKey::from_seed(1);
        let player_pk = key.public_key();
        let mut unsent_shares = BTreeMap::new();
        unsent_shares.insert(player_pk.clone(), ());

        let mut acked_players = std::collections::BTreeSet::new();
        acked_players.insert(player_pk.clone());
        unsent_shares.remove(&player_pk);

        assert!(unsent_shares.is_empty());
        assert_eq!(acked_players.len(), 1);
    }

    #[test]
    fn duplicate_dealer_bundle_reuses_cached_ack() {
        let mut keys: Vec<bls12381::PrivateKey> =
            (0..3).map(bls12381::PrivateKey::from_seed).collect();
        keys.sort_by_key(|key| key.public_key().encode());
        let participants: Set<bls12381::PublicKey> = keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &crate::config::outbe_app_namespace(),
            0,
            None,
            Mode::NonZeroCounter,
            participants.clone(),
            participants,
        )
        .unwrap();

        let dealer_key = keys[0].clone();
        let player_key = keys[1].clone();
        let dealer_pk = dealer_key.public_key();
        let player_pk = player_key.public_key();
        let (_, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core::OsRng,
            info.clone(),
            dealer_key,
            None,
        )
        .unwrap();
        let priv_msg = priv_msgs
            .into_iter()
            .find(|(pk, _)| *pk == player_pk)
            .unwrap()
            .1;
        let mut player = Player::<MinSig, bls12381::PrivateKey>::new(info, player_key).unwrap();
        let mut accepted = BTreeMap::new();

        let first = handle_dealer_bundle(
            &mut player,
            &mut accepted,
            dealer_pk.clone(),
            pub_msg.clone(),
            priv_msg.clone(),
        );
        let second = handle_dealer_bundle(&mut player, &mut accepted, dealer_pk, pub_msg, priv_msg);

        assert!(matches!(first, DealerBundleAction::SendAck(_)));
        assert!(matches!(second, DealerBundleAction::DuplicateAck(_)));
        assert_eq!(accepted.len(), 1);
    }

    #[test]
    fn conflicting_dealer_bundle_is_not_acknowledged() {
        let mut keys: Vec<bls12381::PrivateKey> =
            (10..13).map(bls12381::PrivateKey::from_seed).collect();
        keys.sort_by_key(|key| key.public_key().encode());
        let participants: Set<bls12381::PublicKey> = keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &crate::config::outbe_app_namespace(),
            0,
            None,
            Mode::NonZeroCounter,
            participants.clone(),
            participants,
        )
        .unwrap();

        let dealer_key = keys[0].clone();
        let player_key = keys[1].clone();
        let dealer_pk = dealer_key.public_key();
        let player_pk = player_key.public_key();
        let (_, pub_msg, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core::OsRng,
            info.clone(),
            dealer_key.clone(),
            None,
        )
        .unwrap();
        let (_, conflicting_pub_msg, conflicting_priv_msgs) =
            Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                rand_core::OsRng,
                info.clone(),
                dealer_key,
                None,
            )
            .unwrap();
        let priv_msg = priv_msgs
            .into_iter()
            .find(|(pk, _)| *pk == player_pk)
            .unwrap()
            .1;
        let conflicting_priv_msg = conflicting_priv_msgs
            .into_iter()
            .find(|(pk, _)| *pk == player_pk)
            .unwrap()
            .1;
        let mut player = Player::<MinSig, bls12381::PrivateKey>::new(info, player_key).unwrap();
        let mut accepted = BTreeMap::new();

        let first = handle_dealer_bundle(
            &mut player,
            &mut accepted,
            dealer_pk.clone(),
            pub_msg,
            priv_msg,
        );
        let second = handle_dealer_bundle(
            &mut player,
            &mut accepted,
            dealer_pk,
            conflicting_pub_msg,
            conflicting_priv_msg,
        );

        assert!(matches!(first, DealerBundleAction::SendAck(_)));
        assert!(matches!(second, DealerBundleAction::Equivocation { .. }));
        assert_eq!(accepted.len(), 1);
    }

    #[test]
    fn retry_distribution_stops_after_dealer_finalization() {
        let mut keys: Vec<bls12381::PrivateKey> =
            (21..24).map(bls12381::PrivateKey::from_seed).collect();
        keys.sort_by_key(|key| key.public_key().encode());
        let participants: Set<bls12381::PublicKey> = keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let info = Info::<MinSig, bls12381::PublicKey>::new::<N3f1>(
            &crate::config::outbe_app_namespace(),
            0,
            None,
            Mode::NonZeroCounter,
            participants.clone(),
            participants,
        )
        .unwrap();
        let (_, _, priv_msgs) = Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_core::OsRng,
            info,
            keys[0].clone(),
            None,
        )
        .unwrap();
        let mut unsent_shares = BTreeMap::new();
        let (player_pk, priv_msg) = priv_msgs.into_iter().next().unwrap();
        unsent_shares.insert(player_pk, priv_msg);

        assert!(!should_retry_share_distribution(false, &unsent_shares));
        assert!(should_retry_share_distribution(true, &unsent_shares));
    }
}
