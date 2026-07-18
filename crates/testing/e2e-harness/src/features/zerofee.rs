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
