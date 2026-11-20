//! Proof checkpoint, history cache, and registerEnclave receipt filtering.
use super::super::super::boundary_artifact::boundary_extra;
use super::*;
use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::SolEvent;
use jsonrpsee::{server::ServerBuilder, types::ErrorObjectOwned, RpcModule};
use outbe_primitives::{addresses::TEE_REGISTRY_ADDRESS, tee_registry_abi_v1::ITeeRegistryV1};
use outbe_tee::dcap_protocol::{DcapOnboardingArtifactV1, DcapOnboardingContextV1};
use std::collections::HashMap;

fn older_admission(number: u64, state_root: B256, timestamp: u64) -> (UpgradeKeyProofV1, B256) {
    let header = alloy_consensus::Header {
        number,
        state_root,
        timestamp,
        ..Default::default()
    };
    let hash = header.hash_slow();
    (super::bound_upgrade_proof(&header), hash)
}

fn context_for(at: LifecycleCheckpointV1) -> DcapOnboardingContextV1 {
    DcapOnboardingContextV1 {
        chain_id: U256::from(at.chain_id).to_be_bytes(),
        genesis_hash: at.genesis_hash,
        intent_hash: B256::repeat_byte(0x44),
        node_id_hash: B256::repeat_byte(0x55),
        enclave_id: B256::repeat_byte(0x56),
        binding_id: B256::repeat_byte(0x57),
        policy_hash: B256::repeat_byte(0x58),
        recipient_x25519: [9; 32],
        network_encryption_public: [8; 32],
        key_epoch: 0,
        network_encryption_epoch: 0,
    }
}

#[tokio::test]
async fn upgrade_key_accepts_an_older_canonical_checkpoint() {
    let head = LifecycleCheckpointV1 {
        height: 3,
        hash: B256::repeat_byte(3),
        ..checkpoint()
    };
    let (proof, proved_hash) = older_admission(2, B256::repeat_byte(9), 11);
    let context = context_for(head);
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce: [1; 12],
        ciphertext: vec![2; 16],
    };
    let encoded = artifact.encode_canonical().unwrap();
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    module
        .register_method("eth_chainId", |_, context, _| {
            context
                .lock()
                .unwrap()
                .push(("eth_chainId".into(), Vec::new()));
            Ok::<_, ErrorObjectOwned>("0x7")
        })
        .unwrap();
    module
        .register_method("eth_getBlockByNumber", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context
                .lock()
                .unwrap()
                .push(("eth_getBlockByNumber".into(), args.clone()));
            let tag = args[0].as_str().unwrap_or_default();
            let header = if tag == "0x0" {
                let mut genesis = block(head);
                genesis["number"] = json!("0x0");
                genesis["hash"] = json!(head.genesis_hash);
                genesis
            } else if tag == "0x2" {
                json!({
                    "number": "0x2",
                    "hash": proved_hash,
                    "stateRoot": B256::repeat_byte(9),
                    "timestamp": "0xb",
                    "baseFeePerGas": "0x1",
                    "parentHash": head.genesis_hash,
                })
            } else {
                block(head)
            };
            Ok::<_, ErrorObjectOwned>(header)
        })
        .unwrap();
    let body = Bytes::from(encoded);
    module
        .register_method("outbe_upgradeKeyV1", move |_, _, _| {
            Ok::<_, ErrorObjectOwned>(body.clone())
        })
        .unwrap();
    let handle = server.start(module);
    let port = RpcLifecyclePort::new(
        &format!("http://{address}"),
        head.chain_id,
        head.genesis_hash,
    )
    .unwrap();
    let imported = port.upgrade_key(head, &context, &proof).await.unwrap();
    let decoded = DcapOnboardingArtifactV1::decode_canonical(&imported.artifact).unwrap();
    assert_eq!(decoded.context, context);
    assert!(counts(&calls, "eth_getBlockByNumber")
        .iter()
        .any(|args| args[0] == "0x2"));
    handle.stop().unwrap();
}

struct FinalityBlock {
    hash: B256,
    parent: B256,
    state: B256,
    extra: Vec<u8>,
    finalization: Vec<u8>,
    block: Vec<u8>,
}

fn hex_prefixed(bytes: &[u8]) -> String {
    format!("0x{}", alloy_primitives::hex::encode(bytes))
}

async fn finality_server(
    rows: HashMap<u64, FinalityBlock>,
) -> (RpcLifecyclePort, jsonrpsee::server::ServerHandle, Calls) {
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let calls = Calls::default();
    let mut module = RpcModule::new((calls.clone(), rows));
    module
        .register_method("eth_getBlockByNumber", |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context
                .0
                .lock()
                .unwrap()
                .push(("eth_getBlockByNumber".into(), args.clone()));
            let tag = args[0].as_str().unwrap_or_default();
            let height = u64::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap_or(0);
            let row = context
                .1
                .get(&height)
                .ok_or_else(|| ErrorObjectOwned::owned(-32000, "missing block", None::<()>))?;
            Ok::<_, ErrorObjectOwned>(json!({
                "number": format!("0x{height:x}"),
                "hash": row.hash,
                "parentHash": row.parent,
                "stateRoot": row.state,
                "timestamp": "0x3e8",
                "baseFeePerGas": "0x1",
                "extraData": hex_prefixed(&row.extra),
            }))
        })
        .unwrap();
    module
        .register_method("outbe_getFinalityProof", |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context
                .0
                .lock()
                .unwrap()
                .push(("outbe_getFinalityProof".into(), args.clone()));
            let height = args[0].as_u64().unwrap_or(0);
            let row = context
                .1
                .get(&height)
                .ok_or_else(|| ErrorObjectOwned::owned(-32000, "missing proof", None::<()>))?;
            Ok::<_, ErrorObjectOwned>(json!({
                "finalizationHex": hex_prefixed(&row.finalization),
                "blockHex": hex_prefixed(&row.block),
            }))
        })
        .unwrap();
    let handle = server.start(module);
    let genesis = B256::repeat_byte(0x11);
    (
        RpcLifecyclePort::new(&format!("http://{address}"), 676, genesis).unwrap(),
        handle,
        calls,
    )
}

fn row_from(
    certified: &outbe_consensus::finalized_admission_test_utils::CertifiedBlockFixture,
    parent: B256,
    state: B256,
    extra: Vec<u8>,
) -> FinalityBlock {
    FinalityBlock {
        hash: certified.block_hash,
        parent,
        state,
        extra,
        finalization: certified.finalization.clone(),
        block: certified.block.clone(),
    }
}

fn descriptor_for(
    committee: &outbe_consensus::finalized_admission_test_utils::FinalityCommitteeFixture,
) -> outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1 {
    use outbe_primitives::tee_attestation_v1::*;
    TrustedNetworkDescriptorV1 {
        network_binding: NetworkBindingV1 {
            chain_id: U256::from(676_u64).to_be_bytes(),
            genesis_hash: B256::repeat_byte(0x11),
            attestation_mode: AttestationMode::NoAttestationDev,
        },
        genesis_consensus_keys: committee.public_keys_min_pk(),
    }
}

struct RegistryOpening {
    state_root: B256,
    account: outbe_tee::finalized_admission::MptAccountProofV1,
    storage: Vec<outbe_tee::finalized_admission::MptStorageProofV1>,
}

fn registry_opening(context: &DcapOnboardingContextV1) -> RegistryOpening {
    use outbe_primitives::{
        addresses::TEE_REGISTRY_ADDRESS,
        test_utils::state_proof::{account_mpt_with_proofs, storage_trie},
    };
    use outbe_tee::finalized_admission::{
        onboarding_registry_slots_v1, MptAccountProofV1, MptStorageProofV1,
    };
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
    let account = reth_trie::TrieAccount {
        nonce: 1,
        balance: U256::ZERO,
        storage_root,
        code_hash: B256::repeat_byte(0x91),
    };
    let (state_root, accounts) = account_mpt_with_proofs(&[(TEE_REGISTRY_ADDRESS, account)]);
    RegistryOpening {
        state_root,
        account: MptAccountProofV1 {
            nonce: account.nonce,
            balance: account.balance,
            code_hash: account.code_hash,
            storage_root,
            nodes: accounts[&TEE_REGISTRY_ADDRESS]
                .iter()
                .map(|node| node.to_vec())
                .collect(),
        },
        storage: slots
            .into_iter()
            .zip(values)
            .zip(openings)
            .map(|((key, value), nodes)| MptStorageProofV1 {
                key,
                value,
                nodes: nodes.into_iter().map(|node| node.to_vec()).collect(),
            })
            .collect(),
    }
}

fn encode_witness(
    admission: outbe_tee::finalized_admission::CertifiedHeaderV1,
    opening: &RegistryOpening,
) -> Vec<u8> {
    use outbe_tee::finalized_admission::FinalizedAdmissionWitnessV1;
    FinalizedAdmissionWitnessV1 {
        admission,
        registry_account: opening.account.clone(),
        registry_storage: opening.storage.clone(),
    }
    .encode_canonical()
    .unwrap()
}

#[tokio::test]
async fn epoch_transition_is_deferred_at_the_boundary_and_a_bad_signature_is_not_cached() {
    use outbe_consensus::finalized_admission_test_utils::{
        CertifiedChildBlockInput, FinalityCommitteeFixture,
    };
    use outbe_tee::in_process::admission::FinalizedAdmissionVerifierV1;
    outbe_consensus::proof::init_consensus_chain_id(676).unwrap();
    let committee = FinalityCommitteeFixture::new(21);
    let epoch0 = commonware_consensus::types::Epoch::new(0);
    let epoch1 = commonware_consensus::types::Epoch::new(1);
    let context = DcapOnboardingContextV1 {
        chain_id: U256::from(676_u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x11),
        intent_hash: B256::repeat_byte(0x44),
        node_id_hash: B256::repeat_byte(0x55),
        enclave_id: B256::repeat_byte(0x56),
        binding_id: B256::repeat_byte(0x57),
        policy_hash: B256::repeat_byte(0x58),
        recipient_x25519: [9; 32],
        network_encryption_public: [8; 32],
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    let opening = registry_opening(&context);
    let state1 = B256::repeat_byte(0x21);
    let extra1 = boundary_extra(&committee);
    let first = committee.certify_block(epoch0, 1, 1_000, state1, extra1.clone());
    let extra2 = committee.preannounce_extra_data(epoch1);
    let second = committee.certify_child_block(CertifiedChildBlockInput {
        epoch: epoch0,
        height: 2,
        timestamp: 1_001,
        state_root: opening.state_root,
        extra_data: extra2.clone(),
        parent_hash: first.block_hash,
        base_fee_per_gas: None,
    });
    let third = committee.certify_child_block(CertifiedChildBlockInput {
        epoch: epoch1,
        height: 3,
        timestamp: 1_002,
        state_root: opening.state_root,
        extra_data: Vec::new(),
        parent_hash: second.block_hash,
        base_fee_per_gas: None,
    });
    let foreign = FinalityCommitteeFixture::new(99);
    let broken = foreign.certify_child_block(CertifiedChildBlockInput {
        epoch: epoch1,
        height: 4,
        timestamp: 1_003,
        state_root: opening.state_root,
        extra_data: Vec::new(),
        parent_hash: third.block_hash,
        base_fee_per_gas: None,
    });
    let mut rows = HashMap::new();
    rows.insert(1, row_from(&first, B256::ZERO, state1, extra1));
    rows.insert(
        2,
        row_from(&second, first.block_hash, opening.state_root, extra2),
    );
    rows.insert(
        3,
        row_from(&third, second.block_hash, opening.state_root, Vec::new()),
    );
    rows.insert(
        4,
        row_from(&broken, third.block_hash, opening.state_root, Vec::new()),
    );
    let (mut port, handle, calls) = finality_server(rows).await;
    let descriptor = descriptor_for(&committee);
    port.bind_network(descriptor.clone());
    let at = LifecycleCheckpointV1 {
        height: 2,
        hash: second.block_hash,
        state_root: opening.state_root,
        timestamp: 1_001,
        base_fee_per_gas: U256::from(1),
        chain_id: 676,
        genesis_hash: B256::repeat_byte(0x11),
    };
    let opened = port.admission_history(at).await.unwrap();
    let (anchor, transitions, admission) = opened.finish().unwrap();
    assert!(transitions.is_empty());
    FinalizedAdmissionVerifierV1::new(&descriptor, &context, &anchor)
        .unwrap()
        .verify_admission(&encode_witness(admission, &opening))
        .unwrap();
    calls.lock().unwrap().clear();
    let ahead = LifecycleCheckpointV1 {
        height: 3,
        hash: third.block_hash,
        state_root: opening.state_root,
        timestamp: 1_002,
        ..at
    };
    let resumed = port.admission_history(ahead).await.unwrap();
    let (anchor, transitions, admission) = resumed.finish().unwrap();
    assert_eq!(transitions.len(), 1);
    let blocks = counts(&calls, "eth_getBlockByNumber");
    assert!(blocks.iter().all(|args| {
        let tag = args[0].as_str().unwrap_or_default();
        tag != "0x1" && tag != "0x2"
    }));
    assert!(blocks.iter().any(|args| args[0] == "0x3"));
    let proofs = counts(&calls, "outbe_getFinalityProof");
    assert!(proofs.iter().all(|args| args[0].as_u64() == Some(3)));
    let mut verifier = FinalizedAdmissionVerifierV1::new(&descriptor, &context, &anchor).unwrap();
    verifier.advance_committee(&transitions[0]).unwrap();
    verifier
        .verify_admission(&encode_witness(admission, &opening))
        .unwrap();
    calls.lock().unwrap().clear();
    let bad = LifecycleCheckpointV1 {
        height: 4,
        hash: broken.block_hash,
        timestamp: 1_003,
        ..ahead
    };
    assert!(matches!(
        port.admission_history(bad).await,
        Err(RpcLifecycleError::Unauthenticated(..))
    ));
    let during = counts(&calls, "eth_getBlockByNumber");
    assert!(during.iter().all(|args| {
        let tag = args[0].as_str().unwrap_or_default();
        tag != "0x1" && tag != "0x2" && tag != "0x3"
    }));
    assert!(during.iter().any(|args| args[0] == "0x4"));
    calls.lock().unwrap().clear();
    assert!(matches!(
        port.admission_history(bad).await,
        Err(RpcLifecycleError::Unauthenticated(..))
    ));
    assert!(counts(&calls, "eth_getBlockByNumber")
        .iter()
        .any(|args| args[0] == "0x1"));
    handle.stop().unwrap();
}

struct WitnessParts<'a> {
    descriptor: &'a outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1,
    context: &'a DcapOnboardingContextV1,
    opening: &'a RegistryOpening,
}

fn prove_witness(
    parts: &WitnessParts<'_>,
    anchor: &[u8],
    transitions: &[Bytes],
    admission: outbe_tee::finalized_admission::CertifiedHeaderV1,
) {
    use outbe_tee::in_process::admission::FinalizedAdmissionVerifierV1;
    let mut verifier =
        FinalizedAdmissionVerifierV1::new(parts.descriptor, parts.context, anchor).unwrap();
    for transition in transitions {
        verifier.advance_committee(transition).unwrap();
    }
    verifier
        .verify_admission(&encode_witness(admission, parts.opening))
        .unwrap();
}

fn only_new_height(calls: &Calls, height: u64) -> bool {
    let tag = format!("0x{height:x}");
    let blocks = counts(calls, "eth_getBlockByNumber");
    let proofs = counts(calls, "outbe_getFinalityProof");
    !blocks.is_empty()
        && blocks.iter().all(|args| args[0] == tag)
        && !proofs.is_empty()
        && proofs.iter().all(|args| args[0].as_u64() == Some(height))
}

#[tokio::test]
async fn outgoing_committee_still_signs_the_block_after_preannounce() {
    use outbe_consensus::finalized_admission_test_utils::{
        CertifiedChildBlockInput, FinalityCommitteeFixture,
    };
    outbe_consensus::proof::init_consensus_chain_id(676).unwrap();
    let committee = FinalityCommitteeFixture::new(41);
    let epoch0 = commonware_consensus::types::Epoch::new(0);
    let epoch1 = commonware_consensus::types::Epoch::new(1);
    let context = DcapOnboardingContextV1 {
        chain_id: U256::from(676_u64).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x11),
        intent_hash: B256::repeat_byte(0x44),
        node_id_hash: B256::repeat_byte(0x55),
        enclave_id: B256::repeat_byte(0x56),
        binding_id: B256::repeat_byte(0x57),
        policy_hash: B256::repeat_byte(0x58),
        recipient_x25519: [9; 32],
        network_encryption_public: [8; 32],
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    let opening = registry_opening(&context);
    let state1 = B256::repeat_byte(0x21);
    let extra1 = boundary_extra(&committee);
    let anchor = committee.certify_block(epoch0, 1, 1_000, state1, extra1.clone());
    let extra_pre = committee.preannounce_extra_data(epoch1);
    let announced = committee.certify_child_block(CertifiedChildBlockInput {
        epoch: epoch0,
        height: 2,
        timestamp: 1_001,
        state_root: opening.state_root,
        extra_data: extra_pre.clone(),
        parent_hash: anchor.block_hash,
        base_fee_per_gas: None,
    });
    let still_outgoing = committee.certify_child_block(CertifiedChildBlockInput {
        epoch: epoch0,
        height: 3,
        timestamp: 1_002,
        state_root: opening.state_root,
        extra_data: Vec::new(),
        parent_hash: announced.block_hash,
        base_fee_per_gas: None,
    });
    let activated = committee.certify_child_block(CertifiedChildBlockInput {
        epoch: epoch1,
        height: 4,
        timestamp: 1_003,
        state_root: opening.state_root,
        extra_data: Vec::new(),
        parent_hash: still_outgoing.block_hash,
        base_fee_per_gas: None,
    });
    let mut rows = HashMap::new();
    rows.insert(1, row_from(&anchor, B256::ZERO, state1, extra1));
    rows.insert(
        2,
        row_from(&announced, anchor.block_hash, opening.state_root, extra_pre),
    );
    rows.insert(
        3,
        row_from(
            &still_outgoing,
            announced.block_hash,
            opening.state_root,
            Vec::new(),
        ),
    );
    rows.insert(
        4,
        row_from(
            &activated,
            still_outgoing.block_hash,
            opening.state_root,
            Vec::new(),
        ),
    );
    let (mut port, handle, calls) = finality_server(rows).await;
    let descriptor = descriptor_for(&committee);
    port.bind_network(descriptor.clone());
    let parts = WitnessParts {
        descriptor: &descriptor,
        context: &context,
        opening: &opening,
    };
    let at = LifecycleCheckpointV1 {
        height: 2,
        hash: announced.block_hash,
        state_root: opening.state_root,
        timestamp: 1_001,
        base_fee_per_gas: U256::from(1),
        chain_id: 676,
        genesis_hash: B256::repeat_byte(0x11),
    };
    let (anchor_bytes, transitions, admission) =
        port.admission_history(at).await.unwrap().finish().unwrap();
    assert!(transitions.is_empty());
    prove_witness(&parts, &anchor_bytes, &transitions, admission);
    calls.lock().unwrap().clear();
    let held = LifecycleCheckpointV1 {
        height: 3,
        hash: still_outgoing.block_hash,
        timestamp: 1_002,
        ..at
    };
    let (anchor_bytes, transitions, admission) = port
        .admission_history(held)
        .await
        .unwrap()
        .finish()
        .unwrap();
    assert!(transitions.is_empty());
    prove_witness(&parts, &anchor_bytes, &transitions, admission);
    assert!(only_new_height(&calls, 3));
    calls.lock().unwrap().clear();
    let switched = LifecycleCheckpointV1 {
        height: 4,
        hash: activated.block_hash,
        timestamp: 1_003,
        ..held
    };
    let (anchor_bytes, transitions, admission) = port
        .admission_history(switched)
        .await
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(transitions.len(), 1);
    prove_witness(&parts, &anchor_bytes, &transitions, admission);
    assert!(only_new_height(&calls, 4));
    handle.stop().unwrap();
}

#[tokio::test]
async fn unauthenticated_certificate_is_not_pinned() {
    use outbe_consensus::finalized_admission_test_utils::FinalityCommitteeFixture;
    outbe_consensus::proof::init_consensus_chain_id(676).unwrap();
    let trusted = FinalityCommitteeFixture::new(31);
    let foreign = FinalityCommitteeFixture::new(32);
    let state = B256::repeat_byte(0x41);
    let extra = boundary_extra(&trusted);
    let signed = foreign.certify_block(
        commonware_consensus::types::Epoch::new(0),
        1,
        1_000,
        state,
        extra.clone(),
    );
    let mut rows = HashMap::new();
    rows.insert(1, row_from(&signed, B256::ZERO, state, extra));
    let (mut port, handle, calls) = finality_server(rows).await;
    port.bind_network(descriptor_for(&trusted));
    let at = LifecycleCheckpointV1 {
        height: 1,
        hash: signed.block_hash,
        state_root: state,
        timestamp: 1_000,
        base_fee_per_gas: U256::from(1),
        chain_id: 676,
        genesis_hash: B256::repeat_byte(0x11),
    };
    assert!(matches!(
        port.admission_history(at).await,
        Err(RpcLifecycleError::Unauthenticated(..))
    ));
    calls.lock().unwrap().clear();
    assert!(matches!(
        port.admission_history(at).await,
        Err(RpcLifecycleError::Unauthenticated(..))
    ));
    assert!(counts(&calls, "eth_getBlockByNumber")
        .iter()
        .any(|args| args[0] == "0x1"));
    handle.stop().unwrap();
}

fn sealed_log(at: LifecycleCheckpointV1, tx: B256, encoded: alloy_primitives::LogData) -> Value {
    json!({
        "address": TEE_REGISTRY_ADDRESS,
        "topics": encoded.topics(),
        "data": encoded.data,
        "blockNumber": format!("0x{:x}", at.height),
        "blockHash": at.hash,
        "transactionHash": tx,
        "removed": false,
    })
}

#[tokio::test]
async fn known_receipt_skips_foreign_logs_and_does_not_scan() {
    let at = checkpoint();
    let context = context_for(at);
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce: [3; 12],
        ciphertext: vec![4; 16],
    };
    let sealed = ITeeRegistryV1::OfferKeySealedForRegistryV1 {
        nodeIdHash: context.node_id_hash,
        sealedOfferKey: artifact.encode_canonical().unwrap().into(),
    }
    .encode_log_data();
    let registered = ITeeRegistryV1::EnclaveRegisteredV1 {
        nodeIdHash: context.node_id_hash,
        enclaveId: context.enclave_id,
        bindingId: context.binding_id,
        validUntil: 90,
        bindingVersion: 1,
    }
    .encode_log_data();
    let tx = B256::repeat_byte(0x59);
    let (port, handle, calls) = receipt_port(
        at,
        tx,
        vec![sealed_log(at, tx, registered), sealed_log(at, tx, sealed)],
    )
    .await;
    assert_eq!(port.send_raw_transaction(&[0xab]).await.unwrap(), tx);
    let error = port.onboarding(at, context.intent_hash).await.unwrap_err();
    assert!(
        !matches!(
            error,
            RpcLifecycleError::InvalidProof(ProofFailure::InvalidReceipt)
        ),
        "bound receipt log must pass validation before assemble, got {error}"
    );
    assert!(counts(&calls, "eth_getLogs").is_empty());
    assert_eq!(counts(&calls, "eth_getProof").len(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn finalized_receipt_remembers_success_for_onboarding_without_send() {
    let at = checkpoint();
    let context = context_for(at);
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce: [3; 12],
        ciphertext: vec![4; 16],
    };
    let sealed = ITeeRegistryV1::OfferKeySealedForRegistryV1 {
        nodeIdHash: context.node_id_hash,
        sealedOfferKey: artifact.encode_canonical().unwrap().into(),
    }
    .encode_log_data();
    let registered = ITeeRegistryV1::EnclaveRegisteredV1 {
        nodeIdHash: context.node_id_hash,
        enclaveId: context.enclave_id,
        bindingId: context.binding_id,
        validUntil: 90,
        bindingVersion: 1,
    }
    .encode_log_data();
    let tx = B256::repeat_byte(0x59);
    let (port, handle, calls) = receipt_port(
        at,
        tx,
        vec![sealed_log(at, tx, registered), sealed_log(at, tx, sealed)],
    )
    .await;
    assert!(
        port.finalized_receipt(at, tx)
            .await
            .unwrap()
            .unwrap()
            .success
    );
    let error = port.onboarding(at, context.intent_hash).await.unwrap_err();
    assert!(
        !matches!(
            error,
            RpcLifecycleError::InvalidProof(ProofFailure::InvalidReceipt)
        ),
        "bound receipt log must pass validation before assemble, got {error}"
    );
    assert!(counts(&calls, "eth_sendRawTransaction").is_empty());
    assert!(counts(&calls, "eth_getLogs").is_empty());
    handle.stop().unwrap();
}

#[tokio::test]
async fn receipt_log_with_foreign_hash_is_rejected() {
    let at = checkpoint();
    let context = context_for(at);
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce: [3; 12],
        ciphertext: vec![4; 16],
    };
    let sealed = ITeeRegistryV1::OfferKeySealedForRegistryV1 {
        nodeIdHash: context.node_id_hash,
        sealedOfferKey: artifact.encode_canonical().unwrap().into(),
    }
    .encode_log_data();
    let tx = B256::repeat_byte(0x59);
    let mut foreign = at;
    foreign.hash = B256::repeat_byte(0x9a);
    let (port, handle, calls) = receipt_port(
        at,
        tx,
        vec![sealed_log(foreign, B256::repeat_byte(0xab), sealed)],
    )
    .await;
    assert_eq!(port.send_raw_transaction(&[0xab]).await.unwrap(), tx);
    assert!(matches!(
        port.onboarding(at, context.intent_hash).await,
        Err(RpcLifecycleError::InvalidProof(
            ProofFailure::InvalidReceipt
        ))
    ));
    assert!(counts(&calls, "eth_getLogs").is_empty());
    assert!(counts(&calls, "eth_getProof").is_empty());
    handle.stop().unwrap();
}

async fn receipt_port(
    at: LifecycleCheckpointV1,
    tx: B256,
    logs: Vec<Value>,
) -> (RpcLifecyclePort, jsonrpsee::server::ServerHandle, Calls) {
    let receipt = json!({
        "transactionHash": tx,
        "blockHash": at.hash,
        "blockNumber": format!("0x{:x}", at.height),
        "status": "0x1",
        "logs": logs,
    });
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    register_chain_head(&mut module, at);
    module
        .register_method("eth_sendRawTransaction", move |_, context, _| {
            context
                .lock()
                .unwrap()
                .push(("eth_sendRawTransaction".into(), Vec::new()));
            Ok::<_, ErrorObjectOwned>(tx)
        })
        .unwrap();
    module
        .register_method("eth_getTransactionReceipt", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context
                .lock()
                .unwrap()
                .push(("eth_getTransactionReceipt".into(), args));
            Ok::<_, ErrorObjectOwned>(receipt.clone())
        })
        .unwrap();
    module
        .register_method("eth_getLogs", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context.lock().unwrap().push(("eth_getLogs".into(), args));
            Ok::<_, ErrorObjectOwned>(json!([]))
        })
        .unwrap();
    module
        .register_method("eth_getProof", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context.lock().unwrap().push(("eth_getProof".into(), args));
            Ok::<_, ErrorObjectOwned>(json!({}))
        })
        .unwrap();
    let handle = server.start(module);
    let port =
        RpcLifecyclePort::new(&format!("http://{address}"), at.chain_id, at.genesis_hash).unwrap();
    (port, handle, calls)
}

fn chain_checkpoint(height: u64, hash: B256, state: B256, timestamp: u64) -> LifecycleCheckpointV1 {
    LifecycleCheckpointV1 {
        height,
        hash,
        state_root: state,
        timestamp,
        base_fee_per_gas: U256::from(1),
        chain_id: 676,
        genesis_hash: B256::repeat_byte(0x11),
    }
}

async fn serve_epoch_chain(
    seed: u8,
    plan: impl FnOnce(
        &outbe_consensus::finalized_admission_test_utils::FinalityCommitteeFixture,
    ) -> Vec<(commonware_consensus::types::Epoch, Vec<u8>)>,
) -> (
    RpcLifecyclePort,
    jsonrpsee::server::ServerHandle,
    outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1,
    DcapOnboardingContextV1,
    RegistryOpening,
    Vec<B256>,
) {
    use outbe_consensus::finalized_admission_test_utils::{
        CertifiedChildBlockInput, FinalityCommitteeFixture,
    };
    outbe_consensus::proof::init_consensus_chain_id(676).unwrap();
    let committee = FinalityCommitteeFixture::new(u64::from(seed));
    let context = context_for(chain_checkpoint(1, B256::ZERO, B256::ZERO, 1_000));
    let opening = registry_opening(&context);
    let state1 = B256::repeat_byte(0x21);
    let anchor_extra = boundary_extra(&committee);
    let anchor = committee.certify_block(
        commonware_consensus::types::Epoch::new(0),
        1,
        1_000,
        state1,
        anchor_extra.clone(),
    );
    let mut rows = HashMap::new();
    rows.insert(1, row_from(&anchor, B256::ZERO, state1, anchor_extra));
    let mut parent = anchor.block_hash;
    let mut hashes = vec![parent];
    for (offset, (epoch, extra)) in plan(&committee).into_iter().enumerate() {
        let height = offset as u64 + 2;
        let block = committee.certify_child_block(CertifiedChildBlockInput {
            epoch,
            height,
            timestamp: 999 + height,
            state_root: opening.state_root,
            extra_data: extra.clone(),
            parent_hash: parent,
            base_fee_per_gas: None,
        });
        rows.insert(height, row_from(&block, parent, opening.state_root, extra));
        parent = block.block_hash;
        hashes.push(parent);
    }
    let (mut port, handle, _) = finality_server(rows).await;
    let descriptor = descriptor_for(&committee);
    port.bind_network(descriptor.clone());
    (port, handle, descriptor, context, opening, hashes)
}

async fn admit(
    port: &RpcLifecyclePort,
    parts: &WitnessParts<'_>,
    height: u64,
    hash: B256,
    timestamp: u64,
) -> Vec<Bytes> {
    let at = chain_checkpoint(height, hash, parts.opening.state_root, timestamp);
    let (anchor, transitions, admission) =
        port.admission_history(at).await.unwrap().finish().unwrap();
    prove_witness(parts, &anchor, &transitions, admission);
    transitions
}

#[tokio::test]
async fn cold_request_at_the_next_height_keeps_the_outgoing_committee() {
    let epoch0 = commonware_consensus::types::Epoch::new(0);
    let (port, handle, descriptor, context, opening, hashes) = serve_epoch_chain(51, |committee| {
        vec![
            (
                epoch0,
                committee.preannounce_extra_data(commonware_consensus::types::Epoch::new(1)),
            ),
            (epoch0, Vec::new()),
        ]
    })
    .await;
    let parts = WitnessParts {
        descriptor: &descriptor,
        context: &context,
        opening: &opening,
    };
    assert!(admit(&port, &parts, 3, hashes[2], 1_002).await.is_empty());
    handle.stop().unwrap();
}

#[tokio::test]
async fn cached_preannounce_jumps_two_epochs() {
    let epoch0 = commonware_consensus::types::Epoch::new(0);
    let epoch1 = commonware_consensus::types::Epoch::new(1);
    let epoch2 = commonware_consensus::types::Epoch::new(2);
    let (port, handle, descriptor, context, opening, hashes) = serve_epoch_chain(52, |committee| {
        vec![
            (epoch0, committee.preannounce_extra_data(epoch1)),
            (epoch1, committee.preannounce_extra_data(epoch2)),
            (epoch2, Vec::new()),
        ]
    })
    .await;
    let parts = WitnessParts {
        descriptor: &descriptor,
        context: &context,
        opening: &opening,
    };
    assert!(admit(&port, &parts, 2, hashes[1], 1_001).await.is_empty());
    assert_eq!(admit(&port, &parts, 4, hashes[3], 1_003).await.len(), 2);
    handle.stop().unwrap();
}
