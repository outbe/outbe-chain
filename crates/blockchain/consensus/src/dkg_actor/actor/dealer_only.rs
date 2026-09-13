use super::super::recovery::DkgDealerRetrySnapshot;
use super::super::recovery::DkgRetryStore;
use super::super::wire::DkgCeremonyId;
use super::super::wire::DkgMessage;
use super::super::wire::DkgMessageReadError;
use super::super::wire::DkgWireConfig;
use super::acknowledge_restart_replay;
use super::send_finalized_log;
use super::send_shares;
use super::sleep_until_optional;
use super::take_restart_replay_shares;
use super::DkgDealerOnlyComplete;
use super::DkgProgress;
use super::ACK_COLLECTION_GRACE;
use super::DKG_TIMEOUT;
use super::RETRY_INTERVAL;
use alloy_primitives::Bytes;
use commonware_codec::Encode;
use commonware_cryptography::bls12381;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Dealer;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Info;
use commonware_cryptography::bls12381::dkg::feldman_desmedt::Output;
use commonware_cryptography::bls12381::primitives::group::Share;
use commonware_cryptography::bls12381::primitives::sharing::Mode;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_p2p::Receiver as P2pReceiver;
use commonware_p2p::Sender as P2pSender;
use commonware_runtime::Clock;
use commonware_utils::ordered::Quorum;
use commonware_utils::ordered::Set;
use commonware_utils::N3f1;
use eyre::Result;
use rand_commonware::Rng as RngCore;
use rand_commonware::SeedableRng;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::info;
use tracing::warn;

#[allow(clippy::too_many_arguments)]
pub async fn run_reshare_dealer_only_durable(
    clock: &impl Clock,
    signing_key: bls12381::PrivateKey,
    participants: Set<bls12381::PublicKey>,
    previous_output: Output<MinSig, bls12381::PublicKey>,
    previous_share: Share,
    round: u64,
    progress_tx: mpsc::UnboundedSender<DkgProgress>,
    retry_store: DkgRetryStore,
    mut sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    mut receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<DkgDealerOnlyComplete> {
    let retry_store = Some(retry_store);
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
        commonware_cryptography::bls12381::dkg::feldman_desmedt::Reveal::V1,
        dealers,
        participants.clone(),
    )
    .map_err(|e| eyre::eyre!("failed to create dealer-only DKG info: {e:?}"))?;

    let max_players = NonZeroU32::new(participants.len() as u32)
        .ok_or_else(|| eyre::eyre!("dealer-only DKG requires at least one target player"))?;
    let ceremony_id = DkgCeremonyId::new(
        &crate::config::outbe_app_namespace(),
        round,
        Some(&previous_output_for_id),
        &participants,
    );

    let mut dealer_retry_snapshot = match retry_store.as_ref() {
        Some(store) => match store.load_dealer(ceremony_id)? {
            Some(snapshot) => {
                info!(round, "restoring durable dealer-only DKG transcript");
                snapshot
            }
            None => {
                let mut seed = [0u8; 32];
                rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng)
                    .fill_bytes(&mut seed);
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
            rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng).fill_bytes(&mut seed);
            DkgDealerRetrySnapshot {
                ceremony_id,
                seed,
                accepted_acks: BTreeMap::new(),
            }
        }
    };

    let (mut dealer, my_pub_msg, priv_msgs) =
        Dealer::<MinSig, bls12381::PrivateKey>::start::<N3f1>(
            rand_commonware::rngs::ChaCha20Rng::from_seed(dealer_retry_snapshot.seed),
            info.clone(),
            signing_key,
            Some(previous_share),
        )
        .map_err(|e| eyre::eyre!("failed to start dealer-only DKG dealer: {e:?}"))?;

    let wire_cfg = DkgWireConfig {
        max_players,
        expected_ceremony_id: ceremony_id,
    };

    let mut unsent_shares: BTreeMap<bls12381::PublicKey, _> = priv_msgs.into_iter().collect();
    let mut acked_players: std::collections::BTreeSet<bls12381::PublicKey> =
        std::collections::BTreeSet::new();
    let mut restart_replay_shares =
        take_restart_replay_shares(&mut unsent_shares, &dealer_retry_snapshot.accepted_acks);
    for (player_pk, ack) in &dealer_retry_snapshot.accepted_acks {
        dealer
            .receive_player_ack(player_pk.clone(), ack.clone())
            .map_err(|error| eyre::eyre!("failed to replay durable dealer-only ACK: {error:?}"))?;
        acked_players.insert(player_pk.clone());
    }
    send_shares(
        &mut sender,
        ceremony_id,
        &my_pub_msg,
        &restart_replay_shares,
    )
    .await;
    send_shares(&mut sender, ceremony_id, &my_pub_msg, &unsent_shares).await;

    // Same runtime-agnostic deadline + interval-cadence retry tick as
    // `run_initial_dkg` (see that function for the first-tick rationale).
    let now = clock.current();
    let deadline = now + DKG_TIMEOUT;
    let mut next_retry_tick = now + RETRY_INTERVAL;
    let mut ack_collection_deadline = None;

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
                        if acknowledge_restart_replay(
                            &mut restart_replay_shares,
                            Some(&dealer_retry_snapshot.accepted_acks),
                            &from,
                            &ack,
                        ) {
                            debug!(
                                ?from,
                                remaining = restart_replay_shares.len(),
                                "dealer-only restart replay confirmed by player"
                            );
                        } else {
                            match dealer.receive_player_ack(from.clone(), ack.clone()) {
                            Ok(()) => {
                                let is_new_ack = acked_players.insert(from.clone());
                                if is_new_ack {
                                    dealer_retry_snapshot
                                        .accepted_acks
                                        .insert(from.clone(), ack);
                                    if let Some(store) = retry_store.as_ref() {
                                        store.save_dealer(&dealer_retry_snapshot)?;
                                    }
                                }
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
                if !restart_replay_shares.is_empty() || !unsent_shares.is_empty() {
                    debug!(
                        unacknowledged = unsent_shares.len(),
                        restart_replays = restart_replay_shares.len(),
                        "dealer-only DKG retrying share distribution"
                    );
                    send_shares(
                        &mut sender,
                        ceremony_id,
                        &my_pub_msg,
                        &restart_replay_shares,
                    )
                    .await;
                    send_shares(&mut sender, ceremony_id, &my_pub_msg, &unsent_shares).await;
                }
            },

            _ = sleep_until_optional(clock, ack_collection_deadline) => {},

            _ = clock.sleep_until(deadline) => {
                return Err(eyre::eyre!(
                    "dealer-only DKG timed out after {:?} (acks: {}/{})",
                    DKG_TIMEOUT,
                    acked_players.len(),
                    player_threshold,
                ));
            },
        }

        if acked_players.len() >= player_threshold as usize && ack_collection_deadline.is_none() {
            ack_collection_deadline = Some(clock.current() + ACK_COLLECTION_GRACE);
            debug!(
                acks = acked_players.len(),
                validators = participants.len(),
                grace_ms = ACK_COLLECTION_GRACE.as_millis(),
                "dealer-only quorum reached; collecting remaining ACKs before finalization"
            );
        }
        let ack_grace_elapsed =
            ack_collection_deadline.is_some_and(|at| clock.current().duration_since(at).is_ok());
        let current_process_delivery_complete =
            unsent_shares.is_empty() && restart_replay_shares.is_empty();
        if current_process_delivery_complete || ack_grace_elapsed {
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
        player_threshold, "dealer-only DKG complete - local dealer log published"
    );
    Ok(DkgDealerOnlyComplete { participants })
}
