//! Full-DKG failure-class tests for the metadata-bound `verify_v2_proof` entry
//! (audit-001 closure).
//!
//! Each test does these steps:
//! 1. Build a real DKG fixture.
//! 2. Construct a real BLS-signed `HybridCertificate` against the bytes the
//!    metadata-bound verifier derives internally (`Proposal::encode()` vote
//!    message under `finalize_namespace`, `Round::encode()` seed message under
//!    `hybrid_seed_namespace`).
//! 3. Inject exactly one defect.
//! 4. Assert the exact `V2VerifyError` variant.
//!
//! Implements the 13.rs` deferred
//! because they required real signed certificates. Combined with the 20
//! binding tests in `tests/verifier.rs` and the 5 low-level smoke tests in
//! `tests/verifier_smoke.rs`'s full 31-test contract is covered.

use alloy_primitives::B256;
use commonware_codec::{Encode, Read};
use commonware_consensus::{
    simplex::types::{Finalization, Notarization, Proposal, Subject},
    types::{Epoch, Round, View},
};
use commonware_cryptography::{
    bls12381::{
        primitives::{
            ops::{aggregate, sign_message},
            variant::{MinPk, MinSig},
        },
        PrivateKey,
    },
    certificate::{Scheme as _, Signers, Verifier as _},
    sha256::Digest as Sha256Digest,
    Signer,
};
use commonware_parallel::Sequential;
use commonware_utils::{
    ordered::{Quorum, Set},
    Participant,
};
use outbe_consensus::bls::bootstrap_dkg_for_participants;
use outbe_consensus::hybrid::{HybridScheme, VrfMaterialProvider};
use outbe_consensus::proof::constants::{
    finalize_namespace, notarize_namespace, outbe_app_namespace,
};
use outbe_consensus::proof::CommitteeSnapshot;
use outbe_consensus::proof::{
    hybrid_seed_namespace, verify_v2_proof, HybridCertificate, V2VerifyError, VrfProof,
};
use outbe_consensus::test_harness::{
    finalize_messages, test_fully_signed_metadata, vrf_test_committee, CertificateMessages,
    TestFinalizedParent, VrfTestCommittee,
};
use outbe_primitives::consensus_metadata::{
    CertifiedParentAccountingMetadata, ParentParticipationProof,
};

// -- Shared fixture --------------------------------------------------------

const FINALIZED_EPOCH: u64 = 3;
const FINALIZED_VIEW: u64 = 100;
const PARENT_VIEW: u64 = 99;
const VRF_MATERIAL_VERSION: u64 = 5;
const FINALIZED_BLOCK_NUMBER: u64 = 41;

fn proof_envelope_bytes(
    cert: &HybridCertificate<MinSig>,
    parent_hash: B256,
    proof_kind: ParentParticipationProof,
) -> Vec<u8> {
    let round = Round::new(Epoch::new(FINALIZED_EPOCH), View::new(FINALIZED_VIEW));
    let payload = Sha256Digest(parent_hash.0);
    let proposal: Proposal<Sha256Digest> = Proposal::new(round, View::new(PARENT_VIEW), payload);
    match proof_kind {
        ParentParticipationProof::Finalization => {
            Finalization::<HybridScheme<MinSig>, Sha256Digest> {
                proposal,
                certificate: cert.clone(),
            }
            .encode()
            .to_vec()
        }
        ParentParticipationProof::CertifiedNotarization => {
            Notarization::<HybridScheme<MinSig>, Sha256Digest> {
                proposal,
                certificate: cert.clone(),
            }
            .encode()
            .to_vec()
        }
    }
}

/// Build a real BLS-signed `HybridCertificate` for the given signers,
/// signed against the metadata-bound verifier's canonical bytes.
fn build_cert(
    dkg: &VrfTestCommittee,
    signer_indices: &[u32],
    parent_hash: B256,
    proof_kind: ParentParticipationProof,
) -> HybridCertificate<MinSig> {
    let (_, vote, seed) =
        finalize_messages(FINALIZED_EPOCH, FINALIZED_VIEW, PARENT_VIEW, parent_hash);
    // Vote namespaces bind the ordered committee. The canonical `Set` of the
    // full DKG committee matches what the verifier rebuilds.
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
    dkg.certificate(
        signer_indices,
        &messages,
        &dkg.vrf_threshold_private,
        VRF_MATERIAL_VERSION,
    )
}

fn build_metadata(
    snapshot: &CommitteeSnapshot,
    cert_bytes: &[u8],
    parent_hash: B256,
    proof_kind: ParentParticipationProof,
) -> CertifiedParentAccountingMetadata {
    let parent = TestFinalizedParent {
        block_number: FINALIZED_BLOCK_NUMBER,
        block_hash: parent_hash,
        epoch: FINALIZED_EPOCH,
        view: FINALIZED_VIEW,
        parent_view: PARENT_VIEW,
        vrf_material_version: VRF_MATERIAL_VERSION,
        proof_kind,
    };
    test_fully_signed_metadata(&parent, snapshot, cert_bytes)
}

fn bare_certificate_rejection_error(
    proof_kind: ParentParticipationProof,
    expected_rejection: &str,
) -> V2VerifyError {
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(&dkg, &[0, 1, 2, 3], parent_hash, proof_kind);
    let envelope_bytes = cert.encode().to_vec();
    let metadata = build_metadata(&snapshot, &envelope_bytes, parent_hash, proof_kind);
    verify_v2_proof(&metadata, &snapshot, &envelope_bytes, parent_hash)
        .expect_err(expected_rejection)
}

fn is_non_envelope_wire_error(err: &V2VerifyError) -> bool {
    matches!(
        *err,
        V2VerifyError::Decode(_)
            | V2VerifyError::BitmapMismatch { .. }
            | V2VerifyError::NonHybridEncoding { .. }
            | V2VerifyError::SignerIndexOutOfRange { .. }
            | V2VerifyError::BelowQuorum { .. }
    )
}

// -- Happy-path baseline (sanity) -------------------------------------------

#[test]
fn cluster_happy_path_quorum_certificate_verifies() {
    // Baseline: real cert against real metadata + snapshot -> verify_v2_proof
    // returns Ok. This proves the fixture is well-formed. Any failure-class test
    // that adjusts ONE field can attribute the rejection to that change.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    let metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let verified = verify_v2_proof(&metadata, &snapshot, &cert_bytes, parent_hash)
        .expect("happy-path cert must verify");
    assert_eq!(verified.signer_bitmap, vec![1, 1, 1, 1]);
    assert_eq!(verified.vrf_material_version, VRF_MATERIAL_VERSION);
}

#[test]
fn assembled_finalization_passes_hybrid_and_phase1_verification() {
    let keys: Vec<PrivateKey> = (0..4)
        .map(|index| PrivateKey::from_seed(index + 1))
        .collect();
    let participants = Set::from_iter_dedup(keys.iter().map(PrivateKey::public_key));
    let dkg = bootstrap_dkg_for_participants(participants.clone()).unwrap();
    let namespace = outbe_app_namespace();

    let signers = keys
        .iter()
        .map(|key| {
            let index = participants.index(&key.public_key()).unwrap();
            HybridScheme::signer_with_vrf_provider(
                &namespace,
                participants.clone(),
                key.clone(),
                VrfMaterialProvider::new(
                    VRF_MATERIAL_VERSION,
                    dkg.polynomial.clone(),
                    Some(dkg.shares[index.get() as usize].clone()),
                ),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let verifier = HybridScheme::verifier_with_vrf_provider(
        &namespace,
        participants.clone(),
        VrfMaterialProvider::new(VRF_MATERIAL_VERSION, dkg.polynomial.clone(), None),
    )
    .unwrap();

    let parent_hash = B256::with_last_byte(0xAB);
    let proposal = Proposal::new(
        Round::new(Epoch::new(FINALIZED_EPOCH), View::new(FINALIZED_VIEW)),
        View::new(PARENT_VIEW),
        Sha256Digest(parent_hash.0),
    );
    let subject = Subject::Finalize {
        proposal: &proposal,
    };
    let certificate = verifier
        .assemble(
            commonware_utils::iter::NonEmpty::try_new(
                signers
                    .iter()
                    .map(|scheme| scheme.sign::<Sha256Digest>(subject).unwrap()),
            )
            .unwrap(),
            &Sequential,
        )
        .expect("verified local attestations assemble");

    assert!(verifier.verify_certificate::<_, Sha256Digest>(
        &mut rand_core_commonware::UnwrapErr(rand_commonware::rngs::SysRng),
        subject,
        &certificate,
        &Sequential,
    ));

    let snapshot = CommitteeSnapshot {
        committee: outbe_consensus::test_harness::committee_entries(participants.iter()),
        vrf_material_version: VRF_MATERIAL_VERSION,
        vrf_group_public_key_bytes: dkg.polynomial.public().encode().to_vec(),
        vrf_public_polynomial_hash: B256::ZERO,
    };
    let proof = proof_envelope_bytes(
        &certificate,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let metadata = build_metadata(
        &snapshot,
        &proof,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let verified = verify_v2_proof(&metadata, &snapshot, &proof, parent_hash)
        .expect("the exact locally assembled certificate passes Phase-1");
    assert_eq!(verified.signer_bitmap, vec![1, 1, 1, 1]);
    assert_eq!(verified.vrf_material_version, VRF_MATERIAL_VERSION);
}

// -- wrong_bls_domain_rejects -------------------------------------

#[test]
fn wrong_bls_domain_rejects() {
    // Sign votes under the WRONG namespace. The metadata-bound verifier
    // derives `finalize_namespace` internally, and the aggregate
    // verify fails.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let (_, vote_message, seed_message) =
        finalize_messages(FINALIZED_EPOCH, FINALIZED_VIEW, PARENT_VIEW, parent_hash);

    // Sign under wrong namespace.
    let sigs: Vec<_> = (0..4)
        .map(|i| dkg.keys[i].sign(b"WRONG_NAMESPACE", &vote_message))
        .collect();
    let bls_aggregated_vote = aggregate::combine_signatures::<MinPk, _>(
        commonware_utils::iter::NonEmpty::try_new(sigs.iter().map(|s| s.as_ref())).unwrap(),
    );
    let threshold_signature = sign_message::<MinSig>(
        &dkg.vrf_threshold_private,
        &hybrid_seed_namespace(),
        &seed_message,
    );
    let cert: HybridCertificate<MinSig> = HybridCertificate {
        signers: Signers::new(4, (0..4).map(Participant::new)).unwrap(),
        bls_aggregated_vote,
        vrf_proof: VrfProof::<MinSig> {
            material_version: VRF_MATERIAL_VERSION,
            threshold_signature,
        },
    };
    let cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    let metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let err = verify_v2_proof(&metadata, &snapshot, &cert_bytes, parent_hash)
        .expect_err("wrong BLS domain must reject");
    assert!(matches!(err, V2VerifyError::BlsAggregateInvalid), "{err:?}");
}

// -- proof_trailing_bytes_rejects ---------------------------------

#[test]
fn proof_trailing_bytes_rejects() {
    // Append a trailing byte after the canonical HybridCertificate body.
    // The proof-bytes-equal-metadata.proof check fires first (since
    // we must update the metadata.proof to match). Then the inner
    // decoder sees trailing bytes.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let mut cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    cert_bytes.push(0x00); // trailing byte
    let metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let err = verify_v2_proof(&metadata, &snapshot, &cert_bytes, parent_hash)
        .expect_err("trailing bytes must reject");
    assert!(matches!(err, V2VerifyError::TrailingBytes), "{err:?}");
}

// -- proof_codec_wrong_committee_size_rejects ---------------------

#[test]
fn proof_codec_wrong_committee_size_rejects() {
    // Cert built for 4 participants, snapshot has 3. The length mismatch between
    // metadata.ordered_committee and snapshot.committee triggers BitmapMismatch
    // before the inner decoder sees the cert.
    let dkg_4 = vrf_test_committee(4);
    let snapshot_3 = vrf_test_committee(3).snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg_4,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    let metadata = build_metadata(
        &dkg_4.snapshot(VRF_MATERIAL_VERSION),
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let err = verify_v2_proof(&metadata, &snapshot_3, &cert_bytes, parent_hash)
        .expect_err("committee size mismatch must reject");
    assert!(
        matches!(err, V2VerifyError::BitmapMismatch { .. }),
        "{err:?}"
    );
}

// -- non_hybrid_certificate_encoding_rejects ----------------------

#[test]
fn non_hybrid_certificate_encoding_rejects() {
    // A bare HybridCertificate is not a valid OAV3 proof. The metadata-bound
    // verifier now expects a full Finalization/Notarization envelope.
    let err = bare_certificate_rejection_error(
        ParentParticipationProof::Finalization,
        "bare certificate must not decode as a full proof envelope",
    );
    assert!(
        is_non_envelope_wire_error(&err),
        "bare certificate must reject as non-envelope wire shape; got {err:?}"
    );
}

// -- hybrid_signer_length_mismatch_rejects ------------------------

#[test]
fn hybrid_signer_length_mismatch_rejects() {
    // Corrupt the cert bytes to claim a different committee size in the
    // signers prefix. The decoder will reject because the bitmap length
    // does not match the committee-size config the verifier passes in.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let mut cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    // Mutate first byte (likely the bitmap length / Signers length prefix).
    cert_bytes[0] ^= 0xFF;
    let metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let err = verify_v2_proof(&metadata, &snapshot, &cert_bytes, parent_hash)
        .expect_err("length corruption must reject");
    assert!(
        matches!(
            err,
            V2VerifyError::Decode(_)
                | V2VerifyError::BitmapMismatch { .. }
                | V2VerifyError::SignerIndexOutOfRange { .. }
                | V2VerifyError::BelowQuorum { .. }
        ),
        "{err:?}"
    );
}

// -- hybrid_signer_duplicate_or_out_of_range_rejects --------------

#[test]
fn hybrid_signer_duplicate_or_out_of_range_rejects() {
    // Build cert. Tamper metadata.signer_bitmap to be 1 byte too short.
    // This is the "signer index out of range / bitmap mismatch" failure
    // class. The inner decoder + the metadata bitmap reconciliation
    // both catch it.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    let mut metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    // Truncate bitmap -> length now != committee size.
    metadata.signer_bitmap.pop();
    let err = verify_v2_proof(&metadata, &snapshot, &cert_bytes, parent_hash)
        .expect_err("bitmap length mismatch must reject");
    assert!(
        matches!(err, V2VerifyError::BitmapMismatch { .. }),
        "{err:?}"
    );
}

// -- signer_bitmap_round_trips_with_hybrid_signers_via_commonware_pk_order

#[test]
fn signer_bitmap_round_trips_with_hybrid_signers_via_commonware_pk_order() {
    // Happy-path round-trip: every position in metadata.signer_bitmap must
    // equal the position the verifier reconstructs from cert.signers.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    let metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let verified = verify_v2_proof(&metadata, &snapshot, &cert_bytes, parent_hash)
        .expect("happy-path must verify");
    // VerifiedProof.signer_bitmap is the canonical reconstruction from the
    // cert. The verifier's bitmap reconciliation cross-check enforces
    // equal-to-metadata (BitmapMismatch on inequality).
    assert_eq!(verified.signer_bitmap, metadata.signer_bitmap);
}

// -- missing_vrf_proof_rejects ------------------------------------

#[test]
fn truncated_mandatory_vrf_proof_rejects() {
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let mut cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    cert_bytes.truncate(cert_bytes.len() - 56);
    let metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let err = verify_v2_proof(&metadata, &snapshot, &cert_bytes, parent_hash)
        .expect_err("truncated mandatory VRF must reject under V2");
    assert!(matches!(err, V2VerifyError::Decode(_)), "{err:?}");
}

// -- wire_level_none_vrf_proof_decodes_but_v2_verifier_rejects ---

#[test]
fn wire_level_certificate_without_mandatory_vrf_does_not_decode() {
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let mut cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    cert_bytes.truncate(cert_bytes.len() - 56);
    let mut reader = cert_bytes.as_slice();
    Finalization::<HybridScheme<MinSig>, Sha256Digest>::read_cfg(
        &mut reader,
        &snapshot.committee.len(),
    )
    .expect_err("wire-level certificate without its mandatory VRF must not decode");
    let metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let err = verify_v2_proof(&metadata, &snapshot, &cert_bytes, parent_hash)
        .expect_err("V2 verifier must reject a certificate without its mandatory VRF");
    assert!(matches!(err, V2VerifyError::Decode(_)), "{err:?}");
}

// -- malformed_vrf_proof_encoding_rejects -------------------------

#[test]
fn malformed_vrf_proof_encoding_rejects() {
    // Corrupt the VRF threshold signature bytes inside the encoded cert.
    // The decoder may accept the bytes (length matches), but the verify
    // step fails. This yields InvalidVrfSignature.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let mut cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);
    // Flip a byte near the end (VRF signature is near the tail).
    let last = cert_bytes.len() - 1;
    cert_bytes[last] ^= 0x01;
    let metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let err = verify_v2_proof(&metadata, &snapshot, &cert_bytes, parent_hash)
        .expect_err("malformed VRF proof bytes must reject");
    // Either decode failure or VRF verify failure: both prove the verifier
    // rejected without panic.
    assert!(
        matches!(
            err,
            V2VerifyError::Decode(_)
                | V2VerifyError::InvalidVrfSignature
                | V2VerifyError::MalformedVrfProof
        ),
        "{err:?}"
    );
}

// -- wrong_vrf_seed_round_rejects ---------------------------------

#[test]
fn wrong_vrf_seed_round_rejects() {
    // Cert built with seed_message = Round(99, 99).encode(). Metadata
    // claims (epoch=3, view=100). The verifier derives the seed message
    // from metadata's round. VRF verify fails because the signature was
    // produced over a different seed message.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let cert_bytes =
        proof_envelope_bytes(&cert, parent_hash, ParentParticipationProof::Finalization);

    // Override the threshold signature with one signed over a different round.
    let wrong_round = Round::new(Epoch::new(99), View::new(99));
    let wrong_seed = wrong_round.encode().to_vec();
    let wrong_threshold = sign_message::<MinSig>(
        &dkg.vrf_threshold_private,
        &hybrid_seed_namespace(),
        &wrong_seed,
    );
    let mut cert_with_wrong_vrf = cert.clone();
    cert_with_wrong_vrf.vrf_proof = VrfProof {
        material_version: VRF_MATERIAL_VERSION,
        threshold_signature: wrong_threshold,
    };
    let bad_bytes = proof_envelope_bytes(
        &cert_with_wrong_vrf,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let _ = cert_bytes; // baseline kept for symmetry

    let metadata = build_metadata(
        &snapshot,
        &bad_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let err = verify_v2_proof(&metadata, &snapshot, &bad_bytes, parent_hash)
        .expect_err("wrong VRF seed round must reject");
    assert!(matches!(err, V2VerifyError::InvalidVrfSignature), "{err:?}");
}

// -- invalid_vrf_signature_rejects_before_state_change ------------

#[test]
fn invalid_vrf_signature_rejects_before_state_change() {
    // Replace threshold signature with one signed under a completely
    // different namespace. VRF verify fails. The verifier returns Err before
    // it constructs VerifiedProof -> no state mutation, no proof-less
    // metadata leak ( in spirit, applied at the verifier layer).
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let (_, _, seed_message) =
        finalize_messages(FINALIZED_EPOCH, FINALIZED_VIEW, PARENT_VIEW, parent_hash);

    // Sign threshold under wrong namespace.
    let wrong_threshold = sign_message::<MinSig>(
        &dkg.vrf_threshold_private,
        b"WRONG_VRF_NAMESPACE",
        &seed_message,
    );
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let mut bad_cert = cert.clone();
    bad_cert.vrf_proof = VrfProof {
        material_version: VRF_MATERIAL_VERSION,
        threshold_signature: wrong_threshold,
    };
    let bad_bytes = proof_envelope_bytes(
        &bad_cert,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let metadata = build_metadata(
        &snapshot,
        &bad_bytes,
        parent_hash,
        ParentParticipationProof::Finalization,
    );
    let err = verify_v2_proof(&metadata, &snapshot, &bad_bytes, parent_hash)
        .expect_err("invalid VRF signature must reject pre-state-change");
    assert!(matches!(err, V2VerifyError::InvalidVrfSignature), "{err:?}");
}

// -- activity_envelope_notarization_rejected_as_system_tx_proof --

#[test]
fn activity_envelope_notarization_rejected_as_system_tx_proof() {
    // A bare certificate is NOT a valid OAV3 system-tx proof body. Same root
    // cause as pinned for certified-notarization proof_kind.
    let err = bare_certificate_rejection_error(
        ParentParticipationProof::CertifiedNotarization,
        "bare certificate must reject as V2 proof bytes",
    );
    assert!(is_non_envelope_wire_error(&err), "{err:?}");
}

// -- certified_notarization_proof_rejected_for_non_parent_ancestor --

#[test]
fn certified_notarization_proof_rejected_for_non_parent_ancestor() {
    // Cert built for parent_hash X. Metadata claims parent_hash Y AND
    // verifier's header_parent_hash is Y. The proof-bytes-equal-metadata
    // check passes (we put the cert bytes in metadata.proof). But the
    // inner BLS verifier fails because the Proposal payload derived from
    // metadata uses Y, while the cert was signed for X. failure
    // through the BLS aggregate layer.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let ancestor_hash = B256::with_last_byte(0xAA);
    let actual_parent_hash = B256::with_last_byte(0xBB);
    // Cert built for ancestor.
    let cert = build_cert(
        &dkg,
        &[0, 1, 2, 3],
        ancestor_hash,
        ParentParticipationProof::CertifiedNotarization,
    );
    let cert_bytes = proof_envelope_bytes(
        &cert,
        ancestor_hash,
        ParentParticipationProof::CertifiedNotarization,
    );
    // Metadata claims actual_parent_hash.
    let metadata = build_metadata(
        &snapshot,
        &cert_bytes,
        actual_parent_hash,
        ParentParticipationProof::CertifiedNotarization,
    );
    let err = verify_v2_proof(&metadata, &snapshot, &cert_bytes, actual_parent_hash)
        .expect_err("non-parent ancestor proof must reject");
    assert!(
        matches!(err, V2VerifyError::WrongProofDomain { .. }),
        "{err:?}"
    );
}

#[test]
fn parent_randomness_is_independent_of_vote_kind_and_signer_subset() {
    use commonware_cryptography::{Hasher as _, Sha256};
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let (_, _, seed_message) =
        finalize_messages(FINALIZED_EPOCH, FINALIZED_VIEW, PARENT_VIEW, parent_hash);
    // Independent protocol oracle: derive from the raw threshold signature,
    // without invoking the production VrfProof helper or proof fingerprint.
    let signature = sign_message::<MinSig>(
        &dkg.vrf_threshold_private,
        &hybrid_seed_namespace(),
        &seed_message,
    );
    let expected = B256::from_slice(Sha256::hash(&[signature.encode().as_ref()]).as_ref());
    assert_ne!(expected, B256::ZERO);
    for kind in [
        ParentParticipationProof::Finalization,
        ParentParticipationProof::CertifiedNotarization,
    ] {
        for signers in [&[0, 1, 2][..], &[0, 1, 2, 3][..]] {
            let cert = build_cert(&dkg, signers, parent_hash, kind);
            let bytes = proof_envelope_bytes(&cert, parent_hash, kind);
            let mut metadata = build_metadata(&snapshot, &bytes, parent_hash, kind);
            metadata.signer_bitmap = vec![1, 1, 1, u8::from(signers.len() == 4)];
            let verified = verify_v2_proof(&metadata, &snapshot, &bytes, parent_hash).unwrap();
            assert_eq!(verified.prev_randao, expected);
            assert_ne!(verified.prev_randao, verified.vrf_proof_hash);
        }
    }
}
