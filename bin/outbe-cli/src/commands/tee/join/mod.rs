use super::authorize_validator_node_binding;
use super::call_u256;
use super::compressed_public_key;
use super::load_secp256k1_key_file;
use super::parse_nonzero_b256;
use super::sign_node_hash;
use super::CliFinalityRpc;
use crate::abi::ITeeRegistry;
use crate::rpc::Rpc;

use alloy_primitives::B256;
use alloy_sol_types::SolCall;
use eyre::{Result, WrapErr};
use outbe_operator::tee::{read_finalized_registry_view_v1, NodeBindingSelectorV1};
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapEvidenceV1,
    GramineDirectEvidenceV1, NodeIdV1, RegistrationIntentV1, RegistryMutatorV1, TeePolicyV1,
    TeeRegistryGasScheduleV1,
};
use outbe_tee::protocol::EnclaveRequest;
use outbe_tee::{
    acquire_dcap_collateral_v1, persist_committed_join_submission,
    persist_replacement_candidate_submission,
};
use std::path::Path;
use std::time::Duration;

mod policy;
pub(super) use policy::{
    classify_join_offer_key_state, classify_join_offer_key_state_transport,
    committed_manifest_matches_binding, ensure_durable_join_registration_caller,
    ensure_joinable_binding, finalized_binding_matches_intent, plan_join_completion,
    plan_missing_committed_relay, registration_counters, select_join_transport,
    DurableJoinSubmissionV1, ExactJoinRelayV1, JoinOfferKeyState, JoinTransport,
    MissingCommittedRelayPlan,
};

#[cfg(test)]
pub(super) use policy::JoinCompletionPlan;

mod enclave;
pub(super) use enclave::JoinEnclave;
use enclave::{connect_join_enclave, EnclaveConnectionContext, EnclaveIdentityKeys};

mod admission;
pub(super) use admission::{
    load_finalized_admission_anchor_v1, persist_authorized_join_admission_anchor_v1,
    run_finalized_admission_recovery_v1, CliFinalizedAdmissionIoV1,
};

#[cfg(test)]
pub(super) use admission::{
    finalized_join_admission_anchor_v1, FinalizedAdmissionAttemptErrorV1,
    FinalizedAdmissionRecoveryIoV1,
};

mod relay;
pub(super) use relay::{await_finalized_join_target, relay_exact_join_transaction};
use relay::{submit_or_resume_join_tx, JoinRelayContext};

mod preflight;
use preflight::{
    evaluate_replay_and_lease, load_durable_checkpoint, resolve_join_policy,
    verify_finalized_node_ownership, PreflightOutcome, ReplayCheckInput,
};

mod completion;
use completion::{
    finalize_and_reopen_join, ingest_onboarding_offer_key, JoinFinalizationContext,
    OnboardingIngestContext,
};

pub(super) struct TeeJoinArgs<'a> {
    pub(super) private_key: Option<&'a str>,
    pub(super) enclave_socket: &'a str,
    pub(super) node_data_dir: Option<&'a Path>,
    pub(super) reth_p2p_secret_key: Option<&'a Path>,
    pub(super) genesis: &'a Path,
    pub(super) binding_id: &'a str,
    pub(super) valid_until: u64,
    pub(super) timeout_secs: u64,
    pub(super) successor_policy: bool,
}

pub(super) async fn join(client: &(impl Rpc + Sync), args: TeeJoinArgs<'_>) -> Result<()> {
    let TeeJoinArgs {
        private_key,
        enclave_socket,
        node_data_dir,
        reth_p2p_secret_key,
        genesis,
        binding_id,
        valid_until,
        timeout_secs,
        successor_policy,
    } = args;
    let private_key_hex = private_key
        .ok_or_else(|| eyre::eyre!("tee join requires the global --private-key EVM signer"))?;
    let evm_signer = super::require_signer(Some(private_key_hex))?;
    let rpc_chain_id = client.eth_chain_id().await?;
    let binding_id = parse_nonzero_b256(binding_id, "--binding-id")?;
    let path = reth_p2p_secret_key
        .ok_or_else(|| eyre::eyre!("tee join requires --reth-p2p-secret-key"))?;
    let node_signing_key = load_secp256k1_key_file(path)?;
    let reth_p2p_public = compressed_public_key(&node_signing_key)?;
    let node_id = NodeIdV1 { reth_p2p_public };
    let node_id_hash = node_id
        .node_id_hash()
        .map_err(|error| eyre::eyre!("hash V1 node identity: {error}"))?;
    let binding_selector = NodeBindingSelectorV1::NodeHost(reth_p2p_public);
    let finalized = read_finalized_registry_view_v1(&CliFinalityRpc(client), &binding_selector)
        .await
        .wrap_err("read exact finalized TeeRegistry join state")?;
    let policy = resolve_join_policy(client, rpc_chain_id, &finalized, successor_policy).await?;
    verify_finalized_node_ownership(client, &finalized, node_id_hash, evm_signer.address()).await?;
    let (mut durable_submission, durable_resume_intent) =
        load_durable_checkpoint(node_data_dir, evm_signer.address())?;
    let resumes_finalized_target = match evaluate_replay_and_lease(ReplayCheckInput {
        enclave_socket,
        node_data_dir,
        finalized: &finalized,
        durable_resume_intent: durable_resume_intent.as_ref(),
        binding_id,
        valid_until,
        policy: &policy,
    })? {
        PreflightOutcome::CompletedReplay => return Ok(()),
        PreflightOutcome::Proceed {
            resumes_finalized_target,
        } => resumes_finalized_target,
    };

    let (validator_binding, validator_signature, node_binding_signature) =
        encode_signed_validator_binding(&policy, node_id_hash, &evm_signer, &node_signing_key)?;
    let expected_offer_pub: [u8; 32] = finalized.tribute_offer_public.into();
    let key_epoch = call_u256(client, ITeeRegistry::keyEpochCall {}.abi_encode())
        .await?
        .to::<u64>();
    let tribute_offer_epoch =
        call_u256(client, ITeeRegistry::tributeOfferEpochCall {}.abi_encode())
            .await?
            .to::<u64>();
    let policy_hash = policy
        .policy_hash()
        .map_err(|error| eyre::eyre!("active V1 policy is invalid: {error}"))?;
    if !resumes_finalized_target {
        ensure_joinable_binding(
            finalized.binding.as_ref(),
            finalized.schedule.finalized_timestamp,
        )?;
    }

    let join_transport = select_join_transport(policy.attestation_mode, node_data_dir.is_some())?;
    let connected = connect_join_enclave(EnclaveConnectionContext {
        join_transport,
        enclave_socket,
        node_data_dir,
        policy: &policy,
        has_finalized_binding: finalized.binding.is_some(),
        reth_p2p_public,
        node_id: node_id.clone(),
        node_signing_key: &node_signing_key,
    })?;
    let mut enclave = connected.enclave;
    let keys = connected.keys;

    let offer_key_state = classify_join_offer_key_state(
        enclave.request(&EnclaveRequest::GetPublicKeys)?,
        expected_offer_pub,
    )?;
    let completion_plan = plan_join_completion(offer_key_state, enclave.is_candidate())?;
    ensure_matching_checkpoint_lifecycle(durable_submission.as_ref(), enclave.is_candidate())?;

    let source_binding = if resumes_finalized_target {
        None
    } else {
        finalized.binding.as_ref()
    };
    let intent = build_or_resume_intent(IntentBuildContext {
        durable_resume_intent,
        source_binding,
        policy: &policy,
        policy_hash,
        node_id,
        binding_id,
        valid_until,
        keys: &keys,
    })?;
    let intent_hash = intent
        .intent_hash()
        .map_err(|error| eyre::eyre!("invalid V1 registration intent: {error}"))?;
    let fresh_node_signature = sign_node_hash(&node_signing_key, intent_hash)
        .map_err(|error| eyre::eyre!("sign V1 node intent: {error}"))?;

    let (evidence, node_signature, enclave_signature) = prepare_join_evidence(
        &mut enclave,
        &mut durable_submission,
        EvidencePrepContext {
            policy: &policy,
            intent: &intent,
            fresh_node_signature: &fresh_node_signature,
            attestation_ed25519: keys.attestation_ed25519,
            join_transport,
            offer_key_state,
            node_data_dir,
            signer_address: evm_signer.address(),
        },
    )?;
    let node_id_hash = decode_evidence_node_id_hash(&evidence)?;
    let call = ITeeRegistry::registerEnclaveCall {
        evidence: evidence.clone().into(),
        nodeSignature: node_signature.to_vec().into(),
        enclaveSignature: enclave_signature.to_vec().into(),
        validatorNodeBinding: validator_binding.into(),
        validatorSignature: validator_signature.to_vec().into(),
        nodeBindingSignature: node_binding_signature.to_vec().into(),
    }
    .abi_encode();
    let gas_limit = TeeRegistryGasScheduleV1::normative()
        .maximum_transaction_gas(
            RegistryMutatorV1::RegisterEnclave,
            call.len(),
            evidence.len(),
            policy.measurement_rules.len(),
            policy.attestation_mode,
        )
        .map_err(|error| eyre::eyre!("calculate V1 registration gas: {error}"))?;

    let is_committed_durable = durable_submission
        .as_ref()
        .is_some_and(DurableJoinSubmissionV1::is_committed);
    let mut enclave_slot = Some(enclave);
    let Some(tx_hash) = submit_or_resume_join_tx(
        client,
        &mut enclave_slot,
        JoinRelayContext {
            private_key_hex,
            evm_signer: &evm_signer,
            rpc_chain_id,
            node_data_dir,
            enclave_socket,
            call,
            gas_limit,
            resumes_finalized_target,
            offer_key_state,
            expected_offer_pub,
            finalized: &finalized,
            node_id_hash,
            enclave_id: keys.enclave_id,
            intent_hash,
            is_committed_durable,
        },
    )
    .await?
    else {
        return Ok(());
    };
    let mut enclave = enclave_slot.expect("enclave remains open when transaction is relayed");
    println!(
        "V1 registerEnclave submitted by {}: {tx_hash}",
        evm_signer.address()
    );

    let finalized_join = await_finalized_join_target(
        client,
        &binding_selector,
        &intent,
        Duration::from_secs(timeout_secs),
    )
    .await?;

    let authorized_node_data_dir = persist_admission_anchor_if_authorized(
        join_transport,
        node_data_dir,
        &finalized_join,
        (node_id_hash, keys.enclave_id, intent_hash),
    )?;

    let tribute_offer_public = if completion_plan.ingest_offer_key {
        ingest_onboarding_offer_key(
            client,
            &mut enclave,
            OnboardingIngestContext {
                binding_selector: &binding_selector,
                policy: &policy,
                node_id_hash,
                enclave_id: keys.enclave_id,
                intent_hash,
                recipient_x25519: keys.recipient_x25519,
                expected_offer_pub,
                key_epoch,
                tribute_offer_epoch,
                tx_hash: &tx_hash,
                timeout_secs,
                genesis,
                authorized_node_data_dir,
                enclave_socket,
                reth_p2p_public,
                node_signing_key: &node_signing_key,
            },
        )
        .await?
    } else {
        expected_offer_pub
    };

    finalize_and_reopen_join(
        client,
        enclave,
        JoinFinalizationContext {
            join_transport,
            authorized_node_data_dir,
            promote_candidate: completion_plan.promote_candidate,
            binding_selector: &binding_selector,
            intent: &intent,
            intent_hash,
            enclave_socket,
            expected_offer_pub,
            tribute_offer_public,
            durable_submission: durable_submission.as_ref(),
        },
    )
    .await
}

fn encode_signed_validator_binding(
    policy: &TeePolicyV1,
    node_id_hash: B256,
    evm_signer: &crate::tx::TxSigner,
    node_signing_key: &k256::ecdsa::SigningKey,
) -> Result<(Vec<u8>, [u8; 65], [u8; 65])> {
    let (validator_binding, validator_binding_hash, validator_signature) =
        authorize_validator_node_binding(
            policy.chain_id,
            policy.genesis_hash,
            node_id_hash,
            evm_signer,
        )?;
    let node_binding_signature = sign_node_hash(node_signing_key, validator_binding_hash)
        .map_err(|error| eyre::eyre!("sign NodeHost side of address binding: {error}"))?;
    let encoded_binding = validator_binding
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode address-to-NodeHost binding: {error}"))?;
    Ok((encoded_binding, validator_signature, node_binding_signature))
}

fn ensure_matching_checkpoint_lifecycle(
    durable_submission: Option<&DurableJoinSubmissionV1>,
    is_candidate: bool,
) -> Result<()> {
    if durable_submission.is_some_and(|submission| submission.is_candidate() != is_candidate) {
        eyre::bail!("durable tee join checkpoint targets another enclave lifecycle");
    }
    Ok(())
}

struct IntentBuildContext<'a> {
    durable_resume_intent: Option<RegistrationIntentV1>,
    source_binding: Option<&'a outbe_operator::tee::RenewalBindingV1>,
    policy: &'a TeePolicyV1,
    policy_hash: B256,
    node_id: NodeIdV1,
    binding_id: B256,
    valid_until: u64,
    keys: &'a EnclaveIdentityKeys,
}

fn build_or_resume_intent(ctx: IntentBuildContext<'_>) -> Result<RegistrationIntentV1> {
    let (binding_version, registration_version, renewal_nonce, transition_nonce) =
        registration_counters(ctx.source_binding)?;
    let fresh_intent = RegistrationIntentV1 {
        chain_id: ctx.policy.chain_id,
        genesis_hash: ctx.policy.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: ctx.policy.attestation_mode,
        policy_hash: ctx.policy_hash,
        node_id: ctx.node_id,
        enclave_id: ctx.keys.enclave_id,
        binding_id: ctx.binding_id,
        binding_version,
        registration_version,
        renewal_nonce,
        transition_nonce,
        requested_valid_until: ctx.valid_until,
        recipient_x25519: ctx.keys.recipient_x25519,
        attestation_ed25519: ctx.keys.attestation_ed25519,
        noise_responder_x25519: ctx.keys.noise_responder_x25519,
        node_host_authorization_hash: ctx.keys.node_host_authorization_hash,
    };
    let intent = ctx.durable_resume_intent.unwrap_or(fresh_intent);
    let actual_fields = (
        intent.binding_id,
        intent.requested_valid_until,
        intent.enclave_id,
        intent.recipient_x25519,
        intent.attestation_ed25519,
        intent.noise_responder_x25519,
        intent.node_host_authorization_hash,
    );
    let expected_fields = (
        ctx.binding_id,
        ctx.valid_until,
        ctx.keys.enclave_id,
        ctx.keys.recipient_x25519,
        ctx.keys.attestation_ed25519,
        ctx.keys.noise_responder_x25519,
        ctx.keys.node_host_authorization_hash,
    );
    if actual_fields != expected_fields {
        eyre::bail!("requested tee join does not match its durable checkpoint");
    }
    Ok(intent)
}

struct EvidencePrepContext<'a> {
    policy: &'a TeePolicyV1,
    intent: &'a RegistrationIntentV1,
    fresh_node_signature: &'a [u8; 65],
    attestation_ed25519: [u8; 32],
    join_transport: JoinTransport,
    offer_key_state: JoinOfferKeyState,
    node_data_dir: Option<&'a Path>,
    signer_address: alloy_primitives::Address,
}

fn prepare_join_evidence(
    enclave: &mut JoinEnclave,
    durable_submission: &mut Option<DurableJoinSubmissionV1>,
    ctx: EvidencePrepContext<'_>,
) -> Result<(Vec<u8>, [u8; 65], [u8; 64])> {
    let (evidence_value, node_signature, enclave_signature) = if let Some(durable) =
        durable_submission.as_ref()
    {
        let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
            .map_err(|error| eyre::eyre!("decode durable join evidence: {error}"))?;
        let durable_intent = match &evidence {
            AttestationEvidenceV1::Dcap(value) => &value.intent,
            AttestationEvidenceV1::GramineDirectDev(value) => &value.intent,
        };
        if durable_intent != ctx.intent || durable.node_signature() != ctx.fresh_node_signature {
            eyre::bail!("requested tee join conflicts with the durable registration");
        }
        (
            evidence,
            *durable.node_signature(),
            *durable.enclave_signature(),
        )
    } else {
        let (evidence, signature) =
            generate_fresh_evidence(enclave, ctx.policy, ctx.intent, ctx.attestation_ed25519)?;
        persist_fresh_submission_if_needed(
            durable_submission,
            enclave.is_candidate(),
            &ctx,
            &evidence,
            &signature,
        )?;
        (evidence, *ctx.fresh_node_signature, signature)
    };
    let encoded = evidence_value
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode canonical V1 evidence: {error}"))?;
    Ok((encoded, node_signature, enclave_signature))
}

fn generate_fresh_evidence(
    enclave: &mut JoinEnclave,
    policy: &TeePolicyV1,
    intent: &RegistrationIntentV1,
    attestation_ed25519: [u8; 32],
) -> Result<(AttestationEvidenceV1, [u8; 64])> {
    match policy.attestation_mode {
        AttestationMode::DcapRequired => {
            let generated = enclave.generate_dcap_quote(intent)?;
            let components = acquire_dcap_collateral_v1(&generated.quote_body)
                .map_err(|error| eyre::eyre!("acquire canonical DCAP collateral: {error}"))?;
            let signature = generated.enclave_signature;
            Ok((
                AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
                    intent: intent.clone(),
                    quote: generated.quote_body,
                    components,
                    transition_key_ready_proof: generated.transition_key_ready_proof,
                }),
                signature,
            ))
        }
        AttestationMode::GramineDirectDev => {
            let signature = enclave.sign_registration_intent_dev_v1(intent)?;
            Ok((
                AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
                    transition_key_ready_proof: None,
                    intent: intent.clone(),
                    dev_attestation_public: attestation_ed25519,
                    dev_signature: signature,
                }),
                signature,
            ))
        }
    }
}

fn persist_fresh_submission_if_needed(
    durable_submission: &mut Option<DurableJoinSubmissionV1>,
    is_candidate: bool,
    ctx: &EvidencePrepContext<'_>,
    evidence: &AttestationEvidenceV1,
    signature: &[u8; 64],
) -> Result<()> {
    if is_candidate {
        let persisted = persist_replacement_candidate_submission(
            ctx.node_data_dir
                .expect("candidate join requires NodeHost state"),
            evidence,
            ctx.fresh_node_signature,
            signature,
        )
        .map_err(|error| eyre::eyre!("persist candidate registration: {error}"))?;
        *durable_submission = Some(DurableJoinSubmissionV1::Candidate(persisted));
    } else if ctx.join_transport == JoinTransport::AuthorizedNodeHost
        && ctx.offer_key_state == JoinOfferKeyState::Keyless
    {
        let persisted = persist_committed_join_submission(
            ctx.node_data_dir
                .expect("committed join requires NodeHost state"),
            ctx.signer_address,
            evidence,
            ctx.fresh_node_signature,
            signature,
        )
        .map_err(|error| eyre::eyre!("persist committed registration: {error}"))?;
        *durable_submission = Some(DurableJoinSubmissionV1::Committed(persisted));
    }
    Ok(())
}

fn decode_evidence_node_id_hash(evidence: &[u8]) -> Result<B256> {
    match AttestationEvidenceV1::decode_canonical(evidence)
        .map_err(|error| eyre::eyre!("re-decode canonical V1 evidence: {error}"))?
    {
        AttestationEvidenceV1::Dcap(value) => value.intent.node_id,
        AttestationEvidenceV1::GramineDirectDev(value) => value.intent.node_id,
    }
    .node_id_hash()
    .map_err(|error| eyre::eyre!("hash V1 node identity: {error}"))
}

fn persist_admission_anchor_if_authorized<'a>(
    join_transport: JoinTransport,
    node_data_dir: Option<&'a Path>,
    finalized_join: &outbe_operator::tee::FinalizedRegistryChainViewV1,
    (node_id_hash, enclave_id, intent_hash): (B256, B256, B256),
) -> Result<Option<&'a Path>> {
    if join_transport != JoinTransport::AuthorizedNodeHost {
        return Ok(None);
    }
    let dir = node_data_dir.ok_or_else(|| {
        eyre::eyre!("authenticated onboarding lost its required node data directory")
    })?;
    persist_authorized_join_admission_anchor_v1(
        dir,
        finalized_join,
        node_id_hash,
        enclave_id,
        intent_hash,
    )?;
    Ok(Some(dir))
}
