use super::*;

pub(super) async fn prepare_upgrade_relay_v1(
    rpc: &(impl RegistryRpc + RelayPreparationRpc + Sync),
    relay: &RelaySignerV1,
    node_data_dir: &Path,
    selector: &NodeBindingSelectorV1,
) -> Result<()> {
    let (context, durable, evidence) = load_key_ready_material(node_data_dir)?;
    let proof = evidence
        .transition_key_ready_proof()
        .ok_or_else(|| eyre::eyre!("durable transition evidence has no key-ready proof"))?;
    let active = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
    let successor = read_finalized_upgrade_policy_v1(rpc)
        .await?
        .ok_or_else(|| eyre::eyre!("no successor TEE policy is staged at finalized state"))?;
    if active.schedule.finalized_height != successor.finalized_height
        || active.schedule.finalized_hash != successor.finalized_hash
    {
        eyre::bail!("finalized active binding and staged policy were read at different heads");
    }
    let policy_hash = successor
        .policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("hash staged successor policy: {error}"))?;
    let staged_policy_matches = policy_hash == context.successor_policy_hash
        && successor.policy.activation_height == context.activation_height;
    if !staged_policy_matches
        || evidence.intent().policy_hash != policy_hash
        || evidence.intent().operation != AttestationOperationV1::TransitionEnclaveMeasurement
    {
        eyre::bail!("durable candidate submission targets another transition policy");
    }
    proof
        .verify_for_transition(evidence.intent(), active.tribute_offer_public.into())
        .map_err(|error| eyre::eyre!("durable key-ready proof is invalid: {error}"))?;
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
        .map_err(|error| eyre::eyre!("calculate normative transition gas: {error}"))?;
    let chain_id = rpc.chain_id().await?;
    let account_nonce = rpc.transaction_count(relay.address()).await?;
    let gas_price = buffered_gas_price(rpc.gas_price().await?);
    let required_balance = gas_price.saturating_mul(U256::from(gas_limit));
    let balance = rpc.balance(relay.address()).await?;
    if balance < required_balance {
        eyre::bail!(
            "upgrade relay {} has {balance} but needs at least {required_balance}",
            relay.address()
        );
    }
    let raw = relay.sign_renewal(
        chain_id,
        account_nonce,
        gas_price,
        gas_limit,
        TEE_REGISTRY_ADDRESS,
        &calldata,
    )?;
    Ok((calldata, raw))
}

fn load_key_ready_material(
    node_data_dir: &Path,
) -> Result<(
    UpgradeContextV1,
    ReplacementCandidateSubmissionV1,
    AttestationEvidenceV1,
)> {
    let snapshot = inspect_upgrade_journal_v1(node_data_dir)?
        .ok_or_else(|| eyre::eyre!("upgrade candidate is not prepared"))?;
    let UpgradeJournalStateV1::CandidateKeyReady {
        context, security, ..
    } = snapshot.lifecycle
    else {
        eyre::bail!("upgrade relay requires candidate-key-ready checkpoint");
    };
    let durable = load_replacement_candidate_submission(node_data_dir)
        .map_err(|error| eyre::eyre!("reload exact candidate submission: {error}"))?
        .ok_or_else(|| eyre::eyre!("candidate-key-ready checkpoint has no durable submission"))?;
    let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
        .map_err(|error| eyre::eyre!("decode durable transition evidence: {error}"))?;
    let proof = evidence
        .transition_key_ready_proof()
        .ok_or_else(|| eyre::eyre!("durable transition evidence has no key-ready proof"))?;
    let encoded_proof = proof
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode durable key-ready proof: {error}"))?;
    if keccak256(encoded_proof) != security.proof_hash
        || B256::from(proof.resident_offer_public) != security.resident_offer_public
    {
        eyre::bail!("durable transition proof differs from the journaled key-ready checkpoint");
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
        .map_err(|error| eyre::eyre!("reload exact candidate submission: {error}"))?
        .ok_or_else(|| eyre::eyre!("submission checkpoint has no durable NodeHost material"))?;
    let durable_evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
        .map_err(|error| eyre::eyre!("decode durable transition evidence: {error}"))?;
    let (intent_hash, evidence_hash) = transition_commitments(&durable_evidence, &durable)?;
    let calldata = transition_calldata(&durable);
    if intent_hash != submission.intent_hash
        || evidence_hash != submission.evidence_hash
        || keccak256(calldata) != submission.calldata_hash
    {
        eyre::bail!("NodeHost transition material differs from the relay checkpoint");
    }
    let view = read_finalized_bound_renewal_view_v1(rpc, selector).await?;
    ensure_transition_source_or_target_v1(&view.binding, durable_evidence.intent())?;
    Ok(transition_target_matches_v1(
        &view.binding,
        durable_evidence.intent(),
    ))
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
        .map_err(|error| eyre::eyre!("hash durable transition intent: {error}"))?;
    let evidence_hash = AttestationEvidenceV1::decode_canonical(durable.evidence())
        .and_then(|e| e.evidence_hash())
        .map_err(|code| eyre::eyre!("hash durable transition evidence: {code:?}"))?;
    Ok((intent_hash, evidence_hash))
}
