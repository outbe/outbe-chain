//! Steps for `features/s1_s2_s6_s3_lifecycle.feature` — port of
//! The validator lifecycle feature, one chain through four e2e.md stages:
//!   S1 cold full-node sync + tribute offer (state/supply parity)
//!   S2 promote full-node -> validator via reshare (stake -> confirm -> ACTIVE)
//!   S6 in-flight tribute offer that lands exactly once across the committee change
//!   S3 validator exit -> reshare down -> node demotes to verifier-follower (alive)

use std::thread::sleep;
use std::time::Duration;

use cucumber::{then, when};

use crate::world::rpc::Rpc;
use crate::world::World;

/// Lockstep probe (script's `LOCK` loop): 5×10s, joiner within 3 of committee and
/// strictly advancing each round.
fn lockstep_ok(rpc: &Rpc, committee: u16, joiner: u16) -> bool {
    let mut prev = 0u64;
    for _ in 0..5 {
        sleep(Duration::from_secs(10));
        let ch = rpc.head(committee).unwrap_or(0);
        let vh = rpc.head(joiner).unwrap_or(0);
        if ch.saturating_sub(vh) > 3 || vh <= prev {
            return false;
        }
        prev = vh;
    }
    true
}

/// S1 — submit a tribute offer from the operator; capture the day status first so
/// the invariant (offer is time-driven, not offer-driven) can be checked.
#[when(expr = "operator {string} submits a tribute offer")]
fn submit_offer(world: &mut World, name: String) {
    let primary = world.validators.primary_port();
    let wwd = world.state.wwd.clone().expect("worldwide-day set at setup");
    let key = world
        .validators
        .by_name(&name)
        .expect("resolve operator")
        .evm_key()
        .expect("key");
    world.state.wwd_status_before = world.rpc.wwd_status(primary, &wwd);
    world.state.tribute_tx_hash = world
        .rpc
        .offer_until_supply_hash(&key, &wwd, primary, "1", 5);
    assert!(
        world.state.tribute_tx_hash.is_some(),
        "committee did not process the offer (supply != 1)"
    );
}

/// S1 — supply reached 1 and the worldwide-day status is unchanged by the offer.
#[then("the committee processes the offer without changing the day status")]
fn offer_processed_status_unchanged(world: &mut World) {
    let primary = world.validators.primary_port();
    let wwd = world.state.wwd.clone().expect("wwd");
    assert_eq!(
        world.rpc.supply(primary).as_deref(),
        Some("1"),
        "supply should be 1"
    );
    assert_eq!(
        world.rpc.wwd_status(primary, &wwd),
        world.state.wwd_status_before,
        "day status changed by the offer (should be time-driven)"
    );
    world
        .mongodb
        .wait_for_tribute_projection(
            world.state.tribute_tx_hash.as_deref().expect("tribute tx"),
            60,
        )
        .expect("initial Tribute and both indexes must match on every committee validator");
}

/// S1 — provision + launch a REGISTERED (not staked) full node and sync it to tip.
#[when("a full node joins and syncs to the committee tip")]
fn full_node_syncs(world: &mut World) {
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    world
        .localnet
        .provision_joiner(idx)
        .expect("provision joiner");
    world
        .localnet
        .launch_joiner(idx, &[])
        .expect("launch joiner");
    let h = world.rpc.wait_block(joiner_port, 20, 40).unwrap_or(0);
    assert!(h >= 20, "full node did not catch up to tip (head {h})");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    world.state.joiner_addr = world.rpc.address_of(&key);
}

/// S1 — the full node has supply + state-root parity and is NOT a participant.
#[then("the full node matches committee supply and state root and is not a participant")]
fn full_node_parity(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner addr");

    assert_eq!(
        world.rpc.supply(joiner_port).as_deref(),
        Some("1"),
        "full-node supply parity"
    );
    assert!(
        !world.rpc.is_participant(joiner_port, &addr),
        "a full node must not be a consensus participant"
    );
    assert_eq!(
        world.rpc.active_count(primary),
        Some(4),
        "active set unchanged by a full node"
    );
    assert_eq!(
        world.rpc.validator_status(primary, &addr),
        Some(0),
        "authorized self-registration must leave the joiner REGISTERED after the rejected third-party attempt"
    );

    let pn = world.rpc.finalized(joiner_port).unwrap_or(20);
    let sr_c = world.rpc.state_root(primary, pn);
    let sr_v = world.rpc.state_root(joiner_port, pn);
    assert_eq!(
        sr_c, sr_v,
        "state-root parity committee vs full node @h{pn}"
    );
}

/// S2 — stake the full node, confirm PENDING + not-yet-participant, confirm ready.
#[when("the full node stakes and confirms readiness")]
fn full_node_stakes_confirms(world: &mut World) {
    let primary = world.validators.primary_port();
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.state.joiner_addr.clone().expect("joiner addr");

    world.rpc.stake(&key, 1000).expect("stake");
    sleep(Duration::from_secs(6));
    assert_eq!(
        world.rpc.validator_status(primary, &addr),
        Some(1),
        "staked joiner is PENDING"
    );
    assert!(
        !world.rpc.is_participant(primary, &addr),
        "PENDING joiner must not be a participant before confirm-ready"
    );
    world.rpc.confirm_ready(&key).expect("confirm ready");
}

/// S2 + S6 — the joiner activates via reshare while an in-flight offer lands once.
#[then("it is promoted to an active participant and the in-flight offer lands once")]
fn promoted_with_inflight_offer(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner addr");
    let wwd = world.state.wwd.clone().expect("wwd");
    let v1 = world
        .validators
        .by_name("validator-1")
        .expect("v1")
        .evm_key()
        .expect("v1 key");

    // In-flight offer submitted during the reshare window; must land exactly once.
    world.state.tribute_tx_hash = world
        .rpc
        .offer_until_supply_hash(&v1, &wwd, primary, "2", 15);
    assert!(
        world.state.tribute_tx_hash.is_some(),
        "in-flight offer did not land (supply != 2)"
    );
    assert!(
        world.rpc.wait_participant(primary, &addr, 70),
        "joiner never became a consensus participant"
    );
    assert_eq!(
        world.rpc.validator_status(primary, &addr),
        Some(2),
        "joiner ACTIVE (2)"
    );
    assert_eq!(
        world.rpc.active_count(primary),
        Some(5),
        "active set grew to 5"
    );
    assert_eq!(
        world.rpc.supply(primary).as_deref(),
        Some("2"),
        "in-flight offer landed once"
    );
    world
        .mongodb
        .wait_for_tribute_projection_on_nodes(
            world
                .state
                .tribute_tx_hash
                .as_deref()
                .expect("in-flight tribute tx"),
            60,
            world.validators.joiner_index() + 1,
        )
        .expect("in-flight Tribute and indexes must match across the promoted committee");

    sleep(Duration::from_secs(30)); // settle: engine restarts for the new epoch
    assert!(
        lockstep_ok(&world.rpc, primary, joiner_port),
        "activated joiner does not advance in lockstep (has no working share)"
    );
    assert_eq!(
        world.rpc.supply(joiner_port).as_deref(),
        Some("2"),
        "offer parity on joiner RPC"
    );
}

/// S3 — the promoted validator self-deactivates (ACTIVE -> EXITING, stays a
/// participant with its share until the exclusion reshare).
#[when("the promoted validator deactivates")]
fn promoted_deactivates(world: &mut World) {
    let primary = world.validators.primary_port();
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let addr = world.state.joiner_addr.clone().expect("joiner addr");

    let stake = world
        .rpc
        .stake_on(primary, &addr)
        .expect("joiner stake before exit");
    let total = world
        .rpc
        .total_staked_on(primary)
        .expect("total stake before exit");
    let staking_balance = world
        .rpc
        .staking_balance_on(primary)
        .expect("Staking balance before exit");
    assert!(!stake.is_zero(), "active joiner must have bonded stake");
    world.state.lifecycle_stake_before_exit = Some(stake);
    world.state.lifecycle_total_before_exit = Some(total);
    world.state.lifecycle_staking_balance_before_exit = Some(staking_balance);

    // A different validator cannot remove the joiner. The failed preflight or
    // reverted receipt must leave every lifecycle/accounting value unchanged.
    let other_key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    let target = addr.parse().expect("joiner address");
    let unauthorized = world
        .rpc
        .deactivate_as(&other_key, target)
        .expect_err("third party deactivation must fail")
        .to_string();
    assert!(
        unauthorized.contains("unauthorized")
            || unauthorized.contains("receipt was not successful"),
        "unexpected unauthorized deactivate error: {unauthorized}"
    );
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(2));
    assert_eq!(world.rpc.stake_on(primary, &addr), Some(stake));
    assert_eq!(world.rpc.total_staked_on(primary), Some(total));

    world.rpc.deactivate(&key).expect("deactivate");
    sleep(Duration::from_secs(6));
    assert_eq!(
        world.rpc.validator_status(primary, &addr),
        Some(3),
        "deactivated -> EXITING (3)"
    );
    assert!(
        world.rpc.is_participant(primary, &addr),
        "EXITING stays a participant until the reshare"
    );
    assert_eq!(
        world.rpc.consensus_count(primary),
        Some(5),
        "consensus set still 5"
    );

    let repeated = world
        .rpc
        .deactivate(&key)
        .expect_err("repeated voluntary exit must fail")
        .to_string();
    assert!(
        repeated.contains("can only deactivate an active validator")
            || repeated.contains("receipt was not successful"),
        "unexpected repeated deactivate error: {repeated}"
    );
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(3));
    assert_eq!(world.rpc.stake_on(primary, &addr), Some(stake));
    assert_eq!(world.rpc.total_staked_on(primary), Some(total));
}

/// S3 — the exclusion reshare shrinks the set to 4, the exited validator becomes
/// UNBONDING, and its node DEMOTES to a verifier-follower that keeps following.
#[then("it exits, the committee reshares down, and the node demotes to a follower")]
fn exits_and_demotes(world: &mut World) {
    let primary = world.validators.primary_port();
    let idx = world.validators.joiner_index();
    let joiner_port = world.validators.http_port(idx);
    let addr = world.state.joiner_addr.clone().expect("joiner addr");

    let mut exit_h = 0u64;
    for _ in 0..45 {
        sleep(Duration::from_secs(10));
        let st = world.rpc.validator_status(primary, &addr);
        let cc = world.rpc.consensus_count(primary);
        if cc == Some(4) && st == Some(4) {
            exit_h = world.rpc.head(primary).unwrap_or(0);
            break;
        }
    }
    assert_eq!(
        world.rpc.validator_status(primary, &addr),
        Some(4),
        "exited -> UNBONDING (4)"
    );
    assert_eq!(
        world.rpc.consensus_count(primary),
        Some(4),
        "consensus set shrank to 4"
    );
    let exited_stake = world.state.lifecycle_stake_before_exit.expect("exit stake");
    let total_before = world
        .state
        .lifecycle_total_before_exit
        .expect("total before exit");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, exit_h.saturating_add(1), 30),
        "committee did not finalize the post-boundary block that drains UNBONDING stake"
    );
    for _ in 0..20 {
        if world.rpc.stake_on(primary, &addr) == Some(alloy_primitives::U256::ZERO)
            && world.rpc.total_staked_on(primary) == Some(total_before - exited_stake)
        {
            break;
        }
        sleep(Duration::from_secs(1));
    }
    assert_eq!(
        world.rpc.stake_on(primary, &addr),
        Some(alloy_primitives::U256::ZERO),
        "UNBONDING validator must have zero bonded stake"
    );
    assert_eq!(
        world.rpc.total_staked_on(primary),
        Some(total_before - exited_stake),
        "total_staked must decrease by exactly the exited stake"
    );
    assert_eq!(
        world.rpc.staking_balance_on(primary),
        world.state.lifecycle_staking_balance_before_exit,
        "moving stake to the unbonding queue must not move native value"
    );
    assert!(
        world.localnet.log_has(
            idx,
            "demoting to verifier-follower of the resharded committee"
        ),
        "node did not demote to a verifier-follower"
    );

    sleep(Duration::from_secs(20));
    let vh = world.rpc.head(joiner_port).unwrap_or(0);
    assert!(
        vh > exit_h,
        "demoted node stopped following finality (head {vh} <= {exit_h})"
    );

    // A new offer is still executed by the demoted follower (supply parity).
    let wwd = world.state.wwd.clone().expect("wwd");
    let v2 = world
        .validators
        .by_name("validator-2")
        .expect("v2")
        .evm_key()
        .expect("v2 key");
    world.state.tribute_tx_hash = world
        .rpc
        .offer_until_supply_hash(&v2, &wwd, primary, "3", 5);
    assert!(
        world.state.tribute_tx_hash.is_some(),
        "post-exit offer did not land (supply != 3)"
    );
    sleep(Duration::from_secs(6));
    assert_eq!(
        world.rpc.supply(primary),
        world.rpc.supply(joiner_port),
        "demoted follower supply parity"
    );
    world
        .mongodb
        .wait_for_tribute_projection_on_nodes(
            world
                .state
                .tribute_tx_hash
                .as_deref()
                .expect("post-exit tribute tx"),
            60,
            world.validators.joiner_index() + 1,
        )
        .expect("post-exit Tribute and indexes must match across validators and follower");
}

/// Mature the short E2E unbonding entry and prove exact value conservation,
/// caller isolation, idempotency, per-node parity, and continued finalization.
#[then("its unbonded stake can be claimed with exact accounting")]
fn claim_with_exact_accounting(world: &mut World) {
    let primary = world.validators.primary_port();
    let addr = world.state.joiner_addr.clone().expect("joiner addr");
    let key = world.validators.joiner().evm_key().expect("joiner key");
    let amount = world.state.lifecycle_stake_before_exit.expect("exit stake");

    // The configured period is eight seconds; wait for a block timestamp beyond
    // it so claimability is an observed chain condition, not a wall-clock guess.
    sleep(Duration::from_secs(10));
    let before_height = world
        .rpc
        .finalized(primary)
        .expect("finalized before claim");
    let user_before = world
        .rpc
        .balance_on(primary, &addr)
        .expect("claimant balance");
    let staking_before = world
        .rpc
        .staking_balance_on(primary)
        .expect("Staking balance before claim");

    // claimUnbonded has no target argument: another EOA can only inspect/claim
    // its own queue. An empty successful claim must not touch the joiner's value.
    let other_key = world
        .validators
        .by_name("validator-0")
        .expect("validator-0")
        .evm_key()
        .expect("validator-0 key");
    world
        .rpc
        .claim_unbonded(&other_key)
        .expect("unrelated caller's empty claim receipt");
    assert_eq!(world.rpc.balance_on(primary, &addr), Some(user_before));
    assert_eq!(world.rpc.staking_balance_on(primary), Some(staking_before));
    assert_eq!(world.rpc.validator_status(primary, &addr), Some(4));

    let receipt = world.rpc.claim_unbonded(&key).expect("claim unbonded");
    let fee = Rpc::receipt_gas_cost(&receipt).expect("claim receipt gas accounting");
    assert_eq!(
        world.rpc.staking_balance_on(primary),
        Some(staking_before - amount),
        "claim must transfer exactly the queued amount out of Staking"
    );
    assert_eq!(
        world.rpc.balance_on(primary, &addr),
        Some(user_before + amount - fee),
        "claimant balance must increase by claim minus exact transaction fee"
    );
    assert_eq!(
        world.rpc.validator_status(primary, &addr),
        Some(5),
        "mature claimed validator must become INACTIVE"
    );

    // A repeated claim succeeds as an empty idempotent operation and cannot pay
    // the queue twice; only its own transaction fee may reduce the caller.
    let repeat_user_before = world
        .rpc
        .balance_on(primary, &addr)
        .expect("repeat balance");
    let repeat_staking_before = world
        .rpc
        .staking_balance_on(primary)
        .expect("repeat Staking balance");
    let repeat = world.rpc.claim_unbonded(&key).expect("repeat empty claim");
    let repeat_fee = Rpc::receipt_gas_cost(&repeat).expect("repeat claim gas");
    assert_eq!(
        world.rpc.staking_balance_on(primary),
        Some(repeat_staking_before),
        "repeated claim must not transfer value twice"
    );
    assert_eq!(
        world.rpc.balance_on(primary, &addr),
        Some(repeat_user_before - repeat_fee),
        "empty repeated claim may charge only its exact gas fee"
    );

    let expected_total = world.state.lifecycle_total_before_exit.expect("total") - amount;
    let expected_staking = repeat_staking_before;
    for port in world.validators.committee_ports() {
        assert_eq!(
            world.rpc.stake_on(port, &addr),
            Some(alloy_primitives::U256::ZERO)
        );
        assert_eq!(world.rpc.total_staked_on(port), Some(expected_total));
        assert_eq!(world.rpc.staking_balance_on(port), Some(expected_staking));
        assert_eq!(world.rpc.validator_status(port, &addr), Some(5));
    }
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, before_height + 3, 30),
        "committee did not continue finalizing after exit and claim"
    );
}
