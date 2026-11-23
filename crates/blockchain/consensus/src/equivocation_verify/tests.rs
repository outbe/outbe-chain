use super::*;
use crate::{bls::bootstrap_dkg, test_fixtures::participants};
use commonware_consensus::{
    simplex::types::{ConflictingFinalize, ConflictingNotarize, NullifyFinalize},
    types::View,
};
use metrics_util::debugging::{DebugValue, DebuggingRecorder};

struct Fixture {
    signer: HybridScheme<MinSig>,
    verifier: Arc<HybridScheme<MinSig>>,
}

impl Fixture {
    fn new() -> Self {
        let (keys, participants) = participants();
        let dkg = bootstrap_dkg(3).unwrap();
        let key = keys[0].clone();
        let index = participants
            .index(&bls12381::PublicKey::from(key.clone()))
            .unwrap()
            .get() as usize;
        Self {
            signer: HybridScheme::signer(
                b"equivocation",
                participants.clone(),
                key,
                dkg.polynomial.clone(),
                dkg.shares[index].clone(),
            )
            .unwrap(),
            verifier: Arc::new(
                HybridScheme::verifier(b"equivocation", participants, dkg.polynomial).unwrap(),
            ),
        }
    }

    fn pair(
        &self,
        kind: EquivocationKind,
    ) -> (RawEquivocationEvidence, VerifiedEquivocationEvidence) {
        let first = proposal(300, 1);
        let second = proposal(300, 2);
        match kind {
            EquivocationKind::ConflictingNotarize => {
                let a = Notarize::sign(&self.signer, first).unwrap();
                let b = Notarize::sign(&self.signer, second).unwrap();
                self.notarize_pair(a, b)
            }
            EquivocationKind::ConflictingFinalize => {
                let a = Finalize::sign(&self.signer, first).unwrap();
                let b = Finalize::sign(&self.signer, second).unwrap();
                let expected = self.expected(
                    kind,
                    &a.attestation,
                    &a.proposal.encode(),
                    &b.attestation,
                    &b.proposal.encode(),
                );
                let conflict = ConflictingFinalize::new(a, b);
                (
                    RawEquivocationEvidence::new(kind, &conflict, self.verifier.clone()),
                    expected,
                )
            }
            EquivocationKind::NullifyFinalize => {
                let a = Nullify::sign::<Digest>(&self.signer, first.round).unwrap();
                let b = Finalize::sign(&self.signer, second).unwrap();
                let expected = self.expected(
                    kind,
                    &a.attestation,
                    &a.round.encode(),
                    &b.attestation,
                    &b.proposal.encode(),
                );
                let conflict = NullifyFinalize::new(a, b);
                (
                    RawEquivocationEvidence::new(kind, &conflict, self.verifier.clone()),
                    expected,
                )
            }
        }
    }

    fn notarize_pair(
        &self,
        a: Notarize<HybridScheme<MinSig>, Digest>,
        b: Notarize<HybridScheme<MinSig>, Digest>,
    ) -> (RawEquivocationEvidence, VerifiedEquivocationEvidence) {
        let kind = EquivocationKind::ConflictingNotarize;
        let expected = self.expected(
            kind,
            &a.attestation,
            &a.proposal.encode(),
            &b.attestation,
            &b.proposal.encode(),
        );
        let conflict = ConflictingNotarize::new(a, b);
        (
            RawEquivocationEvidence::new(kind, &conflict, self.verifier.clone()),
            expected,
        )
    }

    fn expected(
        &self,
        kind: EquivocationKind,
        first: &Attestation<HybridScheme<MinSig>>,
        first_payload: &[u8],
        second: &Attestation<HybridScheme<MinSig>>,
        second_payload: &[u8],
    ) -> VerifiedEquivocationEvidence {
        let pubkey = self
            .verifier
            .participants()
            .key(first.signer)
            .unwrap()
            .encode();
        // Assemble independently of the production packing helper.
        let block = |att: &Attestation<HybridScheme<MinSig>>, payload: &[u8]| {
            let sig = att.signature.get().unwrap().bls_individual_vote.encode();
            [pubkey.as_ref(), sig.as_ref(), payload].concat()
        };
        let first_block = block(first, first_payload);
        let second_block = block(second, second_payload);
        VerifiedEquivocationEvidence {
            kind,
            epoch: 1,
            accused_pubkey: pubkey.as_ref().try_into().unwrap(),
            evidence_hash: canonical_equivocation_evidence_hash(&first_block, &second_block),
            first_block,
            second_block,
        }
    }
}

fn proposal(view: u64, hash: u8) -> Proposal<Digest> {
    Proposal::new(
        Round::new(Epoch::new(1), View::new(view)),
        View::new(0),
        Digest(B256::repeat_byte(hash)),
    )
}

pub(crate) fn fixture() -> (RawEquivocationEvidence, VerifiedEquivocationEvidence) {
    Fixture::new().pair(EquivocationKind::ConflictingNotarize)
}

fn assert_delivered(
    bridge: &EquivocationEvidenceMailbox,
    mut expected: VerifiedEquivocationEvidence,
) {
    // Both coalescing-key components differ: only the exact hash can match.
    expected.epoch += 1;
    expected.accused_pubkey = [0xEE; 48];
    assert_eq!(bridge.push(expected), EquivocationPushOutcome::Duplicate);
}

fn assert_counter(recorder: &DebuggingRecorder, name: &str, labels: &[(&str, &str)], count: u64) {
    let snapshot = recorder.snapshotter().snapshot().into_vec();
    let entry = snapshot
        .iter()
        .find(|(key, _, _, _)| {
            key.key().name() == name
                && labels.iter().all(|(k, v)| {
                    key.key()
                        .labels()
                        .any(|label| label.key() == *k && label.value() == *v)
                })
        })
        .unwrap();
    assert_eq!(entry.3, DebugValue::Counter(count));
}

#[tokio::test]
async fn all_classes_pack_exact_individual_votes_and_canonical_namespaces() {
    let fixture = Fixture::new();
    let committee = fixture.verifier.participants();
    let notarize = outbe_primitives::proof::notarize_namespace(committee);
    let finalize = outbe_primitives::proof::finalize_namespace(committee);
    let nullify = outbe_primitives::proof::nullify_namespace(committee);
    for kind in [
        EquivocationKind::ConflictingNotarize,
        EquivocationKind::ConflictingFinalize,
        EquivocationKind::NullifyFinalize,
    ] {
        let (raw, expected) = fixture.pair(kind);
        let actual = verify(&raw).unwrap();
        assert_eq!(actual.kind, kind);
        assert_eq!(actual.epoch, 1);
        assert_eq!(actual.accused_pubkey, expected.accused_pubkey);
        assert_eq!(actual.first_block, expected.first_block);
        assert_eq!(actual.second_block, expected.second_block);
        assert_eq!(actual.evidence_hash, expected.evidence_hash);
        unpack_verified_equivocation_evidence(&actual).unwrap();
        for (first, block, hash) in [
            (true, &actual.first_block, 1),
            (false, &actual.second_block, 2),
        ] {
            let is_nullify = kind == EquivocationKind::NullifyFinalize && first;
            // epoch=1, view=300 uses the two-byte LEB128 view, parent=0.
            let mut payload = vec![1, 0xAC, 2];
            if !is_nullify {
                payload.push(0);
                payload.extend_from_slice(&[hash; 32]);
                assert_eq!(payload, proposal(300, hash).encode().as_ref());
            } else {
                assert_eq!(payload, proposal(300, hash).round.encode().as_ref());
            }
            assert_eq!(&block[..48], expected.accused_pubkey);
            assert_eq!(block.len(), 144 + payload.len());
            assert_eq!(&block[144..], payload);
            let pk =
                bls12381::PublicKey::read(&mut commonware_codec::Copying(&block[..48])).unwrap();
            let sig =
                bls12381::Signature::read(&mut commonware_codec::Copying(&block[48..144])).unwrap();
            let (namespace, wrong) = match kind {
                EquivocationKind::ConflictingNotarize => (&notarize, &finalize),
                EquivocationKind::NullifyFinalize if first => (&nullify, &finalize),
                EquivocationKind::ConflictingFinalize | EquivocationKind::NullifyFinalize => {
                    (&finalize, &nullify)
                }
            };
            assert!(pk.verify(namespace, &payload, &sig));
            assert!(!pk.verify(wrong, &payload, &sig));
        }
        let bridge = EquivocationEvidenceMailbox::new();
        let (actor, mailbox) = EquivocationVerifyActor::new(bridge.clone());
        mailbox.enqueue(raw);
        drop(mailbox);
        actor.run().await;
        assert_delivered(&bridge, expected);
    }
}

#[tokio::test]
async fn per_signer_eviction_admits_valid_sixth_pair() {
    let fixture = Fixture::new();
    let bridge = EquivocationEvidenceMailbox::new();
    let (actor, mailbox) = EquivocationVerifyActor::new(bridge.clone());
    let mut invalid = Vec::new();
    for hash in 10..15 {
        let mut a = Notarize::sign(&fixture.signer, proposal(300, hash)).unwrap();
        let b = Notarize::sign(&fixture.signer, proposal(300, hash + 1)).unwrap();
        let mut signature = a.attestation.signature.get().cloned().unwrap();
        signature.bls_individual_vote = b
            .attestation
            .signature
            .get()
            .unwrap()
            .bls_individual_vote
            .clone();
        a.attestation.signature = signature.into();
        let (raw, _) = fixture.notarize_pair(a, b);
        assert!(matches!(verify(&raw), Err(VerifyError::InvalidSignature)));
        invalid.push(raw);
    }
    let recorder = DebuggingRecorder::new();
    metrics::with_local_recorder(&recorder, || {
        for raw in &invalid {
            mailbox.enqueue(raw.clone());
        }
        // Inspect pending entries before the actor can run, then restore their FIFO order.
        let mut admission = mailbox.admission.lock().unwrap();
        let pending: Vec<_> = std::iter::from_fn(|| admission.pop()).collect();
        assert_eq!(pending.len(), 4);
        for (queued, original) in pending.iter().zip(&invalid[1..]) {
            assert_eq!(queued.raw.bytes, original.bytes);
        }
        for queued in pending {
            assert!(admission.push(queued));
        }
    });
    let (raw, expected) = fixture.pair(EquivocationKind::ConflictingNotarize);
    assert_eq!(raw.key(), invalid[0].key());
    metrics::with_local_recorder(&recorder, || mailbox.enqueue(raw));
    assert_counter(
        &recorder,
        "outbe_consensus_equivocation_verify_admission_total",
        &[("result", "evicted_per_signer")],
        2,
    );
    drop(mailbox);
    actor.run().await;
    assert_delivered(&bridge, expected);
}

#[tokio::test]
async fn unrelated_seed_partial_does_not_change_slash_relevant_evidence() {
    let fixture = Fixture::new();
    let mut a = Notarize::sign(&fixture.signer, proposal(300, 1)).unwrap();
    let b = Notarize::sign(&fixture.signer, proposal(300, 2)).unwrap();
    let (_, expected) = fixture.notarize_pair(a.clone(), b.clone());
    let foreign = Notarize::sign(&fixture.signer, proposal(301, 1)).unwrap();
    let mut signature = a.attestation.signature.get().cloned().unwrap();
    let other = foreign
        .attestation
        .signature
        .get()
        .unwrap()
        .bls_seed_partial;
    assert_ne!(signature.bls_seed_partial, other);
    signature.bls_seed_partial = other;
    a.attestation.signature = signature.into();
    let (raw, _) = fixture.notarize_pair(a, b);
    let actual = verify(&raw).unwrap();
    assert_eq!(actual.first_block, expected.first_block);
    assert_eq!(actual.second_block, expected.second_block);
    unpack_verified_equivocation_evidence(&actual).unwrap();
    let bridge = EquivocationEvidenceMailbox::new();
    let (actor, mailbox) = EquivocationVerifyActor::new(bridge.clone());
    mailbox.enqueue(raw);
    drop(mailbox);
    actor.run().await;
    assert_delivered(&bridge, expected);
}

#[test]
fn full_bridge_does_not_remember_hash_and_retry_delivers() {
    let (raw, expected) = fixture();
    let full = EquivocationEvidenceMailbox::new();
    for i in 0..64 {
        let mut entry = expected.clone();
        entry.epoch = 1000 + u64::from(i);
        entry.accused_pubkey = [i; 48];
        entry.evidence_hash = B256::repeat_byte(i + 1);
        assert_ne!(entry.evidence_hash, expected.evidence_hash);
        assert_eq!(full.push(entry), EquivocationPushOutcome::Enqueued);
    }
    let (mut actor, _mailbox) = EquivocationVerifyActor::new(full);
    let recorder = DebuggingRecorder::new();
    metrics::with_local_recorder(&recorder, || actor.process(raw.clone()));
    assert_counter(
        &recorder,
        "outbe_consensus_equivocation_verify_total",
        &[("kind", raw.kind.label()), ("result", "bridge_full")],
        1,
    );
    assert!(actor.verified.is_empty());
    actor.bridge = EquivocationEvidenceMailbox::new();
    metrics::with_local_recorder(&recorder, || actor.process(raw));
    assert_eq!(actor.verified.back(), Some(&expected.evidence_hash));
    assert_counter(
        &recorder,
        "outbe_consensus_equivocation_verify_total",
        &[("kind", expected.kind.label()), ("result", "verified")],
        1,
    );
    assert_delivered(&actor.bridge, expected);
}
