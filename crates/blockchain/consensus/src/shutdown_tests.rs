use std::{
    convert::Infallible,
    marker::PhantomData,
    num::{NonZeroU16, NonZeroUsize},
    sync::mpsc,
    time::{Duration, SystemTime},
};

use commonware_actor::{Feedback, Unreliable};
use commonware_codec::DecodeExt as _;
use commonware_consensus::{
    simplex::{
        elector::RoundRobin, types::Vote, Config as SimplexConfig, Engine, Floor, ForwardPolicy,
    },
    types::{Epoch, View, ViewDelta},
    Viewable as _,
};
use commonware_cryptography::{
    bls12381::{primitives::variant::MinSig, PrivateKey},
    Sha256, Signer as _,
};
use commonware_p2p::{Blocker, CheckedSender, LimitedSender, Message, Receiver, Recipients};
use commonware_parallel::Sequential;
use commonware_runtime::{
    buffer::paged::CacheRef, tokio, Clock as _, IoBufs, Runner as _, Spawner as _, Supervisor as _,
};
use commonware_utils::{ordered::Set, NZUsize};

use crate::{
    bls::bootstrap_dkg,
    hybrid::HybridScheme,
    test_harness::{mock_genesis, MockAutomaton, MockRelay, MockReporter},
};

type RecordedVote = Vote<HybridScheme<MinSig>, commonware_cryptography::sha256::Digest>;

const PAGE_SIZE: NonZeroU16 = NonZeroU16::new(1024).expect("page size is non-zero");
const PAGE_CACHE_SIZE: NonZeroUsize = NZUsize!(10);

#[derive(Clone)]
struct NullSender<P> {
    participants: Vec<P>,
    send_notifications: Option<mpsc::Sender<RecordedVote>>,
}

struct NullCheckedSender<P> {
    recipients: Vec<P>,
    send_notifications: Option<mpsc::Sender<RecordedVote>>,
}

impl<P> CheckedSender for NullCheckedSender<P>
where
    P: commonware_cryptography::PublicKey,
{
    type PublicKey = P;

    fn recipients(&self) -> Vec<Self::PublicKey> {
        self.recipients.clone()
    }

    fn send(self, message: impl Into<IoBufs> + Send, _priority: bool) -> Unreliable<Feedback> {
        if let Some(notifications) = self.send_notifications {
            let vote =
                RecordedVote::decode(message.into()).expect("outbound Simplex vote must decode");
            let _ = notifications.send(vote);
        }
        Unreliable::Outcome(Feedback::Ok)
    }
}

impl<P> LimitedSender for NullSender<P>
where
    P: commonware_cryptography::PublicKey,
{
    type PublicKey = P;
    type Checked<'a>
        = NullCheckedSender<P>
    where
        Self: 'a;

    fn check(
        &mut self,
        recipients: Recipients<Self::PublicKey>,
    ) -> Result<Self::Checked<'_>, SystemTime> {
        let recipients = match recipients {
            Recipients::All => self.participants.clone(),
            Recipients::Some(recipients) => recipients,
            Recipients::One(recipient) => vec![recipient],
        };
        Ok(NullCheckedSender {
            recipients,
            send_notifications: self.send_notifications.clone(),
        })
    }
}

#[derive(Debug)]
struct NullReceiver<P>(PhantomData<P>);

impl<P> Receiver for NullReceiver<P>
where
    P: commonware_cryptography::PublicKey,
{
    type Error = Infallible;
    type PublicKey = P;

    async fn recv(&mut self) -> Result<Message<Self::PublicKey>, Self::Error> {
        std::future::pending().await
    }
}

#[derive(Clone)]
struct NullBlocker<P>(PhantomData<P>);

impl<P> Blocker for NullBlocker<P>
where
    P: commonware_cryptography::PublicKey,
{
    type PublicKey = P;

    fn block(&mut self, _peer: Self::PublicKey) -> Feedback {
        Feedback::Ok
    }
    fn blocked(&mut self) -> commonware_p2p::BlockedSubscription<Self::PublicKey> {
        let (_, receiver) = commonware_utils::channel::ring::channel(commonware_utils::NZUsize!(1));
        receiver
    }
}

#[test]
fn global_stop_reopens_voter_journal_and_resumes_without_conflicting_votes() {
    let storage = tempfile::tempdir().expect("shutdown test storage");
    let epoch = Epoch::new(1);
    let signing_key = PrivateKey::from_seed(7);
    let public_key = signing_key.public_key();
    let participants = Set::from_iter_dedup([public_key.clone()]);
    let dkg = bootstrap_dkg(1).expect("single-validator DKG fixture");
    let scheme = HybridScheme::<MinSig>::signer(
        &crate::config::outbe_app_namespace(),
        participants.clone(),
        signing_key,
        dkg.polynomial,
        dkg.shares[0].clone(),
    )
    .expect("single-validator hybrid signer");
    let mut recorded = Vec::<RecordedVote>::new();
    for _ in 0..2 {
        let previous_view = recorded.iter().map(|vote| vote.view()).max();
        let config = tokio::Config::default()
            .with_worker_threads(1)
            .with_max_blocking_threads(1)
            .with_catch_panics(true)
            .with_storage_directory(storage.path());
        let public_key = public_key.clone();
        let scheme = scheme.clone();
        let votes = tokio::Runner::new(config).start(|context| async move {
            let sender = NullSender {
                participants: vec![public_key.clone()],
                send_notifications: None,
            };
            let (votes_tx, votes_rx) = mpsc::channel();
            let vote_network = (
                NullSender {
                    send_notifications: Some(votes_tx),
                    ..sender.clone()
                },
                NullReceiver(PhantomData),
            );
            let certificate_network = (sender.clone(), NullReceiver(PhantomData));
            let resolver_network = (sender, NullReceiver(PhantomData));

            let reporter = MockReporter::new();
            let engine_config = SimplexConfig {
                scheme,
                elector: RoundRobin::<Sha256>::default(),
                blocker: NullBlocker(PhantomData),
                automaton: MockAutomaton::new(public_key),
                relay: MockRelay::new(),
                reporter,
                strategy: Sequential,
                forward: ForwardPolicy::Disabled,
                partition: "shutdown_voter_journal".to_owned(),
                epoch,
                floor: Floor::Genesis(mock_genesis(epoch)),
                mailbox_size: NZUsize!(64),
                leader_timeout: Duration::from_millis(10),
                certification_timeout: Duration::from_millis(20),
                timeout_retry: Duration::from_millis(40),
                view_retention: ViewDelta::new(16),
                skip: commonware_consensus::simplex::SkipPolicy::Disabled,
                track_historical_votes: true,
                fetch_timeout: Duration::from_millis(20),

                replay_buffer: NZUsize!(64 * 1024),
                write_buffer: NZUsize!(4 * 1024),
                page_cache: CacheRef::from_pooler(&context, PAGE_SIZE, PAGE_CACHE_SIZE),
            };
            let engine = Engine::new(context.child("engine"), engine_config);
            let mut engine_handle =
                engine.start(vote_network, certificate_network, resolver_network);

            // Receiving a vote observes the production sync-before-broadcast path.
            // On restart require progress beyond the previously observed view.
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut votes = Vec::new();
            loop {
                let vote = votes_rx
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("journal restart must resume outbound voting");
                let advanced = previous_view.map_or(vote.view() >= View::new(3), |previous| {
                    vote.view() > previous
                });
                if let Some(previous) = previous_view {
                    assert!(
                        vote.view() >= previous,
                        "restart regressed below the last durable outbound view"
                    );
                }
                votes.push(vote);
                if advanced {
                    break;
                }
            }
            let (started_tx, started_rx) = mpsc::sync_channel(1);
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let blocking = context
                .child("blocking_gate")
                .shared(true)
                .spawn(move |_| async move {
                    started_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(Duration::from_secs(3))
                        .expect("release blocking worker");
                });
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            let stop_handle = context.child("shutdown").spawn(|shutdown| async move {
                shutdown.stop(0, Some(Duration::from_secs(2))).await
            });
            // Completion may precede the final sync in unmodified upstream. Release
            // the I/O worker independently and observe both terminal results.
            let (engine_result, ()) = futures::join!(
                ::tokio::time::timeout(Duration::from_secs(3), &mut engine_handle),
                async {
                    context.sleep(Duration::from_millis(50)).await;
                    release_tx.send(()).unwrap();
                }
            );
            blocking.await.expect("blocking worker completes");
            engine_result
                .expect("Simplex engine stops within deadline")
                .expect("Simplex engine stops successfully");
            stop_handle
                .await
                .expect("shutdown task completes")
                .expect("global stop completes");
            votes.extend(votes_rx.try_iter());
            votes
        });
        for vote in &votes {
            for previous in recorded
                .iter()
                .chain(votes.iter())
                .filter(|other| other.view() == vote.view())
            {
                match (previous, vote) {
                    (Vote::Notarize(a), Vote::Notarize(b)) => assert_eq!(a.proposal, b.proposal),
                    (Vote::Finalize(a), Vote::Finalize(b)) => assert_eq!(a.proposal, b.proposal),
                    (Vote::Notarize(a), Vote::Finalize(b)) => assert_eq!(a.proposal, b.proposal),
                    (Vote::Finalize(a), Vote::Notarize(b)) => assert_eq!(a.proposal, b.proposal),
                    (Vote::Finalize(_), Vote::Nullify(_))
                    | (Vote::Nullify(_), Vote::Finalize(_)) => {
                        panic!("conflicting finalize/nullify votes across journal restart");
                    }
                    _ => {}
                }
            }
        }
        recorded.extend(votes);
    }
}

#[test]
fn planned_abort_during_pending_sync_reopens_the_same_voter_journal() {
    let storage = tempfile::tempdir().expect("planned replacement storage");
    let config = tokio::Config::default()
        .with_worker_threads(1)
        .with_max_blocking_threads(1)
        .with_catch_panics(true)
        .with_storage_directory(storage.path());
    let runner = tokio::Runner::new(config);

    runner.start(|context| async move {
        let epoch = Epoch::new(1);
        let signing_key = PrivateKey::from_seed(11);
        let public_key = signing_key.public_key();
        let participants = Set::from_iter_dedup([public_key.clone()]);
        let dkg = bootstrap_dkg(1).expect("single-validator DKG fixture");
        let scheme = HybridScheme::<MinSig>::signer(
            &crate::config::outbe_app_namespace(),
            participants.clone(),
            signing_key,
            dkg.polynomial,
            dkg.shares[0].clone(),
        )
        .expect("single-validator hybrid signer");
        let partition = "planned_abort_voter_journal";

        let engine_config = |context: &commonware_runtime::tokio::Context| SimplexConfig {
            scheme: scheme.clone(),
            elector: RoundRobin::<Sha256>::default(),
            blocker: NullBlocker(PhantomData),
            automaton: MockAutomaton::new(public_key.clone()),
            relay: MockRelay::new(),
            reporter: MockReporter::new(),
            strategy: Sequential,
            forward: ForwardPolicy::Disabled,
            partition: partition.to_owned(),
            epoch,
            floor: Floor::Genesis(mock_genesis(epoch)),
            mailbox_size: NZUsize!(64),
            // Leave enough time for the journal to open before the test
            // occupies the sole blocking worker. The first-attempt timeout
            // vote then deterministically queues its required sync behind the
            // gate instead of racing journal initialization.
            leader_timeout: Duration::from_millis(200),
            certification_timeout: Duration::from_millis(400),
            timeout_retry: Duration::from_millis(800),
            view_retention: ViewDelta::new(16),
            skip: commonware_consensus::simplex::SkipPolicy::Disabled,
            track_historical_votes: true,
            fetch_timeout: Duration::from_millis(20),

            replay_buffer: NZUsize!(64 * 1024),
            write_buffer: NZUsize!(4 * 1024),
            page_cache: CacheRef::from_pooler(context, PAGE_SIZE, PAGE_CACHE_SIZE),
        };
        let (durable_vote_tx, durable_vote_rx) = mpsc::channel();
        let networks = || {
            let vote_sender = NullSender {
                participants: vec![public_key.clone()],
                send_notifications: Some(durable_vote_tx.clone()),
            };
            let other_sender = NullSender {
                participants: vec![public_key.clone()],
                send_notifications: None,
            };
            (
                (vote_sender, NullReceiver(PhantomData)),
                (other_sender.clone(), NullReceiver(PhantomData)),
                (other_sender, NullReceiver(PhantomData)),
            )
        };

        let first_context = context.child("engine_before_planned_abort");
        let engine = Engine::new(first_context, engine_config(&context));
        let (vote, certificate, resolver) = networks();
        let first_handle = engine.start(vote, certificate, resolver);
        durable_vote_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first outbound vote proves the journal header and vote are durable");

        // Hold the only blocking worker while the voter reaches another
        // journal sync. This is the storage schedule under which a planned
        // role/epoch replacement currently hard-aborts the engine tree.
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let _blocking_handle = context
            .child("planned_abort_blocking_gate")
            .shared(true)
            .spawn(move |_| async move {
                started_tx.send(()).expect("report blocking worker start");
                let _ = release_rx.recv();
            });
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("sole blocking worker must be occupied");
        context.sleep(Duration::from_millis(250)).await;

        first_handle.abort();
        first_handle
            .await
            .expect_err("planned hard abort must cancel the old engine root");
        release_tx.send(()).expect("release blocking worker");

        // Planned replacement reuses the same epoch journal partition. The
        // observable safety contract is that the replacement can replay and
        // continue; terminal drain is an implementation choice, not the test.
        for _ in durable_vote_rx.try_iter() {}
        let second_context = context.child("engine_after_planned_abort");
        let engine = Engine::new(second_context, engine_config(&context));
        let (vote, certificate, resolver) = networks();
        let second_handle = engine.start(vote, certificate, resolver);
        durable_vote_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("replacement must emit a durable vote after reopening the journal");

        let stop_handle = context
            .child("shutdown_replacement")
            .spawn(|shutdown| async move { shutdown.stop(0, Some(Duration::from_secs(1))).await });
        second_handle
            .await
            .expect("replacement engine stops after reopening the journal");
        stop_handle
            .await
            .expect("shutdown driver must finish")
            .expect("global shutdown must complete");
    });
}
