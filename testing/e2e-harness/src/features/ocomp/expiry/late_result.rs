//! A correct late validator-3 result after the finalized quorum.

use crate::features::ocomp::*;

#[when("validator 3 OCOMP worker is stopped before the job")]
fn stop_late_worker_before_job(world: &mut World) {
    world
        .ocomp
        .ensure_baseline_runtime_ready(1)
        .expect("all baseline OCOMP workers are live and authenticated before the fault");
    world
        .ocomp
        .apply_process_fault(OcompProcessFault::StopWorker {
            validator_index: 3,
            worker_ordinal: 0,
        })
        .expect("stop validator-3 Worker before the job");
}

#[then("validators 0, 1 and 2 finalize the result quorum while validator 3 remains computing")]
fn three_validators_finalize_before_late_compute(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let ports = world.validators.committee_ports();
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(600);

    loop {
        world
            .localnet
            .ensure_committee_alive()
            .expect("validator committee remains alive before the late local result");
        for validator_index in [0, 1, 2] {
            world
                .ocomp
                .ensure_worker_alive(validator_index, 0)
                .unwrap_or_else(|error| {
                    panic!(
                        "validator-{validator_index} Worker exited before the three-voter quorum: {error}"
                    )
                });
        }
        let records = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_record_on(port, request.intent_id)
            })
            .collect::<Vec<_>>();
        let completed = records.iter().all(|record| {
            record
                .as_ref()
                .is_some_and(|record| record.status == OcompJobStatus::Completed)
        });
        if completed {
            let record = records[0]
                .clone()
                .expect("all completed records are present");
            assert!(
                records
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&record)),
                "validators expose different completed OCOMP records"
            );
            let finalized = record
                .finalized
                .as_ref()
                .expect("completed OCOMP record is finalized");
            let job_id = finalized.job_id;
            let accountability = world
                .rpc
                .finalized_ocomp_vote_accountability_on(primary, job_id)
                .expect("completed three-voter accountability");
            if accountability.slot_validator_indexes != [0, 1, 2] {
                assert!(
                    Instant::now() < timeout,
                    "unexpected finalized OCOMP voters before late compute: {:?}",
                    accountability.slot_validator_indexes
                );
                sleep(Duration::from_millis(250));
                continue;
            }
            assert!(accountability.quorum_result_digest.is_some());
            assert!(
                !local_result_path(world, 3, job_id).exists(),
                "validator-3 produced a local result while its Worker was stopped"
            );
            let activations = ports
                .iter()
                .copied()
                .map(|port| {
                    world.rpc.finalized_ocomp_activation_on(
                        port,
                        request.request_height,
                        request.intent_id,
                    )
                })
                .collect::<Vec<_>>();
            assert!(activations.iter().all(Option::is_some));
            let activation = activations[0]
                .clone()
                .expect("all finalized activations are present");
            assert!(
                activations
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&activation)),
                "validators expose different finalized activations"
            );
            world.state.ocomp_activation = Some(activation);
            world.state.ocomp_vote_accountability = Some(accountability);
            world.state.ocomp_finality_before_fault = Some(
                world
                    .rpc
                    .finalized_result(primary)
                    .expect("capture finalized height before releasing held OCOMP result"),
            );
            return;
        }
        assert!(
            Instant::now() < timeout,
            "three validators did not finalize the OCOMP quorum while validator-3 was held"
        );
        sleep(Duration::from_millis(250));
    }
}

#[when("validator 3 OCOMP worker restarts after the finalized quorum")]
fn restart_late_worker_after_quorum(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("canonical activation");
    let job_id = activation.job_id;
    let canonical_result = std::fs::read(local_result_path(world, 0, job_id))
        .expect("read the quorum participant's canonical result");
    let result = LysisResultV1::decode_canonical(&canonical_result, &poc_schema_limits())
        .expect("decode the quorum participant's canonical result");
    assert_eq!(result.job_id, job_id);
    assert_eq!(
        result.result_digest(&poc_schema_limits()).unwrap(),
        activation.result_digest
    );
    assert_eq!(
        world
            .state
            .ocomp_vote_accountability
            .as_ref()
            .unwrap()
            .quorum_result_digest,
        Some(activation.result_digest)
    );
    let (cas, reference) = late_validator_result_cas(world, &canonical_result);
    assert!(
        read_pending_late_result(&cas, &reference).is_none(),
        "held worker must not have already produced this job's exact result"
    );
    assert!(!local_result_path(world, 3, job_id)
        .try_exists()
        .expect("observe held validator's local result path"));
    let node_dir = world.validators.data_dir(3);
    let node_log = crate::internal::launch_log::LaunchLog::checkpoint(
        &node_dir
            .parent()
            .expect("validator slot directory")
            .join("node.log"),
    )
    .expect("arm the existing node log before the late worker starts");
    let (node_pid, status) = world
        .localnet
        .owned_validator_process(3)
        .expect("capture exact live validator-3 incarnation");
    assert!(status.is_none());
    world
        .ocomp
        .restart_worker(3, 0)
        .expect("restart validator-3 Worker after finalized quorum");
    let worker_pid = late_validator_worker_pid(world);
    world.state.ocomp_late_validator_result =
        Some(crate::world::state::LateValidatorResultObservation {
            node_pid,
            worker_pid,
            node_log,
            canonical_result,
        });
}

fn late_validator_worker_pid(world: &World) -> u32 {
    let workers = world
        .ocomp
        .process_records()
        .iter()
        .filter(|record| {
            record.validator_index == Some(3)
                && record.role == OcompProcessRole::Worker
                && record.worker_ordinal == Some(0)
                && record.stopped_at_millis.is_none()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        workers.len(),
        1,
        "late worker must have one exact owned incarnation"
    );
    workers[0].pid
}

fn late_validator_result_cas(
    world: &World,
    canonical_result: &[u8],
) -> (
    outbe_ocomp::cas::FilesystemCasReader,
    outbe_ocomp_protocol::CasObjectRefV1,
) {
    let encoded_bytes = u64::try_from(canonical_result.len()).expect("canonical result length");
    let cas = outbe_ocomp::cas::FilesystemCasReader::open(
        world
            .ocomp
            .domain_root(3)
            .expect("validator-3 domain")
            .join("cas-v1"),
        outbe_ocomp::cas::CasLimits {
            // This observer reads one exact expected object, not a job-data quota.
            max_object_bytes: encoded_bytes,
            max_total_bytes: u64::MAX,
        },
    )
    .expect("open the held validator's existing CAS read-only");
    let reference = outbe_ocomp_protocol::CasObjectRefV1 {
        transport_digest: alloy_primitives::keccak256(canonical_result),
        encoded_bytes,
        expected_ocb1_kind: None,
    };
    (cas, reference)
}

fn read_pending_late_result(
    cas: &outbe_ocomp::cas::FilesystemCasReader,
    reference: &outbe_ocomp_protocol::CasObjectRefV1,
) -> Option<Vec<u8>> {
    match cas.read_verified(reference) {
        Ok(object) => Some(object.bytes().to_vec()),
        Err(outbe_ocomp::cas::CasError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => panic!("late result CAS observation failed: {error}"),
    }
}

// Completed quorum evidence is immutable, but an unfilled participant slot
// remains open until the exclusive response deadline. Build the exact expected
// projection from the pre-release snapshot and the canonical accepted vote.
pub(in crate::features::ocomp) fn late_result_expected_accountability(
    baseline: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    late_vote: Option<(u64, Vec<u8>)>,
    height: u64,
    deadline: u64,
) -> eyre::Result<crate::world::rpc::OcompPublicVoteAccountabilityV1> {
    ensure!(baseline.slot_validator_indexes == [0, 1, 2]);
    ensure!(baseline.member_count == 4 && baseline.quorum_threshold == 3);
    ensure!(baseline.closed_height.is_none());
    let mut expected = baseline.clone();
    if let Some((inclusion, signature)) = late_vote {
        ensure!(
            baseline
                .quorum_height
                .is_some_and(|quorum| inclusion > quorum)
                && inclusion < deadline
                && inclusion <= height,
            "late vote must be finalized after quorum and before the exclusive deadline"
        );
        expected.slot_validator_indexes.push(3);
        expected.slot_first_signatures.push((3, signature));
    }
    if height >= deadline {
        let timely = if expected.slot_validator_indexes.len() == 4 {
            0x0f
        } else {
            0x07
        };
        expected.closed_height = Some(deadline);
        expected.timely_bitmap = Some(vec![timely]);
        expected.matching_bitmap = Some(vec![timely]);
        expected.missing_bitmap = Some(vec![0x0f ^ timely]);
        expected.divergent_bitmap = Some(vec![0]);
        expected.equivocation_bitmap = Some(vec![0]);
    }
    Ok(expected)
}

type LateObservation = crate::world::state::LateValidatorResultObservation;

fn assert_late_owner_live(world: &mut World, observation: &LateObservation) {
    let (pid, status) = world
        .localnet
        .owned_validator_process(3)
        .expect("observe exact validator-3 incarnation");
    assert_eq!(
        pid, observation.node_pid,
        "validator-3 must not restart to hide failure"
    );
    assert!(status.is_none());
    world
        .ocomp
        .ensure_worker_alive(3, 0)
        .expect("late worker remains alive");
    assert_eq!(late_validator_worker_pid(world), observation.worker_pid);
}

fn late_result_discarded(log: &str, job_id: B256) -> bool {
    log.lines().any(|line| {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        line.contains("ignored late OCOMP result before local persistence")
            && fields.contains(&format!("job_id={job_id}").as_str())
            && fields.contains(&"reason=\"checkpoint_pruned\"")
    })
}

fn late_result_persisted(path: &Path, canonical_result: &[u8]) -> bool {
    match std::fs::read(path) {
        Ok(bytes) => {
            assert_eq!(
                bytes, canonical_result,
                "late local result is not canonical"
            );
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => panic!("cannot observe late local result: {error}"),
    }
}

/// Wait until validator-3 computes the exact late result and either persists
/// it or discards it as checkpoint-pruned.
fn wait_late_disposition(
    world: &mut World,
    observation: &mut LateObservation,
    job_id: B256,
) -> &'static str {
    let (cas, reference) = late_validator_result_cas(world, &observation.canonical_result);
    let result_path = local_result_path(world, 3, job_id);
    let timeout = Instant::now() + Duration::from_secs(600);
    loop {
        world
            .localnet
            .ensure_committee_alive()
            .expect("late local OCOMP result must not shut down validator-3");
        assert_late_owner_live(world, observation);
        let log = observation
            .node_log
            .read()
            .expect("read exact post-release node log");
        let discarded = late_result_discarded(&log, job_id);
        let persisted = late_result_persisted(&result_path, &observation.canonical_result);
        if let Some(bytes) = read_pending_late_result(&cas, &reference) {
            assert_eq!(
                bytes, observation.canonical_result,
                "late compute output differs from quorum"
            );
            if persisted {
                return "persisted";
            }
            if discarded {
                return "checkpoint_pruned";
            }
        }
        assert!(
            Instant::now() < timeout,
            "validator-3 did not produce and safely handle the exact late result"
        );
        sleep(Duration::from_millis(250));
    }
}

fn assert_late_vote_bound(
    vote: &ResultVoteV1,
    request: &crate::world::rpc::OcompPublicJobRequestV1,
    baseline: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    canonical_result: &LysisResultV1,
) {
    assert_eq!(vote.result, *canonical_result);
    assert_eq!(vote.attempt, request.attempt);
    assert_eq!(
        vote.result_validator_set_epoch,
        baseline.result_validator_set_epoch
    );
    assert_eq!(
        vote.result_committee_set_hash,
        baseline.result_committee_set_hash
    );
    assert_eq!(
        vote.result_ocomp_binding_hash,
        baseline.result_ocomp_binding_hash
    );
}

fn canonical_late_vote(
    world: &World,
    request: &crate::world::rpc::OcompPublicJobRequestV1,
    job_id: B256,
    canonical_result: &[u8],
    (from, to): (u64, u64),
) -> Option<(u64, Vec<u8>)> {
    let primary = world.validators.primary_port();
    let baseline = world
        .state
        .ocomp_vote_accountability
        .as_ref()
        .expect("pre-release canonical votes");
    let delegate = world
        .ocomp
        .ocomp_delegate_address(3)
        .expect("validator-3 delegate");
    let canonical_result = LysisResultV1::decode_canonical(canonical_result, &poc_schema_limits())
        .expect("decode exact late result");
    let transactions = world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(primary, from, to)
        .expect("enumerate canonical post-release votes and matching receipts");
    let mut late_vote = None;
    for transaction in transactions
        .iter()
        .filter(|tx| tx.success && tx.signer == delegate)
    {
        let bytes = world
            .rpc
            .ocomp_result_vote_bytes_on(primary, transaction.transaction_hash)
            .expect("read canonical validator-3 vote");
        let vote = ResultVoteV1::decode_canonical(&bytes, &poc_schema_limits())
            .expect("decode canonical validator-3 vote");
        if vote.job_id != job_id {
            continue;
        }
        assert_late_vote_bound(&vote, request, baseline, &canonical_result);
        assert!(transaction.block_number >= request.open_height);
        assert!(
            transaction.block_number < request.deadline_height,
            "validator-3 vote succeeded at or after the exclusive deadline"
        );
        // The first accepted signature owns the slot. Later identical retries
        // cannot replace it. Canonical block enumeration is height ordered.
        late_vote.get_or_insert((transaction.block_number, vote.signature_rs.to_vec()));
    }
    late_vote
}

fn assert_completed_job_unchanged(
    world: &World,
    request: &crate::world::rpc::OcompPublicJobRequestV1,
    job_id: B256,
    (finalized_before, height): (u64, u64),
    expected_accountability: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
) {
    let baseline = world
        .state
        .ocomp_vote_accountability
        .as_ref()
        .expect("pre-release canonical votes");
    for port in world.validators.committee_ports() {
        let before = world
            .rpc
            .ocomp_job_record_at_on(port, request.intent_id, finalized_before)
            .expect("read completed job at the pre-release finalized block");
        let after = world
            .rpc
            .ocomp_job_record_at_on(port, request.intent_id, height)
            .expect("read completed job at the post-result common finalized block");
        assert_eq!(before.status, OcompJobStatus::Completed);
        assert_eq!(
            after, before,
            "late compute changed the canonical completed job"
        );
        let accountability = world
            .rpc
            .ocomp_vote_accountability_at_on(port, job_id, height)
            .expect("read post-result votes at the common finalized block");
        assert!(completed_accountability_is_preserved(
            baseline,
            &accountability
        ));
        assert_eq!(
            accountability, *expected_accountability,
            "late result may only add validator-3's canonical timely matching vote and deadline summary"
        );
    }
}

#[then("validator 3 safely handles its correct late result without changing the canonical outcome or quorum")]
fn late_local_result_is_not_fatal(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .clone()
        .expect("finalized OCOMP activation");
    let job_id = activation.job_id;
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("canonical request");
    let mut observation = world
        .state
        .ocomp_late_validator_result
        .take()
        .expect("late result observer was armed before restarting the worker");
    let primary = world.validators.primary_port();
    let finalized_before = world
        .state
        .ocomp_finality_before_fault
        .expect("finality captured before releasing validator-3 Worker");
    let disposition = wait_late_disposition(world, &mut observation, job_id);

    // Require progress after observing the late disposition, not progress that
    // happened while the worker was still stopped.
    let target = world
        .rpc
        .finalized_result(primary)
        .expect("read finality after the late result disposition")
        .max(finalized_before)
        .checked_add(3)
        .expect("post-result finality target");
    let checkpoint = wait_for_common_finalized_checkpoint(world, target, "late validator result");
    let late_vote = canonical_late_vote(
        world,
        &request,
        job_id,
        &observation.canonical_result,
        (finalized_before + 1, checkpoint.height),
    );
    let expected_accountability = late_result_expected_accountability(
        world
            .state
            .ocomp_vote_accountability
            .as_ref()
            .expect("pre-release canonical votes"),
        late_vote,
        checkpoint.height,
        request.deadline_height,
    )
    .expect("exact late-vote and natural deadline expectations");
    assert_completed_job_unchanged(
        world,
        &request,
        job_id,
        (finalized_before, checkpoint.height),
        &expected_accountability,
    );
    world
        .localnet
        .ensure_committee_alive()
        .expect("committee remains live after result handling");
    world
        .ocomp
        .ensure_worker_alive(3, 0)
        .expect("late worker remains live after finalized progress");
    assert_eq!(late_validator_worker_pid(world), observation.worker_pid);
    let (pid, status) = world.localnet.owned_validator_process(3).unwrap();
    assert_eq!(pid, observation.node_pid);
    assert!(status.is_none());
    eprintln!("E2E_OCOMP_LATE_RESULT job_id={job_id} result_digest={} disposition={disposition} finalized_height={}",
        activation.result_digest, checkpoint.height);

    assert!(
        !world
            .localnet
            .log_has(3, "unexpected embedded OCOMP local-result action")
            .expect("read required owned process log"),
        "validator-3 treated the protocol-owned late result as fatal"
    );
    assert!(
        !world
            .localnet
            .log_has(3, "embedded OCOMP requested local node shutdown")
            .expect("read required owned process log"),
        "validator-3 requested shutdown after its late local result"
    );
}
