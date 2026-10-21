//! - full A4 verifier integration tests.
//!
//! Exercises the metadata-bound `verify_v2_proof(metadata, snapshot,
//! proof_bytes, header_parent_hash)` entry point. Each test drives one
//! specific failure class and asserts the exact `V2VerifyError`
//! variant. Happy-path tests run the full flow end-to-end against a real
//! DKG-derived HybridCertificate.

#[path = "support/parent_proof.rs"]
mod parent_proof;

use alloy_primitives::{Address, Bytes, B256};
use commonware_codec::{Encode, FixedSize};
use commonware_consensus::{
    simplex::types::{Finalization, Proposal},
    types::{Epoch, Round, View},
};
use commonware_cryptography::{
    bls12381::primitives::variant::{MinSig, Variant},
    sha256::Digest as Sha256Digest,
};
use outbe_consensus::hybrid::HybridScheme;
use outbe_consensus::proof::constants::{finalize_namespace, notarize_namespace};
use outbe_consensus::proof::{
    verify_v2_proof, CommitteeEntry, CommitteeSnapshot, HybridCertificate, V2VerifyError,
};
use outbe_consensus::test_harness::{
    finalize_messages, test_fully_signed_metadata, vrf_test_committee, CertificateMessages,
    TestFinalizedParent,
};
use outbe_primitives::consensus_metadata::{
    CertifiedParentAccountingMetadata, MissedProposerEvent, ParentParticipationProof,
};

// -- Test fixtures ---------------------------------------------------------

/// Build a fixed-size committee snapshot of `n` deterministic entries.
/// Each entry has a stable address + 48 zero bytes for the consensus pubkey.
/// This is sufficient for the binding tests that don't exercise BLS
/// verification. Those tests use either the inner bitmap/structural rules or
/// assert pre-BLS failure variants.
fn fixture_snapshot(n: usize) -> CommitteeSnapshot {
    let committee: Vec<CommitteeEntry> = (0..n)
        .map(|i| CommitteeEntry {
            address: Address::with_last_byte((i + 1) as u8),
            consensus_pubkey: [0u8; 48],
        })
        .collect();
    CommitteeSnapshot {
        committee,
        vrf_material_version: 7,
        vrf_group_public_key_bytes: vec![0u8; <MinSig as Variant>::Public::SIZE],
        vrf_public_polynomial_hash: alloy_primitives::B256::ZERO,
    }
}

/// Build a `CertifiedParentAccountingMetadata` matching the given snapshot
/// (committee, vrf_material_version, vrf_group_public_key_hash all aligned
/// with the snapshot). The `certificate` blob is opaque bytes. Tests that
/// need a real cert override it.
fn fixture_metadata(
    snapshot: &CommitteeSnapshot,
    parent_hash: B256,
) -> CertifiedParentAccountingMetadata {
    let parent = TestFinalizedParent {
        block_number: 41,
        block_hash: parent_hash,
        epoch: 3,
        view: 100,
        parent_view: 99,
        vrf_material_version: snapshot.vrf_material_version,
        proof_kind: ParentParticipationProof::Finalization,
    };
    test_fully_signed_metadata(&parent, snapshot, b"opaque-cert-bytes-not-real")
}

/// Build a cryptographically valid `(snapshot, metadata, cert, proof_bytes)` tuple
/// for `proof_kind` using the cached 4-validator DKG fixture.
fn real_proof_fixture(
    parent_hash: B256,
    proof_kind: ParentParticipationProof,
) -> (
    CommitteeSnapshot,
    CertifiedParentAccountingMetadata,
    HybridCertificate<MinSig>,
    Vec<u8>,
) {
    let dkg = vrf_test_committee(4);
    let version = 7u64;
    let epoch = 3u64;
    let view = 100u64;
    let parent_view = 99u64;
    let snapshot = dkg.snapshot(version);
    let (_, vote, seed) = finalize_messages(epoch, view, parent_view, parent_hash);
    let committee_set = dkg.committee_set();
    let vote_namespace = match proof_kind {
        ParentParticipationProof::Finalization => finalize_namespace(&committee_set),
        ParentParticipationProof::CertifiedNotarization => notarize_namespace(&committee_set),
    };
    let messages = CertificateMessages {
        vote_namespace: &vote_namespace,
        vote: &vote,
        seed: &seed,
    };
    let cert = dkg.certificate(
        &[0, 1, 2, 3],
        &messages,
        &dkg.vrf_threshold_private,
        version,
    );
    let proposal = Proposal::new(
        Round::new(Epoch::new(epoch), View::new(view)),
        View::new(parent_view),
        Sha256Digest(parent_hash.0),
    );
    let proof_bytes = parent_proof::encode_parent_proof(proposal, &cert, proof_kind);
    let parent = TestFinalizedParent {
        block_number: 41,
        block_hash: parent_hash,
        epoch,
        view,
        parent_view,
        vrf_material_version: version,
        proof_kind,
    };
    let metadata = test_fully_signed_metadata(&parent, &snapshot, &proof_bytes);
    (snapshot, metadata, cert, proof_bytes)
}

// -- Tests: NonEmptyMissedProposers (highest-priority gate) ----------------

#[test]
fn certified_notarization_missed_proposers_must_be_empty() {
    let snapshot = fixture_snapshot(3);
    let parent_hash = B256::with_last_byte(0xBB);
    let mut metadata = fixture_metadata(&snapshot, parent_hash);
    metadata.proof_kind = ParentParticipationProof::CertifiedNotarization;
    metadata.missed_proposers.push(MissedProposerEvent {
        view: 1,
        validator: Address::with_last_byte(1),
    });
    let err = verify_v2_proof(&metadata, &snapshot, &[], parent_hash)
        .expect_err("non-empty missed_proposers must reject for CertifiedNotarization too");
    assert!(matches!(
        err,
        V2VerifyError::NonEmptyMissedProposers { count: 1 }
    ));
}

#[test]
fn missed_proposers_any_non_empty_v2_list_rejects() {
    let snapshot = fixture_snapshot(3);
    let parent_hash = B256::with_last_byte(0xCC);
    let mut metadata = fixture_metadata(&snapshot, parent_hash);
    for n in 1..=4 {
        metadata.missed_proposers = (0..n)
            .map(|i| MissedProposerEvent {
                view: i,
                validator: Address::with_last_byte(i as u8),
            })
            .collect();
        let err = verify_v2_proof(&metadata, &snapshot, &[], parent_hash)
            .expect_err("any non-empty list must reject");
        assert!(matches!(
            err,
            V2VerifyError::NonEmptyMissedProposers { count } if count == n as usize
        ));
    }
}

// -- Tests: exact-parent binding -------------------------------------------

#[test]
fn wrong_accounted_block_hash_rejects() {
    let snapshot = fixture_snapshot(3);
    let parent_hash = B256::with_last_byte(0xAA);
    let wrong_hash = B256::with_last_byte(0xBB);
    let metadata = fixture_metadata(&snapshot, parent_hash);
    let err = verify_v2_proof(&metadata, &snapshot, &metadata.proof, wrong_hash)
        .expect_err("hash mismatch must reject");
    assert!(matches!(err, V2VerifyError::WrongAccountedHash { .. }));
}

#[test]
fn sibling_fork_binding_discriminates_by_hash_not_number() {
    // Reorg / sibling-fork lock. Two finalized-parent candidates occupy the
    // SAME slot (height/epoch/view) but have DIFFERENT block hashes - a fork.
    // The verifier must accept a parent-accounting proof bound to one sibling
    // only against that sibling's hash, and reject it against the other. This
    // proves the binding key is the block hash, not the block number. (outbe's reorg-safe
    // reference is the cert-carried verifier hash rule, not an EL-state read,
    // so this is the right altitude to lock it.)
    let snapshot = fixture_snapshot(3);
    let parent_a = B256::with_last_byte(0xA1);
    let parent_b = B256::with_last_byte(0xB2);
    assert_ne!(parent_a, parent_b);

    let meta_a = fixture_metadata(&snapshot, parent_a);
    let meta_b = fixture_metadata(&snapshot, parent_b);
    // Siblings share the slot; only the hash differs.
    assert_eq!(meta_a.finalized_block_number, meta_b.finalized_block_number);
    assert_eq!(meta_a.finalized_epoch, meta_b.finalized_epoch);
    assert_eq!(meta_a.finalized_view, meta_b.finalized_view);

    // Cross-fork: the verifier rejects each proof against the OTHER sibling's hash.
    let err_ab = verify_v2_proof(&meta_a, &snapshot, &meta_a.proof, parent_b)
        .expect_err("proof bound to A must reject against sibling B");
    assert!(
        matches!(err_ab, V2VerifyError::WrongAccountedHash { .. }),
        "{err_ab:?}"
    );
    let err_ba = verify_v2_proof(&meta_b, &snapshot, &meta_b.proof, parent_a)
        .expect_err("proof bound to B must reject against sibling A");
    assert!(
        matches!(err_ba, V2VerifyError::WrongAccountedHash { .. }),
        "{err_ba:?}"
    );

    // Same-fork: each proof passes the parent-hash gate against its OWN hash.
    // It then fails downstream on the opaque fixture cert, NEVER with
    // WrongAccountedHash. This makes the cross-fork rejections non-vacuous.
    let err_aa = verify_v2_proof(&meta_a, &snapshot, &meta_a.proof, parent_a)
        .expect_err("opaque fixture cert fails downstream, past the hash gate");
    assert!(
        !matches!(err_aa, V2VerifyError::WrongAccountedHash { .. }),
        "{err_aa:?}"
    );

    // Binding is the hash, not the number: flipping only the block number on a
    // correctly-hash-bound proof does NOT trip the parent-hash gate.
    let mut meta_a_wrong_number = fixture_metadata(&snapshot, parent_a);
    meta_a_wrong_number.finalized_block_number = meta_a.finalized_block_number + 10_000;
    let err_num = verify_v2_proof(
        &meta_a_wrong_number,
        &snapshot,
        &meta_a_wrong_number.proof,
        parent_a,
    )
    .expect_err("number flip alone does not trip the hash gate; fails downstream");
    assert!(
        !matches!(err_num, V2VerifyError::WrongAccountedHash { .. }),
        "{err_num:?}"
    );
}

// -- Tests: committee / snapshot binding -----------------------------------

#[test]
fn committee_snapshot_missing_rejects() {
    let mut snapshot = fixture_snapshot(3);
    snapshot.committee.clear();
    let parent_hash = B256::with_last_byte(0xAA);
    let metadata = fixture_metadata(&fixture_snapshot(3), parent_hash);
    let err = verify_v2_proof(&metadata, &snapshot, &metadata.proof, parent_hash)
        .expect_err("empty snapshot must reject");
    assert!(matches!(err, V2VerifyError::CommitteeSnapshotMissing));
}

#[test]
fn committee_mismatch_rejects() {
    let snapshot_3 = fixture_snapshot(3);
    let snapshot_4 = fixture_snapshot(4);
    let parent_hash = B256::with_last_byte(0xAA);
    let metadata = fixture_metadata(&snapshot_3, parent_hash);
    let err = verify_v2_proof(&metadata, &snapshot_4, &metadata.proof, parent_hash)
        .expect_err("committee length mismatch must reject");
    assert!(matches!(err, V2VerifyError::BitmapMismatch { .. }));
}

#[test]
fn metadata_cannot_override_consensus_pubkeys() {
    // Per-position address mismatch: metadata claims a different committee
    // than the on-chain snapshot. The verifier rejects on structural mismatch.
    let snapshot = fixture_snapshot(3);
    let parent_hash = B256::with_last_byte(0xAA);
    let mut metadata = fixture_metadata(&snapshot, parent_hash);
    metadata.ordered_committee[1] = Address::with_last_byte(0xFE);
    let err = verify_v2_proof(&metadata, &snapshot, &metadata.proof, parent_hash)
        .expect_err("address mismatch must reject");
    assert!(matches!(err, V2VerifyError::BitmapMismatch { .. }));
}

#[test]
fn address_pubkey_order_mismatch_rejects() {
    let parent_hash = B256::with_last_byte(0xAA);
    let (snapshot, mut metadata, _cert, proof_bytes) =
        real_proof_fixture(parent_hash, ParentParticipationProof::Finalization);
    // Valid control passes full verification.
    verify_v2_proof(&metadata, &snapshot, &proof_bytes, parent_hash)
        .expect("control proof must verify before address-order mutation");
    // Reverse the committee in metadata. This gives a per-position address mismatch.
    metadata.ordered_committee.reverse();
    let err = verify_v2_proof(&metadata, &snapshot, &proof_bytes, parent_hash)
        .expect_err("reversed ordered_committee must reject");
    assert!(
        matches!(err, V2VerifyError::BitmapMismatch { .. }),
        "{err:?}"
    );
}

// -- Tests: committee_set_hash + vrf material binding ----------------------

#[test]
fn committee_set_hash_mismatch_rejects() {
    let snapshot = fixture_snapshot(3);
    let parent_hash = B256::with_last_byte(0xAA);
    let mut metadata = fixture_metadata(&snapshot, parent_hash);
    metadata.committee_set_hash = B256::with_last_byte(0xFE);
    let err = verify_v2_proof(&metadata, &snapshot, &metadata.proof, parent_hash)
        .expect_err("committee_set_hash mismatch must reject");
    assert!(matches!(
        err,
        V2VerifyError::CommitteeSetHashMismatch { .. }
    ));
}

#[test]
fn wrong_vrf_material_version_rejects() {
    let snapshot = fixture_snapshot(3);
    let parent_hash = B256::with_last_byte(0xAA);
    let mut metadata = fixture_metadata(&snapshot, parent_hash);
    metadata.vrf_material_version = snapshot.vrf_material_version + 1;
    let err = verify_v2_proof(&metadata, &snapshot, &metadata.proof, parent_hash)
        .expect_err("vrf_material_version mismatch must reject");
    assert!(matches!(err, V2VerifyError::WrongVrfMaterialVersion { .. }));
}

#[test]
fn wrong_vrf_group_public_key_hash_rejects() {
    let snapshot = fixture_snapshot(3);
    let parent_hash = B256::with_last_byte(0xAA);
    let mut metadata = fixture_metadata(&snapshot, parent_hash);
    metadata.vrf_group_public_key_hash = B256::with_last_byte(0xFE);
    let err = verify_v2_proof(&metadata, &snapshot, &metadata.proof, parent_hash)
        .expect_err("vrf group pk hash mismatch must reject");
    assert!(matches!(err, V2VerifyError::WrongVrfGroupKeyHash { .. }));
}

// -- Tests: proof bytes domain ---------------------------------------------

#[test]
fn wrong_proof_domain_rejects_when_bytes_differ_from_metadata_certificate() {
    let snapshot = fixture_snapshot(3);
    let parent_hash = B256::with_last_byte(0xAA);
    let metadata = fixture_metadata(&snapshot, parent_hash);
    let wrong_bytes: Bytes = Bytes::from_static(b"this-does-not-match-metadata.proof");
    let err = verify_v2_proof(&metadata, &snapshot, &wrong_bytes, parent_hash)
        .expect_err("proof bytes != metadata.proof must reject");
    assert!(matches!(err, V2VerifyError::WrongProofDomain { .. }));
}

#[test]
fn proof_embedded_proposal_mismatch_rejects() {
    // Pass outer `proof_bytes == metadata.proof` (Rule 8) with a real decoded
    // Finalization certificate, and mutate each embedded Proposal identity field
    // (`epoch`, `view`, `parent_view`, `payload`) in isolation to verify that
    // `proposal_matches_metadata` rejects with `WrongProofDomain`.
    let parent_hash = B256::with_last_byte(0xAA);
    let (snapshot, control_metadata, cert, control_bytes) =
        real_proof_fixture(parent_hash, ParentParticipationProof::Finalization);
    verify_v2_proof(&control_metadata, &snapshot, &control_bytes, parent_hash)
        .expect("control proof must verify");

    let wrong_payload = B256::with_last_byte(0xEE);
    let mismatched_proposals = [
        (
            Proposal::new(
                Round::new(Epoch::new(4), View::new(100)),
                View::new(99),
                Sha256Digest(parent_hash.0),
            ),
            parent_hash,
        ),
        (
            Proposal::new(
                Round::new(Epoch::new(3), View::new(101)),
                View::new(99),
                Sha256Digest(parent_hash.0),
            ),
            parent_hash,
        ),
        (
            Proposal::new(
                Round::new(Epoch::new(3), View::new(100)),
                View::new(98),
                Sha256Digest(parent_hash.0),
            ),
            parent_hash,
        ),
        (
            Proposal::new(
                Round::new(Epoch::new(3), View::new(100)),
                View::new(99),
                Sha256Digest(wrong_payload.0),
            ),
            wrong_payload,
        ),
    ];

    for (proposal, expected_actual) in mismatched_proposals {
        let tampered_bytes = Finalization::<HybridScheme<MinSig>, Sha256Digest> {
            proposal,
            certificate: cert.clone(),
        }
        .encode()
        .to_vec();
        let mut metadata = control_metadata.clone();
        metadata.proof = Bytes::from(tampered_bytes.clone());
        let err = verify_v2_proof(&metadata, &snapshot, &tampered_bytes, parent_hash)
            .expect_err("embedded proposal mismatch must reject");
        assert!(
            matches!(
                err,
                V2VerifyError::WrongProofDomain { expected, actual }
                    if expected == parent_hash && actual == expected_actual
            ),
            "expected WrongProofDomain({parent_hash}, {expected_actual}), got {err:?}"
        );
    }
}

// -- Tests: happy-path & mandatory VRF verification ------------------------

#[test]
fn happy_path_metadata_to_verifier_pipeline_reaches_bls_layer() {
    let parent_hash = B256::with_last_byte(0xAA);
    let (snapshot, metadata, _cert, proof_bytes) =
        real_proof_fixture(parent_hash, ParentParticipationProof::Finalization);

    // Valid control reaches and passes the full BLS + VRF layer.
    let verified = verify_v2_proof(&metadata, &snapshot, &proof_bytes, parent_hash)
        .expect("real certificate must pass metadata binding and BLS/VRF verification");
    assert_eq!(verified.signer_bitmap, vec![1, 1, 1, 1]);

    // Corrupting only the BLS aggregate vote signature (wrong vote domain) while
    // keeping all metadata bindings, VRF proof, and proposal fields intact proves
    // the pipeline reaches the BLS layer.
    let dkg = vrf_test_committee(4);
    let (_, vote, seed) = finalize_messages(3, 100, 99, parent_hash);
    let bad_bls_cert = dkg.certificate(
        &[0, 1, 2, 3],
        &CertificateMessages {
            vote_namespace: b"WRONG_BLS_DOMAIN",
            vote: &vote,
            seed: &seed,
        },
        &dkg.vrf_threshold_private,
        snapshot.vrf_material_version,
    );
    let bad_bytes = Finalization::<HybridScheme<MinSig>, Sha256Digest> {
        proposal: Proposal::new(
            Round::new(Epoch::new(3), View::new(100)),
            View::new(99),
            Sha256Digest(parent_hash.0),
        ),
        certificate: bad_bls_cert,
    }
    .encode()
    .to_vec();
    let mut bad_metadata = metadata;
    bad_metadata.proof = Bytes::from(bad_bytes.clone());
    let err = verify_v2_proof(&bad_metadata, &snapshot, &bad_bytes, parent_hash)
        .expect_err("corrupted BLS vote signature must fail at BLS layer");
    assert!(matches!(err, V2VerifyError::BlsAggregateInvalid), "{err:?}");
}

#[test]
fn valid_vrf_proof_required_for_certified_notarization_and_finalization() {
    // For BOTH proof kinds, a real certificate with a valid VRF proof verifies,
    // and stripping the mandatory 56-byte VRF tail fails decoding.
    let parent_hash = B256::with_last_byte(0xAA);
    for kind in [
        ParentParticipationProof::Finalization,
        ParentParticipationProof::CertifiedNotarization,
    ] {
        let (snapshot, mut metadata, _cert, mut proof_bytes) =
            real_proof_fixture(parent_hash, kind);
        verify_v2_proof(&metadata, &snapshot, &proof_bytes, parent_hash)
            .expect("valid certificate with mandatory VRF must verify");

        proof_bytes.truncate(proof_bytes.len() - 56);
        metadata.proof = Bytes::from(proof_bytes.clone());
        let err = verify_v2_proof(&metadata, &snapshot, &proof_bytes, parent_hash)
            .expect_err("missing mandatory VRF must reject for both proof kinds");
        assert!(matches!(err, V2VerifyError::Decode(_)), "{err:?}");
    }
}

// -- Determinism ------------------------------------------------------------

#[test]
fn verifier_outcome_deterministic_from_parent_state_and_body() {
    let parent_hash = B256::with_last_byte(0xAA);
    let (snapshot, metadata, _cert, proof_bytes) =
        real_proof_fixture(parent_hash, ParentParticipationProof::Finalization);

    // Call the verifier 16 times on a real valid certificate (and without a Tokio
    // runtime) to confirm deterministic Ok outcomes.
    let first = verify_v2_proof(&metadata, &snapshot, &proof_bytes, parent_hash)
        .expect("real certificate must verify deterministically");
    for _ in 0..15 {
        let next = verify_v2_proof(&metadata, &snapshot, &proof_bytes, parent_hash)
            .expect("repeated verification must succeed");
        assert_eq!(first.signer_bitmap, next.signer_bitmap);
        assert_eq!(first.vrf_proof_hash, next.vrf_proof_hash);
        assert_eq!(first.prev_randao, next.prev_randao);
        assert_eq!(first.vrf_material_version, next.vrf_material_version);
    }
}
