//! V3 Rewards fingerprint sensitivity tests.
//!
//! Each test pins one field of the V3 fingerprint contract. A change to a
//! single bound field must change the computed fingerprint. Thus the dedup
//! guard in `check_and_record_metadata_fingerprint` treats two metadata-txes
//! that differ in that field for the same `fb_hash` as contradictory.
//!

use alloy_primitives::{address, b256, Bytes, B256, U256};
use outbe_primitives::consensus_metadata::{
    CertifiedParentAccountingMetadata, ParentParticipationProof,
};
use outbe_rewards::runtime::{
    check_and_record_metadata_fingerprint, compute_metadata_fingerprint, MetadataFingerprintOutcome,
};

mod support;

use support::with_block;

fn base_metadata() -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        finalized_block_number: 42,
        finalized_block_hash: b256!(
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ),
        finalized_epoch: 8,
        finalized_view: 1010,
        parent_view: 1009,
        ordered_committee: vec![
            address!("0x1111111111111111111111111111111111111111"),
            address!("0x2222222222222222222222222222222222222222"),
            address!("0x3333333333333333333333333333333333333333"),
            address!("0x4444444444444444444444444444444444444444"),
        ],
        signer_bitmap: vec![1, 1, 1, 0],
        proof: Bytes::new(),
        committee_set_hash: b256!(
            "0xc0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0"
        ),
        vrf_material_version: 7,
        vrf_group_public_key_hash: b256!(
            "0xdadadadadadadadadadadadadadadadadadadadadadadadadadadadadadadada"
        ),
        proof_kind: ParentParticipationProof::Finalization,
        missed_proposers: vec![],
    }
}

const VRF_PROOF_HASH_A: B256 =
    b256!("0x1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa");

// ---------------------------------------------------------------------------
// Flipping a single bit in `signer_bitmap` changes the
// fingerprint. Consequently, a second metadata-tx with the perturbed
// bitmap for the same `fb_hash` is contradictory (no double-credit).
// ---------------------------------------------------------------------------

#[test]
fn v2_rewards_fingerprint_changes_on_signer_bitmap_change() {
    let m1 = base_metadata();
    let mut m2 = m1.clone();
    m2.signer_bitmap = vec![1, 1, 1, 1];

    let fp1 = compute_metadata_fingerprint(&m1, U256::from(100u64), VRF_PROOF_HASH_A);
    let fp2 = compute_metadata_fingerprint(&m2, U256::from(100u64), VRF_PROOF_HASH_A);
    assert_ne!(
        fp1, fp2,
        "V3 fingerprint must change when signer_bitmap changes"
    );

    // End-to-end through the guard: second call with perturbed bitmap
    // for the same fb_hash is contradictory.
    with_block(2, |ctx| {
        let outcome =
            check_and_record_metadata_fingerprint(&ctx, &m1, U256::from(100u64), VRF_PROOF_HASH_A)
                .unwrap();
        assert_eq!(outcome, MetadataFingerprintOutcome::Fresh);

        let err =
            check_and_record_metadata_fingerprint(&ctx, &m2, U256::from(100u64), VRF_PROOF_HASH_A)
                .unwrap_err();
        assert!(
            format!("{err}").contains("contradictory consensus metadata"),
            "perturbed bitmap must trigger contradictory-fatal; got: {err}"
        );
    });
}

// ---------------------------------------------------------------------------
// Switching `proof_kind` (Finalization <->
// CertifiedNotarization) changes the fingerprint.
// ---------------------------------------------------------------------------

#[test]
fn v2_rewards_fingerprint_changes_on_proof_type_change() {
    let mut m_fin = base_metadata();
    m_fin.proof_kind = ParentParticipationProof::Finalization;
    let mut m_notar = base_metadata();
    m_notar.proof_kind = ParentParticipationProof::CertifiedNotarization;

    let fp_fin = compute_metadata_fingerprint(&m_fin, U256::from(100u64), VRF_PROOF_HASH_A);
    let fp_notar = compute_metadata_fingerprint(&m_notar, U256::from(100u64), VRF_PROOF_HASH_A);
    assert_ne!(
        fp_fin, fp_notar,
        "V3 fingerprint must change when proof_kind changes"
    );
}

// ---------------------------------------------------------------------------
// Changing `vrf_material_version` OR
// `vrf_group_public_key_hash` (the "seed hash") changes the fingerprint.
// ---------------------------------------------------------------------------

#[test]
fn v2_rewards_fingerprint_changes_on_vrf_material_or_seed_hash_change() {
    let m_base = base_metadata();
    let fp_base = compute_metadata_fingerprint(&m_base, U256::from(100u64), VRF_PROOF_HASH_A);

    // Bump vrf_material_version.
    let mut m_bump_material = m_base.clone();
    m_bump_material.vrf_material_version += 1;
    let fp_bump_material =
        compute_metadata_fingerprint(&m_bump_material, U256::from(100u64), VRF_PROOF_HASH_A);
    assert_ne!(
        fp_base, fp_bump_material,
        "V3 fingerprint must change when vrf_material_version changes"
    );

    // Change vrf_group_public_key_hash (seed hash).
    let mut m_swap_seed = m_base.clone();
    m_swap_seed.vrf_group_public_key_hash =
        b256!("0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
    let fp_swap_seed =
        compute_metadata_fingerprint(&m_swap_seed, U256::from(100u64), VRF_PROOF_HASH_A);
    assert_ne!(
        fp_base, fp_swap_seed,
        "V3 fingerprint must change when vrf_group_public_key_hash (seed hash) changes"
    );
}

// ---------------------------------------------------------------------------
// The fingerprint includes the canonical VRF proof hash
// (`outbe_consensus::proof::canonical_vrf_proof_hash_v2(VrfProof)`).
// Changing the proof hash argument while keeping the metadata identical
// must change the fingerprint. This proves that the proof hash is bound.
// ---------------------------------------------------------------------------

#[test]
fn v2_certificate_fingerprint_includes_valid_vrf_material_and_proof_hash() {
    let m = base_metadata();
    let vrf_hash_a = VRF_PROOF_HASH_A;
    let vrf_hash_b = b256!("0x2222bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222bbbb");
    assert_ne!(vrf_hash_a, vrf_hash_b);

    let fp_a = compute_metadata_fingerprint(&m, U256::from(100u64), vrf_hash_a);
    let fp_b = compute_metadata_fingerprint(&m, U256::from(100u64), vrf_hash_b);
    assert_ne!(
        fp_a, fp_b,
        "V3 fingerprint must include canonical_vrf_proof_hash"
    );

    // Cross-check: with the same proof hash AND the same metadata, the
    // fingerprint is deterministic (re-computing returns the same B256).
    let fp_a_again = compute_metadata_fingerprint(&m, U256::from(100u64), vrf_hash_a);
    assert_eq!(fp_a, fp_a_again, "fingerprint must be deterministic");
}

// ---------------------------------------------------------------------------
// Fingerprint guard characterization (moved from the `runtime` unit tests).
// ---------------------------------------------------------------------------

/// [`base_metadata`] without committee and VRF material.
fn meta_v1() -> CertifiedParentAccountingMetadata {
    CertifiedParentAccountingMetadata {
        committee_set_hash: B256::ZERO,
        vrf_material_version: 0,
        vrf_group_public_key_hash: B256::ZERO,
        ..base_metadata()
    }
}

#[test]
fn fingerprint_first_call_is_fresh() {
    with_block(1, |ctx| {
        let m = meta_v1();
        let outcome =
            check_and_record_metadata_fingerprint(&ctx, &m, U256::from(100u64), B256::ZERO)
                .unwrap();
        assert_eq!(outcome, MetadataFingerprintOutcome::Fresh);
        let stored = ctx
            .storage
            .contract::<outbe_rewards::schema::Rewards>()
            .metadata_fingerprint_for_block
            .read(&m.finalized_block_hash)
            .unwrap();
        assert_ne!(stored, B256::ZERO);
    });
}

#[test]
fn fingerprint_replay_is_identical_replay() {
    with_block(1, |ctx| {
        let m = meta_v1();
        let _ = check_and_record_metadata_fingerprint(&ctx, &m, U256::from(100u64), B256::ZERO)
            .unwrap();
        let outcome =
            check_and_record_metadata_fingerprint(&ctx, &m, U256::from(100u64), B256::ZERO)
                .unwrap();
        assert_eq!(outcome, MetadataFingerprintOutcome::IdenticalReplay);
        let outcome3 =
            check_and_record_metadata_fingerprint(&ctx, &m, U256::from(100u64), B256::ZERO)
                .unwrap();
        assert_eq!(outcome3, MetadataFingerprintOutcome::IdenticalReplay);
    });
}

#[test]
fn fingerprint_mismatch_for_same_fb_hash_is_fatal() {
    with_block(1, |ctx| {
        let m1 = meta_v1();
        let _ = check_and_record_metadata_fingerprint(&ctx, &m1, U256::from(100u64), B256::ZERO)
            .unwrap();

        // Mutate `missed_proposers` (canonical content) - same fb_hash.
        let mut m2 = m1.clone();
        m2.missed_proposers = vec![outbe_primitives::consensus_metadata::MissedProposerEvent {
            view: 1,
            validator: address!("0x9999999999999999999999999999999999999999"),
        }];
        let err = check_and_record_metadata_fingerprint(&ctx, &m2, U256::from(100u64), B256::ZERO)
            .unwrap_err();
        assert!(
            format!("{err}").contains("contradictory consensus metadata"),
            "expected contradictory-fatal, got: {err}"
        );

        // Different fee sum, original metadata - also contradictory.
        let err2 = check_and_record_metadata_fingerprint(&ctx, &m1, U256::from(101u64), B256::ZERO)
            .unwrap_err();
        assert!(format!("{err2}").contains("contradictory consensus metadata"));
    });
}

/// The V3 fingerprint binds the base certificate's signer bitmap.
/// Late credits must use their separate authenticated phase, never a
/// changed bitmap in a replay of the original CPA metadata.
#[test]
fn fingerprint_signer_bitmap_variation_is_contradictory_v3() {
    with_block(1, |ctx| {
        let m1 = meta_v1();
        let _ = check_and_record_metadata_fingerprint(&ctx, &m1, U256::from(100u64), B256::ZERO)
            .unwrap();

        let mut m2 = m1.clone();
        m2.signer_bitmap = vec![1, 1, 1, 1];
        let err = check_and_record_metadata_fingerprint(&ctx, &m2, U256::from(100u64), B256::ZERO)
            .unwrap_err();
        assert!(
            format!("{err}").contains("contradictory consensus metadata"),
            "V3: signer_bitmap variation must trigger contradictory-fatal; got: {err}"
        );
    });
}

#[test]
fn fingerprint_distinct_fb_hashes_are_independent() {
    with_block(1, |ctx| {
        let mut m1 = meta_v1();
        let mut m2 = meta_v1();
        m2.finalized_block_hash =
            b256!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        m1.finalized_block_number = 42;
        m2.finalized_block_number = 43;

        let r1 = check_and_record_metadata_fingerprint(&ctx, &m1, U256::from(100u64), B256::ZERO)
            .unwrap();
        let r2 = check_and_record_metadata_fingerprint(&ctx, &m2, U256::from(200u64), B256::ZERO)
            .unwrap();
        assert_eq!(r1, MetadataFingerprintOutcome::Fresh);
        assert_eq!(r2, MetadataFingerprintOutcome::Fresh);
    });
}

#[test]
fn fingerprint_canonical_encoding_is_length_prefix_safe() {
    // [A,B] || [C] should NOT collide with [A] || [B,C] under our
    // canonical encoding because both lists carry length prefixes.
    let a = address!("0x1111111111111111111111111111111111111111");
    let b = address!("0x2222222222222222222222222222222222222222");
    let c = address!("0x3333333333333333333333333333333333333333");

    let m_x = CertifiedParentAccountingMetadata {
        ordered_committee: vec![a, b],
        missed_proposers: vec![outbe_primitives::consensus_metadata::MissedProposerEvent {
            view: 1,
            validator: c,
        }],
        ..meta_v1()
    };
    let m_y = CertifiedParentAccountingMetadata {
        ordered_committee: vec![a],
        missed_proposers: vec![
            outbe_primitives::consensus_metadata::MissedProposerEvent {
                view: 1,
                validator: b,
            },
            outbe_primitives::consensus_metadata::MissedProposerEvent {
                view: 2,
                validator: c,
            },
        ],
        ..meta_v1()
    };

    let fp_x = compute_metadata_fingerprint(&m_x, U256::ZERO, B256::ZERO);
    let fp_y = compute_metadata_fingerprint(&m_y, U256::ZERO, B256::ZERO);
    assert_ne!(
        fp_x, fp_y,
        "length-prefix collision: lists [A,B]||[C] should not equal [A]||[B,C]"
    );
}
