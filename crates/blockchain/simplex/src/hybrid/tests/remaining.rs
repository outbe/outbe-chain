use super::*;

/// When more than `f` partials are unusable, fewer than quorum complete
/// attestations remain and no certificate is assembled.
#[test]
fn excess_invalid_seed_partials_prevent_certificate_assembly() {
    let (schemes, verifier) = signers_and_verifier(4);
    let round = Round::new(Epoch::new(1), View::new(2));
    let proposal = sample_proposal(round.epoch(), round.view(), 7);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };

    let honest: Vec<_> = schemes
        .iter()
        .map(|s| s.sign::<Sha256Digest>(subject).unwrap())
        .collect();

    // Corrupt two partials -> only two honest remain (< required = 3).
    let mut corrupted = honest.clone();
    corrupt_seed_partial(&mut corrupted[1], &foreign_round_attestation(&schemes[1]));
    corrupt_seed_partial(&mut corrupted[2], &foreign_round_attestation(&schemes[2]));

    let mut rng = bls_batch_verification_rng();
    let verification = verifier.verify_attestations::<_, Sha256Digest, _>(
        &mut rng,
        subject,
        corrupted,
        &Sequential,
    );
    assert!(
        verification.invalid.is_empty(),
        "partial drops do not p2p-block"
    );
    assert_eq!(verification.verified.len(), 2);
    assert!(
        verifier
            .assemble(
                NonEmpty::try_new(verification.verified.into_iter())
                    .expect("test attestations are non-empty"),
                &Sequential
            )
            .ok()
            .is_none(),
        "too few complete attestations must never produce a certificate"
    );
}

/// Honest path through `verify_attestations` is unchanged: every valid
/// partial is retained (not retagged), recovery succeeds, and the proof
/// verifies.
#[test]
fn valid_seed_partials_form_a_verifiable_certificate() {
    let (schemes, verifier) = signers_and_verifier(4);
    let round = Round::new(Epoch::new(1), View::new(2));
    let proposal = sample_proposal(round.epoch(), round.view(), 9);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let attestations: Vec<_> = schemes
        .iter()
        .map(|s| s.sign::<Sha256Digest>(subject).unwrap())
        .collect();

    let mut rng = bls_batch_verification_rng();
    let verification = verifier.verify_attestations::<_, Sha256Digest, _>(
        &mut rng,
        subject,
        attestations,
        &Sequential,
    );
    assert_eq!(verification.verified.len(), 4);
    let certificate = verifier
        .assemble(
            NonEmpty::try_new(verification.verified.into_iter())
                .expect("test attestations are non-empty"),
            &Sequential,
        )
        .unwrap();
    assert_eq!(certificate.vrf_proof.material_version, 0);
    assert!(
        verifier
            .verified_vrf_seed_for_round(&mut rng, round, &certificate, &Sequential)
            .is_some(),
        "untouched honest partials recover a verifying proof"
    );
}

/// Tamper a single field of an attestation's signature, preserving the
/// signer index. Used to prove the identity-sig binding.
fn tamper_signature(
    attestation: &Attestation<TestScheme>,
    mutate: impl FnOnce(&mut HybridSignature<MinSig>),
) -> Attestation<TestScheme> {
    let mut sig = attestation.signature.get().cloned().unwrap();
    mutate(&mut sig);
    Attestation {
        signer: attestation.signer,
        signature: commonware_codec::types::lazy::Lazy::from(sig),
    }
}

#[test]
fn test_hybrid_signature_size_includes_identity_sig() {
    // MinPk vote (96) + version (8) + MinSig partial (48) + MinPk identity (96).
    assert_eq!(HybridSignature::<MinSig>::SIZE, 96 + 8 + 48 + 96);
}

#[test]
fn test_hybrid_signature_codec_roundtrip_preserves_identity_sig() {
    let (schemes, _verifier) = signers_and_verifier(3);
    let proposal = sample_proposal(Epoch::new(1), View::new(2), 5);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let attestation = schemes[0].sign::<Sha256Digest>(subject).unwrap();
    let sig = attestation.signature.get().cloned().unwrap();
    let encoded = sig.encode();
    assert_eq!(encoded.len(), HybridSignature::<MinSig>::SIZE);
    let decoded = HybridSignature::<MinSig>::read(&mut encoded.as_ref()).unwrap();
    assert_eq!(sig, decoded);
    assert_eq!(
        sig.seed_partial_identity_sig,
        decoded.seed_partial_identity_sig
    );
}

#[test]
fn test_seed_partial_identity_sig_binds_round_version_partial() {
    let (schemes, verifier) = signers_and_verifier(3);
    let proposal = sample_proposal(Epoch::new(1), View::new(2), 7);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let attestation = schemes[0].sign::<Sha256Digest>(subject).unwrap();

    // Happy path: the rider identity sig verifies for the real signer.
    assert!(verifier.verify_seed_partial_identity_sig(subject, &attestation));

    // Different round (different subject) breaks the binding.
    let other_proposal = sample_proposal(Epoch::new(1), View::new(3), 7);
    let other_subject = Subject::Finalize {
        proposal: &other_proposal,
    };
    assert!(!verifier.verify_seed_partial_identity_sig(other_subject, &attestation));

    // Tampered version breaks the binding.
    let bad_version = tamper_signature(&attestation, |s| s.vrf_material_version ^= 1);
    assert!(!verifier.verify_seed_partial_identity_sig(subject, &bad_version));

    // Tampered partial breaks the binding (graft signer 1's partial).
    let other = schemes[1].sign::<Sha256Digest>(subject).unwrap();
    let other_partial = other.signature.get().unwrap().bls_seed_partial;
    let bad_partial = tamper_signature(&attestation, |s| s.bls_seed_partial = other_partial);
    assert!(!verifier.verify_seed_partial_identity_sig(subject, &bad_partial));

    // Re-attributing signer 0's attestation to index 1 fails (wrong key).
    let misattributed = Attestation {
        signer: Participant::new(1),
        signature: attestation.signature.clone(),
    };
    assert!(!verifier.verify_seed_partial_identity_sig(subject, &misattributed));
}

#[test]
fn test_garbage_identity_sig_does_not_censor_the_vote() {
    // A relay that strips/corrupts seed_partial_identity_sig must NOT get the
    // vote rejected. verify_attestation only gates on the MinPk vote.
    let (schemes, verifier) = signers_and_verifier(3);
    let proposal = sample_proposal(Epoch::new(1), View::new(2), 11);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let attestation = schemes[0].sign::<Sha256Digest>(subject).unwrap();
    let wrong_sig = bls12381::PrivateKey::from_seed(424242).sign(b"x", b"y");
    let tampered = tamper_signature(&attestation, |s| s.seed_partial_identity_sig = wrong_sig);

    let mut rng = bls_batch_verification_rng();
    assert!(
        verifier.verify_attestation(&mut rng, subject, &tampered, &Sequential),
        "vote must still verify despite a garbage identity sig"
    );
    assert!(
        !verifier.verify_seed_partial_identity_sig(subject, &tampered),
        "but the identity sig itself must be rejected (no attribution)"
    );
}

#[test]
fn seed_partial_verdict_routes_only_valid_pairs_into_quorum() {
    use crate::proof::{seed_attest_namespace, seed_partial_attest_message};

    let (keys, schemes, verifier) = signers_keys_and_verifier(4);
    let proposal = sample_proposal(Epoch::new(1), View::new(2), 13);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let mut rng = bls_batch_verification_rng();

    let honest = schemes[0].sign::<Sha256Digest>(subject).unwrap();
    let honest_sig = honest.signature.get().cloned().unwrap();

    // Valid: an honest active-version partial.
    assert_eq!(
        verifier.classify_seed_partial(&mut rng, subject, &honest, &Sequential),
        SeedPartialVerdict::Valid
    );

    // StaleVersion: a non-active material version is excluded, not byzantine.
    let stale = tamper_signature(&honest, |s| s.vrf_material_version = 999);
    assert_eq!(
        verifier.classify_seed_partial(&mut rng, subject, &stale, &Sequential),
        SeedPartialVerdict::StaleVersion
    );

    // A well-formed-but-wrong partial value (signer 0 over a foreign round).
    let bad_partial = foreign_round_attestation(&schemes[0])
        .signature
        .get()
        .unwrap()
        .bls_seed_partial;
    let attest_msg = seed_partial_attest_message(1, 2, 0, bad_partial.encode().as_ref());

    // AttributableInvalid: bad partial bound by signer 0's own identity sig.
    let good_identity_sig = keys[0].sign(&seed_attest_namespace(), &attest_msg);
    let attributable = Attestation {
        signer: honest.signer,
        signature: commonware_codec::types::lazy::Lazy::from(HybridSignature::<MinSig> {
            bls_individual_vote: honest_sig.bls_individual_vote.clone(),
            vrf_material_version: 0,
            bls_seed_partial: bad_partial,
            seed_partial_identity_sig: good_identity_sig,
        }),
    };
    assert_eq!(
        verifier.classify_seed_partial(&mut rng, subject, &attributable, &Sequential),
        SeedPartialVerdict::AttributableInvalid
    );

    // Unattributable: same bad partial, but the identity sig is from the
    // WRONG key, so it cannot be attributed to signer 0 (relay-forgery case).
    let wrong_identity_sig =
        bls12381::PrivateKey::from_seed(999).sign(&seed_attest_namespace(), &attest_msg);
    let unattributable = Attestation {
        signer: honest.signer,
        signature: commonware_codec::types::lazy::Lazy::from(HybridSignature::<MinSig> {
            bls_individual_vote: honest_sig.bls_individual_vote.clone(),
            vrf_material_version: 0,
            bls_seed_partial: bad_partial,
            seed_partial_identity_sig: wrong_identity_sig,
        }),
    };
    assert_eq!(
        verifier.classify_seed_partial(&mut rng, subject, &unattributable, &Sequential),
        SeedPartialVerdict::Unattributable
    );

    let routed = verifier.verify_attestations::<_, Sha256Digest, _>(
        &mut rng,
        subject,
        [honest, stale, attributable, unattributable],
        &Sequential,
    );
    assert_eq!(routed.verified.len(), 1, "only the Valid pair counts");
    assert!(
        routed.invalid.is_empty(),
        "partial verdicts are dropped without p2p-blocking the signer"
    );
}

#[test]
fn invalid_seed_partial_verdict_does_not_depend_on_batch_rng_weights() {
    let (keys, schemes, verifier) = signers_keys_and_verifier(4);
    let proposal = sample_proposal(Epoch::new(1), View::new(2), 14);
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let honest = schemes[0].sign::<Sha256Digest>(subject).unwrap();
    let honest_signature = honest.signature.get().cloned().unwrap();
    let bad_partial = foreign_round_attestation(&schemes[0])
        .signature
        .get()
        .unwrap()
        .bls_seed_partial;
    let attest_message =
        crate::proof::seed_partial_attest_message(1, 2, 0, bad_partial.encode().as_ref());
    let attributable = Attestation {
        signer: honest.signer,
        signature: commonware_codec::types::lazy::Lazy::from(HybridSignature::<MinSig> {
            bls_individual_vote: honest_signature.bls_individual_vote,
            vrf_material_version: 0,
            bls_seed_partial: bad_partial,
            seed_partial_identity_sig: keys[0]
                .sign(&crate::proof::seed_attest_namespace(), &attest_message),
        }),
    };

    assert_eq!(
        verifier.classify_seed_partial(&mut ZeroRng, subject, &attributable, &Sequential,),
        SeedPartialVerdict::AttributableInvalid
    );
    assert_eq!(
        verifier.classify_seed_partial(
            &mut bls_batch_verification_rng(),
            subject,
            &attributable,
            &Sequential,
        ),
        SeedPartialVerdict::AttributableInvalid
    );
}

#[test]
fn test_hybrid_certificate_codec_roundtrip() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

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

    let attestations: Vec<_> = schemes
        .iter()
        .map(|s| s.sign::<Sha256Digest>(subject).unwrap())
        .collect();

    let certificate = verifier
        .assemble(
            NonEmpty::try_new(attestations.into_iter()).expect("test attestations are non-empty"),
            &Sequential,
        )
        .unwrap();

    // Encode and decode
    let encoded = commonware_codec::Encode::encode(&certificate);
    let decoded = HybridCertificate::<MinSig>::read_cfg(
        &mut encoded.as_ref(),
        &verifier.certificate_codec_config(),
    )
    .unwrap();

    assert_eq!(certificate, decoded);
}

#[test]
fn test_hybrid_scheme_provider() {
    let (_, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    let provider = HybridSchemeProvider::<MinSig>::new();

    let verifier =
        HybridScheme::<MinSig>::verifier(NAMESPACE, participants.clone(), dkg.polynomial.clone())
            .unwrap();

    let epoch = Epoch::new(1);
    assert!(provider.register(epoch, verifier));

    // Lookup
    assert!(certificate::Provider::scoped(&provider, epoch).is_some());
    let scheme = certificate::Provider::scheme(&provider, epoch)
        .expect("epoch participant material remains available to marshal");
    assert_eq!(scheme.participants(), &participants);
    assert!(scheme.me().is_none());
    let round = Round::new(epoch, View::new(1));
    assert!(scheme
        .sign::<Sha256Digest>(Subject::Nullify { round })
        .is_none());
    assert!(certificate::Provider::scoped(&provider, Epoch::new(2)).is_none());

    // Remove
    assert!(provider.remove(&epoch));
    assert!(certificate::Provider::scoped(&provider, epoch).is_none());
}

#[test]
fn test_hybrid_certificate_size_small() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    let schemes = signer_schemes(NAMESPACE, &keys, &participants, &dkg);

    let epoch = Epoch::new(1);
    let view = View::new(2);
    let proposal = sample_proposal(epoch, view, 42);
    let subject = Subject::Notarize {
        proposal: &proposal,
    };

    let attestations: Vec<_> = schemes
        .iter()
        .map(|s| s.sign::<Sha256Digest>(subject).unwrap())
        .collect();

    let certificate = schemes[0]
        .assemble(
            NonEmpty::try_new(attestations.into_iter()).expect("test attestations are non-empty"),
            &Sequential,
        )
        .unwrap();

    let encoded = certificate.encode();
    // Certificate should be much smaller than ed25519 variant.
    // Signers bitmap + aggregated vote + mandatory VRF proof stays compact.
    assert!(
        encoded.len() < 200,
        "certificate should be compact, got {} bytes",
        encoded.len()
    );
}

#[test]
fn test_signer_returns_none_on_polynomial_participant_mismatch() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(4).unwrap(); // 4-validator polynomial

    // 3 participants but 4-validator polynomial -> mismatch -> None
    let result = HybridScheme::<MinSig>::signer(
        NAMESPACE,
        participants.clone(),
        keys[0].clone(),
        dkg.polynomial.clone(),
        dkg.shares[0].clone(),
    );
    assert!(
        result.is_none(),
        "signer should return None on polynomial/participant mismatch"
    );
}

#[test]
fn test_verifier_returns_none_on_polynomial_participant_mismatch() {
    let (_, participants) = test_participants(3);
    let dkg = bootstrap_dkg(4).unwrap(); // 4-validator polynomial

    // 3 participants but 4-validator polynomial -> mismatch -> None
    let result =
        HybridScheme::<MinSig>::verifier(NAMESPACE, participants.clone(), dkg.polynomial.clone());
    assert!(
        result.is_none(),
        "verifier should return None on polynomial/participant mismatch"
    );
}

#[test]
fn test_signer_returns_none_on_invalid_key() {
    let (_, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    // Use a key that is NOT in the participant set
    let foreign_key = bls12381::PrivateKey::from_seed(999);
    let result = HybridScheme::<MinSig>::signer(
        NAMESPACE,
        participants.clone(),
        foreign_key,
        dkg.polynomial.clone(),
        dkg.shares[0].clone(),
    );
    assert!(
        result.is_none(),
        "signer should return None when key not in participant set"
    );
}

#[test]
fn test_signer_returns_none_on_share_public_mismatch() {
    let (keys, participants) = test_participants(3);

    // Create two different DKG outputs - shares from dkg2 won't match
    // polynomial from dkg1.
    let dkg1 = bootstrap_dkg(3).unwrap();
    let dkg2 = bootstrap_dkg(3).unwrap();

    // Use key[0] with dkg1's polynomial but dkg2's share[0].
    // share[0] from dkg2 has the same index but different public key
    // because it was generated from a different polynomial.
    let result = HybridScheme::<MinSig>::signer(
        NAMESPACE,
        participants.clone(),
        keys[0].clone(),
        dkg1.polynomial.clone(),
        dkg2.shares[0].clone(), // share from different DKG -> public mismatch
    );
    assert!(
        result.is_none(),
        "signer should return None when share public doesn't match polynomial"
    );
}

// Insert-once and poisoned-mutex recovery for the providers' shared storage
// mechanism are tested directly on `EpochRegistry` (crate::epoch_registry).

// -----------------------------------------------------------------------
// signer() rejects share.index != participant index
// -----------------------------------------------------------------------

/// signer() with mismatched share.index returns None.
#[test]
fn test_signer_share_index_mismatch_returns_none() {
    let (keys, participants) = test_participants(3);
    let dkg = bootstrap_dkg(3).unwrap();

    let pk0 = bls12381::PublicKey::from(keys[0].clone());
    let correct_idx = participants.index(&pk0).unwrap();

    // Get a share that belongs to a DIFFERENT participant index
    let wrong_share_idx = if correct_idx.get() == 0 {
        1usize
    } else {
        0usize
    };
    let wrong_share = dkg.shares[wrong_share_idx].clone();

    // Verify: the share's index does NOT match participant 0's index
    assert_ne!(
        correct_idx, wrong_share.index,
        "test setup: share must have wrong index"
    );

    let result = HybridScheme::<MinSig>::signer(
        NAMESPACE,
        participants,
        keys[0].clone(),
        dkg.polynomial.clone(),
        wrong_share,
    );
    assert!(
        result.is_none(),
        "signer with mismatched share.index must return None"
    );
}

/// The Subject seed message must be byte-identical to the proof-side canonical
/// recipe `proof::constants::seed_namespace_and_message`. This makes sure that
/// the hybrid signer, the hybrid verifier, and the proof-side slashing /
/// next-height-gate verifiers all derive the same VRF seed message for a given round. This test
/// locks this cross-path determinism invariant against drift in either side.
#[test]
fn seed_message_matches_proof_side_recipe() {
    let epoch = Epoch::new(7);
    let view = View::new(42);
    let subject: Subject<'_, Sha256Digest> = Subject::Nullify {
        round: Round::new(epoch, view),
    };

    let hybrid_seed_message = seed_message_from_subject(&subject);
    let (_, proof_seed_message) =
        crate::proof::constants::seed_namespace_and_message(epoch.get(), view.get());

    assert_eq!(hybrid_seed_message.as_ref(), proof_seed_message.as_slice());
    // The seed message is exactly the round's canonical bytes. No path can make
    // the round and its seed message diverge now that one derives from the other.
    assert_eq!(hybrid_seed_message, round_from_subject(&subject).encode());
}
