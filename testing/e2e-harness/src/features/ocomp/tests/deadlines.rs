use super::*;

#[test]
fn dynamic_deadline_requires_all_five_distinct_observers() {
    assert_eq!(
        dynamic_deadline_ports(vec![10, 11, 12, 13], 14).unwrap(),
        vec![10, 11, 12, 13, 14]
    );
    assert!(dynamic_deadline_ports(vec![10, 11, 12], 14).is_err());
    assert!(dynamic_deadline_ports(vec![10, 11, 12, 13], 13).is_err());
    assert!(dynamic_deadline_ports(vec![10, 10, 12, 13], 14).is_err());
}

#[test]
fn dynamic_deadlines_prove_one_real_unit_slash_and_an_unchanged_repeat_window() {
    let (members, states, events, _, _) = dynamic_deadline_fixture();
    dynamic_deadline_validate_penalties(&states, &events, &members, [1000, 1200], 1201).unwrap();
    assert_eq!(
        events[0].slashed_bonded,
        U256::from(10_000_u64) * U256::from(1_000_000_000_000_000_000_u64)
    );
    assert_eq!(
        states[1].ordinary_slash_count, 7,
        "OCOMP miss count is not ordinary slashCount"
    );
    assert_eq!(events[1].miss_count, 2);
    assert_eq!(events[1].slashed_bonded, U256::ZERO);
    assert_eq!(events[0].recovery_deadline, 44_200);
    assert_eq!(events[1].recovery_deadline, 44_200);
}

#[test]
fn dynamic_deadline_storage_requires_a_complete_u64_word() {
    for expected in [0, 1, 2, 44_200, u64::MAX] {
        let word = B256::from(U256::from(expected).to_be_bytes::<32>());
        assert_eq!(
            dynamic_deadline_storage_u64(&serde_json::json!(word)).unwrap(),
            expected
        );
    }
    for wrong in [
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!("0x"),
        serde_json::json!("0x01"),
        serde_json::json!("not-hex"),
        serde_json::json!(1),
        serde_json::json!(B256::from(
            (U256::from(u64::MAX) + U256::from(1)).to_be_bytes::<32>()
        )),
    ] {
        assert!(
            dynamic_deadline_storage_u64(&wrong).is_err(),
            "accepted {wrong}"
        );
    }
}

#[test]
fn dynamic_deadlines_reject_incomplete_durable_recovery_transitions() {
    let (members, states, events, _, _) = dynamic_deadline_fixture();
    for ordinal in 0..5 {
        for deadline_defect in [false, true] {
            let mut wrong = states.clone();
            if deadline_defect {
                wrong[ordinal].ocomp_recovery_deadline ^= 1;
            } else {
                wrong[ordinal].ocomp_miss_count ^= 1;
            }
            assert!(
                dynamic_deadline_validate_penalties(&wrong, &events, &members, [1000, 1200], 1201,)
                    .is_err(),
                "accepted durable point {ordinal}, deadline={deadline_defect}"
            );
        }
    }
    let mut lost_window = states.clone();
    lost_window[4].ocomp_recovery_deadline = 0;
    assert!(dynamic_deadline_validate_penalties(
        &lost_window,
        &events,
        &members,
        [1000, 1200],
        1201,
    )
    .is_err());
}

#[test]
fn dynamic_deadlines_require_successful_exact_system_receipts() {
    let (_, _, events, _, logs) = dynamic_deadline_fixture();
    for (ordinal, event) in events.iter().enumerate() {
        let receipt = serde_json::json!({
            "status": "0x1", "blockNumber": format!("0x{:x}", event.height),
            "blockHash": event.block_hash, "transactionHash": event.transaction_hash,
            "logs": [logs[ordinal]],
        });
        dynamic_deadline_validate_receipt(&receipt, event).unwrap();
        for (field, value) in [
            ("status", serde_json::json!("0x0")),
            ("status", serde_json::Value::Null),
            ("status", serde_json::json!("0x10000000000000000")),
            ("blockNumber", serde_json::json!("0x0")),
            ("blockHash", serde_json::json!(B256::repeat_byte(99))),
            ("transactionHash", serde_json::json!(B256::repeat_byte(99))),
            ("transactionHash", serde_json::Value::Null),
            ("logs", serde_json::json!([])),
            ("logs", serde_json::Value::Null),
            ("logs", serde_json::json!([logs[ordinal], logs[ordinal]])),
        ] {
            let mut wrong = receipt.clone();
            wrong[field] = value;
            assert!(
                dynamic_deadline_validate_receipt(&wrong, event).is_err(),
                "accepted receipt defect {field}"
            );
        }
        for (field, value) in [
            (
                "address",
                serde_json::json!(crate::internal::addresses::VS_ADDR),
            ),
            ("topics", serde_json::json!([])),
            ("data", serde_json::json!("0x00")),
            ("removed", serde_json::json!(true)),
            ("removed", serde_json::Value::Null),
            ("logIndex", serde_json::json!("0x1")),
            ("logIndex", serde_json::Value::Null),
            ("blockNumber", serde_json::json!("0x0")),
            ("blockHash", serde_json::json!(B256::repeat_byte(99))),
            ("transactionHash", serde_json::json!(B256::repeat_byte(99))),
        ] {
            let mut wrong = receipt.clone();
            wrong["logs"][0][field] = value;
            assert!(
                dynamic_deadline_validate_receipt(&wrong, event).is_err(),
                "accepted receipt log defect {field}"
            );
        }
        let mut noncanonical = receipt.clone();
        let mut data = serde_json::from_value::<Bytes>(noncanonical["logs"][0]["data"].clone())
            .unwrap()
            .to_vec();
        data[127] = 2;
        noncanonical["logs"][0]["data"] = serde_json::json!(Bytes::from(data));
        assert!(dynamic_deadline_validate_receipt(&noncanonical, event).is_err());
        assert!(dynamic_deadline_validate_receipt(&serde_json::Value::Null, event).is_err());
    }
}

#[test]
fn dynamic_deadlines_reject_wrong_event_source_identity_or_canonical_point() {
    let (members, _, events, checkpoints, logs) = dynamic_deadline_fixture();
    let jobs = events.clone().map(|event| event.job_id);
    let replacements = [
        (
            "address",
            serde_json::json!(crate::internal::addresses::VS_ADDR),
        ),
        ("removed", serde_json::json!(true)),
        ("removed", serde_json::Value::Null),
        ("blockNumber", serde_json::json!("0x3e9")),
        ("blockHash", serde_json::json!(B256::repeat_byte(99))),
        ("transactionHash", serde_json::Value::Null),
        ("logIndex", serde_json::Value::Null),
        ("data", serde_json::json!("0x00")),
        ("topics", serde_json::json!([])),
    ];
    for (field, value) in replacements {
        let mut wrong = logs.clone();
        wrong[0][field] = value;
        assert!(
            dynamic_deadline_decode_events(&wrong, members[3], jobs, checkpoints).is_err(),
            "accepted wrong {field}"
        );
    }
    for topic in 0..3 {
        let mut wrong = logs.clone();
        wrong[0]["topics"][topic] = serde_json::json!(B256::repeat_byte(90));
        assert!(
            dynamic_deadline_decode_events(&wrong, members[3], jobs, checkpoints).is_err(),
            "accepted wrong topic {topic}"
        );
    }
    let mut invalid_bool = logs.clone();
    let mut data: Vec<u8> = serde_json::from_value::<Bytes>(invalid_bool[0]["data"].clone())
        .unwrap()
        .to_vec();
    data[127] = 2;
    invalid_bool[0]["data"] = serde_json::json!(Bytes::from(data));
    assert!(dynamic_deadline_decode_events(&invalid_bool, members[3], jobs, checkpoints).is_err());
    for rows in [
        vec![],
        vec![logs[0].clone()],
        vec![logs[0].clone(), logs[0].clone()],
        vec![logs[0].clone(), logs[1].clone(), logs[1].clone()],
    ] {
        assert!(dynamic_deadline_decode_events(
            &serde_json::Value::Array(rows),
            members[3],
            jobs,
            checkpoints
        )
        .is_err());
    }
    assert!(
        dynamic_deadline_decode_events(&serde_json::json!({}), members[3], jobs, checkpoints)
            .is_err()
    );
    // RPC ordering is not identity: both exact canonical events still agree.
    let reversed = serde_json::json!([logs[1], logs[0]]);
    assert_eq!(
        dynamic_deadline_decode_events(&reversed, members[3], jobs, checkpoints).unwrap(),
        events
    );
}

#[test]
fn dynamic_deadlines_reject_repeat_slash_count_reset_and_deadline_extension() {
    let (members, states, events, _, _) = dynamic_deadline_fixture();
    for ordinal in 0..2 {
        for defect in 0..6 {
            let mut wrong = events.clone();
            match defect {
                0 => wrong[ordinal].miss_count += 1,
                1 => wrong[ordinal].first_in_window = !wrong[ordinal].first_in_window,
                2 => wrong[ordinal].recovery_deadline += 1,
                3 => wrong[ordinal].slashed_bonded += U256::from(1),
                4 => wrong[ordinal].validator = members[2],
                5 => wrong[ordinal].height += 1,
                _ => unreachable!(),
            }
            assert!(
                dynamic_deadline_validate_penalties(&states, &wrong, &members, [1000, 1200], 1201)
                    .is_err(),
                "accepted event {ordinal}, defect {defect}"
            );
        }
    }
}

#[test]
fn dynamic_deadlines_reject_unrelated_jail_or_any_unaccounted_burn() {
    let (members, states, events, _, _) = dynamic_deadline_fixture();
    for point in 0..5 {
        for defect in 0..8 {
            let mut wrong = states.clone();
            match defect {
                0 => wrong[point].bonded += U256::from(1),
                1 => wrong[point].mirrored += U256::from(1),
                2 => wrong[point].total_staked += U256::from(1),
                3 => wrong[point].staking_balance += U256::from(1),
                4 => wrong[point].ordinary_slash_count += 1,
                5 => wrong[point].status = 6,
                6 => {
                    wrong[point].active.remove(3);
                }
                7 => {
                    wrong[point].participants.remove(4);
                }
                _ => unreachable!(),
            }
            assert!(
                dynamic_deadline_validate_penalties(&wrong, &events, &members, [1000, 1200], 1201)
                    .is_err(),
                "accepted accounting point {point}, defect {defect}"
            );
        }
    }
    let mut duplicate_members = members.clone();
    duplicate_members[4] = members[3];
    assert!(dynamic_deadline_validate_penalties(
        &states,
        &events,
        &duplicate_members,
        [1000, 1200],
        1201
    )
    .is_err());
}

#[test]
fn dynamic_deadlines_do_not_claim_the_full_recovery_gate() {
    let (members, states, events, _, _) = dynamic_deadline_fixture();
    for (deadlines, height) in [
        ([1000, 1200], 1200),
        ([1000, 1200], 44_200),
        ([1000, 1200], 44_201),
        ([1000, 1000], 1201),
        ([0, 1200], 1201),
        ([u64::MAX - 1, u64::MAX], u64::MAX),
    ] {
        assert!(
            dynamic_deadline_validate_penalties(&states, &events, &members, deadlines, height)
                .is_err()
        );
    }
}

#[test]
fn dynamic_deadlines_use_overflow_safe_floor_slashing_and_reject_underfunding() {
    let (members, mut states, mut events, _, _) = dynamic_deadline_fixture();
    for field in 0..2 {
        let mut wrong = states.clone();
        if field == 0 {
            wrong[0].total_staked = U256::ZERO;
        } else {
            wrong[0].staking_balance = U256::ZERO;
        }
        assert!(
            dynamic_deadline_validate_penalties(&wrong, &events, &members, [1000, 1200], 1201)
                .is_err()
        );
    }
    // Exercise the evaluator's U256 arithmetic without multiplying MAX by 10.
    states[0].bonded = U256::MAX;
    states[0].mirrored = U256::MAX;
    states[0].total_staked = U256::MAX;
    states[0].staking_balance = U256::MAX;
    let slash = U256::MAX / U256::from(10);
    let mut after = states[0].clone();
    after.bonded -= slash;
    after.mirrored -= slash;
    after.total_staked -= slash;
    after.staking_balance -= slash;
    for (ordinal, state) in states.iter_mut().enumerate().skip(1) {
        *state = after.clone();
        state.ocomp_miss_count = [0, 1, 1, 2, 2][ordinal];
        state.ocomp_recovery_deadline = 1000 + DYNAMIC_OCOMP_RECOVERY_BLOCKS;
    }
    events[0].slashed_bonded = slash;
    dynamic_deadline_validate_penalties(&states, &events, &members, [1000, 1200], 1201).unwrap();
}

#[test]
fn dynamic_deadlines_require_every_finalized_hash_root_not_a_filtered_subset() {
    let ports = [10, 11, 12, 13, 14];
    let checkpoint = crate::world::rpc::FinalizedCheckpoint {
        height: 1201,
        block_hash: B256::repeat_byte(7),
        state_root: B256::repeat_byte(8),
    };
    let observed: Vec<_> = ports.iter().map(|&port| (port, 1202, checkpoint)).collect();
    assert_eq!(
        dynamic_deadline_validate_checkpoints(&ports, 1201, &observed).unwrap(),
        checkpoint
    );
    for index in 0..5 {
        for defect in 0..5 {
            let mut wrong = observed.clone();
            match defect {
                0 => {
                    wrong.remove(index);
                }
                1 => wrong[index].0 = 99,
                2 => wrong[index].1 = 1200,
                3 => wrong[index].2.block_hash = B256::repeat_byte(9),
                4 => wrong[index].2.state_root = B256::repeat_byte(9),
                _ => unreachable!(),
            }
            assert!(dynamic_deadline_validate_checkpoints(&ports, 1201, &wrong).is_err());
        }
    }
    assert!(dynamic_deadline_validate_checkpoints(&ports, 1200, &observed).is_err());
}

#[test]
fn dynamic_deadlines_preserve_both_historical_quorums_and_missing_snapshot_indexes() {
    // Missing snapshot index is not assumed to equal validator directory 3.
    for (members, quorum, missing) in [(4, 3, 1), (5, 4, 4)] {
        let (baseline, closed) = dynamic_accountability_fixture(members, quorum, missing);
        let slots = baseline.slot_validator_indexes.clone();
        dynamic_deadline_validate_accountability(
            &closed,
            &baseline,
            &slots,
            1000,
            (members, quorum),
        )
        .unwrap();
        for defect in 0..13 {
            let mut wrong = closed.clone();
            match defect {
                0 => wrong.member_count += 1,
                1 => wrong.quorum_threshold -= 1,
                2 => wrong.job_id = B256::ZERO,
                3 => wrong.result_validator_set_epoch += 1,
                4 => wrong.result_committee_set_hash = B256::ZERO,
                5 => wrong.result_ocomp_binding_hash = B256::ZERO,
                6 => wrong.quorum_result_digest = Some(B256::ZERO),
                7 => wrong.closed_height = Some(1001),
                8 => wrong.missing_bitmap = Some(vec![0]),
                9 => wrong.slot_first_signatures[0].1[0] ^= 1,
                10 => {
                    wrong.slot_validator_indexes.pop();
                }
                11 => wrong.quorum_height = None,
                12 => wrong.quorum_signer_bitmap = None,
                _ => unreachable!(),
            }
            assert!(
                dynamic_deadline_validate_accountability(
                    &wrong,
                    &baseline,
                    &slots,
                    1000,
                    (members, quorum)
                )
                .is_err(),
                "accepted changed historical accountability {defect}"
            );
        }
    }
}
