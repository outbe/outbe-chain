//! Network provisioning of a staged enclave; never copies a predecessor seal.
use super::join::load_finalized_admission_anchor_v1;
use super::join::{classify_join_offer_key_state_transport, JoinOfferKeyState};
use super::upgrade::connect_upgrade_candidate_v1;
use super::*;
use alloy_consensus::BlockHeader as _;
use alloy_primitives::keccak256;
use alloy_primitives::{B256, U256};
use alloy_sol_types::{SolCall, SolValue};
use eyre::WrapErr;
use outbe_operator::tee::{
    generate_transition_evidence_v1, read_finalized_bound_renewal_view_v1,
    record_network_key_provisioned_v1, transition_intent_v1, NetworkUpgradeSubmissionV1,
    UpgradeJournalGuardV1,
};
use outbe_operator::tee::{read_finalized_upgrade_policy_v1, UpgradeJournalStateV1};
use outbe_operator::tx::{buffered_gas_price, RelaySignerV1};
use outbe_primitives::addresses::TEE_REGISTRY_ADDRESS;
use outbe_primitives::reshare_artifact::{decode_outbe_block_artifacts, ConsensusHeaderArtifact};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationOperationV1, RegistryMutatorV1, TeeRegistryGasScheduleV1,
};
use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
use outbe_tee::dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1};
use outbe_tee::finalized_admission::{
    FinalizedAdmissionWitnessV1, MptAccountProofV1, MptStorageProofV1,
};
use outbe_tee::load_committed_enclave_manifest_v1;
use outbe_tee::protocol::EnclaveRequest;
use outbe_tee::protocol::EnclaveResponse;
use outbe_tee::upgrade_transfer::UpgradeKeyProofV1;
use std::path::Path;
use std::time::{Duration, Instant};

#[allow(clippy::too_many_arguments)]
pub(super) async fn provision(
    rpc: &(impl Rpc + Sync),
    private_key: Option<&str>,
    endpoint: &str,
    node_dir: &Path,
    node_key: Option<&Path>,
    genesis: &Path,
    binding: &str,
    valid_until: u64,
    timeout: Duration,
    legacy_source: bool,
    new_attempt: bool,
) -> Result<()> {
    let relay = RelaySignerV1::new(
        private_key.ok_or_else(|| eyre::eyre!("upgrade-provision requires --private-key"))?,
    )?;
    let committed = load_committed_enclave_manifest_v1(node_dir)?;
    let (mut candidate, signing, selector) = connect_upgrade_candidate_v1(
        endpoint,
        node_dir,
        &committed,
        rpc.eth_chain_id().await?,
        node_key,
    )?;
    let binding_id = parse_nonzero_b256(binding, "--binding-id")?;
    let manifest_hash = candidate
        .manifest()
        .authorization_hash()
        .map_err(|e| eyre::eyre!("manifest: {e}"))?;
    let durable = {
        // Also serializes with ordinary renewal preparation. No lock is held
        // while waiting for finality or downloading the committee history.
        let guard = UpgradeJournalGuardV1::acquire(node_dir)?;
        let journal = guard
            .load()?
            .ok_or_else(|| eyre::eyre!("run upgrade-prepare first"))?;
        if journal.lifecycle.context().candidate_manifest_hash != manifest_hash {
            eyre::bail!("connected candidate differs from prepared upgrade");
        }
        if !matches!(
            journal.lifecycle,
            UpgradeJournalStateV1::CandidatePrepared { .. }
                | UpgradeJournalStateV1::KeyProvisioned { .. }
        ) {
            eyre::bail!(
                "candidate has already advanced to {}; use upgrade-submit or upgrade-finalize",
                journal.lifecycle.label()
            );
        }
        let active = read_finalized_bound_renewal_view_v1(&CliFinalityRpc(rpc), &selector).await?;
        let existing = guard.load_network_submission()?;
        let existing = existing.filter(|v| v.candidate_manifest_hash == manifest_hash);
        let previous_transaction = existing.as_ref().map(|v| v.transaction.clone());
        let durable = if let Some(value) = existing {
            let evidence = AttestationEvidenceV1::decode_canonical(&value.evidence)
                .map_err(|e| eyre::eyre!("saved prepare evidence: {e}"))?;
            if evidence.intent().binding_id != binding_id {
                eyre::bail!(
                    "saved candidate uses another binding ID; retain the original --binding-id"
                );
            }
            if new_attempt {
                let pending = pending_at(
                    rpc,
                    evidence
                        .intent()
                        .node_id
                        .node_id_hash()
                        .map_err(|e| eyre::eyre!("node: {e}"))?,
                    &format!("0x{:x}", active.schedule.finalized_height),
                )
                .await?;
                if !pending.contextHash.is_zero()
                    && pending.validUntil > active.schedule.finalized_timestamp
                {
                    eyre::bail!("cancel the live candidate before --new-attempt");
                }
                if pending.nonce < evidence.intent().transition_nonce
                    && active.schedule.finalized_timestamp < evidence.intent().requested_valid_until
                {
                    eyre::bail!("previous prepare is not finalized or expired; replay it before starting another");
                }
            }
            if new_attempt
                || active.schedule.finalized_timestamp >= evidence.intent().requested_valid_until
            {
                None // Next nonce and fresh evidence; the expired authority cannot be reused.
            } else {
                Some(value)
            }
        } else {
            None
        };
        if let Some(value) = durable {
            value
        } else {
            let successor = read_finalized_upgrade_policy_v1(&CliFinalityRpc(rpc))
                .await?
                .ok_or_else(|| eyre::eyre!("no approved successor"))?;
            if successor.finalized_hash != active.schedule.finalized_hash {
                eyre::bail!("finalized head advanced during preparation; retry");
            }
            if successor
                .policy
                .policy_hash()
                .map_err(|e| eyre::eyre!("policy: {e}"))?
                != journal.lifecycle.context().successor_policy_hash
            {
                eyre::bail!("approved successor changed");
            }
            let tag = format!("0x{:x}", active.schedule.finalized_height);
            let node = candidate
                .manifest()
                .node_id
                .node_id_hash()
                .map_err(|e| eyre::eyre!("node: {e}"))?;
            let pending = pending_at(rpc, node, &tag).await?;
            if !pending.contextHash.is_zero()
                && pending.validUntil > active.schedule.finalized_timestamp
            {
                eyre::bail!("a live candidate already exists; recover its saved submission or cancel it explicitly");
            }
            let mut intent = transition_intent_v1(
                &candidate,
                &active,
                &successor.policy,
                binding_id,
                valid_until,
            )?;
            intent.operation = AttestationOperationV1::PrepareEnclaveUpgrade;
            intent.transition_nonce = pending
                .nonce
                .checked_add(1)
                .ok_or_else(|| eyre::eyre!("prepare nonce exhausted"))?;
            let prepared =
                generate_transition_evidence_v1(&mut candidate, intent, &successor.policy)?;
            let ceiling = prepared
                .collateral_expiration
                .checked_sub(successor.policy.collateral_margin)
                .ok_or_else(|| eyre::eyre!("collateral safety margin"))?;
            let lease = valid_until
                .checked_sub(active.schedule.finalized_timestamp)
                .ok_or_else(|| eyre::eyre!("candidate already expired"))?;
            if lease < successor.policy.minimum_lease
                || lease > successor.policy.maximum_lease
                || valid_until > ceiling
                || prepared.collateral_issue_floor > active.schedule.finalized_timestamp
            {
                eyre::bail!("candidate lease is outside policy/collateral limits; choose a valid --valid-until");
            }
            let evidence = prepared
                .evidence
                .encode_canonical()
                .map_err(|e| eyre::eyre!("evidence: {e}"))?;
            let intent = &prepared.intent;
            let context = DcapOnboardingContextV1 {
                chain_id: intent.chain_id,
                genesis_hash: intent.genesis_hash,
                intent_hash: intent
                    .intent_hash()
                    .map_err(|e| eyre::eyre!("intent: {e}"))?,
                node_id_hash: node,
                enclave_id: intent.enclave_id,
                binding_id,
                policy_hash: intent.policy_hash,
                recipient_x25519: intent.recipient_x25519,
                tribute_offer_public: active.tribute_offer_public.0,
                key_epoch: scalar_at(rpc, ITeeRegistryV1::keyEpochCall {}.abi_encode(), &tag)
                    .await?,
                tribute_offer_epoch: scalar_at(
                    rpc,
                    ITeeRegistryV1::tributeOfferEpochCall {}.abi_encode(),
                    &tag,
                )
                .await?,
            };
            let signature =
                sign_node_hash(&signing, context.intent_hash).map_err(|e| eyre::eyre!(e))?;
            let calldata = ITeeRegistryV1::prepareEnclaveUpgradeCall {
                evidence: evidence.clone().into(),
                nodeSignature: signature.to_vec().into(),
                enclaveSignature: prepared.enclave_signature.to_vec().into(),
            }
            .abi_encode();
            let gas = TeeRegistryGasScheduleV1::normative()
                .maximum_transaction_gas(
                    RegistryMutatorV1::PrepareEnclaveUpgrade,
                    calldata.len(),
                    evidence.len(),
                    successor.policy.measurement_rules.len(),
                    successor.policy.attestation_mode,
                )
                .map_err(|e| eyre::eyre!("prepare gas: {e}"))?;
            let nonce = rpc.eth_get_transaction_count(relay.address()).await?;
            let mut price = buffered_gas_price(rpc.eth_gas_price().await?);
            if let Some(previous) = previous_transaction.filter(|t| t.account_nonce == nonce) {
                price = price.max(
                    previous.gas_price.saturating_mul(U256::from(1125)) / U256::from(1000)
                        + U256::from(1),
                );
            }
            if rpc.eth_get_balance(relay.address()).await? < price.saturating_mul(U256::from(gas)) {
                eyre::bail!("insufficient balance for prepare transaction");
            }
            let transaction = relay.sign_renewal(
                rpc.eth_chain_id().await?,
                nonce,
                price,
                gas,
                TEE_REGISTRY_ADDRESS,
                &calldata,
            )?;
            let value = NetworkUpgradeSubmissionV1 {
                candidate_manifest_hash: manifest_hash,
                evidence,
                context: context.encode_canonical(),
                calldata,
                transaction,
            };
            guard.store_network_submission(&value)?;
            value
        }
    };
    if durable.transaction.relay != relay.address()
        || keccak256(&durable.transaction.raw_transaction) != durable.transaction.transaction_hash
    {
        eyre::bail!("saved prepare transaction does not match this relay");
    }
    let context = DcapOnboardingContextV1::decode_canonical(&durable.context)
        .map_err(|e| eyre::eyre!("saved context: {e:?}"))?;
    let expected = context.context_hash();
    let started = Instant::now();
    let mut sent = false;
    let finalized_height = loop {
        let block = rpc.eth_get_finalized_block().await?;
        let height = json_hex_u64_field(&block, "number")?;
        let now = json_hex_u64_field(&block, "timestamp")?;
        let pending = pending_at(rpc, context.node_id_hash, &format!("0x{height:x}")).await?;
        if pending.contextHash == expected && pending.validUntil > now {
            break height;
        }
        if started.elapsed() >= timeout {
            eyre::bail!(
                "prepare not finalized; saved transaction retained, rerun upgrade-provision"
            );
        }
        if !sent {
            match rpc
                .eth_send_raw_transaction(&durable.transaction.raw_transaction)
                .await
            {
                Ok(hash) if hash.parse::<B256>()? == durable.transaction.transaction_hash => {}
                Ok(_) => eyre::bail!("RPC returned a different prepare transaction hash"),
                Err(e) if e.to_string().to_ascii_lowercase().contains("already known") => {}
                Err(e) => return Err(e).wrap_err("send saved prepare transaction"),
            }
            sent = true;
        }
        if let Some(receipt) = rpc
            .eth_get_transaction_receipt(&format!("{:#x}", durable.transaction.transaction_hash))
            .await?
        {
            if json_hex_u64_field(&receipt, "status")? == 0 {
                eyre::bail!("prepare transaction reverted; saved evidence retained for diagnosis");
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    };
    match classify_join_offer_key_state_transport(
        candidate.request(&EnclaveRequest::GetPublicKeys)?,
        context.tribute_offer_public,
    )? {
        JoinOfferKeyState::ReadyExact => {}
        JoinOfferKeyState::ReadyMismatch => {
            eyre::bail!("candidate already contains a different network key")
        }
        JoinOfferKeyState::Keyless => {
            let proof = collect_proof(rpc, genesis, finalized_height, &context).await?;
            refresh_call_context(rpc, None).await?;
            let artifact = rpc
                .upgrade_key_v1(&durable.context, &proof, legacy_source)
                .await?;
            let decoded = DcapOnboardingArtifactV1::decode_canonical(&artifact)
                .map_err(|e| eyre::eyre!("export artifact: {e:?}"))?;
            if decoded.context != context {
                eyre::bail!("source returned a different recipient");
            }
            let response = candidate.ingest_upgrade_key_v1(&proof, &artifact)?;
            if !matches!(response, EnclaveResponse::FinalizedAdmissionIngestedV1 { tribute_offer_public, .. } if tribute_offer_public == context.tribute_offer_public)
            {
                eyre::bail!("candidate installed an unexpected network key");
            }
        }
    }
    let snapshot = record_network_key_provisioned_v1(node_dir)?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    println!("network key received and sealed by candidate; next: upgrade-submit with the same binding ID");
    Ok(())
}

async fn pending_at(
    rpc: &(impl Rpc + Sync),
    node: B256,
    tag: &str,
) -> Result<ITeeRegistryV1::pendingEnclaveUpgradeReturn> {
    let bytes = rpc
        .eth_call_at(
            TEE_REGISTRY_ADDRESS,
            &ITeeRegistryV1::pendingEnclaveUpgradeCall { nodeIdHash: node }.abi_encode(),
            tag,
        )
        .await?;
    Ok(ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_decode_returns(&bytes)?)
}
async fn scalar_at(rpc: &(impl Rpc + Sync), call: Vec<u8>, tag: &str) -> Result<u64> {
    let bytes = rpc.eth_call_at(TEE_REGISTRY_ADDRESS, &call, tag).await?;
    let value = U256::abi_decode(&bytes)?;
    u64::try_from(value).map_err(|_| eyre::eyre!("registry epoch exceeds u64"))
}

async fn collect_proof(
    rpc: &(impl Rpc + Sync),
    genesis: &Path,
    finalized_height: u64,
    context: &DcapOnboardingContextV1,
) -> Result<UpgradeKeyProofV1> {
    let mut transitions = Vec::new();
    // Capture the exact state proof before scanning history while the head advances.
    let slots = outbe_tee::finalized_admission::upgrade_registry_slots_v1(context);
    let slot_params = slots
        .iter()
        .map(|slot| format!("{slot:#x}"))
        .collect::<Vec<_>>();
    let (finalized_height, opening) =
        admission_history::registry_opening(rpc, finalized_height, &slot_params).await?;
    let anchor =
        load_finalized_admission_anchor_v1(rpc, genesis, finalized_height, context).await?;

    let mut next_transition_epoch = 1_u64;
    let mut admission = None;
    for height in 1..=finalized_height {
        let Some(public) = admission_history::admission_public(
            rpc,
            height,
            finalized_height,
            next_transition_epoch,
        )
        .await?
        else {
            continue;
        };
        let (block, compact) = admission_history::compact_header(&public, height)?;
        let artifacts = decode_outbe_block_artifacts(block.header().extra_data().as_ref())
            .map_err(|error| eyre::eyre!("decode block {height} artifacts: {error:?}"))?;
        if let Some(ConsensusHeaderArtifact::CommitteePreAnnounce { epoch, .. }) =
            artifacts.consensus_header_artifact
        {
            if height < finalized_height && epoch == next_transition_epoch {
                let transition = compact
                    .clone()
                    .encode_canonical()
                    .map_err(|error| eyre::eyre!("encode committee transition: {error}"))?;
                transitions.push(transition.into());
                if transitions.len() > outbe_tee::upgrade_transfer::MAX_UPGRADE_COMMITTEES {
                    eyre::bail!("upgrade committee proof exceeds count limit");
                }
                next_transition_epoch = next_transition_epoch
                    .checked_add(1)
                    .ok_or_else(|| eyre::eyre!("committee transition epoch overflow"))?;
            }
        }
        if height == finalized_height {
            admission = Some(compact);
        }
    }

    let registry_account = MptAccountProofV1 {
        nonce: json_hex_u64_field(&opening, "nonce")?,
        balance: json_hex_u256_field(&opening, "balance")?,
        code_hash: json_b256_field(&opening, "codeHash")?,
        storage_root: json_b256_field(&opening, "storageHash")?,
        nodes: json_hex_array(&opening, "accountProof")?,
    };
    let storage = opening
        .get("storageProof")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| eyre::eyre!("eth_getProof has no storageProof array"))?;
    let mut registry_storage = Vec::with_capacity(storage.len());
    for item in storage {
        registry_storage.push(MptStorageProofV1 {
            key: json_b256_field(item, "key")?,
            value: json_hex_u256_field(item, "value")?,
            nodes: json_hex_array(item, "proof")?,
        });
    }
    if registry_storage.len() != slots.len() {
        return Err(eyre::eyre!("eth_getProof omitted a required TeeRegistry slot").into());
    }
    let admission_witness = FinalizedAdmissionWitnessV1 {
        admission: admission.expect("positive finalized height sets admission proof"),
        registry_account,
        registry_storage,
    }
    .encode_canonical()
    .map_err(|error| eyre::eyre!("encode finalized onboarding admission witness: {error}"))?;

    let proof = UpgradeKeyProofV1 {
        anchor_outcome: anchor.into(),
        committee_transitions: transitions,
        admission: admission_witness.into(),
    };
    proof.validate()?;
    Ok(proof)
}
