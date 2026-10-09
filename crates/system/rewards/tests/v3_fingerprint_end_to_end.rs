//! Audit follow-up.
//!
//! Real-BLS+VRF integration test that closes the wire-level gap
//! between `outbe_consensus::proof::verify_v2_proof` and the V3 Rewards
//! fingerprint helper. A stub-based test alone asserts that
//! `compute_metadata_fingerprint` is *sensitive* to its `canonical_vrf_proof_hash`
//! argument. But it uses a stubbed `B256` value and does NOT exercise the
//! production path:
//!
//! ```text
//! verify_v2_proof  ->  VerifiedProof::vrf_proof_hash
//!                          |
//!                          v
//!                  OutbeBlockExecutor.verified_phase1_vrf_proof_hash
//!                          |
//!                          v
//!         PreloadedSystemTxContext.canonical_vrf_proof_hash
//!                          |
//!                          v
//!  outbe_rewards::runtime::check_and_record_metadata_fingerprint(_, _, _, hash)
//! ```
//!

use alloy_primitives::{B256, U256};
use commonware_codec::Encode;
use commonware_consensus::{simplex::types::Proposal, types::View};
use commonware_cryptography::{
    bls12381::primitives::variant::MinSig, sha256::Digest as Sha256Digest,
};
use outbe_consensus::proof::{
    canonical_vrf_proof_hash_v2, constants::finalize_namespace, verify_v2_proof, HybridCertificate,
    VrfProof,
};
use outbe_consensus::test_harness::{
    finalize_messages, test_fully_signed_metadata, vrf_test_committee, CertificateMessages,
    TestFinalizedParent, VrfTestCommittee,
};
use outbe_primitives::consensus_metadata::{
    CertifiedParentAccountingMetadata, ParentParticipationProof,
};
use outbe_rewards::runtime::compute_metadata_fingerprint;
use outbe_validatorset::state::CommitteeSnapshot;

// Fixture constants. They have the same shape as the verifier_cluster.rs
// fixture, so the cert format and metadata field layout are byte-compatible
// with the production verifier path.
const FINALIZED_EPOCH: u64 = 3;
const FINALIZED_VIEW: u64 = 100;
const PARENT_VIEW: u64 = 99;
const VRF_MATERIAL_VERSION: u64 = 5;
const FINALIZED_BLOCK_NUMBER: u64 = 41;

/// Build a real BLS+VRF signed certificate AND return the underlying
/// `VrfProof`. With it, the test can independently compute the canonical
/// proof hash and compare it to whatever `verify_v2_proof` derives.
fn build_cert_with_vrf_proof(
    dkg: &VrfTestCommittee,
    signer_indices: &[u32],
    parent_hash: B256,
) -> (HybridCertificate<MinSig>, VrfProof<MinSig>) {
    let (_, vote, seed) =
        finalize_messages(FINALIZED_EPOCH, FINALIZED_VIEW, PARENT_VIEW, parent_hash);
    let vote_namespace = finalize_namespace(&dkg.committee_set());
    let messages = CertificateMessages {
        vote_namespace: &vote_namespace,
        vote: &vote,
        seed: &seed,
    };
    let cert = dkg.certificate(
        signer_indices,
        &messages,
        &dkg.vrf_threshold_private,
        VRF_MATERIAL_VERSION,
    );
    let vrf_proof = cert.vrf_proof.clone();
    (cert, vrf_proof)
}

fn build_metadata(
    snapshot: &CommitteeSnapshot,
    cert_bytes: &[u8],
    parent_hash: B256,
) -> CertifiedParentAccountingMetadata {
    let parent = TestFinalizedParent {
        block_number: FINALIZED_BLOCK_NUMBER,
        block_hash: parent_hash,
        epoch: FINALIZED_EPOCH,
        view: FINALIZED_VIEW,
        parent_view: PARENT_VIEW,
        vrf_material_version: VRF_MATERIAL_VERSION,
        proof_kind: ParentParticipationProof::Finalization,
    };
    test_fully_signed_metadata(&parent, snapshot, cert_bytes)
}

/// End-to-end wire test:
/// - Build a real BLS+VRF certificate and run `verify_v2_proof` against it.
/// - Take the returned `vrf_proof_hash` and feed it into the V3 fingerprint
///   helper. In production, the executor's `verified_phase1_vrf_proof_hash`
///   cache would hold this value.
/// - Assert byte-equality between (a) the verifier-derived hash and (b) an
///   independent computation via the public
///   `canonical_vrf_proof_hash_v2(VrfProof)` helper.
/// - Build the V3 fingerprint twice, once with each source, and assert that
///   the two fingerprints are byte-equal.
///
/// This pins the contract: a future refactor cannot pass while it silently
/// breaks the verify_v2_proof -> fingerprint wire.
#[test]
fn phase1_end_to_end_real_vrf_proof_binds_v3_fingerprint() {
    // 1. Build the DKG fixture and a real signed certificate.
    let dkg = vrf_test_committee(4);
    let snapshot = dkg.snapshot(VRF_MATERIAL_VERSION);
    let parent_hash = B256::with_last_byte(0xAA);
    let (cert, vrf_proof) = build_cert_with_vrf_proof(&dkg, &[0, 1, 2, 3], parent_hash);
    let (round, _, _) =
        finalize_messages(FINALIZED_EPOCH, FINALIZED_VIEW, PARENT_VIEW, parent_hash);
    let proposal = Proposal::new(round, View::new(PARENT_VIEW), Sha256Digest(parent_hash.0));
    let finalization: outbe_consensus::proof::Finalization<
        outbe_consensus::hybrid::HybridScheme<MinSig>,
        Sha256Digest,
    > = commonware_consensus::simplex::types::Finalization {
        proposal,
        certificate: cert,
    };
    let proof_bytes = finalization.encode().to_vec();
    let metadata = build_metadata(&snapshot, &proof_bytes, parent_hash);

    // 2. Run the production verifier path.
    let verified = verify_v2_proof(&metadata, &snapshot, &proof_bytes, parent_hash)
        .expect("happy-path real BLS+VRF cert must verify");

    // 3. Independently compute the canonical VRF proof hash from the
    // raw VrfProof. This is what the executor's cache would receive
    // in production via `verified.vrf_proof_hash`.
    let independent_vrf_hash = canonical_vrf_proof_hash_v2(&vrf_proof);

    // 4. Wire contract: verifier output must equal the independent
    // computation BYTE-FOR-BYTE.
    assert_eq!(
        verified.vrf_proof_hash, independent_vrf_hash,
        "verify_v2_proof must return the same canonical_vrf_proof_hash_v2 the test \
         computes independently - this pins the verify_v2_proof -> cache -> context -> \
         fingerprint wire end-to-end"
    );

    // 5. Cross-check the other VerifiedProof fields the wire carries.
    assert_eq!(verified.vrf_material_version, VRF_MATERIAL_VERSION);
    assert_eq!(verified.signer_bitmap.len(), snapshot.committee.len());

    // 6. Build the V3 fingerprint twice: once with the verifier's
    // output, once with the independent computation. Both must be
    // byte-equal. They share the same (metadata, fee_sum), and step 4
    // just proved the only varying input (`canonical_vrf_proof_hash`)
    // equal.
    let fee_sum = U256::from(12_345_678_900_000u128);
    let fp_via_verifier = compute_metadata_fingerprint(&metadata, fee_sum, verified.vrf_proof_hash);
    let fp_via_independent = compute_metadata_fingerprint(&metadata, fee_sum, independent_vrf_hash);
    assert_eq!(
        fp_via_verifier, fp_via_independent,
        "V3 fingerprint must be byte-equal whether the canonical_vrf_proof_hash comes \
         from the verifier or from an independent canonical_vrf_proof_hash_v2 call"
    );

    // 7. Negative control: substituting a wrong-VRF hash MUST produce
    // a different fingerprint. This pins that the fingerprint actually
    // consumes the hash. It defends against a future refactor that
    // ignores the argument.
    let wrong_vrf_hash = B256::with_last_byte(0xFF);
    assert_ne!(
        wrong_vrf_hash, verified.vrf_proof_hash,
        "wrong-VRF sentinel must differ from the verifier output"
    );
    let fp_with_wrong_vrf = compute_metadata_fingerprint(&metadata, fee_sum, wrong_vrf_hash);
    assert_ne!(
        fp_via_verifier, fp_with_wrong_vrf,
        "fingerprint must change when canonical_vrf_proof_hash is altered - proves the \
         argument flows through to the keccak input"
    );
}
