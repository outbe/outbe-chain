//! Held, changed-binding and equivocating result votes around the deadline.

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

#[when("the held validator vote is broadcast after the exclusive deadline closes")]
fn held_vote_is_broadcast_at_deadline(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let primary = world.validators.primary_port();
    let closed_height = request.deadline_height;
    let timeout = Instant::now() + Duration::from_secs(180);
    while world
        .rpc
        .finalized_result(primary)
        .expect("primary finality while closing exclusive vote deadline")
        < closed_height
    {
        assert!(
            Instant::now() < timeout,
            "chain did not close the exclusive vote deadline"
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
        "a vote included after the exclusive deadline must revert"
    );
    let inclusion_height = world
        .rpc
        .receipt_block_number(&submitted_hash, primary)
        .expect("held vote inclusion height");
    assert!(
        inclusion_height > request.deadline_height,
        "held vote was included before the closed exclusive boundary: \
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
                    .contains("result vote has no response window"),
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

#[when("the first validator submits a second correctly signed vote with the same binding and a different result")]
fn one_valid_then_same_binding_equivocation(world: &mut World) {
    let request = world
        .state
        .ocomp_job_request
        .clone()
        .expect("finalized public JobIntent");
    let primary = world.validators.primary_port();
    let deadline = Instant::now() + Duration::from_secs(180);
    let (job_id, original) = loop {
        let record = world
            .rpc
            .finalized_ocomp_job_record_on(primary, request.intent_id)
            .expect("finalized OCOMP job record");
        if record.status == OcompJobStatus::VotingOpen {
            let job_id = record.finalized.as_ref().expect("finalized job").job_id;
            if let Some(accountability) = world
                .rpc
                .finalized_ocomp_vote_accountability_on(primary, job_id)
            {
                if accountability.slot_validator_indexes.contains(&0) {
                    break (job_id, accountability);
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "validator-0 did not finalize its first result vote"
        );
        sleep(Duration::from_millis(250));
    };
    assert!(
        original.quorum_result_digest.is_none(),
        "one validator unexpectedly formed a quorum"
    );
    let finalized = world.rpc.finalized(primary).expect("finalized vote height");
    let original_tx = world
        .rpc
        .finalized_ocomp_result_vote_transactions_on(primary, request.request_height, finalized)
        .expect("enumerate public OCOMP votes")
        .into_iter()
        .find(|tx| {
            tx.success
                && tx.signer
                    == world
                        .ocomp
                        .ocomp_delegate_address(0)
                        .expect("validator-0 delegate")
        })
        .expect("validator-0 public vote");
    let bytes = world
        .rpc
        .ocomp_result_vote_bytes_on(primary, original_tx.transaction_hash)
        .expect("decode validator-0 vote");
    let mut conflicting = ResultVoteV1::decode_canonical(&bytes, &poc_schema_limits())
        .expect("canonical validator-0 vote");
    assert_eq!(conflicting.job_id, job_id);
    let first_digest = conflicting
        .result_digest(&poc_schema_limits())
        .expect("first vote digest");
    conflicting.result.result_chunk_list_root =
        if conflicting.result.result_chunk_list_root == B256::repeat_byte(0x5a) {
            B256::repeat_byte(0xa5)
        } else {
            B256::repeat_byte(0x5a)
        };
    let conflicting_digest = conflicting
        .result_digest(&poc_schema_limits())
        .expect("structurally valid conflicting result digest");
    assert_ne!(first_digest, conflicting_digest);
    let delegate = world
        .ocomp
        .ocomp_delegate_address(0)
        .expect("validator-0 delegate");
    let nonce = world
        .rpc
        .canonical_nonce_on(primary, delegate)
        .expect("delegate nonce");
    let fee = world
        .rpc
        .gas_price_on(primary)
        .expect("public gas price")
        .max(MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS);
    let prepared = world
        .ocomp
        .prepare_held_vote_transaction(0, conflicting, nonce, fee, OCOMP_SYSTEM_CARRIER_GAS_LIMIT)
        .expect("sign conflicting vote with the owned OCOMP key");
    let hash = world
        .rpc
        .send_raw_transaction_on(primary, &prepared.raw_transaction.0)
        .expect("submit correctly signed conflicting vote");
    assert!(
        world.rpc.wait_successful_receipt(&hash, 240),
        "correctly bound conflicting vote did not succeed: {hash}"
    );
    let receipt = world
        .rpc
        .transaction_receipt(&hash, primary)
        .expect("conflicting vote receipt");
    assert_eq!(
        receipt.get("status").and_then(serde_json::Value::as_str),
        Some("0x1"),
        "correctly bound conflicting vote was rejected"
    );
    let inclusion = world
        .rpc
        .receipt_block_number(&hash, primary)
        .expect("conflicting vote inclusion height");
    for port in world.validators.committee_ports() {
        assert!(
            world.rpc.wait_finalized_at_least(port, inclusion, 60),
            "validator {port} did not finalize conflicting vote"
        );
    }
    world.state.ocomp_equivocation_job_id = Some(job_id);
    world.state.ocomp_equivocation_first_digest = Some(first_digest);
    world.state.ocomp_equivocation_conflicting_digest = Some(conflicting_digest);
}

#[then("every validator records one equivocation and no result quorum while finality advances")]
fn same_binding_equivocation_is_recorded(world: &mut World) {
    let job_id = world
        .state
        .ocomp_equivocation_job_id
        .expect("equivocation job");
    let first = world
        .state
        .ocomp_equivocation_first_digest
        .expect("first result digest");
    let conflicting = world
        .state
        .ocomp_equivocation_conflicting_digest
        .expect("conflicting result digest");
    let ports = world.validators.committee_ports();
    let baseline = world
        .rpc
        .finalized(world.validators.primary_port())
        .expect("finality after conflict");
    let checkpoint = world
        .rpc
        .wait_finalized_checkpoint(&ports, baseline + 2, 90)
        .expect("committee finality continues after equivocation");
    for port in ports {
        let accountability = world
            .rpc
            .finalized_ocomp_vote_accountability_on(port, job_id)
            .expect("finalized OCOMP accountability");
        assert!(
            accountability.slot_first_digests.contains(&(0, first)),
            "validator-0 first digest missing from accountability"
        );
        assert!(
            accountability
                .slot_equivocation_digests
                .contains(&(0, conflicting)),
            "validator-0 conflicting digest missing from accountability"
        );
        assert!(
            accountability.quorum_result_digest.is_none(),
            "conflicting solo voter formed a result quorum"
        );
        let observed = world
            .rpc
            .checkpoint_at(port, checkpoint.height)
            .expect("finalized checkpoint after equivocation");
        assert_eq!(observed.block_hash, checkpoint.block_hash);
        assert_eq!(observed.state_root, checkpoint.state_root);
    }
}
