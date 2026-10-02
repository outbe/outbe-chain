use super::*;
use commonware_cryptography::Signer as _;
use commonware_p2p::Recipients;
use commonware_runtime::{IoBuf, Runner as _};
use commonware_utils::TryCollect as _;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct CheckingSender {
    store: DkgRetryStore,
    ceremony_id: DkgCeremonyId,
    local: bls12381::PublicKey,
    peers: Vec<bls12381::PublicKey>,
    sent: Arc<Mutex<Vec<IoBuf>>>,
}

struct Checked<'a> {
    sender: &'a CheckingSender,
    recipients: Recipients<bls12381::PublicKey>,
}

impl commonware_p2p::CheckedSender for Checked<'_> {
    type PublicKey = bls12381::PublicKey;

    fn recipients(&self) -> Vec<Self::PublicKey> {
        match &self.recipients {
            Recipients::One(pk) => vec![pk.clone()],
            Recipients::Some(pks) => pks.clone(),
            Recipients::All => self.sender.peers.clone(),
        }
    }

    fn send(
        self,
        message: impl Into<commonware_runtime::IoBufs> + Send,
        _priority: bool,
    ) -> commonware_actor::Unreliable<commonware_actor::Feedback> {
        let snapshot = self
            .sender
            .store
            .load_dealer(self.sender.ceremony_id)
            .unwrap()
            .expect("dealer seed must be durable before the first network send");
        assert!(
            snapshot.accepted_acks.contains_key(&self.sender.local),
            "self ACK must also be durable before sending shares"
        );
        self.sender
            .sent
            .lock()
            .unwrap()
            .push(message.into().coalesce());
        commonware_actor::Unreliable::Outcome(commonware_actor::Feedback::Ok)
    }
}

impl commonware_p2p::LimitedSender for CheckingSender {
    type PublicKey = bls12381::PublicKey;
    type Checked<'a> = Checked<'a>;

    fn check(
        &mut self,
        recipients: Recipients<Self::PublicKey>,
    ) -> Result<Self::Checked<'_>, SystemTime> {
        Ok(Checked {
            sender: self,
            recipients,
        })
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    keys: Vec<bls12381::PrivateKey>,
    participants: Set<bls12381::PublicKey>,
    sender: CheckingSender,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut keys = (300..304)
            .map(bls12381::PrivateKey::from_seed)
            .collect::<Vec<_>>();
        keys.sort_by_key(|key| key.public_key().encode());
        let participants = keys
            .iter()
            .map(|key| key.public_key())
            .try_collect()
            .unwrap();
        let sender = CheckingSender {
            store: DkgRetryStore::in_keys_dir(directory.path(), crate::bls::KeyBackend::Plaintext),
            ceremony_id: DkgCeremonyId::new(
                &crate::config::outbe_app_namespace(),
                0,
                None,
                &participants,
            ),
            local: keys[0].public_key(),
            peers: keys[1..].iter().map(|key| key.public_key()).collect(),
            sent: Arc::new(Mutex::new(Vec::new())),
        };
        Self {
            directory,
            keys,
            participants,
            sender,
        }
    }

    fn config(&self) -> CeremonyConfig {
        CeremonyConfig {
            signing_key: self.keys[0].clone(),
            participants: self.participants.clone(),
            previous_output: None,
            previous_share: None,
            round: 0,
            retry_store: Some(self.sender.store.clone()),
        }
    }

    fn remote_ack(&self, state: &CeremonyState) -> PlayerAck<bls12381::PublicKey> {
        let mut player = Player::new(state.info.clone(), self.keys[1].clone()).unwrap();
        player
            .dealer_message::<N3f1>(
                self.keys[0].public_key(),
                state.roles.my_pub_msg.as_ref().unwrap().clone(),
                state.roles.unsent_shares[&self.keys[1].public_key()].clone(),
            )
            .unwrap()
            .unwrap()
    }
}

#[test]
fn recovery_persists_seed_and_self_ack_before_networking_and_arms_retry_afterwards() {
    commonware_runtime::deterministic::Runner::default().start(|clock| async move {
        let mut fixture = Fixture::new();
        let state = CeremonyState::start(&clock, fixture.config(), false, &mut fixture.sender)
            .await
            .unwrap();
        assert_eq!(fixture.sender.sent.lock().unwrap().len(), 3);
        assert_eq!(state.roles.acked_players.len(), 1);
        assert_eq!(state.next_retry_tick, clock.current() + RETRY_INTERVAL);
        assert_eq!(state.deadline, clock.current() + DKG_TIMEOUT);
        assert!(state.ack_collection_deadline.is_none());
    });
}

#[test]
fn recovered_remote_ack_confirms_replay_without_counting_a_new_ack() {
    commonware_runtime::deterministic::Runner::default().start(|clock| async move {
        let mut fixture = Fixture::new();
        let config = fixture.config();
        let mut state = CeremonyState::start(&clock, config, true, &mut fixture.sender)
            .await
            .unwrap();
        let from = fixture.keys[1].public_key();
        let ack = fixture.remote_ack(&state);
        let message = DkgMessage::Ack {
            ceremony_id: state.ceremony_id,
            ack: ack.clone(),
        };
        assert_eq!(
            state
                .handle_message(from.clone(), message.clone(), &mut fixture.sender, &None)
                .await
                .unwrap(),
            MessageOutcome::Advance
        );
        let persisted = fixture
            .sender
            .store
            .load_dealer(state.ceremony_id)
            .unwrap()
            .unwrap();
        assert_eq!(persisted.accepted_acks[&from].encode(), ack.encode());
        assert!(!state.roles.unsent_shares.contains_key(&from));
        drop(state);

        let mut restarted =
            CeremonyState::start(&clock, fixture.config(), true, &mut fixture.sender)
                .await
                .unwrap();
        assert!(restarted.roles.restart_replay_shares.contains_key(&from));
        let acknowledged = restarted.roles.acked_players.len();
        restarted
            .handle_message(from.clone(), message.clone(), &mut fixture.sender, &None)
            .await
            .unwrap();
        assert!(!restarted.roles.restart_replay_shares.contains_key(&from));
        assert_eq!(restarted.roles.acked_players.len(), acknowledged);
        restarted
            .handle_message(from, message, &mut fixture.sender, &None)
            .await
            .unwrap();
        assert_eq!(restarted.roles.acked_players.len(), acknowledged);
    });
}

#[test]
fn failed_dealer_ack_persistence_keeps_delivery_pending_and_stops_advancement() {
    commonware_runtime::deterministic::Runner::default().start(|clock| async move {
        let mut fixture = Fixture::new();
        let mut state = CeremonyState::start(&clock, fixture.config(), true, &mut fixture.sender)
            .await
            .unwrap();
        let from = fixture.keys[1].public_key();
        let ack = fixture.remote_ack(&state);
        let path = fixture
            .directory
            .path()
            .join(crate::dkg_actor::recovery::DKG_DEALER_RETRY_FILE);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let sent = fixture.sender.sent.lock().unwrap().len();
        let error = state
            .handle_message(
                from.clone(),
                DkgMessage::Ack {
                    ceremony_id: state.ceremony_id,
                    ack,
                },
                &mut fixture.sender,
                &None,
            )
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("failed to persist DKG dealer retry snapshot"));
        assert!(state.roles.unsent_shares.contains_key(&from));
        assert!(state.ack_collection_deadline.is_none());
        assert_eq!(fixture.sender.sent.lock().unwrap().len(), sent);
    });
}

#[test]
fn p2p_finalized_log_in_chain_mode_is_only_a_candidate_and_skips_post_select_gates() {
    commonware_runtime::deterministic::Runner::default().start(|clock| async move {
        let mut fixture = Fixture::new();
        let mut state = CeremonyState::start(&clock, fixture.config(), true, &mut fixture.sender)
            .await
            .unwrap();
        let signed_log = state.roles.dealer.take().unwrap().finalize::<N3f1>();
        let encoded = Bytes::from(signed_log.encode());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let outcome = state
            .handle_message(
                fixture.keys[0].public_key(),
                DkgMessage::FinalizedLog {
                    ceremony_id: state.ceremony_id,
                    signed_log,
                },
                &mut fixture.sender,
                &Some(tx),
            )
            .await
            .unwrap();
        assert_eq!(outcome, MessageOutcome::SkipIteration);
        assert!(state.finalized_logs.is_empty());
        assert!(state.signed_finalized_logs.is_empty());
        assert_eq!(state.last_reconstruct_probe_len, 0);
        match rx.try_recv().unwrap() {
            DkgProgress::P2pDealerLog(bytes) => assert_eq!(bytes, encoded),
            other => panic!("unexpected progress: {other:?}"),
        }
        state.record_chain_log(encoded);
        assert_eq!(state.finalized_logs.len(), 1);
        assert!(!state.chain_complete());
    });
}

#[test]
fn canonical_public_output_does_not_allow_completion_without_a_private_share() {
    commonware_runtime::deterministic::Runner::default().start(|clock| async move {
        let mut fixture = Fixture::new();
        let mut state = CeremonyState::start(&clock, fixture.config(), true, &mut fixture.sender)
            .await
            .unwrap();
        // Finish the actual local dealer, whose private self-dealing is present.
        for key in &fixture.keys[1..] {
            let mut player = Player::new(state.info.clone(), key.clone()).unwrap();
            let ack = player
                .dealer_message::<N3f1>(
                    fixture.keys[0].public_key(),
                    state.roles.my_pub_msg.as_ref().unwrap().clone(),
                    state.roles.unsent_shares[&key.public_key()].clone(),
                )
                .unwrap()
                .unwrap();
            state
                .handle_message(
                    key.public_key(),
                    DkgMessage::Ack {
                        ceremony_id: state.ceremony_id,
                        ack,
                    },
                    &mut fixture.sender,
                    &None,
                )
                .await
                .unwrap();
        }
        let local_log = state.roles.dealer.take().unwrap().finalize::<N3f1>();
        state.record_chain_log(Bytes::from(local_log.encode()));
        // Other players ACK these dealings, but this actor has not ingested them.
        for (index, dealer_key) in fixture.keys.iter().enumerate().skip(1) {
            let (mut dealer, public, private) = Dealer::start::<N3f1>(
                rand_commonware::rngs::ChaCha20Rng::from_seed([index as u8; 32]),
                state.info.clone(),
                dealer_key.clone(),
                None,
            )
            .unwrap();
            for (player_key, private) in private {
                let player_signing_key = fixture
                    .keys
                    .iter()
                    .find(|key| key.public_key() == player_key)
                    .unwrap();
                let mut player =
                    Player::new(state.info.clone(), player_signing_key.clone()).unwrap();
                let ack = player
                    .dealer_message::<N3f1>(dealer_key.public_key(), public.clone(), private)
                    .unwrap()
                    .unwrap();
                dealer.receive_player_ack(player_key, ack).unwrap();
            }
            state.record_chain_log(Bytes::from(dealer.finalize::<N3f1>().encode()));
        }
        assert!(
            state.chain_complete(),
            "the canonical public output is reconstructable"
        );
        assert!(
            state.finalize().is_err(),
            "private player recovery remains mandatory"
        );
    });
}
