//! Case 9's halt-only proof. Production execution-finalization telemetry is
//! trusted only for the captured process interval and is bound to recomputed
//! canonical headers after that exact process exits. This is not independent
//! certificate authentication, a recovery proof, or post-expiry liveness.

use std::collections::BTreeMap;
use std::process::{ExitStatus, Output};

use outbe_primitives::OutbeHeader;

use super::*;

const COHORT: [usize; 3] = [0, 1, 2];
const PHASE: &str = "dkg_expiry_halt_only";
const FINALIZED: &str = "marshal-delivered block finalized and acked";
const FCU: &str = "forkchoice update returned valid status";
const EXPIRED: &str = "frozen DKG target missed VRF expiry: cycle ";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeaderWitness {
    height: u64,
    block_hash: B256,
    state_root: B256,
    parent_hash: B256,
}

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

struct Telemetry {
    finalized: BTreeMap<u64, B256>,
    forkchoice: BTreeMap<u64, B256>,
}

fn within(deadline: Instant) -> Result<()> {
    ensure!(
        Instant::now() < deadline,
        "240-second DKG expiry proof budget exhausted"
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
/// different from the expected production error exit; none can establish halt.
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

fn unique_field<'a>(line: &'a str, name: &str) -> Result<&'a str> {
    let prefix = format!("{name}=");
    let mut values = line
        .split_whitespace()
        .filter_map(|word| word.strip_prefix(&prefix));
    let value = values
        .next()
        .ok_or_else(|| eyre!("finalization record omitted {name}"))?;
    ensure!(
        values.next().is_none(),
        "duplicate finalization field {name}"
    );
    Ok(value)
}

fn number(value: &str) -> Result<u64> {
    ensure!(
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()),
        "invalid telemetry height"
    );
    value
        .parse()
        .map_err(|_| eyre!("telemetry height overflow"))
}

fn digest(value: &str) -> Result<B256> {
    ensure!(
        value.len() == 66 && value.starts_with("0x"),
        "invalid finalization digest shape"
    );
    value
        .parse()
        .map_err(|_| eyre!("invalid finalization digest"))
}

fn insert_finalized(
    map: &mut BTreeMap<u64, B256>,
    height: u64,
    hash: B256,
    expiry: u64,
) -> Result<()> {
    ensure!(
        height <= expiry,
        "production acknowledged finalization above expiry"
    );
    if let Some(old) = map.insert(height, hash) {
        ensure!(
            old == hash,
            "conflicting finalization digests at one height"
        );
    }
    Ok(())
}

fn telemetry(log: &str, target: &FrozenTarget, expiry: u64) -> Result<Telemetry> {
    ensure!(
        log.ends_with('\n') && !log.contains('\u{1b}'),
        "incomplete or colored expiry log interval"
    );
    let mut result = Telemetry {
        finalized: BTreeMap::new(),
        forkchoice: BTreeMap::new(),
    };
    let mut saw_expiry = false;
    for line in log.lines() {
        if line.contains(FINALIZED) || line.contains(FCU) {
            ensure!(
                line.contains(" INFO outbe_consensus::executor::actor: "),
                "wrong finalization record source"
            );
            let (height, hash, map) = if line.contains(FINALIZED) {
                (
                    number(unique_field(line, "height")?)?,
                    digest(unique_field(line, "digest")?)?,
                    &mut result.finalized,
                )
            } else {
                (
                    number(unique_field(line, "finalized_height")?)?,
                    digest(unique_field(line, "finalized_block_hash")?)?,
                    &mut result.forkchoice,
                )
            };
            insert_finalized(map, height, hash, expiry)?;
        }
        if let Some((_, tail)) = line.split_once(EXPIRED) {
            let (cycle, tail) = tail
                .split_once(", height ")
                .ok_or_else(|| eyre!("malformed expiry cycle"))?;
            let (height, deadline) = tail
                .split_once(", deadline ")
                .ok_or_else(|| eyre!("malformed expiry height"))?;
            ensure!(
                number(cycle)? == target.cycle
                    && number(height)? == expiry
                    && number(deadline)? == expiry,
                "wrong frozen cycle, terminal height or expiry deadline"
            );
            // The same error can also appear in the terminal eyre cause chain.
            // Validate every copy, but only the production tracing record is
            // positive evidence that this stack selected its expiry branch.
            saw_expiry |= line.contains(" ERROR outbe_chain: consensus stack failed ");
        }
    }
    ensure!(saw_expiry, "missing exact frozen-target stack error");
    ensure!(
        result.finalized.contains_key(&expiry),
        "missing execution-finalization record at exact expiry"
    );
    for (height, hash) in &result.finalized {
        if let Some(fcu_hash) = result.forkchoice.get(height) {
            ensure!(
                hash == fcu_hash,
                "forkchoice and acknowledgement digests disagree"
            );
        }
    }
    Ok(result)
}

/// The CLI prints tracing lines, followed by the two labelled JSON values.
/// A zero exit code alone is insufficient: missing static rows can still exit
/// zero, and storage-consistency warnings cannot be treated as valid evidence.
fn header_output(output: Output, expected_height: u64) -> Result<HeaderWitness> {
    use eyre::WrapErr as _;

    header_output_inner(&output, expected_height)
        .and_then(|header| header_witness(&header))
        .wrap_err_with(|| {
            format!(
            "canonical header h{expected_height} rejected: exit={}; stdout_hex={}; stderr_hex={}",
            output.status,
            hex::encode(&output.stdout),
            hex::encode(&output.stderr)
        )
        })
}

fn header_output_inner(output: &Output, expected_height: u64) -> Result<OutbeHeader> {
    ensure!(
        output.status.success(),
        "read-only header command failed: {}",
        output.status
    );
    let stdout =
        std::str::from_utf8(&output.stdout).map_err(|_| eyre!("header stdout is not UTF-8"))?;
    let stderr =
        std::str::from_utf8(&output.stderr).map_err(|_| eyre!("header stderr is not UTF-8"))?;
    let (prefix, body) = if let Some(body) = stdout.strip_prefix("Header\n") {
        ("", body)
    } else {
        stdout
            .split_once("\nHeader\n")
            .ok_or_else(|| eyre!("missing canonical Header output"))?
    };
    for line in prefix
        .lines()
        .chain(stderr.lines())
        .filter(|line| !line.trim().is_empty())
    {
        ensure!(
            line.contains(" INFO ")
                && !line.contains(" WARN ")
                && !line.contains(" ERROR ")
                && !line.contains("Inconsistent storage")
                && !line.contains('\u{1b}'),
            "unexpected read-only header command diagnostic"
        );
    }
    let (header, hash) = body
        .split_once("\nBlockHash\n")
        .ok_or_else(|| eyre!("missing canonical BlockHash output"))?;
    let header: OutbeHeader =
        serde_json::from_str(header).map_err(|_| eyre!("invalid canonical header JSON"))?;
    let hash: B256 =
        serde_json::from_str(hash).map_err(|_| eyre!("invalid canonical block hash JSON"))?;
    ensure!(
        header.inner.number == expected_height,
        "canonical header row has wrong height"
    );
    ensure!(
        header.inner.hash_slow() == hash,
        "canonical header hash does not recompute"
    );
    Ok(header)
}

fn header_witness(header: &OutbeHeader) -> Result<HeaderWitness> {
    let artifacts = decode_outbe_block_artifacts(header.inner.extra_data.as_ref())
        .map_err(|_| eyre!("invalid canonical header artifact envelope"))?;
    ensure!(
        !matches!(
            artifacts.consensus_header_artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(_))
        ),
        "DKG boundary activated during the frozen-target halt interval"
    );
    Ok(HeaderWitness {
        height: header.inner.number,
        block_hash: header.inner.hash_slow(),
        state_root: header.inner.state_root,
        parent_hash: header.inner.parent_hash,
    })
}

fn check_chain(
    anchor: &HeaderWitness,
    headers: &[HeaderWitness],
    expiry: u64,
    records: &Telemetry,
) -> Result<()> {
    let expected_len = expiry
        .checked_sub(anchor.height)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| eyre!("invalid terminal header interval"))?;
    ensure!(
        u64::try_from(headers.len())? == expected_len && headers.first() == Some(anchor),
        "missing canonical rows or changed live checkpoint"
    );
    for pair in headers.windows(2) {
        ensure!(
            pair[0].height.checked_add(1) == Some(pair[1].height)
                && pair[1].parent_hash == pair[0].block_hash,
            "canonical header gap or parent-link mismatch"
        );
    }
    for header in headers {
        ensure!(
            records.finalized.get(&header.height) == Some(&header.block_hash),
            "canonical row is not bound to this process's finalization record"
        );
        if let Some(hash) = records.forkchoice.get(&header.height) {
            ensure!(
                *hash == header.block_hash,
                "canonical row disagrees with successful forkchoice"
            );
        }
    }
    Ok(())
}

pub(super) fn observe(world: &mut World) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(240);
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
    let members = dkg_addresses(world, 5)?;
    let ports = dkg_ports(world, &COHORT)?;
    let mut last_error = String::from("frozen target not yet observed");
    let (target, expiry, anchor) = loop {
        within(deadline).map_err(|error| eyre!("{error}; last live observation: {last_error}"))?;
        ensure!(
            !owners(world)?,
            "all survivors exited before the required live pinned checkpoint"
        );
        match observed_frozen_target(world) {
            Ok(target) => {
                let expiry = expiry_height(&target, grace, before.height)?;
                let minimum = target.freeze.max(
                    before
                        .height
                        .checked_add(2)
                        .ok_or_else(|| eyre!("fresh height overflow"))?,
                );
                match world.rpc.wait_finalized_checkpoint(&ports, minimum, 1) {
                    Ok(point) => {
                        within(deadline)?;
                        ensure!(
                            point.height <= expiry,
                            "live finalized checkpoint exceeds expiry"
                        );
                        finalize_dkg_receipts(world, &ports)?;
                        dkg_membership_at(world, &ports, point, &members[..4], members[4], 1)?;
                        let frozen = world.rpc.checkpoint_at(ports[0], target.freeze)?;
                        dkg_membership_at(world, &ports, frozen, &members[..4], members[4], 1)?;
                        record_dkg_checkpoint(world, "dkg_expiry_live", &ports, point);
                        // Anchor the durable scan at freeze, not at the last polled
                        // height: otherwise an unobserved activation tail could hide.
                        break (target, expiry, frozen);
                    }
                    Err(error) => last_error = error.to_string(),
                }
            }
            Err(error) => last_error = error.to_string(),
        }
        sleep(Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())));
    };

    while !owners(world)? {
        within(deadline).map_err(|error| eyre!("{error}; last RPC diagnostic: {last_error}"))?;
        // Each expected port stays in scope. An RPC failure is diagnostic, not
        // a negative membership observation or evidence that the node halted.
        for (&index, &port) in COHORT.iter().zip(&ports) {
            let expected_pid = world.state.lifecycle_incarnations[&index].node_pid;
            let (pid, status) = world.localnet.owned_validator_process(index)?;
            if checked_exit(expected_pid, pid, status)? {
                continue; // Typed exit, never a responsive-peer filter.
            }
            match world.rpc.finalized_result(port) {
                Ok(height) => ensure!(height <= expiry, "live survivor finalized above expiry"),
                Err(error) => last_error = error.to_string(),
            }
        }
        sleep(Duration::from_millis(250).min(deadline.saturating_duration_since(Instant::now())));
    }
    within(deadline).map_err(|error| eyre!("{error}; last RPC diagnostic: {last_error}"))?;
    let mut peers = Vec::new();
    let mut common_anchor = None;
    for index in COHORT {
        let owner = world
            .state
            .lifecycle_incarnations
            .get_mut(&index)
            .ok_or_else(|| eyre!("missing expiry log owner"))?;
        owner.node_log.seal()?;
        let log = owner.node_log.read()?;
        let records = telemetry(&log, &target, expiry)?;
        ensure!(
            frozen_target(&log, dkg_ready_height(world)?)? == Some(target.clone()),
            "sealed frozen target changed"
        );
        let owner = &world.state.lifecycle_incarnations[&index];
        let (node_pid, enclave_pid, log_start) = (
            owner.node_pid,
            owner.enclave_pid,
            owner.node_log.start_offset(),
        );
        let mut headers = Vec::new();
        for height in anchor.height..=expiry {
            within(deadline)?;
            let output = world
                .localnet
                .stopped_validator_header_output(index, node_pid, height, deadline)?;
            let header = header_output(output, height).map_err(|error| {
                eyre!("validator-{index} owned PID {node_pid} canonical header read failed: {error:#}")
            })?;
            if height == anchor.height {
                ensure!(
                    header.block_hash == anchor.block_hash
                        && header.state_root == anchor.state_root,
                    "durable row changed from live finalized checkpoint"
                );
                if let Some(expected) = &common_anchor {
                    ensure!(*expected == header, "survivor canonical anchors differ");
                } else {
                    common_anchor = Some(header.clone());
                }
            }
            headers.push(header);
        }
        check_chain(
            common_anchor
                .as_ref()
                .ok_or_else(|| eyre!("missing canonical anchor"))?,
            &headers,
            expiry,
            &records,
        )?;
        peers.push(PeerWitness {
            index,
            node_pid,
            enclave_pid,
            exit_code: 1,
            log_start,
            log_bytes: log.len(),
            headers,
        });
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    use outbe_primitives::reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts};

    use super::*;

    fn target() -> FrozenTarget {
        FrozenTarget {
            cycle: 1,
            freeze: 40,
            planned: 60,
        }
    }

    fn raw_headers() -> Vec<OutbeHeader> {
        let mut parent = B256::repeat_byte(7);
        (40..=66)
            .map(|height| {
                let mut header = OutbeHeader::default();
                header.inner.number = height;
                header.inner.parent_hash = parent;
                header.inner.state_root = B256::repeat_byte(height as u8);
                header.inner.extra_data =
                    encode_outbe_block_artifacts(&OutbeBlockArtifacts::default()).unwrap();
                parent = header.inner.hash_slow();
                header
            })
            .collect()
    }

    fn output(header: &OutbeHeader) -> Output {
        Output {
            status: ExitStatus::from_raw(0),
            stdout: format!("2026-09-05T12:00:00Z  INFO reth_cli::db: opened read-only database\nHeader\n{}\n\nBlockHash\n{}\n",
                serde_json::to_string_pretty(header).unwrap(), serde_json::to_string(&header.inner.hash_slow()).unwrap()).into_bytes(),
            stderr: Vec::new(),
        }
    }

    fn log(headers: &[HeaderWitness]) -> String {
        let mut log = String::new();
        for header in headers {
            log.push_str(&format!("2026-09-05T12:00:00Z  INFO outbe_consensus::executor::actor: {FCU} finalized_height={} finalized_block_hash={}\n",
                header.height, header.block_hash));
            log.push_str(&format!("2026-09-05T12:00:00Z  INFO outbe_consensus::executor::actor: {FINALIZED} height={} digest={}\n",
                header.height, header.block_hash));
        }
        log.push_str(&format!("2026-09-05T12:00:01Z ERROR outbe_chain: consensus stack failed e={EXPIRED}1, height 66, deadline 66\n"));
        log
    }

    fn witness() -> HaltWitness {
        let headers: Vec<_> = raw_headers()
            .iter()
            .map(|header| header_witness(header).unwrap())
            .collect();
        HaltWitness {
            target: target(),
            grace: 6,
            expiry: 66,
            before_height: 3,
            anchor: headers[0].clone(),
            peers: COHORT
                .into_iter()
                .map(|index| PeerWitness {
                    index,
                    node_pid: 100 + index as u32,
                    enclave_pid: 200 + index as u32,
                    exit_code: 1,
                    log_start: 10,
                    log_bytes: log(&headers).len(),
                    headers: headers.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn three_owned_survivors_prove_exact_ceiling_with_real_header_hashes() {
        let proof = witness();
        for peer in &proof.peers {
            let records = telemetry(&log(&peer.headers), &proof.target, proof.expiry).unwrap();
            check_chain(&proof.anchor, &peer.headers, proof.expiry, &records).unwrap();
        }
        for header in raw_headers() {
            assert_eq!(
                header_output(output(&header), header.inner.number).unwrap(),
                header_witness(&header).unwrap()
            );
        }
        let rows = [json!({"phase": PHASE, "proof": proof})];
        retained_proof(&rows).unwrap();
    }

    #[test]
    fn terminal_log_rejects_wrong_schedule_incomplete_records_and_wrong_source() {
        let proof = witness();
        let good = log(&proof.peers[0].headers);
        for bad in [
            good.replace("cycle 1,", "cycle 2,"),
            good.replace("height 66, deadline 66", "height 65, deadline 66"),
            good.replace("height 66, deadline 66", "height 67, deadline 66"),
            good.replace("deadline 66", "deadline 67"),
            good.replace("height=66 ", "height=oops "),
            good.replace("height=66 ", "height=66 height=66 "),
            good.replace("outbe_consensus::executor::actor:", "another_actor:"),
            good.replace(" ERROR outbe_chain:", " ERROR another_process:"),
            good.replace("deadline 66", "deadline 18446744073709551616"),
            good.trim_end().to_owned(),
            good.replace("digest=0x", "digest=xx"),
        ] {
            assert!(telemetry(&bad, &proof.target, proof.expiry).is_err());
        }
        let missing: String = good
            .lines()
            .filter(|line| !(line.contains(FINALIZED) && line.contains("height=66 ")))
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(telemetry(&missing, &proof.target, proof.expiry).is_err());
    }

    #[test]
    fn propagated_identical_errors_are_not_new_authority_or_conflicting_success() {
        let proof = witness();
        let good = log(&proof.peers[0].headers);
        let propagated = format!("{good}   1: {EXPIRED}1, height 66, deadline 66\n");
        telemetry(&propagated, &proof.target, proof.expiry).unwrap();
        assert!(telemetry(
            &format!("{good}   1: {EXPIRED}2, height 66, deadline 66\n"),
            &proof.target,
            proof.expiry
        )
        .is_err());
        assert!(telemetry(
            &format!("   1: {EXPIRED}1, height 66, deadline 66\n"),
            &proof.target,
            proof.expiry
        )
        .is_err());
    }

    #[test]
    fn higher_successful_forkchoice_and_conflicting_acknowledgements_fail() {
        let proof = witness();
        let good = log(&proof.peers[0].headers);
        for extra in [
            format!(
                "{FCU} finalized_height=67 finalized_block_hash={}",
                B256::ZERO
            ),
            format!("{FINALIZED} height=67 digest={}", B256::ZERO),
            format!("{FINALIZED} height=66 digest={}", B256::ZERO),
            format!(
                "{FCU} finalized_height=66 finalized_block_hash={}",
                B256::ZERO
            ),
        ] {
            assert!(telemetry(
                &format!(
                    "{good}2026-09-05T12:00:01Z  INFO outbe_consensus::executor::actor: {extra}\n"
                ),
                &proof.target,
                proof.expiry
            )
            .is_err());
        }
    }

    #[test]
    fn canonical_cli_requires_complete_successful_unambiguous_output() {
        let header = raw_headers().remove(0);
        let mut failures = Vec::new();
        let mut bad = output(&header);
        bad.status = ExitStatus::from_raw(256);
        failures.push(bad);
        let mut bad = output(&header);
        bad.stdout = vec![0xff];
        failures.push(bad);
        let mut bad = output(&header);
        bad.stderr = vec![0xff];
        failures.push(bad);
        let mut bad = output(&header);
        bad.stdout.extend_from_slice(b"\ntrailing output");
        failures.push(bad);
        let mut bad = output(&header);
        bad.stdout.extend_from_slice(b"\nHeader\n{}");
        failures.push(bad);
        let mut bad = output(&header);
        bad.stderr = b"2026-09-05 WARN Inconsistent storage. Restart node to heal.\n".to_vec();
        failures.push(bad);
        let mut bad = output(&header);
        bad.stdout = b"2026-09-05 ERROR missing header\n".to_vec();
        failures.push(bad);
        let mut bad = output(&header);
        bad.stdout = format!(
            "Header\n{}\n\nBlockHash\n\"{}\"\n",
            serde_json::to_string(&header).unwrap(),
            B256::ZERO
        )
        .into_bytes();
        failures.push(bad);
        for bad in failures {
            assert!(header_output(bad, header.inner.number).is_err());
        }
        assert!(header_output(output(&header), 41).is_err());
    }

    #[test]
    fn failed_header_decode_retains_lossless_command_diagnostics() {
        for raw_status in [0, 256] {
            let output = Output {
                status: ExitStatus::from_raw(raw_status),
                stdout: b"incomplete header\xff".to_vec(),
                stderr: b"storage failure\xfe".to_vec(),
            };
            let status = output.status.to_string();
            let stdout = hex::encode(&output.stdout);
            let stderr = hex::encode(&output.stderr);
            let error = header_output(output, 42).unwrap_err().to_string();
            assert!(error.contains("h42"));
            assert!(error.contains(&format!("exit={status}")));
            assert!(error.contains(&format!("stdout_hex={stdout}")));
            assert!(error.contains(&format!("stderr_hex={stderr}")));
        }
        let mut malformed = raw_headers().remove(0);
        malformed.inner.extra_data = b"OART\xff".to_vec().into();
        let output = output(&malformed);
        let stdout = hex::encode(&output.stdout);
        let error = header_output(output, malformed.inner.number).unwrap_err();
        assert!(error.to_string().contains(&format!("stdout_hex={stdout}")));
        assert!(format!("{error:#}").contains("invalid canonical header artifact envelope"));
    }

    #[test]
    fn canonical_chain_rejects_missing_unacknowledged_or_reparented_rows() {
        let proof = witness();
        let headers = &proof.peers[0].headers;
        let records = telemetry(&log(headers), &proof.target, proof.expiry).unwrap();
        let mut gap = headers.clone();
        gap.remove(1);
        let mut wrong_parent = headers.clone();
        wrong_parent[1].parent_hash = B256::ZERO;
        let mut wrong_number = headers.clone();
        wrong_number[1].height += 1;
        let mut wrong_hash = headers.clone();
        wrong_hash[1].block_hash = B256::ZERO;
        let mut wrong_root = headers.clone();
        wrong_root[0].state_root = B256::ZERO;
        for bad in [gap, wrong_parent, wrong_number, wrong_hash, wrong_root] {
            assert!(check_chain(&proof.anchor, &bad, proof.expiry, &records).is_err());
        }
        let mut missing = records;
        missing.finalized.remove(&41);
        assert!(check_chain(&proof.anchor, headers, proof.expiry, &missing).is_err());
    }

    #[test]
    fn canonical_artifact_decoder_is_not_replaced_by_an_absence_check() {
        let mut header = raw_headers().remove(0);
        header.inner.extra_data = b"OART\xff".to_vec().into();
        assert!(header_witness(&header).is_err());
        // A boundary record with invalid payload must fail decoding too; it
        // cannot turn into a successfully decoded absence of activation.
        header.inner.extra_data = b"OART\x0b\x02\x00\x00".to_vec().into();
        assert!(header_witness(&header).is_err());
    }

    #[test]
    fn a_valid_boundary_outcome_cannot_pass_as_an_unactivated_terminal_header() {
        use outbe_primitives::consensus::ReshareResult;
        use outbe_primitives::reshare_artifact::tee_expired_target_exclusions_hash;

        let boundary = DkgBoundaryArtifact {
            epoch: 1,
            dkg_cycle: 1,
            freeze_height: 40,
            planned_activation_height: 60,
            target_set_hash: B256::repeat_byte(1),
            vrf_material_version: 1,
            vrf_group_public_key: alloy_primitives::keccak256([]),
            vrf_group_public_key_bytes: Default::default(),
            committee_set_hash: B256::repeat_byte(2),
            is_validator_set_change: true,
            outcome: Default::default(),
            is_full_dkg: false,
            reshare: ReshareResult {
                new_active_set: Vec::new(),
                active_set_hash: B256::ZERO,
            },
            tee_recipient_pubkeys: Vec::new(),
            tee_expired_target_exclusions: Vec::new(),
            tee_expired_target_exclusions_hash: tee_expired_target_exclusions_hash(&[]).unwrap(),
        };
        let mut header = raw_headers().remove(0);
        header.inner.extra_data = encode_outbe_block_artifacts(&OutbeBlockArtifacts {
            consensus_header_artifact: Some(ConsensusHeaderArtifact::BoundaryOutcome(boundary)),
            ..Default::default()
        })
        .unwrap();
        assert!(decode_outbe_block_artifacts(header.inner.extra_data.as_ref()).is_ok());
        assert!(header_witness(&header).is_err());
    }

    #[test]
    fn only_the_exact_owned_natural_error_exit_satisfies_halt() {
        assert!(!checked_exit(100, 100, None).unwrap());
        assert!(checked_exit(100, 100, Some(ExitStatus::from_raw(256))).unwrap());
        for (expected, actual, raw) in [
            (100, 101, 256),
            (0, 0, 256),
            (100, 100, 0),
            (100, 100, 9),
            (100, 100, 512),
        ] {
            assert!(checked_exit(expected, actual, Some(ExitStatus::from_raw(raw))).is_err());
        }
    }

    #[test]
    fn incomplete_or_ambiguous_retained_proofs_cannot_pass_the_second_step() {
        let proof = witness();
        assert!(retained_proof(&[]).is_err());
        assert!(retained_proof(&[json!({"phase": PHASE, "proof": {}})]).is_err());
        let row = json!({"phase": PHASE, "proof": proof});
        assert!(retained_proof(&[row.clone(), row]).is_err());
        for path in [
            "missing_peer",
            "wrong_peer",
            "wrong_hash",
            "wrong_root",
            "missing_tail",
            "wrong_exit",
            "wrong_cycle",
            "wrong_expiry",
        ] {
            let mut bad = witness();
            match path {
                "missing_peer" => {
                    bad.peers.pop();
                }
                "wrong_peer" => bad.peers[2].index = 4,
                "wrong_hash" => bad.peers[2].headers[1].block_hash = B256::ZERO,
                "wrong_root" => bad.peers[2].headers[1].state_root = B256::ZERO,
                "missing_tail" => {
                    bad.peers[2].headers.pop();
                }
                "wrong_exit" => bad.peers[2].exit_code = 0,
                "wrong_cycle" => bad.target.freeze += 1,
                "wrong_expiry" => bad.expiry += 1,
                _ => unreachable!(),
            }
            assert!(validate_witness(&bad).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn schedule_arithmetic_preserves_progress_without_relaxing_the_ceiling() {
        assert_eq!(expiry_height(&target(), 6, 3).unwrap(), 66);
        assert!(expiry_height(&target(), 0, 3).is_err());
        assert!(expiry_height(&target(), 6, 60).is_err());
        assert!(expiry_height(&target(), u64::MAX, 3).is_err());
        assert!(expiry_height(&target(), 6, u64::MAX).is_err());
        assert!(within(Instant::now() - Duration::from_secs(1)).is_err());
    }
}
