use super::authorize_validator_node_binding;
use super::call_u256;
use super::compressed_public_key;
use super::development_identity_v1;
use super::load_secp256k1_key_file;
use super::parse_nonzero_b256;
use outbe_operator::tee::read_finalized_staged_successor_policy_v1;

use super::sign_node_hash;
use super::CliFinalityRpc;
use crate::abi;
use crate::abi::ITeeRegistry;
use crate::rpc::Rpc;

use alloy_primitives::keccak256;

use alloy_primitives::B256;
use alloy_primitives::U256;
use alloy_sol_types::SolCall;

use eyre::Result;
use eyre::WrapErr;

use outbe_operator::tee::await_finalized_onboarding_v1;

use outbe_operator::tee::read_finalized_registry_view_v1;

use outbe_operator::tee::ExpectedOnboardingBindingV1;

use outbe_operator::tee::NodeBindingSelectorV1;

use outbe_operator::tx::buffered_gas_price;
use outbe_operator::tx::RelaySignerV1;

use outbe_primitives::tee_attestation_v1::AttestationEvidenceV1;
use outbe_primitives::tee_attestation_v1::AttestationMode;
use outbe_primitives::tee_attestation_v1::AttestationOperationV1;
use outbe_primitives::tee_attestation_v1::DcapEvidenceV1;
use outbe_primitives::tee_attestation_v1::GramineDirectEvidenceV1;
use outbe_primitives::tee_attestation_v1::NodeIdV1;
use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;
use outbe_primitives::tee_attestation_v1::RegistryMutatorV1;

use outbe_primitives::tee_attestation_v1::TeeRegistryGasScheduleV1;

use outbe_tee::acquire_dcap_collateral_v1;
use outbe_tee::clear_committed_join_checkpoint;
use outbe_tee::connect_committed_node_host_enclave;
use outbe_tee::construct_finalized_replacement_authorization_v1;

use outbe_tee::load_committed_enclave_manifest_v1;
use outbe_tee::load_committed_join_relay;
use outbe_tee::load_committed_join_submission;
use outbe_tee::load_replacement_candidate_relay;
use outbe_tee::load_replacement_candidate_submission;
use outbe_tee::persist_committed_join_relay;
use outbe_tee::persist_committed_join_submission;

use outbe_tee::persist_replacement_candidate_relay;
use outbe_tee::persist_replacement_candidate_submission;
use outbe_tee::promote_replacement_candidate;
use outbe_tee::protocol::EnclaveRequest;
use outbe_tee::protocol::EnclaveResponse;

use outbe_tee::EnclaveClient;

use outbe_tee::FinalizedReplacementBindingV1;

use outbe_tee::NodeHostIdentityV1;

use std::fs;

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

pub(super) struct TeeJoinArgs<'a> {
    pub(super) private_key: Option<&'a str>,
    pub(super) enclave_socket: &'a str,
    pub(super) node_data_dir: Option<&'a std::path::Path>,
    pub(super) reth_p2p_secret_key: Option<&'a std::path::Path>,
    pub(super) genesis: &'a std::path::Path,
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
    let policy = if successor_policy {
        let staged = read_finalized_staged_successor_policy_v1(&CliFinalityRpc(client))
            .await?
            .ok_or_else(|| eyre::eyre!("no approved successor TEE policy"))?;
        if staged.finalized_hash != finalized.view.block_hash {
            eyre::bail!("join and successor policy use different finalized heads");
        }
        staged.policy
    } else {
        finalized.policy.clone()
    };
    if policy.chain_id != U256::from(rpc_chain_id).to_be_bytes() {
        return Err(eyre::eyre!(
            "finalized V1 policy chain id does not match eth_chainId"
        ));
    }
    if finalized
        .binding
        .as_ref()
        .is_some_and(|binding| binding.node_id_hash != node_id_hash)
    {
        eyre::bail!("finalized Registry selector returned another node identity");
    }
    if let Some(node_binding) = finalized.binding.as_ref() {
        let signer_view = read_finalized_registry_view_v1(
            &CliFinalityRpc(client),
            &NodeBindingSelectorV1::Validator(evm_signer.address()),
        )
        .await
        .wrap_err("authenticate global EVM signer against finalized TeeRegistry association")?;
        if signer_view.binding.as_ref() != Some(node_binding) {
            eyre::bail!("global --private-key does not own this finalized NodeHost association");
        }
    }
    let mut durable_submission = if let Some(node_data_dir) = node_data_dir {
        let manifest_path = node_data_dir
            .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
            .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1);
        if manifest_path.exists() {
            let candidate = load_replacement_candidate_submission(node_data_dir)
                .map_err(|error| eyre::eyre!("inspect candidate join checkpoint: {error}"))?;
            let committed = load_committed_join_submission(node_data_dir)
                .map_err(|error| eyre::eyre!("inspect committed join checkpoint: {error}"))?;
            match (candidate, committed) {
                (Some(_), Some(_)) => {
                    eyre::bail!("candidate and committed tee join checkpoints coexist");
                }
                (Some(value), None) => Some(DurableJoinSubmissionV1::Candidate(value)),
                (None, Some(value)) => Some(DurableJoinSubmissionV1::Committed(value)),
                (None, None) => None,
            }
        } else {
            None
        }
    } else {
        None
    };
    ensure_durable_join_registration_caller(
        durable_submission
            .as_ref()
            .and_then(DurableJoinSubmissionV1::registration_caller),
        evm_signer.address(),
    )?;
    let durable_resume_intent = durable_submission
        .as_ref()
        .map(|submission| {
            let evidence = AttestationEvidenceV1::decode_canonical(submission.evidence())
                .map_err(|error| eyre::eyre!("decode durable join checkpoint: {error}"))?;
            Ok::<_, eyre::Report>(match evidence {
                AttestationEvidenceV1::Dcap(value) => value.intent,
                AttestationEvidenceV1::GramineDirectDev(value) => value.intent,
            })
        })
        .transpose()?;
    let resumes_finalized_target = match (&finalized.binding, &durable_resume_intent) {
        (Some(binding), Some(intent)) => finalized_binding_matches_intent(binding, intent)?,
        _ => false,
    };
    if !resumes_finalized_target {
        if let Some(binding) = finalized
            .binding
            .as_ref()
            .filter(|binding| finalized.schedule.finalized_timestamp < binding.valid_until)
        {
            let exact_completed_replay = if binding.binding_id == binding_id
                && binding.valid_until == valid_until
            {
                if let Some(node_data_dir) = node_data_dir {
                    let manifest = load_committed_enclave_manifest_v1(node_data_dir)
                        .map_err(|error| eyre::eyre!("load committed replay manifest: {error}"))?;
                    committed_manifest_matches_binding(&manifest, binding)?
                } else {
                    false
                }
            } else {
                false
            };
            if exact_completed_replay {
                let mut committed = connect_committed_node_host_enclave(
                    enclave_socket,
                    node_data_dir.expect("completed replay requires NodeHost state"),
                )
                .map_err(|error| eyre::eyre!("reopen completed tee join: {error}"))?;
                match committed.request(&EnclaveRequest::GetPublicKeys)? {
                    EnclaveResponse::PublicKeys {
                        offer_key_ready: true,
                        recipient_x25519_pub,
                        ..
                    } if recipient_x25519_pub
                        == <B256 as Into<[u8; 32]>>::into(finalized.tribute_offer_public) =>
                    {
                        persist_authorized_join_admission_anchor_v1(
                            node_data_dir.expect("completed replay requires NodeHost state"),
                            &finalized,
                            binding.node_id_hash,
                            binding.enclave_id,
                            binding.intent_hash,
                        )?;
                        println!("[ok] exact tee join is already finalized and locally committed");
                        return Ok(());
                    }
                    other => {
                        eyre::bail!(
                            "finalized tee join is not durably ready in the committed enclave: {other:?}"
                        );
                    }
                }
            }
            ensure_joinable_binding(Some(binding), finalized.schedule.finalized_timestamp)?;
        }
    }
    if !resumes_finalized_target {
        let lease = valid_until
            .checked_sub(finalized.schedule.finalized_timestamp)
            .ok_or_else(|| eyre::eyre!("--valid-until is not after finalized consensus time"))?;
        if lease < policy.minimum_lease || lease > policy.maximum_lease {
            return Err(eyre::eyre!(
                "--valid-until lease {lease}s is outside finalized policy range {}..={}s",
                policy.minimum_lease,
                policy.maximum_lease
            ));
        }
    }
    let (validator_binding, validator_binding_hash, validator_signature) =
        authorize_validator_node_binding(
            policy.chain_id,
            policy.genesis_hash,
            node_id_hash,
            &evm_signer,
        )?;
    let node_binding_signature = sign_node_hash(&node_signing_key, validator_binding_hash)
        .map_err(|error| eyre::eyre!("sign NodeHost side of address binding: {error}"))?;
    let validator_binding = validator_binding
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode address-to-NodeHost binding: {error}"))?;
    // Read the permanent chain key before generating a fresh quote. Its exact
    // value is later authenticated again inside the recipient enclave.
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
    let (
        mut enclave,
        recipient_x25519,
        attestation_ed25519,
        noise_responder_x25519,
        enclave_id,
        node_host_authorization_hash,
    ) = match join_transport {
        JoinTransport::AuthorizedNodeHost => {
            let node_data_dir = node_data_dir.expect("transport selection requires node data dir");
            fs::create_dir_all(node_data_dir).wrap_err_with(|| {
                format!("create NodeHost data directory {}", node_data_dir.display())
            })?;
            let manifest_path = node_data_dir
                .join(outbe_tee::node_host::NODE_HOST_DIRECTORY_V1)
                .join(outbe_tee::node_host::NODE_HOST_MANIFEST_V1);
            let (client, manifest) = if manifest_path.exists() {
                let committed = load_committed_enclave_manifest_v1(node_data_dir)
                    .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
                match connect_committed_node_host_enclave(enclave_socket, node_data_dir) {
                    Ok(client) => (JoinEnclave::Committed(client), committed),
                    Err(committed_error) if finalized.binding.is_some() => {
                        let candidate = outbe_tee::prepare_node_host_enclave_replacement_candidate(
                            enclave_socket,
                            node_data_dir,
                            NodeHostIdentityV1 {
                                network_binding: policy.network_binding(),
                                reth_p2p_public,
                            },
                            |hash| sign_node_hash(&node_signing_key, hash),
                        )
                        .map_err(|candidate_error| {
                            eyre::eyre!(
                                "endpoint matches neither the committed enclave ({committed_error}) nor a resumable fresh candidate ({candidate_error})"
                            )
                        })?;
                        let manifest = candidate.manifest().clone();
                        (JoinEnclave::Candidate(Box::new(candidate)), manifest)
                    }
                    Err(error) => {
                        return Err(eyre::eyre!(
                            "reconnect unregistered committed NodeHost enclave: {error}"
                        ));
                    }
                }
            } else {
                let client = outbe_tee::connect_or_initialize_node_host_enclave(
                    enclave_socket,
                    node_data_dir,
                    NodeHostIdentityV1 {
                        network_binding: policy.network_binding(),
                        reth_p2p_public,
                    },
                    |hash| sign_node_hash(&node_signing_key, hash),
                )
                .map_err(|error| {
                    eyre::eyre!("production NodeHost initialization failed: {error}")
                })?;
                let manifest = load_committed_enclave_manifest_v1(node_data_dir)
                    .map_err(|error| eyre::eyre!("load committed NodeHost manifest: {error}"))?;
                (JoinEnclave::Committed(client), manifest)
            };
            if manifest.network_binding() != policy.network_binding() || manifest.node_id != node_id
            {
                return Err(eyre::eyre!(
                    "committed NodeHost manifest does not match the active chain and node identity"
                ));
            }
            let enclave_id = manifest
                .enclave_id()
                .map_err(|error| eyre::eyre!("invalid committed enclave identity: {error}"))?;
            let authorization = manifest.node_host_authorization_hash().map_err(|error| {
                eyre::eyre!("invalid committed NodeHost authorization: {error}")
            })?;
            (
                client,
                manifest.recipient_x25519,
                manifest.attestation_ed25519,
                manifest.noise_responder_x25519,
                enclave_id,
                authorization,
            )
        }
        JoinTransport::Development => {
            let development = EnclaveClient::connect_endpoint(enclave_socket).map_err(|error| {
                eyre::eyre!("connect development enclave at {enclave_socket}: {error}")
            })?;
            if development.is_hardware_attested() {
                return Err(eyre::eyre!(
                    "GramineDirectDev policy requires the separate non-SGX development transport"
                ));
            }
            let (recipient, attestation, noise) = match development.quote() {
                EnclaveResponse::Quote {
                    recipient_x25519_pub,
                    attestation_pub,
                    noise_static_pub,
                    ..
                } => (*recipient_x25519_pub, *attestation_pub, *noise_static_pub),
                other => return Err(eyre::eyre!("expected development Quote, got {other:?}")),
            };
            let (enclave_id, authorization) =
                development_identity_v1(&policy, &node_id, recipient, attestation, noise)?;
            (
                JoinEnclave::Development(Box::new(development)),
                recipient,
                attestation,
                noise,
                enclave_id,
                authorization,
            )
        }
    };

    // A permanent offer key is write-once enclave state. Classify it before
    // producing or relaying a fresh registration so a mismatched resident key
    // cannot mutate Registry and a matching same-enclave rejoin never repeats
    // the onboarding ingest.
    let offer_key_state = classify_join_offer_key_state(
        enclave.request(&EnclaveRequest::GetPublicKeys)?,
        expected_offer_pub,
    )?;
    let completion_plan = plan_join_completion(offer_key_state, enclave.is_candidate())?;
    if durable_submission
        .as_ref()
        .is_some_and(|submission| submission.is_candidate() != enclave.is_candidate())
    {
        eyre::bail!("durable tee join checkpoint targets another enclave lifecycle");
    }

    let source_binding = if resumes_finalized_target {
        None
    } else {
        finalized.binding.as_ref()
    };
    let (binding_version, registration_version, renewal_nonce, transition_nonce) =
        registration_counters(source_binding)?;

    let fresh_intent = RegistrationIntentV1 {
        chain_id: policy.chain_id,
        genesis_hash: policy.genesis_hash,
        operation: AttestationOperationV1::RegisterEnclave,
        attestation_mode: policy.attestation_mode,
        policy_hash,
        node_id,
        enclave_id,
        binding_id,
        binding_version,
        registration_version,
        renewal_nonce,
        transition_nonce,
        requested_valid_until: valid_until,
        recipient_x25519,
        attestation_ed25519,
        noise_responder_x25519,
        node_host_authorization_hash,
    };
    let intent = durable_resume_intent.unwrap_or(fresh_intent);
    if intent.binding_id != binding_id
        || intent.requested_valid_until != valid_until
        || intent.enclave_id != enclave_id
        || intent.recipient_x25519 != recipient_x25519
        || intent.attestation_ed25519 != attestation_ed25519
        || intent.noise_responder_x25519 != noise_responder_x25519
        || intent.node_host_authorization_hash != node_host_authorization_hash
    {
        eyre::bail!("requested tee join does not match its durable checkpoint");
    }
    let intent_hash = intent
        .intent_hash()
        .map_err(|error| eyre::eyre!("invalid V1 registration intent: {error}"))?;
    let fresh_node_signature = sign_node_hash(&node_signing_key, intent_hash)
        .map_err(|error| eyre::eyre!("sign V1 node intent: {error}"))?;
    let (evidence_value, node_signature, enclave_signature) = if let Some(durable) =
        durable_submission.as_ref()
    {
        let evidence = AttestationEvidenceV1::decode_canonical(durable.evidence())
            .map_err(|error| eyre::eyre!("decode durable join evidence: {error}"))?;
        let durable_intent = match &evidence {
            AttestationEvidenceV1::Dcap(value) => &value.intent,
            AttestationEvidenceV1::GramineDirectDev(value) => &value.intent,
        };
        if durable_intent != &intent || durable.node_signature() != &fresh_node_signature {
            eyre::bail!("requested tee join conflicts with the durable registration");
        }
        (
            evidence,
            *durable.node_signature(),
            *durable.enclave_signature(),
        )
    } else {
        let (evidence, signature) = match policy.attestation_mode {
            AttestationMode::DcapRequired => {
                let generated = enclave.generate_dcap_quote(&intent)?;
                let components = acquire_dcap_collateral_v1(&generated.quote_body)
                    .map_err(|error| eyre::eyre!("acquire canonical DCAP collateral: {error}"))?;
                let signature = generated.enclave_signature;
                (
                    AttestationEvidenceV1::Dcap(DcapEvidenceV1 {
                        intent: intent.clone(),
                        quote: generated.quote_body,
                        components,
                        transition_key_ready_proof: generated.transition_key_ready_proof,
                    }),
                    signature,
                )
            }
            AttestationMode::GramineDirectDev => {
                let signature = enclave.sign_registration_intent_dev_v1(&intent)?;
                (
                    AttestationEvidenceV1::GramineDirectDev(GramineDirectEvidenceV1 {
                        transition_key_ready_proof: None,
                        intent: intent.clone(),
                        dev_attestation_public: attestation_ed25519,
                        dev_signature: signature,
                    }),
                    signature,
                )
            }
        };
        if enclave.is_candidate() {
            let persisted = persist_replacement_candidate_submission(
                node_data_dir.expect("candidate join requires NodeHost state"),
                &evidence,
                &fresh_node_signature,
                &signature,
            )
            .map_err(|error| eyre::eyre!("persist candidate registration: {error}"))?;
            durable_submission = Some(DurableJoinSubmissionV1::Candidate(persisted));
        } else if join_transport == JoinTransport::AuthorizedNodeHost
            && offer_key_state == JoinOfferKeyState::Keyless
        {
            let persisted = persist_committed_join_submission(
                node_data_dir.expect("committed join requires NodeHost state"),
                evm_signer.address(),
                &evidence,
                &fresh_node_signature,
                &signature,
            )
            .map_err(|error| eyre::eyre!("persist committed registration: {error}"))?;
            durable_submission = Some(DurableJoinSubmissionV1::Committed(persisted));
        }
        (evidence, fresh_node_signature, signature)
    };
    let evidence = evidence_value
        .encode_canonical()
        .map_err(|error| eyre::eyre!("encode canonical V1 evidence: {error}"))?;
    let node_id_hash = match AttestationEvidenceV1::decode_canonical(&evidence)
        .map_err(|error| eyre::eyre!("re-decode canonical V1 evidence: {error}"))?
    {
        AttestationEvidenceV1::Dcap(value) => value.intent.node_id,
        AttestationEvidenceV1::GramineDirectDev(value) => value.intent.node_id,
    }
    .node_id_hash()
    .map_err(|error| eyre::eyre!("hash V1 node identity: {error}"))?;
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

    // The global EVM signer owns both the address-to-NodeHost association and
    // the transaction envelope. No second node or renewal EVM key exists.
    let calldata_hash = keccak256(&call);
    let tx_hash = if enclave.is_candidate() {
        let node_data_dir = node_data_dir.expect("candidate join requires NodeHost state");
        let relay = if let Some(durable) = load_replacement_candidate_relay(node_data_dir)
            .map_err(|error| eyre::eyre!("load durable candidate relay: {error}"))?
        {
            if durable.calldata_hash() != calldata_hash {
                eyre::bail!("durable candidate transaction targets different calldata");
            }
            durable
        } else {
            if resumes_finalized_target {
                eyre::bail!(
                    "finalized candidate binding has no durable pre-relay transaction checkpoint"
                );
            }
            let relay_signer = RelaySignerV1::new(private_key_hex)?;
            if relay_signer.address() != evm_signer.address() {
                eyre::bail!("global EVM signer address is inconsistent");
            }
            let account_nonce = client
                .eth_get_transaction_count(evm_signer.address())
                .await?;
            let gas_price = buffered_gas_price(client.eth_gas_price().await?);
            let required_balance = gas_price.saturating_mul(U256::from(gas_limit));
            let balance = client.eth_get_balance(evm_signer.address()).await?;
            if balance < required_balance {
                eyre::bail!(
                    "TEE join EVM signer {} has {balance} but needs at least {required_balance}",
                    evm_signer.address()
                );
            }
            let raw = relay_signer.sign_renewal(
                rpc_chain_id,
                account_nonce,
                gas_price,
                gas_limit,
                abi::TEE_REGISTRY_ADDR,
                &call,
            )?;
            persist_replacement_candidate_relay(node_data_dir, calldata_hash, &raw.raw_transaction)
                .map_err(|error| eyre::eyre!("persist exact candidate transaction: {error}"))?
        };
        let exact = ExactJoinRelayV1 {
            transaction_hash: relay.transaction_hash(),
            raw_transaction: relay.raw_transaction().to_vec(),
        };
        relay_exact_join_transaction(client, &exact, resumes_finalized_target).await?
    } else if durable_submission
        .as_ref()
        .is_some_and(DurableJoinSubmissionV1::is_committed)
    {
        let node_data_dir = node_data_dir.expect("committed join requires NodeHost state");
        let relay = if let Some(durable) = load_committed_join_relay(node_data_dir)
            .map_err(|error| eyre::eyre!("load durable committed relay: {error}"))?
        {
            if durable.calldata_hash() != calldata_hash {
                eyre::bail!("durable committed transaction targets different calldata");
            }
            durable
        } else {
            match plan_missing_committed_relay(resumes_finalized_target, offer_key_state)? {
                MissingCommittedRelayPlan::CleanupReadyExact => {
                    drop(enclave);
                    let mut reopened =
                        connect_committed_node_host_enclave(enclave_socket, node_data_dir)
                            .map_err(|error| {
                                eyre::eyre!("reopen completed committed enclave: {error}")
                            })?;
                    match reopened.request(&EnclaveRequest::GetPublicKeys)? {
                        EnclaveResponse::PublicKeys {
                            offer_key_ready: true,
                            recipient_x25519_pub,
                            ..
                        } if recipient_x25519_pub == expected_offer_pub => {}
                        other => {
                            eyre::bail!(
                                "cleanup recovery did not reopen the exact permanent offer key: {other:?}"
                            );
                        }
                    }
                    persist_authorized_join_admission_anchor_v1(
                        node_data_dir,
                        &finalized,
                        node_id_hash,
                        enclave_id,
                        intent_hash,
                    )?;
                    clear_committed_join_checkpoint(node_data_dir, intent_hash).map_err(
                        |error| eyre::eyre!("clear recovered committed join checkpoint: {error}"),
                    )?;
                    println!(
                        "[ok] finalized tee join cleanup recovered without another transaction or onboarding ingest"
                    );
                    return Ok(());
                }
                MissingCommittedRelayPlan::ConstructAndPersist => {}
            }
            let relay_signer = RelaySignerV1::new(private_key_hex)?;
            if relay_signer.address() != evm_signer.address() {
                eyre::bail!("global EVM signer address is inconsistent");
            }
            let account_nonce = client
                .eth_get_transaction_count(evm_signer.address())
                .await?;
            let gas_price = buffered_gas_price(client.eth_gas_price().await?);
            let required_balance = gas_price.saturating_mul(U256::from(gas_limit));
            let balance = client.eth_get_balance(evm_signer.address()).await?;
            if balance < required_balance {
                eyre::bail!(
                    "TEE join EVM signer {} has {balance} but needs at least {required_balance}",
                    evm_signer.address()
                );
            }
            let from_block = client.eth_block_number().await?;
            let raw = relay_signer.sign_renewal(
                rpc_chain_id,
                account_nonce,
                gas_price,
                gas_limit,
                abi::TEE_REGISTRY_ADDR,
                &call,
            )?;
            persist_committed_join_relay(
                node_data_dir,
                calldata_hash,
                from_block,
                &raw.raw_transaction,
            )
            .map_err(|error| eyre::eyre!("persist exact committed transaction: {error}"))?
        };
        let exact = ExactJoinRelayV1 {
            transaction_hash: relay.transaction_hash(),
            raw_transaction: relay.raw_transaction().to_vec(),
        };
        relay_exact_join_transaction(client, &exact, resumes_finalized_target).await?
    } else {
        evm_signer
            .send_tx_with_gas(client, abi::TEE_REGISTRY_ADDR, call, U256::ZERO, gas_limit)
            .await
            .wrap_err("V1 registerEnclave submission failed")?
    };
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

    // The NodeHost admission checkpoint must be durable before the recipient
    // enclave can activate the permanent key. A crash after this write can
    // safely resume; a write failure leaves the enclave keyless.
    let authorized_node_data_dir = if join_transport == JoinTransport::AuthorizedNodeHost {
        let node_data_dir = node_data_dir.ok_or_else(|| {
            eyre::eyre!("authenticated onboarding lost its required node data directory")
        })?;
        persist_authorized_join_admission_anchor_v1(
            node_data_dir,
            &finalized_join,
            node_id_hash,
            enclave_id,
            intent_hash,
        )?;
        Some(node_data_dir)
    } else {
        None
    };

    let tribute_offer_public = if completion_plan.ingest_offer_key {
        let expected = ExpectedOnboardingBindingV1 {
            selector: binding_selector.clone(),
            chain_id: policy.chain_id,
            genesis_hash: policy.genesis_hash,
            node_id_hash,
            enclave_id,
            intent_hash,
            recipient_x25519,
            tribute_offer_public: expected_offer_pub,
            key_epoch,
            tribute_offer_epoch,
        };
        let finalized = await_finalized_onboarding_v1(
            &CliFinalityRpc(client),
            &tx_hash,
            &expected,
            Duration::from_secs(timeout_secs),
        )
        .await?;
        let artifact = finalized
            .artifact
            .encode_canonical()
            .map_err(|code| eyre::eyre!("encode finalized artifact: {:#06x}", code.code()))?;
        println!(
            "exact onboarding finalized at height {} (artifact {} bytes)",
            finalized.finalized_height,
            artifact.len()
        );
        match policy.attestation_mode {
            AttestationMode::DcapRequired => {
                let anchor_outcome = load_finalized_admission_anchor_v1(
                    client,
                    genesis,
                    finalized.finalized_height,
                    &finalized.artifact.context,
                )
                .await?;
                let node_data_dir = authorized_node_data_dir
                    .expect("DcapRequired join has a durable NodeHost admission anchor");
                let mut recovery = CliFinalizedAdmissionIoV1 {
                    rpc: client,
                    finalized_height: finalized.finalized_height,
                    context: &finalized.artifact.context,
                    artifact: &artifact,
                    anchor_outcome: &anchor_outcome,
                    expected_intent_hash: intent_hash,
                    expected_offer_public: expected_offer_pub,
                    expected_key_epoch: key_epoch,
                    expected_offer_epoch: tribute_offer_epoch,
                    enclave: &mut enclave,
                    enclave_socket,
                    node_data_dir,
                    node_host_identity: NodeHostIdentityV1 {
                        network_binding: policy.network_binding(),
                        reth_p2p_public,
                    },
                    node_signing_key: &node_signing_key,
                };
                run_finalized_admission_recovery_v1(&mut recovery)
                    .await
                    .wrap_err("enclave purpose-bound onboarding ingest failed")?
            }
            AttestationMode::GramineDirectDev => {
                if authorized_node_data_dir.is_none() {
                    eyre::bail!(
                        "GramineDirectDev post-bootstrap onboarding requires authenticated NodeHost state"
                    );
                }
                match enclave.request(
                    &EnclaveRequest::IngestGramineDirectDevOnboardingArtifactV1 {
                        artifact,
                        expected_intent_hash: intent_hash,
                        expected_tribute_offer_public: expected_offer_pub,
                        expected_key_epoch: key_epoch,
                        expected_tribute_offer_epoch: tribute_offer_epoch,
                    },
                )? {
                    EnclaveResponse::GramineDirectDevOnboardingArtifactIngestedV1 {
                        tribute_offer_public,
                    } if tribute_offer_public == expected_offer_pub => tribute_offer_public,
                    other => {
                        eyre::bail!(
                            "GramineDirectDev onboarding returned an unexpected response: {other:?}"
                        );
                    }
                }
            }
        }
    } else {
        expected_offer_pub
    };
    {
        if join_transport == JoinTransport::AuthorizedNodeHost {
            let node_data_dir = authorized_node_data_dir
                .expect("authorized NodeHost data directory was validated before key activation");
            let promotion = if completion_plan.promote_candidate {
                let exact =
                    read_finalized_registry_view_v1(&CliFinalityRpc(client), &binding_selector)
                        .await
                        .wrap_err("read finalized candidate promotion binding")?;
                let binding = exact
                    .binding
                    .ok_or_else(|| eyre::eyre!("finalized Registry lost the candidate binding"))?;
                if !finalized_binding_matches_intent(&binding, &intent)? {
                    eyre::bail!(
                        "finalized Registry binding differs from the durable candidate intent"
                    );
                }
                Some(FinalizedReplacementBindingV1 {
                    view: exact.view,
                    node_id_hash: binding.node_id_hash,
                    enclave_id: binding.enclave_id,
                    binding_id: binding.binding_id,
                    intent_hash: binding.intent_hash,
                    binding_version: binding.binding_version,
                    registration_version: binding.registration_version,
                    valid_until: binding.valid_until,
                    recipient_x25519: binding.recipient_x25519.into(),
                    attestation_ed25519: binding.attestation_ed25519.into(),
                    noise_responder_x25519: binding.noise_responder_x25519.into(),
                    node_host_authorization_hash: binding.node_host_authorization_hash,
                })
            } else {
                None
            };
            drop(enclave);
            if let Some(finalized_binding) = promotion {
                let authorization = construct_finalized_replacement_authorization_v1(
                    node_data_dir,
                    &finalized_binding,
                )
                .map_err(|error| eyre::eyre!("authorize finalized candidate promotion: {error}"))?;
                promote_replacement_candidate(node_data_dir, &authorization)
                    .map_err(|error| eyre::eyre!("promote finalized rejoin candidate: {error}"))?;
            }
            let mut reopened = connect_committed_node_host_enclave(enclave_socket, node_data_dir)
                .map_err(|error| eyre::eyre!("reopen committed enclave: {error}"))?;
            match reopened.request(&EnclaveRequest::GetPublicKeys)? {
                EnclaveResponse::PublicKeys {
                    offer_key_ready: true,
                    recipient_x25519_pub,
                    ..
                } if recipient_x25519_pub == expected_offer_pub => {}
                other => {
                    return Err(eyre::eyre!(
                            "durable onboarding reopen did not expose the exact permanent offer key: {other:?}"
                        ));
                }
            }
            if durable_submission
                .as_ref()
                .is_some_and(DurableJoinSubmissionV1::is_committed)
            {
                clear_committed_join_checkpoint(node_data_dir, intent_hash).map_err(|error| {
                    eyre::eyre!("clear completed committed join checkpoint: {error}")
                })?;
            }
        }
        if join_transport == JoinTransport::AuthorizedNodeHost {
            println!(
                    "[OK] offer key durably installed and the authenticated enclave connection reopened (offer_public 0x{}). \
                     You can now start outbe-chain node.",
                    hex::encode(tribute_offer_public)
                );
        } else {
            println!(
                "[OK] development offer key installed in enclave (offer_public 0x{}). \
                     You can now start the separate GramineDirectDev node.",
                hex::encode(tribute_offer_public)
            );
        }
        Ok(())
    }
}
