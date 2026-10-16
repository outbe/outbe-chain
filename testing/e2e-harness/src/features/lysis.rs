//! Native Metadosis Lysis acceptance through the production node and enclave.
mod bulk;
use crate::world::World;
use cucumber::{given, then, when};
use outbe_metadosis::WwdStatus;
use std::{
    thread::sleep,
    time::{Duration, Instant},
};

#[given("a fresh four-validator native Lysis localnet")]
fn fresh_native_lysis(world: &mut World) {
    assert_eq!(world.validators.size(), 4);
    super::common::bootstrap_localnet(
        world,
        6,
        &[("TESTNET_METADOSIS_OFFERING_SECONDS", "3600".to_owned())],
    );
    let day = world
        .localnet
        .prepare_cross_currency_tribute_fixture()
        .expect("native Offering fixture");
    world
        .localnet
        .prepare_lysis_entry_price()
        .expect("nonzero native Lysis entry price");
    world.state.wwd = Some(day.to_string());
    world.state.tribute_owner_keys = world
        .localnet
        .fund_tribute_owners(40)
        .expect("fund native Tribute owners");
    super::common::start_bootstrapped_localnet(
        world,
        &crate::world::localnet::StartOpts::with_voting_window(6),
    );
}

#[when("the committee logical clock reaches the native Lysis processing time")]
fn reach_processing_time(world: &mut World) {
    let day = world
        .state
        .wwd
        .as_deref()
        .expect("offering day")
        .parse()
        .expect("numeric WWD");
    let ports = world.validators.committee_ports();
    let height = world.rpc.finalized(ports[0]).expect("finalized height");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("common finalized state");
    let state = world
        .rpc
        .metadosis_wwd_state_at(ports[0], day, checkpoint.height)
        .expect("native WWD state");
    assert_eq!(
        state.status,
        WwdStatus::Offering.as_u8(),
        "Tributes must be offered before processing"
    );
    for &port in &ports {
        assert_eq!(
            world
                .rpc
                .metadosis_wwd_state_at(port, day, checkpoint.height),
            Some(state.clone())
        );
    }
    let (_, pending) =
        super::common::restart_committee_at_logical_time(world, state.scheduled_process_time);
    if let Some(pending) = pending {
        while !super::price_oracle::observe_pending_publication(world, &pending) {
            sleep(Duration::from_secs(1));
        }
    }
}

#[then("Metadosis completes confidential Lysis and stores ready Nods on every validator")]
fn native_lysis_completes(world: &mut World) {
    let day = world
        .state
        .wwd
        .as_deref()
        .expect("offering day")
        .parse::<u32>()
        .expect("numeric WWD");
    let ports = world.validators.committee_ports();
    let deadline = Instant::now() + Duration::from_secs(240);
    let height = loop {
        let states = ports
            .iter()
            .map(|&port| {
                world
                    .rpc
                    .metadosis_wwd_state_on(port, day)
                    .map(|state| WwdStatus::try_from(state.status).expect("valid native status"))
            })
            .collect::<Vec<_>>();
        assert!(
            !states.contains(&Some(WwdStatus::Failed)),
            "native Lysis failed: {states:?}"
        );
        if states
            .iter()
            .all(|status| *status == Some(WwdStatus::Completed))
        {
            let changes = world
                .rpc
                .finalized_metadosis_wwd_status_changes_on(ports[0], day)
                .expect("native WWD status events");
            let completed = changes
                .iter()
                .filter(|event| event.new_status == WwdStatus::Completed.as_u8())
                .collect::<Vec<_>>();
            assert_eq!(completed.len(), 1, "exactly one completed transition");
            break completed[0].block_number;
        }
        assert!(
            Instant::now() < deadline,
            "native Lysis did not complete: {states:?}"
        );
        sleep(Duration::from_secs(1));
    };
    world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("same native Lysis block and state root");
    let supply = world.rpc.nod_supply(ports[0]).expect("native Nod supply");
    assert!(supply > 0);
    for port in ports {
        assert_eq!(world.rpc.nod_supply(port), Some(supply));
    }
    world.state.lysis_completion_height = Some(height);
}

#[when("all validator nodes restart with preserved EVM state")]
fn restart_native_nodes(world: &mut World) {
    let height = world
        .state
        .lysis_completion_height
        .expect("completed Lysis");
    let ports = world.validators.committee_ports();
    let before = world
        .rpc
        .wait_finalized_checkpoint(&ports, height, 120)
        .expect("pre-restart checkpoint");
    world
        .localnet
        .restart_committee_preserving_enclaves()
        .expect("restart native validators");
    assert_eq!(
        world
            .rpc
            .wait_finalized_checkpoint(&ports, height + 1, 120)
            .expect("native finality after restart")
            .height,
        height + 1
    );
    for port in ports {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, height)
                .expect("retained checkpoint"),
            before
        );
    }
}
