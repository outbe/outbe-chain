//! Case 9's halt-only proof. Production execution-finalization telemetry is
//! trusted only for the captured process interval and is bound to recomputed
//! canonical headers after that exact process exits. This is not independent
//! certificate authentication, a recovery proof, or post-expiry liveness.

use std::collections::BTreeMap;
use std::process::ExitStatus;

use super::frozen::observed_frozen_target;
use super::*;

mod headers;
mod telemetry;

#[cfg(test)]
use headers::header_witness;
use headers::{check_chain, header_output, HeaderWitness};
use telemetry::{telemetry, Telemetry};

const COHORT: [usize; 3] = [0, 1, 2];
const PHASE: &str = "dkg_expiry_halt_only";
const FINALIZED: &str = "marshal-delivered block finalized and acked";
const FCU: &str = "forkchoice update returned valid status";
const EXPIRED: &str = "frozen DKG target missed VRF expiry: cycle ";
// Cover pre-window progress with a founder offline, reaching expiry, natural
// process exits, and the durable header scan within one finite budget. The SGX
// run reached only height 52 of expiry 66 in 240 seconds due to missed proposers.
const PROOF_BUDGET: Duration = Duration::from_secs(1200);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerWitness {
    index: usize,
    node_pid: u32,
    enclave_pid: u32,
    exit_code: i32,
    log_start: u64,
    log_bytes: usize,
    headers: Vec<HeaderWitness>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HaltWitness {
    target: FrozenTarget,
    grace: u64,
    expiry: u64,
    before_height: u64,
    anchor: HeaderWitness,
    peers: Vec<PeerWitness>,
}

fn within(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "{}-second DKG expiry proof budget exhausted",
        PROOF_BUDGET.as_secs()
    );
    Ok(())
}

fn expiry_height(target: &FrozenTarget, grace: u64, before: u64) -> Result<u64> {
    ensure!(
        grace > 0 && target.freeze <= target.planned,
        "invalid executed expiry schedule"
    );
    let expiry = target
        .planned
        .checked_add(grace)
        .ok_or_else(|| eyre!("expiry height overflow"))?;
    ensure!(
        expiry
            > before
                .checked_add(6)
                .ok_or_else(|| eyre!("progress height overflow"))?,
        "expiry does not preserve the pre-fault +7 progress claim"
    );
    Ok(expiry)
}

/// Missing handles, replacement PIDs, signalled exits and successful exits are
/// different from the expected production error exit. None can establish halt.
fn checked_exit(expected: u32, actual: u32, status: Option<ExitStatus>) -> Result<bool> {
    ensure!(
        expected != 0 && expected == actual,
        "expiry process identity changed"
    );
    match status {
        None => Ok(false),
        Some(status) => {
            ensure!(
                status.code() == Some(1),
                "expiry requires a natural error exit, got {status}"
            );
            Ok(true)
        }
    }
}

fn owners(world: &mut World) -> Result<bool> {
    validate_dkg_owners(&COHORT, &world.localnet.owned_validator_indices())?;
    ensure!(
        world
            .state
            .lifecycle_incarnations
            .keys()
            .copied()
            .collect::<Vec<_>>()
            == [0, 1, 2, 3, 4],
        "expiry fixture requires exactly the five captured incarnations"
    );
    let mut all_exited = true;
    for index in 0..5 {
        let owner = &world.state.lifecycle_incarnations[&index];
        let (expected_node, expected_enclave) = (owner.node_pid, owner.enclave_pid);
        ensure!(
            world.localnet.live_enclave_pid(index)? == expected_enclave,
            "validator-{index} enclave changed during expiry fault"
        );
        if COHORT.contains(&index) {
            let (pid, status) = world.localnet.owned_validator_process(index)?;
            all_exited &= checked_exit(expected_node, pid, status)?;
        }
    }
    Ok(all_exited)
}

struct ExpiryProof {
    deadline: Instant,
    before_height: u64,
    grace: u64,
    members: Vec<Address>,
    ports: Vec<u16>,
}

struct HaltScan<'a> {
    deadline: Instant,
    target: &'a FrozenTarget,
    expiry: u64,
    anchor: FinalizedCheckpoint,
}

/// One live poll; `Ok(None)` keeps polling and records why in `last_error`.
fn poll_live_anchor(
    world: &mut World,
    proof: &ExpiryProof,
    last_error: &mut String,
) -> Result<Option<(FrozenTarget, u64, FinalizedCheckpoint)>> {
    let target = match observed_frozen_target(world) {
        Ok(target) => target,
        Err(error) => {
            *last_error = error.to_string();
            return Ok(None);
        }
    };
    let expiry = expiry_height(&target, proof.grace, proof.before_height)?;
    let minimum = target.freeze.max(
        proof
            .before_height
            .checked_add(2)
            .ok_or_else(|| eyre!("fresh height overflow"))?,
    );
    let point = match world
        .rpc
        .wait_finalized_checkpoint(&proof.ports, minimum, 1)
    {
        Ok(point) => point,
        Err(error) => {
            *last_error = error.to_string();
            return Ok(None);
        }
    };
    within(proof.deadline)?;
    ensure!(
        point.height <= expiry,
        "live finalized checkpoint exceeds expiry"
    );
    finalize_dkg_receipts(world, &proof.ports)?;
    let membership = DkgMembership {
        members: &proof.members[..4],
        joiner: proof.members[4],
        status: 1,
    };
    dkg_membership_at(world, &proof.ports, point, &membership)?;
    let frozen = world.rpc.checkpoint_at(proof.ports[0], target.freeze)?;
    dkg_membership_at(world, &proof.ports, frozen, &membership)?;
    record_dkg_checkpoint(world, "dkg_expiry_live", &proof.ports, point);
    // Anchor the durable scan at freeze, not at the last polled
    // height: otherwise an unobserved activation tail could hide.
    Ok(Some((target, expiry, frozen)))
}

fn wait_live_anchor(
    world: &mut World,
    proof: &ExpiryProof,
) -> Result<(FrozenTarget, u64, FinalizedCheckpoint)> {
    let mut last_error = String::from("frozen target not yet observed");
    loop {
        within(proof.deadline)
            .map_err(|error| eyre!("{error}; last live observation: {last_error}"))?;
        ensure!(
            !owners(world)?,
            "all survivors exited before the required live pinned checkpoint"
        );
        if let Some(found) = poll_live_anchor(world, proof, &mut last_error)? {
            return Ok(found);
        }
        sleep(
            Duration::from_millis(250)
                .min(proof.deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn survivor_observations(world: &mut World, ports: &[u16], expiry: u64) -> Result<Vec<String>> {
    let mut observations = Vec::new();
    // Each expected port stays in scope. An RPC failure is diagnostic, not
    // a negative membership observation or evidence that the node halted.
    for (&index, &port) in COHORT.iter().zip(ports) {
        let expected_pid = world.state.lifecycle_incarnations[&index].node_pid;
        let (pid, status) = world.localnet.owned_validator_process(index)?;
        if checked_exit(expected_pid, pid, status)? {
            observations.push(format!("validator-{index} exited with code 1"));
            continue; // Typed exit, never a responsive-peer filter.
        }
        match world.rpc.finalized_result(port) {
            Ok(height) => {
                ensure!(height <= expiry, "live survivor finalized above expiry");
                observations.push(format!(
                    "validator-{index} live: finalized={height}, expiry={expiry}"
                ));
            }
            Err(error) => observations.push(format!("validator-{index} live: RPC {error}")),
        }
    }
    Ok(observations)
}

fn wait_natural_exits(
    world: &mut World,
    proof: &ExpiryProof,
    target: &FrozenTarget,
    expiry: u64,
) -> Result<()> {
    let mut last_error = format!(
        "target acquired: cycle={}, freeze={}, expiry={expiry}; waiting for natural exits",
        target.cycle, target.freeze
    );
    while !owners(world)? {
        within(proof.deadline)
            .map_err(|error| eyre!("{error}; last expiry observation: {last_error}"))?;
        last_error = survivor_observations(world, &proof.ports, expiry)?.join("; ");
        sleep(
            Duration::from_millis(250)
                .min(proof.deadline.saturating_duration_since(Instant::now())),
        );
    }
    within(proof.deadline).map_err(|error| eyre!("{error}; last expiry observation: {last_error}"))
}

fn bind_anchor(
    anchor: &FinalizedCheckpoint,
    header: &HeaderWitness,
    common_anchor: &mut Option<HeaderWitness>,
) -> Result<()> {
    ensure!(
        header.block_hash == anchor.block_hash && header.state_root == anchor.state_root,
        "durable row changed from live finalized checkpoint"
    );
    if let Some(expected) = common_anchor {
        ensure!(*expected == *header, "survivor canonical anchors differ");
    } else {
        *common_anchor = Some(header.clone());
    }
    Ok(())
}

fn durable_headers(
    world: &mut World,
    scan: &HaltScan<'_>,
    (index, node_pid): (usize, u32),
    common_anchor: &mut Option<HeaderWitness>,
) -> Result<Vec<HeaderWitness>> {
    let mut headers = Vec::new();
    for height in scan.anchor.height..=scan.expiry {
        within(scan.deadline)?;
        let output = world.localnet.stopped_validator_header_output(
            index,
            node_pid,
            height,
            scan.deadline,
        )?;
        let header = header_output(output, height).map_err(|error| {
            eyre!("validator-{index} owned PID {node_pid} canonical header read failed: {error:#}")
        })?;
        if height == scan.anchor.height {
            bind_anchor(&scan.anchor, &header, common_anchor)?;
        }
        headers.push(header);
    }
    Ok(headers)
}

fn sealed_peer(
    world: &mut World,
    scan: &HaltScan<'_>,
    index: usize,
    common_anchor: &mut Option<HeaderWitness>,
) -> Result<PeerWitness> {
    let owner = world
        .state
        .lifecycle_incarnations
        .get_mut(&index)
        .ok_or_else(|| eyre!("missing expiry log owner"))?;
    owner.node_log.seal()?;
    let log = owner.node_log.read()?;
    let records = telemetry(&log, scan.target, scan.expiry)?;
    ensure!(
        frozen_target(&log, dkg_ready_height(world)?)? == Some(scan.target.clone()),
        "sealed frozen target changed"
    );
    let owner = &world.state.lifecycle_incarnations[&index];
    let (node_pid, enclave_pid, log_start) = (
        owner.node_pid,
        owner.enclave_pid,
        owner.node_log.start_offset(),
    );
    let headers = durable_headers(world, scan, (index, node_pid), common_anchor)?;
    check_chain(
        common_anchor
            .as_ref()
            .ok_or_else(|| eyre!("missing canonical anchor"))?,
        &headers,
        scan.expiry,
        &records,
    )?;
    Ok(PeerWitness {
        index,
        node_pid,
        enclave_pid,
        exit_code: 1,
        log_start,
        log_bytes: log.len(),
        headers,
    })
}

pub(super) fn observe(world: &mut World) -> Result<()> {
    let deadline = Instant::now() + PROOF_BUDGET;
    ensure!(
        !world
            .state
            .restart_observations
            .iter()
            .any(|row| row["phase"] == PHASE),
        "expiry proof already recorded"
    );
    let before = world
        .state
        .lifecycle_before
        .ok_or_else(|| eyre!("missing pre-fault canonical checkpoint"))?;
    ensure!(
        world.state.marker_height == Some(before.height),
        "pre-fault height witnesses disagree"
    );
    let genesis: serde_json::Value = serde_json::from_slice(&std::fs::read(
        world.localnet.scenario_dir().join("genesis.json"),
    )?)?;
    let grace = genesis
        .pointer("/config/dkgActivationGraceBlocks")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| eyre!("executed genesis omitted activation grace"))?;
    let proof = ExpiryProof {
        deadline,
        before_height: before.height,
        grace,
        members: dkg_addresses(world, 5)?,
        ports: dkg_ports(world, &COHORT)?,
    };
    let (target, expiry, anchor) = wait_live_anchor(world, &proof)?;
    wait_natural_exits(world, &proof, &target, expiry)?;
    let scan = HaltScan {
        deadline,
        target: &target,
        expiry,
        anchor,
    };
    let mut peers = Vec::new();
    let mut common_anchor = None;
    for index in COHORT {
        peers.push(sealed_peer(world, &scan, index, &mut common_anchor)?);
    }
    let witness = HaltWitness {
        target,
        grace,
        expiry,
        before_height: before.height,
        anchor: common_anchor.ok_or_else(|| eyre!("missing durable anchor"))?,
        peers,
    };
    validate_witness(&witness)?;
    within(deadline)?;
    ensure!(owners(world)?, "terminal owned exit proof changed");
    world.state.vrf_expiry_height = Some(expiry);
    world
        .state
        .restart_observations
        .push(json!({"phase": PHASE, "proof": witness}));
    Ok(())
}

fn validate_witness(witness: &HaltWitness) -> Result<()> {
    ensure!(
        expiry_height(&witness.target, witness.grace, witness.before_height)? == witness.expiry
            && witness.anchor.height == witness.target.freeze,
        "retained expiry schedule changed"
    );
    ensure!(
        witness
            .peers
            .iter()
            .map(|peer| peer.index)
            .collect::<Vec<_>>()
            == COHORT,
        "retained expiry cohort is incomplete"
    );
    let first = &witness.peers[0].headers;
    for peer in &witness.peers {
        ensure!(
            peer.node_pid != 0
                && peer.enclave_pid != 0
                && peer.exit_code == 1
                && peer.log_bytes > 0,
            "retained expiry owner evidence is incomplete"
        );
        ensure!(
            peer.headers == *first,
            "survivors disagree on canonical terminal hash/root chain"
        );
        let records = Telemetry {
            finalized: peer
                .headers
                .iter()
                .map(|header| (header.height, header.block_hash))
                .collect(),
            forkchoice: BTreeMap::new(),
        };
        check_chain(&witness.anchor, &peer.headers, witness.expiry, &records)?;
    }
    Ok(())
}

fn retained_proof(observations: &[serde_json::Value]) -> Result<HaltWitness> {
    let mut rows = observations.iter().filter(|row| row["phase"] == PHASE);
    let row = rows
        .next()
        .ok_or_else(|| eyre!("missing completed halt-only proof"))?;
    ensure!(rows.next().is_none(), "ambiguous retained halt proof");
    let witness: HaltWitness = serde_json::from_value(row["proof"].clone())
        .map_err(|_| eyre!("malformed retained halt proof"))?;
    validate_witness(&witness)?;
    Ok(witness)
}

pub(super) fn assert_retained(world: &mut World) -> Result<()> {
    // A failed revalidation must never leave an earlier cleanup allowance.
    world.state.expected_dkg_expiry_exits.clear();
    let witness = retained_proof(&world.state.restart_observations)?;
    let ready_height = dkg_ready_height(world)?;
    ensure!(
        world.state.vrf_expiry_height == Some(witness.expiry),
        "retained expiry height mismatch"
    );
    ensure!(
        owners(world)?,
        "a survivor did not retain its exact owned exit"
    );
    for peer in &witness.peers {
        let owner = world
            .state
            .lifecycle_incarnations
            .get_mut(&peer.index)
            .ok_or_else(|| eyre!("missing retained expiry owner"))?;
        ensure!(
            owner.node_pid == peer.node_pid
                && owner.enclave_pid == peer.enclave_pid
                && owner.node_log.start_offset() == peer.log_start,
            "retained expiry incarnation changed"
        );
        let log = owner.node_log.read()?;
        ensure!(
            log.len() == peer.log_bytes,
            "sealed expiry interval changed"
        );
        ensure!(
            frozen_target(&log, ready_height)? == Some(witness.target.clone()),
            "retained frozen target no longer matches the sealed process interval"
        );
        check_chain(
            &witness.anchor,
            &peer.headers,
            witness.expiry,
            &telemetry(&log, &witness.target, witness.expiry)?,
        )?;
    }
    world.state.expected_dkg_expiry_exits = witness
        .peers
        .iter()
        .map(|peer| crate::world::state::DkgExpiryExpectedExit {
            slot: peer.index,
            node_pid: peer.node_pid,
        })
        .collect();
    Ok(())
}

#[cfg(test)]
mod tests;
