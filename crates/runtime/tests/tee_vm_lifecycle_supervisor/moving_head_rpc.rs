use crate::{boundary_artifact::boundary_extra, finality, proof, provision_host_key, test_signers};
use alloy_consensus::{BlockHeader as _, Header};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolEvent, SolValue};
use jsonrpsee::{types::ErrorObjectOwned, RpcModule};
use outbe_primitives::tee_attestation_v1::*;
use outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1;
use outbe_primitives::tee_registry_abi_v1::{ITeeRegistryV1, NodeEnclaveBindingV1View};
use outbe_primitives::tee_test_utils::{no_attestation_dev_policy_v1, RegistrationIntentFixture};
use outbe_primitives::{addresses::TEE_REGISTRY_ADDRESS, addresses::VALIDATOR_SET_ADDRESS};
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;
use outbe_tee::finalized_admission::{
    onboarding_registry_slots_v1, upgrade_registry_slots_v1, FinalizedAdmissionWitnessV1,
};
use outbe_tee::lifecycle::{enrollment::EnrollmentOutcome, LifecycleCheckpointV1};
use outbe_tee::registry_view::RenewalBindingV1;
use outbe_tee::resident::ResidentOfferMaterial;
use outbe_tee::upgrade_transfer::UpgradeKeyProofV1;
use outbe_validatorset::precompile::IValidatorSet;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn hex_prefixed(bytes: &[u8]) -> String {
    format!("0x{}", alloy_primitives::hex::encode(bytes))
}

fn binding_for(intent: &RegistrationIntentV1) -> RenewalBindingV1 {
    let mut binding = outbe_tee::test_utils::RenewalBindingFixtureV1::new(1)
        .versions(intent.binding_version, intent.registration_version)
        .nonces(intent.renewal_nonce, intent.transition_nonce)
        .lease(1, intent.requested_valid_until, u64::MAX)
        .authorization(intent.node_host_authorization_hash)
        .build();
    binding.node_id_hash = intent.node_id.node_id_hash().unwrap();
    binding.enclave_id = intent.enclave_id;
    binding.binding_id = intent.binding_id;
    binding.intent_hash = intent.intent_hash().unwrap();
    binding.policy_hash = intent.policy_hash;
    binding.recipient_x25519 = intent.recipient_x25519.into();
    binding.attestation_ed25519 = intent.attestation_ed25519.into();
    binding.noise_responder_x25519 = intent.noise_responder_x25519.into();
    binding
}

fn onboarding_values(context: &DcapOnboardingContextV1) -> [U256; 9] {
    [
        U256::from_be_bytes(context.network_encryption_public),
        U256::from(context.key_epoch),
        U256::from(context.network_encryption_epoch),
        U256::from_be_bytes(context.enclave_id.0),
        U256::from_be_bytes(context.binding_id.0),
        U256::from_be_bytes(context.intent_hash.0),
        U256::from_be_bytes(context.policy_hash.0),
        U256::from(50_000_u64),
        U256::from_be_bytes(context.recipient_x25519),
    ]
}

fn upgrade_values(context: &DcapOnboardingContextV1) -> [U256; 9] {
    [
        U256::from_be_bytes(context.network_encryption_public),
        U256::from(context.key_epoch),
        U256::from(context.network_encryption_epoch),
        U256::from_be_bytes(context.context_hash().0),
        U256::from(40_000_u64),
        U256::from(17),
        U256::from(17),
        U256::from_be_bytes(context.policy_hash.0),
        U256::from(1),
    ]
}

struct Opening {
    root: B256,
    slots: [B256; 9],
    value: Value,
}

impl Opening {
    fn new(slots: [B256; 9], values: [U256; 9], marker: u64) -> Self {
        use outbe_primitives::test_utils::state_proof::{account_mpt_with_proofs, storage_trie};
        let leaves: Vec<_> = slots
            .iter()
            .map(|word| U256::from_be_bytes(word.0))
            .zip(values)
            .chain([(U256::MAX, U256::from(marker))])
            .collect();
        let (storage_root, storage_nodes) = storage_trie(&leaves);
        let code_hash = B256::repeat_byte(0x91);
        let account = reth_trie::TrieAccount {
            nonce: 1,
            balance: U256::ZERO,
            storage_root,
            code_hash,
        };
        let (root, accounts) = account_mpt_with_proofs(&[(TEE_REGISTRY_ADDRESS, account)]);
        let storage = slots
            .iter()
            .zip(leaves)
            .zip(storage_nodes)
            .map(|((key, (_, value)), nodes)| {
                json!({
                    "key": key,
                    "value": format!("0x{value:x}"),
                    "proof": nodes.iter().map(|node| hex_prefixed(node)).collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let value = json!({
            "nonce": "0x1", "balance": "0x0", "codeHash": code_hash,
            "storageHash": storage_root, "storageProof": storage,
            "accountProof": accounts[&TEE_REGISTRY_ADDRESS].iter().map(|node| hex_prefixed(node)).collect::<Vec<_>>()
        });
        Self { root, slots, value }
    }
}

struct CertifiedHead {
    at: LifecycleCheckpointV1,
    parent: B256,
    extra: Vec<u8>,
    finalization: Vec<u8>,
    block: Vec<u8>,
}

fn certify(
    committee: &finality::FinalityCommitteeFixture,
    header: Header,
    genesis: B256,
) -> CertifiedHead {
    let certified = committee.certify_child_block(
        outbe_consensus::finalized_admission_test_utils::CertifiedChildBlockInput {
            epoch: commonware_consensus::types::Epoch::new(0),
            height: header.number,
            timestamp: header.timestamp,
            state_root: header.state_root,
            extra_data: header.extra_data.to_vec(),
            parent_hash: header.parent_hash,
            base_fee_per_gas: Some(1),
        },
    );
    let block = outbe_consensus::committee_chain::decode_public_block(&certified.block).unwrap();
    let header = block.header();
    CertifiedHead {
        at: checkpoint(header, certified.block_hash, genesis),
        parent: header.parent_hash(),
        extra: header.extra_data().to_vec(),
        finalization: certified.finalization,
        block: certified.block,
    }
}

fn checkpoint(
    header: &impl alloy_consensus::BlockHeader,
    hash: B256,
    genesis: B256,
) -> LifecycleCheckpointV1 {
    LifecycleCheckpointV1 {
        height: header.number(),
        hash,
        state_root: header.state_root(),
        timestamp: header.timestamp(),
        base_fee_per_gas: U256::from(1),
        chain_id: crate::TEST_CHAIN_ID,
        genesis_hash: genesis,
    }
}

struct MovingChain {
    certified: [CertifiedHead; 3],
    openings: [Opening; 3],
    blocks: Vec<Value>,
    log: Value,
    policy: TeePolicyV1,
    binding: NodeEnclaveBindingV1View,
    public_key: [u8; 32],
    head_index: AtomicUsize,
    advance_on_schedule_read: bool,
    advance_on_proof: bool,
}

pub(super) fn block_json(at: LifecycleCheckpointV1, parent: B256, extra: &[u8]) -> Value {
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

pub(super) fn schedule_for(at: LifecycleCheckpointV1) -> TeeRenewalScheduleV1 {
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

fn dispatch(chain: &MovingChain, to: Address, data: &[u8]) -> Result<Vec<u8>, MovingRpcError> {
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
        Some(ITeeRegistryV1::enclaveUpgradeV1Call::abi_encode_returns(
            &ITeeRegistryV1::enclaveUpgradeV1Return {
                proposalId: U256::ZERO,
                activationHeight: 0,
                mrenclave: B256::ZERO,
                successorPolicyHash: B256::ZERO,
                predecessorPolicyHash: B256::ZERO,
            },
        ))
    } else if data.starts_with(&ITeeRegistryV1::pendingEnclaveUpgradeCall::SELECTOR) {
        Some(
            ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_encode_returns(
                &ITeeRegistryV1::pendingEnclaveUpgradeReturn {
                    contextHash: B256::ZERO,
                    validUntil: 0,
                    sourceBindingId: B256::ZERO,
                    targetHash: B256::ZERO,
                    nonce: 0,
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
    encoded.ok_or_else(|| MovingRpcError::UnexpectedCall {
        to,
        selector: alloy_primitives::hex::encode(&data[..data.len().min(4)]),
    })
}

#[derive(Debug, thiserror::Error)]
enum MovingRpcError {
    #[error("unexpected call to {to}, selector {selector}")]
    UnexpectedCall { to: Address, selector: String },
    #[error("unknown proof checkpoint {0}")]
    UnknownBlock(B256),
    #[error("unexpected proof address, slots, or canonicality flag")]
    UnexpectedProof,
    #[error("distance to target block exceeds maximum proof window")]
    OutsideWindow,
    #[error("invalid upgrade context: {0:?}")]
    UpgradeContext(outbe_tee::dcap_protocol::DcapRejectCodeV1),
    #[error("donor could not verify and export the upgrade key")]
    Export(#[source] outbe_tee::TransportError),
}

impl From<MovingRpcError> for ErrorObjectOwned {
    fn from(error: MovingRpcError) -> Self {
        Self::owned(-32000, error.to_string(), None::<()>)
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockSelector {
    block_hash: B256,
    require_canonical: bool,
}

impl MovingChain {
    fn head(&self) -> LifecycleCheckpointV1 {
        self.certified[self.head_index.load(Ordering::SeqCst)].at
    }

    fn advance_head_once(&self) {
        let current = self.head_index.load(Ordering::SeqCst);
        if current + 1 < self.certified.len() {
            let _ = self.head_index.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
        }
    }

    fn header(&self, tag: &str) -> Result<Value, ErrorObjectOwned> {
        let height = if tag == "finalized" {
            self.head().height
        } else {
            u64::from_str_radix(tag.trim_start_matches("0x"), 16).map_err(|error| {
                ErrorObjectOwned::owned(-32000, format!("bad tag {tag}: {error}"), None::<()>)
            })?
        };
        self.blocks.get(height as usize).cloned().ok_or_else(|| {
            ErrorObjectOwned::owned(-32000, format!("missing block {tag}"), None::<()>)
        })
    }

    fn schedule_by_params(&self, args: &[Value]) -> Result<TeeRenewalScheduleV1, ErrorObjectOwned> {
        if self.advance_on_schedule_read {
            self.advance_head_once();
        }
        let target = match args.first() {
            None | Some(Value::Null) => self.head(),
            Some(value) => {
                let hash: B256 = serde_json::from_value(value.clone()).map_err(|error| {
                    ErrorObjectOwned::owned(
                        -32602,
                        format!("invalid checkpoint hash: {error}"),
                        None::<()>,
                    )
                })?;
                let found = self
                    .certified
                    .iter()
                    .find(|entry| entry.at.hash == hash)
                    .map(|entry| entry.at)
                    .ok_or_else(|| {
                        ErrorObjectOwned::owned(
                            -32602,
                            format!("unknown schedule checkpoint {hash}"),
                            None::<()>,
                        )
                    })?;
                if found.height > self.head().height {
                    return Err(ErrorObjectOwned::owned(
                        -32602,
                        format!(
                            "height {} is above the node's finalized height {}",
                            found.height,
                            self.head().height
                        ),
                        None::<()>,
                    ));
                }
                found
            }
        };
        Ok(schedule_for(target))
    }

    fn proof(
        &self,
        address: Address,
        slots: Vec<B256>,
        selector: BlockSelector,
    ) -> Result<Value, MovingRpcError> {
        let i = self
            .certified
            .iter()
            .position(|entry| entry.at.hash == selector.block_hash)
            .ok_or(MovingRpcError::UnknownBlock(selector.block_hash))?;
        let opening = &self.openings[i];
        if address != TEE_REGISTRY_ADDRESS || slots != opening.slots || !selector.require_canonical
        {
            return Err(MovingRpcError::UnexpectedProof);
        }
        if self.certified[i].at.height > self.head().height {
            return Err(MovingRpcError::UnknownBlock(selector.block_hash));
        }
        if self.advance_on_proof && i + 1 < self.certified.len() {
            let _ = self
                .head_index
                .compare_exchange(i, i + 1, Ordering::SeqCst, Ordering::SeqCst);
        }
        if self
            .head()
            .height
            .saturating_sub(self.certified[i].at.height)
            > 128
        {
            return Err(MovingRpcError::OutsideWindow);
        }
        Ok(opening.value.clone())
    }
}

type Calls = Arc<Mutex<Vec<(String, Vec<Value>)>>>;

fn in_process_methods(chain: Arc<MovingChain>) -> (jsonrpsee::Methods, Calls) {
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut module = RpcModule::new(());
    let seen = calls.clone();
    module
        .register_method("eth_chainId", move |_, _, _| {
            seen.lock()
                .unwrap()
                .push(("eth_chainId".into(), Vec::new()));
            Ok::<_, ErrorObjectOwned>(format!("0x{:x}", crate::TEST_CHAIN_ID))
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
            state.header(args[0].as_str().unwrap_or_default())
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
            dispatch(&state, to, &data)
                .map(|encoded| hex_prefixed(&encoded))
                .map_err(ErrorObjectOwned::from)
        })
        .unwrap();
    let state = chain.clone();
    let seen = calls.clone();
    module
        .register_method("outbe_teeRenewalScheduleV1", move |params, _, _| {
            let args: Vec<Value> = match params.as_str() {
                None => Vec::new(),
                Some(_) => params.parse()?,
            };
            seen.lock()
                .unwrap()
                .push(("outbe_teeRenewalScheduleV1".into(), args.clone()));
            let schedule = state.schedule_by_params(&args)?;
            Ok::<_, ErrorObjectOwned>(serde_json::to_value(schedule).unwrap())
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
    let seen = calls.clone();
    module
        .register_method("eth_sendRawTransaction", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock()
                .unwrap()
                .push(("eth_sendRawTransaction".into(), args));
            Ok::<_, ErrorObjectOwned>(json!(B256::repeat_byte(0x59)))
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
            let mut receipt = json!({
                "transactionHash": tx,
                "blockHash": state.certified[0].at.hash,
                "blockNumber": format!("0x{:x}", state.certified[0].at.height),
                "status": "0x1",
                "logs": [state.log.clone()],
            });
            if tx == json!(B256::repeat_byte(0x59)) {
                receipt["transactionHash"] = json!(B256::repeat_byte(0x59));
            }
            Ok::<_, ErrorObjectOwned>(receipt)
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
            let cert = state
                .certified
                .iter()
                .find(|entry| entry.at.height == height)
                .ok_or_else(|| {
                    ErrorObjectOwned::owned(
                        -32000,
                        format!("missing certificate {height}"),
                        None::<()>,
                    )
                })?;
            Ok::<_, ErrorObjectOwned>(json!({
                "finalizationHex": hex_prefixed(&cert.finalization),
                "blockHex": hex_prefixed(&cert.block),
            }))
        })
        .unwrap();
    let state = chain;
    let seen = calls.clone();
    module
        .register_method("eth_getProof", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            seen.lock()
                .unwrap()
                .push(("eth_getProof".into(), args.clone()));
            let (address, slots, selector) = params.parse()?;
            state
                .proof(address, slots, selector)
                .map_err(ErrorObjectOwned::from)
        })
        .unwrap();
    module
        .register_method("outbe_upgradeKeyV1", |params, _, _| {
            let (context, proof, legacy): (Bytes, UpgradeKeyProofV1, bool) = params.parse()?;
            let context = DcapOnboardingContextV1::decode_canonical(&context)
                .map_err(MovingRpcError::UpgradeContext)?;
            outbe_tee::upgrade_transfer::export_from_network_source(context, &proof, legacy)
                .map(Bytes::from)
                .map_err(MovingRpcError::Export)
                .map_err(ErrorObjectOwned::from)
        })
        .unwrap();
    (module.into(), calls)
}

fn block_heights(calls: &Calls) -> Vec<u64> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == "eth_getBlockByNumber")
        .filter_map(|(_, args)| {
            let tag = args.first()?.as_str()?;
            if tag == "finalized" || tag == "latest" || tag == "pending" {
                return None;
            }
            u64::from_str_radix(tag.trim_start_matches("0x"), 16).ok()
        })
        .collect()
}

#[tokio::test]
async fn moving_head_upgrade_proof_and_130_block_rollover_use_incremental_history() {
    use outbe_consensus::finalized_admission_test_utils::FinalityCommitteeFixture;
    outbe_consensus::config::init_consensus_chain_id(crate::TEST_CHAIN_ID).ok();
    let committee = FinalityCommitteeFixture::new(11);
    let descriptor = proof::descriptor(&committee);
    let live = Arc::new(proof::context(9, &descriptor));
    let donor = Arc::new(proof::context(8, &descriptor));
    donor
        .activate_for_test(
            ResidentOfferMaterial::for_test(
                vec![7; 96],
                B256::from(donor.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    let policy =
        no_attestation_dev_policy_v1(crate::TEST_CHAIN_ID, live.manifest().genesis_hash).unwrap();
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
    let context = proof::onboarding_context(&intent, donor.network_encryption_public().unwrap());
    let artifact = donor
        .prepare_no_attestation_dev_onboarding_artifact_v2(context)
        .unwrap()
        .onboarding_artifact;
    let slots = onboarding_registry_slots_v1(&context);
    let openings = [
        Opening::new(
            upgrade_registry_slots_v1(&context),
            upgrade_values(&context),
            1,
        ),
        Opening::new(slots, onboarding_values(&context), 2),
        Opening::new(slots, onboarding_values(&context), 3),
    ];
    let (certified, blocks) = canonical_chain(&committee, context.genesis_hash, &openings);
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
        "blockNumber": format!("0x{:x}", certified[0].at.height),
        "blockHash": certified[0].at.hash,
        "transactionHash": tx,
        "removed": false,
    });
    let chain = Arc::new(MovingChain {
        certified,
        openings,
        blocks,
        log,
        policy,
        binding: (&binding_for(&intent)).into(),
        public_key: context.network_encryption_public,
        head_index: AtomicUsize::new(0),
        advance_on_schedule_read: false,
        advance_on_proof: true,
    });
    let (methods, calls) = in_process_methods(Arc::clone(&chain));
    let dir = tempfile::tempdir().unwrap();
    provision_host_key(dir.path());
    let mut supervisor = outbe_runtime::LifecycleSupervisorProbe::new(
        methods,
        test_signers(),
        (Arc::clone(&live), dir.path().to_owned(), descriptor),
    )
    .unwrap();
    let [h1, h2, far] = chain.certified.each_ref().map(|entry| entry.at);
    let _source = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(donor));
    assert_eq!(chain.head(), h1);
    let response = supervisor.upgrade_key(&context).await.unwrap();
    assert_proved_at(&response.proof, h1);
    assert_eq!(chain.head(), h2);
    calls.lock().unwrap().clear();

    let outcome = supervisor
        .observe_enrollment()
        .await
        .expect("retry within the same tick");
    let Some(EnrollmentOutcome::Admitted { anchor }) = outcome else {
        panic!("expected admission, got {outcome:?}");
    };
    assert_eq!(anchor.finalized_height, far.height);
    assert_eq!(anchor.finalized_hash, far.hash);
    assert!(live.offer_key_ready().unwrap());
    assert_eq!(chain.head(), far);
    let proof_targets: Vec<_> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == "eth_getProof")
        .map(|(_, args)| args[2]["blockHash"].clone())
        .collect();
    assert_eq!(proof_targets, [json!(h2.hash), json!(far.hash)]);
    assert!(far.height - h2.height > 128);
    assert_incremental_history(&calls, h1.height, far.height);
}

#[tokio::test]
async fn moving_head_schedule_stays_pinned_for_validator_and_node_host_reads() {
    use outbe_consensus::finalized_admission_test_utils::FinalityCommitteeFixture;
    outbe_consensus::config::init_consensus_chain_id(crate::TEST_CHAIN_ID).ok();
    let committee = FinalityCommitteeFixture::new(11);
    let descriptor = proof::descriptor(&committee);
    let live = Arc::new(proof::context(9, &descriptor));
    let donor = Arc::new(proof::context(8, &descriptor));
    donor
        .activate_for_test(
            ResidentOfferMaterial::for_test(
                vec![7; 96],
                B256::from(donor.manifest().chain_id),
                0,
                0,
            )
            .unwrap(),
        )
        .unwrap();
    let policy =
        no_attestation_dev_policy_v1(crate::TEST_CHAIN_ID, live.manifest().genesis_hash).unwrap();
    let intent = RegistrationIntentFixture {
        operation: AttestationOperationV1::RegisterEnclave,
        policy_hash: policy.policy_hash().unwrap(),
        binding_id: B256::repeat_byte(0x43),
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 0,
        requested_valid_until: 50_000,
    }
    .for_manifest(live.manifest())
    .unwrap();
    let context = proof::onboarding_context(&intent, donor.network_encryption_public().unwrap());
    let artifact = donor
        .prepare_no_attestation_dev_onboarding_artifact_v2(context)
        .unwrap()
        .onboarding_artifact;
    let slots = onboarding_registry_slots_v1(&context);
    let openings = [
        Opening::new(slots, onboarding_values(&context), 1),
        Opening::new(slots, onboarding_values(&context), 2),
        Opening::new(slots, onboarding_values(&context), 3),
    ];
    let (certified, blocks) =
        canonical_chain_with_heights(&committee, context.genesis_hash, &openings, [2, 3, 4]);
    let tx = B256::repeat_byte(0x5a);
    let event = ITeeRegistryV1::OfferKeySealedForRegistryV1 {
        nodeIdHash: context.node_id_hash,
        sealedOfferKey: Bytes::from(artifact),
    };
    let encoded = event.encode_log_data();
    let log = json!({
        "address": TEE_REGISTRY_ADDRESS,
        "topics": encoded.topics(),
        "data": encoded.data,
        "blockNumber": format!("0x{:x}", certified[0].at.height),
        "blockHash": certified[0].at.hash,
        "transactionHash": tx,
        "removed": false,
    });
    let chain = Arc::new(MovingChain {
        certified,
        openings,
        blocks,
        log,
        policy,
        binding: (&binding_for(&intent)).into(),
        public_key: context.network_encryption_public,
        head_index: AtomicUsize::new(0),
        advance_on_schedule_read: true,
        advance_on_proof: false,
    });
    let (methods, calls) = in_process_methods(Arc::clone(&chain));
    let dir = tempfile::tempdir().unwrap();
    provision_host_key(dir.path());
    let mut supervisor = outbe_runtime::LifecycleSupervisorProbe::new(
        methods,
        test_signers(),
        (Arc::clone(&live), dir.path().to_owned(), descriptor),
    )
    .unwrap();
    let [h1, h2, h3] = chain.certified.each_ref().map(|entry| entry.at);
    assert_eq!(chain.head(), h1);

    let outcome = supervisor
        .observe_enrollment()
        .await
        .expect("pinned Validator and NodeHost schedule reads must succeed while head advances");
    let Some(EnrollmentOutcome::Admitted { anchor }) = outcome else {
        panic!("expected admission at pinned checkpoint H, got {outcome:?}");
    };
    assert_eq!(anchor.finalized_height, h2.height);
    assert_eq!(anchor.finalized_hash, h2.hash);
    assert_eq!(
        chain.head(),
        h3,
        "external finalized head must have advanced past H during observation"
    );

    let recorded = calls.lock().unwrap();
    let schedule_calls: Vec<_> = recorded
        .iter()
        .filter(|(name, _)| name == "outbe_teeRenewalScheduleV1")
        .map(|(_, args)| args.clone())
        .collect();
    assert_eq!(
        schedule_calls,
        vec![
            vec![json!(h1.hash)],
            vec![json!(h2.hash)],
            vec![json!(h2.hash)],
        ],
        "network_view and Validator/NodeHost preflight reads must each request schedule at their immutable pinned hash"
    );

    let expected_tag = json!({"blockHash": h2.hash, "requireCanonical": true});
    let validator_selector = hex_prefixed(&ITeeRegistryV1::validatorEnclaveBindingCall::SELECTOR);
    let node_host_selector = hex_prefixed(&ITeeRegistryV1::nodeHostEnclaveBindingCall::SELECTOR);
    let saw_validator_at_h = recorded.iter().any(|(name, args)| {
        name == "eth_call"
            && args.get(1) == Some(&expected_tag)
            && args
                .first()
                .and_then(|v| v.get("data"))
                .and_then(Value::as_str)
                .is_some_and(|data| data.starts_with(&validator_selector))
    });
    let saw_node_host_at_h = recorded.iter().any(|(name, args)| {
        name == "eth_call"
            && args.get(1) == Some(&expected_tag)
            && args
                .first()
                .and_then(|v| v.get("data"))
                .and_then(Value::as_str)
                .is_some_and(|data| data.starts_with(&node_host_selector))
    });
    assert!(
        saw_validator_at_h,
        "expected Validator binding eth_call at H"
    );
    assert!(
        saw_node_host_at_h,
        "expected NodeHost binding eth_call at H"
    );
}

fn assert_proved_at(proof: &UpgradeKeyProofV1, expected: LifecycleCheckpointV1) {
    let witness = FinalizedAdmissionWitnessV1::decode_canonical(&proof.admission).unwrap();
    let header: Header =
        alloy_consensus::private::alloy_rlp::decode_exact(&witness.admission.header).unwrap();
    assert_eq!(header.number, expected.height);
    assert_eq!(header.hash_slow(), expected.hash);
    assert_eq!(header.state_root, expected.state_root);
    assert_eq!(header.timestamp, expected.timestamp);
}

fn assert_incremental_history(calls: &Calls, h1: u64, far: u64) {
    let heights = block_heights(calls);
    assert!(
        !heights.contains(&1),
        "admission history rescanned from genesis"
    );
    assert!(
        heights
            .iter()
            .filter(|height| **height > 0 && **height <= h1)
            .count()
            <= 3
    );
    assert!(heights.contains(&(h1 + 1)));
    assert!(heights.contains(&far));
    let delta_reads = heights.iter().filter(|height| **height > h1).count();
    assert!(delta_reads <= (far - h1) as usize * 3);
}

fn canonical_chain(
    committee: &finality::FinalityCommitteeFixture,
    genesis: B256,
    openings: &[Opening; 3],
) -> ([CertifiedHead; 3], Vec<Value>) {
    canonical_chain_with_heights(committee, genesis, openings, [2, 3, 132])
}

fn canonical_chain_with_heights(
    committee: &finality::FinalityCommitteeFixture,
    genesis: B256,
    openings: &[Opening; 3],
    heights: [u64; 3],
) -> ([CertifiedHead; 3], Vec<Value>) {
    let base = Header {
        base_fee_per_gas: Some(1),
        ..Default::default()
    };
    let mut blocks = vec![block_json(
        checkpoint(&base, genesis, genesis),
        B256::ZERO,
        &[],
    )];
    let mut certified = Vec::new();
    let mut parent = genesis;
    let mut state_root = B256::ZERO;
    for number in 1..=heights[2] {
        let index = heights.iter().position(|&height| height == number);
        if let Some(i) = index {
            state_root = openings[i].root;
        }
        let header = Header {
            number,
            timestamp: 1000 + number,
            parent_hash: parent,
            state_root,
            extra_data: if number == heights[0] {
                boundary_extra(committee).into()
            } else {
                Bytes::new()
            },
            ..base.clone()
        };
        if index.is_some() {
            let entry = certify(committee, header, genesis);
            blocks.push(block_json(entry.at, entry.parent, &entry.extra));
            parent = entry.at.hash;
            certified.push(entry);
        } else {
            let hash = header.hash_slow();
            blocks.push(block_json(
                checkpoint(&header, hash, genesis),
                parent,
                &header.extra_data,
            ));
            parent = hash;
        }
    }
    (certified.try_into().ok().unwrap(), blocks)
}
