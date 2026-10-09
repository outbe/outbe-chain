//! End-to-end tests for the consensus-equivocation evidence verifiers added for
//! `submitConflictingNotarizeEvidence`, `submitConflictingFinalizeEvidence`,
//! and `submitNullifyFinalizeEvidence`. Each proves a validator double-voted
//! within one view and jails + slashes it.

use alloy_primitives::{address, Address, B256, U256};
use blst::min_pk::SecretKey;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::ordered::Set;
use outbe_consensus::proof::{finalize_namespace, notarize_namespace, nullify_namespace};
use outbe_consensus::test_harness::committee_entries;
use outbe_primitives::storage::StorageHandle;
use outbe_slashindicator::schema::SlashIndicator;
use outbe_slashindicator::test_signing;
use outbe_slashindicator::test_signing::nullify_payload as nullify_bytes;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::state::CommitteeSnapshot;
use outbe_validatorset::test_support::{test_lifecycle_of, StorageOverrides};
use outbe_validatorset::ValidatorLifecycle;

mod support;
use support::{register_active_submitter, with_storage};

const OWNER: Address = address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC");
const SUBMITTER: Address = address!("0xDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD");
const ACCUSED: Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
/// The test committee: the vote namespaces bind this set. `setup` writes
/// its snapshot into the ring for every retained epoch, so the evidence verifier
/// rebuilds the same committee and derives the same namespace.
fn committee_keys() -> Vec<bls12381::PrivateKey> {
    (1u64..=4).map(bls12381::PrivateKey::from_seed).collect()
}

fn committee_set() -> Set<bls12381::PublicKey> {
    Set::from_iter_dedup(committee_keys().iter().map(|k| k.public_key()))
}

fn committee_snapshot() -> CommitteeSnapshot {
    let public_keys: Vec<_> = committee_keys().iter().map(|k| k.public_key()).collect();
    CommitteeSnapshot {
        committee: committee_entries(public_keys.iter()),
        vrf_material_version: 1,
        vrf_group_public_key_bytes: vec![0x11; 96],
        vrf_public_polynomial_hash: B256::ZERO,
    }
}

/// Committee-bound consensus sub-namespace. It must match what the
/// SlashIndicator evidence verifier derives from the epoch's committee snapshot.
fn ns_with(suffix: &[u8]) -> Vec<u8> {
    let c = committee_set();
    match suffix {
        b"_NOTARIZE" => notarize_namespace(&c),
        b"_NULLIFY" => nullify_namespace(&c),
        b"_FINALIZE" => finalize_namespace(&c),
        other => panic!("unexpected sub-namespace suffix {other:?}"),
    }
}

/// Build an `EvidenceBlock`: `pubkey[48] || sig[96] || proposal_bytes`.
fn evidence_block(sk: &SecretKey, ns: &[u8], proposal: &[u8]) -> Vec<u8> {
    test_signing::signed_evidence(sk, &sk.sk_to_pk(), ns, proposal, test_signing::POP_DST)
}

/// `epoch || view || parent || digest[32]`.
fn proposal_bytes(epoch: u64, view: u64, parent: u64, digest: u8) -> Vec<u8> {
    test_signing::proposal(epoch, view, parent, [digest; 32])
}

fn accused_sk() -> SecretKey {
    SecretKey::key_gen(&[0x5Au8; 32], &[]).unwrap()
}

fn setup(storage: StorageHandle, sk: &SecretKey) {
    let mut vs = ValidatorSet::new(storage.clone());
    vs.test_configure_registry(OWNER).unwrap();
    let mut epoch = vs.epoch_snapshot().unwrap();
    epoch.number = U256::from(1u64);
    vs.test_set_epoch_snapshot(epoch).unwrap();
    let pk: [u8; 48] = sk.sk_to_pk().to_bytes();
    vs.test_register_active_validator(ACCUSED, &pk, U256::from(1_000_000u64))
        .unwrap();

    // evidence precompiles require an ACTIVE-validator submitter.
    register_active_submitter(&mut vs, SUBMITTER, Some(U256::from(1))).unwrap();

    // the evidence verifier resolves the committee for the vote's epoch via
    // the snapshot ring. Seed it across the retained ring window so any test
    // epoch resolves to the committee the evidence is signed against.
    vs.test_seed_committee_ring(OWNER, &committee_snapshot())
        .unwrap();
}

fn assert_jailed_once(storage: &StorageHandle) {
    assert!(matches!(
        test_lifecycle_of(storage.clone(), ACCUSED).unwrap(),
        ValidatorLifecycle::JailRetained(_)
    ));
    let si = SlashIndicator::new(storage.clone());
    assert_eq!(si.felony_count.read(&ACCUSED).unwrap(), 1);
}

#[test]
fn conflicting_notarize_slashes_and_dedups() {
    with_storage(|storage| {
        let sk = accused_sk();
        setup(storage.clone(), &sk);
        let b1 = evidence_block(&sk, &ns_with(b"_NOTARIZE"), &proposal_bytes(1, 5, 4, 0xA1));
        let b2 = evidence_block(&sk, &ns_with(b"_NOTARIZE"), &proposal_bytes(1, 5, 4, 0xB2));

        let mut si = SlashIndicator::new(storage.clone());
        si.submit_conflicting_notarize_evidence(SUBMITTER, &b1, &b2)
            .expect("conflicting notarize must slash");
        assert_jailed_once(&storage);

        // SlashIndicator rejects a replay (either order).
        let mut si = SlashIndicator::new(storage.clone());
        assert!(si
            .submit_conflicting_notarize_evidence(SUBMITTER, &b2, &b1)
            .unwrap_err()
            .to_string()
            .contains("already processed"));
    });
}

#[test]
fn conflicting_finalize_slashes() {
    with_storage(|storage| {
        let sk = accused_sk();
        setup(storage.clone(), &sk);
        let b1 = evidence_block(&sk, &ns_with(b"_FINALIZE"), &proposal_bytes(1, 6, 5, 0x11));
        let b2 = evidence_block(&sk, &ns_with(b"_FINALIZE"), &proposal_bytes(1, 6, 5, 0x22));
        let mut si = SlashIndicator::new(storage.clone());
        si.submit_conflicting_finalize_evidence(SUBMITTER, &b1, &b2)
            .expect("conflicting finalize must slash");
        assert_jailed_once(&storage);
    });
}

#[test]
fn nullify_finalize_slashes() {
    with_storage(|storage| {
        let sk = accused_sk();
        setup(storage.clone(), &sk);
        let nullify = evidence_block(&sk, &ns_with(b"_NULLIFY"), &nullify_bytes(1, 7));
        let finalize = evidence_block(&sk, &ns_with(b"_FINALIZE"), &proposal_bytes(1, 7, 6, 0x33));
        let mut si = SlashIndicator::new(storage.clone());
        si.submit_nullify_finalize_evidence(SUBMITTER, &nullify, &finalize)
            .expect("nullify+finalize must slash");
        assert_jailed_once(&storage);
    });
}

#[test]
fn identical_notarize_proposals_rejected() {
    with_storage(|storage| {
        let sk = accused_sk();
        setup(storage.clone(), &sk);
        let p = proposal_bytes(1, 5, 4, 0xA1);
        let b1 = evidence_block(&sk, &ns_with(b"_NOTARIZE"), &p);
        let b2 = evidence_block(&sk, &ns_with(b"_NOTARIZE"), &p);
        let mut si = SlashIndicator::new(storage.clone());
        let err = si
            .submit_conflicting_notarize_evidence(SUBMITTER, &b1, &b2)
            .unwrap_err();
        assert!(err.to_string().contains("different proposals"));
    });
}

#[test]
fn wrong_namespace_signature_rejected() {
    with_storage(|storage| {
        let sk = accused_sk();
        setup(storage.clone(), &sk);
        // Finalize-signed blocks submitted as conflicting NOTARIZE -> verify fails.
        let b1 = evidence_block(&sk, &ns_with(b"_FINALIZE"), &proposal_bytes(1, 5, 4, 0xA1));
        let b2 = evidence_block(&sk, &ns_with(b"_FINALIZE"), &proposal_bytes(1, 5, 4, 0xB2));
        let mut si = SlashIndicator::new(storage.clone());
        assert!(si
            .submit_conflicting_notarize_evidence(SUBMITTER, &b1, &b2)
            .is_err());
    });
}

#[test]
fn different_signers_rejected() {
    with_storage(|storage| {
        let sk = accused_sk();
        setup(storage.clone(), &sk);
        let other = SecretKey::key_gen(&[0x99u8; 32], &[]).unwrap();
        let b1 = evidence_block(&sk, &ns_with(b"_NOTARIZE"), &proposal_bytes(1, 5, 4, 0xA1));
        let b2 = evidence_block(
            &other,
            &ns_with(b"_NOTARIZE"),
            &proposal_bytes(1, 5, 4, 0xB2),
        );
        let mut si = SlashIndicator::new(storage.clone());
        let err = si
            .submit_conflicting_notarize_evidence(SUBMITTER, &b1, &b2)
            .unwrap_err();
        assert!(err.to_string().contains("same signer"));
    });
}

#[test]
fn non_active_submitter_is_rejected() {
    with_storage(|storage| {
        let sk = accused_sk();
        setup(storage.clone(), &sk);
        // An address that is not a registered/ACTIVE validator.
        let outsider = address!("0x9999999999999999999999999999999999999999");
        let b1 = evidence_block(&sk, &ns_with(b"_NOTARIZE"), &proposal_bytes(1, 5, 4, 0xA1));
        let b2 = evidence_block(&sk, &ns_with(b"_NOTARIZE"), &proposal_bytes(1, 5, 4, 0xB2));
        let mut si = SlashIndicator::new(storage.clone());
        let err = si
            .submit_conflicting_notarize_evidence(outsider, &b1, &b2)
            .unwrap_err();
        assert!(
            err.to_string().contains("not an ACTIVE validator"),
            "got: {err:?}"
        );
    });
}
