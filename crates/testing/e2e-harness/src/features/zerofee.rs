//! Steps for the live EIP-7702 ZeroFee sponsorship vertical slice.

use cucumber::{then, when};

use crate::world::World;

#[then("Pectra and the ZeroFee views are ready")]
fn readiness(world: &mut World) {
    world.rpc.assert_zerofee_readiness();
}

#[when("a funded fresh account delegates to ZeroFee with EIP-7702")]
fn delegate(world: &mut World) {
    let funder = world
        .validators
        .operator("validator-0")
        .expect("resolve funding validator");
    world
        .rpc
        .prepare_zerofee_account(&funder, &mut world.state)
        .expect("fund and delegate fresh account");
}

#[then("the exact ZeroFee delegation designator is installed")]
fn delegation_installed(world: &mut World) {
    world.rpc.assert_zerofee_delegation(&world.state);
}

#[when("the account submits eight eligible sponsored reward calls")]
fn submit_sponsored_quota(world: &mut World) {
    world
        .rpc
        .submit_zerofee_quota(&mut world.state)
        .expect("submit sponsored quota");
}

#[then("all eight calls succeed without fees and consume the full quota")]
fn sponsored_quota_consumed(world: &mut World) {
    world.rpc.assert_zerofee_quota(&world.state);
}

#[when("the account submits a ninth eligible sponsored reward call")]
fn submit_ninth(world: &mut World) {
    world
        .rpc
        .submit_zerofee_ninth(&mut world.state)
        .expect("submit ninth sponsored call");
}

#[then("the ninth call is mined as ZeroFee soft failure 110 without a fee")]
fn ninth_soft_fails(world: &mut World) {
    world.rpc.assert_zerofee_ninth(&world.state);
}

#[when("the quota-exhausted account submits the same call with a priority fee")]
fn submit_paid(world: &mut World) {
    world
        .rpc
        .submit_zerofee_paid(&mut world.state)
        .expect("submit paid fallback");
}

#[then("the paid call succeeds, charges a fee, and does not change the quota")]
fn paid_fallback(world: &mut World) {
    world.rpc.assert_zerofee_paid(&world.state);
}

#[then("the product CLI emits a canonical ZeroFee authorization")]
fn cli_authorization(world: &mut World) {
    world.rpc.assert_zerofee_cli_authorization(&world.state);
}

#[when("the exact included ZeroFee delegation transaction is replayed")]
fn replay_delegation(world: &mut World) {
    world
        .rpc
        .replay_zerofee_delegation(&mut world.state)
        .expect("replay exact EIP-7702 transaction");
}

#[then("the replay is rejected without changing delegation or quota")]
fn replay_rejected(world: &mut World) {
    assert!(
        world.state.zerofee_replay_error.is_some(),
        "exact transaction replay produced no RPC rejection"
    );
    world.rpc.assert_zerofee_delegation(&world.state);
    world.rpc.assert_zerofee_quota(&world.state);
}

#[when(expr = "validator {string} restarts after quota exhaustion")]
fn restart_validator(world: &mut World, validator: String) {
    let index = validator
        .strip_prefix("validator-")
        .and_then(|value| value.parse::<usize>().ok())
        .expect("validator-N name");
    let before = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("finalized height before validator restart");
    world
        .localnet
        .kill_validator(index)
        .expect("kill validator");
    world.localnet.restart().expect("restart validator");
    assert!(
        world
            .rpc
            .wait_block(
                world.validators.http_port(index),
                before.saturating_add(1),
                60,
            )
            .is_some(),
        "restarted validator did not resume block sync"
    );
}

#[when("the entire committee restarts after quota exhaustion")]
fn restart_committee(world: &mut World) {
    let before = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("finalized height before committee restart");
    world
        .localnet
        .restart_committee_and_enclaves()
        .expect("restart committee and enclaves");
    assert!(
        world
            .rpc
            .wait_block(
                world.validators.primary_port(),
                before.saturating_add(1),
                90
            )
            .is_some(),
        "committee did not resume after restart"
    );
}

#[then("the exhausted ZeroFee state is identical on every validator")]
fn quota_persisted(world: &mut World) {
    let mut ports = vec![world.validators.primary_port()];
    ports.extend(world.validators.peer_ports());
    world
        .rpc
        .assert_zerofee_persisted_on_ports(&world.state, &ports);
}
