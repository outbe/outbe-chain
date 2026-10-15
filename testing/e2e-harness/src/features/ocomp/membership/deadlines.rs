//! Two dynamic job deadlines inside one recovery window with one soft penalty.

use crate::features::ocomp::observations::deadline::DynamicDeadlineMiss;
use crate::features::ocomp::*;

type PublicVotes = crate::world::rpc::OcompPublicVoteAccountabilityV1;

struct DynamicDeadlines {
    ports: Vec<u16>,
    members: Vec<Address>,
    owned: Vec<(u32, u32)>,
    requests: Vec<crate::world::rpc::OcompPublicJobRequestV1>,
    expected_slots: Vec<Vec<u16>>,
    deadlines: [u64; 2],
    recovery: u64,
    records: Vec<OcompJobRecordV1>,
    job_ids: [B256; 2],
    baselines: Vec<PublicVotes>,
    target: u64,
}

struct DeadlineEvidence<'a> {
    states: &'a [DynamicDeadlineAccount; 5],
    checkpoints: [crate::world::rpc::FinalizedCheckpoint; 2],
    final_height: u64,
}

fn dynamic_members(world: &World) -> Vec<Address> {
    (0..5)
        .map(|index| {
            let key = world
                .validators
                .get(index)
                .evm_key()
                .expect("validator identity");
            eth::address_of(&key).expect("derive public validator identity")
        })
        .collect()
}

fn pinned_baselines(
    world: &World,
    requests: &[crate::world::rpc::OcompPublicJobRequestV1],
    baseline_height: u64,
) -> (Vec<OcompJobRecordV1>, [B256; 2], Vec<PublicVotes>) {
    let primary = world.validators.primary_port();
    let records: Vec<_> = requests
        .iter()
        .map(|request| {
            world
                .rpc
                .ocomp_job_record_at_on(primary, request.intent_id, baseline_height)
                .expect("pinned dynamic job identity")
        })
        .collect();
    let job_ids: [B256; 2] = std::array::from_fn(|ordinal| {
        records[ordinal]
            .finalized
            .as_ref()
            .expect("finalized dynamic intent")
            .job_id
    });
    assert_ne!(job_ids[0], job_ids[1], "two different dynamic jobs");
    let baselines: Vec<_> = job_ids
        .iter()
        .map(|&job_id| {
            world
                .rpc
                .ocomp_vote_accountability_at_on(primary, job_id, baseline_height)
                .expect("retain completed quorum before the deadline wait")
        })
        .collect();
    for ordinal in 0..2 {
        let (baseline, intent) = (&baselines[ordinal], &records[ordinal].intent);
        assert_eq!(baseline.job_id, job_ids[ordinal]);
        assert_eq!(
            baseline.result_validator_set_epoch,
            intent.result_validator_set_epoch
        );
        assert_eq!(
            baseline.result_committee_set_hash,
            intent.result_committee_set_hash
        );
        assert_eq!(
            baseline.result_ocomp_binding_hash,
            intent.result_ocomp_binding_hash
        );
    }
    (records, job_ids, baselines)
}

fn arm_dynamic_deadlines(world: &mut World) -> DynamicDeadlines {
    // The full 43,200-block recovery/possible-jail acceptance is a separate lane.
    assert_eq!(world.validators.size(), 4, "four founders plus one joiner");
    let ports = dynamic_deadline_ports(
        world.validators.committee_ports(),
        world.validators.http_port(world.validators.joiner_index()),
    )
    .expect("five distinct expected observers");
    let members = dynamic_members(world);
    let owned: Vec<_> = (0..5)
        .map(|index| {
            world
                .localnet
                .live_validator_and_enclave_pids(index)
                .expect("every expected validator and enclave must be owned and live")
        })
        .collect();
    let requests = world.state.ocomp_dynamic_job_requests.clone();
    assert_eq!(requests.len(), 2, "job A and job B requests");
    let expected_slots = world.state.ocomp_dynamic_vote_slots.clone();
    assert_eq!(expected_slots.len(), 2, "job A and job B voting slots");
    let deadlines = [requests[0].deadline_height, requests[1].deadline_height];
    let recovery = deadlines[0]
        .checked_add(DYNAMIC_OCOMP_RECOVERY_BLOCKS)
        .expect("recovery height overflow");
    assert!(
        deadlines[0] > 0 && deadlines[0] < deadlines[1] && deadlines[1] < recovery,
        "two ordered deadlines must lie in one recovery window"
    );
    let baseline_height = world
        .rpc
        .finalized_result(world.validators.primary_port())
        .expect("finalized completed quorums");
    let (records, job_ids, baselines) = pinned_baselines(world, &requests, baseline_height);
    let target = fresh_deadline_target(world, &ports, deadlines[1], recovery);
    world.state.restart_observations.push(serde_json::json!({
        "phase": "ocomp_dynamic_deadlines_armed", "ports": ports, "members": members,
        "owned_pids": owned, "baseline_height": baseline_height, "baselines": baselines,
        "job_ids": job_ids, "deadlines": deadlines, "target": target,
    }));
    DynamicDeadlines {
        ports,
        members,
        owned,
        requests,
        expected_slots,
        deadlines,
        recovery,
        records,
        job_ids,
        baselines,
        target,
    }
}

fn fresh_deadline_target(world: &World, ports: &[u16], last_deadline: u64, recovery: u64) -> u64 {
    let target = world
        .rpc
        .fresh_finality_target(ports)
        .expect("all-five fresh target")
        .max(
            last_deadline
                .checked_add(1)
                .expect("deadline height overflow"),
        );
    assert!(
        target < recovery,
        "fresh target already crossed the separate recovery gate"
    );
    target
}

fn wait_dynamic_final_checkpoint(
    world: &mut World,
    jobs: &DynamicDeadlines,
) -> crate::world::rpc::FinalizedCheckpoint {
    let target = jobs.target;
    // Preserve the original 900 x 2s allowance, now requiring every expected peer.
    let wait_deadline = Instant::now() + Duration::from_secs(900 * 2);
    loop {
        dynamic_deadline_assert_live(world, &jobs.owned);
        let observations: Vec<_> = jobs
            .ports
            .iter()
            .map(|&port| world.rpc.finalized_result(port))
            .collect();
        assert!(
            observations.iter().all(|height| match height {
                Ok(height) => *height < jobs.recovery,
                Err(_) => true, // transient RPC failure still cannot satisfy the barrier below
            }),
            "an observer crossed the separate recovery gate"
        );
        let reached = observations
            .iter()
            .map(|height| height.as_ref().ok().filter(|height| **height >= target))
            .collect::<Option<Vec<_>>>();
        if let Some(reached) = reached {
            let height = *reached.into_iter().min().expect("five finalities");
            assert!(
                height < jobs.recovery,
                "dynamic deadline proof crossed the separate recovery gate"
            );
            return dynamic_deadline_checkpoint(world, &jobs.ports, height)
                .expect("all-five common finalized hash/root");
        }
        assert!(
            Instant::now() < wait_deadline,
            "all five validators did not finalize dynamic deadlines: {observations:?}"
        );
        sleep(Duration::from_secs(2));
    }
}

fn pinned_deadline_account(
    world: &mut World,
    jobs: &DynamicDeadlines,
    height: u64,
) -> DynamicDeadlineAccount {
    let victim = jobs.members[3];
    let checkpoint = dynamic_deadline_checkpoint(world, &jobs.ports, height)
        .expect("pinned accounting checkpoint");
    let observed: Vec<_> = jobs
        .ports
        .iter()
        .map(|&port| {
            dynamic_deadline_account(world, port, victim, height)
                .expect("pinned soft-penalty accounting")
        })
        .collect();
    world.state.restart_observations.push(serde_json::json!({
        "phase": "ocomp_dynamic_deadline_accounting", "height": height,
        "block_hash": checkpoint.block_hash, "state_root": checkpoint.state_root,
        "ports": jobs.ports, "accounts": observed,
    }));
    assert!(
        observed.iter().all(|account| account == &observed[0]),
        "all-five accounting disagreement"
    );
    assert_eq!(
        dynamic_deadline_checkpoint(world, &jobs.ports, height).unwrap(),
        checkpoint
    );
    observed[0].clone()
}

fn verify_deadline_events_on(
    world: &mut World,
    jobs: &DynamicDeadlines,
    port: u16,
    evidence: &DeadlineEvidence<'_>,
) -> [DynamicDeadlineMiss; 2] {
    let final_height = evidence.final_height;
    let logs = eth::raw_json_result(
        &world.rpc.url(port),
        "eth_getLogs",
        serde_json::json!([{
            "address": crate::internal::addresses::WWD_ADDR,
            "fromBlock": format!("0x{:x}", jobs.requests[0].request_height),
            "toBlock": format!("0x{:x}", final_height),
            "topics": [eth::IMetadosis::OcompVoteMissed::SIGNATURE_HASH],
        }]),
    )
    .expect("finalized canonical OcompVoteMissed events");
    world.state.restart_observations.push(serde_json::json!({
        "phase": "ocomp_dynamic_deadline_events", "port": port, "logs": logs,
        "through_height": final_height,
    }));
    let events =
        dynamic_deadline_decode_events(&logs, jobs.members[3], jobs.job_ids, evidence.checkpoints)
            .expect("exact first/repeat OCOMP event identities");
    for event in &events {
        let receipt = eth::raw_json_result(
            &world.rpc.url(port),
            "eth_getTransactionReceipt",
            serde_json::json!([event.transaction_hash]),
        )
        .expect("canonical OcompLifecycleBegin receipt RPC");
        world.state.restart_observations.push(serde_json::json!({
            "phase": "ocomp_dynamic_deadline_receipt", "port": port,
            "event": event, "receipt": receipt,
        }));
        dynamic_deadline_validate_receipt(&receipt, event)
            .expect("successful canonical system receipt contains the exact miss event");
    }
    dynamic_deadline_validate_penalties(
        evidence.states,
        &events,
        &jobs.members,
        jobs.deadlines,
        final_height,
    )
    .expect("one bonded-only slash, fixed recovery window, and unchanged ACTIVE membership");
    events
}

fn closed_accountability_on(
    world: &World,
    jobs: &DynamicDeadlines,
    (ordinal, port): (usize, u16),
    final_height: u64,
) -> PublicVotes {
    let request = &jobs.requests[ordinal];
    let job_id = jobs.job_ids[ordinal];
    let closed = world
        .rpc
        .ocomp_vote_accountability_at_on(port, job_id, request.deadline_height)
        .expect("accountability at exact closing block");
    dynamic_deadline_validate_accountability(
        &closed,
        &jobs.baselines[ordinal],
        &jobs.expected_slots[ordinal],
        request.deadline_height,
        [(4, 3), (5, 4)][ordinal],
    )
    .expect("pinned historical quorum and exact singleton missing bitmap");
    let record = world
        .rpc
        .ocomp_job_record_at_on(port, request.intent_id, final_height)
        .expect("finalized dynamic job remains available");
    assert_eq!(
        record, jobs.records[ordinal],
        "closing mutated the completed job/result/binding"
    );
    assert_eq!(
        world
            .rpc
            .ocomp_vote_accountability_at_on(port, job_id, final_height)
            .expect("post-deadline accountability"),
        closed,
        "closed accountability changed later"
    );
    closed
}

#[then("both deadlines record validator 3 missing with one soft penalty and all five validators stay live")]
fn dynamic_deadlines_preserve_active_membership_with_one_soft_penalty(world: &mut World) {
    let jobs = arm_dynamic_deadlines(world);
    let final_checkpoint = wait_dynamic_final_checkpoint(world, &jobs);
    let deadlines = jobs.deadlines;
    let checkpoints = deadlines.map(|height| {
        dynamic_deadline_checkpoint(world, &jobs.ports, height).expect("canonical closing block")
    });
    let heights = [
        deadlines[0] - 1,
        deadlines[0],
        deadlines[1] - 1,
        deadlines[1],
        final_checkpoint.height,
    ];
    let states = heights.map(|height| pinned_deadline_account(world, &jobs, height));
    let evidence = DeadlineEvidence {
        states: &states,
        checkpoints,
        final_height: final_checkpoint.height,
    };
    let mut agreed_events = None;
    for &port in &jobs.ports {
        let events = verify_deadline_events_on(world, &jobs, port, &evidence);
        if let Some(ref expected) = agreed_events {
            assert_eq!(&events, expected, "all-five event disagreement");
        } else {
            agreed_events = Some(events);
        }
    }
    for ordinal in 0..jobs.requests.len() {
        let mut agreed = None;
        for &port in &jobs.ports {
            let closed =
                closed_accountability_on(world, &jobs, (ordinal, port), final_checkpoint.height);
            if let Some(ref first) = agreed {
                assert_eq!(
                    &closed, first,
                    "all-five closed accountability disagreement"
                );
            } else {
                agreed = Some(closed);
            }
        }
        world.state.restart_observations.push(serde_json::json!({
            "phase": "ocomp_dynamic_deadline_accountability", "job_id": jobs.job_ids[ordinal],
            "accountability": agreed, "ports": jobs.ports,
        }));
    }
    dynamic_deadline_assert_live(world, &jobs.owned);
    assert_eq!(
        dynamic_deadline_checkpoint(world, &jobs.ports, final_checkpoint.height).unwrap(),
        final_checkpoint
    );
    dynamic_deadline_assert_live(world, &jobs.owned);
    world.state.restart_observations.push(serde_json::json!({
        "phase": "ocomp_dynamic_deadlines_verified", "ports": jobs.ports, "owned_pids": jobs.owned,
        "height": final_checkpoint.height, "block_hash": final_checkpoint.block_hash,
        "state_root": final_checkpoint.state_root, "recovery_deadline": jobs.recovery,
    }));
}
