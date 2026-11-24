use super::*;
use alloy_primitives::Bytes;
use alloy_sol_types::{SolCall, SolValue};
use jsonrpsee::{types::ErrorObjectOwned, RpcModule};
use outbe_primitives::{
    addresses::{TEE_REGISTRY_ADDRESS, VALIDATOR_SET_ADDRESS},
    tee_operator_v1::TeeRenewalScheduleV1,
    tee_registry_abi_v1::{ITeeRegistryV1, NodeEnclaveBindingV1View},
};
use outbe_tee::registry_view::RenewalBindingV1;
use outbe_tee::TeeAuthorityState;
use outbe_validatorset::precompile::IValidatorSet;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Founding {
    genesis: B256,
    hash: B256,
    state_root: B256,
    height: u64,
    timestamp: u64,
    base_fee: U256,
    chain_id: u64,
    policy: Vec<u8>,
    offer_public: U256,
    binding: NodeEnclaveBindingV1View,
    schedule: TeeRenewalScheduleV1,
    sent: Mutex<Vec<Vec<u8>>>,
    submitted: tokio::sync::Notify,
}

fn founding_methods(chain: Arc<Founding>) -> (RpcModule<()>, RpcModule<()>) {
    let mut module = RpcModule::new(());
    let state = chain.clone();
    module
        .register_method("eth_chainId", move |_, _, _| {
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", state.chain_id))
        })
        .unwrap();
    let state = chain.clone();
    module
        .register_method("eth_getBlockByNumber", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            Ok::<_, ErrorObjectOwned>(state.block(args.first()))
        })
        .unwrap();
    let state = chain.clone();
    module
        .register_method("eth_call", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            let to: Address = args[0]["to"].as_str().unwrap().parse().unwrap();
            let data = alloy_primitives::hex::decode(
                args[0]["data"]
                    .as_str()
                    .unwrap_or("0x")
                    .trim_start_matches("0x"),
            )
            .unwrap_or_default();
            match state.dispatch(to, &data) {
                Ok(encoded) => Ok(format!("0x{}", alloy_primitives::hex::encode(encoded))),
                Err(error) => Err(ErrorObjectOwned::owned(
                    -32000,
                    error.to_string(),
                    None::<()>,
                )),
            }
        })
        .unwrap();
    module
        .register_method("eth_getBalance", move |_, _, _| {
            Ok::<_, ErrorObjectOwned>("0x1")
        })
        .unwrap();
    module
        .register_method("eth_getTransactionCount", move |_, _, _| {
            Ok::<_, ErrorObjectOwned>("0x0")
        })
        .unwrap();
    let state = chain.clone();
    module
        .register_method("eth_sendRawTransaction", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            let raw = alloy_primitives::hex::decode(
                args.first()
                    .and_then(Value::as_str)
                    .unwrap_or("0x")
                    .trim_start_matches("0x"),
            )
            .unwrap_or_default();
            state.sent.lock().unwrap().push(raw.clone());
            state.submitted.notify_one();
            Ok::<_, ErrorObjectOwned>(B256::from(alloy_primitives::keccak256(raw)))
        })
        .unwrap();
    let mut outbe = RpcModule::new(());
    outbe
        .register_method("outbe_teeRenewalScheduleV1", move |_, _, _| {
            Ok::<_, ErrorObjectOwned>(serde_json::to_value(&chain.schedule).unwrap())
        })
        .unwrap();
    (module, outbe)
}

#[derive(Debug, thiserror::Error)]
enum FoundingDispatchError {
    #[error("missing ABI selector")]
    MissingSelector,
    #[error("unexpected call to {to}, selector {selector}")]
    Unexpected { to: Address, selector: String },
}

impl Founding {
    fn block(&self, tag: Option<&Value>) -> Value {
        let genesis = tag.and_then(Value::as_str) == Some("0x0");
        json!({
            "number": if genesis { "0x0".to_owned() } else { format!("0x{:x}", self.height) },
            "hash": if genesis { self.genesis } else { self.hash },
            "stateRoot": self.state_root,
            "timestamp": format!("0x{:x}", if genesis { 1 } else { self.timestamp }),
            "baseFeePerGas": format!("0x{:x}", self.base_fee),
            "parentHash": self.genesis,
        })
    }
    fn dispatch(&self, to: Address, data: &[u8]) -> Result<Vec<u8>, FoundingDispatchError> {
        let selector = data
            .get(..4)
            .ok_or(FoundingDispatchError::MissingSelector)?;
        let encoded = match (to, selector) {
            (TEE_REGISTRY_ADDRESS, s) if s == ITeeRegistryV1::activePolicyV1Call::SELECTOR => {
                ITeeRegistryV1::activePolicyV1Call::abi_encode_returns(&Bytes::copy_from_slice(
                    &self.policy,
                ))
            }
            (TEE_REGISTRY_ADDRESS, s)
                if s == ITeeRegistryV1::stagedSuccessorPolicyV1Call::SELECTOR =>
            {
                (false, U256::ZERO, Bytes::new()).abi_encode_params()
            }
            (TEE_REGISTRY_ADDRESS, s)
                if s == ITeeRegistryV1::validatorEnclaveBindingCall::SELECTOR =>
            {
                ITeeRegistryV1::validatorEnclaveBindingCall::abi_encode_returns(&self.binding)
            }
            (TEE_REGISTRY_ADDRESS, s) if s == ITeeRegistryV1::offerPublicKeyCall::SELECTOR => {
                ITeeRegistryV1::offerPublicKeyCall::abi_encode_returns(&self.offer_public)
            }
            (TEE_REGISTRY_ADDRESS, s) if s == ITeeRegistryV1::enclaveUpgradeV1Call::SELECTOR => {
                ITeeRegistryV1::enclaveUpgradeV1Call::abi_encode_returns(
                    &ITeeRegistryV1::enclaveUpgradeV1Return {
                        proposalId: U256::ZERO,
                        activationHeight: 0,
                        mrenclave: B256::ZERO,
                        successorPolicyHash: B256::ZERO,
                        predecessorPolicyHash: B256::ZERO,
                    },
                )
            }
            (TEE_REGISTRY_ADDRESS, s)
                if s == ITeeRegistryV1::pendingEnclaveUpgradeCall::SELECTOR =>
            {
                ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_encode_returns(
                    &ITeeRegistryV1::pendingEnclaveUpgradeReturn {
                        contextHash: B256::ZERO,
                        validUntil: 0,
                        sourceBindingId: B256::ZERO,
                        targetHash: B256::ZERO,
                        nonce: 0,
                    },
                )
            }
            (TEE_REGISTRY_ADDRESS, s)
                if s == ITeeRegistryV1::keyEpochCall::SELECTOR
                    || s == ITeeRegistryV1::offerEpochCall::SELECTOR =>
            {
                U256::ZERO.abi_encode()
            }
            (VALIDATOR_SET_ADDRESS, s) if s == IValidatorSet::isValidatorCall::SELECTOR => {
                IValidatorSet::isValidatorCall::abi_encode_returns(&false)
            }
            _ => {
                return Err(FoundingDispatchError::Unexpected {
                    to,
                    selector: alloy_primitives::hex::encode(selector),
                })
            }
        };
        Ok(encoded)
    }
}

#[tokio::test]
async fn founding_without_http_sends_renew_enclave_in_process() {
    use clap::Parser as _;
    #[derive(clap::Parser)]
    struct Args {
        #[command(flatten)]
        consensus: outbe_consensus::args::ConsensusArgs,
    }

    let harness = Harness::new();
    harness
        .live
        .activate_for_test(
            outbe_tee::resident::ResidentOfferMaterial::for_test(
                vec![5; 96],
                B256::from(harness.live.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    let manifest = harness.live.manifest().clone();
    let policy = harness.chain.policy.clone();
    let valid_until = 10_000;
    let binding = RenewalBindingV1 {
        node_id_hash: manifest.node_id.node_id_hash().unwrap(),
        enclave_id: manifest.enclave_id().unwrap(),
        binding_id: B256::repeat_byte(0x75),
        intent_hash: B256::repeat_byte(0x76),
        evidence_hash: B256::ZERO,
        policy_hash: policy.policy_hash().unwrap(),
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
    let chain = founding_chain(&harness, &policy, binding);
    let authority = outbe_runtime::LifecycleAuthorityProbe::new(
        Arc::clone(&harness.live),
        outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
            reth_p2p_public: harness.live.identity().reth_p2p_public,
            expected_enclave_id: Some(manifest.enclave_id().unwrap()),
            validator: None,
        },
        (chain.height, chain.hash),
        outbe_tee::TeeAuthorityChannel::default(),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    provision_host_key(root.path());
    let cancel = tokio_util::sync::CancellationToken::new();
    let args = Args::parse_from(["outbe-chain", "--validator"]).consensus;
    let rpc = reth_node_core::args::RpcServerArgs {
        http: false,
        ..Default::default()
    };
    let worker = authority.spawn_founding(
        founding_methods(Arc::clone(&chain)),
        (&args, &rpc),
        super::test_signers(),
        (Arc::clone(&harness.live), root.path().to_owned(), TrustedNetworkDescriptorV1 {
            network_binding: manifest.network_binding(),
            genesis_consensus_keys: vec![],
        }),
        (reth_provider::test_utils::NoopProvider::<_, outbe_primitives::OutbePrimitives>::new(
            Arc::new(reth_chainspec::ChainSpec::<OutbeHeader>::default()),
        ), cancel.clone()),
    ).unwrap();
    let submitted = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        chain.submitted.notified(),
    )
    .await;
    cancel.cancel();
    worker.await.unwrap();
    submitted.expect("the founding worker must submit renewEnclave with HTTP disabled");
    let journal = ServiceTxJournalV1::open(root.path()).unwrap();
    let record = journal.load().unwrap().unwrap();
    assert_eq!(record.kind, ServiceTxKindV1::Renew);
    let transactions = chain.sent.lock().unwrap();
    assert_eq!(transactions.len(), 1);
    let raw = &transactions[0];
    use alloy_consensus::{transaction::RlpEcdsaDecodableTx, TxEip1559};
    let tx = TxEip1559::rlp_decode_signed(&mut &raw[1..]).unwrap();
    assert_eq!(
        tx.tx().to,
        alloy_primitives::TxKind::Call(TEE_REGISTRY_ADDRESS)
    );
    ITeeRegistryV1::renewEnclaveCall::abi_decode(tx.tx().input.as_ref()).unwrap();
}

fn founding_chain(
    harness: &Harness,
    policy: &TeePolicyV1,
    binding: RenewalBindingV1,
) -> Arc<Founding> {
    Arc::new(Founding {
        genesis: harness.chain.at.genesis_hash,
        hash: harness.chain.at.hash,
        state_root: harness.chain.at.state_root,
        height: harness.chain.at.height,
        timestamp: 7_000,
        base_fee: U256::from(1),
        chain_id: harness.chain.at.chain_id,
        policy: policy.encode_canonical().unwrap(),
        offer_public: U256::from_be_bytes(harness.live.network_encryption_public().unwrap()),
        binding: (&binding).into(),
        schedule: TeeRenewalScheduleV1 {
            finalized_height: harness.chain.at.height,
            finalized_hash: harness.chain.at.hash,
            finalized_timestamp: 7_000,
            epoch_number: 1,
            epoch_start_height: 0,
            epoch_length_blocks: 100,
            next_freeze_height: 80,
            planned_activation_height: 100,
            dkg_prepare_window_blocks: 20,
            minimum_block_time_millis: 1_000,
        },
        sent: Mutex::new(Vec::new()),
        submitted: tokio::sync::Notify::new(),
    })
}

#[tokio::test]
async fn broken_bls_key_is_typed_before_enrollment() {
    use clap::Parser as _;
    #[derive(clap::Parser)]
    struct Args {
        #[command(flatten)]
        consensus: outbe_consensus::args::ConsensusArgs,
    }
    let harness = Harness::new();
    let path = harness.dir.join("broken.bls");
    std::fs::write(&path, b"not-a-bls-key").unwrap();
    let evm_key = harness.dir.join("operator.hex");
    std::fs::write(&evm_key, hex::encode([7; 32])).unwrap();
    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut rpc = RpcModule::new(Arc::clone(&sent));
    rpc.register_method("eth_sendRawTransaction", |_, sent, _| {
        sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok::<_, ErrorObjectOwned>(B256::ZERO)
    })
    .unwrap();
    rpc.register_method("eth_chainId", |_, _, _| format!("0x{:x}", TEST_CHAIN_ID))
        .unwrap();
    let server = jsonrpsee::server::ServerBuilder::default()
        .build("127.0.0.1:0")
        .await
        .unwrap();
    let upstream = format!("http://{}", server.local_addr().unwrap());
    let handle = server.start(rpc);
    use jsonrpsee::core::client::ClientT as _;
    let client = jsonrpsee::http_client::HttpClientBuilder::default()
        .build(&upstream)
        .unwrap();
    let advertised_chain: String = client
        .request("eth_chainId", jsonrpsee::rpc_params![])
        .await
        .unwrap();
    assert_eq!(advertised_chain, format!("0x{:x}", TEST_CHAIN_ID));
    let args = Args::try_parse_from([
        "outbe-chain",
        "--validator",
        "--upstream",
        &upstream,
        "--consensus.signing-key",
        path.to_str().unwrap(),
        "--validator.evm-key",
        evm_key.to_str().unwrap(),
    ])
    .unwrap()
    .consensus;
    let _ = outbe_runtime::outbe_default_txpool_values().try_init();
    let config = reth_node_builder::NodeConfig::new(Arc::new(reth_chainspec::ChainSpec::default()));
    let error = outbe_runtime::prepare_validator_for_test(
        &config,
        &args,
        harness.dir.clone(),
        &harness.chain.policy,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .chain()
            .any(|cause| cause.downcast_ref::<hex::FromHexError>().is_some()),
        "{error:?}"
    );
    assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert!(harness.journal.load().unwrap().is_none());
    handle.stop().unwrap();
    handle.stopped().await;
}

#[path = "authority_close.rs"]
mod close;

#[tokio::test]
async fn jailed_finalized_block_closes_private_rpc_and_keeps_the_key() {
    close::authority_closes_private_rpc(TeeAuthorityState::SuspendedJailed).await;
}

#[tokio::test]
async fn expired_lease_closes_private_rpc_and_keeps_the_key() {
    close::authority_closes_private_rpc(TeeAuthorityState::SuspendedLeaseExpired).await;
}
