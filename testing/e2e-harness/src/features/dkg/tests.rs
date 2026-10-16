use super::recovery::{
    recovered_checkpoint_attempts, validate_dkg_activation, validate_reveal_interval,
};
use super::*;

#[test]
fn recovery_checkpoint_readiness_uses_only_the_remaining_budget() {
    assert_eq!(
        recovered_checkpoint_attempts(Duration::from_secs(3)).unwrap(),
        1
    );
    assert_eq!(
        recovered_checkpoint_attempts(Duration::from_secs(29)).unwrap(),
        9
    );
    assert_eq!(
        recovered_checkpoint_attempts(Duration::from_secs(270)).unwrap(),
        90
    );
    assert!(recovered_checkpoint_attempts(Duration::ZERO).is_err());
    assert!(recovered_checkpoint_attempts(Duration::from_millis(2999)).is_err());
}

#[test]
fn dkg_fault_cohorts_do_not_restore_the_offline_joiner_or_omit_a_survivor() {
    validate_dkg_owners(&[0, 1, 2], &[0, 1, 2]).unwrap();
    validate_dkg_owners(&[0, 1, 2, 3], &[0, 1, 2, 3]).unwrap();
    for actual in [
        vec![0, 1],
        vec![0, 1, 2, 3],
        vec![0, 1, 2, 4],
        vec![0, 1, 2, 2],
    ] {
        assert!(validate_dkg_owners(&[0, 1, 2], &actual).is_err());
    }
    assert!(validate_dkg_owners(&[0, 1, 2, 3], &[0, 1, 2, 3, 4]).is_err());
    assert!(validate_dkg_owners(&[], &[]).is_err());
}

#[test]
fn frozen_target_observation_requires_exact_public_fields() {
    let line = "INFO freezing validator set and starting DKG rotation dkg_cycle=7 current_height=80 freeze_height=80 planned_activation_height=180";
    let expected = FrozenTarget {
        cycle: 7,
        freeze: 80,
        planned: 180,
    };
    assert_eq!(frozen_target(line, 0).unwrap(), Some(expected));
    assert_eq!(frozen_target(line, 81).unwrap(), None);
    assert_eq!(frozen_target("unrelated DKG progress", 0).unwrap(), None);
    for wrong in [
        line.replace("dkg_cycle=7", "dkg_cycle=bad"),
        line.replace("freeze_height=80", ""),
        line.replace(
            "planned_activation_height=180",
            "planned_activation_height=79",
        ),
        format!("{line}\n{}", line.replace("dkg_cycle=7", "dkg_cycle=8")),
    ] {
        assert!(frozen_target(&wrong, 0).is_err());
    }
}

#[test]
fn recovery_binds_actual_boundary_to_the_same_frozen_target() {
    let target = FrozenTarget {
        cycle: 7,
        freeze: 80,
        planned: 180,
    };
    let members: Vec<_> = (1..=5).map(Address::repeat_byte).collect();
    let boundary = BoundaryWitness {
        height: 193,
        block_hash: B256::repeat_byte(2),
        state_root: B256::repeat_byte(3),
        epoch: 4,
        cycle: 7,
        freeze: 80,
        planned: 180,
        target_hash: B256::repeat_byte(1),
        members: members.clone(),
    };
    validate_dkg_activation(&boundary, &target, &members, 3, false).unwrap();
    validate_dkg_activation(&boundary, &target, &members, 3, true).unwrap();
    let mut on_time = boundary.clone();
    on_time.height = 181;
    validate_dkg_activation(&on_time, &target, &members, 3, false).unwrap();
    assert!(validate_dkg_activation(&on_time, &target, &members, 3, true).is_err());
    for height in [0, 179, 180] {
        let mut early = boundary.clone();
        early.height = height;
        assert!(validate_dkg_activation(&early, &target, &members, 3, false).is_err());
    }
    for defect in 0..7 {
        let mut wrong = boundary.clone();
        match defect {
            0 => wrong.cycle += 1,
            1 => wrong.epoch += 1,
            2 => wrong.freeze += 1,
            3 => wrong.planned += 1,
            4 => wrong.height = 179,
            5 => wrong.target_hash = B256::ZERO,
            _ => {
                wrong.members.pop();
            }
        }
        assert!(validate_dkg_activation(&wrong, &target, &members, 3, false).is_err());
    }
}

#[test]
fn reveal_must_belong_to_the_matching_activation_not_a_later_ceremony() {
    let target = FrozenTarget {
        cycle: 7,
        freeze: 80,
        planned: 180,
    };
    let freeze = "freezing validator set and starting DKG rotation dkg_cycle=7";
    let reveal = "DKG: a validator's individual share was REVEALED revealed_validator=public-key";
    let activated = "VRF/DKG material activated dkg_cycle=7 activation_height=193";
    validate_reveal_interval(
        &format!("{freeze}\n{reveal}\n{activated}"),
        &target,
        194,
        "public-key",
    )
    .unwrap();
    // Captured scenario 8: anchor 180 is committed in boundary block 181.
    validate_reveal_interval(
        &format!("{freeze}\n{reveal}\n{}", activated.replace("193", "180")),
        &target,
        181,
        "public-key",
    )
    .unwrap();
    assert!(validate_reveal_interval(
        &format!("{freeze}\n{reveal}\n{activated}"),
        &target,
        0,
        "public-key",
    )
    .is_err());
    for log in [
        format!("{reveal}\n{freeze}\n{activated}"),
        format!("{freeze}\n{activated}\n{reveal}"),
        format!("{freeze}\n{reveal}\n{}", activated.replace("193", "194")),
        format!(
            "{freeze}\n{}\n{activated}",
            reveal.replace("public-key", "foreign-key")
        ),
        format!(
            "{freeze}\n{reveal}\n{}",
            activated.replace("dkg_cycle=7", "dkg_cycle=8")
        ),
        format!(
            "{}\n{reveal}\n{activated}",
            freeze.replace("dkg_cycle=7", "dkg_cycle=8")
        ),
    ] {
        assert!(validate_reveal_interval(&log, &target, 194, "public-key").is_err());
    }
}
