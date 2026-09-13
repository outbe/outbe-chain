#[cfg(test)]
use super::restart_boundary_transition;
use super::restart_inflight_round;
use super::restart_remaining_tries;
use super::restart_replacement_matches;
#[cfg(test)]
use super::restart_require_markers;
#[cfg(test)]
use super::restart_same_registered_identity;
#[cfg(test)]
use super::restart_signing_comparison;
#[cfg(test)]
use super::restart_target_commitment;
use super::restart_validate_frozen_target;
#[cfg(test)]
use super::restart_validate_membership;
use super::RestartFrozenRound;
use super::RestartFrozenTarget;
#[cfg(test)]
use super::RestartPublicState;

use crate::internal::launch_log::LaunchLog;

use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;

use std::io::Write as _;

#[test]
fn signing_requires_five_eligible_blocks_and_their_delayed_accounting() {
    let start = 100;
    let closed = start + 5 + LATE_FINALIZE_WINDOW_K;
    assert!(restart_signing_comparison(start, closed - 1, 1, 1, 1, 1).is_err());
    assert!(restart_signing_comparison(start, closed, 1, 1, 1, 1).unwrap());
    assert!(restart_signing_comparison(start, closed, 1, 1, 1, 2).is_err());
    assert!(restart_signing_comparison(start, closed, 1, 1, 1, 0).is_err());
    assert!(restart_signing_comparison(u64::MAX, u64::MAX, 1, 1, 0, 0).is_err());
}

#[test]
fn later_rotation_requires_a_new_closed_window_not_a_false_failure() {
    let end = 100 + 5 + LATE_FINALIZE_WINDOW_K;
    assert!(!restart_signing_comparison(100, end, 1, 2, 0, 1).unwrap());
    let drained = end + LATE_FINALIZE_WINDOW_K;
    assert!(
        restart_signing_comparison(drained, drained + 5 + LATE_FINALIZE_WINDOW_K, 2, 2, 1, 1)
            .unwrap()
    );
    assert!(restart_signing_comparison(100, end, 2, 1, 1, 1).is_err());
    assert!(restart_remaining_tries(std::time::Instant::now()).is_err());
}

#[test]
fn admission_requires_one_canonical_boundary_without_partial_activation() {
    assert!(!restart_boundary_transition(0, 0, None, false, false, true).unwrap());
    assert!(restart_boundary_transition(0, 0, None, false, true, true).is_err());
    assert!(restart_boundary_transition(0, 1, Some(1), false, true, true).unwrap());
    assert!(restart_boundary_transition(1, 1, Some(1), true, true, true).is_err());
    assert!(restart_boundary_transition(1, 2, Some(2), true, false, true).is_err());
    assert!(restart_boundary_transition(0, 2, Some(2), false, true, true).is_err());
    assert!(!restart_boundary_transition(1, 2, Some(2), true, true, true).unwrap());
}

#[test]
fn registered_admission_allows_an_intervening_rotation_but_frozen_target_does_not() {
    assert!(!restart_boundary_transition(0, 1, Some(1), false, false, false).unwrap());
    assert!(restart_boundary_transition(1, 2, Some(2), false, true, false).unwrap());
    assert!(restart_boundary_transition(0, 1, Some(1), false, false, true).is_err());
}

#[test]
fn membership_cannot_pass_with_missing_or_duplicate_fifth_peer() {
    let expected: Vec<_> = (1..=5)
        .map(alloy_primitives::Address::with_last_byte)
        .collect();
    let mut state = RestartPublicState {
        address: expected[4],
        consensus_public_key: alloy_primitives::Bytes::from(vec![1; 48]),
        p2p_version: 1,
        p2p_encoded: alloy_primitives::Bytes::from(vec![1]),
        epoch: 1,
        status: 2,
        stake: alloy_primitives::U256::from(1),
        active: expected.clone(),
        participants: expected.clone(),
        voter_misses: 0,
        supply: alloy_primitives::U256::from(1),
    };
    restart_validate_membership(&state, 2, &expected).unwrap();
    state.participants.pop();
    assert!(restart_validate_membership(&state, 2, &expected).is_err());
    state.participants = expected.clone();
    state.active[4] = expected[3];
    assert!(restart_validate_membership(&state, 2, &expected).is_err());
    assert!(restart_validate_membership(&state, 2, &expected[..4]).is_err());
    state.status = 0;
    state.active = expected[..4].to_vec();
    state.participants = state.active.clone();
    assert!(restart_validate_membership(&state, 0, &expected[..4]).is_err());
    state.stake = alloy_primitives::U256::ZERO;
    restart_validate_membership(&state, 0, &expected[..4]).unwrap();
}

#[test]
fn node_only_recovery_preserves_the_enclave_incarnation() {
    assert!(restart_replacement_matches((1, 2), (3, 2), false));
    assert!(!restart_replacement_matches((1, 2), (3, 4), false));
    assert!(!restart_replacement_matches((1, 2), (1, 2), false));
    assert!(restart_replacement_matches((1, 2), (3, 4), true));
    assert!(!restart_replacement_matches((1, 2), (3, 2), true));
    assert!(!restart_replacement_matches((1, 2), (1, 4), true));
}

#[test]
fn recovery_markers_must_come_from_the_replacement_not_old_or_future_logs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("public-markers.log");
    let good = "restoring durable DKG dealer transcript\nunsealed offer key + group signature\n";
    std::fs::write(&path, good).unwrap();
    let mut interval = LaunchLog::arm(&path).unwrap();
    let observed = interval.read().unwrap();
    assert!(restart_require_markers(&observed, &observed, true, true).is_err());
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(
        writer,
        "{good}freezing validator set and starting DKG rotation"
    )
    .unwrap();
    interval.seal().unwrap();
    writeln!(writer, "running DKG ceremony").unwrap();
    let observed = interval.read().unwrap();
    restart_require_markers(&observed, &observed, true, true).unwrap();
    assert!(!observed.contains("running DKG ceremony"));
}

#[test]
fn recovery_logs_reject_genesis_fallback_and_byzantine_evidence() {
    let enclave = "unsealed offer key + group signature";
    let dealer = "restoring durable DKG dealer transcript";
    restart_require_markers(dealer, enclave, true, true).unwrap();
    assert!(restart_require_markers(dealer, "", true, true).is_err());
    assert!(restart_require_markers("", enclave, true, true).is_err());
    assert!(restart_require_markers("running DKG ceremony", enclave, true, false).is_err());
    assert!(restart_require_markers("byzantine evidence observed", enclave, true, false).is_err());
}

#[test]
fn sealed_fault_rejects_completion_after_the_preliminary_check() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node.log");
    let mut log = LaunchLog::arm(&path).unwrap();
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(writer, "freezing validator set and starting DKG rotation dkg_cycle=7 freeze_height=80 planned_activation_height=180").unwrap();
    restart_inflight_round(&log.read().unwrap()).unwrap();
    // This arrives during the snapshot/stop interval, after the first check.
    writeln!(writer, "persisted completed DKG state before activation").unwrap();
    log.seal().unwrap();
    assert!(restart_inflight_round(&log.read().unwrap()).is_err());

    let mut replacement = LaunchLog::arm(&path).unwrap();
    writeln!(writer, "freezing validator set and starting DKG rotation dkg_cycle=8 freeze_height=200 planned_activation_height=300").unwrap();
    replacement.seal().unwrap();
    writeln!(writer, "persisted completed DKG state before activation").unwrap();
    // Completion from a later process cannot alter the sealed verdict.
    assert_eq!(
        restart_inflight_round(&replacement.read().unwrap())
            .unwrap()
            .cycle,
        8
    );
}

#[test]
fn frozen_round_requires_complete_unambiguous_current_fields() {
    let line = "freezing validator set and starting DKG rotation dkg_cycle=7 freeze_height=80 planned_activation_height=180";
    let expected = RestartFrozenRound {
        cycle: 7,
        freeze: 80,
        planned: 180,
    };
    assert_eq!(restart_inflight_round(line).unwrap(), expected);
    for bad in [
        String::new(),
        line.replace("dkg_cycle=7", ""),
        line.replace("freeze_height=80", "freeze_height=bad"),
        line.replace(
            "planned_activation_height=180",
            "planned_activation_height=79",
        ),
        format!("{line} dkg_cycle=8"),
        format!("{line}\n{}", line.replace("dkg_cycle=7", "dkg_cycle=8")),
        format!("{line}\nVRF/DKG material activated"),
    ] {
        assert!(restart_inflight_round(&bad).is_err());
    }
}

#[test]
fn recovered_boundary_requires_the_interrupted_round_and_ordered_commitment() {
    let round = RestartFrozenRound {
        cycle: 7,
        freeze: 80,
        planned: 180,
    };
    let members = vec![
        alloy_primitives::Address::with_last_byte(1),
        alloy_primitives::Address::with_last_byte(2),
    ];
    let commitment = restart_target_commitment(&round, &members);
    let encoded = hex::decode(concat!(
        "0000000000000050",
        "00000000000000b4",
        "0000000000000002",
        "0000000000000000000000000000000000000001",
        "0000000000000000000000000000000000000002",
    ))
    .unwrap();
    assert_eq!(commitment, alloy_primitives::keccak256(encoded));
    let target = RestartFrozenTarget {
        round: round.clone(),
        members: members.clone(),
        commitment,
    };
    restart_validate_frozen_target(&target, &round, commitment, &members, 193).unwrap();
    for field in 0..3 {
        let mut wrong = round.clone();
        match field {
            0 => wrong.cycle += 1,
            1 => wrong.freeze += 1,
            _ => wrong.planned += 1,
        }
        assert!(
            restart_validate_frozen_target(&target, &wrong, commitment, &members, 193).is_err()
        );
    }
    assert!(restart_validate_frozen_target(
        &target,
        &round,
        alloy_primitives::B256::ZERO,
        &members,
        193
    )
    .is_err());
    assert!(restart_validate_frozen_target(
        &target,
        &round,
        commitment,
        &[members[1], members[0]],
        193
    )
    .is_err());
    assert!(restart_validate_frozen_target(&target, &round, commitment, &members, 179).is_err());
}

#[test]
fn registered_restart_preserves_bls_and_p2p_identity_across_later_rotations() {
    let before = RestartPublicState {
        address: alloy_primitives::Address::with_last_byte(4),
        consensus_public_key: alloy_primitives::Bytes::from(vec![1; 48]),
        p2p_version: 1,
        p2p_encoded: alloy_primitives::Bytes::from(vec![0, 4, 127, 0, 0, 1, 1, 2]),
        epoch: 1,
        status: 0,
        stake: alloy_primitives::U256::ZERO,
        active: Vec::new(),
        participants: Vec::new(),
        voter_misses: 0,
        supply: alloy_primitives::U256::ONE,
    };
    let mut after = before.clone();
    after.epoch += 1;
    restart_same_registered_identity(&before, &after).unwrap();
    for field in 0..4 {
        let mut wrong = after.clone();
        match field {
            0 => wrong.address = alloy_primitives::Address::with_last_byte(5),
            1 => wrong.consensus_public_key = alloy_primitives::Bytes::from(vec![2; 48]),
            2 => wrong.p2p_version = 2,
            _ => wrong.p2p_encoded = alloy_primitives::Bytes::new(),
        }
        assert!(restart_same_registered_identity(&before, &wrong).is_err());
    }
}
