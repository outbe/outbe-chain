use super::*;
use crate::bls::bootstrap_dkg;
use commonware_consensus::{
    simplex::types::{Proposal, Subject},
    types::{Epoch, View},
};
use commonware_cryptography::certificate::Verifier as _;
use commonware_cryptography::{
    bls12381::primitives::variant::MinSig, certificate::Scheme as _,
    sha256::Digest as Sha256Digest, Hasher, Sha256,
};
use commonware_parallel::Sequential;
use rand_core_commonware::{TryCryptoRng, TryRng};

use super::test_support::{signer_schemes, test_participants, TestScheme, NAMESPACE};

fn sample_proposal(epoch: Epoch, view: View, tag: u8) -> Proposal<Sha256Digest> {
    Proposal::new(
        Round::new(epoch, view),
        view.previous().unwrap(),
        Sha256::hash(&[&[tag]]),
    )
}

struct ZeroRng;

impl TryRng for ZeroRng {
    type Error = core::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok(0)
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        Ok(0)
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Self::Error> {
        dest.fill(0);
        Ok(())
    }
}

impl TryCryptoRng for ZeroRng {}

#[test]
fn test_hybrid_sign_and_verify_attestation() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    let scheme0 = HybridScheme::<MinSig>::signer(
        NAMESPACE,
        participants.clone(),
        keys[0].clone(),
        dkg.polynomial.clone(),
        dkg.shares[participants
            .index(&bls12381::PublicKey::from(keys[0].clone()))
            .unwrap()
            .get() as usize]
            .clone(),
    );
    assert!(scheme0.is_some(), "signer scheme should be created");
    let scheme0 = scheme0.unwrap();

    let verifier =
        HybridScheme::<MinSig>::verifier(NAMESPACE, participants.clone(), dkg.polynomial.clone())
            .unwrap();

    let epoch = Epoch::new(1);
    let view = View::new(2);
    let proposal = sample_proposal(epoch, view, 42);
    let subject = Subject::Notarize {
        proposal: &proposal,
    };

    // Sign
    let attestation = scheme0.sign::<Sha256Digest>(subject);
    assert!(attestation.is_some());
    let attestation = attestation.unwrap();

    // Verify
    let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    assert!(verifier.verify_attestation(&mut rng, subject, &attestation, &Sequential));
}

#[test]
fn test_hybrid_assemble_and_verify_certificate() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    // Create signer schemes for all participants
    let schemes = signer_schemes(NAMESPACE, &keys, &participants, &dkg);

    let verifier =
        HybridScheme::<MinSig>::verifier(NAMESPACE, participants.clone(), dkg.polynomial.clone())
            .unwrap();

    let epoch = Epoch::new(1);
    let view = View::new(2);
    let proposal = sample_proposal(epoch, view, 42);
    let subject = Subject::Notarize {
        proposal: &proposal,
    };

    // Collect attestations from all signers
    let attestations: Vec<Attestation<TestScheme>> = schemes
        .iter()
        .map(|s| s.sign::<Sha256Digest>(subject).unwrap())
        .collect();

    // Assemble certificate
    let certificate = verifier
        .assemble(
            NonEmpty::try_new(attestations.into_iter()).expect("test attestations are non-empty"),
            &Sequential,
        )
        .ok();
    assert!(certificate.is_some(), "certificate assembly should succeed");
    let certificate = certificate.unwrap();

    // Verify certificate structure
    assert_eq!(certificate.signers.count(), 3);

    // Verify certificate
    let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    assert!(verifier.verify_certificate::<_, Sha256Digest>(
        &mut rng,
        subject,
        &certificate,
        &Sequential
    ));
}

#[test]
fn test_hybrid_is_attributable() {
    assert!(TestScheme::is_attributable());
}

#[test]
fn test_hybrid_is_batchable() {
    assert!(!TestScheme::is_batchable());
}

#[test]
fn test_assemble_rejects_duplicate_signer_without_panic() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    let schemes = signer_schemes(NAMESPACE, &keys, &participants, &dkg);

    let verifier =
        HybridScheme::<MinSig>::verifier(NAMESPACE, participants.clone(), dkg.polynomial.clone())
            .unwrap();

    let proposal = sample_proposal(Epoch::new(1), View::new(2), 42);
    let subject = Subject::Notarize {
        proposal: &proposal,
    };

    let duplicated = vec![
        schemes[0].sign::<Sha256Digest>(subject).unwrap(),
        schemes[0].sign::<Sha256Digest>(subject).unwrap(),
        schemes[1].sign::<Sha256Digest>(subject).unwrap(),
    ];

    assert!(
        verifier
            .assemble(
                NonEmpty::try_new(duplicated.into_iter()).expect("test attestations are non-empty"),
                &Sequential
            )
            .ok()
            .is_none(),
        "duplicate signer attestations must be rejected"
    );
}

#[test]
fn test_hybrid_bad_vote_attestation_rejected() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    let pk0 = bls12381::PublicKey::from(keys[0].clone());
    let idx0 = participants.index(&pk0).unwrap();
    let scheme0 = HybridScheme::<MinSig>::signer(
        NAMESPACE,
        participants.clone(),
        keys[0].clone(),
        dkg.polynomial.clone(),
        dkg.shares[idx0.get() as usize].clone(),
    )
    .unwrap();

    let verifier =
        HybridScheme::<MinSig>::verifier(NAMESPACE, participants.clone(), dkg.polynomial.clone())
            .unwrap();

    let epoch = Epoch::new(1);
    let view = View::new(2);
    let proposal = sample_proposal(epoch, view, 42);
    let subject = Subject::Notarize {
        proposal: &proposal,
    };

    let attestation = scheme0.sign::<Sha256Digest>(subject).unwrap();

    // Tamper with the BLS individual vote. Use a different key to produce a wrong sig.
    let wrong_key = bls12381::PrivateKey::from_seed(99);
    let wrong_sig = wrong_key.sign(b"wrong", b"wrong");
    let mut tampered_sig = attestation.signature.get().cloned().unwrap();
    tampered_sig.bls_individual_vote = wrong_sig;
    let tampered = Attestation {
        signer: attestation.signer,
        signature: commonware_codec::types::lazy::Lazy::from(tampered_sig),
    };

    let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    assert!(!verifier.verify_attestation(&mut rng, subject, &tampered, &Sequential));
}

#[test]
fn assemble_rejects_quorum_without_active_version_partials() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    let schemes = signer_schemes(NAMESPACE, &keys, &participants, &dkg);

    let verifier =
        HybridScheme::<MinSig>::verifier(NAMESPACE, participants.clone(), dkg.polynomial.clone())
            .unwrap();

    let proposal = sample_proposal(Epoch::new(1), View::new(2), 42);
    let subject = Subject::Notarize {
        proposal: &proposal,
    };

    let attestations: Vec<_> = schemes
        .iter()
        .map(|scheme| {
            let mut attestation = scheme.sign::<Sha256Digest>(subject).unwrap();
            let mut signature = attestation.signature.get().cloned().unwrap();
            signature.vrf_material_version = 999;
            attestation.signature = commonware_codec::types::lazy::Lazy::from(signature);
            attestation
        })
        .collect();

    let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    assert!(attestations
        .iter()
        .all(|attestation| verifier.verify_attestation(
            &mut rng,
            subject,
            attestation,
            &Sequential
        )));

    assert!(verifier
        .assemble(
            NonEmpty::try_new(attestations.into_iter()).expect("test attestations are non-empty"),
            &Sequential
        )
        .ok()
        .is_none());
}

#[test]
fn verify_certificate_rejects_wrong_vrf_material_version() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    let schemes = signer_schemes(NAMESPACE, &keys, &participants, &dkg);

    let verifier =
        HybridScheme::<MinSig>::verifier(NAMESPACE, participants.clone(), dkg.polynomial.clone())
            .unwrap();

    let proposal = sample_proposal(Epoch::new(1), View::new(2), 42);
    let subject = Subject::Notarize {
        proposal: &proposal,
    };
    let attestations: Vec<_> = schemes
        .iter()
        .map(|scheme| scheme.sign::<Sha256Digest>(subject).unwrap())
        .collect();
    let mut certificate = verifier
        .assemble(
            NonEmpty::try_new(attestations.into_iter()).expect("test attestations are non-empty"),
            &Sequential,
        )
        .unwrap();
    certificate.vrf_proof.material_version = 999;

    let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    assert!(!verifier.verify_certificate::<_, Sha256Digest>(
        &mut rng,
        subject,
        &certificate,
        &Sequential
    ));
    assert!(verifier
        .verified_vrf_seed_for_round(
            &mut rng,
            Round::new(Epoch::new(1), View::new(2)),
            &certificate,
            &Sequential
        )
        .is_none());
}

#[test]
fn verify_certificate_rejects_vrf_proof_for_a_different_subject() {
    let (schemes, verifier) = signers_and_verifier(4);
    let proposal = sample_proposal(Epoch::new(1), View::new(2), 42);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let mut certificate = verifier
        .assemble(
            NonEmpty::try_new(
                schemes
                    .iter()
                    .map(|scheme| scheme.sign::<Sha256Digest>(subject).unwrap()),
            )
            .expect("test attestations are non-empty"),
            &Sequential,
        )
        .unwrap();

    let other_proposal = sample_proposal(Epoch::new(1), View::new(3), 43);
    let other_subject = Subject::Finalize {
        proposal: &other_proposal,
    };
    certificate.vrf_proof = verifier
        .assemble(
            NonEmpty::try_new(
                schemes
                    .iter()
                    .map(|scheme| scheme.sign::<Sha256Digest>(other_subject).unwrap()),
            )
            .expect("test attestations are non-empty"),
            &Sequential,
        )
        .unwrap()
        .vrf_proof;

    assert!(!verifier.verify_certificate::<_, Sha256Digest>(
        &mut bls_batch_verification_rng(),
        subject,
        &certificate,
        &Sequential,
    ));
}

/// Build `n` signer schemes and a matching verifier from one DKG.
fn signers_and_verifier(n: u8) -> (Vec<TestScheme>, TestScheme) {
    let (_keys, signers, verifier) = signers_keys_and_verifier(n);
    (signers, verifier)
}

#[test]
fn epoch_scheme_keeps_material_version_after_provider_activation() {
    let (keys, participants) = test_participants(4);
    let dkg = bootstrap_dkg(4).unwrap();
    let public_key = bls12381::PublicKey::from(keys[0].clone());
    let index = participants.index(&public_key).unwrap();
    let share = dkg.shares[index.get() as usize].clone();
    let provider = VrfMaterialProvider::new(0, dkg.polynomial.clone(), Some(share.clone()));
    let scheme = HybridScheme::signer_with_vrf_provider(
        NAMESPACE,
        participants,
        keys[0].clone(),
        provider.clone(),
    )
    .unwrap();

    provider.activate(1, dkg.polynomial, Some(share));

    assert_eq!(
        scheme.expected_vrf_material_version(),
        0,
        "an epoch-scoped scheme must not follow later provider activation"
    );
}

#[test]
fn epoch_scheme_keeps_identity_and_signing_material_after_provider_activation() {
    let (keys, participants) = test_participants(4);
    let initial = bootstrap_dkg(4).unwrap();
    let replacement = bootstrap_dkg(4).unwrap();
    let public_key = bls12381::PublicKey::from(keys[0].clone());
    let index = participants.index(&public_key).unwrap();
    let initial_share = initial.shares[index.get() as usize].clone();
    let replacement_share = replacement.shares[index.get() as usize].clone();
    let expected_identity = *initial.polynomial.public();
    let provider = VrfMaterialProvider::new(0, initial.polynomial, Some(initial_share));
    let scheme = HybridScheme::signer_with_vrf_provider(
        NAMESPACE,
        participants,
        keys[0].clone(),
        provider.clone(),
    )
    .unwrap();

    provider.activate(1, replacement.polynomial, Some(replacement_share));

    assert_eq!(scheme.identity(), Some(expected_identity));
    let proposal = sample_proposal(Epoch::new(1), View::new(2), 42);
    let attestation = scheme
        .sign::<Sha256Digest>(Subject::Finalize {
            proposal: &proposal,
        })
        .unwrap();
    assert_eq!(
        attestation.signature.get().unwrap().vrf_material_version,
        0,
        "the old epoch signer must keep signing with its pinned material"
    );
}

#[test]
fn epoch_scheme_rejects_later_material_proof_retained_by_provider() {
    let (keys, participants) = test_participants(4);
    let initial = bootstrap_dkg(4).unwrap();
    let replacement = bootstrap_dkg(4).unwrap();
    let verifier_provider = VrfMaterialProvider::new(0, initial.polynomial, None);
    let old_verifier = HybridScheme::verifier_with_vrf_provider(
        NAMESPACE,
        participants.clone(),
        verifier_provider.clone(),
    )
    .unwrap();
    verifier_provider.activate(1, replacement.polynomial.clone(), None);

    let replacement_signers: Vec<TestScheme> = keys
        .iter()
        .map(|key| {
            let public_key = bls12381::PublicKey::from(key.clone());
            let index = participants.index(&public_key).unwrap();
            HybridScheme::signer_with_vrf_provider(
                NAMESPACE,
                participants.clone(),
                key.clone(),
                VrfMaterialProvider::new(
                    1,
                    replacement.polynomial.clone(),
                    Some(replacement.shares[index.get() as usize].clone()),
                ),
            )
            .unwrap()
        })
        .collect();
    let round = Round::new(Epoch::new(1), View::new(2));
    let proposal = sample_proposal(round.epoch(), round.view(), 42);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let certificate = replacement_signers[0]
        .assemble(
            NonEmpty::try_new(
                replacement_signers
                    .iter()
                    .map(|scheme| scheme.sign::<Sha256Digest>(subject).unwrap()),
            )
            .expect("test attestations are non-empty"),
            &Sequential,
        )
        .unwrap();
    assert_eq!(certificate.vrf_proof.material_version, 1);

    let mut rng = bls_batch_verification_rng();
    assert!(
        old_verifier
            .verified_vrf_seed_for_round(&mut rng, round, &certificate, &Sequential)
            .is_none(),
        "an epoch verifier must reject a proof from a later retained material version"
    );
}

/// Like [`signers_and_verifier`] but also returns the signer private keys
/// (from the same single DKG, so they match the schemes). Needed to craft
/// attestations with a hand-built identity signature.
fn signers_keys_and_verifier(n: u8) -> (Vec<bls12381::PrivateKey>, Vec<TestScheme>, TestScheme) {
    let (keys, participants) = test_participants(n);
    let dkg = bootstrap_dkg(n as u32).unwrap();
    let signers = signer_schemes(NAMESPACE, &keys, &participants, &dkg);
    let verifier =
        HybridScheme::<MinSig>::verifier(NAMESPACE, participants, dkg.polynomial).unwrap();
    (keys, signers, verifier)
}

/// Overwrite `attestation`'s `bls_seed_partial` with `replacement`'s,
/// keeping the active material version and the individual vote intact. This
/// is the byzantine partial of the C-02 attack: a well-formed value that
/// does not verify at this index. `replacement` must be a partial over a
/// *different* seed message (e.g. a different round) so the grafted value is
/// distinct from every honest partial. Recovery dedups by index but not by
/// value. A value equal to a sibling's would be a no-op at this index.
fn corrupt_seed_partial(
    attestation: &mut Attestation<TestScheme>,
    replacement: &Attestation<TestScheme>,
) {
    let other = replacement.signature.get().cloned().unwrap();
    let mut sig = attestation.signature.get().cloned().unwrap();
    sig.bls_seed_partial = other.bls_seed_partial;
    attestation.signature = commonware_codec::types::lazy::Lazy::from(sig);
}

/// A byzantine partial for `scheme`: its own valid partial but over a
/// *different* round, so it is well-formed yet fails verification against
/// the real round's seed message.
fn foreign_round_attestation(scheme: &TestScheme) -> Attestation<TestScheme> {
    let foreign = sample_proposal(Epoch::new(1), View::new(98), 0xCC);
    scheme
        .sign::<Sha256Digest>(Subject::Finalize { proposal: &foreign })
        .unwrap()
}

/// A vote and its seed partial are one atomic quorum contribution. A single
/// byzantine partial removes that complete attestation while the remaining
/// clean n=4 quorum still assembles a valid certificate.
#[test]
fn test_byzantine_seed_partial_excluded_keeps_valid_vrf_proof() {
    let (schemes, verifier) = signers_and_verifier(4);
    let round = Round::new(Epoch::new(1), View::new(2));
    let proposal = sample_proposal(round.epoch(), round.view(), 42);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };

    let honest: Vec<_> = schemes
        .iter()
        .map(|s| s.sign::<Sha256Digest>(subject).unwrap())
        .collect();

    // Corrupt a partial that lands in `threshold::recover`'s interpolation
    // set, with a well-formed-but-wrong value (signer 1's partial over a
    // foreign round). `recover` truncates to `required` partials by index.
    // The control assertion below guards that the chosen index actually
    // poisons recovery, so the regression cannot silently pick a
    // truncated-out index.
    let mut corrupted = honest.clone();
    corrupt_seed_partial(&mut corrupted[1], &foreign_round_attestation(&schemes[1]));

    let mut rng = bls_batch_verification_rng();

    // Control: assembling directly from the corrupted attestations (without
    // admission verification) yields a certificate whose proof does NOT verify.
    // This is the poison that the attack relied on.
    let poisoned = verifier
        .assemble(
            NonEmpty::try_new(corrupted.clone().into_iter())
                .expect("test attestations are non-empty"),
            &Sequential,
        )
        .unwrap();
    assert_eq!(poisoned.vrf_proof.material_version, 0);
    assert!(
        verifier
            .verified_vrf_seed_for_round(&mut rng, round, &poisoned, &Sequential)
            .is_none(),
        "control: the unverified garbage partial poisons the recovered proof"
    );

    // Production path: the batcher drops the whole vote+partial pair.
    let verification = verifier.verify_attestations::<_, Sha256Digest, _>(
        &mut rng,
        subject,
        corrupted,
        &Sequential,
    );
    assert!(
        verification.invalid.is_empty(),
        "a partial problem must not p2p-block the authenticated signer"
    );
    assert_eq!(
        verification.verified.len(),
        3,
        "only the three complete valid attestations count"
    );

    let certificate = verifier
        .assemble(
            NonEmpty::try_new(verification.verified.into_iter())
                .expect("test attestations are non-empty"),
            &Sequential,
        )
        .unwrap();
    assert_eq!(certificate.signers.count(), 3, "the bad pair is excluded");
    assert_eq!(certificate.vrf_proof.material_version, 0);
    assert!(
        verifier
            .verified_vrf_seed_for_round(&mut rng, round, &certificate, &Sequential)
            .is_some(),
        "the recovered proof must verify against the group key - no halt at N+1"
    );
    assert!(verifier.verify_certificate::<_, Sha256Digest>(
        &mut rng,
        subject,
        &certificate,
        &Sequential
    ));
}

mod remaining;

#[test]
fn live_signing_gate_revokes_votes_but_keeps_verification() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();
    let schemes = signer_schemes(NAMESPACE, &keys, &participants, &dkg);
    let open = Arc::new(AtomicBool::new(true));
    let gate = open.clone();
    let scheme = schemes[0]
        .clone()
        .with_signing_gate(move || gate.load(Ordering::SeqCst));
    let proposal = sample_proposal(Epoch::new(1), View::new(2), 42);
    let subject = Subject::Notarize {
        proposal: &proposal,
    };
    let vote = scheme.sign::<Sha256Digest>(subject).unwrap();
    assert!(scheme.me().is_some());
    open.store(false, Ordering::SeqCst);
    assert_eq!(
        scheme.me(),
        schemes[0].me(),
        "WAL replay identity must survive suspension"
    );
    assert!(scheme.sign::<Sha256Digest>(subject).is_none());
    let mut rng = rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng);
    assert!(scheme.verify_attestation(&mut rng, subject, &vote, &Sequential));
    open.store(true, Ordering::SeqCst);
    assert!(scheme.me().is_some());
    assert!(scheme.sign::<Sha256Digest>(subject).is_some());
}
