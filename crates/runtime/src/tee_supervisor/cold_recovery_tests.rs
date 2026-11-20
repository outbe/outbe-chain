//! Cold recovery: a fresh candidate, a new admission attempt, and an atomic publish.
use super::super::boundary_artifact::boundary_extra;
use super::super::founding_http::{
    bind_manifest, devnet_resident, registry_module, scripted, RpcCalls,
};
use super::tests::{admit_successor, source_for_promotion};
use super::*;
use crate::admission::TeeLeaseGuardGateV1;
use crate::admission_authority::live::{LiveTeeError, LiveTeeRuntimeV1, LiveTeeSnapshotV1};
use crate::admission_authority::test_support as support;
use alloy_consensus::transaction::RlpEcdsaDecodableTx as _;
use alloy_consensus::{BlockHeader as _, TxEip1559};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolValue};
use jsonrpsee::{server::ServerBuilder, types::ErrorObjectOwned, RpcModule};
use outbe_consensus::finalized_admission_test_utils::{
    CertifiedChildBlockInput, FinalityCommitteeFixture,
};
use outbe_primitives::tee_attestation_v1::*;
use outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1;
use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
use outbe_primitives::tee_test_utils::no_attestation_dev_policy_v1;
use outbe_primitives::{addresses::TEE_REGISTRY_ADDRESS, addresses::VALIDATOR_SET_ADDRESS};
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;
use outbe_tee::finalized_admission::upgrade_registry_slots_v1;
use outbe_tee::lifecycle::enrollment::{
    load_live_join_admission_anchor_v1, persist_live_join_admission_anchor_v1, LiveJoinAnchorError,
};
use outbe_tee::lifecycle::successor::SuccessorError;
use outbe_tee::lifecycle::LifecycleCheckpointV1;
use outbe_tee::node_host::{
    NodeHostIdentityKey, NODE_HOST_DIRECTORY_V1, NODE_HOST_IDENTITY_KEY_V1,
};
use outbe_tee::registry_view::RenewalBindingV1;
use outbe_tee::TeeAuthorityChannel;
use outbe_validatorset::precompile::IValidatorSet;
use serde_json::{json, Value};
use std::os::unix::fs::DirBuilderExt as _;
use std::sync::{Arc, Mutex};

fn operator() -> (Arc<outbe_evm::OutbeEvmSigner>, k256::ecdsa::SigningKey) {
    (
        Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([0x47; 32]).unwrap()),
        k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap(),
    )
}

fn host_tree() -> (tempfile::TempDir, NodeHostIdentityKey) {
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join(NODE_HOST_DIRECTORY_V1);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&host)
        .unwrap();
    let key = NodeHostIdentityKey::create_new(&host.join(NODE_HOST_IDENTITY_KEY_V1)).unwrap();
    (root, key)
}

fn armed_owner(context: &Arc<InProcessTeeContext>) -> LiveTeeRuntimeV1 {
    let (_, _, identity) = support::provider(context, 7, 1_800_000_000, 1_800_001_000, false);
    LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity,
            gate: TeeLeaseGuardGateV1::Armed,
            context: context.clone(),
        },
        TeeAuthorityChannel::default(),
    )
    .unwrap()
}

fn rpc_error(message: impl Into<String>) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32000, message.into(), None::<()>)
}

fn hex_prefixed(bytes: &[u8]) -> String {
    format!("0x{}", alloy_primitives::hex::encode(bytes))
}

fn hex_bytes(text: &str) -> Vec<u8> {
    alloy_primitives::hex::decode(text.trim_start_matches("0x")).unwrap_or_default()
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

fn plain_extra() -> Vec<u8> {
    use outbe_primitives::reshare_artifact::{encode_outbe_block_artifacts, OutbeBlockArtifacts};
    encode_outbe_block_artifacts(&OutbeBlockArtifacts::default())
        .unwrap()
        .to_vec()
}

fn certify(
    committee: &FinalityCommitteeFixture,
    parent: B256,
    child: (u64, u64, B256),
    extra: Vec<u8>,
) -> (LifecycleCheckpointV1, Vec<u8>, Vec<u8>) {
    let (height, timestamp, state_root) = child;
    let certified = committee.certify_child_block(CertifiedChildBlockInput {
        epoch: commonware_consensus::types::Epoch::new(0),
        height,
        timestamp,
        state_root,
        extra_data: extra,
        parent_hash: parent,
        base_fee_per_gas: Some(1),
    });
    let header = outbe_consensus::committee_chain::decode_public_block(&certified.block)
        .unwrap()
        .header()
        .clone();
    (
        LifecycleCheckpointV1 {
            height: header.number(),
            hash: certified.block_hash,
            state_root: header.state_root(),
            timestamp: header.timestamp(),
            base_fee_per_gas: U256::from(header.base_fee_per_gas().expect("fee sealed")),
            chain_id: 0,
            genesis_hash: B256::ZERO,
        },
        certified.finalization,
        certified.block,
    )
}

fn opening_for(context: &DcapOnboardingContextV1, source: B256, nonce: u64) -> (B256, Value) {
    use outbe_primitives::test_utils::state_proof::{account_mpt_with_proofs, storage_trie};
    let slots = upgrade_registry_slots_v1(context);
    let source_word = U256::from_be_bytes(source.0);
    let values = [
        U256::from_be_bytes(context.network_encryption_public),
        U256::from(context.key_epoch),
        U256::from(context.network_encryption_epoch),
        U256::from_be_bytes(context.context_hash().0),
        U256::from(u64::MAX / 4),
        source_word,
        source_word,
        U256::from_be_bytes(context.policy_hash.0),
        U256::from(nonce.max(1)),
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
    let nodes = |proof: &[Bytes]| {
        proof
            .iter()
            .map(|node| hex_prefixed(node))
            .collect::<Vec<_>>()
    };
    let storage_proof = slots.into_iter().zip(values).zip(openings).map(|((key, value), proof)| {
        json!({"key": key, "value": format!("0x{value:x}"), "proof": nodes(&proof)})
    }).collect::<Vec<_>>();
    let opening = json!({
        "nonce": "0x1", "balance": "0x0", "codeHash": code_hash, "storageHash": storage_root,
        "accountProof": nodes(&accounts[&TEE_REGISTRY_ADDRESS]),
        "storageProof": storage_proof,
    });
    (state_root, opening)
}

#[derive(Clone)]
struct ProofHead {
    at: LifecycleCheckpointV1,
    parent: B256,
    extra: Vec<u8>,
    finalization: Vec<u8>,
    block: Vec<u8>,
    opening: Value,
    artifact: Bytes,
    intent_hash: B256,
}

struct ColdChain {
    genesis: B256,
    h1_hash: B256,
    h2: LifecycleCheckpointV1,
    h2_extra: Vec<u8>,
    h2_proof: (Vec<u8>, Vec<u8>),
    served: LifecycleCheckpointV1,
    served_parent: B256,
    served_extra: Vec<u8>,
    past: Vec<ProofHead>,
    staged: Option<ProofHead>,
    binding: RenewalBindingV1,
    pending_context: B256,
    pending_until: u64,
    pending_target: B256,
    pending_nonce: u64,
    account_finalized: u64,
    account_latest: u64,
    account_pending: u64,
    policy: Bytes,
    predecessor: B256,
    successor_hash: B256,
    public_key: [u8; 32],
    donor: Arc<InProcessTeeContext>,
    committee: FinalityCommitteeFixture,
    upgrade_intents: Vec<B256>,
    proof_hashes: Vec<B256>,
    cancels: usize,
    transition: Option<RegistrationIntentV1>,
}

impl ColdChain {
    fn open(
        donor: Arc<InProcessTeeContext>,
        committee: FinalityCommitteeFixture,
        binding: RenewalBindingV1,
        policy: TeePolicyV1,
        predecessor: B256,
    ) -> Self {
        let genesis = donor.manifest().genesis_hash;
        let chain_id = u64::try_from(U256::from_be_bytes(donor.manifest().chain_id)).unwrap();
        outbe_consensus::proof::init_consensus_chain_id(chain_id).unwrap();
        let h1_hash = B256::repeat_byte(0x91);
        let extra = boundary_extra(&committee);
        let (mut h2, finalization, block) = certify(
            &committee,
            h1_hash,
            (2, 1_000, B256::repeat_byte(0x44)),
            extra.clone(),
        );
        h2.chain_id = chain_id;
        h2.genesis_hash = genesis;
        Self {
            genesis,
            h1_hash,
            served: h2,
            served_parent: h1_hash,
            served_extra: extra.clone(),
            h2,
            h2_extra: extra,
            h2_proof: (finalization, block),
            past: Vec::new(),
            staged: None,
            binding,
            pending_context: B256::ZERO,
            pending_until: 0,
            pending_target: B256::ZERO,
            pending_nonce: 0,
            account_finalized: 0,
            account_latest: 0,
            account_pending: 0,
            successor_hash: policy.policy_hash().unwrap(),
            policy: Bytes::from(policy.encode_canonical().unwrap()),
            predecessor,
            public_key: donor.network_encryption_public().unwrap(),
            donor,
            committee,
            upgrade_intents: Vec::new(),
            proof_hashes: Vec::new(),
            cancels: 0,
            transition: None,
        }
    }

    fn arm(&mut self) {
        let proof = self.staged.take().expect("staged proof");
        self.served_parent = self.served.hash;
        self.served = proof.at;
        self.served_extra = proof.extra.clone();
        self.past.push(proof.clone());
        self.staged = Some(proof);
    }

    fn note_tx(&mut self, raw: &[u8]) {
        let mut bytes = &raw[1..];
        let signed = TxEip1559::rlp_decode_signed(&mut bytes).expect("service tx");
        let input = signed.tx().input.clone();
        if input.starts_with(&ITeeRegistryV1::prepareEnclaveUpgradeCall::SELECTOR) {
            let call = ITeeRegistryV1::prepareEnclaveUpgradeCall::abi_decode(&input).unwrap();
            self.stage_prepare(
                AttestationEvidenceV1::decode_canonical(&call.evidence)
                    .unwrap()
                    .intent()
                    .clone(),
            );
        } else if input.starts_with(&ITeeRegistryV1::cancelEnclaveUpgradeCall::SELECTOR) {
            self.pending_context = B256::ZERO;
            self.cancels += 1;
        } else if input.starts_with(&ITeeRegistryV1::transitionEnclaveMeasurementCall::SELECTOR) {
            let call =
                ITeeRegistryV1::transitionEnclaveMeasurementCall::abi_decode(&input).unwrap();
            self.transition = Some(
                AttestationEvidenceV1::decode_canonical(&call.evidence)
                    .unwrap()
                    .intent()
                    .clone(),
            );
        }
    }

    fn stage_prepare(&mut self, intent: RegistrationIntentV1) {
        let context = DcapOnboardingContextV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            intent_hash: intent.intent_hash().unwrap(),
            node_id_hash: intent.node_id.node_id_hash().unwrap(),
            enclave_id: intent.enclave_id,
            binding_id: intent.binding_id,
            policy_hash: intent.policy_hash,
            recipient_x25519: intent.recipient_x25519,
            network_encryption_public: self.public_key,
            key_epoch: 0,
            network_encryption_epoch: 0,
        };
        let (state_root, opening) =
            opening_for(&context, self.binding.binding_id, intent.transition_nonce);
        let parent = self.served;
        let extra = plain_extra();
        let (mut at, finalization, block) = certify(
            &self.committee,
            parent.hash,
            (
                parent.height + 1,
                parent.timestamp.saturating_add(1),
                state_root,
            ),
            extra.clone(),
        );
        at.chain_id = parent.chain_id;
        at.genesis_hash = parent.genesis_hash;
        let artifact = Bytes::from(
            self.donor
                .prepare_no_attestation_dev_onboarding_artifact_v2(context)
                .unwrap()
                .onboarding_artifact,
        );
        self.pending_context = context.context_hash();
        self.pending_until = intent.requested_valid_until;
        self.pending_target = intent.upgrade_target_hash().unwrap();
        self.pending_nonce = intent.transition_nonce;
        self.proof_hashes.push(at.hash);
        self.staged = Some(ProofHead {
            at,
            parent: parent.hash,
            extra,
            finalization,
            block,
            opening,
            artifact,
            intent_hash: context.intent_hash,
        });
    }

    fn advance_without_proof(&mut self) {
        let prior = self.served;
        let extra = plain_extra();
        let (mut at, finalization, block) = certify(
            &self.committee,
            prior.hash,
            (prior.height + 1, prior.timestamp + 1, prior.state_root),
            extra.clone(),
        );
        at.chain_id = prior.chain_id;
        at.genesis_hash = prior.genesis_hash;
        self.staged = Some(ProofHead {
            at,
            parent: prior.hash,
            extra,
            finalization,
            block,
            opening: Value::Null,
            artifact: Bytes::new(),
            intent_hash: B256::ZERO,
        });
        self.arm();
    }

    fn promote_transition(&mut self) {
        let intent = self.transition.clone().expect("transition intent");
        self.binding.node_id_hash = intent.node_id.node_id_hash().unwrap();
        self.binding.enclave_id = intent.enclave_id;
        self.binding.binding_id = intent.binding_id;
        self.binding.binding_version = intent.binding_version;
        self.binding.registration_version = intent.registration_version;
        self.binding.renewal_nonce = intent.renewal_nonce;
        self.binding.transition_nonce = intent.transition_nonce;
        self.binding.valid_until = intent.requested_valid_until;
        self.binding.policy_hash = intent.policy_hash;
        self.binding.recipient_x25519 = B256::from(intent.recipient_x25519);
        self.pending_context = B256::ZERO;
        self.advance_without_proof();
    }

    fn dispatch(&self, to: Address, data: &[u8]) -> Result<Vec<u8>, String> {
        let pending = ITeeRegistryV1::pendingEnclaveUpgradeReturn {
            contextHash: self.pending_context,
            validUntil: self.pending_until,
            sourceBindingId: self.binding.binding_id,
            targetHash: self.pending_target,
            nonce: self.pending_nonce,
        };
        let upgrade = ITeeRegistryV1::enclaveUpgradeV1Return {
            proposalId: U256::from(1),
            activationHeight: 1,
            mrenclave: B256::ZERO,
            successorPolicyHash: self.successor_hash,
            predecessorPolicyHash: self.predecessor,
        };
        let encoded = if data.starts_with(&ITeeRegistryV1::activePolicyV1Call::SELECTOR) {
            Some(ITeeRegistryV1::activePolicyV1Call::abi_encode_returns(
                &self.policy,
            ))
        } else if data.starts_with(&ITeeRegistryV1::stagedSuccessorPolicyV1Call::SELECTOR) {
            Some((false, U256::ZERO, Bytes::new()).abi_encode_params())
        } else if data.starts_with(&ITeeRegistryV1::validatorEnclaveBindingCall::SELECTOR)
            || data.starts_with(&ITeeRegistryV1::nodeHostEnclaveBindingCall::SELECTOR)
        {
            let view = outbe_primitives::tee_registry_abi_v1::NodeEnclaveBindingV1View::from(
                &self.binding,
            );
            Some(ITeeRegistryV1::validatorEnclaveBindingCall::abi_encode_returns(&view))
        } else if data.starts_with(&ITeeRegistryV1::tributeOfferPublicKeyCall::SELECTOR) {
            Some(
                ITeeRegistryV1::tributeOfferPublicKeyCall::abi_encode_returns(
                    &U256::from_be_bytes(self.public_key),
                ),
            )
        } else if data.starts_with(&ITeeRegistryV1::enclaveUpgradeV1Call::SELECTOR) {
            Some(ITeeRegistryV1::enclaveUpgradeV1Call::abi_encode_returns(
                &upgrade,
            ))
        } else if data.starts_with(&ITeeRegistryV1::pendingEnclaveUpgradeCall::SELECTOR) {
            Some(ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_encode_returns(&pending))
        } else if data.starts_with(&ITeeRegistryV1::keyEpochCall::SELECTOR)
            || data.starts_with(&ITeeRegistryV1::tributeOfferEpochCall::SELECTOR)
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
                "unexpected call {}",
                alloy_primitives::hex::encode(&data[..data.len().min(4)])
            )
        })
    }

    fn block_value(&self, tag: &str) -> Result<Value, ErrorObjectOwned> {
        if tag == "0x0" {
            return Ok(json!({
                "number": "0x0", "hash": self.genesis, "parentHash": B256::ZERO,
                "stateRoot": B256::ZERO, "timestamp": "0x0", "baseFeePerGas": "0x1", "extraData": "0x",
            }));
        }
        if tag == "finalized" {
            return Ok(block_json(
                self.served,
                self.served_parent,
                &self.served_extra,
            ));
        }
        if tag == "0x1" {
            return Ok(block_json(
                LifecycleCheckpointV1 {
                    height: 1,
                    hash: self.h1_hash,
                    state_root: B256::repeat_byte(0x92),
                    timestamp: self.h2.timestamp.saturating_sub(1),
                    ..self.h2
                },
                self.genesis,
                &[],
            ));
        }
        if tag == "0x2" {
            return Ok(block_json(self.h2, self.h1_hash, &self.h2_extra));
        }
        let proof = self
            .past
            .iter()
            .find(|proof| tag == format!("0x{:x}", proof.at.height))
            .ok_or_else(|| rpc_error(format!("missing block {tag}")))?;
        Ok(block_json(proof.at, proof.parent, &proof.extra))
    }

    fn finality_value(&self, height: u64) -> Result<Value, ErrorObjectOwned> {
        let (finalization, block) = if height == self.h2.height {
            self.h2_proof.clone()
        } else if let Some(proof) = self.past.iter().find(|proof| proof.at.height == height) {
            (proof.finalization.clone(), proof.block.clone())
        } else if self
            .staged
            .as_ref()
            .is_some_and(|proof| proof.at.height == height)
        {
            let proof = self.staged.as_ref().unwrap();
            (proof.finalization.clone(), proof.block.clone())
        } else {
            return Err(rpc_error(format!("missing proof {height}")));
        };
        Ok(
            json!({"finalizationHex": hex_prefixed(&finalization), "blockHex": hex_prefixed(&block)}),
        )
    }

    fn upgrade_value(&mut self, raw: &[u8]) -> Result<Value, ErrorObjectOwned> {
        let context = DcapOnboardingContextV1::decode_canonical(raw)
            .map_err(|error| rpc_error(format!("{error:?}")))?;
        self.upgrade_intents.push(context.intent_hash);
        let proof = self
            .staged
            .as_ref()
            .ok_or_else(|| rpc_error("upgrade artifact is not staged"))?;
        if proof.intent_hash != context.intent_hash {
            return Err(rpc_error("upgrade artifact is for a different intent"));
        }
        Ok(serde_json::to_value(proof.artifact.clone()).unwrap())
    }

    fn account_nonce(&self, tag: &Value) -> u64 {
        if tag == "pending" {
            self.account_pending
        } else if tag == "latest" {
            self.account_latest
        } else {
            self.account_finalized
        }
    }

    fn rpc(&mut self, method: &str, args: &[Value]) -> Result<Value, ErrorObjectOwned> {
        match method {
            "eth_chainId" => Ok(json!(format!("0x{:x}", self.served.chain_id))),
            "eth_getBalance" => Ok(json!("0xffffffffffffffffffffffffffffffff")),
            "eth_getTransactionReceipt" => Ok(Value::Null),
            "outbe_teeRenewalScheduleV1" => {
                Ok(serde_json::to_value(schedule_for(self.served)).unwrap())
            }
            "eth_getTransactionCount" => Ok(json!(format!(
                "0x{:x}",
                self.account_nonce(args.get(1).unwrap_or(&Value::Null))
            ))),
            "eth_sendRawTransaction" => {
                let raw = hex_bytes(args.first().and_then(Value::as_str).unwrap_or("0x"));
                self.note_tx(&raw);
                Ok(json!(B256::from(alloy_primitives::keccak256(&raw))))
            }
            "eth_getProof" => self
                .staged
                .as_ref()
                .map(|proof| proof.opening.clone())
                .ok_or_else(|| rpc_error("proof is not staged")),
            "outbe_upgradeKeyV1" => self.upgrade_value(&hex_bytes(
                args.first().and_then(Value::as_str).unwrap_or("0x"),
            )),
            "outbe_getFinalityProof" => self.finality_value(args[0].as_u64().unwrap_or(0)),
            "eth_getBlockByNumber" => self.block_value(args[0].as_str().unwrap_or_default()),
            "eth_call" => {
                let to: Address = args[0]["to"]
                    .as_str()
                    .unwrap_or_default()
                    .parse()
                    .map_err(|error| rpc_error(format!("{error}")))?;
                let data = hex_bytes(args[0]["data"].as_str().unwrap_or("0x"));
                self.dispatch(to, &data)
                    .map(|encoded| Value::String(hex_prefixed(&encoded)))
                    .map_err(rpc_error)
            }
            other => Err(rpc_error(other)),
        }
    }
}

async fn serve(chain: Arc<Mutex<ColdChain>>) -> (String, jsonrpsee::server::ServerHandle) {
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let mut module = RpcModule::new(());
    for name in [
        "eth_chainId",
        "eth_getBlockByNumber",
        "eth_call",
        "outbe_teeRenewalScheduleV1",
        "eth_getBalance",
        "eth_getTransactionCount",
        "eth_sendRawTransaction",
        "eth_getTransactionReceipt",
        "outbe_getFinalityProof",
        "eth_getProof",
        "outbe_upgradeKeyV1",
    ] {
        let state = chain.clone();
        module
            .register_method(name, move |params, _, _| {
                let args: Vec<Value> = params.parse().unwrap_or_default();
                state.lock().unwrap().rpc(name, &args)
            })
            .unwrap();
    }
    (format!("http://{address}"), server.start(module))
}

#[tokio::test]
async fn expired_own_binding_prepares_a_fresh_enclave_before_policy_rejection() {
    let (live, policy, genesis) = devnet_resident();
    let calls = RpcCalls::default();
    let script = scripted(policy.clone(), genesis);
    {
        let mut row = script.lock().unwrap();
        row.binding = bind_manifest(live.manifest(), 100, policy.policy_hash().unwrap());
        row.staged = Some(policy);
        assert_eq!(row.binding.enclaveId, live.manifest().enclave_id().unwrap());
        assert!(row.binding.validUntil < row.schedule.finalized_timestamp);
    }
    let root = tempfile::tempdir().unwrap();
    let descriptor = live_descriptor(&live);
    let mut supervisor = TeeLifecycleSupervisor::new_in_process(
        registry_module(script, calls.clone()).into(),
        operator(),
        (live, root.path().to_owned(), descriptor),
    )
    .unwrap();
    let error = supervisor.advance_successor().await.unwrap_err();
    assert!(
        matches!(
            error,
            SuccessorRuntimeError::Successor(SuccessorError::PolicyMismatch)
        ),
        "same enclave id would be CandidateMismatch, got {error:?}"
    );
    assert!(supervisor.successor.is_none());
    assert_eq!(
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == "eth_sendRawTransaction")
            .count(),
        0
    );
}

#[tokio::test]
async fn cancel_required_retries_cold_prepare_and_verifies_the_second_intent() {
    let (root, host_key) = host_tree();
    let committee = FinalityCommitteeFixture::new(4);
    let (live, descriptor) = source_for_promotion(&host_key, &committee);
    let manifest = live.manifest();
    let chain_id = u64::try_from(U256::from_be_bytes(manifest.chain_id)).unwrap();
    let predecessor = no_attestation_dev_policy_v1(chain_id, manifest.genesis_hash).unwrap();
    let predecessor_hash = predecessor.policy_hash().unwrap();
    let mut policy = predecessor;
    policy.policy_version += 1;
    policy.predecessor_policy_hash = predecessor_hash;
    policy.activation_height = 1;
    let mut binding = RenewalBindingV1 {
        node_id_hash: manifest.node_id.node_id_hash().unwrap(),
        enclave_id: B256::repeat_byte(0x51),
        binding_id: B256::repeat_byte(0x52),
        intent_hash: B256::repeat_byte(0x53),
        evidence_hash: B256::repeat_byte(0x54),
        policy_hash: predecessor_hash,
        binding_version: 1,
        registration_version: 1,
        renewal_nonce: 0,
        transition_nonce: 0,
        lease_started_at: 1,
        valid_until: 50_000,
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
    assert_ne!(binding.enclave_id, manifest.enclave_id().unwrap());
    let chain = Arc::new(Mutex::new(ColdChain::open(
        live.clone(),
        committee,
        binding.clone(),
        policy,
        predecessor_hash,
    )));
    let (url, handle) = serve(chain.clone()).await;
    let mut supervisor = TeeLifecycleSupervisor::new(
        &url,
        operator(),
        (live.clone(), root.path().to_owned(), descriptor),
    )
    .unwrap();
    let owner = armed_owner(&live);
    let watch = owner.authority.subscribe();
    let (source_provider, network, _) = support::provider(&live, 1, 999, 1_800_001_000, false);
    let protocol = crate::OutbeRuntime::new();
    assert!(owner
        .refresh(&source_provider, &protocol, &network)
        .unwrap()
        .is_authority_open());
    let _dispatch =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(live.clone()));
    assert!(
        matches!(watch.snapshot(), outbe_tee::TeeAuthorityState::Armed { enclave_id, .. } if enclave_id == live.manifest().enclave_id().unwrap())
    );
    supervisor.advance_successor().await.expect("cold prepare");
    let first = supervisor.successor.as_ref().unwrap();
    let first_enclave = first.candidate.manifest().enclave_id().unwrap();
    let first_intent = first.attempt.prepare_intent.intent_hash().unwrap();
    let first_nonce = first.attempt.prepare_intent.transition_nonce;
    assert!(
        first.cold
            && !first.candidate.offer_key_ready().unwrap()
            && cold_recovery_marked(root.path())
    );
    chain.lock().unwrap().arm();
    let first_prepare_height = chain.lock().unwrap().served.height;
    supervisor
        .advance_successor()
        .await
        .expect("provision first candidate");
    assert!(supervisor
        .successor
        .as_ref()
        .unwrap()
        .candidate
        .offer_key_ready()
        .unwrap());
    {
        let mut row = chain.lock().unwrap();
        row.pending_context = B256::repeat_byte(0x99);
        row.account_finalized = 4;
        row.account_latest = 4;
        row.account_pending = 4;
    }
    supervisor
        .advance_successor()
        .await
        .expect("upgrade cancel");
    assert!(supervisor.successor.is_none() && chain.lock().unwrap().cancels == 1);
    assert!(cold_recovery_marked(root.path()));
    {
        let mut row = chain.lock().unwrap();
        row.account_pending = 5;
        row.advance_without_proof();
        assert!(row.served.height > first_prepare_height);
    }
    supervisor
        .advance_successor()
        .await
        .expect("second cold prepare");
    let second = supervisor.successor.as_ref().unwrap();
    let second_enclave = second.candidate.manifest().enclave_id().unwrap();
    let second_intent = second.attempt.prepare_intent.intent_hash().unwrap();
    let second_nonce = second.attempt.prepare_intent.transition_nonce;
    assert!(
        second.cold
            && second_enclave != first_enclave
            && second_intent != first_intent
            && second_nonce != first_nonce
    );
    assert!(!second.candidate.offer_key_ready().unwrap());
    let candidate = second.candidate.clone();
    chain.lock().unwrap().arm();
    supervisor
        .advance_successor()
        .await
        .expect("verify second onboarding proof");
    assert!(candidate.offer_key_ready().unwrap());
    {
        let row = chain.lock().unwrap();
        assert_eq!(row.upgrade_intents, vec![first_intent, second_intent]);
        assert_eq!(
            candidate.admission_anchor().unwrap().unwrap().block_hash,
            *row.proof_hashes.last().unwrap()
        );
        assert_ne!(row.proof_hashes[0], row.proof_hashes[1]);
    }
    let activation = {
        let mut row = chain.lock().unwrap();
        row.promote_transition();
        binding = row.binding.clone();
        row.served
    };
    assert_eq!(binding.enclave_id, second_enclave);
    supervisor.advance_successor().await.expect("finalize");
    let done = supervisor.successor.as_ref().unwrap();
    assert_eq!(done.activation, Some(activation));
    assert!(done.cold);
    let proof = candidate.admission_anchor().unwrap().unwrap();
    let mut admitted_provider =
        activation_provider(&chain.lock().unwrap(), &candidate, source_provider);
    let anchor = supervisor
        .complete_cold_recovery(Some(&owner))
        .expect("cold recovery");
    assert_eq!(anchor.finalized_height, activation.height);
    assert_eq!(anchor.finalized_hash, activation.hash);
    assert!(Arc::ptr_eq(&supervisor.live, &candidate));
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &candidate
    ));
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &candidate));
    let snapshot = owner.lock().unwrap().clone();
    assert_eq!(snapshot.identity.expected_enclave_id, Some(second_enclave));
    assert_eq!(snapshot.gate, TeeLeaseGuardGateV1::Pending(anchor));
    assert_eq!(
        watch.snapshot(),
        outbe_tee::TeeAuthorityState::SuspendedPolicyMismatch
    );
    // A published owner may still be ahead of local replay. The guard cannot
    // open until the local canonical marker is the finalized activation.
    admitted_provider.finalized = Some(alloy_eips::BlockNumHash::new(
        activation.height,
        activation.hash,
    ));
    owner
        .refresh(&admitted_provider, &protocol, &network)
        .unwrap();
    assert_eq!(
        watch.snapshot(),
        outbe_tee::TeeAuthorityState::Armed {
            enclave_id: second_enclave,
            valid_until: binding.valid_until,
        }
    );
    assert_eq!(owner.lock().unwrap().gate, TeeLeaseGuardGateV1::Armed);

    assert!(!cold_recovery_marked(root.path()) && supervisor.successor.is_none());
    let persisted = load_live_join_admission_anchor_v1(root.path(), &candidate)
        .unwrap()
        .expect("anchor.bin preserved");
    assert_eq!(persisted.intent_hash, second_intent);
    assert_eq!(persisted.finalized_hash, proof.block_hash);
    assert!(anchor.finalized_height > proof.block_number);
    assert!(root
        .path()
        .join("tee-live-join-admission-v1/anchor.bin")
        .is_file());
    handle.stop().unwrap();
}

fn activation_provider(
    served: &ColdChain,
    candidate: &InProcessTeeContext,
    source: crate::launch_tests::admission_provider::FinalizedMockProvider,
) -> crate::launch_tests::admission_provider::FinalizedMockProvider {
    let activation = served.served;
    let (mut provider, _, _) = support::provider(
        candidate,
        activation.height,
        activation.timestamp,
        served.binding.valid_until,
        false,
    );
    let mut headers = provider.inner.headers.lock();
    headers.retain(|_, header| header.number() == 0);
    for record in &served.past {
        let block = outbe_consensus::committee_chain::decode_public_block(&record.block).unwrap();
        headers.insert(record.at.hash, block.header().clone());
        if record.at.height < activation.height {
            provider
                .historical_states
                .insert(record.at.hash, source.inner.clone());
        }
    }
    drop(headers);
    provider
}

#[path = "cold_recovery_publish.rs"]
mod publish;

fn live_descriptor(live: &InProcessTeeContext) -> TrustedNetworkDescriptorV1 {
    TrustedNetworkDescriptorV1 {
        network_binding: live.manifest().network_binding(),
        genesis_consensus_keys: vec![],
    }
}
