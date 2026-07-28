//! Public-path consistency checks for D-06, S-06, and S-08.
//!
//! These scenarios deliberately compare every coupled public surface after
//! each transition. They never read or patch contract storage directly.

use std::sync::Mutex;
use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{keccak256, Address, B256, U256};
use cucumber::{given, then, when};

use crate::validator_evidence::conflicting_notarize_for_validator;
use crate::world::localnet::{BootstrapProfile, StartOpts};
use crate::world::rpc::ValidatorRecord;
use crate::world::validators::RegistrationIdentity;
use crate::world::World;

const COEN: u128 = 1_000_000_000_000_000_000;

#[derive(Clone, Debug)]
struct ReregistrationScratch {
    address: String,
    old_identity: RegistrationIdentity,
    new_identity: RegistrationIdentity,
    dense_index: u64,
    post_registration: Option<ValidatorRecord>,
}

#[derive(Clone, Debug)]
struct UnbondingScratch {
    address: String,
    key: String,
    first_end: u64,
    second_end: u64,
    final_end: u64,
    first_claimed: bool,
    second_claimed: bool,
}

static REREGISTRATION: Mutex<Option<ReregistrationScratch>> = Mutex::new(None);
static UNBONDING: Mutex<Option<UnbondingScratch>> = Mutex::new(None);
static ACCOUNTING_COMPLETE: Mutex<bool> = Mutex::new(false);

fn whole_coen(amount: u64) -> U256 {
    U256::from(amount) * U256::from(COEN)
}

fn boot_profile(world: &mut World, profile: BootstrapProfile) {
    let committee_size = world.validators.size();
    world
        .localnet
        .bootstrap_with_profile(committee_size, &profile)
        .expect("bootstrap consistency localnet");
    world
        .localnet
        .start(&StartOpts::default())
        .expect("start consistency localnet");
    if world.localnet.tee_enabled() {
        assert!(
            world
                .rpc
                .wait_bootstrapped(world.localnet.tee_bootstrap_wait_attempts()),
            "TEE chain did not bootstrap"
        );
    } else {
        assert!(
            world
                .rpc
                .wait_block(world.validators.primary_port(), 1, 18)
                .is_some(),
            "tee-less chain RPC did not become reachable"
        );
    }
}

fn wait_status(world: &World, address: &str, wanted: u8, tries: u32) -> ValidatorRecord {
    let port = world.validators.primary_port();
    for _ in 0..tries {
        if let Some(record) = world.rpc.validator_record(port, address) {
            if record.status == wanted {
                return record;
            }
        }
        sleep(Duration::from_secs(2));
    }
    let record = world
        .rpc
        .validator_record(port, address)
        .expect("validator record after status wait");
    assert_eq!(
        record.status, wanted,
        "validator {address} did not reach status {wanted}"
    );
    record
}

fn wait_timestamp(world: &World, timestamp: u64, tries: u32) {
    let port = world.validators.primary_port();
    for _ in 0..tries {
        if world
            .rpc
            .latest_block_timestamp(port)
            .is_some_and(|current| current >= timestamp)
        {
            return;
        }
        sleep(Duration::from_secs(1));
    }
    panic!("chain timestamp did not reach {timestamp}");
}

fn wait_zero_bonded_stake(world: &World, address: &str, tries: u32) -> ValidatorRecord {
    let port = world.validators.primary_port();
    for _ in 0..tries {
        if let Some(record) = world.rpc.validator_record(port, address) {
            if record.stake.is_zero() && record.unbonding_end > 0 {
                return record;
            }
        }
        sleep(Duration::from_secs(2));
    }
    panic!("validator {address} did not move all bonded stake to its claim queue");
}

fn dense_index(world: &World, address: Address) -> u64 {
    let port = world.validators.primary_port();
    world
        .rpc
        .validators(port)
        .expect("dense validator index")
        .iter()
        .position(|candidate| *candidate == address)
        .map(|index| index as u64 + 1)
        .expect("validator in dense index")
}

fn active_set_hash(addresses: &[Address]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    keccak256(bytes)
}

fn owner_activate_without(world: &World, omitted: Address) {
    let port = world.validators.primary_port();
    let active = world
        .rpc
        .active_validators(port)
        .expect("active validator set");
    let retained = active
        .into_iter()
        .filter(|address| *address != omitted)
        .collect::<Vec<_>>();
    let owner = world.validators.get(0).evm_key().expect("owner key");
    let outcome = world
        .rpc
        .activate_reshared_set(&owner, &retained, active_set_hash(&retained))
        .expect("submit owner activation");
    assert!(
        outcome.success,
        "owner exclusion boundary reverted: {}",
        outcome.transaction_hash
    );
}

fn current_epoch(world: &World) -> u64 {
    world
        .rpc
        .consensus_status_field(world.validators.primary_port(), "epoch")
        .and_then(|value| value.trim_matches('"').parse().ok())
        .expect("consensus epoch")
}

fn assert_accounting(world: &World, expected_live_claims: U256, label: &str) -> (U256, U256) {
    let primary = world.validators.primary_port();
    let primary_records = world.rpc.validators(primary).expect("validator addresses");
    let total = world
        .rpc
        .total_staked_on(primary)
        .expect("primary total stake");
    let staking_balance = world
        .rpc
        .staking_balance_on(primary)
        .expect("primary staking balance");

    let mut record_total = U256::ZERO;
    for address in &primary_records {
        let address_text = format!("{address:#x}");
        let record = world
            .rpc
            .validator_record(primary, &address_text)
            .expect("primary validator record");
        let staking = world
            .rpc
            .stake_on(primary, &address_text)
            .expect("primary staking mirror");
        assert_eq!(
            record.stake, staking,
            "{label}: ValidatorSet/Staking mirror mismatch for {address:#x}"
        );
        record_total += staking;
    }
    assert_eq!(record_total, total, "{label}: aggregate bonded stake");
    assert_eq!(
        staking_balance,
        total + expected_live_claims,
        "{label}: native Staking balance must equal bonded stake plus live claims"
    );

    for port in world.validators.committee_ports() {
        assert_eq!(
            world.rpc.total_staked_on(port),
            Some(total),
            "{label}: total stake differs on port {port}"
        );
        assert_eq!(
            world.rpc.staking_balance_on(port),
            Some(staking_balance),
            "{label}: staking balance differs on port {port}"
        );
        for address in &primary_records {
            let address_text = format!("{address:#x}");
            assert_eq!(
                world.rpc.validator_record(port, &address_text),
                world.rpc.validator_record(primary, &address_text),
                "{label}: validator record differs on port {port}"
            );
            assert_eq!(
                world.rpc.stake_on(port, &address_text),
                world.rpc.stake_on(primary, &address_text),
                "{label}: Staking mirror differs on port {port}"
            );
        }
    }
    (total, staking_balance)
}

#[given("an exiting validator with an old BLS key, P2P address, and observable lifecycle residue")]
fn exiting_validator_with_residue(world: &mut World) {
    let profile = BootstrapProfile::default()
        .with_validator_set_owner(0)
        .with_max_validators(8)
        .expect("valid capacity")
        .with_reregistration_cooldown_blocks(0)
        .with_dkg_timing(40, 15, 20)
        .expect("valid DKG timing")
        .with_staking_timing(Some(6), Some(12))
        .expect("valid staking timing");
    boot_profile(world, profile);

    let index = world.validators.joiner_index();
    let identity = world
        .localnet
        .prepare_registration_identity(index)
        .expect("prepare retained joiner identity");
    world
        .localnet
        .provision_joiner_with_identity(index, &identity)
        .expect("provision retained joiner identity");
    world
        .localnet
        .launch_joiner(index, &[])
        .expect("launch retained joiner");
    let port = world.validators.primary_port();
    let address = format!("{:#x}", identity.address());
    world
        .rpc
        .wait_block(world.validators.http_port(index), 5, 40)
        .expect("joiner sync");
    world
        .rpc
        .stake(identity.evm_key(), 1000)
        .expect("stake joiner");
    world
        .rpc
        .confirm_ready(identity.evm_key())
        .expect("confirm joiner readiness");
    assert!(
        world.rpc.wait_participant(port, &address, 50),
        "joiner never became a consensus participant"
    );
    let active = world
        .rpc
        .validator_record(port, &address)
        .expect("active joiner record");
    assert_eq!(active.status, 2);
    assert!(active.has_bls_share);
    let p2p = world
        .rpc
        .validator_p2p_address(port, &address)
        .expect("joiner P2P pair");
    assert!(p2p.version > 0 && !p2p.encoded.is_empty());

    let dense_index = dense_index(world, identity.address());
    world
        .rpc
        .deactivate(identity.evm_key())
        .expect("deactivate joiner");
    let exiting = wait_status(world, &address, 3, 20);
    assert!(exiting.deactivated_at_height > 0);

    let new_identity = world
        .localnet
        .rotate_registration_bls(index + 1, &identity)
        .expect("rotate BLS while retaining EOA");
    *REREGISTRATION.lock().expect("reregistration scratch") = Some(ReregistrationScratch {
        address,
        old_identity: identity,
        new_identity,
        dense_index,
        post_registration: None,
    });
}

#[when(
    "its final unbonding claim and same-EOA registration with a new BLS key execute in one block"
)]
fn claim_and_reregister_in_one_block(world: &mut World) {
    let mut scratch = REREGISTRATION
        .lock()
        .expect("reregistration scratch")
        .take()
        .expect("D-06 setup");
    let unbonding = wait_status(world, &scratch.address, 4, 80);
    assert!(!unbonding.has_bls_share);
    let drained = wait_zero_bonded_stake(world, &scratch.address, 20);
    wait_timestamp(world, drained.unbonding_end, 30);

    let outcomes = world
        .rpc
        .claim_unbonded_then_register(
            scratch.new_identity.evm_key(),
            scratch.new_identity.address(),
            scratch.new_identity.bls_public_key(),
            scratch.new_identity.registration_signature(),
        )
        .expect("submit same-block claim and registration");
    assert!(outcomes[0].success, "final claim reverted");
    assert!(outcomes[1].success, "same-EOA re-registration reverted");
    assert_eq!(
        outcomes[0].block_number(),
        outcomes[1].block_number(),
        "claim and re-registration must execute in one block"
    );
    scratch.post_registration = world
        .rpc
        .validator_record(world.validators.primary_port(), &scratch.address);
    *REREGISTRATION.lock().expect("reregistration scratch") = Some(scratch);
}

#[then("re-registration reuses exactly one dense index and resets all lifecycle residue")]
fn reregistration_resets_residue(world: &mut World) {
    let scratch = REREGISTRATION.lock().expect("reregistration scratch");
    let scratch = scratch.as_ref().expect("D-06 setup");
    let port = world.validators.primary_port();
    let record = scratch
        .post_registration
        .as_ref()
        .expect("post-registration record");
    assert_eq!(
        dense_index(world, scratch.new_identity.address()),
        scratch.dense_index
    );
    assert_eq!(world.rpc.validator_count(port), Some(5));
    assert_eq!(record.status, 0);
    assert_eq!(record.stake, U256::ZERO);
    assert_eq!(record.slash_count, 0);
    assert_eq!(record.missed_blocks, 0);
    assert_eq!(record.missed_votes, 0);
    assert_eq!(record.blocks_proposed, 0);
    assert_eq!(record.deactivated_at_height, 0);
    assert_eq!(record.unbonding_end, 0);
    assert!(!record.has_bls_share);
    assert_eq!(
        record.consensus_pubkey.as_ref(),
        scratch.new_identity.bls_public_key().as_ref()
    );
    let p2p = world
        .rpc
        .validator_p2p_address(port, &scratch.address)
        .expect("reset P2P pair");
    assert_eq!(p2p.version, 0);
    assert!(p2p.encoded.is_empty());
}

#[then("the old BLS key can be registered by another candidate exactly once")]
fn old_bls_key_released_once(world: &mut World) {
    let scratch = REREGISTRATION.lock().expect("reregistration scratch");
    let scratch = scratch.as_ref().expect("D-06 setup");
    let funder = world.validators.get(0);

    let candidate = world
        .localnet
        .prepare_registration_identity(world.validators.joiner_index() + 2)
        .expect("prepare candidate EOA");
    let rebound = world
        .localnet
        .rebind_registration_bls(
            world.validators.joiner_index() + 3,
            &candidate,
            &scratch.old_identity,
        )
        .expect("bind released old BLS key");
    let funding = world
        .rpc
        .fund_key(&funder, rebound.evm_key(), 2)
        .expect("fund old-key candidate");
    assert!(world.rpc.wait_successful_receipt(&funding, 20));
    let accepted = world
        .rpc
        .register_validator(
            rebound.evm_key(),
            rebound.address(),
            rebound.bls_public_key(),
            rebound.registration_signature(),
        )
        .expect("register released old key");
    assert!(accepted.success, "released old BLS key was not reusable");

    let second_candidate = world
        .localnet
        .prepare_registration_identity(world.validators.joiner_index() + 4)
        .expect("prepare duplicate old-key candidate");
    let second_rebound = world
        .localnet
        .rebind_registration_bls(
            world.validators.joiner_index() + 5,
            &second_candidate,
            &scratch.old_identity,
        )
        .expect("bind duplicate old BLS key");
    let funding = world
        .rpc
        .fund_key(&funder, second_rebound.evm_key(), 2)
        .expect("fund duplicate old-key candidate");
    assert!(world.rpc.wait_successful_receipt(&funding, 20));
    let duplicate = world
        .rpc
        .register_validator(
            second_rebound.evm_key(),
            second_rebound.address(),
            second_rebound.bls_public_key(),
            second_rebound.registration_signature(),
        )
        .expect("submit duplicate old key");
    assert!(!duplicate.success, "old BLS key registered more than once");
}

#[then("a duplicate registration of the new BLS key is rejected without partial registry writes")]
fn duplicate_new_bls_is_atomic(world: &mut World) {
    let scratch = REREGISTRATION.lock().expect("reregistration scratch");
    let scratch = scratch.as_ref().expect("D-06 setup");
    let port = world.validators.primary_port();
    let before_count = world.rpc.validator_count(port);
    let before_index = world.rpc.validators(port);
    let before_owner = world
        .rpc
        .validator_record(port, &scratch.address)
        .expect("new-key owner before duplicate");

    let candidate = world
        .localnet
        .prepare_registration_identity(world.validators.joiner_index() + 6)
        .expect("prepare duplicate new-key candidate");
    let duplicate_identity = world
        .localnet
        .rebind_registration_bls(
            world.validators.joiner_index() + 7,
            &candidate,
            &scratch.new_identity,
        )
        .expect("bind duplicate new BLS key");
    let funding = world
        .rpc
        .fund_key(&world.validators.get(0), duplicate_identity.evm_key(), 2)
        .expect("fund duplicate new-key candidate");
    assert!(world.rpc.wait_successful_receipt(&funding, 20));
    let outcome = world
        .rpc
        .register_validator(
            duplicate_identity.evm_key(),
            duplicate_identity.address(),
            duplicate_identity.bls_public_key(),
            duplicate_identity.registration_signature(),
        )
        .expect("submit duplicate new key");
    assert!(!outcome.success, "duplicate new BLS key was accepted");
    assert_eq!(world.rpc.validator_count(port), before_count);
    assert_eq!(world.rpc.validators(port), before_index);
    assert_eq!(
        world.rpc.validator_record(port, &scratch.address),
        Some(before_owner)
    );
    assert_eq!(
        world
            .rpc
            .is_validator(port, &format!("{:#x}", duplicate_identity.address())),
        Some(false)
    );
}

#[then("staking without a new readiness confirmation cannot activate the re-registered validator")]
fn reregistered_validator_requires_reconfirmation(world: &mut World) {
    let scratch = REREGISTRATION.lock().expect("reregistration scratch");
    let scratch = scratch.as_ref().expect("D-06 setup");
    let port = world.validators.primary_port();
    world
        .rpc
        .stake(scratch.new_identity.evm_key(), 1000)
        .expect("restake re-registered validator");
    assert_eq!(wait_status(world, &scratch.address, 1, 20).status, 1);
    let start = world.rpc.epoch_start_block(port).unwrap_or_default();
    for _ in 0..100 {
        if world
            .rpc
            .epoch_start_block(port)
            .is_some_and(|height| height > start)
        {
            break;
        }
        sleep(Duration::from_secs(2));
    }
    let record = world
        .rpc
        .validator_record(port, &scratch.address)
        .expect("re-registered record after periodic window");
    assert_eq!(record.status, 1, "validator activated with stale readiness");
    assert!(!record.has_bls_share);
    assert!(!world.rpc.is_participant(port, &scratch.address));
}

#[given("isolated validators for successful and reverting lifecycle actions")]
fn isolated_accounting_validators(world: &mut World) {
    let profile = BootstrapProfile::default()
        .with_validator_set_owner(0)
        .with_dkg_timing(80, 20, 20)
        .expect("valid DKG timing")
        .with_staking_timing(Some(5), Some(10))
        .expect("valid staking timing");
    boot_profile(world, profile);
    *ACCOUNTING_COMPLETE.lock().expect("accounting scratch") = false;
}

#[when("the harness finalizes stake, rejected stake, slash, exit, and claim actions")]
fn run_accounting_lifecycle(world: &mut World) {
    let port = world.validators.primary_port();
    let victim = world.validators.get(2);
    let victim_key = victim.evm_key().expect("slash victim key");
    let victim_address = world.rpc.address_of(&victim_key).expect("victim address");
    let exiting = world.validators.get(3);
    let exiting_key = exiting.evm_key().expect("exit validator key");
    let exiting_address = world.rpc.address_of(&exiting_key).expect("exit address");
    let reporter = world.validators.get(0);
    let reporter_key = reporter.evm_key().expect("reporter key");

    let mut claims = U256::ZERO;
    assert_accounting(world, claims, "genesis");

    world.rpc.stake(&victim_key, 100).expect("successful stake");
    assert_accounting(world, claims, "successful stake");

    let before_rejected = world
        .rpc
        .validator_record(port, &victim_address)
        .expect("record before rejected stake");
    let total_before_rejected = world.rpc.total_staked_on(port);
    assert!(
        world.rpc.stake(&victim_key, 0).is_err(),
        "zero-value stake unexpectedly succeeded"
    );
    assert_eq!(
        world.rpc.validator_record(port, &victim_address),
        Some(before_rejected)
    );
    assert_eq!(world.rpc.total_staked_on(port), total_before_rejected);
    assert_accounting(world, claims, "rejected stake");

    let epoch = current_epoch(world);
    let evidence = conflicting_notarize_for_validator(&world.rpc, port, &victim, epoch, 2)
        .expect("canonical slash evidence");
    let slash = world
        .rpc
        .submit_conflicting_notarize_evidence(&reporter_key, &evidence.block1, &evidence.block2)
        .expect("submit slash evidence");
    assert!(slash.success, "canonical slash evidence reverted");
    assert_accounting(world, claims, "evidence slash");

    let exit_stake = world
        .rpc
        .stake_on(port, &exiting_address)
        .expect("stake before exit");
    world
        .rpc
        .deactivate(&exiting_key)
        .expect("deactivate validator");
    assert_accounting(world, claims, "deactivate");
    owner_activate_without(
        world,
        exiting_address.parse().expect("parse exiting address"),
    );
    wait_status(world, &exiting_address, 4, 20);
    let drained = wait_zero_bonded_stake(world, &exiting_address, 20);
    claims += exit_stake;
    assert_accounting(world, claims, "exit drain");

    wait_timestamp(world, drained.unbonding_end, 30);
    let receipt = world
        .rpc
        .claim_unbonded(&exiting_key)
        .expect("claim exited stake");
    let fee = crate::world::rpc::Rpc::receipt_gas_cost(&receipt).expect("claim gas cost");
    assert!(fee < whole_coen(1), "unexpectedly large localnet claim fee");
    claims -= exit_stake;
    assert_accounting(world, claims, "final claim");
    *ACCOUNTING_COMPLETE.lock().expect("accounting scratch") = true;
}

#[then("after every action the Staking and ValidatorSet stake mirrors agree on every node")]
fn stake_mirrors_agreed_after_every_action(_world: &mut World) {
    assert!(
        *ACCOUNTING_COMPLETE.lock().expect("accounting scratch"),
        "accounting action sequence did not complete"
    );
}

#[then("the Staking native balance equals bonded stake plus all live claims")]
fn native_balance_is_conserved(world: &mut World) {
    assert_accounting(world, U256::ZERO, "completed accounting lifecycle");
}

#[given("an active validator has staggered partial-unstake claims before exit")]
fn active_validator_has_staggered_claims(world: &mut World) {
    let profile = BootstrapProfile::default()
        .with_validator_set_owner(0)
        .with_dkg_timing(80, 20, 20)
        .expect("valid DKG timing")
        .with_staking_timing(Some(20), Some(40))
        .expect("valid staking timing");
    boot_profile(world, profile);

    let validator = world.validators.get(3);
    let key = validator.evm_key().expect("staggered validator key");
    let address = world.rpc.address_of(&key).expect("staggered address");
    let first = world.rpc.unstake(&key, 100).expect("first partial unstake");
    assert!(first.success);
    let first_end = world
        .rpc
        .validator_record(world.validators.primary_port(), &address)
        .expect("record after first unstake")
        .unbonding_end;
    let first_start = first_end.saturating_sub(20);
    wait_timestamp(world, first_start + 8, 30);

    let second = world
        .rpc
        .unstake(&key, 100)
        .expect("second partial unstake");
    assert!(second.success);
    let second_end = world
        .rpc
        .validator_record(world.validators.primary_port(), &address)
        .expect("record after second unstake")
        .unbonding_end;
    assert!(
        second_end > first_end,
        "claim timestamps were not staggered"
    );
    wait_timestamp(world, first_end, 30);
    world
        .rpc
        .deactivate(&key)
        .expect("deactivate after partial claims");

    *UNBONDING.lock().expect("unbonding scratch") = Some(UnbondingScratch {
        address,
        key,
        first_end,
        second_end,
        final_end: 0,
        first_claimed: false,
        second_claimed: false,
    });
}

#[when("an exclusion boundary moves it to UNBONDING")]
fn exclusion_moves_to_unbonding(world: &mut World) {
    let mut scratch = UNBONDING
        .lock()
        .expect("unbonding scratch")
        .take()
        .expect("S-08 setup");
    owner_activate_without(
        world,
        scratch.address.parse().expect("parse unbonding address"),
    );
    wait_status(world, &scratch.address, 4, 20);
    scratch.final_end = wait_zero_bonded_stake(world, &scratch.address, 20).unbonding_end;
    assert!(scratch.final_end > scratch.second_end);
    *UNBONDING.lock().expect("unbonding scratch") = Some(scratch);
}

#[when("its matured claims are consumed one at a time")]
fn consume_staggered_claims(world: &mut World) {
    let mut scratch = UNBONDING
        .lock()
        .expect("unbonding scratch")
        .take()
        .expect("S-08 setup");
    let port = world.validators.primary_port();

    wait_timestamp(world, scratch.first_end, 10);
    let before = world
        .rpc
        .balance_on(port, &scratch.address)
        .expect("balance before first claim");
    let first = world
        .rpc
        .claim_unbonded(&scratch.key)
        .expect("claim first staggered entry");
    let after = world
        .rpc
        .balance_on(port, &scratch.address)
        .expect("balance after first claim");
    assert_eq!(
        after + crate::world::rpc::Rpc::receipt_gas_cost(&first).expect("first claim fee") - before,
        whole_coen(100),
        "first claim consumed more than its matured entry"
    );
    scratch.first_claimed = true;
    assert_eq!(wait_status(world, &scratch.address, 4, 2).status, 4);

    wait_timestamp(world, scratch.second_end, 30);
    let before = world
        .rpc
        .balance_on(port, &scratch.address)
        .expect("balance before second claim");
    let second = world
        .rpc
        .claim_unbonded(&scratch.key)
        .expect("claim second staggered entry");
    let after = world
        .rpc
        .balance_on(port, &scratch.address)
        .expect("balance after second claim");
    assert_eq!(
        after + crate::world::rpc::Rpc::receipt_gas_cost(&second).expect("second claim fee")
            - before,
        whole_coen(100),
        "second claim consumed more than its matured entry"
    );
    scratch.second_claimed = true;
    assert_eq!(wait_status(world, &scratch.address, 4, 2).status, 4);
    *UNBONDING.lock().expect("unbonding scratch") = Some(scratch);
}

#[then("it remains UNBONDING while bonded stake or any live claim remains")]
fn remains_unbonding_with_live_claims(world: &mut World) {
    let scratch = UNBONDING.lock().expect("unbonding scratch");
    let scratch = scratch.as_ref().expect("S-08 setup");
    assert!(scratch.first_claimed && scratch.second_claimed);
    let record = world
        .rpc
        .validator_record(world.validators.primary_port(), &scratch.address)
        .expect("record with final live claim");
    assert_eq!(record.status, 4);
    assert_eq!(record.stake, U256::ZERO);
    assert!(record.unbonding_end > 0);
    assert!(!record.has_bls_share);
}

#[then(
    "only the final claim moves it to INACTIVE with zero stake, zero unbonding end, and no share"
)]
fn final_claim_moves_inactive(world: &mut World) {
    let scratch = UNBONDING
        .lock()
        .expect("unbonding scratch")
        .take()
        .expect("S-08 setup");
    wait_timestamp(world, scratch.final_end, 40);
    world
        .rpc
        .claim_unbonded(&scratch.key)
        .expect("claim final exit entry");
    let record = wait_status(world, &scratch.address, 5, 10);
    assert_eq!(record.stake, U256::ZERO);
    assert_eq!(record.unbonding_end, 0);
    assert!(!record.has_bls_share);
}
