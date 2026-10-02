use super::super::recovery::DkgRetryStore;
use super::super::recovery::{
    load_or_create_dealer_snapshot, persist_dealer_ack, DkgDealerRetrySnapshot,
};
use super::super::wire::DkgCeremonyId;
use super::super::wire::DkgMessage;
use super::super::wire::DkgWireConfig;
use super::acknowledge_restart_replay;
use super::ceremony::{read_ceremony_message, DEALER_ONLY_DIAGNOSTICS};
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
use rand_commonware::SeedableRng;
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use tokio::sync::mpsc;
use tracing::debug;
use tracing::info;

use commonware_cryptography::bls12381::dkg::feldman_desmedt::{
    DealerPrivMsg, DealerPubMsg, PlayerAck,
};
use std::collections::BTreeSet;
use std::time::SystemTime;

struct DealerOnlyConfig {
    signing_key: bls12381::PrivateKey,
    participants: Set<bls12381::PublicKey>,
    previous_output: Output<MinSig, bls12381::PublicKey>,
    previous_share: Share,
    round: u64,
    retry_store: Option<DkgRetryStore>,
}

struct DealerOnlyChannels<S, R> {
    sender: S,
    receiver: R,
    progress_tx: mpsc::UnboundedSender<DkgProgress>,
}

/// A removed validator owns only dealer delivery and sealing state.
struct DealerOnlyState {
    info: Info<MinSig, bls12381::PublicKey>,
    ceremony_id: DkgCeremonyId,
    participants: Set<bls12381::PublicKey>,
    player_threshold: u32,
    wire_cfg: DkgWireConfig,
    retry_store: Option<DkgRetryStore>,
    dealer: Dealer<MinSig, bls12381::PrivateKey>,
    dealer_retry_snapshot: DkgDealerRetrySnapshot,
    my_pub_msg: DealerPubMsg<MinSig>,
    unsent_shares: BTreeMap<bls12381::PublicKey, DealerPrivMsg>,
    acked_players: BTreeSet<bls12381::PublicKey>,
    restart_replay_shares: BTreeMap<bls12381::PublicKey, DealerPrivMsg>,
    deadline: SystemTime,
    next_retry_tick: SystemTime,
    ack_collection_deadline: Option<SystemTime>,
}
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
    sender: impl P2pSender<PublicKey = bls12381::PublicKey>,
    receiver: impl P2pReceiver<PublicKey = bls12381::PublicKey>,
) -> Result<DkgDealerOnlyComplete> {
    run(
        clock,
        DealerOnlyConfig {
            signing_key,
            participants,
            previous_output,
            previous_share,
            round,
            retry_store: Some(retry_store),
        },
        DealerOnlyChannels {
            sender,
            receiver,
            progress_tx,
        },
    )
    .await
}

async fn run(
    clock: &impl Clock,
    config: DealerOnlyConfig,
    mut channels: DealerOnlyChannels<
        impl P2pSender<PublicKey = bls12381::PublicKey>,
        impl P2pReceiver<PublicKey = bls12381::PublicKey>,
    >,
) -> Result<DkgDealerOnlyComplete> {
    let mut state = DealerOnlyState::start(clock, config, &mut channels.sender).await?;
    loop {
        // Keep the original biased receive > retry > grace > timeout order.
        commonware_macros::select! {
            msg_result = channels.receiver.recv() => {
                let (from, mut raw) = msg_result.map_err(|e| eyre::eyre!("dealer-only DKG P2P receiver error: {e}"))?;
                let Some(msg) = state.read_message(&from, &mut raw) else { continue };
                state.handle_message(from, msg)?;
            },
            _ = clock.sleep_until(state.next_retry_tick) => state.retry(&mut channels.sender).await,
            _ = sleep_until_optional(clock, state.ack_collection_deadline) => {},
            _ = clock.sleep_until(state.deadline) => return Err(state.timeout_error()),
        }
        if state.ready_to_finalize(clock) {
            break;
        }
    }
    state.publish(&mut channels.sender, &channels.progress_tx)
}

impl DealerOnlyState {
    async fn start(
        clock: &impl Clock,
        config: DealerOnlyConfig,
        sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
    ) -> Result<Self> {
        let DealerOnlyConfig {
            signing_key,
            participants,
            previous_output,
            previous_share,
            round,
            retry_store,
        } = config;
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

        let dealer_retry_snapshot = load_or_create_dealer_snapshot(
            retry_store.as_ref(),
            ceremony_id,
            round,
            "restoring durable dealer-only DKG transcript",
        )?;

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
        let restart_replay_shares =
            take_restart_replay_shares(&mut unsent_shares, &dealer_retry_snapshot.accepted_acks);
        for (player_pk, ack) in &dealer_retry_snapshot.accepted_acks {
            dealer
                .receive_player_ack(player_pk.clone(), ack.clone())
                .map_err(|error| {
                    eyre::eyre!("failed to replay durable dealer-only ACK: {error:?}")
                })?;
            acked_players.insert(player_pk.clone());
        }
        send_shares(sender, ceremony_id, &my_pub_msg, &restart_replay_shares).await;
        send_shares(sender, ceremony_id, &my_pub_msg, &unsent_shares).await;

        // Same runtime-agnostic deadline + interval-cadence retry tick as
        // `run_initial_dkg` (see that function for the first-tick rationale).
        let now = clock.current();
        let deadline = now + DKG_TIMEOUT;
        let next_retry_tick = now + RETRY_INTERVAL;

        Ok(Self {
            info,
            ceremony_id,
            participants,
            player_threshold,
            wire_cfg,
            retry_store,
            dealer,
            dealer_retry_snapshot,
            my_pub_msg,
            unsent_shares,
            acked_players,
            restart_replay_shares,
            deadline,
            next_retry_tick,
            ack_collection_deadline: None,
        })
    }
    fn read_message(
        &self,
        from: &bls12381::PublicKey,
        buf: &mut impl bytes::Buf,
    ) -> Option<DkgMessage> {
        read_ceremony_message(from, buf, &self.wire_cfg, &DEALER_ONLY_DIAGNOSTICS)
    }
    fn handle_message(&mut self, from: bls12381::PublicKey, msg: DkgMessage) -> Result<()> {
        match msg {
            DkgMessage::Ack { ack, .. } => self.handle_ack(from, ack)?,
            DkgMessage::DealerBundle { .. } => {
                debug!(?from, "dealer-only DKG ignoring dealer bundle")
            }
            DkgMessage::FinalizedLog { .. } => {
                debug!(?from, "dealer-only DKG ignoring finalized log")
            }
        }
        Ok(())
    }
    fn handle_ack(
        &mut self,
        from: bls12381::PublicKey,
        ack: PlayerAck<bls12381::PublicKey>,
    ) -> Result<()> {
        if acknowledge_restart_replay(
            &mut self.restart_replay_shares,
            Some(&self.dealer_retry_snapshot.accepted_acks),
            &from,
            &ack,
        ) {
            debug!(
                ?from,
                remaining = self.restart_replay_shares.len(),
                "dealer-only restart replay confirmed by player"
            );
        } else {
            match self.dealer.receive_player_ack(from.clone(), ack.clone()) {
                Ok(()) => {
                    let is_new_ack = self.acked_players.insert(from.clone());
                    if is_new_ack {
                        persist_dealer_ack(
                            Some(&mut self.dealer_retry_snapshot),
                            self.retry_store.as_ref(),
                            from.clone(),
                            ack,
                        )?;
                    }
                    self.unsent_shares.remove(&from);
                    debug!(
                        ?from,
                        acks_received = self.acked_players.len(),
                        player_threshold = self.player_threshold,
                        is_new_ack,
                        remaining = self.unsent_shares.len(),
                        "dealer-only DKG received ack"
                    );
                }
                Err(e) => {
                    debug!(?e, ?from, "dealer-only DKG ack rejected");
                }
            }
        }
        Ok(())
    }
    async fn retry(&mut self, sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>) {
        self.next_retry_tick += RETRY_INTERVAL;
        if !self.restart_replay_shares.is_empty() || !self.unsent_shares.is_empty() {
            debug!(
                unacknowledged = self.unsent_shares.len(),
                restart_replays = self.restart_replay_shares.len(),
                "dealer-only DKG retrying share distribution"
            );
            send_shares(
                sender,
                self.ceremony_id,
                &self.my_pub_msg,
                &self.restart_replay_shares,
            )
            .await;
            send_shares(
                sender,
                self.ceremony_id,
                &self.my_pub_msg,
                &self.unsent_shares,
            )
            .await;
        }
    }
    fn ready_to_finalize(&mut self, clock: &impl Clock) -> bool {
        if self.acked_players.len() >= self.player_threshold as usize
            && self.ack_collection_deadline.is_none()
        {
            self.ack_collection_deadline = Some(clock.current() + ACK_COLLECTION_GRACE);
            debug!(
                acks = self.acked_players.len(),
                validators = self.participants.len(),
                grace_ms = ACK_COLLECTION_GRACE.as_millis(),
                "dealer-only quorum reached; collecting remaining ACKs before finalization"
            );
        }
        let ack_grace_elapsed = self
            .ack_collection_deadline
            .is_some_and(|at| clock.current().duration_since(at).is_ok());
        let current_process_delivery_complete =
            self.unsent_shares.is_empty() && self.restart_replay_shares.is_empty();
        current_process_delivery_complete || ack_grace_elapsed
    }
    fn timeout_error(&self) -> eyre::Report {
        eyre::eyre!(
            "dealer-only DKG timed out after {:?} (acks: {}/{})",
            DKG_TIMEOUT,
            self.acked_players.len(),
            self.player_threshold,
        )
    }
    fn publish(
        self,
        sender: &mut impl P2pSender<PublicKey = bls12381::PublicKey>,
        progress_tx: &mpsc::UnboundedSender<DkgProgress>,
    ) -> Result<DkgDealerOnlyComplete> {
        let signed_log = self.dealer.finalize::<N3f1>();
        if signed_log.clone().check(&self.info).is_none() {
            return Err(eyre::eyre!(
                "dealer-only DKG finalized log failed verification"
            ));
        }

        progress_tx
            .send(DkgProgress::LocalDealerLog(Bytes::from(
                signed_log.encode(),
            )))
            .map_err(|_| eyre::eyre!("failed to publish dealer-only DKG local dealer log"))?;

        if !send_finalized_log(sender, self.ceremony_id, signed_log) {
            debug!("dealer-only finalized-log broadcast had no accepting recipients this attempt (rate-limited/backpressure); recovered by the retry tick");
        }

        info!(
            acks = self.acked_players.len(),
            player_threshold = self.player_threshold,
            "dealer-only DKG complete - local dealer log published"
        );
        Ok(DkgDealerOnlyComplete {
            participants: self.participants,
        })
    }
}
