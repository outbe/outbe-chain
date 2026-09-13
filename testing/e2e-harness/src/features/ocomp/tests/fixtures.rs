use super::*;

pub(super) fn completed_accountability() -> OcompPublicVoteAccountabilityV1 {
    OcompPublicVoteAccountabilityV1 {
        job_id: alloy_primitives::B256::repeat_byte(0x11),
        result_validator_set_epoch: 7,
        result_committee_set_hash: alloy_primitives::B256::repeat_byte(0x22),
        result_ocomp_binding_hash: alloy_primitives::B256::repeat_byte(0x33),
        member_count: 4,
        quorum_threshold: 3,
        slot_validator_indexes: vec![0, 1, 2],
        slot_first_signatures: vec![(0, vec![0xa0]), (1, vec![0xa1]), (2, vec![0xa2])],
        quorum_result_digest: Some(alloy_primitives::B256::repeat_byte(0x44)),
        quorum_height: Some(92),
        quorum_signer_bitmap: Some(vec![0b0000_0111]),
        closed_height: None,
        timely_bitmap: None,
        matching_bitmap: None,
        divergent_bitmap: None,
        missing_bitmap: None,
        equivocation_bitmap: None,
    }
}

pub(super) fn dynamic_deadline_fixture() -> (
    Vec<Address>,
    [DynamicDeadlineAccount; 5],
    [DynamicDeadlineMiss; 2],
    [crate::world::rpc::FinalizedCheckpoint; 2],
    serde_json::Value,
) {
    let members: Vec<_> = (1..=5).map(Address::repeat_byte).collect();
    // Canonical native units, with a remainder to exercise floor(bonded/10).
    let bonded =
        U256::from(100_000_u64) * U256::from(1_000_000_000_000_000_000_u64) + U256::from(9);
    let before = DynamicDeadlineAccount {
        bonded,
        mirrored: bonded,
        total_staked: bonded * U256::from(5),
        staking_balance: bonded * U256::from(5),
        status: 2,
        ordinary_slash_count: 7,
        ocomp_miss_count: 0,
        ocomp_recovery_deadline: 0,
        active: members.clone(),
        participants: members.clone(),
    };
    let slash = bonded / U256::from(10);
    let mut after = before.clone();
    after.bonded -= slash;
    after.mirrored -= slash;
    after.total_staked -= slash;
    after.staking_balance -= slash;
    let mut states = [before, after.clone(), after.clone(), after.clone(), after];
    for (ordinal, state) in states.iter_mut().enumerate().skip(1) {
        state.ocomp_miss_count = [0, 1, 1, 2, 2][ordinal];
        state.ocomp_recovery_deadline = 1000 + DYNAMIC_OCOMP_RECOVERY_BLOCKS;
    }
    let checkpoints = [1000, 1200].map(|height| crate::world::rpc::FinalizedCheckpoint {
        height,
        block_hash: B256::repeat_byte(if height == 1000 { 10 } else { 12 }),
        state_root: B256::repeat_byte(if height == 1000 { 20 } else { 22 }),
    });
    let jobs = [B256::repeat_byte(1), B256::repeat_byte(2)];
    let logs = serde_json::Value::Array(
        (0..2)
            .map(|ordinal| {
                let event = eth::IMetadosis::OcompVoteMissed {
                    validator: members[3],
                    jobId: jobs[ordinal],
                    missCount: (ordinal + 1) as u64,
                    slashedBonded: if ordinal == 0 { slash } else { U256::ZERO },
                    recoveryDeadline: 1000 + DYNAMIC_OCOMP_RECOVERY_BLOCKS,
                    firstInWindow: ordinal == 0,
                };
                let data = event.encode_log_data();
                serde_json::json!({
                    "address": crate::internal::addresses::WWD_ADDR,
                    "topics": data.topics(), "data": data.data, "removed": false,
                    "blockNumber": format!("0x{:x}", checkpoints[ordinal].height),
                    "blockHash": checkpoints[ordinal].block_hash,
                    "transactionHash": B256::repeat_byte(ordinal as u8 + 30), "logIndex": "0x0",
                })
            })
            .collect(),
    );
    let events = dynamic_deadline_decode_events(&logs, members[3], jobs, checkpoints).unwrap();
    (members, states, events, checkpoints, logs)
}

pub(super) fn dynamic_accountability_fixture(
    members: u16,
    quorum: u16,
    missing: u16,
) -> (
    crate::world::rpc::OcompPublicVoteAccountabilityV1,
    crate::world::rpc::OcompPublicVoteAccountabilityV1,
) {
    let slots: Vec<_> = (0..members).filter(|index| *index != missing).collect();
    let mut timely = vec![0_u8; usize::from(members).div_ceil(8)];
    for index in &slots {
        timely[usize::from(index / 8)] |= 1 << (index % 8);
    }
    let baseline = crate::world::rpc::OcompPublicVoteAccountabilityV1 {
        job_id: B256::repeat_byte(1),
        result_validator_set_epoch: 2,
        result_committee_set_hash: B256::repeat_byte(2),
        result_ocomp_binding_hash: B256::repeat_byte(3),
        member_count: members,
        quorum_threshold: quorum,
        slot_first_signatures: slots
            .iter()
            .map(|&index| (index, vec![index as u8 + 1; 64]))
            .collect(),
        slot_validator_indexes: slots,
        quorum_result_digest: Some(B256::repeat_byte(4)),
        quorum_height: Some(900),
        quorum_signer_bitmap: Some(timely.clone()),
        closed_height: None,
        timely_bitmap: None,
        matching_bitmap: None,
        divergent_bitmap: None,
        missing_bitmap: None,
        equivocation_bitmap: None,
    };
    let mut closed = baseline.clone();
    closed.closed_height = Some(1000);
    closed.timely_bitmap = Some(timely.clone());
    closed.matching_bitmap = Some(timely);
    closed.divergent_bitmap = Some(vec![0]);
    closed.equivocation_bitmap = Some(vec![0]);
    closed.missing_bitmap = Some(singleton_participant_bitmap(members, missing));
    (baseline, closed)
}
