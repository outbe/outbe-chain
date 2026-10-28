//! Native Metadosis Lysis acceptance through the production node and enclave.
mod bulk;
use crate::world::World;
use cucumber::{given, then, when};
use std::{
    thread::sleep,
    time::{Duration, Instant},
};

const EMISSION_DAY_SUCCEEDED: u8 = 1;
const EMISSION_DAY_FAILED: u8 = 2;

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
        .prepare_native_lysis_fixture(day)
        .expect("linked native Lysis allocation and entry price");
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
    reach_native_lysis_processing_time(world);
}

pub(crate) fn reach_native_lysis_processing_time(world: &mut World) {
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
        state.status, 2,
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
    let height = wait_for_native_lysis(world);
    let (nod_height, nod_blocks) = create_nods_for_completed_day(world, day, &ports, height);
    let supply = world.rpc.nod_supply(ports[0]).expect("native Nod supply");
    assert!(supply > 0);
    for port in ports {
        assert_eq!(world.rpc.nod_supply(port), Some(supply));
    }
    world.state.lysis_completion_height = Some(height);
    world.state.nod_creation_height = Some(nod_height);
    world.state.nod_creation_blocks = nod_blocks;
}

/// Waits until confidential Lysis has succeeded on every validator.
///
/// Does not mint Nods. The native scenario mints afterwards; a lifecycle scenario
/// mints through each owner's `ITribute::createNod`.
pub(crate) fn wait_for_native_lysis(world: &World) -> u64 {
    let day = world
        .state
        .wwd
        .as_deref()
        .expect("offering day")
        .parse::<u32>()
        .expect("numeric WWD");
    let ports = world.validators.committee_ports();
    let emission_day = world
        .rpc
        .metadosis_wwd_state_on(ports[0], day)
        .expect("native WWD state")
        .day;
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        let states = ports
            .iter()
            .map(|&port| world.rpc.metadosis_emission_day_on(port, emission_day))
            .collect::<Vec<_>>();
        assert!(
            !states.iter().any(|ed| ed
                .as_ref()
                .is_some_and(|ed| ed.state == EMISSION_DAY_FAILED)),
            "native Lysis failed: {states:?}"
        );
        if states.iter().all(|ed| {
            ed.as_ref()
                .is_some_and(|ed| ed.state == EMISSION_DAY_SUCCEEDED)
        }) {
            let first = states[0].as_ref().expect("native Lysis emission day");
            assert_eq!(first.emission_day, emission_day);
            assert_eq!(first.wwd, day);
            assert!(
                states.iter().all(|other| other.as_ref() == Some(first)),
                "same EmissionDay state on every validator"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native Lysis did not complete: {states:?}"
        );
        sleep(Duration::from_secs(1));
    }
    let head_height = ports
        .iter()
        .filter_map(|&port| world.rpc.head(port))
        .max()
        .expect("native Lysis head height");
    world
        .rpc
        .wait_finalized_checkpoint(&ports, head_height, 120)
        .expect("native Lysis completion finalized on every validator");
    let completed = world
        .rpc
        .finalized_metadosis_lysis_completion_blocks_on(ports[0], day, head_height)
        .expect("canonical native Lysis completion events");
    assert_eq!(
        completed.len(),
        1,
        "exactly one native Lysis completion event"
    );
    let (height, completed_hash) = completed[0];
    let checkpoint = world
        .rpc
        .checkpoint_at(ports[0], height)
        .expect("exact native Lysis completion checkpoint");
    assert_eq!(checkpoint.block_hash, completed_hash);
    for &port in &ports {
        assert_eq!(
            world
                .rpc
                .checkpoint_at(port, height)
                .expect("native Lysis completion checkpoint on every validator"),
            checkpoint,
            "same native Lysis block and state root"
        );
        let state = world
            .rpc
            .metadosis_emission_day_at(port, emission_day, height)
            .expect("native Lysis emission day at the completion checkpoint");
        assert_eq!(state.state, EMISSION_DAY_SUCCEEDED);
    }
    height
}

fn tribute_owner_keys(
    world: &World,
) -> std::collections::BTreeMap<alloy_primitives::Address, String> {
    use crate::internal::eth;
    use std::collections::BTreeMap;

    let mut owner_keys = BTreeMap::new();
    for idx in 0..world.validators.size() {
        if let Ok(key) = world.validators.get(idx).evm_key() {
            if let Some(addr) = eth::address_of(&key) {
                owner_keys.insert(addr, key);
            }
        }
    }
    for key in &world.state.tribute_owner_keys {
        if let Some(addr) = eth::address_of(key) {
            owner_keys.insert(addr, key.clone());
        }
    }
    if let Some(addr) = eth::address_of(crate::world::forge::DEPLOYER_KEY) {
        owner_keys.insert(addr, crate::world::forge::DEPLOYER_KEY.to_owned());
    }
    owner_keys
}

fn live_owner_day_tribute_ids(
    url: &str,
    day: u32,
    owner_keys: &std::collections::BTreeMap<alloy_primitives::Address, String>,
) -> Vec<alloy_primitives::U256> {
    use crate::internal::{addresses, eth};
    use alloy_primitives::Address;

    let worldwide_day = outbe_primitives::time::WorldwideDay::new(day);
    let mut tribute_ids = Vec::new();
    for owner in owner_keys.keys() {
        let Ok(id) =
            outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(*owner, worldwide_day)
        else {
            continue;
        };
        let id = id.to_u256();
        let found: Option<Address> = eth::read_call(
            url,
            addresses::TRIBUTE_ADDR,
            &eth::ITribute::ownerOfCall { tributeId: id },
        );
        if found == Some(*owner) {
            tribute_ids.push(id);
        }
    }
    tribute_ids
}

fn assert_claimed_tributes_left_owner_lists(
    world: &World,
    ports: &[u16],
    calls: &[(alloy_primitives::U256, String)],
) {
    use crate::internal::{addresses, eth};
    use alloy_primitives::Address;

    for &port in ports {
        let port_url = world.rpc.url(port);
        for (tribute_id, key) in calls {
            let owner = eth::address_of(key).expect("createNod owner");
            let still_owner: Option<Address> = eth::read_call(
                &port_url,
                addresses::TRIBUTE_ADDR,
                &eth::ITribute::ownerOfCall {
                    tributeId: *tribute_id,
                },
            );
            assert!(
                still_owner.is_none(),
                "tribute {tribute_id} remains after createNod on port {port}"
            );
            let owned = world.rpc.tributes_by_owner(port, owner).unwrap_or_default();
            assert!(
                !owned.contains(tribute_id),
                "owner list on port {port} still contains tribute {tribute_id} after createNod"
            );
        }
    }
}

fn create_nods_for_completed_day(
    world: &World,
    day: u32,
    ports: &[u16],
    completed_height: u64,
) -> (u64, std::collections::BTreeMap<alloy_primitives::U256, u64>) {
    use crate::internal::{addresses, eth};
    use alloy_primitives::{Address, U256};
    use std::collections::BTreeMap;

    let primary = ports[0];
    let url = world.rpc.url(primary);
    let owner_keys = tribute_owner_keys(world);
    let tribute_ids = live_owner_day_tribute_ids(&url, day, &owner_keys);
    if tribute_ids.is_empty() {
        return (completed_height, BTreeMap::new());
    }

    let mut calls = Vec::with_capacity(tribute_ids.len());
    for tribute_id in &tribute_ids {
        let owner: Address = eth::read_call(
            &url,
            addresses::TRIBUTE_ADDR,
            &eth::ITribute::ownerOfCall {
                tributeId: *tribute_id,
            },
        )
        .expect("tribute owner before createNod");
        let league: u16 = eth::read_call(
            &url,
            addresses::TRIBUTE_ADDR,
            &eth::ITribute::getLeagueCall {
                tributeId: *tribute_id,
            },
        )
        .expect("tribute league before createNod");
        assert!(
            (1..=4096).contains(&league),
            "expected valid league in 1..=4096, got {league}"
        );
        let fraction: u64 = eth::read_call(
            &url,
            addresses::TRIBUTE_ADDR,
            &eth::ITribute::getDayLeagueFractionCall {
                worldwideDay: day,
                league,
            },
        )
        .expect("day league fraction after Metadosis");
        assert!(
            fraction > 0,
            "expected positive day league fraction for day {day} league {league}"
        );
        let key = owner_keys
            .get(&owner)
            .unwrap_or_else(|| panic!("missing private key for Tribute owner {owner:#x}"))
            .clone();
        calls.push((*tribute_id, key));
    }

    let mut last_height = completed_height;
    let mut blocks = BTreeMap::new();
    for chunk in calls.chunks(8) {
        let outcomes = std::thread::scope(|scope| {
            chunk
                .iter()
                .map(|(tribute_id, key)| {
                    let url = url.clone();
                    let tribute_id = *tribute_id;
                    scope.spawn(move || {
                        eth::send_call_outcome(
                            &url,
                            addresses::TRIBUTE_ADDR,
                            key,
                            &eth::ITribute::createNodCall {
                                tributeId: tribute_id,
                            },
                            Some(U256::ZERO),
                        )
                        .expect("submit ITribute::createNod")
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| handle.join().expect("join createNod worker"))
                .collect::<Vec<_>>()
        });
        for ((tribute_id, _), outcome) in chunk.iter().zip(&outcomes) {
            crate::features::settlement::assert_mined_success(outcome, "ITribute::createNod");
            let block_number = world
                .rpc
                .receipt_block_number(&outcome.transaction_hash, primary)
                .expect("createNod receipt block number");
            blocks.insert(*tribute_id, block_number);
            last_height = last_height.max(block_number);
        }
    }

    world
        .rpc
        .wait_finalized_checkpoint(ports, last_height, 120)
        .expect("createNod finalized on all validators");
    assert_claimed_tributes_left_owner_lists(world, ports, &calls);
    (last_height, blocks)
}

#[when("all validator nodes restart with preserved EVM state")]
fn restart_native_nodes(world: &mut World) {
    let height = world.state.nod_creation_height.expect("created Nods");
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
