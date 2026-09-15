use crate::features::ocomp::*;

#[when("validator 2 prepares one valid vote without broadcasting it")]
fn validator_two_prepares_held_vote(world: &mut World) {
    const VALIDATOR_INDEX: usize = 2;

    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(180);
    let vote_bytes = loop {
        let finalized_height = world
            .rpc
            .finalized_result(primary)
            .expect("observe finalized height while waiting for public result vote");
        let public_votes = world
            .rpc
            .finalized_ocomp_result_vote_transactions_on(
                primary,
                request.request_height,
                finalized_height,
            )
            .unwrap_or_default();
        if let Some(transaction) = public_votes.iter().find(|transaction| transaction.success) {
            break world
                .rpc
                .ocomp_result_vote_bytes_on(primary, transaction.transaction_hash)
                .expect("decode one finalized public ResultVoteV1");
        }
        assert!(
            Instant::now() < timeout,
            "no valid public result vote became available for held-vote preparation"
        );
        sleep(Duration::from_millis(250));
    };
    let vote = ResultVoteV1::decode_canonical(&vote_bytes, &poc_schema_limits())
        .expect("canonical public ResultVoteV1");
    let address = world
        .ocomp
        .ocomp_delegate_address(VALIDATOR_INDEX as u8)
        .expect("derive validator-2 OCOMP delegate address");
    let nonce = world
        .rpc
        .canonical_nonce_on(primary, address)
        .expect("read validator-2 canonical nonce");
    let max_fee_per_gas = world
        .rpc
        .gas_price_on(primary)
        .expect("read public gas price")
        .max(MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS);
    assert!(
        world
            .rpc
            .head(primary)
            .expect("primary head before preparing held vote")
            < request.deadline_height,
        "held vote was not prepared before the exclusive deadline"
    );
    let prepared = world
        .ocomp
        .prepare_held_vote_transaction(
            VALIDATOR_INDEX as u8,
            vote,
            nonce,
            max_fee_per_gas,
            OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
        )
        .expect("production node prepares validator-2 vote without exposing its key");
    world.state.ocomp_held_late_vote_hash = Some(prepared.transaction_hash);
    world.state.ocomp_held_late_vote_raw = Some(prepared.raw_transaction.0);
}

#[when("the held validator vote is broadcast at the exclusive deadline")]
fn held_vote_is_broadcast_at_deadline(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let primary = world.validators.primary_port();
    let target_parent = request
        .deadline_height
        .checked_sub(1)
        .expect("deadline follows genesis");
    let timeout = Instant::now() + Duration::from_secs(180);
    while world
        .rpc
        .head(primary)
        .expect("primary head while approaching exclusive vote deadline")
        < target_parent
    {
        assert!(
            Instant::now() < timeout,
            "chain did not approach the exclusive vote deadline"
        );
        sleep(Duration::from_millis(50));
    }

    let raw = world
        .state
        .ocomp_held_late_vote_raw
        .take()
        .expect("locally signed held vote transaction");
    let expected_hash = world
        .state
        .ocomp_held_late_vote_hash
        .expect("held vote transaction hash");
    let submitted_hash = world
        .rpc
        .send_raw_transaction_on(primary, &raw)
        .expect("broadcast held vote through public RPC");
    assert_eq!(
        submitted_hash
            .parse::<B256>()
            .expect("public transaction hash"),
        expected_hash,
        "public RPC changed the locally signed held transaction identity"
    );
    let receipt_timeout = Instant::now() + Duration::from_secs(60);
    let receipt = loop {
        if let Some(receipt) = world
            .rpc
            .transaction_receipt(&submitted_hash, primary)
            .filter(|receipt| receipt.get("blockNumber").is_some())
        {
            break receipt;
        }
        assert!(
            Instant::now() < receipt_timeout,
            "held vote did not receive a mined public receipt"
        );
        sleep(Duration::from_millis(100));
    };
    assert_eq!(
        receipt.get("status").and_then(serde_json::Value::as_str),
        Some("0x0"),
        "a vote included at or after the exclusive deadline must revert"
    );
    let inclusion_height = world
        .rpc
        .receipt_block_number(&submitted_hash, primary)
        .expect("held vote inclusion height");
    assert!(
        inclusion_height >= request.deadline_height,
        "held vote was included before the intended exclusive boundary: \
         inclusion={inclusion_height}, deadline={}",
        request.deadline_height
    );
    assert!(
        world
            .rpc
            .wait_finalized_at_least(primary, inclusion_height, 60),
        "held deadline-vote receipt did not finalize"
    );
    world.state.ocomp_late_vote_reverted = Some(true);
    world.state.ocomp_late_vote_inclusion_height = Some(inclusion_height);
}

#[when("validators 2 and 3 OCOMP workers are stopped before the job")]
fn stop_two_workers_before_job(world: &mut World) {
    capture_ocomp_finality_before_fault(world, "stopping OCOMP workers");
    for validator_index in [2, 3] {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopWorker {
                validator_index,
                worker_ordinal: 0,
            })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Worker before the job: {error}")
            });
    }
}

#[when("all four OCOMP snapshot exporters are stopped before the job")]
fn stop_all_exporters_before_job(world: &mut World) {
    capture_ocomp_finality_before_fault(world, "stopping all OCOMP snapshot exporters");
    for validator_index in 0..4 {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopSnapshotExporter { validator_index })
            .unwrap_or_else(|error| {
                panic!(
                    "stop validator-{validator_index} SnapshotExporter before the job: {error:#}"
                )
            });
    }
}

#[when(
    "all four OCOMP workers stop before voting opens and exporters independently materialize the public JobIntent"
)]
fn stop_workers_before_independent_public_exports(world: &mut World) {
    use crate::internal::ocomp_worker_outage::{require_pre_open_cut, WorkerOutageEvidence};
    world
        .ocomp
        .ensure_baseline_runtime_ready(1)
        .expect("all four workers live and connected before observing the job");
    let ports = world.validators.committee_ports();
    let current_finalized = ports
        .iter()
        .map(|&port| world.rpc.finalized_result(port))
        .collect::<eyre::Result<Vec<_>>>()
        .expect("read current finalized heights before worker outage")
        .into_iter()
        .min()
        .expect("four-validator committee");
    world.state.ocomp_finality_before_fault = Some(
        world
            .rpc
            .wait_finalized_checkpoint(&ports, current_finalized, 60)
            .expect("current common finalized checkpoint before worker outage")
            .height,
    );
    world.state.ocomp_worker_outage = Some(WorkerOutageEvidence::default());
    world
        .ocomp
        .stop_worker_cohort(world.state.ocomp_worker_outage.as_mut().unwrap())
        .expect("stop and reap all four owned workers before observing exports");
    // Capture the cut immediately after reaping. Later export publication cannot
    // move this boundary; its independence from workers is the property tested.
    let heads = world
        .validators
        .committee_ports()
        .into_iter()
        .map(|port| {
            let raw = eth::raw_json_result(
                &world.rpc.url(port),
                "eth_blockNumber",
                serde_json::json!([]),
            )
            .expect("successful canonical HEAD after worker reap");
            serde_json::from_value::<alloy_primitives::U64>(raw)
                .expect("canonical HEAD number")
                .to::<u64>()
        })
        .collect::<Vec<_>>();
    world.state.ocomp_worker_outage.as_mut().unwrap().cut_heads = heads.clone();
    metadosis_creates_finalized_job_intent(world);
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized zero-vote JobIntent");
    require_pre_open_cut(&heads, request.open_height)
        .expect("all four workers must exit before voting opens");
    let primary = world.validators.primary_port();
    let record = world
        .rpc
        .ocomp_job_record_at_on(primary, request.intent_id, request.finality_recorded_height)
        .expect("canonical bound job for exact export verification");
    assert_eq!(record.finalized.as_ref().unwrap().job_id, request.job_id);
    let checkpoint = world
        .rpc
        .checkpoint_at(primary, request.request_height)
        .expect("canonical request checkpoint");
    assert_eq!(checkpoint.block_hash, request.request_block_hash);
    let bundle = world
        .ocomp
        .canonical_fork_install()
        .expect("canonical OCOMP bundle")
        .protocol_bundle;
    let opening_checkpoint =
        wait_for_common_finalized_checkpoint(world, request.open_height, "worker outage opening");
    assert_eq!(opening_checkpoint.height, request.open_height);
    world
        .ocomp
        .wait_for_exports_while_workers_stopped(
            &record,
            checkpoint,
            &bundle,
            world.state.ocomp_worker_outage.as_mut().unwrap(),
            Duration::from_secs(120),
            || world.localnet.ensure_committee_alive(),
        )
        .unwrap_or_else(|error| {
            panic!("exact exports while the complete worker cohort remains stopped: {error:#}")
        });
    for port in world.validators.committee_ports() {
        // Accountability is created at open, not at the earlier worker cut.
        let accountability = world
            .rpc
            .ocomp_vote_accountability_at_on(port, request.job_id, opening_checkpoint.height)
            .expect("canonical post-fault accountability");
        assert!(accountability.slot_validator_indexes.is_empty());
        assert!(accountability.quorum_result_digest.is_none());
    }
}

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
pub(super) fn late_result_expected_accountability(
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
    let (cas, reference) = late_validator_result_cas(world, &observation.canonical_result);
    let result_path = local_result_path(world, 3, job_id);
    let primary = world.validators.primary_port();
    let finalized_before = world
        .state
        .ocomp_finality_before_fault
        .expect("finality captured before releasing validator-3 Worker");
    let timeout = Instant::now() + Duration::from_secs(600);

    let disposition;
    loop {
        world
            .localnet
            .ensure_committee_alive()
            .expect("late local OCOMP result must not shut down validator-3");
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
        let log = observation
            .node_log
            .read()
            .expect("read exact post-release node log");
        let discarded = log.lines().any(|line| {
            line.contains("ignored late OCOMP result before local persistence")
                && line
                    .split_ascii_whitespace()
                    .any(|field| field == format!("job_id={job_id}"))
                && line
                    .split_ascii_whitespace()
                    .any(|field| field == "reason=\"checkpoint_pruned\"")
        });
        let persisted = match std::fs::read(&result_path) {
            Ok(bytes) => {
                assert_eq!(
                    bytes, observation.canonical_result,
                    "late local result is not canonical"
                );
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => panic!("cannot observe late local result: {error}"),
        };
        if let Some(bytes) = read_pending_late_result(&cas, &reference) {
            assert_eq!(
                bytes, observation.canonical_result,
                "late compute output differs from quorum"
            );
            if persisted || discarded {
                disposition = if persisted {
                    "persisted"
                } else {
                    "checkpoint_pruned"
                };
                break;
            }
        }
        assert!(
            Instant::now() < timeout,
            "validator-3 did not produce and safely handle the exact late result"
        );
        sleep(Duration::from_millis(250));
    }

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
    let baseline_accountability = world
        .state
        .ocomp_vote_accountability
        .as_ref()
        .expect("pre-release canonical votes");
    let delegate = world
        .ocomp
        .ocomp_delegate_address(3)
        .expect("validator-3 delegate");
    let canonical_result =
        LysisResultV1::decode_canonical(&observation.canonical_result, &poc_schema_limits())
            .expect("decode exact late result");
    let transactions = world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(
            primary,
            finalized_before + 1,
            checkpoint.height,
        )
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
        assert_eq!(vote.result, canonical_result);
        assert_eq!(vote.attempt, request.attempt);
        assert_eq!(
            vote.result_validator_set_epoch,
            baseline_accountability.result_validator_set_epoch
        );
        assert_eq!(
            vote.result_committee_set_hash,
            baseline_accountability.result_committee_set_hash
        );
        assert_eq!(
            vote.result_ocomp_binding_hash,
            baseline_accountability.result_ocomp_binding_hash
        );
        assert!(transaction.block_number >= request.open_height);
        assert!(
            transaction.block_number < request.deadline_height,
            "validator-3 vote succeeded at or after the exclusive deadline"
        );
        // The first accepted signature owns the slot; later identical retries
        // cannot replace it. Canonical block enumeration is height ordered.
        if late_vote.is_none() {
            late_vote = Some((transaction.block_number, vote.signature_rs.to_vec()));
        }
    }
    let expected_accountability = late_result_expected_accountability(
        baseline_accountability,
        late_vote,
        checkpoint.height,
        request.deadline_height,
    )
    .expect("exact late-vote and natural deadline expectations");
    for port in world.validators.committee_ports() {
        let before = world
            .rpc
            .ocomp_job_record_at_on(port, request.intent_id, finalized_before)
            .expect("read completed job at the pre-release finalized block");
        let after = world
            .rpc
            .ocomp_job_record_at_on(port, request.intent_id, checkpoint.height)
            .expect("read completed job at the post-result common finalized block");
        assert_eq!(before.status, OcompJobStatus::Completed);
        assert_eq!(
            after, before,
            "late compute changed the canonical completed job"
        );
        let accountability = world
            .rpc
            .ocomp_vote_accountability_at_on(port, job_id, checkpoint.height)
            .expect("read post-result votes at the common finalized block");
        assert!(completed_accountability_is_preserved(
            baseline_accountability,
            &accountability
        ));
        assert_eq!(
            accountability, expected_accountability,
            "late result may only add validator-3's canonical timely matching vote and deadline summary"
        );
    }
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

#[when("validators 1, 2 and 3 OCOMP workers are stopped before the job")]
fn stop_three_workers_before_job(world: &mut World) {
    for validator_index in [1, 2, 3] {
        world
            .ocomp
            .apply_process_fault(OcompProcessFault::StopWorker {
                validator_index,
                worker_ordinal: 0,
            })
            .unwrap_or_else(|error| {
                panic!("stop validator-{validator_index} Worker before the job: {error}")
            });
    }
}

#[when("one valid vote is finalized and a changed-binding vote is submitted")]
fn one_valid_then_changed_binding_vote(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized single-voter JobIntent");
    let ports = world.validators.committee_ports();
    let primary = world.validators.primary_port();
    let timeout = Instant::now() + Duration::from_secs(180);
    let (job_id, accountability) = loop {
        let records = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_record_on(port, request.intent_id)
            })
            .collect::<Vec<_>>();
        if records.iter().all(|record| {
            record
                .as_ref()
                .is_some_and(|record| record.status == OcompJobStatus::VotingOpen)
        }) {
            let record = records[0].clone().expect("all records are present");
            assert!(
                records
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&record)),
                "validators expose different single-voter job state"
            );
            let job_id = record
                .finalized
                .as_ref()
                .expect("finalized single-voter job")
                .job_id;
            let accountability = world
                .rpc
                .finalized_ocomp_vote_accountability_on(primary, job_id);
            if accountability
                .as_ref()
                .is_some_and(|value| value.slot_validator_indexes == [0])
            {
                eprintln!(
                    "OCOMP_PUBLIC_MUTATION stage=single_vote_observed finalized_height={:?}",
                    world.rpc.finalized(primary)
                );
                break (job_id, accountability.expect("checked above"));
            }
        }
        assert!(
            Instant::now() < timeout,
            "validator-0 did not finalize the sole public result vote before mutation"
        );
        sleep(Duration::from_millis(250));
    };
    assert_eq!(accountability.quorum_result_digest, None);

    let finalized_height = world.rpc.finalized(primary).expect("finalized height");
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=scan_public_votes from={} to={finalized_height}",
        request.request_height
    );
    let public_votes = world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(
            primary,
            request.request_height,
            finalized_height,
        )
        .expect("enumerate the sole finalized public vote");
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=scan_public_votes_done observed={}",
        public_votes.len()
    );
    let successful = public_votes
        .iter()
        .filter(|transaction| transaction.success)
        .collect::<Vec<_>>();
    assert_eq!(successful.len(), 1, "expected one successful public vote");
    let vote_bytes = world
        .rpc
        .ocomp_result_vote_bytes_on(primary, successful[0].transaction_hash)
        .expect("decode sole public ResultVoteV1");
    let mut mutated = ResultVoteV1::decode_canonical(&vote_bytes, &poc_schema_limits())
        .expect("canonical sole ResultVoteV1");
    assert_eq!(mutated.job_id, job_id);
    let delegate_key = world
        .ocomp
        .ocomp_delegate_private_key_for_vote(&mutated)
        .expect("sole vote OCOMP delegate key");
    mutated.job_id = B256::repeat_byte(0x5a);
    eprintln!("OCOMP_PUBLIC_MUTATION stage=submit_changed_binding");
    let mutation = world.rpc.submit_ocomp_result_vote_bytes(
        primary,
        &delegate_key,
        mutated
            .encode_canonical(&poc_schema_limits())
            .expect("canonical changed-binding payload"),
    );
    let mutation_height = match mutation {
        Ok(mutation_hash) => {
            eprintln!("OCOMP_PUBLIC_MUTATION stage=changed_binding_receipt tx={mutation_hash}");
            let mutation_receipt = world
                .rpc
                .transaction_receipt(&mutation_hash, primary)
                .expect("changed-binding vote receipt");
            assert_eq!(
                mutation_receipt
                    .get("status")
                    .and_then(serde_json::Value::as_str),
                Some("0x0"),
                "changed-binding vote must revert"
            );
            Some(
                world
                    .rpc
                    .receipt_block_number(&mutation_hash, primary)
                    .expect("changed-binding inclusion height"),
            )
        }
        Err(error) => {
            assert!(
                error
                    .to_string()
                    .contains("OCOMP carrier signer is not authorized for this action"),
                "changed-binding vote failed for an unexpected reason: {error:#}"
            );
            None
        }
    };
    world.state.ocomp_non_quorum_changed_binding_reverted = Some(true);
    if let Some(mutation_height) = mutation_height {
        eprintln!(
            "OCOMP_PUBLIC_MUTATION stage=wait_changed_binding_finality inclusion_height={mutation_height}"
        );
        assert!(
            world
                .rpc
                .wait_finalized_at_least(primary, mutation_height, 60),
            "changed-binding vote did not finalize"
        );
    }
    eprintln!(
        "OCOMP_PUBLIC_MUTATION stage=changed_binding_finalized finalized_height={:?}",
        world.rpc.finalized(primary)
    );

    for port in ports {
        let after = world
            .rpc
            .finalized_ocomp_job_record_on(port, request.intent_id)
            .expect("job after changed-binding vote");
        assert_eq!(after.status, OcompJobStatus::VotingOpen);
        assert!(after.terminal.is_none());
        assert_eq!(
            world
                .rpc
                .finalized_ocomp_vote_accountability_on(port, job_id)
                .expect("accountability after changed-binding vote"),
            accountability
        );
        assert_eq!(
            world.rpc.nod_certified_generation_exists_on(
                port,
                request.worldwide_day,
                request.request_height,
            ),
            Some(false)
        );
    }
    world.state.ocomp_non_quorum_state_unchanged = Some(true);
}

#[when("the three stopped workers restart and form the remaining quorum")]
fn restart_three_workers_for_quorum(world: &mut World) {
    for validator_index in [1, 2, 3] {
        world
            .ocomp
            .restart_worker(validator_index, 0)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Worker for quorum: {error}")
            });
    }
}

#[then("the no-quorum job expires at its exclusive deadline without creating Nod")]
fn no_quorum_job_expires_without_nod(world: &mut World) {
    assert_job_expires_without_nod(world, &[0, 1], 0b0011, 0b1100, true, "two-vote");
}

#[then("the unexported zero-vote job expires and reaches Released without an ACK")]
fn unexported_zero_vote_job_expires_without_halt(world: &mut World) {
    assert_job_expires_without_nod(world, &[], 0, 0b1111, false, "zero-vote unexported");
}

#[then("the exported zero-vote job expires at its exclusive deadline and finality continues")]
fn exported_zero_vote_job_expires_without_halt(world: &mut World) {
    assert_job_expires_without_nod(world, &[], 0, 0b1111, true, "zero-vote exported");
    world
        .ocomp
        .ensure_worker_cohort_stopped(
            world
                .state
                .ocomp_worker_outage
                .as_ref()
                .expect("owned worker outage evidence"),
        )
        .expect("the same worker cohort remains stopped through expiry and retention release");
}

#[when("the stopped OCOMP workers restart after canonical expiry")]
fn stopped_workers_restart_after_expiry(world: &mut World) {
    for validator_index in [2, 3] {
        world
            .ocomp
            .restart_worker(validator_index, 0)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Worker after canonical expiry: {error}")
            });
    }
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("all OCOMP roles are live after canonical expiry");
}

#[when("all stopped OCOMP snapshot exporters restart after canonical expiry")]
fn all_exporters_restart_after_expiry(world: &mut World) {
    for validator_index in 0..4 {
        world
            .ocomp
            .restart_snapshot_exporter(validator_index)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} SnapshotExporter: {error:#}")
            });
    }
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("all OCOMP roles are live after canonical expiry");
}

#[when("all stopped OCOMP workers restart after canonical expiry")]
fn all_workers_restart_after_expiry(world: &mut World) {
    world
        .ocomp
        .ensure_worker_cohort_stopped(
            world
                .state
                .ocomp_worker_outage
                .as_ref()
                .expect("owned worker outage evidence"),
        )
        .expect("no worker incarnation restarted before the explicit recovery step");
    for validator_index in 0..4 {
        world
            .ocomp
            .restart_worker(validator_index, 0)
            .unwrap_or_else(|error| {
                panic!("restart validator-{validator_index} Worker: {error:#}")
            });
    }
    world
        .ocomp
        .ensure_baseline_runtime_ready(1)
        .expect("all four workers reconnect to their embedded supervisors after expiry");
}

#[then("the expired job remains terminal with no successor after process recovery")]
fn expired_job_remains_terminal_without_successor(world: &mut World) {
    let original = world
        .state
        .ocomp_job_request
        .clone()
        .expect("expired public JobIntent");
    let ports = world.validators.committee_ports();
    let current = ports
        .iter()
        .copied()
        .map(|port| {
            world
                .rpc
                .finalized_result(port)
                .expect("validator finality after recovery")
        })
        .collect::<Vec<_>>();
    let target = current
        .into_iter()
        .max()
        .expect("four validator finality observations")
        .saturating_add(2);
    wait_for_common_finalized_checkpoint(world, target, "post-expiry process recovery");

    let mut expired_job_id = None;
    for port in ports {
        let retained = world
            .rpc
            .ocomp_job_record_at_on(port, original.intent_id, target)
            .unwrap_or_else(|error| {
                panic!("read expired job at post-recovery h{target} on port {port}: {error:#}")
            });
        assert_eq!(retained.status, OcompJobStatus::Expired);
        assert_eq!(
            retained
                .terminal
                .as_ref()
                .expect("expired terminal remains present")
                .outcome,
            OcompTerminalOutcome::Expired
        );
        let observed_job_id = retained
            .finalized
            .as_ref()
            .expect("expired record keeps finalized job identity")
            .job_id;
        if let Some(expected_job_id) = expired_job_id {
            assert_eq!(
                observed_job_id, expected_job_id,
                "validators disagree on the expired canonical job id"
            );
        } else {
            expired_job_id = Some(observed_job_id);
        }
        let successor = world
            .rpc
            .finalized_ocomp_job_request_for_worldwide_day_result_on(
                port,
                original.request_height.saturating_add(1),
                original.worldwide_day,
            )
            .unwrap_or_else(|error| {
                panic!("prove no successor request on RPC port {port}: {error:#}")
            });
        assert!(
            successor.is_absent(),
            "expired job created a forbidden successor for the same WWD on port {port}: {successor:?}"
        );
    }
    wait_for_released_retention(
        world,
        expired_job_id.expect("expired finalized job id after recovery"),
        world
            .state
            .ocomp_expired_release_had_export
            .expect("released export authority observation"),
        "post-expiry process recovery",
    );
    world
        .localnet
        .ensure_committee_alive()
        .expect("all validators remain live after terminal recovery");
}

#[when("one independent next-day Tribute is submitted after OCOMP recovery")]
fn submit_independent_next_day_tribute_after_recovery(world: &mut World) {
    let original = world
        .state
        .ocomp_job_request
        .clone()
        .expect("expired public JobIntent");
    let original_wwd = WorldwideDay::new(original.worldwide_day);
    let followup_wwd = WorldwideDay::from_timestamp(
        original_wwd
            .start_timestamp()
            .checked_add(86_400)
            .expect("next WorldwideDay timestamp"),
    )
    .value();
    assert_ne!(followup_wwd, original.worldwide_day);

    let ports = world.validators.committee_ports();
    let primary = world.validators.primary_port();
    let states = ports
        .iter()
        .copied()
        .map(|port| world.rpc.metadosis_wwd_state_on(port, followup_wwd))
        .collect::<Vec<_>>();
    let schedule = states[0]
        .clone()
        .expect("recovery fixture exposes its independent next-day WorldwideDay");
    assert_eq!(
        schedule.status, 2,
        "independent next-day WorldwideDay must be exactly OFFERING before submission: {schedule:?}"
    );
    assert!(
        states
            .iter()
            .all(|candidate| candidate.as_ref() == Some(&schedule)),
        "validators disagree on the next-day OFFERING state: {states:?}"
    );
    let finalized_height = world
        .rpc
        .finalized_result(primary)
        .expect("read finalized height before the independent Tribute");
    let finalized_timestamp = world
        .rpc
        .block_timestamp(primary, finalized_height)
        .expect("read finalized timestamp before the independent Tribute");
    assert!(
        finalized_timestamp < schedule.offering_end,
        "independent next-day offering already closed before submission: finalized_timestamp={finalized_timestamp}, schedule={schedule:?}"
    );
    for port in &ports {
        let existing = world
            .rpc
            .finalized_ocomp_job_request_for_worldwide_day_result_on(*port, 0, followup_wwd)
            .unwrap_or_else(|error| {
                panic!("prove no pre-seeded next-day JobIntent on RPC port {port}: {error:#}")
            });
        assert!(
            existing.is_absent(),
            "recovery fixture pre-seeded a forbidden next-day JobIntent on RPC port {port}: {existing:?}"
        );
    }

    let offerer = world
        .validators
        .by_name("validator-0")
        .expect("validator-0 next-day Tribute owner")
        .evm_key()
        .expect("validator-0 next-day Tribute key");
    let tribute_tx = world
        .rpc
        .tribute_offer(&offerer, &followup_wwd.to_string())
        .expect("submit independent next-day Tribute");
    assert!(
        world.rpc.wait_successful_receipt(&tribute_tx, 240),
        "independent next-day Tribute transaction failed: {tribute_tx}"
    );
    world
        .projection
        .wait_for_tribute_projection(&tribute_tx, 240)
        .expect("all exporters project the independent next-day Tribute");
    world.state.tribute_tx_hash = Some(tribute_tx);

    let processing_target =
        first_protocol_cycle_at_or_after(world, schedule.scheduled_process_time);
    let _ = restart_committee_at_logical_time(world, processing_target);
    world
        .ocomp
        .ensure_validator_roles_alive()
        .expect("OCOMP roles survive the next-day processing-time restart");

    let request_deadline = Instant::now() + Duration::from_secs(OCOMP_JOB_REQUEST_TIMEOUT_SECS);
    let request = loop {
        let observed = ports
            .iter()
            .copied()
            .map(|port| {
                world
                    .rpc
                    .finalized_ocomp_job_request_for_worldwide_day_result_on(
                        port,
                        original.deadline_height.saturating_add(1),
                        followup_wwd,
                    )
                    .and_then(|observation| observation.into_bound_request())
                    .unwrap_or_else(|error| {
                        panic!(
                            "observe independent next-day OCOMP request on port {port}: {error:#}"
                        )
                    })
            })
            .collect::<Vec<_>>();
        if observed.iter().all(Option::is_some) {
            let first = observed[0]
                .clone()
                .expect("all independent next-day requests are present");
            assert!(
                observed
                    .iter()
                    .all(|candidate| candidate.as_ref() == Some(&first)),
                "validators expose different independent next-day requests: {observed:?}"
            );
            break first;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles remain live while waiting for the next-day request");
        assert!(
            Instant::now() < request_deadline,
            "independent next-day OCOMP request was not finalized: {observed:?}"
        );
        sleep(Duration::from_millis(250));
    };
    assert_eq!(request.worldwide_day, followup_wwd);
    assert_eq!(request.pending_nonce, 0);
    assert_eq!(request.attempt, 0);
    assert_ne!(request.intent_id, original.intent_id);
    assert!(request.request_height > original.deadline_height);
    world.state.ocomp_followup_job_request = Some(request);
}

#[then("the independent OCOMP job completes on every validator")]
fn independent_ocomp_job_completes_on_every_validator(world: &mut World) {
    let original = world
        .state
        .ocomp_job_request
        .clone()
        .expect("expired original JobIntent");
    let followup = world
        .state
        .ocomp_followup_job_request
        .clone()
        .expect("independent next-day JobIntent");
    quorum_applies_lysis_and_creates_nod_for_request(
        world,
        followup.clone(),
        PublicVoteSetExpectation::Exact(&[0, 1, 2, 3]),
    );
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("independent next-day Lysis activation");
    let target = activation.block_number.saturating_add(2);
    wait_for_common_finalized_checkpoint(world, target, "independent next-day completion");
    for port in world.validators.committee_ports() {
        let completed = world
            .rpc
            .ocomp_job_record_at_on(port, followup.intent_id, target)
            .unwrap_or_else(|error| {
                panic!("read completed independent job at h{target} on port {port}: {error:#}")
            });
        assert_eq!(completed.status, OcompJobStatus::Completed);
        let expired = world
            .rpc
            .ocomp_job_record_at_on(port, original.intent_id, target)
            .unwrap_or_else(|error| {
                panic!("read original expired job at h{target} on port {port}: {error:#}")
            });
        assert_eq!(expired.status, OcompJobStatus::Expired);
    }
    world.state.ocomp_followup_completed_finality = Some(target);
    world
        .localnet
        .ensure_committee_alive()
        .expect("all validators remain live after the independent job completes");
}
