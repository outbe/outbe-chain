//! Steps for the live EIP-7702 ZeroFee sponsorship vertical slice.

use std::time::{SystemTime, UNIX_EPOCH};

use cucumber::{given, then, when};

use crate::features::common::boot_localnet_with_opts;
use crate::world::localnet::StartOpts;
use crate::world::World;

const ZEROFEE_BOUNDARY_FORMING_PERIOD_SECONDS: u64 = 50 * 60 * 60;
const ZEROFEE_BOUNDARY_LEAD_SECONDS: u64 = 10 * 60;

fn boundary_protocol_tuning() -> [(&'static str, String); 1] {
    [(
        "TESTNET_METADOSIS_FORMING_SECONDS",
        ZEROFEE_BOUNDARY_FORMING_PERIOD_SECONDS.to_string(),
    )]
}

fn boundary_start_opts(now_secs: u64) -> StartOpts {
    StartOpts::near_next_utc_day_with_lead(20, now_secs, ZEROFEE_BOUNDARY_LEAD_SECONDS)
}

#[given("a fresh localnet near the next UTC worldwide-day boundary")]
fn near_day_boundary(world: &mut World) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_secs();
    let tuning = boundary_protocol_tuning();
    boot_localnet_with_opts(world, 20, &tuning, boundary_start_opts(now));
    let timestamp = world
        .rpc
        .latest_block_timestamp(world.validators.primary_port())
        .expect("latest block timestamp in boundary setup");
    assert!(
        timestamp % 86_400 >= 86_400 - ZEROFEE_BOUNDARY_LEAD_SECONDS,
        "debug-node clock did not enter the worldwide-day boundary window: {timestamp}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_profile_keeps_forming_open_after_the_genesis_clock_shift() {
        let wwd_start = outbe_primitives::time::date_key_to_utc_timestamp(20260812)
            - outbe_primitives::time::UTC_PLUS_14_OFFSET;
        let shifted_genesis = outbe_primitives::time::date_key_to_utc_timestamp(20260812) - 2 * 60;

        assert!(
            wwd_start + ZEROFEE_BOUNDARY_FORMING_PERIOD_SECONDS > shifted_genesis,
            "the ZeroFee day-boundary clock must not start after FORMING expired"
        );
        assert_eq!(
            boundary_protocol_tuning(),
            [("TESTNET_METADOSIS_FORMING_SECONDS", "180000".to_owned())]
        );
    }

    #[test]
    fn boundary_profile_leaves_sgx_bootstrap_and_feeder_publication_headroom() {
        const NOW: u64 = 1_700_000_000;
        const SECONDS_PER_DAY: u64 = 86_400;

        let opts = boundary_start_opts(NOW);
        let shifted_now = (NOW as i64
            + opts
                .unix_time_offset_secs
                .expect("boundary profile must shift the node clock"))
            as u64;
        let next_day = NOW - (NOW % SECONDS_PER_DAY) + SECONDS_PER_DAY;

        assert_eq!(next_day - shifted_now, 600);
    }
}

#[then("Pectra and the ZeroFee views are ready")]
fn readiness(world: &mut World) {
    world.rpc.assert_zerofee_readiness();
}

#[when("an account with one atomic COEN bootstraps its ZeroFee delegation")]
fn delegate(world: &mut World) {
    let funder = world
        .validators
        .operator("validator-0")
        .expect("resolve funding validator");
    world
        .rpc
        .prepare_zerofee_account(&funder, &mut world.state)
        .expect("bootstrap fresh ZeroFee account");
}

#[then("the exact ZeroFee delegation designator is installed")]
fn delegation_installed(world: &mut World) {
    world.rpc.assert_zerofee_delegation(&world.state);
}

#[when("the exact included ZeroFee bootstrap transaction is replayed")]
fn replay_bootstrap_transaction(world: &mut World) {
    world
        .rpc
        .replay_zerofee_bootstrap_transaction(&world.state)
        .expect("replay exact included bootstrap transaction");
}

#[then("the bootstrap replay is rejected without changing account or quota state")]
fn bootstrap_replay_rejected(world: &mut World) {
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

#[when("the exact included sponsored ZeroFee transaction is replayed")]
fn replay_sponsored_transaction(world: &mut World) {
    world
        .rpc
        .replay_zerofee_sponsored_transaction(&mut world.state)
        .expect("replay exact included sponsored transaction");
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

#[when("a funded account submits an EIP-7702 authorization for the wrong chain")]
fn invalid_authorization(world: &mut World) {
    let funder = world.validators.get(0);
    world
        .rpc
        .submit_zerofee_invalid_authorization(&funder, &mut world.state)
        .expect("submit wrong-chain EIP-7702 authorization");
}

#[then("the invalid authorization leaves delegation and ZeroFee quota unset")]
fn invalid_authorization_preserves_state(world: &mut World) {
    world.rpc.assert_zerofee_invalid_authorization(&world.state);
}

#[when("the account delegates to a non-ZeroFee target and submits a sponsored-shaped call")]
fn wrong_target_delegation(world: &mut World) {
    world
        .rpc
        .submit_zerofee_wrong_target(&mut world.state)
        .expect("submit wrong-target delegation and call");
}

#[then("the wrong-target call receives no sponsorship and leaves ZeroFee quota unchanged")]
fn wrong_target_not_sponsored(world: &mut World) {
    world.rpc.assert_zerofee_wrong_target(&world.state);
}

#[when("a stale conflicting authorization attempts to replace the wrong target")]
fn conflicting_authorization(world: &mut World) {
    world
        .rpc
        .submit_zerofee_conflicting_authorization(&mut world.state)
        .expect("submit stale conflicting authorization");
}

#[then("the conflicting authorization leaves the prior delegation and ZeroFee quota unchanged")]
fn conflicting_authorization_preserves_state(world: &mut World) {
    world
        .rpc
        .assert_zerofee_conflicting_authorization(&world.state);
}

#[when("the chain crosses into the next worldwide day")]
fn cross_worldwide_day(world: &mut World) {
    world
        .rpc
        .wait_zerofee_day_rollover_and_submit(&mut world.state)
        .expect("wait for ZeroFee worldwide-day rollover");
}

#[then(
    "ZeroFee quota resets lazily and the first new-day sponsored call succeeds on every validator"
)]
fn quota_resets_on_new_day(world: &mut World) {
    let ports = world.validators.committee_ports();
    world.rpc.assert_zerofee_day_rollover(&world.state, &ports);
}
