//! `OutbeReporter` fanout tests.
//!
//! Verifies that `OutbeReporter` (the Outbe-side branch of the
//! `Reporters::from((outbe, marshal))` fanout) admits `Activity::Certification(notarization)`
//! and persists it to the certified-parent proof store.
//! Marshal's mailbox drops `Activity::Certification` via its `_ => return;` arm
//! (monorepo `consensus/src/marshal/core/mailbox.rs:396-410`). Thus marshal would *not* do
//! the persistence that this test observes.
//!

use alloy_primitives::{address, Address};
use commonware_consensus::{
    simplex::types::{Activity, Notarization, Proposal, Subject},
    types::{Epoch, Round, View},
    Reporter as _,
};
use commonware_cryptography::{
    bls12381::{self, primitives::variant::MinSig},
    certificate::Scheme as _,
    Hasher as _, Sha256, Signer as _,
};
use commonware_parallel::Sequential;
use commonware_utils::{ordered::Set, TryCollect as _};
use futures::channel::mpsc;
use outbe_consensus::{
    bls::bootstrap_dkg,
    digest::Digest as OutbeDigest,
    finalization::{
        ingress::{Mailbox as FinalizationMailbox, Message as FinalizationMessage},
        parent_cert_store::{
            CertificationWitnessSink, CertifiedParentProofKey, CertifiedParentProofStore,
            FinalizedParentCertStore, CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
        },
    },
    hybrid::HybridScheme,
    reporter::{OutbeReporter, ReporterCommittee, ReporterContinuity, ReporterDependencies},
};
use outbe_primitives::consensus_metadata::ParentParticipationProof;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

fn test_participants(n: u8) -> (Vec<bls12381::PrivateKey>, Set<bls12381::PublicKey>) {
    let keys: Vec<bls12381::PrivateKey> = (0..n)
        .map(|i| bls12381::PrivateKey::from_seed((i + 1) as u64))
        .collect();
    let participants = keys
        .iter()
        .map(|sk| bls12381::PublicKey::from(sk.clone()))
        .try_collect()
        .unwrap();
    (keys, participants)
}

fn ordered_addresses() -> Vec<Address> {
    vec![
        address!("0x1111111111111111111111111111111111111111"),
        address!("0x2222222222222222222222222222222222222222"),
        address!("0x3333333333333333333333333333333333333333"),
    ]
}

type NotarizationFixture = (
    Notarization<HybridScheme<MinSig>, OutbeDigest>,
    HybridScheme<MinSig>,
    Set<bls12381::PublicKey>,
);

/// Build a valid Notarization and the exact verifier/material that produced it.
fn valid_notarization() -> NotarizationFixture {
    certificate_fixture(2, false)
}

fn certificate_fixture(view: u64, finalize: bool) -> NotarizationFixture {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();
    let schemes: Vec<HybridScheme<MinSig>> = outbe_consensus::test_harness::fixture_signer_schemes(
        b"reporter-test",
        &keys,
        &participants,
        outbe_consensus::test_harness::FixtureSignerSharing {
            polynomial: &dkg.polynomial,
            shares: &dkg.shares,
        },
    );
    let verifier =
        HybridScheme::<MinSig>::verifier(b"reporter-test", participants.clone(), dkg.polynomial)
            .unwrap();
    let payload = OutbeDigest::from(alloy_primitives::B256::from_slice(
        Sha256::hash(&[b"test-payload"]).as_ref(),
    ));
    let proposal = Proposal::new(
        Round::new(Epoch::new(0), View::new(view)),
        View::new(view - 1),
        payload,
    );
    let subject = if finalize {
        Subject::Finalize {
            proposal: &proposal,
        }
    } else {
        Subject::Notarize {
            proposal: &proposal,
        }
    };
    let attestations: Vec<_> = schemes
        .iter()
        .map(|scheme| scheme.sign::<OutbeDigest>(subject).unwrap())
        .collect();
    let certificate = verifier
        .assemble(
            commonware_utils::iter::NonEmpty::try_new(attestations.into_iter()).unwrap(),
            &Sequential,
        )
        .unwrap();
    (
        Notarization {
            proposal,
            certificate,
        },
        verifier,
        participants,
    )
}

fn build_reporter(
    witness_sink: Arc<dyn CertificationWitnessSink>,
    verifier: HybridScheme<MinSig>,
    participants: &Set<bls12381::PublicKey>,
) -> (OutbeReporter, mpsc::UnboundedReceiver<FinalizationMessage>) {
    // The reporter enqueues certified-notarization persistence to the
    // FinalizationActor mailbox. The test keeps the receiver to drain it.
    build_reporter_with_continuity(
        witness_sink,
        verifier,
        participants,
        ReporterContinuity::default(),
    )
}

fn build_reporter_with_continuity(
    witness_sink: Arc<dyn CertificationWitnessSink>,
    verifier: HybridScheme<MinSig>,
    _participants: &Set<bls12381::PublicKey>,
    continuity: ReporterContinuity,
) -> (OutbeReporter, mpsc::UnboundedReceiver<FinalizationMessage>) {
    let (tx, rx) = mpsc::unbounded::<FinalizationMessage>();
    // This test exercises only the certification fan-out, not finalize votes.
    // Thus a verify actor whose receiver is dropped immediately is sufficient.
    // Its `mailbox.verify` becomes a no-op.
    let (_verify_actor, verify_mailbox) =
        outbe_consensus::finalization::finalize_verify::FinalizeVerifyActor::new(
            outbe_consensus::hybrid::HybridSchemeProvider::new(),
            outbe_consensus::finalization::late_sig_store::shared(
                outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K,
            ),
        );
    let reporter = OutbeReporter::new(
        continuity,
        ReporterCommittee {
            validator_addresses: ordered_addresses(),
            verifier_scheme: verifier,
            epoch: Epoch::new(0),
        },
        ReporterDependencies {
            finalization_mailbox: FinalizationMailbox::from_sender(tx),
            bridge: None,
            witness_sink,
            finalize_verify_mailbox: verify_mailbox,
        },
    );
    (reporter, rx)
}

/// Apply the off-thread certified-notarization writes that the reporter
/// enqueued, exactly as the `FinalizationActor` would. A test can then assert on
/// the store after `report(Activity::Certification(..))`.
fn drain_certification_writes(
    rx: &mut mpsc::UnboundedReceiver<FinalizationMessage>,
    store: &FinalizedParentCertStore,
) {
    while let Ok(msg) = rx.try_recv() {
        if let FinalizationMessage::CertifiedNotarization(record) = msg {
            store.put_certified_notarization(record.record).unwrap();
        }
    }
}

#[test]
fn finalization_after_large_view_gap_emits_empty_v2_attribution() {
    let (fixture, verifier, participants) = certificate_fixture(257, true);
    let continuity = ReporterContinuity::default();
    continuity.update(1, None, None);
    let store = FinalizedParentCertStore::new();
    let (mut reporter, mut rx) = build_reporter_with_continuity(
        Arc::new(store),
        verifier,
        &participants,
        continuity.clone(),
    );
    let feedback = reporter.report(Activity::Finalization(
        commonware_consensus::simplex::types::Finalization {
            proposal: fixture.proposal,
            certificate: fixture.certificate,
        },
    ));
    assert!(matches!(feedback, commonware_actor::Feedback::Ok));
    let message = rx.try_recv().unwrap();
    let FinalizationMessage::Finalized(finalized) = message else {
        panic!("expected finalization");
    };
    assert!(
        finalized.consensus_data.missed_proposers.is_empty(),
        "V2 has no missed-leader attribution consumer"
    );
    assert_eq!(continuity.snapshot().last_finalized_view, 257);
}

#[test]
fn certification_retention_uses_round_and_ignores_chain_height() {
    let store = FinalizedParentCertStore::new();
    let (notarization, verifier, participants) = valid_notarization();
    let key = CertifiedParentProofKey::new(0, 2, notarization.proposal.payload.0);
    let (mut reporter, mut rx) = build_reporter(Arc::new(store.clone()), verifier, &participants);
    let _ = reporter.report(Activity::Certification(notarization));
    drain_certification_writes(&mut rx, &store);
    store.prune_below_height(10_000).unwrap();
    assert!(store.get_certified_notarization(key).is_some());
    store
        .prune_certified_notarizations_below_round(Round::new(Epoch::new(0), View::new(3)))
        .unwrap();
    assert!(store.get_certified_notarization(key).is_none());
    assert!(!store.has_local_certification_witness(key));
}

#[tokio::test(flavor = "current_thread")]
async fn reporter_fanout_persists_certification_activity_before_marshal_filter() {
    let store = FinalizedParentCertStore::new();
    let (notarization, verifier, participants) = valid_notarization();
    let (mut reporter, mut rx) = build_reporter(Arc::new(store.clone()), verifier, &participants);
    let parent_hash = notarization.proposal.payload.0;
    let proof_key = CertifiedParentProofKey::new(0, 2, parent_hash);

    let _ = reporter.report(Activity::Certification(notarization.clone()));
    drain_certification_writes(&mut rx, &store);

    let record = store
        .get_certified_notarization(proof_key)
        .expect("Activity::Certification must persist a certified-parent proof record");
    assert_eq!(
        record.format_version,
        CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION
    );
    assert_eq!(
        record.proof_kind(),
        ParentParticipationProof::CertifiedNotarization
    );
    assert_eq!(record.finalized_epoch, 0);
    assert_eq!(record.finalized_view, 2);
    assert_eq!(record.parent_view, 1);
    assert_eq!(record.finalized_block_hash, parent_hash);
    // An Activity-driven insert always sets the local certification
    // witness flag. Remote-fetch fallbacks gate writes on
    // this flag being true.
    assert!(record.is_certification_witness());
    // The store accepted only the certified-notarization slot. The finalization
    // slot is untouched.
    assert!(store.get_finalization(proof_key).is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn proof_store_ingestion_verifies_certification_activity_before_write() {
    // Build a structurally valid notarization, then tamper the proposal so the
    // certificate signature no longer matches the proposal subject. The
    // reporter must drop the activity without writing.
    let (mut notarization, verifier, participants) = valid_notarization();
    // Swap the proposal payload. The cert was signed for the original
    // payload. The verifier will reject the post-mutation Notarization.
    let original_hash = notarization.proposal.payload.0;
    notarization.proposal.payload = OutbeDigest::from(alloy_primitives::B256::from_slice(
        Sha256::hash(&[b"tampered-payload"]).as_ref(),
    ));
    let tampered_hash = notarization.proposal.payload.0;
    assert_ne!(original_hash, tampered_hash);

    let store = FinalizedParentCertStore::new();
    let (mut reporter, mut rx) = build_reporter(Arc::new(store.clone()), verifier, &participants);

    let _ = reporter.report(Activity::Certification(notarization));
    // A verify failure drops on-thread before enqueue, so draining finds
    // nothing. The test still applies any writes, so the "nothing persisted"
    // assertion is exact even if behavior regresses.
    drain_certification_writes(&mut rx, &store);

    // Verify-before-write contract: no record persisted on signature mismatch.
    assert!(
        store
            .get_certified_notarization(CertifiedParentProofKey::new(0, 2, tampered_hash))
            .is_none(),
        "tampered Activity::Certification must be rejected before persistence"
    );
    assert!(
        store
            .get_certified_notarization(CertifiedParentProofKey::new(0, 2, original_hash))
            .is_none(),
        "tampered Activity::Certification must not be persisted under the original hash either"
    );
    assert_eq!(store.len(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn reporter_handle_certification_records_witness_flag_true() {
    let store = FinalizedParentCertStore::new();
    let (notarization, verifier, participants) = valid_notarization();
    let (mut reporter, mut rx) = build_reporter(Arc::new(store.clone()), verifier, &participants);
    let parent_hash = notarization.proposal.payload.0;
    let _ = reporter.report(Activity::Certification(notarization));
    drain_certification_writes(&mut rx, &store);
    let record = store
        .get_certified_notarization(CertifiedParentProofKey::new(0, 2, parent_hash))
        .unwrap();
    assert!(
        record.is_certification_witness(),
        "Activity-driven inserts must always set local_certification_witness=true"
    );
}

/// A fake `CertificationWitnessSink` that records every mark. It exercises the narrow
/// capability seam that the reporter receives (instead of the full store). This is the
/// reason the seam is a trait rather than a newtype.
#[derive(Default)]
struct CountingWitnessSink {
    count: AtomicUsize,
    keys: Mutex<Vec<CertifiedParentProofKey>>,
}

impl CertificationWitnessSink for CountingWitnessSink {
    fn mark_local_certification_witness(&self, key: CertifiedParentProofKey) {
        self.count.fetch_add(1, Ordering::SeqCst);
        self.keys.lock().unwrap().push(key);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn reporter_marks_witness_through_narrow_sink() {
    // The reporter's only capability onto the proof store is the
    // `CertificationWitnessSink`. A valid `Activity::Certification` must drive
    // exactly one witness mark for the `(epoch, view, parent)` it observed.
    // This proves that the narrow seam is the path. It also proves that the
    // reporter is mockable without a real store.
    let sink = Arc::new(CountingWitnessSink::default());
    let (notarization, verifier, participants) = valid_notarization();
    let (mut reporter, mut rx) = build_reporter(sink.clone(), verifier, &participants);
    let parent_hash = notarization.proposal.payload.0;

    let _ = reporter.report(Activity::Certification(notarization));
    // Drain the off-thread persistence enqueue so no sender is left dangling.
    while rx.try_recv().is_ok() {}

    assert_eq!(
        sink.count.load(Ordering::SeqCst),
        1,
        "a valid Activity::Certification must mark the witness exactly once"
    );
    let keys = sink.keys.lock().unwrap();
    assert_eq!(keys.len(), 1);
    assert!(
        keys[0] == CertifiedParentProofKey::new(0, 2, parent_hash),
        "marked key must match the observed (epoch=0, view=2, parent)"
    );
}
