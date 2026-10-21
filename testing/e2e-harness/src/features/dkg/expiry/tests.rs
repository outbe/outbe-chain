use std::os::unix::process::ExitStatusExt;
use std::process::Output;

use outbe_primitives::reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts};
use outbe_primitives::OutbeHeader;

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
        log.push_str(&format!("2026-09-05T12:00:00Z  INFO outbe_consensus::executor::actor::finalization: {FCU} finalized_height={} finalized_block_hash={}\n",
            header.height, header.block_hash));
        log.push_str(&format!("2026-09-05T12:00:00Z  INFO outbe_consensus::executor::actor::finalization: {FINALIZED} height={} digest={}\n",
            header.height, header.block_hash));
    }
    log.push_str(&format!("2026-09-05T12:00:01Z ERROR outbe_chain::launch::node::consensus: consensus stack failed e={EXPIRED}1, height 66, deadline 66\n"));
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
        good.replace(
            "outbe_consensus::executor::actor::finalization:",
            "another_actor:",
        ),
        good.replace(
            " ERROR outbe_chain::launch::node:",
            " ERROR another_process:",
        ),
        good.replace(
            "outbe_consensus::executor::actor::finalization:",
            "outbe_consensus::executor::actor:",
        ),
        good.replace(" ERROR outbe_chain::launch::node:", " ERROR outbe_chain:"),
        good.replace(" INFO ", " DEBUG "),
        good.replace(" ERROR ", " WARN "),
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
fn terminal_log_accepts_current_production_finalization_and_expiry_records() {
    let captured = "2026-09-13T19:38:27.057012Z  INFO outbe_consensus::executor::actor::finalization: marshal-delivered block finalized and acked height=66 digest=0x82b8a3fa749f55a368d7ce6a2c2ad2b73bcd1110304f1d59a20ded5de6e2dc00\n\
        2026-09-13T19:39:49.404497Z ERROR outbe_chain::launch::node::consensus: consensus stack failed e=frozen DKG target missed VRF expiry: cycle 1, height 66, deadline 66\n";
    let records = telemetry(captured, &target(), 66).unwrap();
    assert_eq!(records.finalized.len(), 1);
    assert_eq!(
        records.finalized[&66],
        "0x82b8a3fa749f55a368d7ce6a2c2ad2b73bcd1110304f1d59a20ded5de6e2dc00"
            .parse::<B256>()
            .unwrap()
    );
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
                "{good}2026-09-05T12:00:01Z  INFO outbe_consensus::executor::actor::finalization: {extra}\n"
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
    // A boundary record with invalid payload must fail decoding too. It
    // cannot turn into a successfully decoded absence of activation.
    header.inner.extra_data = b"OART\x01\x00\x01\x02\x00\x00".to_vec().into();
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
    // A tag/height or generic error record is not a completed halt proof.
    assert!(retained_proof(&[json!({
        "phase": PHASE,
        "proof": {"expiry": 66, "exit_code": 1, "error": "consensus failed"}
    })])
    .is_err());
    let row = json!({"phase": PHASE, "proof": proof});
    assert!(retained_proof(&[row.clone(), row]).is_err());
    for path in [
        "missing_peer",
        "wrong_peer",
        "wrong_hash",
        "wrong_root",
        "missing_tail",
        "wrong_exit",
        "non_one_exit",
        "missing_node_pid",
        "missing_enclave_pid",
        "empty_log",
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
            "non_one_exit" => bad.peers[2].exit_code = 2,
            "missing_node_pid" => bad.peers[2].node_pid = 0,
            "missing_enclave_pid" => bad.peers[2].enclave_pid = 0,
            "empty_log" => bad.peers[2].log_bytes = 0,
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
