//! Smoke tests for the V2 self-contained verifier shell.
//!
//! These tests exercise the rules owns:
//! decode + structural + quorum + BLS aggregate + mandatory threshold-VRF.
//! Binding and missed_proposers rules are covered.

use commonware_codec::Encode;
use commonware_cryptography::bls12381::primitives::variant::MinSig;
use commonware_utils::{Faults as _, N3f1};
use outbe_consensus::proof::{
    verify_v2_proof_low_level, CommitteeSnapshotView, HybridCertificate, V2VerifyError,
    VoteBinding, VoteSubject,
};
use outbe_consensus::test_harness::{vrf_test_committee, CertificateMessages, VrfTestCommittee};

const VOTE_NAMESPACE: &[u8] = b"outbe_FINALIZE";
const VOTE_MESSAGE: &[u8] = b"finalize-proposal-message";
const SEED_MESSAGE: &[u8] = b"seed-round-1";

fn build_certificate(
    committee: &VrfTestCommittee,
    signer_indices: &[u32],
) -> HybridCertificate<MinSig> {
    let messages = CertificateMessages {
        vote_namespace: VOTE_NAMESPACE,
        vote: VOTE_MESSAGE,
        seed: SEED_MESSAGE,
    };
    committee.certificate(
        signer_indices,
        &messages,
        &committee.vrf_threshold_private,
        5,
    )
}

fn snapshot<'a>(committee: &'a VrfTestCommittee) -> CommitteeSnapshotView<'a> {
    CommitteeSnapshotView {
        participants: committee.pubkeys.as_slice(),
        vrf_group_public_key: committee.vrf_group_public_key,
        vrf_material_version: 5,
    }
}

fn binding<'a>() -> VoteBinding<'a> {
    VoteBinding {
        subject: VoteSubject::Finalize,
        namespace: VOTE_NAMESPACE,
        message: VOTE_MESSAGE,
        seed_message: SEED_MESSAGE,
    }
}

#[test]
fn verify_v2_proof_accepts_valid_quorum_certificate() {
    let committee = vrf_test_committee(4);
    let cert = build_certificate(&committee, &[0, 1, 2, 3]);
    let bytes = cert.encode();
    let verified = verify_v2_proof_low_level(&snapshot(&committee), &binding(), bytes.as_ref())
        .expect("valid quorum certificate must verify");
    assert_eq!(verified.signer_bitmap, vec![1, 1, 1, 1]);
    assert_eq!(verified.vrf_material_version, 5);
}

#[test]
fn verify_v2_proof_rejects_below_quorum() {
    // 4 participants, 2 signers -> N3f1 quorum is 3, so this rejects.
    let committee = vrf_test_committee(4);
    let cert = build_certificate(&committee, &[0, 1]);
    let bytes = cert.encode();
    let err = verify_v2_proof_low_level(&snapshot(&committee), &binding(), bytes.as_ref())
        .expect_err("below quorum must reject");
    match err {
        V2VerifyError::BelowQuorum {
            signers: 2,
            quorum: 3,
        } => {}
        other => panic!("expected BelowQuorum {{2,3}}, got {other:?}"),
    }
}

#[test]
fn verify_v2_proof_rejects_truncated_mandatory_vrf() {
    let committee = vrf_test_committee(4);
    let cert = build_certificate(&committee, &[0, 1, 2, 3]);
    let mut bytes = cert.encode().to_vec();
    bytes.truncate(bytes.len() - 56);
    let err = verify_v2_proof_low_level(&snapshot(&committee), &binding(), bytes.as_ref())
        .expect_err("truncated mandatory VRF must fail certificate decoding");
    assert!(matches!(err, V2VerifyError::Decode(_)), "{err:?}");
}

#[test]
fn verify_v2_proof_rejects_wrong_vote_message() {
    let committee = vrf_test_committee(4);
    let cert = build_certificate(&committee, &[0, 1, 2, 3]);
    let bytes = cert.encode();
    let mut bad = binding();
    bad.message = b"tampered-finalize-message";
    let err = verify_v2_proof_low_level(&snapshot(&committee), &bad, bytes.as_ref())
        .expect_err("wrong vote message must reject BLS aggregate");
    assert!(matches!(err, V2VerifyError::BlsAggregateInvalid), "{err:?}");
}

#[test]
fn verify_v2_proof_rejects_wrong_seed_message() {
    let committee = vrf_test_committee(4);
    let cert = build_certificate(&committee, &[0, 1, 2, 3]);
    let bytes = cert.encode();
    let mut bad = binding();
    bad.seed_message = b"tampered-seed";
    let err = verify_v2_proof_low_level(&snapshot(&committee), &bad, bytes.as_ref())
        .expect_err("wrong seed message must reject threshold VRF");
    assert!(matches!(err, V2VerifyError::InvalidVrfSignature), "{err:?}");
}

#[test]
fn quorum_constant_matches_n3f1() {
    // The exported helper is the single source used by both consensus proof
    // verification and OCOMP attempt construction.
    // commonware_utils::N3f1::quorum for every committee size we test.
    for n in [1usize, 2, 3, 4, 5, 6, 7, 10, 16, 64, 128] {
        let expected = N3f1::quorum(n as u32) as usize;
        let derived = outbe_consensus::proof::simplex_n3f1_quorum(n);
        assert_eq!(derived, expected, "N3f1 quorum mismatch at n={n}");
    }
}
