use alloy_primitives::{address, b256, Address, B256, U256};
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::finalized_guard_ring::{test_ring_hash, RingPosition};
use outbe_primitives::storage::hashmap::{HashMapStorageProvider, MutationPrefixViews};
use outbe_primitives::storage::StorageHandle;
use outbe_staking::contract::Staking;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::test_support::test_lifecycle_of;
use outbe_validatorset::{StakeProjection, ValidatorLifecycle};

use crate::hooks;
use crate::schema::SlashIndicator;
use crate::test_signing::{self, signed_evidence, POP_DST};

const CHAIN_ID: u64 = 1;

const VAL_A: Address = address!("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
const VAL_B: Address = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
const OWNER: Address = address!("0xCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC");
const SUBMITTER: Address = address!("0xDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD");

/// Runs `f` inside a fresh HashMapStorageProvider context.
fn with_storage<R>(f: impl FnOnce(StorageHandle) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(1);
    StorageHandle::enter(&mut storage, f)
}

/// Registers `validator` in ValidatorSet and activates it.
/// Also sets a non-zero stake in the Staking contract so slash_stake has something to work with.
fn register_and_activate(storage: StorageHandle, validator: Address, seed: u8) {
    register_and_activate_with_stake(storage, validator, seed, U256::from(1_000_000u64));
}

/// Registers `validator` in ValidatorSet, activates it, and sets the given stake.
fn register_and_activate_with_stake(
    storage: StorageHandle,
    validator: Address,
    seed: u8,
    stake_amount: U256,
) {
    let mut pk = [0u8; 48];
    pk[0] = seed;
    register_and_activate_with_pubkey_and_stake(storage, validator, &pk, stake_amount);
}

fn register_and_activate_with_pubkey_and_stake(
    storage: StorageHandle,
    validator: Address,
    consensus_pubkey: &[u8; 48],
    stake_amount: U256,
) {
    let mut vs = ValidatorSet::new(storage.clone());
    vs.test_configure_registry(OWNER).unwrap();
    register_active_fixture(&mut vs, validator, consensus_pubkey, stake_amount);

    // Give the validator some stake so slash_stake has an effect
    let staking = Staking::new(storage.clone());
    staking
        .stake_amount
        .write(&validator, stake_amount)
        .unwrap();

    // Fund STAKING_ADDRESS so decrease_balance (burn) can succeed during slash.
    staking
        .storage
        .increase_balance(STAKING_ADDRESS, stake_amount)
        .unwrap();
    staking.total_staked.write(stake_amount).unwrap();
    // the evidence precompiles now require an ACTIVE-validator submitter.
    // Register SUBMITTER as ACTIVE so the evidence tests reach the verifier (a
    // distinct test asserts a non-ACTIVE caller is rejected).
    if !vs
        .validator_lifecycle(SUBMITTER)
        .unwrap()
        .is_active_status()
    {
        let mut sub_pk = [0u8; 48];
        sub_pk[0] = 0xEE;
        register_active_fixture(&mut vs, SUBMITTER, &sub_pk, U256::from(1));
    }
}

fn register_active_fixture(
    vs: &mut ValidatorSet<'_>,
    validator: Address,
    consensus_pubkey: &[u8; 48],
    stake: U256,
) {
    vs.test_register_active_validator(validator, consensus_pubkey, stake)
        .unwrap();
}

// ---------------------------------------------------------------------------
// 1. test_slash_proposer_misdemeanor
// ---------------------------------------------------------------------------
/// Reaches the misdemeanor threshold (default 50) without triggering a felony.
/// Verifies the miss count is accumulated and felony_count stays zero.
#[test]
fn test_slash_proposer_misdemeanor() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_A, 1);

        let mut si = SlashIndicator::new(storage.clone());

        // Default misdemeanor threshold is 50
        for _ in 0..50 {
            si.slash_proposer(VAL_A).unwrap();
        }

        assert_eq!(si.get_proposer_miss_count(VAL_A).unwrap(), 50);
        // SlashIndicator only logs the misdemeanor. No felony.
        assert_eq!(si.get_felony_count(VAL_A).unwrap(), 0);

        // Validator status must still be ACTIVE (not force-exited)
        assert!(test_lifecycle_of(storage.clone(), VAL_A)
            .unwrap()
            .is_active_status());
    });
}

// ---------------------------------------------------------------------------
// 2. test_slash_proposer_felony
// ---------------------------------------------------------------------------
/// Reaches the felony threshold (default 150), verifying:
/// - felony_count is incremented
/// - validator is forced out in ValidatorSet
/// - stake is reduced in Staking
#[test]
fn test_slash_proposer_felony() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_A, 2);

        let mut si = SlashIndicator::new(storage.clone());
        // Pin the felony threshold so the test is independent of the prod default.
        si.config_proposer_felony_threshold.write(150).unwrap();

        for _ in 0..150 {
            si.slash_proposer(VAL_A).unwrap();
        }

        assert_eq!(si.get_proposer_miss_count(VAL_A).unwrap(), 150);
        // Felony count must be incremented to 1
        assert_eq!(si.get_felony_count(VAL_A).unwrap(), 1);

        // Validator must be forced out
        assert!(matches!(
            test_lifecycle_of(storage.clone(), VAL_A).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));

        // Stake must be reduced (5% slashed from 1_000_000)
        let staking = Staking::new(storage.clone());
        let remaining = staking.get_stake(VAL_A).unwrap();
        let expected = U256::from(1_000_000u64) * U256::from(95u64) / U256::from(100u64);
        assert_eq!(
            remaining, expected,
            "stake should be 95% of original after 5% slash"
        );
    });
}

#[test]
fn test_felony_stays_jailed_when_slash_drops_below_min_stake() {
    // The single biggest ordering invariant: JAIL BEFORE SLASH. slash_stake demotes
    // ACTIVE->EXITING / PENDING->REGISTERED when stake drops below min_stake. A JAILED
    // status matches neither arm. Thus a JAILED validator stays JAILED even when the
    // slash takes it below min_stake.
    with_storage(|storage| {
        let stake = U256::from(1_000u64);
        register_and_activate_with_stake(storage.clone(), VAL_A, 2, stake);
        // min_stake == current stake, so the 5% slash lands below it.
        Staking::new(storage.clone())
            .config_min_stake
            .write(stake)
            .unwrap();

        let mut si = SlashIndicator::new(storage.clone());
        si.config_proposer_felony_threshold.write(150).unwrap();
        for _ in 0..150 {
            si.slash_proposer(VAL_A).unwrap();
        }

        // 5% slash of 1_000 = 950 < min_stake(1_000).
        assert_eq!(
            Staking::new(storage.clone()).get_stake(VAL_A).unwrap(),
            U256::from(950u64)
        );
        assert!(
            matches!(
                test_lifecycle_of(storage.clone(), VAL_A).unwrap(),
                ValidatorLifecycle::JailRetained(_)
            ),
            "jail-before-slash must keep JAILED even when the slash drops below min_stake"
        );
    });
}

// ---------------------------------------------------------------------------
// 3. test_slash_voter
// ---------------------------------------------------------------------------
/// Increments voter miss count for a validator.
/// No on-chain action at threshold in v1.
#[test]
fn test_slash_voter() {
    with_storage(|storage| {
        let mut si = SlashIndicator::new(storage.clone());

        assert_eq!(si.get_voter_miss_count(VAL_A).unwrap(), 0);

        si.slash_voter(VAL_A).unwrap();
        assert_eq!(si.get_voter_miss_count(VAL_A).unwrap(), 1);

        si.slash_voter(VAL_A).unwrap();
        assert_eq!(si.get_voter_miss_count(VAL_A).unwrap(), 2);

        // Different validator is independent
        assert_eq!(si.get_voter_miss_count(VAL_B).unwrap(), 0);

        si.slash_voter(VAL_B).unwrap();
        assert_eq!(si.get_voter_miss_count(VAL_B).unwrap(), 1);
    });
}

// ---------------------------------------------------------------------------
// 4. test_reset_epoch_counters
// ---------------------------------------------------------------------------
/// After accumulating miss counts, reset_epoch_counters zeros proposer and voter
/// counts for each listed validator without affecting felony_count.
#[test]
fn test_reset_epoch_counters() {
    with_storage(|storage| {
        let mut si = SlashIndicator::new(storage.clone());

        // Accumulate some counts
        for _ in 0..10 {
            si.slash_proposer(VAL_A).unwrap();
            si.slash_voter(VAL_A).unwrap();
        }
        for _ in 0..5 {
            si.slash_proposer(VAL_B).unwrap();
            si.slash_voter(VAL_B).unwrap();
        }

        assert_eq!(si.get_proposer_miss_count(VAL_A).unwrap(), 10);
        assert_eq!(si.get_voter_miss_count(VAL_A).unwrap(), 10);
        assert_eq!(si.get_proposer_miss_count(VAL_B).unwrap(), 5);
        assert_eq!(si.get_voter_miss_count(VAL_B).unwrap(), 5);

        // Reset both validators
        si.reset_epoch_counters(&[VAL_A, VAL_B]).unwrap();

        assert_eq!(si.get_proposer_miss_count(VAL_A).unwrap(), 0);
        assert_eq!(si.get_voter_miss_count(VAL_A).unwrap(), 0);
        assert_eq!(si.get_proposer_miss_count(VAL_B).unwrap(), 0);
        assert_eq!(si.get_voter_miss_count(VAL_B).unwrap(), 0);
    });
}

// ---------------------------------------------------------------------------
// 5. test_felony_count_cumulative
// ---------------------------------------------------------------------------
/// felony_count persists across epoch resets (it is never zeroed by reset_epoch_counters).
/// Triggering another felony in the next epoch increments the count further.
#[test]
fn test_felony_count_cumulative() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_A, 5);

        let mut si = SlashIndicator::new(storage.clone());
        si.config_proposer_felony_threshold.write(150).unwrap();

        // First epoch: trigger one felony (150 misses)
        for _ in 0..150 {
            si.slash_proposer(VAL_A).unwrap();
        }
        assert_eq!(si.get_felony_count(VAL_A).unwrap(), 1);

        // Epoch boundary: reset miss counters
        si.reset_epoch_counters(&[VAL_A]).unwrap();
        assert_eq!(si.get_proposer_miss_count(VAL_A).unwrap(), 0);
        // Felony count must survive the reset
        assert_eq!(si.get_felony_count(VAL_A).unwrap(), 1);

        // Exclude, unjail, and re-activate through the canonical lifecycle before
        // verifying cumulative felony accounting for a later independent fault.
        let mut vs = ValidatorSet::new(storage.clone());
        vs.test_activate_validated_boundary_set(&[SUBMITTER], B256::ZERO, 1)
            .unwrap();
        vs.unjail_after_stake_check(VAL_A).unwrap();
        vs.test_activate_validator_canonically(
            VAL_A,
            StakeProjection::new(U256::from(950_000u64), None),
            U256::from(1),
        )
        .unwrap();

        // Second epoch: trigger another felony
        for _ in 0..150 {
            si.slash_proposer(VAL_A).unwrap();
        }
        // Felony count must now be 2
        assert_eq!(si.get_felony_count(VAL_A).unwrap(), 2);
        assert_eq!(si.get_proposer_miss_count(VAL_A).unwrap(), 150);
    });
}

// ---------------------------------------------------------------------------
// 6. test_evidence_reward
// ---------------------------------------------------------------------------
/// Verifies that evidence submitter receives a reward when submitting
/// double-proposal evidence. Reward = slashed_amount * evidence_reward_percent / 100.
#[test]
fn test_evidence_reward() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(1);
    // Seed STAKING_ADDRESS with funds for the reward transfer
    storage.set_balance(STAKING_ADDRESS, U256::from(10_000_000u64));

    StorageHandle::enter(&mut storage, |storage| {
        // Generate a BLS keypair for the validator
        let (sk, pk) = test_signing::keypair(99).unwrap();
        let pk_bytes: [u8; 48] = pk.to_bytes();

        // Register validator with this pubkey
        let validator = VAL_A;
        let mut vs = ValidatorSet::new(storage.clone());
        vs.test_configure_registry(OWNER).unwrap();
        register_active_fixture(&mut vs, validator, &pk_bytes, U256::from(1));
        {
            let mut sub_pk = [0u8; 48];
            sub_pk[0] = 0xEE;
            register_active_fixture(&mut vs, SUBMITTER, &sub_pk, U256::from(1));
        }

        // Set stake
        let stake = U256::from(1_000_000u64);
        let staking = Staking::new(storage.clone());
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();
        vs.test_set_stake_projection(validator, StakeProjection::new(stake, None))
            .unwrap();

        // Create two different proposals for the same round
        let proposal1 = test_signing::proposal(1, 5, 0, [0xAA; 32]);
        let proposal2 = test_signing::proposal(1, 5, 0, [0xBB; 32]);

        // Sign both with BLS
        let ev1_data = sign_notarize_evidence(&sk, &pk, &proposal1);
        let ev2_data = sign_notarize_evidence(&sk, &pk, &proposal2);

        // seed the committee snapshot the evidence verifier resolves.
        write_test_committee(&storage);

        // Submit evidence
        let mut si = SlashIndicator::new(storage.clone());
        si.submit_double_proposal_evidence(SUBMITTER, &ev1_data, &ev2_data)
            .unwrap();

        // Verify felony applied
        assert_eq!(si.get_felony_count(validator).unwrap(), 1);
        assert!(matches!(
            vs.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));

        // Verify evidence reward paid to submitter
        // slashed = 1_000_000 * 5 / 100 = 50_000
        // reward  = 50_000 * 10 / 100 = 5_000
        let ctx = storage.clone();
        assert_eq!(ctx.balance(SUBMITTER).unwrap(), U256::from(5_000u64));
    });
}

// ---------------------------------------------------------------------------
// 7. test_conflicting_vote_evidence
// ---------------------------------------------------------------------------
/// Verifies that conflicting vote evidence (notarize + nullify same round)
/// correctly jails the validator and rewards the submitter.
#[test]
fn test_conflicting_vote_evidence() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(1);
    storage.set_balance(STAKING_ADDRESS, U256::from(10_000_000u64));

    StorageHandle::enter(&mut storage, |storage| {
        let (sk, pk) = test_signing::keypair(77).unwrap();
        let pk_bytes: [u8; 48] = pk.to_bytes();

        let validator = VAL_B;
        let mut vs = ValidatorSet::new(storage.clone());
        vs.test_configure_registry(OWNER).unwrap();
        register_active_fixture(&mut vs, validator, &pk_bytes, U256::from(1));
        {
            let mut sub_pk = [0u8; 48];
            sub_pk[0] = 0xEE;
            register_active_fixture(&mut vs, SUBMITTER, &sub_pk, U256::from(1));
        }

        let stake = U256::from(2_000_000u64);
        let staking = Staking::new(storage.clone());
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();
        vs.test_set_stake_projection(validator, StakeProjection::new(stake, None))
            .unwrap();

        // Create a notarize proposal (epoch=3, view=7)
        let proposal = test_signing::proposal(3, 7, 0, [0xCC; 32]);
        let notarize_data = sign_notarize_evidence(&sk, &pk, &proposal);

        // Create a nullify vote for the same round (epoch=3, view=7)
        let nullify_payload = test_signing::nullify_payload(3, 7);
        let nullify_data = sign_nullify_evidence(&sk, &pk, &nullify_payload);

        write_test_committee(&storage);

        // Submit conflicting vote evidence
        let mut si = SlashIndicator::new(storage.clone());
        si.submit_conflicting_vote_evidence(SUBMITTER, &notarize_data, &nullify_data)
            .unwrap();

        // Verify felony applied
        assert_eq!(si.get_felony_count(validator).unwrap(), 1);
        assert!(matches!(
            vs.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));

        // Verify evidence reward
        // slashed = 2_000_000 * 5 / 100 = 100_000
        // reward  = 100_000 * 10 / 100 = 10_000
        let ctx = storage.clone();
        assert_eq!(ctx.balance(SUBMITTER).unwrap(), U256::from(10_000u64));
    });
}

// ---------------------------------------------------------------------------
// 8. test_conflicting_vote_evidence_reversed_order
// ---------------------------------------------------------------------------
/// Same as above but with nullify first, notarize second.
#[test]
fn test_conflicting_vote_evidence_reversed_order() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(1);
    storage.set_balance(STAKING_ADDRESS, U256::from(10_000_000u64));

    StorageHandle::enter(&mut storage, |storage| {
        let (sk, pk) = test_signing::keypair(88).unwrap();
        let pk_bytes: [u8; 48] = pk.to_bytes();

        let validator = VAL_A;
        let mut vs = ValidatorSet::new(storage.clone());
        vs.test_configure_registry(OWNER).unwrap();
        register_active_fixture(&mut vs, validator, &pk_bytes, U256::from(1));
        {
            let mut sub_pk = [0u8; 48];
            sub_pk[0] = 0xEE;
            register_active_fixture(&mut vs, SUBMITTER, &sub_pk, U256::from(1));
        }

        let stake = U256::from(1_000_000u64);
        let staking = Staking::new(storage.clone());
        staking.stake_amount.write(&validator, stake).unwrap();
        staking.total_staked.write(stake).unwrap();
        vs.test_set_stake_projection(validator, StakeProjection::new(stake, None))
            .unwrap();

        let proposal = test_signing::proposal(2, 4, 0, [0xDD; 32]);
        let notarize_data = sign_notarize_evidence(&sk, &pk, &proposal);

        let nullify_payload = test_signing::nullify_payload(2, 4);
        let nullify_data = sign_nullify_evidence(&sk, &pk, &nullify_payload);

        write_test_committee(&storage);

        // Submit in reversed order: nullify first, notarize second
        let mut si = SlashIndicator::new(storage.clone());
        si.submit_conflicting_vote_evidence(SUBMITTER, &nullify_data, &notarize_data)
            .unwrap();

        assert_eq!(si.get_felony_count(validator).unwrap(), 1);
    });
}

// ---------------------------------------------------------------------------
// 9. test_conflicting_vote_same_type_fails
// ---------------------------------------------------------------------------
/// Two notarize signatures for the same round should fail (not conflicting types).
#[test]
fn test_conflicting_vote_same_type_fails() {
    with_storage(|storage| {
        let (sk, pk) = test_signing::keypair(66).unwrap();
        let pk_bytes: [u8; 48] = pk.to_bytes();

        let validator = VAL_A;
        let mut vs = ValidatorSet::new(storage.clone());
        vs.test_configure_registry(OWNER).unwrap();
        register_active_fixture(&mut vs, validator, &pk_bytes, U256::from(1));
        {
            let mut sub_pk = [0u8; 48];
            sub_pk[0] = 0xEE;
            register_active_fixture(&mut vs, SUBMITTER, &sub_pk, U256::from(1));
        }

        // Two notarize proposals for the same round
        let proposal1 = test_signing::proposal(1, 1, 0, [0x11; 32]);
        let proposal2 = test_signing::proposal(1, 1, 0, [0x22; 32]);
        let ev1 = sign_notarize_evidence(&sk, &pk, &proposal1);
        let ev2 = sign_notarize_evidence(&sk, &pk, &proposal2);

        write_test_committee(&storage);

        // This should fail. Both are notarize. The call needs one notarize + one nullify.
        let mut si = SlashIndicator::new(storage.clone());
        assert!(si
            .submit_conflicting_vote_evidence(SUBMITTER, &ev1, &ev2)
            .is_err());
    });
}

// ---------------------------------------------------------------------------
// 10. test_full_lifecycle_integration
// ---------------------------------------------------------------------------
/// Integration test: register -> stake -> activate -> propose -> slash -> jail.
#[test]
fn test_full_lifecycle_integration() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(1);
    storage.set_timestamp(U256::from(100_000u64));
    StorageHandle::enter(&mut storage, |storage| {
        let validator = VAL_A;
        let min_stake = U256::from(1_000u64);

        // 1. Setup ValidatorSet config
        let mut vs = ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();

        // 2. Register validator
        let pk = [0x42u8; 48];
        vs.test_register_validator_without_pop(validator, &pk)
            .unwrap();
        assert!(matches!(
            vs.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::WaitingForStake(_)
        ));

        // 3. Setup Staking config and stake
        let mut staking = Staking::new(storage.clone());
        staking.config_min_stake.write(min_stake).unwrap();

        // Seed validator balance so transfer_balance in stake() succeeds
        let ctx = storage.clone();
        ctx.set_balance(validator, U256::from(1_000_000u64))
            .unwrap();

        // Stake to meet min_stake -> PENDING (PoS lifecycle). Then activate, so the
        // felony has an ACTIVE consensus participant to act on. The DKG reshare
        // normally promotes PENDING->ACTIVE. This test uses the semantic boundary fixture.
        staking
            .stake(validator, validator, U256::from(10_000u64))
            .unwrap();
        // stake() no longer transfers funds (EVM call value does it).
        // slash_stake burns from STAKING_ADDRESS. Fund it for the test.
        ctx.set_balance(STAKING_ADDRESS, U256::from(10_000u64))
            .unwrap();
        assert!(matches!(
            vs.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::WaitingForReadiness(_)
        ));
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        assert!(vs
            .validator_lifecycle(validator)
            .unwrap()
            .is_active_status());
        assert_eq!(staking.get_stake(validator).unwrap(), U256::from(10_000u64));

        // 4. Record proposer blocks
        vs.record_proposer(validator).unwrap();
        vs.record_proposer(validator).unwrap();
        vs.record_proposer(validator).unwrap();
        assert_eq!(vs.participation(validator).unwrap().blocks_proposed, 3);

        // 5. Slash proposer until felony (150 misses)
        let mut si = SlashIndicator::new(storage.clone());
        si.config_proposer_felony_threshold.write(150).unwrap();
        for _ in 0..150 {
            si.slash_proposer(validator).unwrap();
        }

        // 6. Verify jail
        assert!(matches!(
            vs.validator_lifecycle(validator).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));
        assert_eq!(si.get_felony_count(validator).unwrap(), 1);

        // Stake reduced by 5%: 10_000 * 95 / 100 = 9_500
        assert_eq!(staking.get_stake(validator).unwrap(), U256::from(9_500u64));

        // 7. Reset epoch counters (epoch boundary)
        si.reset_epoch_counters(&[validator]).unwrap();
        assert_eq!(si.get_proposer_miss_count(validator).unwrap(), 0);
        // Felony count survives
        assert_eq!(si.get_felony_count(validator).unwrap(), 1);
    });

    // 8. On a felony the validator is JAILED (not force-exited). It remains
    // JAILED until the operator unjails (-> PENDING) or unstakes out. DKG drops
    // it from the committee at the next reshare regardless.
    storage.set_timestamp(U256::from(200_000u64));
    StorageHandle::enter(&mut storage, |storage| {
        let validator = VAL_A;
        assert!(matches!(
            test_lifecycle_of(storage.clone(), validator).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));
    });
}

// ---------------------------------------------------------------------------
// Voter felony: missed finalize votes are punitive at the felony threshold.
// Mirrors the proposer-felony path: jail + 5% slash.
// ---------------------------------------------------------------------------
#[test]
fn slash_voter_felony_force_exits_and_slashes_at_threshold() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_A, 0xA1);

        let mut si = SlashIndicator::new(storage.clone());
        // Pin the felony threshold (prod default is 500). The felony branch fires
        // first at this pinned 150, so the test does not reach the misdemeanor warning.
        si.config_voter_felony_threshold.write(150).unwrap();
        for _ in 0..149 {
            si.slash_voter(VAL_A).unwrap();
        }
        assert_eq!(si.get_voter_miss_count(VAL_A).unwrap(), 149);
        assert_eq!(si.get_felony_count(VAL_A).unwrap(), 0);
        assert!(test_lifecycle_of(storage.clone(), VAL_A)
            .unwrap()
            .is_active_status());

        // 150th miss crosses the felony threshold -> jail + 5% stake slash.
        si.slash_voter(VAL_A).unwrap();
        assert_eq!(si.get_voter_miss_count(VAL_A).unwrap(), 150);
        assert_eq!(si.get_felony_count(VAL_A).unwrap(), 1);
        assert!(matches!(
            test_lifecycle_of(storage.clone(), VAL_A).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));

        // 1_000_000 stake slashed by 5% -> 950_000.
        let staking = Staking::new(storage.clone());
        assert_eq!(staking.get_stake(VAL_A).unwrap(), U256::from(950_000u64));
    });
}

/// Graduated escalation invariant: the misdemeanor (warning) threshold
/// must be strictly below the felony (slash) threshold for both proposer and
/// voter. This lets the warning fire before the punishment.
#[test]
fn default_thresholds_warn_before_they_punish() {
    with_storage(|storage| {
        let si = SlashIndicator::new(storage);
        assert!(
            si.proposer_misdemeanor_threshold().unwrap() < si.proposer_felony_threshold().unwrap()
        );
        assert!(
            si.voter_misdemeanor_threshold().unwrap() < si.voter_felony_threshold().unwrap(),
            "voter misdemeanor must be below voter felony"
        );
    });
}

/// A validator already JAILED for a continuous liveness fault is NOT
/// re-felonied (re-slashed 5%) when it crosses the next miss threshold. Only the
/// miss counter keeps moving until the next reshare removes it from the set.
#[test]
fn already_jailed_voter_is_not_re_slashed() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_A, 0xA1);
        let mut si = SlashIndicator::new(storage.clone());
        si.config_voter_felony_threshold.write(2).unwrap();

        // First felony at count==2: JAIL + 5% slash (1_000_000 -> 950_000).
        si.slash_voter(VAL_A).unwrap();
        si.slash_voter(VAL_A).unwrap();
        let staking = Staking::new(storage.clone());
        assert_eq!(si.get_felony_count(VAL_A).unwrap(), 1);
        assert!(matches!(
            test_lifecycle_of(storage.clone(), VAL_A).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));
        assert_eq!(staking.get_stake(VAL_A).unwrap(), U256::from(950_000u64));

        // Two more misses reach count==4 (another threshold multiple), but the
        // validator is already JAILED -> no second felony, no second slash.
        si.slash_voter(VAL_A).unwrap();
        si.slash_voter(VAL_A).unwrap();
        assert_eq!(
            si.get_voter_miss_count(VAL_A).unwrap(),
            4,
            "the miss is still recorded while JAILED"
        );
        assert_eq!(
            si.get_felony_count(VAL_A).unwrap(),
            1,
            "no second felony while already JAILED"
        );
        assert!(matches!(
            test_lifecycle_of(storage.clone(), VAL_A).unwrap(),
            ValidatorLifecycle::JailRetained(_)
        ));
        assert_eq!(
            staking.get_stake(VAL_A).unwrap(),
            U256::from(950_000u64),
            "stake must not be slashed a second time while JAILED"
        );
    });
}

/// A voter miss below the felony threshold is non-punitive: counter increments,
/// the validator stays ACTIVE with full stake (no force-exit, no slash).
#[test]
fn slash_voter_below_threshold_is_not_punitive() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_B, 0xB2);

        let mut si = SlashIndicator::new(storage.clone());
        si.slash_voter(VAL_B).unwrap();

        assert_eq!(si.get_voter_miss_count(VAL_B).unwrap(), 1);
        assert_eq!(si.get_felony_count(VAL_B).unwrap(), 0);
        assert!(test_lifecycle_of(storage.clone(), VAL_B)
            .unwrap()
            .is_active_status());
        let staking = Staking::new(storage.clone());
        assert_eq!(staking.get_stake(VAL_B).unwrap(), U256::from(1_000_000u64));
    });
}

// ===========================================================================
// Test helpers
// ===========================================================================

/// Signs proposal bytes with the notarize namespace and returns evidence data.
fn sign_notarize_evidence(
    sk: &blst::min_pk::SecretKey,
    pk: &blst::min_pk::PublicKey,
    proposal_bytes: &[u8],
) -> Vec<u8> {
    let ns = build_test_namespace(b"_NOTARIZE");
    signed_evidence(sk, pk, &ns, proposal_bytes, POP_DST)
}

/// Signs payload bytes with the nullify namespace and returns evidence data.
fn sign_nullify_evidence(
    sk: &blst::min_pk::SecretKey,
    pk: &blst::min_pk::PublicKey,
    payload_bytes: &[u8],
) -> Vec<u8> {
    let ns = build_test_namespace(b"_NULLIFY");
    signed_evidence(sk, pk, &ns, payload_bytes, POP_DST)
}

/// Seed the committee snapshot into the ring for every retained epoch, so any
/// evidence epoch resolves to the test committee.
fn write_test_committee(storage: &StorageHandle) {
    let snapshot = outbe_validatorset::state::CommitteeSnapshot {
        committee: outbe_consensus::test_harness::committee_entries(
            test_signing::committee_public_keys().iter(),
        ),
        vrf_material_version: 1,
        vrf_group_public_key_bytes: vec![0x11; 96],
        vrf_public_polynomial_hash: B256::ZERO,
    };
    let mut validators = ValidatorSet::new(storage.clone());
    validators.test_configure_registry(OWNER).unwrap();
    validators
        .test_seed_committee_ring(OWNER, &snapshot)
        .unwrap();
}

fn build_test_namespace(suffix: &[u8]) -> Vec<u8> {
    // Committee-bound: the evidence verifier derives the same bytes from
    // the epoch's committee snapshot.
    let c = test_signing::committee();
    match suffix {
        b"_NOTARIZE" => outbe_consensus::proof::notarize_namespace(&c),
        b"_NULLIFY" => outbe_consensus::proof::nullify_namespace(&c),
        b"_FINALIZE" => outbe_consensus::proof::finalize_namespace(&c),
        other => panic!("unexpected sub-namespace suffix {other:?}"),
    }
}

// ===========================================================================
// Evidence dedup regression tests
// ===========================================================================

/// Same evidence submitted twice - second must be rejected.
#[test]
fn test_evidence_dedup_rejects_duplicate() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.set_block_number(1);
    storage.set_balance(STAKING_ADDRESS, U256::from(10_000_000u64));
    storage.set_timestamp(U256::from(100_000u64));

    StorageHandle::enter(&mut storage, |storage| {
        let (sk, pk) = test_signing::keypair(99).unwrap();

        let pk_bytes: [u8; 48] = pk.to_bytes();
        register_and_activate_with_pubkey_and_stake(
            storage.clone(),
            VAL_A,
            &pk_bytes,
            U256::from(100_000u64),
        );

        // Build two different proposals for the same round
        let prop1 = test_signing::proposal(1, 5, 0, [0xAA; 32]);
        let prop2 = test_signing::proposal(1, 5, 0, [0xBB; 32]);

        let ev1 = sign_notarize_evidence(&sk, &pk, &prop1);
        let ev2 = sign_notarize_evidence(&sk, &pk, &prop2);

        write_test_committee(&storage);

        let submitter = address!("0xdddddddddddddddddddddddddddddddddddddddd");
        let mut si = SlashIndicator::new(storage.clone());

        // First submission succeeds
        si.submit_double_proposal_evidence(submitter, &ev1, &ev2)
            .unwrap();

        // Second identical submission must be rejected
        let result = si.submit_double_proposal_evidence(submitter, &ev1, &ev2);
        assert!(result.is_err(), "duplicate evidence must be rejected");

        // Reversed order must also be rejected (canonical hash is order-independent)
        let result = si.submit_double_proposal_evidence(submitter, &ev2, &ev1);
        assert!(
            result.is_err(),
            "reversed duplicate evidence must be rejected"
        );
    });
}

// ===========================================================================
// Evidence with wrong DST must be rejected
// ===========================================================================

// ---- Step 7: idempotent slashing wrapper tests --------------------------

const FB_HASH_A: B256 = b256!("0x1111111111111111111111111111111111111111111111111111111111111111");
const FB_HASH_B: B256 = b256!("0x2222222222222222222222222222222222222222222222222222222222222222");

#[test]
fn slash_window_voters_idempotent_on_repeat_for_same_fb_hash() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_A, 1);

        hooks::slash_window_voters(storage.clone(), FB_HASH_A, &[VAL_A]).unwrap();
        let after_first = SlashIndicator::new(storage.clone())
            .voter_miss_count
            .read(&VAL_A)
            .unwrap();
        assert_eq!(after_first, 1, "first window pass bumps the counter");

        // Replay: same fb_hash window. It must be a no-op (per-fb_hash guard).
        hooks::slash_window_voters(storage.clone(), FB_HASH_A, &[VAL_A]).unwrap();
        hooks::slash_window_voters(storage.clone(), FB_HASH_A, &[VAL_A]).unwrap();
        assert_eq!(
            SlashIndicator::new(storage.clone())
                .voter_miss_count
                .read(&VAL_A)
                .unwrap(),
            1,
            "replaying the same finalized block's window must not double-count"
        );
    });
}

#[test]
fn slash_window_voters_increments_for_different_fb_hash() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_A, 1);

        hooks::slash_window_voters(storage.clone(), FB_HASH_A, &[VAL_A]).unwrap();
        hooks::slash_window_voters(storage.clone(), FB_HASH_B, &[VAL_A]).unwrap();

        let count = SlashIndicator::new(storage.clone())
            .voter_miss_count
            .read(&VAL_A)
            .unwrap();
        assert_eq!(count, 2, "a distinct finalized block re-counts the miss");
    });
}

#[test]
fn slash_window_voters_slashes_all_absentees_once() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_A, 1);
        register_and_activate(storage.clone(), VAL_B, 2);

        // One window pass slashes every absentee in the list.
        hooks::slash_window_voters(storage.clone(), FB_HASH_A, &[VAL_A, VAL_B]).unwrap();
        let si = SlashIndicator::new(storage.clone());
        assert_eq!(si.voter_miss_count.read(&VAL_A).unwrap(), 1);
        assert_eq!(si.voter_miss_count.read(&VAL_B).unwrap(), 1);

        // Replay the same fb_hash window: no further bumps.
        hooks::slash_window_voters(storage.clone(), FB_HASH_A, &[VAL_A, VAL_B]).unwrap();
        assert_eq!(si.voter_miss_count.read(&VAL_A).unwrap(), 1);
        assert_eq!(si.voter_miss_count.read(&VAL_B).unwrap(), 1);
    });
}

#[test]
fn slash_window_proposers_processes_list_once() {
    with_storage(|storage| {
        register_and_activate(storage.clone(), VAL_A, 1);

        // The same proposer can appear twice (two skipped views) in one window.
        // The hook slashes each occurrence within the single atomic pass.
        hooks::slash_window_proposers(storage.clone(), FB_HASH_A, &[VAL_A, VAL_A]).unwrap();
        assert_eq!(
            SlashIndicator::new(storage.clone())
                .proposer_miss_count
                .read(&VAL_A)
                .unwrap(),
            2,
            "duplicate missed-proposer events in one window are each counted"
        );

        // Replaying the same finalized block's window is a no-op.
        hooks::slash_window_proposers(storage.clone(), FB_HASH_A, &[VAL_A, VAL_A]).unwrap();
        assert_eq!(
            SlashIndicator::new(storage.clone())
                .proposer_miss_count
                .read(&VAL_A)
                .unwrap(),
            2,
            "same finalized block window replay is idempotent"
        );
    });
}

/// The window guards of one block and the ring position.
#[derive(Debug, PartialEq)]
struct SlashRingView {
    voter_guard: bool,
    proposer_guard: bool,
    ring: RingPosition,
}

/// The view with `guards` = [voter guard, proposer guard].
fn slash_view(guards: [bool; 2], entry: B256, seq: u64) -> SlashRingView {
    SlashRingView {
        voter_guard: guards[0],
        proposer_guard: guards[1],
        ring: RingPosition { entry, seq },
    }
}

/// Storage after seeding the ring cursor `seq`, the entry at `seq % RETAIN`
/// and both window guards of every block in `guarded`.
fn seeded_slash_ring(seq: u64, entry: B256, guarded: &[B256]) -> HashMapStorageProvider {
    let mut provider = HashMapStorageProvider::new(CHAIN_ID);
    provider.set_block_number(1);
    provider.enter(|storage| {
        let si = SlashIndicator::new(storage);
        si.slash_prune_ring().seed_for_test(seq, entry).unwrap();
        for hash in guarded {
            si.voter_window_slashed.write(hash, true).unwrap();
            si.proposer_window_slashed.write(hash, true).unwrap();
        }
    });
    provider
}

fn slash_ring_view(
    provider: &mut HashMapStorageProvider,
    guarded: B256,
    idx: u64,
) -> SlashRingView {
    provider.enter(|storage| {
        let si = SlashIndicator::new(storage);
        SlashRingView {
            voter_guard: si.voter_window_slashed.read(&guarded).unwrap(),
            proposer_guard: si.proposer_window_slashed.read(&guarded).unwrap(),
            ring: si.slash_prune_ring().position_for_test(idx).unwrap(),
        }
    })
}

/// Characterizes the write order of one prune at a wrapped cursor: the voter
/// guard, then the proposer guard of the evicted block, then the ring entry,
/// then the cursor. A failure before write `n` leaves exactly the first `n`
/// writes applied.
#[test]
fn prune_slash_guards_write_order_at_a_wrapped_cursor() {
    let evicted = test_ring_hash(1);
    let fb_hash = test_ring_hash(2);
    let seq = hooks::SLASH_GUARD_RETAIN + 5;
    let views = HashMapStorageProvider::mutation_prefix_views(
        || seeded_slash_ring(seq, evicted, &[evicted]),
        |storage| hooks::prune_slash_guards(storage, fb_hash),
        |provider| slash_ring_view(provider, evicted, 5),
    )
    .unwrap();
    assert_eq!(
        views,
        MutationPrefixViews {
            before_mutation: vec![
                slash_view([true, true], evicted, seq),
                slash_view([false, true], evicted, seq),
                slash_view([false, false], evicted, seq),
                slash_view([false, false], fb_hash, seq),
            ],
            complete: slash_view([false, false], fb_hash, seq + 1),
            mutations: 4,
        }
    );
}

/// At the last cursor value the prune still evicts and writes the ring entry.
/// Then it reverts with the original message and leaves the cursor unchanged.
#[test]
fn prune_slash_guards_cursor_overflow_reverts_after_the_ring_write() {
    let evicted = test_ring_hash(1);
    let fb_hash = test_ring_hash(2);
    let mut provider = seeded_slash_ring(u64::MAX, evicted, &[evicted]);
    let result = provider.enter(|storage| hooks::prune_slash_guards(storage, fb_hash));
    assert!(
        matches!(&result, Err(PrecompileError::Revert(message)) if message == "slash_guard_ring_seq overflow"),
        "{result:?}"
    );
    assert_eq!(
        slash_ring_view(&mut provider, evicted, u64::MAX % hooks::SLASH_GUARD_RETAIN),
        slash_view([false, false], fb_hash, u64::MAX)
    );
}

/// Evidence signed with the old NUL_ DST must fail verification.
#[test]
fn test_evidence_wrong_dst_rejected() {
    use crate::evidence::EvidenceBlock;

    let (sk, pk) = test_signing::keypair(0x02).unwrap();

    let ns = build_test_namespace(b"_NOTARIZE");
    // Sign with the OLD incorrect DST (NUL_ instead of POP_)
    let wrong_dst = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_";
    let data = signed_evidence(
        &sk,
        &pk,
        &ns,
        &test_signing::proposal(1, 5, 0, [0xAA; 32]),
        wrong_dst,
    );

    let block = EvidenceBlock::parse(&data).unwrap();

    // Verification with POP_ DST must fail for NUL_-signed evidence
    assert!(
        block
            .verify_notarize_signature(&test_signing::committee())
            .is_err(),
        "evidence signed with wrong DST (NUL_) must be rejected"
    );
}
