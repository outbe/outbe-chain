use super::*;
use crate::tx::UnsignedRelayTransactionV1;

pub(super) async fn prepare_upgrade_relay_v1(
    rpc: &(impl RegistryRpc + RelayPreparationRpc + Sync),
    relay: &RelaySignerV1,
    node_data_dir: &Path,
    selector: &NodeBindingSelectorV1,
) -> Result<()> {
    let (context, durable, evidence) = load_key_ready_material(node_data_dir)?;
    let proof = evidence
        .transition_key_ready_proof()
        .ok_or(UpgradeError::MissingDurableKeyReadyProof)?;
    let active = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
    let successor = read_finalized_upgrade_policy_v1(rpc)
        .await?
        .ok_or(UpgradeError::NoStagedSuccessorPolicy)?;
    if active.schedule.finalized_height != successor.finalized_height
        || active.schedule.finalized_hash != successor.finalized_hash
    {
        return Err(UpgradeError::FinalizedHeadMismatch.into());
    }
    let policy_hash = successor
        .policy
        .policy_hash()
        .map_err(UpgradeError::HashStagedSuccessorPolicy)?;
    let staged_policy_matches = policy_hash == context.successor_policy_hash
        && successor.policy.activation_height == context.activation_height;
    if !staged_policy_matches
        || evidence.intent().policy_hash != policy_hash
        || evidence.intent().operation != AttestationOperationV1::TransitionEnclaveMeasurement
    {
        return Err(UpgradeError::DurableSubmissionPolicyMismatch.into());
    }
    proof
        .verify_for_transition(evidence.intent(), active.tribute_offer_public.into())
        .map_err(UpgradeError::InvalidDurableKeyReadyProof)?;
    ensure_transition_source_or_target_v1(&active.binding, evidence.intent())?;

    let (calldata, raw) = sign_transition_relay(rpc, relay, &durable, &successor.policy).await?;
    let (intent_hash, evidence_hash) = transition_commitments(&evidence, &durable)?;
    record_upgrade_submission_prepared_v1(
        node_data_dir,
        PreparedUpgradeSubmissionV1 {
            intent_hash,
            evidence_hash,
            calldata_hash: keccak256(&calldata),
            relay: relay.address(),
            relay_variants: vec![raw],
        },
    )?;
    Ok(())
}
async fn sign_transition_relay(
    rpc: &(impl RelayPreparationRpc + Sync),
    relay: &RelaySignerV1,
    durable: &ReplacementCandidateSubmissionV1,
    policy: &outbe_primitives::tee_attestation_v1::TeePolicyV1,
) -> Result<(Vec<u8>, RawRelayTransactionV1)> {
    let calldata = transition_calldata(durable);
    let gas_limit = TeeRegistryGasScheduleV1::normative()
        .maximum_transaction_gas(
            RegistryMutatorV1::TransitionEnclaveMeasurement,
            calldata.len(),
            durable.evidence().len(),
            policy.measurement_rules.len(),
            policy.attestation_mode,
        )
        .map_err(UpgradeError::CalculateNormativeTransitionGas)?;
    let chain_id = rpc.chain_id().await?;
    let account_nonce = rpc.transaction_count(relay.address()).await?;
    let gas_price = buffered_gas_price(rpc.gas_price().await?);
    let required_balance = gas_price.saturating_mul(U256::from(gas_limit));
    let balance = rpc.balance(relay.address()).await?;
    if balance < required_balance {
        return Err(UpgradeError::InsufficientRelayBalance {
            address: relay.address(),
            balance,
            required_balance,
        }
        .into());
    }
    let raw = relay.sign_renewal(UnsignedRelayTransactionV1 {
        chain_id,
        account_nonce,
        gas_price,
        gas_limit,
        to: TEE_REGISTRY_ADDRESS,
        calldata: &calldata,
    })?;
    Ok((calldata, raw))
}

fn load_key_ready_material(
    node_data_dir: &Path,
) -> Result<(
    UpgradeContextV1,
    ReplacementCandidateSubmissionV1,
    AttestationEvidenceV1,
)> {
    let snapshot =
        inspect_upgrade_journal_v1(node_data_dir)?.ok_or(UpgradeError::CandidateNotPrepared)?;
    let UpgradeJournalStateV1::CandidateKeyReady {
        context, security, ..
    } = snapshot.lifecycle
    else {
        return Err(UpgradeError::RelayRequiresCandidateKeyReady.into());
    };
    let durable = load_replacement_candidate_submission(node_data_dir)
        .map_err(UpgradeError::ReloadExactCandidateSubmission)?
        .ok_or(UpgradeError::KeyReadyMissingDurableSubmission)?;
    let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
        .map_err(UpgradeError::DecodeDurableTransitionEvidence)?;
    let proof = evidence
        .transition_key_ready_proof()
        .ok_or(UpgradeError::MissingDurableKeyReadyProof)?;
    let encoded_proof = proof
        .encode_canonical()
        .map_err(UpgradeError::EncodeDurableKeyReadyProof)?;
    if keccak256(encoded_proof) != security.proof_hash
        || B256::from(proof.resident_offer_public) != security.resident_offer_public
    {
        return Err(UpgradeError::DurableProofSecurityMismatch.into());
    }

    Ok((context, durable, evidence))
}

pub(super) async fn finalized_transition_matches_v1(
    rpc: &(impl RegistryRpc + Sync),
    selector: &NodeBindingSelectorV1,
    node_data_dir: &Path,
    submission: &PreparedUpgradeSubmissionV1,
) -> Result<bool> {
    let durable = load_replacement_candidate_submission(node_data_dir)
        .map_err(UpgradeError::ReloadExactCandidateSubmission)?
        .ok_or(UpgradeError::SubmissionMissingDurableMaterial)?;
    let durable_evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
        .map_err(UpgradeError::DecodeDurableTransitionEvidence)?;
    let (intent_hash, evidence_hash) = transition_commitments(&durable_evidence, &durable)?;
    let calldata = transition_calldata(&durable);
    if intent_hash != submission.intent_hash
        || evidence_hash != submission.evidence_hash
        || keccak256(calldata) != submission.calldata_hash
    {
        return Err(UpgradeError::NodeHostRelayMaterialMismatch.into());
    }
    let view = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
    ensure_transition_source_or_target_v1(&view.binding, durable_evidence.intent())?;
    transition_target_matches_v1(&view.binding, durable_evidence.intent())
}

fn transition_calldata(durable: &ReplacementCandidateSubmissionV1) -> Vec<u8> {
    ITeeRegistryV1::transitionEnclaveMeasurementCall {
        evidence: durable.evidence().to_vec().into(),
        nodeSignature: durable.node_signature().to_vec().into(),
        enclaveSignature: durable.enclave_signature().to_vec().into(),
    }
    .abi_encode()
}

fn transition_commitments(
    evidence: &AttestationEvidenceV1,
    durable: &ReplacementCandidateSubmissionV1,
) -> Result<(B256, B256)> {
    let intent_hash = evidence
        .intent()
        .intent_hash()
        .map_err(UpgradeError::HashDurableTransitionIntent)?;
    let evidence_hash = AttestationEvidenceV1::decode_canonical(durable.evidence())
        .and_then(|e| e.evidence_hash())
        .map_err(UpgradeError::HashDurableTransitionEvidence)?;
    Ok((intent_hash, evidence_hash))
}
