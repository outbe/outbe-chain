use super::{
    proposal::{Permission, ProposalGate},
    sender::VoteSenderGate,
};
use crate::{
    digest::Digest,
    stack::epoch::signer::authority_signer_enabled,
    validators::{
        validator_status_can_vote, VALIDATOR_STATUS_ACTIVE, VALIDATOR_STATUS_EXITING,
        VALIDATOR_STATUS_PENDING,
    },
};
use alloy_primitives::B256;
use commonware_actor::{Feedback, Unreliable};
use commonware_consensus::{
    simplex::Plan,
    types::{Epoch, Round, View},
    Automaton, CertifiableAutomaton, Relay,
};
use commonware_cryptography::{bls12381, Signer as _};
use commonware_p2p::{CheckedSender, LimitedSender, Recipients, Sender as _};
use commonware_runtime::IoBufs;
use commonware_utils::channel::oneshot;
use outbe_radicle::integration::RadicleVotingGate;
use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState as State};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::SystemTime,
};

#[derive(Clone, Default)]
struct Application(Arc<AtomicUsize>);
impl Automaton for Application {
    type Context = ();
    type Digest = Digest;
    async fn propose(&mut self, _: ()) -> oneshot::Receiver<Digest> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Digest(B256::repeat_byte(9)));
        rx
    }
    async fn verify(&mut self, _: (), _: Digest) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(true);
        rx
    }
}
impl CertifiableAutomaton for Application {}
impl Relay for Application {
    type Digest = Digest;
    type PublicKey = bls12381::PublicKey;
    type Plan = Plan<Self::PublicKey>;
    fn broadcast(&mut self, _: Digest, _: Self::Plan) -> Feedback {
        self.0.fetch_add(1, Ordering::SeqCst);
        Feedback::Ok
    }
}
#[derive(Clone)]
struct CountingSender {
    peer: bls12381::PublicKey,
    count: Arc<AtomicUsize>,
}
impl LimitedSender for CountingSender {
    type PublicKey = bls12381::PublicKey;
    type Checked<'a> = Self;
    fn check(&mut self, _: Recipients<Self::PublicKey>) -> Result<Self::Checked<'_>, SystemTime> {
        Ok(self.clone())
    }
}
impl CheckedSender for CountingSender {
    type PublicKey = bls12381::PublicKey;
    fn recipients(&self) -> Vec<Self::PublicKey> {
        vec![self.peer.clone()]
    }
    fn send(self, _: impl Into<IoBufs> + Send, _: bool) -> Unreliable<Feedback> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Unreliable::Outcome(Feedback::Ok)
    }
}

#[tokio::test]
async fn canonical_role_matrix_gates_votes_certificates_and_proposals() {
    let states = [
        (
            State::ArmedBootstrap {
                enclave_id: B256::repeat_byte(1),
            },
            true,
        ),
        (
            State::Armed {
                enclave_id: B256::repeat_byte(1),
                valid_until: 100,
            },
            true,
        ),
        (State::SuspendedJailed, false),
        (State::SuspendedLeaseExpired, false),
        (State::SuspendedPolicyMismatch, false),
        (State::UnkeyedAwaitingAdmission, false),
    ];
    // Stable ABI ACTIVE/EXITING retain authority; PENDING (including readiness) and JAILED do not.
    let statuses = [
        (VALIDATOR_STATUS_ACTIVE, true),
        (VALIDATOR_STATUS_EXITING, true),
        (VALIDATOR_STATUS_PENDING, false),
        (crate::protocol_state::VALIDATOR_STATUS_JAILED, false),
    ];
    for (state, open) in states {
        for (status, eligible) in statuses {
            for (share, radicle) in [(false, false), (false, true), (true, false), (true, true)] {
                assert_role_case(
                    state,
                    status,
                    share,
                    radicle,
                    open && eligible && share && radicle,
                )
                .await;
            }
        }
    }
    assert!(!authority_signer_enabled(None, true, true, RadicleVotingGate::SignerAllowed).unwrap());
}

async fn assert_role_case(state: State, status: u8, share: bool, radicle: bool, expected: bool) {
    let channel = TeeAuthorityChannel::new(state);
    let watch = channel.subscribe();
    let gate = if radicle {
        RadicleVotingGate::SignerAllowed
    } else {
        RadicleVotingGate::Verifier
    };
    let permission: Permission = Arc::new(move || {
        authority_signer_enabled(Some(&watch), validator_status_can_vote(status), share, gate)
            .unwrap()
    });
    assert_eq!(
        permission(),
        expected,
        "status={status}, state={state:?}, share={share}, radicle={radicle}"
    );
    let count = Arc::new(AtomicUsize::new(0));
    let inner = CountingSender {
        peer: bls12381::PrivateKey::from_seed(1).public_key(),
        count: count.clone(),
    };
    for mut sender in [
        VoteSenderGate::new(inner.clone(), permission.clone()),
        VoteSenderGate::new(inner, permission.clone()),
    ] {
        assert_eq!(
            !sender.send(Recipients::All, vec![1], true).is_empty(),
            expected
        );
    }
    assert_eq!(count.load(Ordering::SeqCst), if expected { 2 } else { 0 });
    assert_proposals(permission, expected).await;
}

async fn assert_proposals(permission: Permission, expected: bool) {
    let round = Round::new(Epoch::new(1), View::new(2));
    let payload = Digest(B256::repeat_byte(9));
    let app = Application::default();
    let proposals = app.0.clone();
    let mut app = ProposalGate::new(app, permission);
    assert_eq!(app.propose(()).await.await.is_ok(), expected);
    assert_eq!(
        app.broadcast(payload, Plan::Propose { round }),
        if expected {
            Feedback::Ok
        } else {
            Feedback::Backoff
        }
    );
    assert_eq!(
        proposals.load(Ordering::SeqCst),
        if expected { 2 } else { 0 }
    );
    assert!(app.verify((), payload).await.await.unwrap());
    assert!(app.certify(round, payload).await.await.unwrap());
}

#[test]
fn live_authority_and_cached_membership_gate_after_check_and_resume() {
    let channel = TeeAuthorityChannel::new(State::ArmedBootstrap {
        enclave_id: B256::repeat_byte(1),
    });
    let watch = channel.subscribe();
    let accountable = Arc::new(AtomicBool::new(true));
    let cached = accountable.clone();
    let permission: Permission = Arc::new(move || {
        authority_signer_enabled(
            Some(&watch),
            cached.load(Ordering::Acquire),
            true,
            RadicleVotingGate::SignerAllowed,
        )
        .unwrap()
    });
    let count = Arc::new(AtomicUsize::new(0));
    let inner = CountingSender {
        peer: bls12381::PrivateKey::from_seed(1).public_key(),
        count: count.clone(),
    };
    let mut sender = VoteSenderGate::new(inner, permission);
    let checked = sender.check(Recipients::All).unwrap();
    channel.publish(State::SuspendedJailed);
    assert!(checked.recipients().is_empty());
    assert!(!checked.send(vec![1], true).accepted());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    channel.publish(State::Armed {
        enclave_id: B256::repeat_byte(1),
        valid_until: 100,
    });
    assert_eq!(sender.send(Recipients::All, vec![2], true).len(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    let checked = sender.check(Recipients::All).unwrap();
    accountable.store(false, Ordering::Release);
    assert!(!checked.send(vec![3], true).accepted());
    assert!(sender.send(Recipients::All, vec![4], true).is_empty());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    accountable.store(true, Ordering::Release);
    assert_eq!(sender.send(Recipients::All, vec![5], true).len(), 1);
    assert_eq!(count.load(Ordering::SeqCst), 2);
}
