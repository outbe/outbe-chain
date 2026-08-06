//! Steps for `features/s7a_downtime_slash.feature` — port of
//! The downtime-slashing feature. Kill one committee validator and prove
//! the chain keeps finalizing on the surviving 3-of-4 BFT quorum, then prove the
//! resulting voter felony burns exactly the configured stake once.

use std::thread::sleep;
use std::time::Duration;

use cucumber::{given, then, when};

use crate::world::World;

/// `outbe-cli slash config` exposes the felony slash percent (s7a:31-32).
#[given("the slashing config is readable")]
fn slashing_config_readable(world: &mut World) {
    assert!(
        world.rpc.slash_percent().is_some(),
        "slashing config not readable (felony slash percent absent)"
    );
}

/// The named validator is ACTIVE (status 2) before the kill (s7a:33-34).
#[given(expr = "validator {string} starts active")]
fn validator_starts_active(world: &mut World, name: String) {
    let port = world.validators.primary_port();
    let key = world
        .validators
        .by_name(&name)
        .expect("resolve validator")
        .evm_key()
        .expect("read key");
    let addr = world.rpc.address_of(&key).expect("derive address");
    assert_eq!(
        world.rpc.validator_status(port, &addr),
        Some(2),
        "{name} should start ACTIVE (2)"
    );
    let stake_before_top_up = world
        .rpc
        .stake_on(port, &addr)
        .expect("read victim stake before top-up");
    let top_up = alloy_primitives::U256::from(100u64)
        * alloy_primitives::U256::from(1_000_000_000_000_000_000u128);
    let top_up_tx = world.rpc.stake(&key, 100).expect("top up victim stake");
    assert!(
        world.rpc.wait_successful_receipt(&top_up_tx, 20),
        "victim stake top-up receipt must succeed"
    );
    assert_eq!(
        world.rpc.stake_on(port, &addr),
        Some(stake_before_top_up + top_up),
        "victim stake top-up must be visible before downtime"
    );
    world.state.slash_stake_before = Some(
        world
            .rpc
            .stake_on(port, &addr)
            .expect("read victim stake before downtime"),
    );
    world.state.slash_total_before = Some(
        world
            .rpc
            .total_staked_on(port)
            .expect("read total stake before downtime"),
    );
    world.state.slash_staking_balance_before = Some(
        world
            .rpc
            .staking_balance_on(port)
            .expect("read staking balance before downtime"),
    );
    world.state.slash_count_before = Some(
        world
            .rpc
            .slash_count(port, &addr)
            .expect("read slash count before downtime"),
    );
    world.state.joiner_addr = Some(addr); // reuse the addr slot for the victim
}

/// Kill the named committee validator, recording the head at kill time (s7a:37-38).
#[when(expr = "validator {string} is killed")]
fn validator_is_killed(world: &mut World, name: String) {
    let i = world
        .validators
        .by_name(&name)
        .expect("resolve validator")
        .index;
    let port = world.validators.primary_port();
    world.state.marker_height = world.rpc.head(port);
    world.localnet.kill_validator(i).expect("kill validator");
}

/// The surviving committee reaches the dev felony threshold and every punitive
/// accounting surface reflects one exact burn.
#[then("the committee keeps finalizing until the validator is slashed exactly once")]
fn committee_keeps_finalizing_until_one_slash(world: &mut World) {
    let port = world.validators.primary_port();
    let kill_h = world.state.marker_height.expect("kill height captured");
    let victim = world.state.joiner_addr.clone().expect("victim address");
    let before_count = world
        .state
        .slash_count_before
        .expect("slash count snapshot");

    let mut observed_count = None;
    for _ in 0..80 {
        let head = world.rpc.head(port).unwrap_or_default();
        let count = world.rpc.slash_count(port, &victim);
        if head > kill_h && count.is_some_and(|value| value == before_count + 1) {
            observed_count = count;
            break;
        }
        sleep(Duration::from_secs(3));
    }
    assert!(
        observed_count == Some(before_count + 1),
        "surviving quorum did not finalize a single downtime felony"
    );

    let percent = world.rpc.slash_percent().expect("slash percent");
    let stake_before = world.state.slash_stake_before.expect("stake snapshot");
    let total_before = world.state.slash_total_before.expect("total snapshot");
    let staking_balance_before = world
        .state
        .slash_staking_balance_before
        .expect("staking balance snapshot");
    let expected_burn =
        stake_before * alloy_primitives::U256::from(percent) / alloy_primitives::U256::from(100u64);
    assert!(!expected_burn.is_zero(), "configured slash must burn value");

    let stake_after = world
        .rpc
        .stake_on(port, &victim)
        .expect("stake after slash");
    assert_eq!(
        stake_after,
        stake_before - expected_burn,
        "victim stake burn"
    );
    assert_eq!(
        world.rpc.total_staked_on(port).expect("total after slash"),
        total_before - expected_burn,
        "network total must decrease by the exact burn"
    );
    assert_eq!(
        world
            .rpc
            .staking_balance_on(port)
            .expect("staking balance after slash"),
        staking_balance_before - expected_burn,
        "staking precompile balance must burn the exact same value"
    );
    assert_eq!(
        world.rpc.validator_status(port, &victim),
        Some(6),
        "downtime felony must jail the validator"
    );
    assert!(
        world.rpc.has_voter_felony_event(port, &victim, kill_h),
        "finalized VoterFelony event is missing"
    );

    for rpc_port in world
        .validators
        .committee_ports()
        .into_iter()
        .filter(|rpc_port| world.rpc.head(*rpc_port).is_some())
    {
        assert_eq!(
            world.rpc.slash_count(rpc_port, &victim),
            Some(before_count + 1)
        );
        assert_eq!(world.rpc.stake_on(rpc_port, &victim), Some(stake_after));
        assert_eq!(
            world.rpc.total_staked_on(rpc_port),
            Some(total_before - expected_burn)
        );
    }
    world.state.slash_stake_after = Some(stake_after);
    world.state.marker_height = world.rpc.head(port);
}

/// A continuously absent validator is not repeatedly penalized after it has
/// entered JAILED; the chain must still advance while that guard is exercised.
#[then("continued downtime does not slash the validator twice")]
fn continued_downtime_is_idempotent(world: &mut World) {
    let port = world.validators.primary_port();
    let victim = world.state.joiner_addr.clone().expect("victim address");
    let marker = world.state.marker_height.expect("post-slash height");
    let target = marker + 35;
    let head = world
        .rpc
        .wait_block_gt(port, target, 40)
        .unwrap_or_default();
    assert!(head > target, "chain stopped after downtime felony");

    let stake_after = world.state.slash_stake_after.expect("post-slash stake");
    let before_count = world
        .state
        .slash_count_before
        .expect("slash count snapshot");
    assert_eq!(world.rpc.slash_count(port, &victim), Some(before_count + 1));
    assert_eq!(world.rpc.stake_on(port, &victim), Some(stake_after));
}
