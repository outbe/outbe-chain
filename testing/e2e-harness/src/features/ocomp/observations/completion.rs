use crate::features::ocomp::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::features::ocomp) enum BoundedCompletionDecision {
    Complete,
    Continue,
    TimedOut,
}

pub(in crate::features::ocomp) fn bounded_completion_decision(
    all_complete: bool,
    now: Instant,
    deadline: Instant,
) -> BoundedCompletionDecision {
    if all_complete {
        BoundedCompletionDecision::Complete
    } else if now >= deadline {
        BoundedCompletionDecision::TimedOut
    } else {
        BoundedCompletionDecision::Continue
    }
}

pub(in crate::features::ocomp) fn wait_for_finalized_ocomp_activation(world: &mut World) {
    let activation_height = world
        .state
        .ocomp_activation_height
        .expect("prepared OCOMP activation height");
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let finalized = world
            .validators
            .committee_ports()
            .into_iter()
            .map(|port| world.rpc.finalized(port))
            .collect::<Vec<_>>();
        if finalized
            .iter()
            .all(|height| height.is_some_and(|height| height >= activation_height))
        {
            return;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles stay alive until the immutable fork is active");
        assert!(
            Instant::now() < deadline,
            "OCOMP fork did not finalize on every validator before public Tribute submission: \
             expected height {activation_height}, observed {finalized:?}"
        );
        sleep(Duration::from_millis(250));
    }
}

pub(in crate::features::ocomp) fn dynamic_job_record(
    world: &World,
    request: &crate::world::rpc::OcompPublicJobRequestV1,
) -> OcompJobRecordV1 {
    world
        .rpc
        .finalized_ocomp_job_record_on(world.validators.primary_port(), request.intent_id)
        .expect("dynamic OCOMP job record")
}

pub(in crate::features::ocomp) fn finalized_vote_for_delegate_on_job(
    world: &World,
    from_height: u64,
    to_height: u64,
    node_index: usize,
    job_id: B256,
) -> Option<ResultVoteV1> {
    let validator_index = u8::try_from(node_index).ok()?;
    let delegate = world.ocomp.ocomp_delegate_address(validator_index).ok()?;
    world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(
            world.validators.primary_port(),
            from_height,
            to_height,
        )?
        .into_iter()
        .filter(|transaction| transaction.success && transaction.signer == delegate)
        .find_map(|transaction| {
            let bytes = world.rpc.ocomp_result_vote_bytes_on(
                world.validators.primary_port(),
                transaction.transaction_hash,
            )?;
            let vote = ResultVoteV1::decode_canonical(&bytes, &poc_schema_limits()).ok()?;
            (vote.job_id == job_id).then_some(vote)
        })
}

pub(in crate::features::ocomp) fn dynamic_pre_restart_vote_baseline_ready(
    job_a_vote_count: usize,
    job_b_vote_count: usize,
    joiner_vote_present: bool,
) -> bool {
    job_a_vote_count == 2 && job_b_vote_count == 3 && joiner_vote_present
}

pub(in crate::features::ocomp) fn singleton_participant_bitmap(
    member_count: u16,
    participant_index: u16,
) -> Vec<u8> {
    assert!(participant_index < member_count);
    let mut bitmap = vec![0_u8; usize::from(member_count).div_ceil(8)];
    bitmap[usize::from(participant_index / 8)] |= 1_u8 << (participant_index % 8);
    bitmap
}

pub(in crate::features::ocomp) fn dynamic_vote_submission_path(
    world: &World,
    node_index: usize,
    job_id: B256,
) -> std::path::PathBuf {
    let job_component = hex::encode(job_id.as_slice());
    world
        .validators
        .data_dir(node_index)
        .parent()
        .expect("validator data directory has a node-slot parent")
        .join("ocomp")
        .join("domain-v1")
        .join("supervisor-v1")
        .join("vote-submissions")
        .join(&job_component)
        .join(format!("{job_component}.vote.v1"))
}

pub(in crate::features::ocomp) fn local_result_path(
    world: &World,
    node_index: usize,
    job_id: B256,
) -> std::path::PathBuf {
    world
        .validators
        .data_dir(node_index)
        .parent()
        .expect("node data directory has a node-slot parent")
        .join("ocomp")
        .join("domain-v1")
        .join("node-v1")
        .join("local-results")
        .join(format!(
            "{}.lysis-result-v1.ocb1",
            hex::encode(job_id.as_slice())
        ))
}

pub(in crate::features::ocomp) fn finalized_job_id(world: &World) -> B256 {
    let request = world
        .state
        .ocomp_job_request
        .as_ref()
        .expect("finalized OCOMP JobIntent");
    world
        .rpc
        .finalized_ocomp_job_record_on(world.validators.primary_port(), request.intent_id)
        .and_then(|record| record.finalized.map(|finalized| finalized.job_id))
        .expect("finalized OCOMP job identity")
}

pub(crate) fn result_nod_actions_on(
    world: &World,
    node_index: usize,
    job_id: B256,
) -> Vec<NodActionV1> {
    let objects = world
        .validators
        .data_dir(node_index)
        .parent()
        .expect("node data directory has a node-slot parent")
        .join("ocomp")
        .join("domain-v1")
        .join("cas-v1")
        .join("objects");
    let limits = poc_schema_limits();
    let mut chunks = Vec::new();
    for prefix in std::fs::read_dir(&objects).expect("read OCOMP CAS prefix directory") {
        let prefix = prefix.expect("read OCOMP CAS prefix entry");
        assert!(
            prefix.file_type().expect("read CAS prefix type").is_dir(),
            "OCOMP CAS prefix is not a directory: {:?}",
            prefix.path()
        );
        for object in std::fs::read_dir(prefix.path()).expect("read OCOMP CAS object directory") {
            let object = object.expect("read OCOMP CAS object entry");
            assert!(
                object.file_type().expect("read CAS object type").is_file(),
                "OCOMP CAS object is not a file: {:?}",
                object.path()
            );
            let bytes = std::fs::read(object.path()).expect("read OCOMP CAS object");
            if let Ok(chunk) = ResultChunkV1::decode_canonical(&bytes, &limits) {
                if chunk.job_id == job_id {
                    chunks.push(chunk);
                }
            }
        }
    }
    chunks.sort_by_key(|chunk| chunk.chunk_ordinal);
    assert!(!chunks.is_empty(), "node has no result chunks for {job_id}");
    chunks
        .into_iter()
        .flat_map(|chunk| chunk.ordered_nod_actions)
        .collect()
}

/// This step belongs to the 1/10-Tribute happy paths. Fault scenarios can
/// deliberately prevent a validator from calculating its own result.
#[then("every validator independently verifies the V1 Nod commitment encoding")]
fn independent_nod_commitment_encoding(world: &mut World) {
    let generation = world
        .state
        .ocomp_certified_generation
        .as_ref()
        .expect("certified Nod generation before commitment check");
    assert!(
        matches!(generation.nod_count, 1 | 10),
        "declared 1/10-Nod fixture"
    );
    let ports = world.validators.committee_ports();
    let deadline = Instant::now() + Duration::from_secs(120);
    for (index, port) in ports.iter().enumerate() {
        let path = local_result_path(world, index, generation.job_id);
        while !path.is_file() {
            assert!(
                Instant::now() < deadline,
                "validator {port} has no completed local result"
            );
            sleep(Duration::from_millis(250));
        }
        let actions = result_nod_actions_on(world, index, generation.job_id);
        assert_eq!(
            actions.len(),
            generation.nod_count as usize,
            "result action population on port {port}"
        );
        let expected_root = crate::internal::nod_reference::nod_root(&actions);
        assert_eq!(
            generation.nod_root, expected_root,
            "independent V1 Nod commitment on port {port}"
        );
        eprintln!(
            "NOD_COMMITMENT_EXPECTATION port={port} job={} count={} root={} height={} block_hash={}",
            generation.job_id, actions.len(), expected_root,
            generation.block_number, generation.block_hash,
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::features::ocomp) enum PublicVoteSetExpectation {
    AnyQuorum,
    Exact(&'static [u16]),
}

pub(in crate::features::ocomp) fn public_vote_set_matches(
    expectation: PublicVoteSetExpectation,
    validator_indexes: &[u16],
    quorum_threshold: usize,
) -> bool {
    match expectation {
        PublicVoteSetExpectation::AnyQuorum => validator_indexes.len() >= quorum_threshold,
        PublicVoteSetExpectation::Exact(expected) => validator_indexes == expected,
    }
}

pub(in crate::features::ocomp) fn completed_accountability_is_preserved(
    expected: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
    observed: &crate::world::rpc::OcompPublicVoteAccountabilityV1,
) -> bool {
    expected.job_id == observed.job_id
        && expected.result_validator_set_epoch == observed.result_validator_set_epoch
        && expected.result_committee_set_hash == observed.result_committee_set_hash
        && expected.result_ocomp_binding_hash == observed.result_ocomp_binding_hash
        && expected.member_count == observed.member_count
        && expected.quorum_threshold == observed.quorum_threshold
        && expected.quorum_result_digest == observed.quorum_result_digest
        && expected.quorum_height == observed.quorum_height
        && expected.quorum_signer_bitmap == observed.quorum_signer_bitmap
        && expected
            .slot_validator_indexes
            .iter()
            .all(|index| observed.slot_validator_indexes.contains(index))
        && expected
            .slot_first_signatures
            .iter()
            .all(|signature| observed.slot_first_signatures.contains(signature))
}

pub(in crate::features::ocomp) fn quorum_applies_lysis_and_creates_nod_for_request(
    world: &mut World,
    request: crate::world::rpc::OcompPublicJobRequestV1,
    vote_expectation: PublicVoteSetExpectation,
) {
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let ports = world.validators.committee_ports();
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
        if activations.iter().all(Option::is_some) {
            let activation = activations[0].clone().expect("all activations are present");
            assert!(
                activations
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&activation)),
                "validators expose different finalized Lysis activation"
            );
            assert_eq!(activation.intent_id, request.intent_id);
            assert_eq!(activation.worldwide_day, request.worldwide_day);
            assert_ne!(activation.job_id, B256::ZERO);
            assert_ne!(activation.result_digest, B256::ZERO);
            assert_ne!(activation.activation_call_id, B256::ZERO);
            assert_ne!(activation.terminal_receipt_hash, B256::ZERO);

            let generations = ports
                .iter()
                .copied()
                .map(|port| {
                    world
                        .rpc
                        .finalized_ocomp_certified_generation_on(port, &activation)
                })
                .collect::<Vec<_>>();
            assert!(
                generations.iter().all(Option::is_some),
                "one or more validators cannot verify both generation projections at the exact \
                 finalized activation block: {generations:?}"
            );
            let generation = generations[0]
                .clone()
                .expect("all certified generations are present");
            assert!(
                generations
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&generation)),
                "validators expose different certified Nod generations"
            );
            assert_eq!(generation.worldwide_day, request.worldwide_day);
            assert_eq!(generation.job_id, activation.job_id);
            assert_eq!(generation.block_number, activation.block_number);
            assert_eq!(generation.block_hash, activation.block_hash);
            assert_ne!(generation.program_semantics_hash, B256::ZERO);
            assert_ne!(generation.nod_root, B256::ZERO);
            assert_ne!(generation.bucket_root, B256::ZERO);
            assert_ne!(generation.output_manifest_root, B256::ZERO);
            assert_eq!(generation.tribute_count, generation.nod_count);
            assert!(generation.tribute_count > 0);
            assert!(generation.bucket_count <= generation.nod_count);

            let accountability_deadline = Instant::now() + Duration::from_secs(120);
            let accountability = loop {
                let observed = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .finalized_ocomp_vote_accountability_on(port, activation.job_id)
                    })
                    .collect::<Vec<_>>();
                if observed.iter().all(|value| {
                    value.as_ref().is_some_and(|accountability| {
                        public_vote_set_matches(
                            vote_expectation,
                            &accountability.slot_validator_indexes,
                            usize::from(accountability.quorum_threshold),
                        )
                    })
                }) {
                    let first = observed[0]
                        .clone()
                        .expect("all accountability records are present");
                    assert!(
                        observed.iter().all(|value| value.as_ref() == Some(&first)),
                        "validators expose different finalized vote accountability"
                    );
                    break first;
                }
                assert!(
                    Instant::now() < accountability_deadline,
                    "the expected {vote_expectation:?} validator vote set did not reach \
                     finalized accountability: {observed:?}"
                );
                sleep(Duration::from_millis(250));
            };
            assert_eq!(accountability.job_id, activation.job_id);
            let observed_vote_count = accountability.slot_validator_indexes.len();
            assert_eq!(
                accountability.quorum_result_digest,
                Some(activation.result_digest)
            );
            assert_eq!(
                accountability
                    .quorum_signer_bitmap
                    .as_ref()
                    .expect("completed job quorum")
                    .iter()
                    .map(|byte| byte.count_ones())
                    .sum::<u32>(),
                3
            );

            let finalized_height = ports
                .iter()
                .copied()
                .map(|port| {
                    world
                        .rpc
                        .finalized(port)
                        .expect("validator finalized height")
                })
                .min()
                .expect("four validator ports");
            let public_votes = ports
                .iter()
                .copied()
                .map(|port| {
                    world.rpc.finalized_ocomp_result_vote_transactions_on(
                        port,
                        request.request_height,
                        finalized_height,
                    )
                })
                .collect::<Vec<_>>();
            assert!(
                public_votes.iter().all(Option::is_some),
                "one or more validators cannot enumerate finalized public result votes"
            );
            let first_votes = public_votes[0]
                .clone()
                .expect("all public vote collections are present");
            assert!(
                public_votes
                    .iter()
                    .all(|observed| observed.as_ref() == Some(&first_votes)),
                "proposer/import/replay validators expose different public result-vote transactions"
            );
            assert_eq!(
                first_votes
                    .iter()
                    .filter(|transaction| transaction.success)
                    .count(),
                observed_vote_count,
                "unexpected number of independent successful public validator result votes"
            );
            let mut signers = first_votes
                .iter()
                .map(|transaction| transaction.signer)
                .collect::<Vec<_>>();
            signers.sort_unstable();
            signers.dedup();
            assert_eq!(
                signers.len(),
                observed_vote_count,
                "public result votes must come from the expected distinct validator EVM signers"
            );
            assert!(
                first_votes
                    .iter()
                    .any(|transaction| transaction.transaction_hash == activation.transaction_hash),
                "q-forming activation transaction is absent from the public result-vote set"
            );

            let primary = world.validators.primary_port();
            let mut balances_after = Vec::with_capacity(4);
            for (address, before) in &world.state.ocomp_validator_balances_before {
                let after = world
                    .rpc
                    .balance_on(primary, &format!("{address:#x}"))
                    .expect("read validator balance after result votes");
                assert_eq!(
                    after, *before,
                    "OCOMP delegate {address:#x} paid for a system-carrier result vote"
                );
                balances_after.push((*address, after));
            }

            if generation.tribute_count as usize == OCOMP_CAPACITY_TRIBUTE_COUNT {
                let q_forming = first_votes
                    .iter()
                    .find(|transaction| transaction.transaction_hash == activation.transaction_hash)
                    .expect("q-forming public transaction");
                let vote_bytes = world
                    .rpc
                    .ocomp_result_vote_bytes_on(primary, q_forming.transaction_hash)
                    .expect("canonical q-forming ResultVoteV1 bytes");
                let internal_work =
                    outbe_ocomp_protocol::capacity::result_vote_internal_work(vote_bytes.len())
                        .expect("q-forming vote fits generated internal-work cap");
                let finalized_block_hash = world
                    .rpc
                    .block_hash(primary, finalized_height)
                    .and_then(|value| value.parse::<B256>().ok())
                    .expect("finalized capacity capture block hash");
                let block_commitments = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .block_commitment(port, q_forming.block_number)
                            .unwrap_or_else(|| {
                                panic!(
                                    "validator on port {port} has no canonical q-forming \
                                     block/state/CE commitment"
                                )
                            })
                    })
                    .collect::<Vec<_>>();
                let canonical_commitment = block_commitments
                    .first()
                    .expect("four validator block commitments");
                assert_eq!(
                    canonical_commitment.block_hash, q_forming.block_hash,
                    "receipt block hash differs from the canonical imported block"
                );
                assert!(
                    block_commitments
                        .iter()
                        .all(|observed| observed == canonical_commitment),
                    "validators imported different q-forming block/state/CE commitments: \
                     {block_commitments:?}"
                );
                let receipt_hash = format!("{:#x}", q_forming.transaction_hash);
                let receipts = ports
                    .iter()
                    .copied()
                    .map(|port| {
                        world
                            .rpc
                            .transaction_receipt(&receipt_hash, port)
                            .unwrap_or_else(|| {
                                panic!(
                                    "validator on port {port} has no canonical q-forming receipt"
                                )
                            })
                    })
                    .collect::<Vec<_>>();
                let canonical_receipt =
                    receipts.first().expect("four validator q-forming receipts");
                assert!(
                    receipts
                        .iter()
                        .all(|observed| observed == canonical_receipt),
                    "validators retained different q-forming receipts"
                );
                let q_forming_validator_receipt_sha256 = receipts
                    .iter()
                    .map(|receipt| {
                        crate::ocomp_evidence::sha256_hex(
                            &serde_json::to_vec(receipt)
                                .expect("canonical q-forming receipt is JSON-serializable"),
                        )
                    })
                    .collect::<Vec<_>>();
                let q_forming_receipt_sha256 = q_forming_validator_receipt_sha256
                    .first()
                    .expect("four validator q-forming receipt digests")
                    .clone();
                world.state.ocomp_capacity_observation =
                    Some(crate::world::state::OcompPublicCapacityObservationV1 {
                        job_id: activation.job_id,
                        result_digest: activation.result_digest,
                        q_forming_transaction_hash: q_forming.transaction_hash,
                        q_forming_block_number: q_forming.block_number,
                        q_forming_block_hash: q_forming.block_hash,
                        q_forming_receipt_success: q_forming.success,
                        q_forming_receipt_sha256,
                        q_forming_validator_receipt_sha256,
                        q_forming_state_root: canonical_commitment.state_root,
                        q_forming_ce_root: canonical_commitment.ce_root,
                        q_forming_validator_commitments: block_commitments.clone(),
                        canonical_import_validator_count: u8::try_from(
                            block_commitments.len(),
                        )
                        .expect("validator count fits u8"),
                        canonical_import_verified: true,
                        finalized_block_number: finalized_height,
                        finalized_block_hash,
                        tribute_count: u64::from(generation.tribute_count),
                        nod_count: u64::from(generation.nod_count),
                        worker_shard_count:
                            outbe_ocomp_protocol::capacity::worker_shard_count(
                                u64::from(generation.tribute_count),
                                u32::try_from(
                                    outbe_ocomp_protocol::generated_shape::
                                        OCOMP_POC_CANDIDATE_LIMITS_V1
                                            .max_tributes_per_work_shard,
                                )
                                .expect("generated shard cap fits u32"),
                            )
                            .expect("generated shard cap is non-zero"),
                        transaction_bytes: u64::try_from(q_forming.raw_transaction_len)
                            .expect("q-forming transaction length fits u64"),
                        block_bytes: u64::try_from(q_forming.block_rlp_len)
                            .expect("q-forming block length fits u64"),
                        gas: q_forming.gas_used,
                        internal_work,

                    });
            }

            world.state.ocomp_activation = Some(activation);
            world.state.ocomp_certified_generation = Some(generation);
            world.state.ocomp_result_vote_transactions = first_votes;
            world.state.ocomp_vote_accountability = Some(accountability);
            world.state.ocomp_validator_balances_after = balances_after;
            world.state.ocomp_atomic_quorum_apply_verified = true;
            return;
        }
        world
            .ocomp
            .ensure_validator_roles_alive()
            .expect("OCOMP roles stay alive through public activation");
        assert!(
            Instant::now() < deadline,
            "q=3 public Lysis activation did not finalize before the bounded E2E deadline; \
             activations={activations:?}"
        );
        sleep(Duration::from_millis(500));
    }
}

pub(in crate::features::ocomp) fn timely_replay_receipt_height(
    receipt: &serde_json::Value,
    deadline: u64,
) -> eyre::Result<u64> {
    let block = receipt
        .get("blockNumber")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre!("replay receipt omitted blockNumber: {receipt}"))?;
    let height = u64::from_str_radix(
        block
            .strip_prefix("0x")
            .ok_or_else(|| eyre!("invalid replay block: {block}"))?,
        16,
    )?;
    ensure!(
        height < deadline,
        "replay included at {height}, outside exclusive deadline {deadline}: {receipt}"
    );
    ensure!(
        receipt.get("status").and_then(serde_json::Value::as_str) == Some("0x1"),
        "timely replay reverted: {receipt}"
    );
    Ok(height)
}

pub(in crate::features::ocomp) fn observe_timely_completed_replay(
    world: &mut World,
    phase: &str,
    hash: &str,
    receipt: &serde_json::Value,
) {
    let request = world.state.ocomp_job_request.as_ref().expect("replay job");
    let deadline = request.deadline_height;
    let observation = serde_json::json!({
        "phase": phase, "job_id": request.job_id, "open_height": request.open_height,
        "deadline_height": deadline, "transaction_hash": hash, "receipt": receipt
    });
    eprintln!("OCOMP_REPLAY_RECEIPT {observation}");
    world.state.ocomp_replay_receipts.push(observation);
    let height = timely_replay_receipt_height(receipt, deadline)
        .unwrap_or_else(|error| panic!("{phase} replay {hash}: {error:#}"));
    wait_for_common_finalized_checkpoint(world, height, phase);
}

pub(in crate::features::ocomp) fn verify_case_one_completed_artifacts(
    world: &mut World,
    intent_id: B256,
    bundle_hash: B256,
) -> eyre::Result<()> {
    world.localnet.ensure_committee_alive()?;
    let ports = world.validators.committee_ports();
    ensure!(
        ports.len() == 4,
        "artifact proof omitted an expected validator"
    );
    let height = ports
        .iter()
        .map(|&port| world.rpc.finalized_result(port))
        .collect::<eyre::Result<Vec<_>>>()?
        .into_iter()
        .min()
        .ok_or_else(|| eyre!("artifact proof has no finalized checkpoint"))?;
    let checkpoint = world.rpc.checkpoint_at(ports[0], height)?;
    let record = world
        .rpc
        .ocomp_job_record_at_on(ports[0], intent_id, height)?;
    ensure!(
        record.status == OcompJobStatus::Completed,
        "artifact job is not Completed"
    );
    ensure!(
        record.intent.protocol_bundle_hash == bundle_hash,
        "artifact job changed bundle"
    );
    let finalized = record
        .finalized
        .as_ref()
        .ok_or_else(|| eyre!("artifact job lacks finality"))?;
    let job_id = finalized.job_id;
    let binding = record
        .terminal
        .as_ref()
        .and_then(|terminal| terminal.completed_binding.as_ref())
        .ok_or_else(|| eyre!("artifact job lacks canonical completed binding"))?;
    let accountability = world
        .rpc
        .ocomp_vote_accountability_at_on(ports[0], job_id, height)?;
    ensure!(
        accountability.job_id == job_id
            && accountability.member_count == 4
            && accountability.quorum_threshold == 3
            && accountability.quorum_height == Some(binding.quorum_height)
            && accountability.quorum_result_digest == Some(binding.result_digest)
            && accountability
                .quorum_signer_bitmap
                .as_ref()
                .is_some_and(|bitmap| {
                    bitmap.iter().map(|byte| byte.count_ones()).sum::<u32>() == 3
                })
            && binding.quorum_height <= height,
        "artifact quorum differs from canonical completed authority"
    );
    for &port in &ports {
        ensure!(
            world.rpc.checkpoint_at(port, height)? == checkpoint,
            "artifact checkpoint differs"
        );
        ensure!(
            world.rpc.ocomp_job_record_at_on(port, intent_id, height)? == record,
            "artifact job differs at shared finalized checkpoint"
        );
        ensure!(
            world
                .rpc
                .ocomp_vote_accountability_at_on(port, job_id, height)?
                == accountability,
            "artifact voters differ at shared finalized checkpoint"
        );
    }
    let quorum_checkpoint = world.rpc.checkpoint_at(ports[0], binding.quorum_height)?;
    let transactions = world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(
            ports[0],
            binding.quorum_height,
            binding.quorum_height,
        )
        .ok_or_else(|| eyre!("cannot enumerate canonical quorum transactions"))?;
    let mut canonical_result = None;
    for transaction in transactions
        .iter()
        .filter(|transaction| transaction.success)
    {
        ensure!(
            transaction.block_number == binding.quorum_height
                && transaction.block_hash == quorum_checkpoint.block_hash,
            "quorum transaction has a foreign canonical block"
        );
        let bytes = world
            .rpc
            .ocomp_result_vote_bytes_on(ports[0], transaction.transaction_hash)
            .ok_or_else(|| eyre!("cannot read canonical quorum vote"))?;
        let vote = ResultVoteV1::decode_canonical(&bytes, &poc_schema_limits())?;
        if vote.job_id != job_id {
            continue;
        }
        ensure!(
            vote.protocol_bundle_hash == bundle_hash
                && vote.result_validator_set_epoch == accountability.result_validator_set_epoch
                && vote.result_committee_set_hash == accountability.result_committee_set_hash
                && vote.result_ocomp_binding_hash == accountability.result_ocomp_binding_hash,
            "quorum vote has a foreign job binding"
        );
        if vote.result.result_digest(&poc_schema_limits())? != binding.result_digest {
            continue;
        }
        if let Some(previous) = &canonical_result {
            ensure!(previous == &vote.result, "canonical quorum results differ");
        }
        canonical_result = Some(vote.result);
    }
    let result =
        canonical_result.ok_or_else(|| eyre!("canonical completed result is unavailable"))?;
    ensure!(
        result.job_id == job_id,
        "canonical result belongs to another job"
    );
    let proof = crate::world::ocomp::OcompCanonicalArtifactProof {
        checkpoint,
        bundle_hash,
        result,
        voters: accountability.slot_validator_indexes,
    };
    loop {
        world.localnet.ensure_committee_alive()?;
        let pids = (0..4_u8)
            .map(|index| {
                world
                    .localnet
                    .validator_pid(usize::from(index))
                    .map(|pid| (index, pid))
            })
            .collect::<eyre::Result<std::collections::BTreeMap<_, _>>>()?;
        if let Some(evidence) = world
            .ocomp
            .verify_completed_artifacts_canonical(&proof, &pids)?
        {
            world.localnet.ensure_committee_alive()?;
            eprintln!("OCOMP_ARTIFACT_PROOF_V1 {evidence}");
            return Ok(());
        }
        sleep(Duration::from_millis(250));
    }
}
