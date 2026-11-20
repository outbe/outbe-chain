//! In-process founding lifecycle tick through the production module seam.
use super::*;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolValue};
use jsonrpsee::{types::ErrorObjectOwned, RpcModule};
use outbe_primitives::{
    addresses::VALIDATOR_SET_ADDRESS,
    chain::DEVNET_CHAIN_ID,
    tee_attestation_v1::*,
    tee_operator_v1::TeeRenewalScheduleV1,
    tee_registry_abi_v1::{ITeeRegistryV1, NodeEnclaveBindingV1View},
    tee_test_utils::no_attestation_dev_policy_v1,
};
use outbe_tee::resident::{ResidentOfferMaterial, ResidentTeeKeys};
use outbe_validatorset::precompile::IValidatorSet;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

pub(super) type RpcCalls = Arc<Mutex<Vec<String>>>;

pub(super) struct ScriptedRegistry {
    pub policy: TeePolicyV1,
    pub schedule: TeeRenewalScheduleV1,
    pub genesis: B256,
    pub state_root: B256,
    pub base_fee: U256,
    pub binding: NodeEnclaveBindingV1View,
    pub pending_context: B256,
    pub balance: U256,
    pub staged: Option<TeePolicyV1>,
    pub receipt_success: bool,
}

pub(super) fn absent_binding() -> NodeEnclaveBindingV1View {
    NodeEnclaveBindingV1View {
        exists: false,
        nodeIdHash: B256::ZERO,
        enclaveId: B256::ZERO,
        bindingId: B256::ZERO,
        intentHash: B256::ZERO,
        evidenceHash: B256::ZERO,
        policyHash: B256::ZERO,
        bindingVersion: 0,
        registrationVersion: 0,
        renewalNonce: 0,
        transitionNonce: 0,
        leaseStartedAt: 0,
        validUntil: 0,
        collateralValidUntil: 0,
        recipientX25519: B256::ZERO,
        attestationEd25519: B256::ZERO,
        noiseResponderX25519: B256::ZERO,
        mrenclave: B256::ZERO,
        mrsigner: B256::ZERO,
        isvProdId: 0,
        isvSvn: 0,
        platformTcbStatus: 0,
        verdictHash: B256::ZERO,
        nodeHostAuthorizationHash: B256::ZERO,
    }
}

pub(super) fn devnet_schedule() -> TeeRenewalScheduleV1 {
    TeeRenewalScheduleV1 {
        finalized_height: 120,
        finalized_hash: B256::repeat_byte(0x70),
        finalized_timestamp: 1_000,
        epoch_number: 2,
        epoch_start_height: 100,
        epoch_length_blocks: 100,
        next_freeze_height: 180,
        planned_activation_height: 200,
        dkg_prepare_window_blocks: 20,
        minimum_block_time_millis: 2_000,
    }
}

pub(super) fn devnet_resident() -> (Arc<InProcessTeeContext>, TeePolicyV1, B256) {
    let genesis = B256::repeat_byte(0x11);
    let policy = no_attestation_dev_policy_v1(DEVNET_CHAIN_ID, genesis).unwrap();
    let keys = ResidentTeeKeys::from_seed([4; 32]).unwrap();
    let node = k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap();
    let manifest = EnclaveInitializationManifestV1 {
        chain_id: U256::from(DEVNET_CHAIN_ID).to_be_bytes(),
        genesis_hash: genesis,
        attestation_mode: AttestationMode::NoAttestationDev,
        node_id: NodeIdV1 {
            reth_p2p_public: node
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
        },
        initialization_challenge: [3; 32],
        node_host_noise_x25519: [4; 32],
        recipient_x25519: keys.tribute_offer_public(),
        attestation_ed25519: keys.attestation_pub(),
        noise_responder_x25519: keys.noise_public(),
    };
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: manifest.network_binding(),
        genesis_consensus_keys: vec![],
    };
    let identity = outbe_tee::NodeHostIdentityV1 {
        network_binding: descriptor.network_binding,
        reth_p2p_public: manifest.node_id.reth_p2p_public,
    };
    let context = Arc::new(
        InProcessTeeContext::new(
            keys,
            identity,
            manifest,
            descriptor,
            outbe_tee::in_process::ProofSourceV1::NoHardware,
        )
        .unwrap(),
    );
    context
        .activate_for_test(
            ResidentOfferMaterial::for_test(
                vec![7; 96],
                B256::from(context.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    (context, policy, genesis)
}

fn head(script: &ScriptedRegistry) -> Value {
    json!({
        "number": format!("0x{:x}", script.schedule.finalized_height),
        "hash": script.schedule.finalized_hash,
        "stateRoot": script.state_root,
        "timestamp": format!("0x{:x}", script.schedule.finalized_timestamp),
        "baseFeePerGas": format!("0x{:x}", script.base_fee),
        "parentHash": script.genesis,
    })
}

fn chain_id_of(policy: &TeePolicyV1) -> u64 {
    u64::try_from(U256::from_be_bytes(policy.chain_id)).unwrap()
}

fn dispatch(script: &ScriptedRegistry, to: Address, data: &[u8]) -> Result<Vec<u8>, String> {
    let policy_bytes = Bytes::from(script.policy.encode_canonical().unwrap());
    let pending = ITeeRegistryV1::pendingEnclaveUpgradeReturn {
        contextHash: script.pending_context,
        validUntil: script.schedule.finalized_timestamp.saturating_add(1),
        sourceBindingId: script.binding.bindingId,
        targetHash: B256::repeat_byte(0x21),
        nonce: 1,
    };
    let encoded = if data.starts_with(&ITeeRegistryV1::activePolicyV1Call::SELECTOR) {
        ITeeRegistryV1::activePolicyV1Call::abi_encode_returns(&policy_bytes)
    } else if data.starts_with(&ITeeRegistryV1::stagedSuccessorPolicyV1Call::SELECTOR) {
        script
            .staged
            .as_ref()
            .map(|policy| {
                (
                    true,
                    U256::from(7),
                    Bytes::from(policy.encode_canonical().unwrap()),
                )
            })
            .unwrap_or((false, U256::ZERO, Bytes::new()))
            .abi_encode_params()
    } else if data.starts_with(&ITeeRegistryV1::validatorEnclaveBindingCall::SELECTOR)
        || data.starts_with(&ITeeRegistryV1::nodeHostEnclaveBindingCall::SELECTOR)
    {
        ITeeRegistryV1::validatorEnclaveBindingCall::abi_encode_returns(&script.binding)
    } else if data.starts_with(&ITeeRegistryV1::tributeOfferPublicKeyCall::SELECTOR) {
        ITeeRegistryV1::tributeOfferPublicKeyCall::abi_encode_returns(&U256::from(7))
    } else if data.starts_with(&ITeeRegistryV1::enclaveUpgradeV1Call::SELECTOR) {
        ITeeRegistryV1::enclaveUpgradeV1Call::abi_encode_returns(
            &ITeeRegistryV1::enclaveUpgradeV1Return {
                proposalId: U256::ZERO,
                activationHeight: 0,
                mrenclave: B256::ZERO,
                successorPolicyHash: B256::ZERO,
                predecessorPolicyHash: B256::ZERO,
            },
        )
    } else if data.starts_with(&ITeeRegistryV1::pendingEnclaveUpgradeCall::SELECTOR) {
        ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_encode_returns(&pending)
    } else if data.starts_with(&ITeeRegistryV1::keyEpochCall::SELECTOR)
        || data.starts_with(&ITeeRegistryV1::tributeOfferEpochCall::SELECTOR)
    {
        U256::ZERO.abi_encode()
    } else if to == VALIDATOR_SET_ADDRESS
        && data.starts_with(&IValidatorSet::isValidatorCall::SELECTOR)
    {
        IValidatorSet::isValidatorCall::abi_encode_returns(&false)
    } else {
        return Err(format!(
            "unexpected call to {to} selector {}",
            alloy_primitives::hex::encode(&data[..data.len().min(4)])
        ));
    };
    Ok(encoded)
}

pub(super) fn eth_module(script: Arc<Mutex<ScriptedRegistry>>, calls: RpcCalls) -> RpcModule<()> {
    let mut module = RpcModule::new(());
    let state = script.clone();
    let seen = calls.clone();
    module
        .register_method("eth_chainId", move |_, _, _| {
            seen.lock().unwrap().push("eth_chainId".into());
            let chain = chain_id_of(&state.lock().unwrap().policy);
            Ok::<_, ErrorObjectOwned>(format!("0x{chain:x}"))
        })
        .unwrap();
    let state = script.clone();
    let seen = calls.clone();
    module
        .register_method("eth_getBlockByNumber", move |params, _, _| {
            seen.lock().unwrap().push("eth_getBlockByNumber".into());
            let args: Vec<Value> = params.parse()?;
            let script = state.lock().unwrap();
            let mut block = head(&script);
            if args[0] == "0x0" {
                block["number"] = json!("0x0");
                block["hash"] = json!(script.genesis);
            }
            Ok::<_, ErrorObjectOwned>(block)
        })
        .unwrap();
    let state = script.clone();
    let seen = calls.clone();
    module
        .register_method("eth_call", move |params, _, _| {
            seen.lock().unwrap().push("eth_call".into());
            let args: Vec<Value> = params.parse()?;
            let to: Address =
                args[0]["to"].as_str().unwrap().parse().map_err(|error| {
                    ErrorObjectOwned::owned(-32000, format!("{error}"), None::<()>)
                })?;
            let data = alloy_primitives::hex::decode(
                args[0]["data"]
                    .as_str()
                    .unwrap_or("0x")
                    .trim_start_matches("0x"),
            )
            .unwrap_or_default();
            match dispatch(&state.lock().unwrap(), to, &data) {
                Ok(encoded) => Ok(format!("0x{}", alloy_primitives::hex::encode(encoded))),
                Err(error) => Err(ErrorObjectOwned::owned(-32000, error, None::<()>)),
            }
        })
        .unwrap();
    let state = script.clone();
    let seen = calls.clone();
    module
        .register_method("eth_getBalance", move |_, _, _| {
            seen.lock().unwrap().push("eth_getBalance".into());
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", state.lock().unwrap().balance))
        })
        .unwrap();
    let seen = calls.clone();
    module
        .register_method("eth_getTransactionCount", move |_, _, _| {
            seen.lock().unwrap().push("eth_getTransactionCount".into());
            Ok::<_, ErrorObjectOwned>("0x0")
        })
        .unwrap();
    let state = script.clone();
    let seen = calls;
    module
        .register_method("eth_sendRawTransaction", move |params, _, _| {
            seen.lock().unwrap().push("eth_sendRawTransaction".into());
            let args: Vec<Value> = params.parse()?;
            let raw = alloy_primitives::hex::decode(
                args.first()
                    .and_then(Value::as_str)
                    .unwrap_or("0x")
                    .trim_start_matches("0x"),
            )
            .unwrap_or_default();
            Ok::<_, ErrorObjectOwned>(B256::from(alloy_primitives::keccak256(raw)))
        })
        .unwrap();
    module
        .register_method("eth_getTransactionReceipt", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            let script = state.lock().unwrap();
            if !script.receipt_success {
                return Ok::<_, ErrorObjectOwned>(Value::Null);
            }
            Ok::<_, ErrorObjectOwned>(json!({
                "transactionHash": args.first().cloned().unwrap_or(Value::Null),
                "blockHash": script.schedule.finalized_hash,
                "blockNumber": format!("0x{:x}", script.schedule.finalized_height),
                "status": "0x1",
            }))
        })
        .unwrap();
    module
}

pub(super) fn outbe_module(script: Arc<Mutex<ScriptedRegistry>>, calls: RpcCalls) -> RpcModule<()> {
    let mut module = RpcModule::new(());
    module
        .register_method("outbe_teeRenewalScheduleV1", move |_, _, _| {
            calls
                .lock()
                .unwrap()
                .push("outbe_teeRenewalScheduleV1".into());
            Ok::<_, ErrorObjectOwned>(
                serde_json::to_value(&script.lock().unwrap().schedule).unwrap(),
            )
        })
        .unwrap();
    module
}

pub(super) fn registry_module(
    script: Arc<Mutex<ScriptedRegistry>>,
    calls: RpcCalls,
) -> RpcModule<()> {
    let mut module = eth_module(script.clone(), calls.clone());
    module
        .merge(outbe_module(script, calls))
        .expect("eth and outbe methods are disjoint");
    module
}

pub(super) fn scripted(policy: TeePolicyV1, genesis: B256) -> Arc<Mutex<ScriptedRegistry>> {
    Arc::new(Mutex::new(ScriptedRegistry {
        policy,
        schedule: devnet_schedule(),
        genesis,
        state_root: B256::repeat_byte(0x71),
        base_fee: U256::from(3),
        binding: absent_binding(),
        pending_context: B256::ZERO,
        balance: U256::ZERO,
        staged: None,
        receipt_success: false,
    }))
}

pub(super) fn bind_manifest(
    manifest: &EnclaveInitializationManifestV1,
    valid_until: u64,
    policy_hash: B256,
) -> NodeEnclaveBindingV1View {
    let record = outbe_tee::registry_view::RenewalBindingV1 {
        node_id_hash: manifest.node_id.node_id_hash().unwrap(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x75),
        intent_hash: B256::repeat_byte(0x76),
        evidence_hash: B256::ZERO,
        policy_hash,
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 0,
        lease_started_at: 1,
        valid_until,
        collateral_valid_until: u64::MAX,
        recipient_x25519: B256::from(manifest.recipient_x25519),
        attestation_ed25519: B256::from(manifest.attestation_ed25519),
        noise_responder_x25519: B256::from(manifest.noise_responder_x25519),
        mrenclave: B256::ZERO,
        mrsigner: B256::ZERO,
        isv_prod_id: 0,
        isv_svn: 0,
        platform_tcb_status: 0,
        verdict_hash: B256::ZERO,
        node_host_authorization_hash: manifest.node_host_authorization_hash().unwrap(),
    };
    (&record).into()
}

#[tokio::test]
async fn in_process_founding_tick_omits_domain_rpc() {
    let skipped = crate::launch::stage_founding_lifecycle_module(
        false,
        RpcModule::new(()),
        &RpcModule::new(()),
    )
    .unwrap();
    assert!(skipped.is_none());
    let (live, policy, genesis) = devnet_resident();
    let calls = RpcCalls::default();
    let script = scripted(policy, genesis);
    let methods = crate::launch::stage_founding_lifecycle_module(
        true,
        eth_module(script.clone(), calls.clone()),
        &outbe_module(script, calls.clone()),
    )
    .unwrap()
    .expect("founding lifecycle module");
    for method in [
        "outbe_deriveKeys",
        "outbe_deriveGratisKeys",
        "outbe_getTributeStateSource",
    ] {
        assert!(methods.method(method).is_none(), "{method}");
    }
    assert!(methods.method("eth_chainId").is_some());
    assert!(methods.method("outbe_teeRenewalScheduleV1").is_some());
    let root = tempfile::tempdir().unwrap();
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: live.manifest().network_binding(),
        genesis_consensus_keys: vec![],
    };
    let mut supervisor = TeeLifecycleSupervisor::new_in_process(
        methods,
        (
            Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([0x47; 32]).unwrap()),
            k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap(),
        ),
        (live, root.path().to_owned(), descriptor),
    )
    .unwrap();
    let step = supervisor
        .observe_tick()
        .await
        .expect("in-process founding tick");
    assert!(matches!(
        step,
        engine::Step::Enrollment(EnrollmentOutcome::WaitForGasBalance)
    ));
    assert!(calls
        .lock()
        .unwrap()
        .iter()
        .any(|method| method == "outbe_teeRenewalScheduleV1"));
}

#[tokio::test]
async fn launch_slot_http_off_spawns_founding_supervisor() {
    let rpc = reth_node_core::args::RpcServerArgs {
        http: false,
        ..Default::default()
    };
    let (live, policy, genesis) = devnet_resident();
    let calls = RpcCalls::default();
    let script = scripted(policy, genesis);
    let slot = std::sync::Mutex::new(None);
    crate::launch::store_founding_module(
        true,
        eth_module(script.clone(), calls.clone()),
        &outbe_module(script, calls.clone()),
        &slot,
    )
    .unwrap();
    let signer = Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([0x47; 32]).unwrap());
    let root = tempfile::tempdir().unwrap();
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: live.manifest().network_binding(),
        genesis_consensus_keys: vec![],
    };
    let identity = outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
        reth_p2p_public: live.identity().reth_p2p_public,
        expected_enclave_id: Some(live.manifest().enclave_id().unwrap()),
        validator: None,
    };
    let owner = Arc::new(
        crate::admission_authority::live::LiveTeeRuntimeV1::new(
            crate::admission_authority::live::LiveTeeSnapshotV1 {
                context: Arc::clone(&live),
                identity,
                gate: crate::TeeLeaseGuardGateV1::AwaitingAnchor,
            },
            outbe_tee::TeeAuthorityChannel::new(
                outbe_tee::TeeAuthorityState::UnkeyedAwaitingAdmission,
            ),
        )
        .unwrap(),
    );
    let (provider, _, _) =
        crate::admission_authority::test_support::provider(&live, 120, 1_000, 9_000, false);
    let cancel = tokio_util::sync::CancellationToken::new();
    let node_shutdown = outbe_execution::shutdown::NodeShutdown::default();
    let handle = crate::tee::spawn_founding_validator_supervisor(
        &slot,
        crate::tee::FoundingSlotRequest {
            rpc: &rpc,
            upstream: None,
            evm_signer: Some(&signer),
            is_validator: true,
        },
        crate::tee::FoundingResident {
            node_signing: k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap(),
            live,
            node_data_dir: root.path().to_owned(),
            descriptor,
        },
        crate::tee::FoundingRun {
            owner,
            provider,
            protocol: Arc::new(crate::OutbeRuntime::new()),
            shutdown: cancel.clone(),
            finalized: Arc::new(tokio::sync::Notify::new()),
        },
        &node_shutdown,
    )
    .unwrap();
    assert!(slot.lock().unwrap().is_none(), "slot must be consumed");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if calls
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "outbe_teeRenewalScheduleV1")
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background supervisor made no request: {:?}",
            calls.lock().unwrap()
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    cancel.cancel();
    handle.await.unwrap();
}
