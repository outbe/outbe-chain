//! End-to-end tests for the consensus-equivocation evidence verifiers added for
//! `submitConflictingNotarizeEvidence`, `submitConflictingFinalizeEvidence`,
//! and `submitNullifyFinalizeEvidence`. Each proves a validator double-voted
//! within one view and jails + slashes it.

use alloy_primitives::{address, Address, B256, U256};
use alloy_sol_types::SolError;
use blst::min_pk::SecretKey;
use commonware_cryptography::{bls12381, Signer as _};
use commonware_utils::ordered::Set;
use outbe_primitives::equivocation_evidence::canonical_equivocation_evidence_hash;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::StorageHandle;
use outbe_simplex::proof::{finalize_namespace, notarize_namespace, nullify_namespace};
use outbe_simplex::test_harness::committee_entries;
use outbe_slashindicator::precompile::ISlashIndicator;
use outbe_slashindicator::schema::SlashIndicator;
use outbe_slashindicator::test_signing;
use outbe_slashindicator::test_signing::nullify_payload as nullify_bytes;
use outbe_staking::contract::Staking;
use outbe_system_common::addresses::STAKING_ADDRESS;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::state::CommitteeSnapshot;
use outbe_validatorset::test_support::{test_lifecycle_of, StorageOverrides};
use outbe_validatorset::ValidatorLifecycle;

mod support;
use support::{
    assert_selector, evidence_already_processed, register_active_submitter, with_storage,
};

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
    let bonded = U256::from(1_000_000u64);
    vs.test_seed_authoritative_min_stake(U256::from(1)).unwrap();
    vs.test_register_active_validator(ACCUSED, &pk, bonded)
        .unwrap();
    // The registration seeds Staking.stake_amount. Slash burns the same
    // deposit from the STAKING_ADDRESS escrow and subtracts it from total.
    let staking = Staking::new(storage.clone());
    staking.total_staked.write(bonded).unwrap();
    staking
        .storage
        .increase_balance(STAKING_ADDRESS, bonded)
        .unwrap();

    // evidence precompiles require an ACTIVE-validator submitter.
    register_active_submitter(&mut vs, SUBMITTER, Some(U256::from(1))).unwrap();

    // the evidence verifier resolves the committee for the vote's epoch via
    // the snapshot ring. Seed it across the retained ring window so any test
    // epoch resolves to the committee the evidence is signed against.
    let snap = committee_snapshot();
    for member in &snap.committee {
        if !vs.is_validator(member.address).unwrap() {
            vs.register_validator(OWNER, member.address, &member.consensus_pubkey)
                .unwrap();
            vs.test_seed_authoritative_bonded(member.address, U256::from(1))
                .unwrap();
            vs.activate_validator_via_boundary_for_test(member.address)
                .unwrap();
        }
    }
    vs.test_seed_committee_ring(OWNER, &snap).unwrap();
}

fn assert_jailed_once(storage: &StorageHandle) {
    assert!(matches!(
        test_lifecycle_of(storage.clone(), ACCUSED).unwrap(),
        ValidatorLifecycle::JailRetained(_)
    ));
    let si = SlashIndicator::new(storage.clone());
    assert_eq!(si.felony_count.read(&ACCUSED).unwrap(), 1);
    // 8% of the funded 1_000_000 deposit. One check for every real slash.
    assert_eq!(
        Staking::new(storage.clone()).get_stake(ACCUSED).unwrap(),
        U256::from(920_000u64)
    );
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
        assert_selector(
            &si.submit_conflicting_notarize_evidence(SUBMITTER, &b2, &b1)
                .unwrap_err(),
            evidence_already_processed(),
        );
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
        assert_selector(
            &err,
            ISlashIndicator::ConflictingVotesMustBeDifferentProposals::SELECTOR,
        );
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
        assert_selector(&err, ISlashIndicator::EvidenceSignerMismatch::SELECTOR);
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
        assert_selector(&err, ISlashIndicator::SubmitterNotActiveValidator::SELECTOR);
    });
}

/// Exercise every public equivocation entry point with its actual signed wire format.
fn submit_pair(
    si: &mut SlashIndicator<'_>,
    method: usize,
    epoch: u64,
    view: u64,
) -> (Vec<u8>, Vec<u8>, outbe_primitives::error::Result<()>) {
    let sk = accused_sk();
    let (ns1, ns2) = match method {
        0 | 2 => (b"_NOTARIZE".as_slice(), b"_NOTARIZE".as_slice()),
        1 => (b"_NOTARIZE".as_slice(), b"_NULLIFY".as_slice()),
        3 => (b"_FINALIZE".as_slice(), b"_FINALIZE".as_slice()),
        4 => (b"_NULLIFY".as_slice(), b"_FINALIZE".as_slice()),
        _ => unreachable!(),
    };
    let payload = |ns, digest| {
        if ns == b"_NULLIFY" {
            nullify_bytes(epoch, view)
        } else {
            proposal_bytes(epoch, view, view - 1, digest)
        }
    };
    let a = evidence_block(&sk, &ns_with(ns1), &payload(ns1, 0x11));
    let b = evidence_block(&sk, &ns_with(ns2), &payload(ns2, 0x22));
    let result = submit_raw(si, method, &a, &b);
    (a, b, result)
}

fn submit_raw(
    si: &mut SlashIndicator<'_>,
    method: usize,
    a: &[u8],
    b: &[u8],
) -> outbe_primitives::error::Result<()> {
    match method {
        0 => si.submit_double_proposal_evidence(SUBMITTER, a, b),
        1 => si.submit_conflicting_vote_evidence(SUBMITTER, a, b),
        2 => si.submit_conflicting_notarize_evidence(SUBMITTER, a, b),
        3 => si.submit_conflicting_finalize_evidence(SUBMITTER, a, b),
        4 => si.submit_nullify_finalize_evidence(SUBMITTER, a, b),
        _ => unreachable!(),
    }
}

fn epoch_key(epoch: u64) -> B256 {
    alloy_primitives::keccak256(
        [
            b"outbe:equivocation-epoch-felony:v1".as_slice(),
            ACCUSED.as_slice(),
            &epoch.to_be_bytes(),
        ]
        .concat(),
    )
}

#[test]
fn epoch_felony_guard_covers_all_five_entrypoints_and_cross_method_pairs() {
    for first in 0..5 {
        for second in 0..5 {
            with_storage(|storage| {
                setup(storage.clone(), &accused_sk());
                let mut si = SlashIndicator::new(storage.clone());
                let (first_a, first_b, first_result) = submit_pair(&mut si, first, 1, 5);
                first_result.unwrap();
                assert_selector(
                    &submit_raw(&mut si, first, &first_b, &first_a).unwrap_err(),
                    evidence_already_processed(),
                );
                assert_jailed_once(&storage);
                assert!(si.is_evidence_processed(epoch_key(1)).unwrap());
                let (a, b, result) = submit_pair(&mut si, second, 1, 6);
                let expected = ISlashIndicator::ValidatorAlreadyPenalized {
                    validator: ACCUSED,
                    epoch: 1,
                }
                .abi_encode();
                assert!(matches!(
                    result,
                    Err(PrecompileError::RevertBytes(bytes)) if bytes.as_ref() == expected
                ));
                assert!(!si
                    .is_evidence_processed(canonical_equivocation_evidence_hash(&a, &b))
                    .unwrap());
                assert_jailed_once(&storage);
            });
        }
    }
}

/// The validated boundary seam preserves every other ACTIVE participant.
fn exclude_accused(vs: &mut ValidatorSet<'_>) {
    let incoming: Vec<_> = vs
        .get_active_consensus_set()
        .unwrap()
        .into_iter()
        .filter(|v| {
            v.validator_address != ACCUSED
                && v.status == outbe_validatorset::runtime::status::ACTIVE
        })
        .map(|v| v.validator_address)
        .collect();
    vs.test_activate_validated_boundary_set(&incoming, B256::repeat_byte(0x42), 1)
        .unwrap();
}

fn assert_unmarked(si: &SlashIndicator<'_>, a: &[u8], b: &[u8], epoch: u64) {
    assert!(!si
        .is_evidence_processed(canonical_equivocation_evidence_hash(a, b))
        .unwrap());
    assert!(!si.is_evidence_processed(epoch_key(epoch)).unwrap());
}

#[test]
fn voluntary_exit_and_unbonding_remain_slashable_in_every_entrypoint() {
    for method in 0..5 {
        for full_unstake in [false, true] {
            for boundary in [false, true] {
                with_storage(|storage| {
                    setup(storage.clone(), &accused_sk());
                    let mut vs = ValidatorSet::new(storage.clone());
                    let mut staking = Staking::new(storage.clone());
                    staking.config_unbonding_period.write(100).unwrap();
                    if full_unstake {
                        staking.unstake(ACCUSED, U256::from(1_000_000)).unwrap();
                    } else {
                        // A live queued withdrawal and bonded funds must both lose 8%.
                        staking.unstake(ACCUSED, U256::from(200_000)).unwrap();
                        vs.deactivate_validator(ACCUSED, ACCUSED).unwrap();
                    }
                    assert!(matches!(
                        vs.validator_lifecycle(ACCUSED).unwrap(),
                        ValidatorLifecycle::Exiting(_)
                    ));
                    if boundary {
                        exclude_accused(&mut vs);
                        vs.advance_epoch(1, 2).unwrap();
                        assert!(matches!(
                            vs.validator_lifecycle(ACCUSED).unwrap(),
                            ValidatorLifecycle::Unbonding(_)
                        ));
                    }
                    let mut si = SlashIndicator::new(storage.clone());
                    submit_pair(&mut si, method, 1, 5).2.unwrap();
                    assert_eq!(si.get_felony_count(ACCUSED).unwrap(), 1);
                    let remaining = if full_unstake { 0 } else { 736_000 };
                    let queued = if full_unstake { 920_000 } else { 184_000 };
                    assert_eq!(staking.get_stake(ACCUSED).unwrap(), U256::from(remaining));
                    assert_eq!(
                        staking.unbonding_amount.read(&0).unwrap(),
                        U256::from(queued)
                    );
                    assert!(si.is_evidence_processed(epoch_key(1)).unwrap());
                    assert!(matches!(
                        vs.validator_lifecycle(ACCUSED).unwrap(),
                        ValidatorLifecycle::Exiting(_) | ValidatorLifecycle::Unbonding(_)
                    ));
                });
            }
        }
    }
}

#[test]
fn full_unstake_after_first_felony_cannot_consume_a_second_pair() {
    with_storage(|storage| {
        setup(storage.clone(), &accused_sk());
        let mut si = SlashIndicator::new(storage.clone());
        submit_pair(&mut si, 2, 1, 5).2.unwrap();
        let mut staking = Staking::new(storage.clone());
        staking.config_unbonding_period.write(100).unwrap();
        staking.unstake(ACCUSED, U256::from(920_000)).unwrap();
        assert!(matches!(
            test_lifecycle_of(storage.clone(), ACCUSED).unwrap(),
            ValidatorLifecycle::Exiting(_)
        ));
        let (a, b, result) = submit_pair(&mut si, 3, 1, 6);
        assert_selector(
            &result.unwrap_err(),
            ISlashIndicator::ValidatorAlreadyPenalized::SELECTOR,
        );
        assert!(!si
            .is_evidence_processed(canonical_equivocation_evidence_hash(&a, &b))
            .unwrap());
        assert_eq!(si.get_felony_count(ACCUSED).unwrap(), 1);
        assert_eq!(
            staking.unbonding_amount.read(&0).unwrap(),
            U256::from(920_000)
        );
    });
}

#[test]
fn rejoining_preserves_old_epoch_guard_but_allows_new_epoch_felony() {
    with_storage(|storage| {
        setup(storage.clone(), &accused_sk());
        let mut si = SlashIndicator::new(storage.clone());
        submit_pair(&mut si, 2, 1, 5).2.unwrap();
        let mut vs = ValidatorSet::new(storage.clone());
        exclude_accused(&mut vs);
        assert!(matches!(
            vs.validator_lifecycle(ACCUSED).unwrap(),
            ValidatorLifecycle::Jail(_)
        ));
        vs.advance_epoch(1, 2).unwrap();
        Staking::new(storage.clone())
            .unjail_validator(ACCUSED)
            .unwrap();
        assert!(matches!(
            vs.validator_lifecycle(ACCUSED).unwrap(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ));
        vs.confirm_validator_ready(ACCUSED).unwrap();
        assert!(matches!(
            vs.validator_lifecycle(ACCUSED).unwrap(),
            ValidatorLifecycle::Joining(_)
        ));
        vs.advance_epoch(2, 3).unwrap();
        vs.activate_validator_via_boundary_for_test(ACCUSED)
            .unwrap();
        assert!(matches!(
            vs.validator_lifecycle(ACCUSED).unwrap(),
            ValidatorLifecycle::Active(_)
        ));
        // Restore the retained test committee namespaces after the activation fixture.
        vs.test_seed_committee_ring(OWNER, &committee_snapshot())
            .unwrap();
        let (a, b, result) = submit_pair(&mut si, 3, 1, 6);
        assert_selector(
            &result.unwrap_err(),
            ISlashIndicator::ValidatorAlreadyPenalized::SELECTOR,
        );
        assert!(!si
            .is_evidence_processed(canonical_equivocation_evidence_hash(&a, &b))
            .unwrap());
        assert_eq!(si.get_felony_count(ACCUSED).unwrap(), 1);
        submit_pair(&mut si, 3, 3, 7).2.unwrap();
        assert_eq!(si.get_felony_count(ACCUSED).unwrap(), 2);
        assert_eq!(
            Staking::new(storage.clone()).get_stake(ACCUSED).unwrap(),
            U256::from(846_400)
        );
        assert!(si.is_evidence_processed(epoch_key(3)).unwrap());
    });
}

#[test]
fn pending_statuses_reject_before_missing_committee_and_preserve_evidence() {
    for joining in [false, true] {
        for method in 0..5 {
            with_storage(|storage| {
                setup(storage.clone(), &accused_sk());
                let mut vs = ValidatorSet::new(storage.clone());
                vs.jail_validator(ACCUSED).unwrap();
                exclude_accused(&mut vs);
                Staking::new(storage.clone())
                    .unjail_validator(ACCUSED)
                    .unwrap();
                if joining {
                    vs.confirm_validator_ready(ACCUSED).unwrap();
                }
                let status = vs
                    .validator_lifecycle(ACCUSED)
                    .unwrap()
                    .stored_status()
                    .unwrap();
                let mut si = SlashIndicator::new(storage.clone());
                for epoch in [1, 99] {
                    let (a, b, result) = submit_pair(&mut si, method, epoch, 5);
                    let expected = ISlashIndicator::ValidatorNotSlashable {
                        validator: ACCUSED,
                        status,
                    }
                    .abi_encode();
                    assert!(matches!(
                        result,
                        Err(PrecompileError::RevertBytes(bytes)) if bytes.as_ref() == expected
                    ));
                    assert_unmarked(&si, &a, &b, epoch);
                    assert_eq!(si.get_felony_count(ACCUSED).unwrap(), 0);
                    assert_eq!(
                        Staking::new(storage.clone()).get_stake(ACCUSED).unwrap(),
                        U256::from(1_000_000)
                    );
                }
                vs.activate_validator_via_boundary_for_test(ACCUSED)
                    .unwrap();
                vs.test_seed_committee_ring(OWNER, &committee_snapshot())
                    .unwrap();
                submit_pair(&mut si, method, 1, 5).2.unwrap();
                assert_jailed_once(&storage);
            });
        }
    }
}

#[test]
fn inactive_status_rejects_without_markers_then_same_evidence_survives_reactivation() {
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    for method in 0..5 {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_block_number(1);
        provider.enter(|storage| {
            setup(storage.clone(), &accused_sk());
            storage.set_block_timestamp(U256::from(1)).unwrap();
            let mut staking = Staking::new(storage.clone());
            staking.config_unbonding_period.write(100).unwrap();
            staking.unstake(ACCUSED, U256::from(1_000_000)).unwrap();
            let mut vs = ValidatorSet::new(storage.clone());
            exclude_accused(&mut vs);
            storage.set_block_timestamp(U256::from(101)).unwrap();
            staking.claim_unbonded(ACCUSED).unwrap();
            assert!(matches!(
                vs.validator_lifecycle(ACCUSED).unwrap(),
                ValidatorLifecycle::Inactive(_)
            ));
            let status = vs
                .validator_lifecycle(ACCUSED)
                .unwrap()
                .stored_status()
                .unwrap();
            let mut si = SlashIndicator::new(storage.clone());
            for epoch in [1, 99] {
                let (a, b, result) = submit_pair(&mut si, method, epoch, 5);
                let expected = ISlashIndicator::ValidatorNotSlashable {
                    validator: ACCUSED,
                    status,
                }
                .abi_encode();
                assert!(matches!(
                    result,
                    Err(PrecompileError::RevertBytes(bytes)) if bytes.as_ref() == expected
                ));
                assert_unmarked(&si, &a, &b, epoch);
            }
        });
        provider.set_block_number(100);
        provider.enter(|storage| {
            let mut vs = ValidatorSet::new(storage.clone());
            let mut epoch = vs.epoch_snapshot().unwrap();
            epoch.number = U256::from(3);
            epoch.start_block = 100;
            vs.test_set_epoch_snapshot(epoch).unwrap();
            // Re-register the same identity through the canonical registration facade.
            vs.test_register_active_validator(
                ACCUSED,
                &accused_sk().sk_to_pk().to_bytes(),
                U256::from(1_000_000),
            )
            .unwrap();
            let staking = Staking::new(storage.clone());
            staking.total_staked.write(U256::from(1_000_000)).unwrap();
            staking
                .storage
                .increase_balance(STAKING_ADDRESS, U256::from(1_000_000))
                .unwrap();
            vs.test_seed_committee_ring(OWNER, &committee_snapshot())
                .unwrap();
            let mut si = SlashIndicator::new(storage.clone());
            submit_pair(&mut si, method, 1, 5).2.unwrap();
            assert_jailed_once(&storage);
        });
    }
}

#[test]
fn failed_felony_rolls_back_both_markers_and_penalty_effects() {
    for method in 0..5 {
        with_storage(|storage| {
            setup(storage.clone(), &accused_sk());
            // A provider-side burn failure occurs after both marker writes and jail.
            storage
                .decrease_balance(STAKING_ADDRESS, U256::from(1_000_000))
                .unwrap();
            let mut si = SlashIndicator::new(storage.clone());
            let (a, b, result) = submit_pair(&mut si, method, 1, 5);
            assert!(result.is_err());
            assert_unmarked(&si, &a, &b, 1);
            assert_eq!(si.get_felony_count(ACCUSED).unwrap(), 0);
            assert_eq!(
                Staking::new(storage.clone()).get_stake(ACCUSED).unwrap(),
                U256::from(1_000_000)
            );
            assert!(matches!(
                test_lifecycle_of(storage.clone(), ACCUSED).unwrap(),
                ValidatorLifecycle::Active(_)
            ));
        });
    }
}

#[test]
fn first_equivocation_felony_remains_slashable_after_non_equivocation_jail() {
    for method in 0..5 {
        for retained in [true, false] {
            with_storage(|storage| {
                setup(storage.clone(), &accused_sk());
                let mut vs = ValidatorSet::new(storage.clone());
                assert!(vs.jail_validator_for_tee_expiry(ACCUSED).unwrap());
                if retained {
                    assert!(matches!(
                        vs.validator_lifecycle(ACCUSED).unwrap(),
                        ValidatorLifecycle::JailRetained(_)
                    ));
                } else {
                    exclude_accused(&mut vs);
                    assert!(matches!(
                        vs.validator_lifecycle(ACCUSED).unwrap(),
                        ValidatorLifecycle::Jail(_)
                    ));
                }
                let before = vs.validator_lifecycle(ACCUSED).unwrap();
                let mut si = SlashIndicator::new(storage.clone());
                assert_eq!(si.get_felony_count(ACCUSED).unwrap(), 0);
                assert!(!si.is_evidence_processed(epoch_key(1)).unwrap());
                let (a, b, result) = submit_pair(&mut si, method, 1, 5);
                result.unwrap();
                assert_eq!(si.get_felony_count(ACCUSED).unwrap(), 1);
                assert_eq!(
                    Staking::new(storage.clone()).get_stake(ACCUSED).unwrap(),
                    U256::from(920_000)
                );
                assert!(si.is_evidence_processed(epoch_key(1)).unwrap());
                assert!(si
                    .is_evidence_processed(canonical_equivocation_evidence_hash(&a, &b))
                    .unwrap());
                let after = vs.validator_lifecycle(ACCUSED).unwrap();
                assert_eq!(before.stored_status(), after.stored_status());
            });
        }
    }
}
