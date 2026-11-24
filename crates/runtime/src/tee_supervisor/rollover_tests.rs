//! One tick retries enrollment when the eth proof window moves from H1 to H2.
use super::boundary_artifact::boundary_extra;
use super::driver::LifecycleTask;
use super::engine;
use crate::admission_authority::test_support as support;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use jsonrpsee::{server::ServerBuilder, types::ErrorObjectOwned, RpcModule};
use outbe_consensus::finalized_admission_test_utils::FinalityCommitteeFixture;
use outbe_primitives::tee_attestation_v1::*;
use outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1;
use outbe_primitives::tee_registry_abi_v1::{ITeeRegistryV1, NodeEnclaveBindingV1View};
use outbe_primitives::tee_test_utils::{no_attestation_dev_policy_v1, RegistrationIntentFixture};
use outbe_primitives::{addresses::TEE_REGISTRY_ADDRESS, addresses::VALIDATOR_SET_ADDRESS};
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;
use outbe_tee::in_process::{InProcessTeeContext, ProofSourceV1};
use outbe_tee::lifecycle::enrollment::EnrollmentOutcome;
use outbe_tee::lifecycle::LifecycleCheckpointV1;
use outbe_tee::node_host::{
    NodeHostIdentityKey, NODE_HOST_DIRECTORY_V1, NODE_HOST_IDENTITY_KEY_V1,
};
use outbe_tee::registry_view::RenewalBindingV1;
use outbe_tee::resident::{ResidentOfferMaterial, ResidentTeeKeys};
use outbe_validatorset::precompile::IValidatorSet;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn resident(
    seed: u8,
    host: Option<&NodeHostIdentityKey>,
    committee: &FinalityCommitteeFixture,
    activate: bool,
) -> (Arc<InProcessTeeContext>, TrustedNetworkDescriptorV1) {
    let template = support::context(seed);
    let mut manifest = template.manifest().clone();
    if let Some(host) = host {
        manifest.node_host_noise_x25519 = host.public();
    }
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: manifest.network_binding(),
        genesis_consensus_keys: committee.public_keys_min_pk(),
    };
    let context = Arc::new(
        InProcessTeeContext::new(
            ResidentTeeKeys::from_seed([seed; 32]).unwrap(),
            *template.identity(),
            manifest,
            descriptor.clone(),
            ProofSourceV1::NoHardware,
        )
        .unwrap(),
    );
    if activate {
        let chain_id = B256::from(context.manifest().chain_id);
        context
            .activate_for_test(
                ResidentOfferMaterial::for_test(vec![7; 96], chain_id, 0, 0).unwrap(),
            )
            .unwrap();
    }
    (context, descriptor)
}

fn binding_for(intent: &RegistrationIntentV1) -> RenewalBindingV1 {
    RenewalBindingV1 {
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        intent_hash: intent.intent_hash().unwrap(),
        evidence_hash: B256::ZERO,
        policy_hash: intent.policy_hash,
        binding_version: intent.binding_version,
        registration_version: intent.registration_version,
        renewal_nonce: intent.renewal_nonce,
        transition_nonce: intent.transition_nonce,
        lease_started_at: 1,
        valid_until: intent.requested_valid_until,
        collateral_valid_until: u64::MAX,
        recipient_x25519: B256::from(intent.recipient_x25519),
        attestation_ed25519: B256::from(intent.attestation_ed25519),
        noise_responder_x25519: B256::from(intent.noise_responder_x25519),
        mrenclave: B256::ZERO,
        mrsigner: B256::ZERO,
        isv_prod_id: 0,
        isv_svn: 0,
        platform_tcb_status: 0,
        verdict_hash: B256::ZERO,
        node_host_authorization_hash: intent.node_host_authorization_hash,
    }
}

fn hex_prefixed(bytes: &[u8]) -> String {
    format!("0x{}", alloy_primitives::hex::encode(bytes))
}

struct ServedChain {
    phase: AtomicUsize,
    genesis: B256,
    h1: LifecycleCheckpointV1,
    h2: LifecycleCheckpointV1,
    boundary: Vec<u8>,
    finalization: Vec<u8>,
    block: Vec<u8>,
    opening: Value,
    log: Value,
    policy: TeePolicyV1,
    binding: NodeEnclaveBindingV1View,
    public_key: [u8; 32],
    successor_upgrade: AtomicBool,
    pending: Mutex<ServedPending>,
    sent: Mutex<Vec<Vec<u8>>>,
}

struct ServedPending {
    context_hash: B256,
    valid_until: u64,
    source_binding_id: B256,
    target_hash: B256,
    nonce: u64,
}

fn block_json(at: LifecycleCheckpointV1, parent: B256, extra: &[u8]) -> Value {
    json!({
        "number": format!("0x{:x}", at.height),
        "hash": at.hash,
        "parentHash": parent,
        "stateRoot": at.state_root,
        "timestamp": format!("0x{:x}", at.timestamp),
        "baseFeePerGas": "0x1",
        "extraData": hex_prefixed(extra),
    })
}

fn schedule_for(at: LifecycleCheckpointV1) -> TeeRenewalScheduleV1 {
    TeeRenewalScheduleV1 {
        finalized_height: at.height,
        finalized_hash: at.hash,
        finalized_timestamp: at.timestamp,
        epoch_number: 0,
        epoch_start_height: 0,
        epoch_length_blocks: 100,
        next_freeze_height: 90,
        planned_activation_height: 100,
        dkg_prepare_window_blocks: 10,
        minimum_block_time_millis: 1_000,
    }
}

fn dispatch(chain: &ServedChain, to: Address, data: &[u8]) -> Result<Vec<u8>, String> {
    let policy = Bytes::from(chain.policy.encode_canonical().unwrap());
    let encoded = if data.starts_with(&ITeeRegistryV1::activePolicyV1Call::SELECTOR) {
        Some(ITeeRegistryV1::activePolicyV1Call::abi_encode_returns(
            &policy,
        ))
    } else if data.starts_with(&ITeeRegistryV1::stagedSuccessorPolicyV1Call::SELECTOR) {
        Some((false, U256::ZERO, Bytes::new()).abi_encode_params())
    } else if data.starts_with(&ITeeRegistryV1::validatorEnclaveBindingCall::SELECTOR)
        || data.starts_with(&ITeeRegistryV1::nodeHostEnclaveBindingCall::SELECTOR)
    {
        Some(ITeeRegistryV1::validatorEnclaveBindingCall::abi_encode_returns(&chain.binding))
    } else if data.starts_with(&ITeeRegistryV1::offerPublicKeyCall::SELECTOR) {
        Some(ITeeRegistryV1::offerPublicKeyCall::abi_encode_returns(
            &U256::from_be_bytes(chain.public_key),
        ))
    } else if data.starts_with(&ITeeRegistryV1::enclaveUpgradeV1Call::SELECTOR) {
        let upgrade = if chain.successor_upgrade.load(Ordering::SeqCst) {
            ITeeRegistryV1::enclaveUpgradeV1Return {
                proposalId: U256::from(1),
                activationHeight: chain.policy.activation_height,
                mrenclave: B256::ZERO,
                successorPolicyHash: chain.policy.policy_hash().unwrap(),
                predecessorPolicyHash: chain.policy.predecessor_policy_hash,
            }
        } else {
            ITeeRegistryV1::enclaveUpgradeV1Return {
                proposalId: U256::ZERO,
                activationHeight: 0,
                mrenclave: B256::ZERO,
                successorPolicyHash: B256::ZERO,
                predecessorPolicyHash: B256::ZERO,
            }
        };
        Some(ITeeRegistryV1::enclaveUpgradeV1Call::abi_encode_returns(
            &upgrade,
        ))
    } else if data.starts_with(&ITeeRegistryV1::pendingEnclaveUpgradeCall::SELECTOR) {
        let pending = chain.pending.lock().unwrap();
        Some(
            ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_encode_returns(
                &ITeeRegistryV1::pendingEnclaveUpgradeReturn {
                    contextHash: pending.context_hash,
                    validUntil: pending.valid_until,
                    sourceBindingId: pending.source_binding_id,
                    targetHash: pending.target_hash,
                    nonce: pending.nonce,
                },
            ),
        )
    } else if data.starts_with(&ITeeRegistryV1::keyEpochCall::SELECTOR)
        || data.starts_with(&ITeeRegistryV1::offerEpochCall::SELECTOR)
    {
        Some(U256::ZERO.abi_encode())
    } else if to == VALIDATOR_SET_ADDRESS
        && data.starts_with(&IValidatorSet::isValidatorCall::SELECTOR)
    {
        Some(IValidatorSet::isValidatorCall::abi_encode_returns(&false))
    } else {
        None
    };
    encoded.ok_or_else(|| {
        format!(
            "unexpected call to {to} selector {}",
            alloy_primitives::hex::encode(&data[..data.len().min(4)])
        )
    })
}

fn opening_for(context: &DcapOnboardingContextV1) -> (B256, Value) {
    use outbe_primitives::test_utils::state_proof::{account_mpt_with_proofs, storage_trie};
    use outbe_tee::finalized_admission::onboarding_registry_slots_v1;
    let slots = onboarding_registry_slots_v1(context);
    let values = [
        U256::from_be_bytes(context.network_encryption_public),
        U256::from(context.key_epoch),
        U256::from(context.network_encryption_epoch),
        U256::from_be_bytes(context.enclave_id.0),
        U256::from_be_bytes(context.binding_id.0),
        U256::from_be_bytes(context.intent_hash.0),
        U256::from_be_bytes(context.policy_hash.0),
        U256::from(50_000_u64),
        U256::from_be_bytes(context.recipient_x25519),
    ];
    let leaves = slots
        .iter()
        .zip(values)
        .map(|(slot, value)| (U256::from_be_bytes(slot.0), value))
        .collect::<Vec<_>>();
    let (storage_root, openings) = storage_trie(&leaves);
    let code_hash = B256::repeat_byte(0x91);
    let account = reth_trie::TrieAccount {
        nonce: 1,
        balance: U256::ZERO,
        storage_root,
        code_hash,
    };
    let (state_root, accounts) = account_mpt_with_proofs(&[(TEE_REGISTRY_ADDRESS, account)]);
    let storage_proof = slots
        .into_iter()
        .zip(values)
        .zip(openings)
        .map(|((key, value), nodes)| {
            json!({
                "key": key,
                "value": format!("0x{value:x}"),
                "proof": nodes.iter().map(|node| hex_prefixed(node)).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let opening = json!({
        "nonce": "0x1",
        "balance": "0x0",
        "codeHash": code_hash,
        "storageHash": storage_root,
        "accountProof": accounts[&TEE_REGISTRY_ADDRESS]
            .iter()
            .map(|node| hex_prefixed(node))
            .collect::<Vec<_>>(),
        "storageProof": storage_proof,
    });
    (state_root, opening)
}

fn served_chain(
    committee: &FinalityCommitteeFixture,
    context: &DcapOnboardingContextV1,
    policy: TeePolicyV1,
    binding: RenewalBindingV1,
    artifact: Vec<u8>,
) -> ServedChain {
    use alloy_consensus::BlockHeader as _;
    outbe_consensus::proof::init_consensus_chain_id(676).unwrap();
    let (state_root, opening) = opening_for(context);
    let h1_hash = B256::repeat_byte(0x91);
    let boundary = boundary_extra(committee);
    let certified = committee.certify_child_block(
        outbe_consensus::finalized_admission_test_utils::CertifiedChildBlockInput {
            epoch: commonware_consensus::types::Epoch::new(0),
            height: 2,
            timestamp: 1_000,
            state_root,
            extra_data: boundary.clone(),
            parent_hash: h1_hash,
            base_fee_per_gas: None,
        },
    );
    let block = outbe_consensus::committee_chain::decode_public_block(&certified.block).unwrap();
    let header = block.header();
    let h2 = LifecycleCheckpointV1 {
        height: header.number(),
        hash: certified.block_hash,
        state_root: header.state_root(),
        timestamp: header.timestamp(),
        base_fee_per_gas: U256::from(1),
        chain_id: 676,
        genesis_hash: context.genesis_hash,
    };
    let h1 = LifecycleCheckpointV1 {
        height: 1,
        hash: h1_hash,
        state_root: B256::repeat_byte(0x92),
        timestamp: h2.timestamp.saturating_sub(1),
        ..h2
    };
    let tx = B256::repeat_byte(0x59);
    let event = ITeeRegistryV1::OfferKeySealedForRegistryV1 {
        nodeIdHash: context.node_id_hash,
        sealedOfferKey: Bytes::from(artifact),
    };
    let encoded = event.encode_log_data();
    let log = json!({
        "address": TEE_REGISTRY_ADDRESS,
        "topics": encoded.topics(),
        "data": encoded.data,
        "blockNumber": format!("0x{:x}", h1.height),
        "blockHash": h1.hash,
        "transactionHash": tx,
        "removed": false,
    });
    ServedChain {
        phase: AtomicUsize::new(0),
        genesis: context.genesis_hash,
        h1,
        h2,
        boundary,
        finalization: certified.finalization,
        block: certified.block,
        opening,
        log,
        policy,
        binding: (&binding).into(),
        public_key: context.network_encryption_public,
        successor_upgrade: AtomicBool::new(false),
        pending: Mutex::new(ServedPending {
            context_hash: B256::ZERO,
            valid_until: 0,
            source_binding_id: B256::ZERO,
            target_hash: B256::ZERO,
            nonce: 0,
        }),
        sent: Mutex::new(Vec::new()),
    }
}

fn hex_bytes(value: &str) -> Vec<u8> {
    alloy_primitives::hex::decode(value.trim_start_matches("0x")).unwrap_or_default()
}

fn prepare_intent_from_tx(raw: &[u8]) -> RegistrationIntentV1 {
    use alloy_consensus::transaction::RlpEcdsaDecodableTx as _;
    use alloy_consensus::TxEip1559;
    let mut body = &raw[1..];
    let tx = TxEip1559::rlp_decode_signed(&mut body).unwrap();
    let call = ITeeRegistryV1::prepareEnclaveUpgradeCall::abi_decode(&tx.tx().input).unwrap();
    AttestationEvidenceV1::decode_canonical(&call.evidence)
        .unwrap()
        .intent()
        .clone()
}

type Calls = Arc<Mutex<Vec<(String, Vec<Value>)>>>;

fn header_for(chain: &ServedChain, tag: &str) -> Result<Value, ErrorObjectOwned> {
    if tag == "0x0" {
        return Ok(json!({
            "number": "0x0",
            "hash": chain.genesis,
            "parentHash": B256::ZERO,
            "stateRoot": B256::ZERO,
            "timestamp": "0x0",
            "baseFeePerGas": "0x1",
            "extraData": "0x",
        }));
    }
    if tag == "finalized" {
        let head = if chain.phase.load(Ordering::SeqCst) == 0 {
            chain.h1
        } else {
            chain.h2
        };
        return Ok(block_json(head, chain.genesis, &[]));
    }
    if tag == "0x1" {
        return Ok(block_json(chain.h1, chain.genesis, &[]));
    }
    if tag == "0x2" {
        return Ok(block_json(chain.h2, chain.h1.hash, &chain.boundary));
    }
    Err(ErrorObjectOwned::owned(
        -32000,
        format!("missing block {tag}"),
        None::<()>,
    ))
}

async fn serve(chain: Arc<ServedChain>) -> (String, jsonrpsee::server::ServerHandle, Calls) {
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut module = RpcModule::new(());
    let seen = calls.clone();
    module
        .register_method("eth_chainId", move |_, _, _| {
            seen.lock()
                .unwrap()
                .push(("eth_chainId".into(), Vec::new()));
            Ok::<_, ErrorObjectOwned>("0x2a4")
        })
        .unwrap();
    let state = chain.clone();
    let seen = calls.clone();
    module
        .register_method("eth_getBlockByNumber", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock()
                .unwrap()
                .push(("eth_getBlockByNumber".into(), args.clone()));
            let tag = args[0].as_str().unwrap_or_default();
            header_for(&state, tag)
        })
        .unwrap();
    let state = chain.clone();
    let seen = calls.clone();
    module
        .register_method("eth_call", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock().unwrap().push(("eth_call".into(), args.clone()));
            let to: Address = args[0]["to"]
                .as_str()
                .unwrap_or_default()
                .parse()
                .map_err(|error| ErrorObjectOwned::owned(-32000, format!("{error}"), None::<()>))?;
            let data = alloy_primitives::hex::decode(
                args[0]["data"]
                    .as_str()
                    .unwrap_or("0x")
                    .trim_start_matches("0x"),
            )
            .unwrap_or_default();
            match dispatch(&state, to, &data) {
                Ok(encoded) => Ok::<_, ErrorObjectOwned>(format!(
                    "0x{}",
                    alloy_primitives::hex::encode(encoded)
                )),
                Err(error) => Err(ErrorObjectOwned::owned(-32000, error, None::<()>)),
            }
        })
        .unwrap();
    let state = chain.clone();
    let seen = calls.clone();
    module
        .register_method("outbe_teeRenewalScheduleV1", move |_, _, _| {
            seen.lock()
                .unwrap()
                .push(("outbe_teeRenewalScheduleV1".into(), Vec::new()));
            let head = if state.phase.load(Ordering::SeqCst) == 0 {
                state.h1
            } else {
                state.h2
            };
            Ok::<_, ErrorObjectOwned>(serde_json::to_value(schedule_for(head)).unwrap())
        })
        .unwrap();
    let seen = calls.clone();
    module
        .register_method("eth_getBalance", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock().unwrap().push(("eth_getBalance".into(), args));
            Ok::<_, ErrorObjectOwned>("0x0")
        })
        .unwrap();
    let seen = calls.clone();
    module
        .register_method("eth_getTransactionCount", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock()
                .unwrap()
                .push(("eth_getTransactionCount".into(), args));
            Ok::<_, ErrorObjectOwned>("0x0")
        })
        .unwrap();
    let state = chain.clone();
    let seen = calls.clone();
    module
        .register_method("eth_sendRawTransaction", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock()
                .unwrap()
                .push(("eth_sendRawTransaction".into(), args.clone()));
            let raw = hex_bytes(args.first().and_then(Value::as_str).unwrap_or("0x"));
            state.sent.lock().unwrap().push(raw.clone());
            Ok::<_, ErrorObjectOwned>(json!(B256::from(alloy_primitives::keccak256(&raw))))
        })
        .unwrap();
    let state = chain.clone();
    let seen = calls.clone();
    module
        .register_method("eth_getLogs", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock().unwrap().push(("eth_getLogs".into(), args));
            Ok::<_, ErrorObjectOwned>(json!([state.log.clone()]))
        })
        .unwrap();
    let state = chain.clone();
    let seen = calls.clone();
    module
        .register_method("eth_getTransactionReceipt", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock()
                .unwrap()
                .push(("eth_getTransactionReceipt".into(), args.clone()));
            let tx = args.first().cloned().unwrap_or(Value::Null);
            Ok::<_, ErrorObjectOwned>(json!({
                "transactionHash": tx,
                "blockHash": state.h1.hash,
                "blockNumber": format!("0x{:x}", state.h1.height),
                "status": "0x1",
                "logs": [state.log.clone()],
            }))
        })
        .unwrap();
    let state = chain.clone();
    let seen = calls.clone();
    module
        .register_method("outbe_getFinalityProof", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock()
                .unwrap()
                .push(("outbe_getFinalityProof".into(), args.clone()));
            let height = args[0].as_u64().unwrap_or(0);
            if height != state.h2.height {
                return Err(ErrorObjectOwned::owned(
                    -32000,
                    format!("missing proof {height}"),
                    None::<()>,
                ));
            }
            Ok::<_, ErrorObjectOwned>(json!({
                "finalizationHex": hex_prefixed(&state.finalization),
                "blockHex": hex_prefixed(&state.block),
            }))
        })
        .unwrap();
    let state = chain;
    let seen = calls.clone();
    module
        .register_method("eth_getProof", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock().unwrap().push(("eth_getProof".into(), args));
            let seen_proof = state.phase.fetch_add(1, Ordering::SeqCst);
            if seen_proof == 0 {
                return Err(ErrorObjectOwned::owned(
                    -32000,
                    "distance to target block exceeds maximum proof window",
                    None::<()>,
                ));
            }
            Ok::<_, ErrorObjectOwned>(state.opening.clone())
        })
        .unwrap();
    let handle = server.start(module);
    (format!("http://{address}"), handle, calls)
}

#[tokio::test]
async fn proof_window_rollover_retries_enrollment_and_imports_the_key() {
    use std::os::unix::fs::DirBuilderExt as _;
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join(NODE_HOST_DIRECTORY_V1);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&host)
        .unwrap();
    let host_key = NodeHostIdentityKey::create_new(&host.join(NODE_HOST_IDENTITY_KEY_V1)).unwrap();
    let committee = FinalityCommitteeFixture::new(11);
    let (live, descriptor) = resident(9, Some(&host_key), &committee, false);
    let (donor, _) = resident(8, None, &committee, true);
    assert!(!live.offer_key_ready().unwrap());
    let policy = no_attestation_dev_policy_v1(676, live.manifest().genesis_hash).unwrap();
    let intent = RegistrationIntentFixture {
        operation: AttestationOperationV1::RegisterEnclave,
        policy_hash: policy.policy_hash().unwrap(),
        binding_id: B256::repeat_byte(0x42),
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 50_000,
    }
    .for_manifest(live.manifest())
    .unwrap();
    let context = DcapOnboardingContextV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        intent_hash: intent.intent_hash().unwrap(),
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        policy_hash: intent.policy_hash,
        recipient_x25519: intent.recipient_x25519,
        network_encryption_public: donor.network_encryption_public().unwrap(),
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    let artifact = donor
        .prepare_no_attestation_dev_onboarding_artifact_v2(context)
        .unwrap()
        .onboarding_artifact;
    let chain = Arc::new(served_chain(
        &committee,
        &context,
        policy,
        binding_for(&intent),
        artifact,
    ));
    assert!(chain.h2.height - chain.h1.height <= 128);
    let (url, handle, calls) = serve(chain.clone()).await;
    let mut supervisor = super::TeeLifecycleSupervisor::new(
        &url,
        (
            Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([0x47; 32]).unwrap()),
            k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap(),
        ),
        (live, root.path().to_owned(), descriptor),
    )
    .unwrap();
    let step = LifecycleTask::tick(&mut supervisor).await.unwrap();
    assert!(matches!(
        step,
        engine::Step::Enrollment(EnrollmentOutcome::Admitted { .. })
    ));
    assert!(supervisor.live.offer_key_ready().unwrap());
    assert_eq!(
        supervisor.live.network_encryption_public().unwrap(),
        context.network_encryption_public
    );
    let proofs: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == "eth_getProof")
        .map(|(_, args)| args.clone())
        .collect();
    assert_eq!(proofs.len(), 2);
    assert_eq!(proofs[0][2]["blockHash"], json!(chain.h1.hash));
    assert_eq!(proofs[1][2]["blockHash"], json!(chain.h2.hash));
    handle.stop().unwrap();
}

#[tokio::test]
async fn successor_upgrade_proof_retries_proof_window_move_in_the_same_tick() {
    let root = tempfile::tempdir().unwrap();
    let committee = FinalityCommitteeFixture::new(12);
    let (live, descriptor) = resident(9, None, &committee, false);
    let policy = no_attestation_dev_policy_v1(676, live.manifest().genesis_hash).unwrap();
    let source_intent = RegistrationIntentFixture {
        operation: AttestationOperationV1::RegisterEnclave,
        policy_hash: policy.policy_hash().unwrap(),
        binding_id: B256::repeat_byte(0x42),
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 50_000,
    }
    .for_manifest(live.manifest())
    .unwrap();
    let context = DcapOnboardingContextV1 {
        chain_id: source_intent.chain_id,
        genesis_hash: source_intent.genesis_hash,
        intent_hash: source_intent.intent_hash().unwrap(),
        node_id_hash: source_intent.node_id.node_id_hash().unwrap(),
        enclave_id: source_intent.enclave_id,
        binding_id: source_intent.binding_id,
        policy_hash: source_intent.policy_hash,
        recipient_x25519: source_intent.recipient_x25519,
        network_encryption_public: [0x44; 32],
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    let served = served_chain(
        &committee,
        &context,
        policy,
        binding_for(&source_intent),
        vec![1, 2, 3],
    );
    served.successor_upgrade.store(true, Ordering::SeqCst);
    let chain = Arc::new(served);
    let (url, handle, calls) = serve(chain.clone()).await;
    let mut supervisor = super::TeeLifecycleSupervisor::new(
        &url,
        (
            Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([0x47; 32]).unwrap()),
            k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap(),
        ),
        (live, root.path().to_owned(), descriptor),
    )
    .unwrap();
    supervisor
        .advance_successor()
        .await
        .expect("direct successor prepare");
    assert!(supervisor.successor.is_some());
    let raw = chain
        .sent
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("prepare transaction");
    let intent = prepare_intent_from_tx(&raw);
    let attempt = DcapOnboardingContextV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        intent_hash: intent.intent_hash().unwrap(),
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        policy_hash: intent.policy_hash,
        recipient_x25519: intent.recipient_x25519,
        network_encryption_public: chain.public_key,
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    *chain.pending.lock().unwrap() = ServedPending {
        context_hash: attempt.context_hash(),
        valid_until: intent.requested_valid_until,
        source_binding_id: source_intent.binding_id,
        target_hash: intent.upgrade_target_hash().unwrap(),
        nonce: intent.transition_nonce,
    };
    std::fs::remove_file(root.path().join("tee-service-tx-v1/journal.json")).unwrap();
    let step = LifecycleTask::tick(&mut supervisor).await;
    let proofs: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == "eth_getProof")
        .map(|(_, args)| args.clone())
        .collect();
    assert_eq!(proofs.len(), 2, "{step:?}");
    assert_eq!(proofs[0][2]["blockHash"], json!(chain.h1.hash));
    assert_eq!(proofs[1][2]["blockHash"], json!(chain.h2.hash));
    let error = step.expect_err("retry leaves the proof window");
    assert!(!error.is_proof_window_moved());
    handle.stop().unwrap();
}

#[tokio::test]
async fn background_admitted_publishes_pending_gate_and_arms_on_matching_local_head() {
    use crate::admission::{LocalTeeAdmissionAnchorV1, TeeLeaseGuardGateV1};
    use crate::admission_authority::live::*;
    use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState};
    use std::os::unix::fs::DirBuilderExt as _;

    let root = tempfile::tempdir().unwrap();
    let host = root.path().join(NODE_HOST_DIRECTORY_V1);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&host)
        .unwrap();
    let host_key = NodeHostIdentityKey::create_new(&host.join(NODE_HOST_IDENTITY_KEY_V1)).unwrap();
    let committee = FinalityCommitteeFixture::new(11);
    let (live, descriptor) = resident(9, Some(&host_key), &committee, false);
    let (donor, _) = resident(8, None, &committee, true);
    let policy = no_attestation_dev_policy_v1(676, live.manifest().genesis_hash).unwrap();

    // Reachable startup Path A (upstream at H_up == 0 -> prepare() returns Ok(None))
    // and Path B (non-founder validator at local H == 0 -> founder_lease_gate returns AwaitingAnchor).
    let (provider_genesis, _, _) = support::provider(&donor, 0, 1_000, 50_000, false);
    let gate_upstream_h0 = crate::tee::select_startup_lease_gate(
        &provider_genesis,
        0,
        (true, true, false),
        (None, None),
        || unreachable!("upstream launch never calls founder_lease_gate"),
    )
    .unwrap();
    let gate_non_founder_h0 = crate::tee::select_startup_lease_gate(
        &provider_genesis,
        0,
        (false, true, false),
        (None, None),
        || Ok(TeeLeaseGuardGateV1::AwaitingAnchor),
    )
    .unwrap();
    assert_eq!(gate_upstream_h0, TeeLeaseGuardGateV1::AwaitingAnchor);
    assert_eq!(gate_non_founder_h0, TeeLeaseGuardGateV1::AwaitingAnchor);

    // Later, network advances past block 1 and background LiveTask::tick enrolls and returns Admitted.
    let intent = RegistrationIntentFixture {
        operation: AttestationOperationV1::RegisterEnclave,
        policy_hash: policy.policy_hash().unwrap(),
        binding_id: B256::repeat_byte(0x41),
        binding_version: 1,
        registration_version: 0,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 50_000,
    }
    .for_manifest(live.manifest())
    .unwrap();
    let context = DcapOnboardingContextV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        intent_hash: intent.intent_hash().unwrap(),
        node_id_hash: intent.node_id.node_id_hash().unwrap(),
        enclave_id: intent.enclave_id,
        binding_id: intent.binding_id,
        policy_hash: intent.policy_hash,
        recipient_x25519: intent.recipient_x25519,
        network_encryption_public: donor.network_encryption_public().unwrap(),
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    let artifact = donor
        .prepare_no_attestation_dev_onboarding_artifact_v2(context)
        .unwrap()
        .onboarding_artifact;
    let chain = Arc::new(served_chain(
        &committee,
        &context,
        policy,
        binding_for(&intent),
        artifact,
    ));
    let (url, handle, _) = serve(chain.clone()).await;
    let supervisor = super::TeeLifecycleSupervisor::new(
        &url,
        (
            Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([0x47; 32]).unwrap()),
            k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap(),
        ),
        (Arc::clone(&live), root.path().to_owned(), descriptor),
    )
    .unwrap();

    let (provider_below, reg_chain, mut identity) = support::provider(
        &donor,
        chain.h2.height - 1,
        chain.h2.timestamp - 1,
        50_000,
        false,
    );
    identity.reth_p2p_public = live.identity().reth_p2p_public;
    identity.expected_enclave_id = Some(live.manifest().enclave_id().unwrap());
    let owner = Arc::new(
        LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                context: Arc::clone(&live),
                identity,
                gate: gate_upstream_h0,
            },
            TeeAuthorityChannel::new(TeeAuthorityState::UnkeyedAwaitingAdmission),
        )
        .unwrap(),
    );
    let protocol = Arc::new(crate::OutbeRuntime::new());
    let mut live_task = super::LiveTask {
        supervisor,
        owner: Arc::clone(&owner),
        provider: provider_below.clone(),
        protocol: Arc::clone(&protocol),
    };

    // Before admission (context admission_anchor is None), publish_admitted_anchor is a no-op.
    assert!(live.admission_anchor().unwrap().is_none());
    owner.publish_admitted_anchor().unwrap();
    assert_eq!(
        owner.lock().unwrap().gate,
        TeeLeaseGuardGateV1::AwaitingAnchor
    );

    let step = LifecycleTask::tick(&mut live_task).await.unwrap();
    let engine::Step::Enrollment(EnrollmentOutcome::Admitted { anchor }) = step else {
        panic!("expected background EnrollmentOutcome::Admitted, got {step:?}");
    };
    assert!(live.offer_key_ready().unwrap());

    let expected_anchor = LocalTeeAdmissionAnchorV1 {
        finalized_height: anchor.finalized_height,
        finalized_hash: anchor.finalized_hash,
    };
    let expected_gate = TeeLeaseGuardGateV1::Pending {
        target: expected_anchor,
        floor: None,
    };
    assert_eq!(owner.lock().unwrap().gate, expected_gate);

    // Below anchor height (H=1 < 2): authority stays closed and gate stays Pending.
    support::bind_context(&provider_below, &live);
    let closed = owner
        .refresh(&provider_below, protocol.as_ref(), &reg_chain)
        .unwrap();
    assert_eq!(closed, TeeAuthorityState::SuspendedPolicyMismatch);
    assert!(!closed.is_authority_open());
    assert_eq!(owner.lock().unwrap().gate, expected_gate);

    // At matching anchor height and canonical hash (H=2) with live binding + matching offer key:
    // refresh validates anchor and arms authority.
    let exact_header = outbe_consensus::committee_chain::decode_public_block(&chain.block)
        .unwrap()
        .header()
        .clone();
    let (mut provider_at_anchor, _, _) =
        support::provider(&live, chain.h2.height, chain.h2.timestamp, 50_000, false);
    provider_at_anchor
        .inner
        .headers
        .lock()
        .retain(|_, header| header.inner.number != chain.h2.height);
    provider_at_anchor
        .inner
        .add_header(chain.h2.hash, exact_header);
    provider_at_anchor.finalized = Some(alloy_eips::BlockNumHash::new(
        chain.h2.height,
        chain.h2.hash,
    ));
    let opened = owner
        .refresh(&provider_at_anchor, protocol.as_ref(), &reg_chain)
        .unwrap();
    assert!(
        opened.is_authority_open(),
        "expected open authority, got {opened:?}"
    );
    assert_eq!(owner.lock().unwrap().gate, TeeLeaseGuardGateV1::Armed);

    // Monotonicity: publish_admitted_anchor never downgrades Armed or a newer Pending gate.
    owner.publish_admitted_anchor().unwrap();
    assert_eq!(owner.lock().unwrap().gate, TeeLeaseGuardGateV1::Armed);
    let newer_gate = TeeLeaseGuardGateV1::Pending {
        target: LocalTeeAdmissionAnchorV1 {
            finalized_height: anchor.finalized_height + 5,
            finalized_hash: B256::repeat_byte(0xEE),
        },
        floor: Some(expected_anchor),
    };
    owner.lock().unwrap().gate = newer_gate;
    owner.publish_admitted_anchor().unwrap();
    assert_eq!(owner.lock().unwrap().gate, newer_gate);

    // State-based idempotency: if gate is still AwaitingAnchor when a subsequent Resident tick runs,
    // publish_admitted_anchor recovers it from owner.context.admission_anchor().
    owner.lock().unwrap().gate = TeeLeaseGuardGateV1::AwaitingAnchor;
    let resident_step = LifecycleTask::tick(&mut live_task).await.unwrap();
    assert!(matches!(resident_step, engine::Step::Resident { .. }));
    assert_eq!(owner.lock().unwrap().gate, expected_gate);

    // Error-path publication (B1): even when observe_tick fails after admission (stopped HTTP server),
    // LiveTask::tick still publishes Pending before propagating the error.
    owner.lock().unwrap().gate = TeeLeaseGuardGateV1::AwaitingAnchor;
    handle.stop().unwrap();
    handle.stopped().await;
    let failed_tick = LifecycleTask::tick(&mut live_task).await;
    assert!(
        failed_tick.is_err(),
        "stopped HTTP server must cause observe_tick Err, got {failed_tick:?}"
    );
    assert_eq!(owner.lock().unwrap().gate, expected_gate);
}
