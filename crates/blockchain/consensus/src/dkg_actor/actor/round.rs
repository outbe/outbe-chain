use super::super::recovery::handle_player_bundle;
use super::super::recovery::restore_player;
use super::super::recovery::DkgDealerRetrySnapshot;
use super::super::recovery::DkgRetryStore;
use super::super::recovery::PlayerBundleAction;
use super::super::wire::DkgCeremonyId;
use super::super::wire::DkgMessage;
use super::super::wire::DkgMessageReadError;
use super::super::wire::DkgWireConfig;
use super::acknowledge_restart_replay;
use super::chain_finalized_reconstructable;
use super::decode_signed_dealer_log;
use super::gossip_finalized_logs;
use super::record_and_store_signed_dealer_log;
use super::record_signed_dealer_log;
use super::recv_chain_finalized_log;
use super::send_ack;
use super::send_finalized_log;
use super::send_shares;
use super::should_retry_finalized_log_gossip;
use super::should_retry_share_distribution;
use super::sleep_until_optional;
use super::take_restart_replay_shares;
use super::DkgComplete;
use super::DkgProgress;
use super::ACK_COLLECTION_GRACE;
use super::BOOTSTRAP_FINALIZED_LOG_GOSSIP_GRACE;
use super::DKG_TIMEOUT;
use super::RETRY_INTERVAL;
use alloy_primitives::Bytes;
use commonware_codec::Encode;
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Dealer;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::DealerLog;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Info;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Logs;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Output;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::SignedDealerLog;
use commonware_cryptography::bls12381::primitives::group::Share;
use commonware_cryptography::bls12381::primitives::sharing::Mode;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_p2p::Receiver as P2pReceiver;
use commonware_p2p::Sender as P2pSender;
use commonware_parallel::Sequential;
use commonware_runtime::Clock;
use commonware_utils::ordered::Quorum;
use commonware_utils::ordered::Set;
use commonware_utils::N3f1;
use eyre::Result;
use rand_commonware::Rng as RngCore;
use rand_commonware::SeedableRng;
use std::collections::BTreeMap;
use std::num::NonZeroU32;

// Intentionally `tokio::sync::mpsc`: `progress_tx` / `finalized_log_rx` are created
// cross-crate by `outbe-engine` (`crates/blockchain/engine/src/stack.rs`) and have no
// timer/spawn dependency, so they are runtime-agnostic and do not require the tokio
// reactor. The type is kept to preserve the cross-crate engine API.
use tokio::sync::mpsc;
use tracing::debug;
use tracing::info;
use tracing::warn;

/// Run a DKG ceremony with durable local dealer and player recovery.
///
/// The dealer seed is persisted before any bundle is sent. Player inputs are
/// persisted before their ACK is emitted. A restarted process reconstructs both
/// roles and verifies byte-identical ACK replay before networking resumes.
#[allow(clippy::too_many_arguments)]
pub async fn run_initial_dkg_durable(
    clock: &impl Clock,
    signing_key: bls12381::PrivateKey,
    participants: Set<bls12381::PublicKey>,
    previous_output: Option<Output<MinSig, bls12381::PublicKey>>,
    previous_share: Option<Share>,
    round: u64,
    progress_tx: Option<mpsc::UnboundedSender<DkgProgress>>,
    finalized_log_rx: Option<mpsc::UnboundedReceiver<Bytes>>,
    retry_store: DkgRetryStore,
    mut sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    mut receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<DkgComplete> {
    let retry_store = Some(retry_store);
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
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        dealers,
        participants.clone(),
    )
    .map_err(|e| eyre::eyre!("failed to create DKG info: {e:?}"))?;

    let mut dealer_retry_snapshot = if is_local_dealer {
        let snapshot = match retry_store.as_ref() {
            Some(store) => match store.load_dealer(ceremony_id)? {
                Some(snapshot) => {
                    info!(round, "restoring durable DKG dealer transcript");
                    snapshot
                }
                None => {
                    let mut seed = [0u8; 32];
                    rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng)
                        .fill_bytes(&mut seed);
                    // Persist before the first network-visible dealer message.
                    let snapshot = DkgDealerRetrySnapshot {
                        ceremony_id,
                        seed,
                        accepted_acks: BTreeMap::new(),
                    };
                    store.save_dealer(&snapshot)?;
                    snapshot
                }
            },
            None => {
                let mut seed = [0u8; 32];
                rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng)
                    .fill_bytes(&mut seed);
                DkgDealerRetrySnapshot {
                    ceremony_id,
                    seed,
                    accepted_acks: BTreeMap::new(),
                }
            }
        };
        Some(snapshot)
    } else {
        None
    };

    // Old share holders are dealers in a reshare. New validators are
    // players only until they receive a fresh threshold share.
    let (mut dealer, my_pub_msg, priv_msgs) = if is_local_dealer {
        let dealer_seed = dealer_retry_snapshot
            .as_ref()
            .ok_or_else(|| eyre::eyre!("local dealer retry snapshot was not initialized"))?
            .seed;
        let (dealer, my_pub_msg, priv_msgs) =
            Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
                rand_commonware::rngs::ChaCha20Rng::from_seed(dealer_seed),
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

    let max_players = NonZeroU32::new(n)
        .ok_or_else(|| eyre::eyre!("DKG ceremony requires at least one participant"))?;
    // Reconstruct every previously acknowledged dealing before processing new
    // traffic. The snapshot is ceremony-scoped and replayed in dealer-key order.
    let (mut player, mut player_retry_snapshot) = restore_player(
        info.clone(),
        signing_key.clone(),
        ceremony_id,
        max_players,
        retry_store.as_ref(),
    )?;

    // Build a map of unsent private shares: public_key -> DealerPrivMsg.
    let mut unsent_shares: BTreeMap<bls12381::PublicKey, _> = priv_msgs.into_iter().collect();

    // Use a BTreeSet for unique ack tracking instead of a counter
    // (BTreeSet, not HashSet - deterministic iteration order on the consensus path).
    // Start empty - only count self-ack if self-dealing succeeded below.
    let mut acked_players: std::collections::BTreeSet<bls12381::PublicKey> =
        std::collections::BTreeSet::new();

    // Handle self-dealing locally (no network round-trip).
    // Only count self-ack if self-dealing validation succeeds.
    if let (Some(my_pub_msg), Some(my_priv_msg)) =
        (my_pub_msg.as_ref(), unsent_shares.remove(&my_pk))
    {
        if let PlayerBundleAction::SendAck(generated_ack)
        | PlayerBundleAction::DuplicateAck(generated_ack) = handle_player_bundle(
            &mut player,
            &mut player_retry_snapshot,
            retry_store.as_ref(),
            my_pk.clone(),
            my_pub_msg.clone(),
            my_priv_msg,
        )? {
            if let Some(ref mut d) = dealer {
                let recovered_ack = dealer_retry_snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.accepted_acks.get(&my_pk))
                    .cloned();
                let is_recovered = recovered_ack.is_some();
                let ack = recovered_ack.unwrap_or_else(|| generated_ack.clone());
                d.receive_player_ack(my_pk.clone(), ack)
                    .map_err(|e| eyre::eyre!("failed to process self-ack: {e:?}"))?;
                acked_players.insert(my_pk.clone());
                if !is_recovered {
                    if let Some(snapshot) = dealer_retry_snapshot.as_mut() {
                        snapshot.accepted_acks.insert(my_pk.clone(), generated_ack);
                        if let Some(store) = retry_store.as_ref() {
                            store.save_dealer(snapshot)?;
                        }
                    }
                }
            }
            debug!("self-dealing complete");
        } else {
            warn!("self-dealing validation failed");
        }
    }

    // Rehydrate accepted remote ACKs before the first periodic retry. An ACK is
    // durable evidence for this local Dealer, but it does not prove that the
    // remote process-local Player state survived the same restart. Preserve each
    // acknowledged dealing in a separate byte-identical replay set until the
    // restarted Player confirms current-process ingestion with the same ACK.
    let mut restart_replay_shares = BTreeMap::new();
    if let (Some(d), Some(snapshot)) = (dealer.as_mut(), dealer_retry_snapshot.as_ref()) {
        restart_replay_shares =
            take_restart_replay_shares(&mut unsent_shares, &snapshot.accepted_acks);
        for (player_pk, ack) in &snapshot.accepted_acks {
            if player_pk == &my_pk {
                continue;
            }
            d.receive_player_ack(player_pk.clone(), ack.clone())
                .map_err(|error| eyre::eyre!("failed to replay durable DKG ACK: {error:?}"))?;
            acked_players.insert(player_pk.clone());
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
    let wire_cfg = DkgWireConfig {
        max_players,
        expected_ceremony_id: ceremony_id,
    };
    let chain_finalized_mode = finalized_log_rx.is_some();
    let mut finalized_log_rx = finalized_log_rx;

    // Send shares to all other players.
    if let Some(my_pub_msg) = my_pub_msg.as_ref() {
        send_shares(&mut sender, ceremony_id, my_pub_msg, &restart_replay_shares).await;
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
    let mut ack_collection_deadline = None;
    // C1 (chain-finalized completion gate): only probe `observe` when a NEW dealer
    // log has arrived, so the actor breaks at the same first-reconstructable log
    // prefix that `DkgManager` freezes `canonical_output` at (-> matching output).
    let mut last_reconstruct_probe_len: usize = 0;

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
                        match handle_player_bundle(
                            &mut player,
                            &mut player_retry_snapshot,
                            retry_store.as_ref(),
                            from.clone(),
                            pub_msg,
                            priv_msg,
                        )? {
                            PlayerBundleAction::SendAck(ack) => {
                                send_ack(&mut sender, ceremony_id, &from, ack, "sent ack to dealer").await;
                            }
                            PlayerBundleAction::DuplicateAck(ack) => {
                                send_ack(
                                    &mut sender,
                                    ceremony_id,
                                    &from,
                                    ack,
                                    "resent cached ack for duplicate dealer bundle",
                                )
                                .await;
                            }
                            PlayerBundleAction::Equivocation { previous, received } => {
                                warn!(
                                    ?from,
                                    previous_bundle_hash = %previous,
                                    received_bundle_hash = %received,
                                    "dealer sent conflicting DKG bundle"
                                );
                                invalid_dealers.insert(from.clone());
                            }
                            PlayerBundleAction::Invalid => {
                                warn!(?from, "dealer sent invalid share - potential misbehavior");
                                invalid_dealers.insert(from.clone());
                            }
                        }
                    }
                    DkgMessage::Ack { ack, .. } => {
                        // We are a Dealer receiving an ack from a Player.
                        // A byte-identical ACK for a recovered dealing confirms that
                        // the restarted remote Player ingested our replay. It is
                        // already present in Dealer state, so do not feed it twice.
                        if let Some(ref mut d) = dealer {
                            if acknowledge_restart_replay(
                                &mut restart_replay_shares,
                                dealer_retry_snapshot
                                    .as_ref()
                                    .map(|snapshot| &snapshot.accepted_acks),
                                &from,
                                &ack,
                            ) {
                                debug!(
                                    ?from,
                                    remaining = restart_replay_shares.len(),
                                    "restart replay confirmed by player"
                                );
                            } else {
                                match d.receive_player_ack(from.clone(), ack.clone()) {
                                Ok(()) => {
                                    let is_new_ack = acked_players.insert(from.clone());
                                    if is_new_ack {
                                        if let Some(snapshot) = dealer_retry_snapshot.as_mut() {
                                            snapshot.accepted_acks.insert(from.clone(), ack);
                                            if let Some(store) = retry_store.as_ref() {
                                                store.save_dealer(snapshot)?;
                                            }
                                        }
                                    }
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
                if should_retry_share_distribution(dealer.is_some(), &restart_replay_shares)
                    || should_retry_share_distribution(dealer.is_some(), &unsent_shares)
                {
                    if let Some(my_pub_msg) = my_pub_msg.as_ref() {
                        debug!(
                            unacknowledged = unsent_shares.len(),
                            restart_replays = restart_replay_shares.len(),
                            "retrying share distribution"
                        );
                        send_shares(
                            &mut sender,
                            ceremony_id,
                            my_pub_msg,
                            &restart_replay_shares,
                        )
                        .await;
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

            _ = sleep_until_optional(clock, ack_collection_deadline) => {},

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

        // A threshold is enough for liveness, but not enough to avoid publicly
        // revealing a healthy player's evaluation. Give the remaining players a
        // bounded ACK grace; finalize immediately if everybody already ACKed.
        if dealer.is_some()
            && acked_players.len() >= player_threshold as usize
            && ack_collection_deadline.is_none()
        {
            ack_collection_deadline = Some(clock.current() + ACK_COLLECTION_GRACE);
            debug!(
                acks = acked_players.len(),
                validators = n,
                grace_ms = ACK_COLLECTION_GRACE.as_millis(),
                "dealer quorum reached; collecting remaining ACKs before finalization"
            );
        }
        let ack_grace_elapsed =
            ack_collection_deadline.is_some_and(|at| clock.current().duration_since(at).is_ok());
        let current_process_delivery_complete =
            unsent_shares.is_empty() && restart_replay_shares.is_empty();
        if dealer.is_some()
            && acked_players.len() >= player_threshold as usize
            && (current_process_delivery_complete || ack_grace_elapsed)
        {
            let Some(d) = dealer.take() else {
                continue;
            };
            // Do not leave an elapsed timer permanently ready in the biased
            // select: after sealing, the actor must keep receiving dealer logs.
            ack_collection_deadline = None;
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
            restart_replay_shares.clear();
            if !send_finalized_log(&mut sender, ceremony_id, signed_log) {
                debug!("finalized-log broadcast had no accepting recipients this attempt (rate-limited/backpressure); recovered by the DKG retry tick");
            }
        }

        // Non-chain interactive bootstrap: there is no canonical chain carrier yet,
        // so a validator completes only when ALL genesis dealer logs are collected
        // (threshold P2P subsets are not canonical and may otherwise produce
        // different public polynomials on different validators). Chain-finalized
        // reshare does NOT complete on the raw all-n count - it flows through the
        // observe gate below (C1), so the actor breaks at the same log-set prefix
        // `DkgManager` freezes `canonical_output` at.
        if !chain_finalized_mode && finalized_logs.len() as u32 >= n {
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

        // Once the finalized chain carries a reconstructable threshold, an
        // interrupted local dealer must not hold recovery hostage waiting for
        // ACKs from peers that have already completed this ceremony. The
        // canonical logs are the decision; durable Player replay lets a target
        // participant recover its private share from them. Bootstrap still
        // requires the local dealer to finish because it has no chain-finalized
        // carrier.
        if (chain_finalized_mode || dealer.is_none())
            && finalized_logs.len() as u32 >= log_threshold
        {
            if chain_finalized_mode {
                // C1: do NOT complete on a RAW 2f+1 count. A byzantine dealer can
                // chain-finalize a signed-but-content-garbage log (passes the
                // signature-only acceptance check, then dropped by `select`'s content
                // check), so the raw first-2f+1 may hold < 2f+1 content-valid logs and
                // one-shot `finalize` would fail `DkgFailed` identically on every node.
                // Gate completion on `observe` (public, share-free) over the FULL
                // finalized_logs - probing once per NEW chain log so the actor breaks
                // at the SAME first-observe-success prefix `DkgManager` freezes
                // `canonical_output` at (-> matching output). Never waits for all-n (an
                // offline dealer's log never arrives); the ceremony deadline bounds it.
                if finalized_logs.len() > last_reconstruct_probe_len {
                    last_reconstruct_probe_len = finalized_logs.len();
                    if chain_finalized_reconstructable(&info, &finalized_logs) {
                        info!(
                            logs = finalized_logs.len(),
                            log_threshold,
                            "chain-finalized content-valid threshold reached (observe ok); completing ceremony"
                        );
                        break;
                    }
                    debug!(
                        logs = finalized_logs.len(),
                        log_threshold,
                        "raw threshold reached but < 2f+1 content-valid dealer logs (a signed-but-garbage log is present); awaiting more chain-finalized logs"
                    );
                }
            } else if !bootstrap_threshold_logged {
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
    // A target participant completes only with its private threshold share.
    // Public `observe` remains the canonical-output gate above, but it is not a
    // substitute for local Player state and cannot promote a shareless validator.
    let (output, share) = player
        .finalize::<N3f1, commonware_cryptography::bls12381::Batch>(
            &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
            finalize_logs,
            &Sequential,
        )
        .map_err(|error| eyre::eyre!("player finalize failed: {error:?}"))?;

    info!("DKG ceremony complete - threshold material obtained");

    // surface validators whose individual share evaluation was publicly
    // REVEALED during the ceremony (they were offline/non-acking, so
    // `feldman_desmedt` reveals their share so recovery can complete). The
    // reveals are permanently committed on-chain in the `DealerLog` artifacts,
    // and a revealed share makes that validator's VRF threshold partial publicly
    // forgeable - bounded (VRF drives leader election/fairness, not BFT safety:
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
                 its VRF threshold partial is now publicly forgeable - rotate this validator's \
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
