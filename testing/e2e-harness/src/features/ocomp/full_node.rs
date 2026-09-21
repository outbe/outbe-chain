use crate::features::ocomp::*;

pub(in crate::features::ocomp) fn full_node_local_result_path(
    world: &World,
    job_id: B256,
) -> std::path::PathBuf {
    local_result_path(world, world.validators.joiner_index(), job_id)
}

fn launch_preserved_keyless_full_node(world: &mut World) {
    let index = world.validators.joiner_index();
    world
        .localnet
        .launch_dcap_full_node(&format!("joiner-full-node-{index}"), index, 0)
        .expect("restart keyless FullNode with its preserved datadir and domain");
}

#[then("the FullNode independently materializes job A without voting")]
fn full_node_materializes_job_a_without_voting(world: &mut World) {
    let request = world
        .state
        .ocomp_dynamic_job_requests
        .first()
        .expect("job A request before FullNode verification");
    let record = dynamic_job_record(world, request);
    let job_id = record
        .finalized
        .as_ref()
        .expect("job A finalized intent")
        .job_id;
    let path = full_node_local_result_path(world, job_id);
    let deadline = Instant::now() + Duration::from_secs(120);
    while !path.is_file() {
        assert!(
            Instant::now() < deadline,
            "keyless FullNode did not publish canonical job A result at {}",
            path.display()
        );
        sleep(Duration::from_millis(250));
    }
    let encoded = std::fs::read(&path).expect("read FullNode canonical job A result");
    let result = LysisResultV1::decode_canonical(&encoded, &poc_schema_limits())
        .expect("decode FullNode canonical job A result");
    assert_eq!(result.job_id, job_id);
    assert_ne!(
        result
            .result_digest(&poc_schema_limits())
            .expect("validate FullNode job A result"),
        B256::ZERO
    );

    assert!(
        !dynamic_vote_submission_path(world, world.validators.joiner_index(), job_id).exists(),
        "keyless FullNode must compute job A without creating a validator vote submission"
    );
}

#[then("the FullNode result for job A matches the canonical quorum result")]
fn full_node_job_a_result_matches_quorum(world: &mut World) {
    let request = world
        .state
        .ocomp_dynamic_job_requests
        .first()
        .expect("job A request");
    let record = dynamic_job_record(world, request);
    let job_id = record
        .finalized
        .as_ref()
        .expect("job A finalized intent")
        .job_id;
    let encoded = std::fs::read(full_node_local_result_path(world, job_id))
        .expect("read persisted FullNode job A result after promotion");
    let result = LysisResultV1::decode_canonical(&encoded, &poc_schema_limits())
        .expect("decode persisted FullNode job A result");
    let local_digest = result
        .result_digest(&poc_schema_limits())
        .expect("validate persisted FullNode job A result");
    let accountability = world
        .rpc
        .finalized_ocomp_vote_accountability_on(world.validators.primary_port(), job_id)
        .expect("job A quorum accountability");
    assert_eq!(accountability.quorum_result_digest, Some(local_digest));
}

#[then("the keyless FullNode verifies the same finalized Nod body through its local proof path")]
fn keyless_full_node_verifies_finalized_nod_body(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("finalized OCOMP activation before FullNode Nod proof read");
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("certified Nod generation before FullNode proof read");
    assert_eq!(generation.nod_count, 1, "single-Tribute proof scenario");

    let primary = world.validators.primary_port();
    let full_node = world.validators.http_port(world.validators.joiner_index());
    assert!(
        world
            .rpc
            .wait_finalized_at_least(full_node, activation.block_number, 60),
        "keyless FullNode did not finalize the canonical Nod activation block"
    );
    assert_eq!(
        world.rpc.state_root(full_node, activation.block_number),
        world.rpc.state_root(primary, activation.block_number),
        "FullNode EVM state root differs at Nod activation"
    );
    assert_eq!(
        world
            .rpc
            .finalized_ocomp_certified_generation_on(full_node, activation),
        Some(generation.clone()),
        "FullNode exposes a different certified Nod generation"
    );

    let tribute_owner_key = world
        .validators
        .by_name("validator-0")
        .expect("public Tribute owner")
        .evm_key()
        .expect("public Tribute owner key");
    let tribute_owner = eth::address_of(&tribute_owner_key).expect("public Tribute owner address");
    let validator_actions = result_nod_actions_on(world, 0, generation.job_id);
    let full_node_actions =
        result_nod_actions_on(world, world.validators.joiner_index(), generation.job_id);
    assert_eq!(full_node_actions, validator_actions);
    let [action] = full_node_actions.as_slice() else {
        panic!("single-Tribute proof scenario must materialize exactly one Nod action")
    };
    assert_eq!(action.owner, tribute_owner);
    assert_eq!(action.wwd, activation.worldwide_day);
    let authority = ActiveNodSetV1 {
        job_id: generation.job_id,
        program_semantics_hash: generation.program_semantics_hash,
        worldwide_day: generation.worldwide_day,
        generation: generation.generation,
        nod_root: generation.nod_root,
        nod_count: generation.nod_count,
    };
    NodMembershipProofV1 {
        job_id: generation.job_id,
        program_semantics_hash: generation.program_semantics_hash,
        worldwide_day: generation.worldwide_day,
        generation: generation.generation,
        nod_ordinal: 0,
        action: action.clone(),
        membership_siblings: Vec::new(),
    }
    .verify_against(&authority, &poc_schema_limits())
    .expect("FullNode Nod action membership proof against finalized generation root");

    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("public JobIntent before FullNode local-result comparison");
    let local_result = std::fs::read(full_node_local_result_path(world, generation.job_id))
        .expect("read keyless FullNode canonical Lysis result");
    let local_result = LysisResultV1::decode_canonical(&local_result, &poc_schema_limits())
        .expect("decode keyless FullNode canonical Lysis result");
    assert_eq!(local_result.job_id, generation.job_id);
    assert_eq!(request.intent_id, activation.intent_id);
    assert_eq!(
        local_result
            .result_digest(&poc_schema_limits())
            .expect("validate keyless FullNode canonical Lysis result"),
        activation.result_digest,
        "FullNode computed result differs from the canonical quorum result"
    );
    assert!(
        !dynamic_vote_submission_path(world, world.validators.joiner_index(), generation.job_id,)
            .exists(),
        "keyless FullNode must not publish an OCOMP vote"
    );
}

#[when("the keyless FullNode compute roles stop before the job")]
fn stop_keyless_full_node_compute_before_job(world: &mut World) {
    let index = u8::try_from(world.validators.joiner_index()).expect("joiner index fits u8");
    world
        .ocomp
        .stop_keyless_full_node_roles(index)
        .expect("stop only the keyless FullNode compute clients");
}

#[then("the keyless FullNode holds at the exclusive deadline while validators keep finalizing")]
fn keyless_full_node_holds_at_deadline(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized OCOMP JobIntent");
    let expected_barrier = request.deadline_height;
    let committee_target = request.deadline_height.saturating_add(2);
    let primary = world.validators.primary_port();
    let full_node = world.validators.http_port(world.validators.joiner_index());
    let mut previous_committee = world
        .rpc
        .finalized_result(primary)
        .expect("observe committee finality before FullNode deadline barrier");
    let mut progress_deadline =
        Instant::now() + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);

    loop {
        world
            .localnet
            .ensure_committee_alive()
            .expect("validators remain alive while the FullNode waits");
        let committee_finalized = world
            .rpc
            .finalized_result(primary)
            .expect("observe committee finality at FullNode deadline barrier");
        let full_node_finalized = world.rpc.finalized(full_node);
        if committee_finalized >= committee_target {
            assert_eq!(
                full_node_finalized,
                Some(expected_barrier),
                "FullNode must finalize deadline block D and hold before D+1"
            );
            break;
        }
        if let Some(full_node_finalized) = full_node_finalized {
            assert!(
                full_node_finalized <= expected_barrier,
                "unresolved FullNode advanced past deadline D: deadline={expected_barrier}, \
                 full_node={full_node_finalized}"
            );
        }
        assert!(
            !world
                .localnet
                .joiner_full_node_exited(world.validators.joiner_index()),
            "keyless FullNode exited instead of holding its deadline barrier"
        );
        let now = Instant::now();
        match monotonic_progress_decision(
            committee_finalized,
            committee_target,
            previous_committee,
            now,
            progress_deadline,
        ) {
            ProgressWaitDecision::Reached => unreachable!("target handled above"),
            ProgressWaitDecision::Progressed => {
                previous_committee = committee_finalized;
                progress_deadline = now + Duration::from_secs(OCOMP_PROGRESS_STALL_TIMEOUT_SECS);
            }
            ProgressWaitDecision::Waiting => {}
            ProgressWaitDecision::Stalled => {
                panic!(
                    "committee finality stalled before proving the FullNode D boundary: \
                     deadline={}, target={committee_target}, committee={committee_finalized}, \
                     full_node={full_node_finalized:?}",
                    request.deadline_height
                );
            }
        }
        sleep(Duration::from_millis(250));
    }
    let job_id = finalized_job_id(world);
    assert!(!full_node_local_result_path(world, job_id).exists());
    assert!(!dynamic_vote_submission_path(world, world.validators.joiner_index(), job_id).exists());
    world.state.ocomp_full_node_deadline_barrier_height = Some(expected_barrier);
}

#[when("the unresolved keyless FullNode restarts with preserved data")]
fn restart_unresolved_keyless_full_node(world: &mut World) {
    let index = world.validators.joiner_index();
    world.localnet.stop_joiner_full_node(index);
    launch_preserved_keyless_full_node(world);
}

#[then("the restarted keyless FullNode restores the same deadline barrier without voting")]
fn restarted_keyless_full_node_restores_barrier(world: &mut World) {
    let index = world.validators.joiner_index();
    let port = world.validators.http_port(index);
    let barrier = world
        .state
        .ocomp_full_node_deadline_barrier_height
        .expect("captured FullNode deadline barrier");
    assert!(
        world.rpc.wait_finalized_at_least(port, barrier, 90),
        "restarted FullNode did not recover the exact unresolved checkpoint"
    );
    sleep(Duration::from_secs(2));
    assert_eq!(world.rpc.finalized(port), Some(barrier));
    assert!(!world.localnet.joiner_full_node_exited(index));
    assert!(!dynamic_vote_submission_path(world, index, finalized_job_id(world)).exists());
}

#[when("the keyless FullNode compute roles restart after the canonical quorum")]
fn restart_keyless_full_node_compute_after_quorum(world: &mut World) {
    let index = u8::try_from(world.validators.joiner_index()).expect("joiner index fits u8");
    world
        .ocomp
        .start_keyless_full_node_roles(index)
        .expect("restart keyless FullNode compute clients after canonical quorum");
}

#[then(
    "the keyless FullNode verifies the exact result and resumes finalized catch-up without voting"
)]
fn keyless_full_node_resumes_after_exact_result(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("finalized canonical OCOMP activation");
    let path = full_node_local_result_path(world, activation.job_id);
    let timeout = Instant::now() + Duration::from_secs(600);
    while !path.is_file() {
        assert!(
            Instant::now() < timeout,
            "keyless FullNode did not persist its exact late result"
        );
        sleep(Duration::from_millis(250));
    }
    let result = LysisResultV1::decode_canonical(
        &std::fs::read(&path).expect("read FullNode late result"),
        &poc_schema_limits(),
    )
    .expect("decode FullNode late result");
    assert_eq!(
        result
            .result_digest(&poc_schema_limits())
            .expect("validate FullNode late result"),
        activation.result_digest
    );
    let target = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("committee finalized target after exact result");
    let index = world.validators.joiner_index();
    let port = world.validators.http_port(index);
    assert!(world.rpc.wait_finalized_at_least(port, target, 120));
    assert!(!world.localnet.joiner_full_node_exited(index));
    assert!(!dynamic_vote_submission_path(world, index, activation.job_id).exists());
    world.state.ocomp_full_node_resumed_finalized_height = world.rpc.finalized(port);
}

#[then("the keyless FullNode computes its local result before canonical quorum")]
fn keyless_full_node_computes_before_quorum(world: &mut World) {
    let job_id = finalized_job_id(world);
    let path = full_node_local_result_path(world, job_id);
    let timeout = Instant::now() + Duration::from_secs(600);
    while !path.is_file() {
        assert!(
            Instant::now() < timeout,
            "keyless FullNode did not compute before the remaining validator workers resumed"
        );
        sleep(Duration::from_millis(250));
    }
    let bytes = std::fs::read(&path).expect("read FullNode local-first result");
    let result = LysisResultV1::decode_canonical(&bytes, &poc_schema_limits())
        .expect("decode FullNode local-first result");
    let digest = result
        .result_digest(&poc_schema_limits())
        .expect("validate FullNode local-first result");
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized JobIntent");
    let record = world
        .rpc
        .finalized_ocomp_job_record_on(world.validators.primary_port(), request.intent_id)
        .expect("non-quorum job remains public");
    assert_eq!(record.status, OcompJobStatus::VotingOpen);
    assert!(record.terminal.is_none());
    assert!(!dynamic_vote_submission_path(world, world.validators.joiner_index(), job_id).exists());
    world.state.ocomp_full_node_local_result_before_restart = Some(bytes);
    world.state.ocomp_full_node_local_first_digest = Some(digest);
}

#[when("the keyless FullNode restarts with that preserved local result")]
fn restart_keyless_full_node_with_local_result(world: &mut World) {
    let index = world.validators.joiner_index();
    let validator_index = u8::try_from(index).expect("joiner index fits u8");
    world
        .ocomp
        .stop_keyless_full_node_roles(validator_index)
        .expect("stop FullNode compute clients before local-first restart");
    world.localnet.stop_joiner_full_node(index);
    launch_preserved_keyless_full_node(world);
    world
        .ocomp
        .start_keyless_full_node_roles(validator_index)
        .expect("restart FullNode compute clients after local-first restart");
    let bytes = std::fs::read(full_node_local_result_path(world, finalized_job_id(world)))
        .expect("read preserved local-first result after restart");
    assert_eq!(
        Some(&bytes),
        world
            .state
            .ocomp_full_node_local_result_before_restart
            .as_ref()
    );
}

#[when("the keyless FullNode arms one valid local-result mismatch")]
fn arm_keyless_full_node_mismatch(world: &mut World) {
    let index = u8::try_from(world.validators.joiner_index()).expect("joiner index fits u8");
    world
        .ocomp
        .arm_keyless_full_node_result_mismatch(index)
        .expect("arm one test-only valid FullNode result mutation");
}

#[then("only the keyless FullNode shuts down with durable mismatch evidence")]
fn only_keyless_full_node_shuts_down_on_mismatch(world: &mut World) {
    let activation = world
        .state
        .ocomp_activation
        .as_ref()
        .expect("canonical activation before mismatch shutdown");
    let job_id = activation.job_id;
    let index = world.validators.joiner_index();
    let timeout = Instant::now() + Duration::from_secs(120);
    while !world.localnet.joiner_full_node_exited(index) {
        world
            .localnet
            .ensure_committee_alive()
            .expect("validator committee remains alive during FullNode mismatch");
        assert!(
            Instant::now() < timeout,
            "mismatched FullNode did not request isolated shutdown"
        );
        sleep(Duration::from_millis(250));
    }
    let evidence_root = world
        .ocomp
        .keyless_full_node_fatal_evidence_root(u8::try_from(index).expect("joiner fits u8"))
        .expect("resolve FullNode fatal evidence root");
    let mut evidence = std::fs::read_dir(&evidence_root)
        .expect("read durable FullNode fatal evidence")
        .map(|entry| entry.expect("read fatal evidence entry").path())
        .collect::<Vec<_>>();
    evidence.sort();
    assert_eq!(
        evidence.len(),
        2,
        "mismatch and sticky evidence are both durable"
    );
    let joined = evidence
        .iter()
        .map(|path| std::fs::read_to_string(path).expect("read fatal evidence file"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains(&format!("job_id={job_id}")));
    assert!(joined.contains("local_result_digest="));
    assert!(joined.contains("canonical_result_digest="));
    world.state.ocomp_full_node_mismatch_job_id = Some(job_id);
    world.state.ocomp_full_node_mismatch_evidence_files = evidence
        .iter()
        .map(|path| {
            path.file_name()
                .expect("fatal evidence file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    world.state.ocomp_finality_before_fault = Some(
        world
            .rpc
            .finalized_result(world.validators.primary_port())
            .expect("capture finalized height before FullNode mismatch restart"),
    );
}

#[when("the mismatched keyless FullNode restarts with preserved data")]
fn restart_mismatched_keyless_full_node(world: &mut World) {
    let index = world.validators.joiner_index();
    let validator_index = u8::try_from(index).expect("joiner fits u8");
    world
        .ocomp
        .stop_keyless_full_node_roles(validator_index)
        .expect("stop external FullNode clients before sticky-fatal restart");
    world.localnet.stop_joiner_full_node(index);
    launch_preserved_keyless_full_node(world);
}

#[then("it fails closed from sticky evidence while validators keep finalizing")]
fn sticky_mismatch_remains_isolated(world: &mut World) {
    let index = world.validators.joiner_index();
    let timeout = Instant::now() + Duration::from_secs(60);
    while !world.localnet.joiner_full_node_exited(index) {
        assert!(
            Instant::now() < timeout,
            "FullNode restart ignored sticky fatal evidence"
        );
        sleep(Duration::from_millis(250));
    }
    assert!(world
        .localnet
        .log_has(index, "embedded OCOMP persisted fatal evidence")
        .expect("read required owned process log"));
    let before = world
        .state
        .ocomp_finality_before_fault
        .expect("finality before FullNode mismatch restart");
    let primary = world.validators.primary_port();
    assert!(world
        .rpc
        .wait_finalized_at_least(primary, before.saturating_add(2), 60));
    world
        .localnet
        .ensure_committee_alive()
        .expect("all validators remain alive after isolated FullNode fatal");
}
