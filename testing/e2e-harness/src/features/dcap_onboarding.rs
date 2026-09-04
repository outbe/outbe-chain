//! Focused hardware evidence for P0 DcapRequired onboarding. These steps stop
//! at durable key possession and node startup; validator-set activation belongs
//! to the independent lifecycle suite.

use cucumber::{then, when};
use std::{thread::sleep, time::Duration};

use crate::world::World;

const FULL_NODE_NAME: &str = "dcap-full-node";
const VALIDATOR_CATCHUP_LOG: &str = "local TEE lease guard armed at authenticated catch-up anchor";

fn wait_for_live_follower_log(
    world: &mut World,
    name: &str,
    index: usize,
    needle: &str,
    attempts: u32,
) -> bool {
    for _ in 0..attempts {
        if world.localnet.log_has(index, needle) {
            return true;
        }
        if !world.localnet.follower_running(name) {
            return false;
        }
        sleep(Duration::from_secs(1));
    }
    false
}

fn require_live_validator_progress(
    world: &mut World,
    index: usize,
    port: u16,
    primary: u16,
    baseline: u64,
    phase: &str,
) -> u64 {
    assert!(
        world.localnet.validator_running(index),
        "{phase} validator process exited immediately after launch"
    );
    let target = world
        .rpc
        .finalized(primary)
        .unwrap_or(baseline)
        .max(baseline.saturating_add(1));
    assert!(
        world.rpc.wait_finalized_at_least(primary, target, 60),
        "primary did not advance beyond the {phase} validator baseline"
    );
    assert!(
        world.rpc.wait_finalized_at_least(port, target, 60),
        "{phase} validator did not finalize a block after launch"
    );
    assert!(
        world.localnet.validator_running(index),
        "{phase} validator process exited while catching up"
    );
    let validator_hash = world
        .rpc
        .block_hash(port, target)
        .expect("validator finalized target block hash");
    let primary_hash = world
        .rpc
        .block_hash(primary, target)
        .expect("primary finalized target block hash");
    assert_eq!(
        validator_hash, primary_hash,
        "{phase} validator finalized a noncanonical block"
    );
    target
}

#[when("a production validator joins and restarts with its permanent offer key")]
fn validator_joins_and_restarts(world: &mut World) {
    let index = world.validators.joiner_index();
    let port = world.validators.http_port(index);
    world
        .localnet
        .provision_joiner(index)
        .expect("production validator DcapRequired join");
    let anchor = outbe_tee::load_finalized_join_admission_anchor(&world.validators.data_dir(index))
        .expect("load durable finalized join admission anchor")
        .expect("finalized join must persist its exact admission anchor");
    let anchor_hash = format!("{:#x}", anchor.finalized_hash);
    let primary = world.validators.primary_port();
    assert_eq!(
        world.rpc.block_hash(primary, anchor.finalized_height),
        Some(anchor_hash.clone()),
        "durable admission anchor is not canonical on the primary"
    );

    // A fresh validator datadir cannot start authority at height zero merely
    // because its enclave has a finalized admission. First catch the exact same
    // NodeHost/datadir up through a certified, non-authority follower path.
    let catchup_name = format!("validator-{index}");
    world
        .localnet
        .launch_dcap_full_node(&catchup_name, index, 0)
        .expect("launch admitted validator datadir as a certified follower");
    assert!(
        world
            .rpc
            .wait_finalized_at_least(port, anchor.finalized_height, 120),
        "certified follower did not reach its durable admission anchor"
    );
    assert_eq!(
        world.rpc.block_hash(port, anchor.finalized_height),
        Some(anchor_hash),
        "certified follower reached a noncanonical admission checkpoint"
    );
    assert!(
        wait_for_live_follower_log(world, &catchup_name, index, VALIDATOR_CATCHUP_LOG, 60,),
        "certified follower did not authenticate and arm the admission anchor"
    );
    let catchup_finalized = world
        .rpc
        .finalized(port)
        .expect("certified follower finalized height before validator launch");
    world
        .localnet
        .stop_follower(&catchup_name)
        .expect("stop certified follower before validator authority startup");

    world
        .localnet
        .launch_joiner(index, &[])
        .expect("launch joined production validator");
    let checkpoint =
        require_live_validator_progress(world, index, port, primary, catchup_finalized, "joined");
    world.state.joiner_offer_public_before_restart = Some(
        world
            .localnet
            .node_offer_public(index)
            .expect("authenticated validator offer key before restart"),
    );
    world.state.marker_height = Some(checkpoint);

    let restart_baseline = world
        .rpc
        .finalized(port)
        .expect("joined validator finalized height before restart");
    world.localnet.stop_joiner(index).expect("stop validator");
    world
        .localnet
        .restart_joiner_enclave(index)
        .expect("restart validator enclave from sealed state");
    world
        .localnet
        .launch_joiner(index, &[])
        .expect("restart joined production validator");
    require_live_validator_progress(world, index, port, primary, restart_baseline, "restarted");
}

#[then("the validator reopens the exact permanent key without re-registration")]
fn validator_reopens_exact_key(world: &mut World) {
    let index = world.validators.joiner_index();
    let expected = world
        .state
        .joiner_offer_public_before_restart
        .expect("validator pre-restart offer key");
    let actual = world
        .localnet
        .node_offer_public(index)
        .expect("authenticated validator offer key after restart");
    assert_eq!(
        actual, expected,
        "validator restart changed permanent offer key"
    );
    assert!(
        world
            .localnet
            .enclave_log_has(index, "unsealed offer key + group signature"),
        "validator enclave did not restore the permanent key from SGX sealed state"
    );
}

#[when("the validator DCAP sealed state is offered to an SGX no-attestation runtime")]
fn validator_state_is_offered_to_no_attest(world: &mut World) {
    let index = world.validators.joiner_index();
    world
        .localnet
        .stop_joiner(index)
        .expect("stop validator before downgrade attempt");
    world
        .localnet
        .attempt_no_attest_sealed_restart(index)
        .expect("run bounded no-attestation downgrade attempt");
}

#[then("the downgrade runtime cannot reopen or expose the permanent key")]
fn downgrade_cannot_reopen_key(world: &mut World) {
    let index = world.validators.joiner_index();
    world
        .localnet
        .assert_no_attest_restart_rejected(index)
        .expect("no-attestation runtime must fail closed");
    world
        .localnet
        .restart_joiner_enclave(index)
        .expect("restore canonical DCAP enclave after downgrade test");
    world
        .localnet
        .launch_joiner(index, &[])
        .expect("restart validator after downgrade test");
    let expected = world
        .state
        .joiner_offer_public_before_restart
        .expect("validator permanent key before downgrade test");
    assert_eq!(
        world
            .localnet
            .node_offer_public(index)
            .expect("canonical DCAP enclave key after downgrade test"),
        expected
    );
}

#[when("a production full node joins, starts, and restarts with its own enclave")]
fn full_node_joins_starts_and_restarts(world: &mut World) {
    let index = world.validators.joiner_index() + 1;
    world
        .localnet
        .provision_dcap_full_node(index)
        .expect("production FullNode DcapRequired join");
    let expected = world
        .localnet
        .node_offer_public(index)
        .expect("authenticated FullNode offer key after join");
    world.state.joiner_offer_public_before_restart = Some(expected);

    world
        .localnet
        .launch_dcap_full_node(FULL_NODE_NAME, index, 0)
        .expect("launch joined production FullNode");
    let checkpoint = world.rpc.head(world.validators.primary_port()).unwrap_or(1);
    assert!(
        world
            .rpc
            .wait_block(world.validators.http_port(index), checkpoint, 24)
            .is_some(),
        "production FullNode did not start and sync"
    );
    world
        .localnet
        .stop_follower(FULL_NODE_NAME)
        .expect("stop production FullNode");
    world
        .localnet
        .restart_full_node_enclave(index)
        .expect("restart FullNode enclave from sealed state");
    world
        .localnet
        .launch_dcap_full_node(FULL_NODE_NAME, index, 0)
        .expect("restart production FullNode");
    assert!(
        world
            .rpc
            .wait_block(world.validators.http_port(index), checkpoint, 24)
            .is_some(),
        "restarted FullNode did not resume sync"
    );
}

#[then("the full node reopens the exact permanent key before execution sync")]
fn full_node_reopens_exact_key(world: &mut World) {
    let index = world.validators.joiner_index() + 1;
    let expected = world
        .state
        .joiner_offer_public_before_restart
        .expect("FullNode pre-restart offer key");
    let actual = world
        .localnet
        .node_offer_public(index)
        .expect("authenticated FullNode offer key after restart");
    assert_eq!(
        actual, expected,
        "FullNode restart changed permanent offer key"
    );
    assert!(
        world.localnet.log_has(
            index,
            "full-node resident offer key matched upstream before execution launch"
        ),
        "FullNode startup did not gate execution on the exact upstream offer key"
    );
    assert!(
        world
            .localnet
            .enclave_log_has(index, "unsealed offer key + group signature"),
        "FullNode enclave did not restore the permanent key from SGX sealed state"
    );
}

#[when("finalized consensus time enters the renewal window")]
fn enter_renewal_window(world: &mut World) {
    let validator = world.validators.joiner_index();
    let full_node = validator + 1;
    let validator_deadline = world
        .localnet
        .node_renewal_status(validator)
        .expect("Validator finalized lease before renewal")
        .valid_until;
    let full_node_deadline = world
        .localnet
        .node_renewal_status(full_node)
        .expect("FullNode finalized lease before renewal")
        .valid_until;
    let renewal_timestamp = validator_deadline
        .max(full_node_deadline)
        .checked_sub(7 * 24 * 60 * 60)
        .and_then(|timestamp| timestamp.checked_add(2))
        .expect("manual renewal window timestamp");
    world
        .localnet
        .stop_joiner(validator)
        .expect("stop joining Validator while its committee peers restart");
    world
        .localnet
        .stop_follower(FULL_NODE_NAME)
        .expect("stop FullNode while its upstream committee restarts");
    world
        .localnet
        .restart_committee_at_consensus_timestamp(renewal_timestamp)
        .expect("restart complete committee at a controlled testnet timestamp");
    super::tee_lease::wait_for_finalized_timestamp(
        world,
        renewal_timestamp,
        "the DCAP onboarding manual renewal window",
    );
    world
        .localnet
        .launch_joiner(validator, &[])
        .expect("restart joining Validator after its committee peers are available");
    world
        .localnet
        .launch_dcap_full_node(FULL_NODE_NAME, full_node, 0)
        .expect("restart FullNode after its upstream committee is available");
}

#[then("manual renewal finalizes for the Validator and FullNode without changing their offer key")]
fn manual_renewal_finalizes_for_both_roles(world: &mut World) {
    let validator = world.validators.joiner_index();
    let full_node = validator + 1;
    let validator_offer = world
        .localnet
        .node_offer_public(validator)
        .expect("Validator offer key before renewal");
    let full_node_offer = world
        .localnet
        .node_offer_public(full_node)
        .expect("FullNode offer key before renewal");
    for index in [validator, full_node] {
        let observation = world
            .localnet
            .renew_node_enclave_until_finalized(index)
            .unwrap_or_else(|error| panic!("manually renew node {index}: {error:#}"));
        assert_eq!(
            observation.renewal_nonce, 1,
            "first finalized manual renewal must advance the exact nonce once"
        );
    }
    assert_eq!(
        world
            .localnet
            .node_offer_public(validator)
            .expect("Validator offer key after renewal"),
        validator_offer,
        "Validator renewal changed the permanent offer key"
    );
    assert_eq!(
        world
            .localnet
            .node_offer_public(full_node)
            .expect("FullNode offer key after renewal"),
        full_node_offer,
        "FullNode renewal changed the permanent offer key"
    );
}
