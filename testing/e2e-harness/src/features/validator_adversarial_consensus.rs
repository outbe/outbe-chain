//! Defensive boundary-validation scenario using an isolated patched proposer.
//!
//! Honest nodes remain on the normal binary. The variant proposer changes one
//! internally cross-consistent boundary artifact and the assertions prove that
//! honest nodes compare it with their independently reconstructed expectation.

use std::sync::Mutex;
use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::{Address, B256};
use bytes::Bytes as CodecBytes;
use commonware_codec::DecodeExt;
use commonware_cryptography::bls12381;
use commonware_utils::{modulo, ordered::Set};
use cucumber::{given, then, when};

use crate::world::localnet::{BootstrapProfile, StartOpts};
use crate::world::World;

const DKG_COMPLETED_LOG: &str = "DKG completed; waiting for activation height";
const HONEST_REJECTION_LOG: &str = "block BoundaryOutcome does not match pending DKG boundary";
const INJECTION_LOG: &str = "E2E_ADVERSARY emitted one self-consistent omit-active boundary";

#[derive(Clone, Debug)]
struct BoundaryDefenseScratch {
    planned_activation: u64,
    leader_index: usize,
    omitted: Address,
    honest_indices: Vec<usize>,
    expected_members: Vec<Address>,
    expected_records: Vec<(Address, crate::world::rpc::ValidatorRecord)>,
    honest_finalized_height: Option<u64>,
}

static BOUNDARY_DEFENSE: Mutex<Option<BoundaryDefenseScratch>> = Mutex::new(None);

fn parse_status_u64(world: &World, port: u16, field: &str) -> u64 {
    world
        .rpc
        .consensus_status_field(port, field)
        .and_then(|value| value.trim_matches('"').parse().ok())
        .unwrap_or_else(|| panic!("consensus status field {field} is not a u64"))
}

fn parse_last_vrf_seed(world: &World, port: u16) -> B256 {
    world
        .rpc
        .consensus_status_field(port, "lastVrfSeed")
        .and_then(|value| value.trim_matches('"').parse().ok())
        .expect("committed lastVrfSeed")
}

fn validator_index_for_address(world: &World, address: Address) -> usize {
    (0..world.validators.size())
        .find(|index| {
            world
                .validators
                .get(*index)
                .evm_key()
                .ok()
                .and_then(|key| world.rpc.address_of(&key))
                .and_then(|text| text.parse::<Address>().ok())
                == Some(address)
        })
        .unwrap_or_else(|| panic!("no local validator owns {address:#x}"))
}

fn view_one_leader(world: &World, port: u16, seed: B256) -> Address {
    let active = world
        .rpc
        .active_consensus_set(port)
        .expect("active consensus set for leader election");
    let mut key_to_address = Vec::with_capacity(active.len());
    for address in active {
        let record = world
            .rpc
            .validator_record(port, &format!("{address:#x}"))
            .expect("leader-election validator record");
        let public_key = <bls12381::PublicKey as DecodeExt<()>>::decode(CodecBytes::from(
            record.consensus_pubkey.to_vec(),
        ))
        .expect("decode leader-election BLS key");
        key_to_address.push((public_key, address));
    }
    let ordered = Set::from_iter_dedup(key_to_address.iter().map(|(key, _)| key.clone()));
    assert_eq!(
        ordered.len(),
        key_to_address.len(),
        "committee contains a duplicate consensus key"
    );
    let participant = modulo(seed.as_slice(), ordered.len() as u64) as usize;
    let elected_key = ordered.get(participant).expect("elected participant index");
    key_to_address
        .iter()
        .find_map(|(key, address)| (key == elected_key).then_some(*address))
        .expect("map elected BLS key to EVM address")
}

fn wait_log_count(world: &World, index: usize, needle: &str, minimum: usize, tries: u32) {
    for _ in 0..tries {
        if world.localnet.log_count(index, needle) >= minimum {
            return;
        }
        sleep(Duration::from_secs(1));
    }
    panic!("validator-{index} did not log '{needle}'");
}

fn honest_ports(world: &World, scratch: &BoundaryDefenseScratch) -> Vec<u16> {
    scratch
        .honest_indices
        .iter()
        .map(|index| world.validators.http_port(*index))
        .collect()
}

#[given("a fresh localnet has completed a valid pending reshare boundary")]
fn pending_valid_boundary_is_ready(world: &mut World) {
    // Build before starting the chain so compilation cannot consume the live
    // pre-activation window.
    let binary = world
        .localnet
        .build_omit_active_boundary_binary()
        .expect("build isolated boundary-variant binary");
    assert!(
        binary.is_file(),
        "isolated boundary-variant binary is absent"
    );

    let profile = BootstrapProfile::default()
        .with_dkg_timing(90, 40, 40)
        .expect("valid adversarial DKG timing")
        .with_dev_felony_threshold(89)
        .expect("valid adversarial felony threshold");
    world
        .localnet
        .bootstrap_with_profile(world.validators.size(), &profile)
        .expect("bootstrap adversarial localnet");
    world
        .localnet
        .start(&StartOpts::default())
        .expect("start adversarial localnet");
    assert!(
        world
            .rpc
            .wait_bootstrapped(world.localnet.tee_bootstrap_wait_attempts()),
        "TEE chain did not bootstrap"
    );

    let port = world.validators.primary_port();
    for _ in 0..180 {
        let completed_everywhere = (0..world.validators.size())
            .all(|index| world.localnet.log_has(index, DKG_COMPLETED_LOG));
        if completed_everywhere {
            break;
        }
        sleep(Duration::from_secs(2));
    }
    assert!(
        (0..world.validators.size()).all(|index| world.localnet.log_has(index, DKG_COMPLETED_LOG)),
        "not every validator reconstructed the same pending DKG boundary"
    );
    let planned = parse_status_u64(world, port, "nextPlannedActivationHeight");
    assert!(
        world.rpc.head(port).is_some_and(|head| head < planned),
        "DKG boundary was no longer pending when the defensive test attached"
    );
    *BOUNDARY_DEFENSE.lock().expect("boundary-defense scratch") = Some(BoundaryDefenseScratch {
        planned_activation: planned,
        leader_index: usize::MAX,
        omitted: Address::ZERO,
        honest_indices: Vec::new(),
        expected_members: Vec::new(),
        expected_records: Vec::new(),
        honest_finalized_height: None,
    });
}

#[given("the next view-one leader is selected from the committed VRF seed")]
fn select_next_view_one_leader(world: &mut World) {
    let mut scratch = BOUNDARY_DEFENSE
        .lock()
        .expect("boundary-defense scratch")
        .take()
        .expect("adversarial setup");
    let port = world.validators.primary_port();
    let parent_height = scratch.planned_activation.saturating_sub(1);
    assert!(
        world.rpc.wait_finalized_at_least(port, parent_height, 180),
        "activation parent did not finalize"
    );
    let seed = parse_last_vrf_seed(world, port);
    let leader = view_one_leader(world, port, seed);
    scratch.leader_index = validator_index_for_address(world, leader);
    scratch.honest_indices = (0..world.validators.size())
        .filter(|index| *index != scratch.leader_index)
        .collect();

    scratch.expected_members = world
        .rpc
        .active_consensus_set(port)
        .expect("expected boundary membership");
    scratch.omitted = scratch
        .expected_members
        .iter()
        .copied()
        .find(|address| *address != leader)
        .expect("an active validator other than the leader");
    scratch.expected_records = scratch
        .expected_members
        .iter()
        .map(|address| {
            (
                *address,
                world
                    .rpc
                    .validator_record(port, &format!("{address:#x}"))
                    .expect("pre-injection validator record"),
            )
        })
        .collect();
    *BOUNDARY_DEFENSE.lock().expect("boundary-defense scratch") = Some(scratch);
}

#[when("that leader restarts with the omit-active-boundary adversarial binary")]
fn restart_selected_leader(world: &mut World) {
    let scratch = BOUNDARY_DEFENSE.lock().expect("boundary-defense scratch");
    let scratch = scratch.as_ref().expect("selected leader");
    world
        .localnet
        .restart_validator_with_omit_active_boundary(scratch.leader_index, scratch.omitted)
        .expect("restart only the selected leader with isolated binary");
}

#[when("it proposes a self-consistent boundary that omits one active validator")]
fn wait_for_variant_boundary_proposal(world: &mut World) {
    let scratch = BOUNDARY_DEFENSE.lock().expect("boundary-defense scratch");
    let scratch = scratch.as_ref().expect("selected leader");
    wait_log_count(world, scratch.leader_index, INJECTION_LOG, 1, 90);
    for honest in &scratch.honest_indices {
        wait_log_count(world, *honest, HONEST_REJECTION_LOG, 1, 90);
    }
}

#[then("every honest validator rejects the malicious boundary without changing committee state")]
fn honest_validators_reject_variant(world: &mut World) {
    let scratch = BOUNDARY_DEFENSE.lock().expect("boundary-defense scratch");
    let scratch = scratch.as_ref().expect("selected leader");
    for honest in &scratch.honest_indices {
        assert_eq!(
            world.localnet.log_count(*honest, HONEST_REJECTION_LOG),
            1,
            "validator-{honest} did not record exactly one rejected variant proposal"
        );
        let port = world.validators.http_port(*honest);
        assert_eq!(
            world.rpc.active_consensus_set(port),
            Some(scratch.expected_members.clone()),
            "validator-{honest} changed committee membership after rejected proposal"
        );
        for (address, before) in &scratch.expected_records {
            let after = world
                .rpc
                .validator_record(port, &format!("{address:#x}"))
                .expect("validator record after rejected proposal");
            assert_eq!(after.status, before.status);
            assert_eq!(after.has_bls_share, before.has_bls_share);
            assert_eq!(after.consensus_pubkey, before.consensus_pubkey);
        }
    }
}

#[then("a later honest leader commits the original expected boundary")]
fn honest_leader_commits_expected_boundary(world: &mut World) {
    let scratch = BOUNDARY_DEFENSE.lock().expect("boundary-defense scratch");
    let scratch = scratch.as_ref().expect("selected leader");
    for honest in &scratch.honest_indices {
        let port = world.validators.http_port(*honest);
        for _ in 0..120 {
            if parse_status_u64(world, port, "lastDkgActivationHeight")
                >= scratch.planned_activation
            {
                break;
            }
            sleep(Duration::from_secs(1));
        }
        assert!(
            parse_status_u64(world, port, "lastDkgActivationHeight") >= scratch.planned_activation,
            "validator-{honest} did not commit the expected boundary"
        );
    }
}

#[then("honest committee membership, shares, snapshots, and state roots converge")]
fn honest_state_converges(world: &mut World) {
    let mut scratch = BOUNDARY_DEFENSE
        .lock()
        .expect("boundary-defense scratch")
        .take()
        .expect("selected leader");
    let ports = honest_ports(world, &scratch);
    let common_height = ports
        .iter()
        .filter_map(|port| world.rpc.finalized(*port))
        .min()
        .expect("honest finalized height");
    for port in &ports {
        assert!(
            world.rpc.wait_finalized_at_least(*port, common_height, 30),
            "honest RPC {port} did not reach common finalized height"
        );
    }
    let expected_root = world
        .rpc
        .state_root(ports[0], common_height)
        .expect("honest state root");
    let expected_epoch = world.rpc.epoch_on(ports[0]);
    let expected_material = world
        .rpc
        .consensus_status_field(ports[0], "vrfMaterialVersion");
    for port in &ports {
        assert_eq!(
            world.rpc.active_consensus_set(*port),
            Some(scratch.expected_members.clone())
        );
        for address in &scratch.expected_members {
            let record = world
                .rpc
                .validator_record(*port, &format!("{address:#x}"))
                .expect("post-boundary validator record");
            assert_eq!(record.status, 2);
            assert!(record.has_bls_share);
        }
        assert_eq!(world.rpc.epoch_on(*port), expected_epoch);
        assert_eq!(
            world
                .rpc
                .consensus_status_field(*port, "vrfMaterialVersion"),
            expected_material,
            "incoming snapshot VRF material differs on RPC {port}"
        );
        assert_eq!(
            world.rpc.state_root(*port, common_height),
            Some(expected_root.clone()),
            "honest state root differs on RPC {port}"
        );
    }
    scratch.honest_finalized_height = Some(common_height);
    *BOUNDARY_DEFENSE.lock().expect("boundary-defense scratch") = Some(scratch);
}

#[when("the malicious validator restarts with the normal binary")]
fn restore_normal_binary(world: &mut World) {
    let scratch = BOUNDARY_DEFENSE.lock().expect("boundary-defense scratch");
    let scratch = scratch.as_ref().expect("selected leader");
    world
        .localnet
        .restart_validator_with_normal_binary(scratch.leader_index)
        .expect("restore selected validator to normal binary");
}

#[then("it catches up to the honest finalized state")]
fn restored_validator_catches_up(world: &mut World) {
    let scratch = BOUNDARY_DEFENSE.lock().expect("boundary-defense scratch");
    let scratch = scratch.as_ref().expect("selected leader");
    let target = scratch
        .honest_finalized_height
        .expect("honest finalized convergence height");
    let restored_port = world.validators.http_port(scratch.leader_index);
    assert!(
        world.rpc.wait_finalized_at_least(restored_port, target, 90),
        "restored validator did not catch up"
    );
    let honest_port = world.validators.http_port(scratch.honest_indices[0]);
    assert_eq!(
        world.rpc.state_root(restored_port, target),
        world.rpc.state_root(honest_port, target),
        "restored validator state root differs at honest finalized height"
    );
    assert_eq!(
        world.rpc.active_consensus_set(restored_port),
        Some(scratch.expected_members.clone())
    );
    for address in &scratch.expected_members {
        assert_eq!(
            world.rpc.has_share(restored_port, &format!("{address:#x}")),
            Some(true)
        );
    }
}

#[then("exactly one adversarial boundary injection is recorded")]
fn exactly_one_variant_is_recorded(world: &mut World) {
    let scratch = BOUNDARY_DEFENSE
        .lock()
        .expect("boundary-defense scratch")
        .take()
        .expect("selected leader");
    assert_eq!(
        world
            .localnet
            .log_count(scratch.leader_index, INJECTION_LOG),
        1
    );
}
