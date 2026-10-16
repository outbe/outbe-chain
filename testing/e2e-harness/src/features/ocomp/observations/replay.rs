//! Timely replay of a completed job and its retained case-one artifacts.

use crate::features::ocomp::*;

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
