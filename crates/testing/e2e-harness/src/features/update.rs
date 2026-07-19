//! Steps for `features/update_operator.feature` — a step-for-step port of
//! The update-operator feature. Each step drives the
//! `World` handles only; no `cast`/`cli` strings appear here.

use std::thread::sleep;
use std::time::Duration;

use alloy_primitives::U256;
use cucumber::{then, when};

use crate::internal::addresses::UPDATE_ADDR;
use crate::world::World;

/// Blocks past the vote window before an update may activate
/// (`MIN_ACTIVATION_BUFFER`, update_operator_flow.sh:30).
const MIN_ACTIVATION_BUFFER: u64 = 0;

/// Encoded `v3.0` (`u8 major << 24 | u24 minor`). Localnet/testnet activation
/// ceiling is `v2.3`; this is strictly greater and must Fatal at activation.
const UNSUPPORTED_PROTOCOL_VERSION: u64 = 3u64 << 24;

/// Propose a bump to the next protocol version from an operator
/// (update_operator_flow.sh:234-251).
#[when(expr = "operator {string} proposes an update to the next protocol version")]
fn propose_update(world: &mut World, name: String) {
    let active = world.rpc.active_version().expect("read active version");
    propose_update_version(world, &name, active + 1, "e2e update operator smoke");
}

/// Propose a protocol version above the testnet/devnet activation ceiling
/// (`version > 2.3`). Scheduling is allowed; activation is Fatal.
#[when(expr = "operator {string} proposes an update to an unsupported protocol version")]
fn propose_unsupported_update(world: &mut World, name: String) {
    let active = world.rpc.active_version().expect("read active version");
    assert!(
        UNSUPPORTED_PROTOCOL_VERSION > active,
        "unsupported version must be above active ({active})"
    );
    propose_update_version(
        world,
        &name,
        UNSUPPORTED_PROTOCOL_VERSION,
        "e2e unsupported protocol version",
    );
}

fn propose_update_version(world: &mut World, name: &str, version: u64, info: &str) {
    let operator = world.validators.operator(name).expect("resolve operator");
    let port = world.validators.primary_port();
    let head = world.rpc.head(port).expect("read head");
    let activation = head + world.state.voting_window + MIN_ACTIVATION_BUFFER + 30;
    // VoteTarget JSON expects `"major.minor"`; on-chain ABI still uses packed u32.
    let version_str = format!("{}.{}", version >> 24, version & 0x00FF_FFFF);

    let payload = serde_json::json!({
        "version": version_str,
        "activationHeight": activation,
        "info": info,
    })
    .to_string();

    let tx = world
        .rpc
        .send_propose(&operator, &format!("{UPDATE_ADDR:#x}"), &payload)
        .expect("send propose");
    assert!(world.rpc.wait_tx(&tx, 40), "propose tx not mined: {tx}");

    world.state.proposed_version = Some(version);
    world.state.activation_height = Some(activation);
}

/// The proposal is visible, pending, targets the update module, and carries the
/// activation height (update_operator_flow.sh:253-266).
#[then(
    expr = "proposal {int} is pending, targets the update module, and carries the activation height"
)]
fn proposal_pending(world: &mut World, id: u64) {
    let mut vs = world.rpc.vote_status(id);
    for _ in 0..10 {
        if vs.visible {
            break;
        }
        sleep(Duration::from_secs(3));
        vs = world.rpc.vote_status(id);
    }
    assert!(vs.visible, "proposal #{id} not visible after propose");
    assert_eq!(vs.status, "pending", "proposal should be pending");
    let update_addr = format!("{UPDATE_ADDR:#x}");
    assert!(
        vs.target.eq_ignore_ascii_case(&update_addr),
        "target {} != update module {update_addr}",
        vs.target
    );
    let activation = world
        .state
        .activation_height
        .expect("activation set by propose");
    assert!(
        vs.payload
            .contains(&format!("\"activationHeight\":{activation}")),
        "payload missing activation height {activation}: {}",
        vs.payload
    );
}

/// Cast yes votes from a comma-separated list of validators
/// (update_operator_flow.sh:268-285).
///
/// Every ballot must be mined successfully. Merely getting a transaction hash
/// is not evidence that the vote was accepted.
#[when(expr = "validators {string} cast yes votes")]
fn cast_yes_votes(world: &mut World, names: String) {
    let id = world.state.proposal_id;
    for name in names.split(',') {
        let name = name.trim();
        let validator = world.validators.by_name(name).expect("resolve validator");
        let tx = world
            .rpc
            .cast_vote(&validator, id, true)
            .expect("send vote");
        assert!(
            world.rpc.wait_successful_receipt(&tx, 40),
            "vote receipt for {name} was not successful ({tx})",
        );
    }
}

#[when(expr = "validator {string} repeats the yes vote on proposal {int}")]
fn repeat_yes_vote(world: &mut World, name: String, id: u64) {
    let validator = world.validators.by_name(&name).expect("resolve validator");
    let error = world
        .rpc
        .cast_vote_rejection(&validator, id, true)
        .expect("duplicate vote must be rejected during RPC preflight");
    assert!(
        error.contains("validator has already voted on proposal"),
        "unexpected duplicate-vote rejection: {error}"
    );
}

#[then(
    expr = "the duplicate vote reverts and proposal {int} still has {int} yes votes on every validator"
)]
fn duplicate_vote_preserves_tally(world: &mut World, id: u64, yes: u64) {
    let status = world.rpc.vote_status(id);
    assert_eq!(
        status.status, "pending",
        "duplicate changed proposal status"
    );
    assert_eq!(status.yes, yes, "duplicate changed yes tally");
    assert_eq!(status.no, 0, "duplicate changed no tally");
    proposal_parity(world, id);
}

#[when(expr = "validator {string} restarts during the voting window")]
fn restart_during_voting(world: &mut World, name: String) {
    let validator = world.validators.by_name(&name).expect("resolve validator");
    let index = validator.index;
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
            .wait_block(world.validators.http_port(index), before, 60)
            .is_some(),
        "{name} did not recover its chain state"
    );
}

fn validator_ports(world: &World) -> Vec<u16> {
    let mut ports = vec![world.validators.primary_port()];
    ports.extend(world.validators.peer_ports());
    ports
}

#[then(expr = "proposal {int} and its votes are identical on every validator")]
fn proposal_parity(world: &mut World, id: u64) {
    let expected = world.rpc.vote_status(id);
    assert!(
        expected.visible,
        "proposal #{id} must be visible on primary"
    );
    for port in validator_ports(world) {
        let actual = world.rpc.vote_status_on(port, id);
        assert_eq!(
            actual.status, expected.status,
            "proposal status on RPC {port}"
        );
        assert_eq!(actual.yes, expected.yes, "yes tally on RPC {port}");
        assert_eq!(actual.no, expected.no, "no tally on RPC {port}");
        assert_eq!(actual.deadline, expected.deadline, "deadline on RPC {port}");
        assert_eq!(actual.target, expected.target, "target on RPC {port}");
        assert_eq!(actual.payload, expected.payload, "payload on RPC {port}");
    }
}

fn restart_entire_committee(world: &mut World, context: &str) {
    let before = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("finalized height before committee restart");
    world
        .localnet
        .restart_committee_and_enclaves()
        .expect(context);
    for port in validator_ports(world) {
        let recovered = world.rpc.wait_block(port, before, 90);
        assert!(
            recovered.is_some_and(|height| height >= before),
            "RPC {port} did not recover to finalized height {before} after {context}: {recovered:?}"
        );
    }
}

#[when("the entire committee restarts after update scheduling")]
fn restart_after_scheduling(world: &mut World) {
    restart_entire_committee(world, "restart after update scheduling");
}

#[when("the entire committee restarts at the activation boundary")]
fn restart_at_activation_boundary(world: &mut World) {
    restart_entire_committee(world, "restart at activation boundary");
}

#[then("the approved proposal and waiting schedule are identical on every validator")]
fn approved_schedule_parity(world: &mut World) {
    proposal_parity(world, world.state.proposal_id);
    let expected = world
        .rpc
        .scheduled_update(world.state.proposal_id)
        .expect("scheduled update on primary");
    assert_eq!(expected.status, 0, "schedule must still be waiting");
    for port in validator_ports(world) {
        assert_eq!(
            world.rpc.scheduled_update_on(port, world.state.proposal_id),
            Some(expected.clone()),
            "scheduled update on RPC {port}"
        );
    }
}

#[then("the activated update state is identical on every validator")]
fn activated_update_parity(world: &mut World) {
    let want = world.state.proposed_version.expect("proposed version");
    for port in validator_ports(world) {
        assert_eq!(
            world.rpc.active_version_on(port),
            Some(want),
            "active version on RPC {port}"
        );
        let scheduled = world
            .rpc
            .scheduled_update_on(port, world.state.proposal_id)
            .unwrap_or_else(|| panic!("scheduled update missing on RPC {port}"));
        assert_eq!(scheduled.status, 1, "schedule status on RPC {port}");
    }
}

#[then("the update activation converges on every validator after the boundary restart")]
fn activation_converges_after_boundary_restart(world: &mut World) {
    let want = world.state.proposed_version.expect("proposed version");
    let id = world.state.proposal_id;
    let ports = validator_ports(world);
    let mut converged = false;
    for _ in 0..60 {
        converged = ports.iter().all(|port| {
            world.rpc.active_version_on(*port) == Some(want)
                && world
                    .rpc
                    .scheduled_update_on(*port, id)
                    .is_some_and(|scheduled| scheduled.status == 1)
        });
        if converged {
            break;
        }
        sleep(Duration::from_secs(3));
    }
    assert!(
        converged,
        "committee did not converge to activated version {want} after boundary restart"
    );
    activated_update_parity(world);
    proposal_parity(world, id);
}

#[then(expr = "proposal {int} is expired without an update schedule on every validator")]
fn expired_without_schedule(world: &mut World, id: u64) {
    assert!(
        world.rpc.wait_vote_status(id, "expired", 60),
        "proposal #{id} did not expire"
    );
    proposal_parity(world, id);
    for port in validator_ports(world) {
        assert!(world.rpc.head(port).is_some(), "RPC {port} is unavailable");
        assert!(
            world.rpc.active_version_on(port).is_some(),
            "update read API is unavailable on RPC {port}"
        );
        assert!(
            world.rpc.scheduled_update_on(port, id).is_none(),
            "expired proposal unexpectedly created a schedule on RPC {port}"
        );
    }
}

#[then("the active protocol version remains baseline on every validator")]
fn baseline_version_on_all_validators(world: &mut World) {
    for port in validator_ports(world) {
        assert_eq!(
            world.rpc.active_version_on(port),
            Some(0),
            "expired proposal mutated active version on RPC {port}"
        );
    }
}

#[then("the committee continues producing finalized blocks")]
fn committee_continues_finalizing(world: &mut World) {
    let ports = validator_ports(world);
    let before = world
        .rpc
        .finalized(ports[0])
        .expect("finalized height before liveness check");
    for port in ports {
        let head = world
            .rpc
            .wait_block(port, before.saturating_add(2), 60)
            .unwrap_or_else(|| panic!("RPC {port} did not advance"));
        let finalized = world
            .rpc
            .finalized(port)
            .unwrap_or_else(|| panic!("RPC {port} finalized height unavailable"));
        assert!(head >= before + 2, "RPC {port} head did not advance");
        assert!(finalized >= before, "RPC {port} finalized height regressed");
    }
}

/// Still pending before the deadline, with the expected yes tally
/// (update_operator_flow.sh:287-294).
#[then(expr = "proposal {int} is still pending with {int} yes votes")]
fn still_pending_with_votes(world: &mut World, id: u64, yes: u64) {
    // The just-fired votes may still be settling; poll until the tally is in,
    // bounded so we stay inside the voting window (bail out the moment the
    // proposal leaves `pending`).
    let mut vs = world.rpc.vote_status(id);
    for _ in 0..8 {
        if vs.yes >= yes || vs.status != "pending" {
            break;
        }
        sleep(Duration::from_secs(2));
        vs = world.rpc.vote_status(id);
    }
    assert_eq!(vs.status, "pending", "proposal should still be pending");
    assert_eq!(vs.yes, yes, "yes tally");
    assert_eq!(vs.no, 0, "no tally");
    let deadline = vs
        .deadline
        .expect("pending proposal should expose deadline");
    let port = world.validators.primary_port();
    let head = world
        .rpc
        .head(port)
        .expect("primary head should be readable");
    assert!(
        head <= deadline,
        "proposal #{id} reported pending after its voting window: head={head}, deadline={deadline}"
    );
    world.state.vote_deadline = vs.deadline;
}

/// Advance past the voting deadline (update_operator_flow.sh:296-298).
#[when("the committee passes the vote deadline")]
fn pass_vote_deadline(world: &mut World) {
    let deadline = world.state.vote_deadline.expect("deadline captured");
    let port = world.validators.primary_port();
    let h = world.rpc.wait_block_gt(port, deadline, 80).unwrap_or(0);
    assert!(
        h > deadline,
        "did not pass vote deadline {deadline} (got {h})"
    );
}

/// Approved after the deadline tally, with the scheduled update matching the
/// proposal (update_operator_flow.sh:299-311).
#[then(expr = "proposal {int} is approved and the scheduled update matches the proposal")]
fn approved_and_scheduled(world: &mut World, id: u64) {
    assert!(
        world.rpc.wait_vote_status(id, "approved", 60),
        "proposal not approved after deadline"
    );
    let su = world.rpc.scheduled_update(id).expect("scheduled update");
    assert_eq!(
        su.version,
        world.state.proposed_version.expect("version"),
        "scheduled version"
    );
    assert_eq!(
        su.activation,
        world.state.activation_height.expect("activation"),
        "scheduled activation height"
    );
    assert_eq!(
        su.status, 0,
        "scheduled update should be waiting for activation"
    );
}

/// Advance past the activation height (update_operator_flow.sh:313-315).
#[when("the committee passes the activation height")]
fn pass_activation_height(world: &mut World) {
    let activation = world.state.activation_height.expect("activation captured");
    let port = world.validators.primary_port();
    let h = world.rpc.wait_block_gt(port, activation, 180).unwrap_or(0);
    assert!(
        h > activation,
        "did not pass activation {activation} (got {h})"
    );
}

/// Wait until head is at least `activation - 1` so the next block would be the
/// activation height (where an unsupported version Fatals).
#[when("the committee approaches the activation height")]
fn approach_activation_height(world: &mut World) {
    let activation = world.state.activation_height.expect("activation captured");
    let target = activation.saturating_sub(1);
    let port = world.validators.primary_port();
    let h = world.rpc.wait_block(port, target, 180).unwrap_or(0);
    assert!(
        h >= target,
        "did not approach activation {activation} (want head>={target}, got {h})"
    );
}

/// Activation-height block must not commit when the scheduled version exceeds
/// the binary `PROTOCOL_VERSION` (Fatal aborts pre-exec hooks).
#[then("the committee does not advance past the activation height")]
fn does_not_pass_activation(world: &mut World) {
    let activation = world.state.activation_height.expect("activation captured");
    let port = world.validators.primary_port();
    // Give the committee time to attempt (and fail) the activation block.
    for _ in 0..20 {
        if let Some(h) = world.rpc.head(port) {
            assert!(
                h < activation,
                "committee advanced to {h} past activation {activation}; unsupported version should Fatal"
            );
        }
        sleep(Duration::from_secs(1));
    }
    let head = world.rpc.head(port).unwrap_or(0);
    assert!(
        head < activation,
        "expected stall below activation {activation}, head={head}"
    );
}

/// Active version must stay at the pre-proposal value (unsupported never activates).
#[then("the active protocol version is unchanged")]
fn active_version_unchanged(world: &mut World) {
    let proposed = world.state.proposed_version.expect("version");
    let got = world.rpc.active_version().expect("read active version");
    assert_ne!(
        got, proposed,
        "unsupported version {proposed} must not become active"
    );
    assert_eq!(
        got, 0,
        "fresh localnet active version should remain 0, got {got}"
    );
}

/// Scheduled row stays waiting — activation never committed.
#[then("the scheduled update is still waiting for activation")]
fn scheduled_still_waiting(world: &mut World) {
    let su = world
        .rpc
        .scheduled_update(world.state.proposal_id)
        .expect("scheduled update");
    assert_eq!(
        su.status, 0,
        "scheduled update should still be waiting for activation"
    );
}

/// Node log should surface the Fatal activation guard message.
#[then(expr = "validator {string} logs report the unsupported activation as fatal")]
fn log_reports_unsupported_fatal(world: &mut World, name: String) {
    let validator = world.validators.by_name(&name).expect("resolve validator");
    let idx = validator.index;
    let mut found = false;
    for _ in 0..15 {
        if world
            .localnet
            .log_has(idx, "cannot activate protocol version")
            || world.localnet.log_has(idx, "binary supports at most")
        {
            found = true;
            break;
        }
        sleep(Duration::from_secs(2));
    }
    assert!(
        found,
        "validator-{idx} log missing unsupported-activation Fatal message"
    );
}

#[when("the entire committee restarts after the unsupported activation failure")]
fn restart_after_unsupported_activation(world: &mut World) {
    restart_entire_committee(world, "restart after unsupported activation failure");
}

#[then("every validator RPC recovers below the unsupported activation height")]
fn unsupported_rpc_recovery(world: &mut World) {
    let activation = world.state.activation_height.expect("activation height");
    for port in validator_ports(world) {
        let head = world
            .rpc
            .head(port)
            .unwrap_or_else(|| panic!("RPC {port} did not recover"));
        assert!(
            head < activation,
            "RPC {port} advanced to {head} at/past unsupported activation {activation}"
        );
    }
}

#[then("the unsupported proposal and waiting schedule are identical on every validator")]
fn unsupported_state_parity(world: &mut World) {
    approved_schedule_parity(world);
    let proposed = world.state.proposed_version.expect("unsupported version");
    for port in validator_ports(world) {
        assert_ne!(
            world.rpc.active_version_on(port),
            Some(proposed),
            "unsupported version became active on RPC {port}"
        );
    }
}

#[then("the committee remains stalled below the unsupported activation height")]
fn unsupported_still_stalled(world: &mut World) {
    does_not_pass_activation(world);
}

/// Active protocol version updated to the proposed version
/// (update_operator_flow.sh:316-317).
#[then("the active protocol version equals the proposed version")]
fn active_version_bumped(world: &mut World) {
    let want = world.state.proposed_version.expect("version");
    let got = world.rpc.wait_active_version(want, 60);
    assert_eq!(got, Some(want), "active protocol version not updated");
}

/// The scheduled update is marked activated (update_operator_flow.sh:318-319).
#[then("the scheduled update is marked activated")]
fn scheduled_marked_activated(world: &mut World) {
    let su = world
        .rpc
        .scheduled_update(world.state.proposal_id)
        .expect("scheduled update");
    assert_eq!(su.status, 1, "scheduled update should be activated");
}

/// Fire `IVote.listProposals` with an oversized U256 index against a specific
/// validator RPC. Before the saturating-conversion fix this panicked inside
/// `clamp_page`; after the fix the call must remain non-fatal.
#[when(expr = "validator {string} receives listProposals with index 2^256-1 and count {int}")]
fn list_proposals_oversized(world: &mut World, name: String, count: u64) {
    let validator = world.validators.by_name(&name).expect("resolve validator");
    let port = world.validators.http_port(validator.index);
    // Intentionally ignore the Option: a panic mid-call surfaces as transport
    // failure / None. Survival is asserted by the following Then steps.
    let _ = world
        .rpc
        .list_proposals_on(port, U256::MAX, U256::from(count));
    // Give a crashed node a moment to exit before the survival check.
    sleep(Duration::from_secs(2));
}

/// The targeted validator process is still alive after the oversized call.
#[then(expr = "validator {string} node process is still running")]
fn validator_still_running(world: &mut World, name: String) {
    let validator = world.validators.by_name(&name).expect("resolve validator");
    assert!(
        !world.localnet.validator_exited(validator.index),
        "validator-{} exited after oversized listProposals (log panic={})",
        validator.index,
        world.localnet.log_has(validator.index, "panicked"),
    );
    let port = world.validators.http_port(validator.index);
    assert!(
        world.rpc.head(port).is_some(),
        "validator-{} RPC became unreachable after oversized listProposals",
        validator.index,
    );
}

/// Oversized index saturates and yields an empty page (index past the end).
#[then(expr = "listProposals with index 2^256-1 and count {int} on {string} returns an empty page")]
fn list_proposals_empty_page(world: &mut World, count: u64, name: String) {
    let validator = world.validators.by_name(&name).expect("resolve validator");
    let port = world.validators.http_port(validator.index);
    let page = world
        .rpc
        .list_proposals_on(port, U256::MAX, U256::from(count))
        .expect("listProposals eth_call should succeed after saturating conversion");
    assert!(
        page.is_empty(),
        "oversized index should return an empty page, got {page:?}"
    );
}
