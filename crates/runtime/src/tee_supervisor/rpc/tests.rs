use super::*;
use jsonrpsee::{server::ServerBuilder, types::ErrorObjectOwned, RpcModule};
use outbe_primitives::tee_registry_abi_v1::ITeeRegistryV1;
use std::sync::{Arc, Mutex};

type Calls = Arc<Mutex<Vec<(String, Vec<Value>)>>>;
fn checkpoint() -> LifecycleCheckpointV1 {
    LifecycleCheckpointV1 {
        height: 2,
        hash: B256::repeat_byte(2),
        state_root: B256::repeat_byte(9),
        timestamp: 20,
        base_fee_per_gas: U256::from(3),
        chain_id: 7,
        genesis_hash: B256::repeat_byte(1),
    }
}
fn block(at: LifecycleCheckpointV1) -> Value {
    json!({"number":format!("0x{:x}",at.height),"hash":at.hash,"stateRoot":at.state_root,"timestamp":format!("0x{:x}",at.timestamp),"baseFeePerGas":format!("0x{:x}",at.base_fee_per_gas),"parentHash":at.genesis_hash})
}
fn bound_upgrade_proof(header: &alloy_consensus::Header) -> UpgradeKeyProofV1 {
    use alloy_consensus::private::alloy_rlp;
    use outbe_tee::finalized_admission::{
        CertifiedHeaderV1, FinalizedAdmissionWitnessV1, MptAccountProofV1, MptStorageProofV1,
    };
    let witness = FinalizedAdmissionWitnessV1 {
        admission: CertifiedHeaderV1 {
            finalization: vec![1],
            header: alloy_rlp::encode(header),
            descendants: vec![],
        },
        registry_account: MptAccountProofV1 {
            nonce: 0,
            balance: U256::ZERO,
            code_hash: B256::ZERO,
            storage_root: B256::ZERO,
            nodes: vec![],
        },
        registry_storage: (0..9)
            .map(|index| MptStorageProofV1 {
                key: B256::from(U256::from(index).to_be_bytes::<32>()),
                value: U256::ZERO,
                nodes: vec![],
            })
            .collect(),
    };
    UpgradeKeyProofV1 {
        anchor_outcome: vec![1].into(),
        committee_transitions: vec![],
        admission: witness.encode_canonical().unwrap().into(),
    }
}

fn register_chain_head(module: &mut RpcModule<Calls>, at: LifecycleCheckpointV1) {
    module
        .register_method("eth_chainId", |_, _, _| "0x7")
        .unwrap();
    module
        .register_method("eth_getBlockByNumber", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context
                .lock()
                .unwrap()
                .push(("eth_getBlockByNumber".into(), args.clone()));
            let mut value = block(at);
            if args[0] == "0x0" {
                value["number"] = json!("0x0");
                value["hash"] = json!(at.genesis_hash);
            }
            Ok::<_, ErrorObjectOwned>(value)
        })
        .unwrap();
}
async fn server(
    overrides: Vec<(&'static str, Value)>,
) -> (RpcLifecyclePort, jsonrpsee::server::ServerHandle, Calls) {
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    let at = checkpoint();
    module
        .register_method("eth_chainId", |_, _, _| "0x7")
        .unwrap();
    module
        .register_method("eth_getBlockByNumber", move |params, _, _| {
            let args: Vec<Value> = params.parse()?;
            let mut value = block(at);
            if args[0] == "0x0" {
                value["hash"] = json!(at.genesis_hash);
                value["number"] = json!("0x0");
            }
            if args[0] == "0x1" {
                value["hash"] = json!(at.genesis_hash);
                value["number"] = json!("0x1");
            }
            Ok::<_, ErrorObjectOwned>(value)
        })
        .unwrap();
    for (name, value) in overrides {
        module
            .register_method(name, move |params, context, _| {
                let args: Vec<Value> = match params.as_str() {
                    None => Vec::new(),
                    Some(_) => params.parse()?,
                };
                context.lock().unwrap().push((name.to_owned(), args));
                Ok::<_, ErrorObjectOwned>(value.clone())
            })
            .unwrap();
    }
    let handle = server.start(module);
    (
        RpcLifecyclePort::new(&format!("http://{address}"), at.chain_id, at.genesis_hash).unwrap(),
        handle,
        calls,
    )
}
#[tokio::test]
async fn cancelled_candidate_keeps_nonce_and_uses_canonical_hash() {
    let pending = ITeeRegistryV1::pendingEnclaveUpgradeReturn {
        contextHash: B256::ZERO,
        validUntil: 0,
        sourceBindingId: B256::ZERO,
        targetHash: B256::ZERO,
        nonce: 19,
    };
    let encoded = ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_encode_returns(&pending);
    let (port, handle, calls) = server(vec![("eth_call", json!(Bytes::from(encoded)))]).await;
    let public = alloy_primitives::hex::decode(
        "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
    )
    .unwrap()
    .try_into()
    .unwrap();
    assert_eq!(
        port.pending_upgrade_nonce(checkpoint(), &NodeBindingSelectorV1::NodeHost(public))
            .await
            .unwrap(),
        19
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].1[1],
        json!({"blockHash":checkpoint().hash,"requireCanonical":true})
    );
    handle.stop().unwrap();
}
#[tokio::test]
async fn pinned_balance_and_nonce_reject_wrong_checkpoint_before_state_read() {
    let (port, handle, calls) = server(vec![
        ("eth_getBalance", json!("0x2a")),
        ("eth_getTransactionCount", json!("0x5")),
    ])
    .await;
    let account = Address::repeat_byte(4);
    assert_eq!(
        port.balance(checkpoint(), account).await.unwrap(),
        U256::from(42)
    );
    assert_eq!(
        port.account_nonce(account, LifecycleNonceTagV1::Finalized(checkpoint()))
            .await
            .unwrap(),
        5
    );
    let mut wrong = checkpoint();
    wrong.hash = B256::repeat_byte(8);
    assert!(matches!(
        port.balance(wrong, account).await,
        Err(LifecyclePortError::Source(
            RpcLifecycleError::CheckpointMismatch
        ))
    ));
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|(_, args)| args[1] == RpcLifecyclePort::tag(checkpoint())));
    handle.stop().unwrap();
}
#[tokio::test]
async fn receipt_requires_canonical_block_and_matching_transaction() {
    let tx = B256::repeat_byte(5);
    for (receipt_hash, tx_hash, status, valid) in [
        (checkpoint().genesis_hash, tx, "0x1", true),
        (B256::repeat_byte(8), tx, "0x1", false),
        (checkpoint().genesis_hash, B256::ZERO, "0x1", false),
        (checkpoint().genesis_hash, tx, "0x2", false),
    ] {
        let receipt = json!({"transactionHash":tx_hash,"blockHash":receipt_hash,"blockNumber":"0x1","status":status});
        let (port, handle, _) = server(vec![("eth_getTransactionReceipt", receipt)]).await;
        let result = port.finalized_receipt(checkpoint(), tx).await;
        if valid {
            assert_eq!(
                result.unwrap(),
                Some(LifecycleReceiptV1 {
                    tx_hash: tx,
                    block_height: 1,
                    block_hash: checkpoint().genesis_hash,
                    success: true
                })
            );
        } else {
            assert!(result.is_err());
        }
        handle.stop().unwrap();
    }
}
#[tokio::test]
async fn old_finalized_receipt_uses_a_bounded_number_of_rpc_calls() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let at = LifecycleCheckpointV1 {
        height: 500_000,
        ..checkpoint()
    };
    let tx = B256::repeat_byte(5);
    let receipt_hash = B256::repeat_byte(10);
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut module = RpcModule::new(calls.clone());
    module
        .register_method("eth_chainId", |_, calls, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            "0x7"
        })
        .unwrap();
    module
        .register_method("eth_getBlockByNumber", move |params, calls, _| {
            let args: Vec<Value> = params.parse()?;
            let count = calls.fetch_add(1, Ordering::SeqCst) + 1;
            if count > 10 {
                return Err(ErrorObjectOwned::owned(
                    -32000,
                    "RPC request budget exceeded",
                    None::<()>,
                ));
            }
            let mut value = block(at);
            value["parentHash"] = json!(receipt_hash);
            if args[0] == "0x0" {
                value["number"] = json!("0x0");
                value["hash"] = json!(at.genesis_hash);
            } else if args[0] != "finalized" {
                value["number"] = args[0].clone();
                if args[0] != format!("0x{:x}", at.height) {
                    value["hash"] = json!(receipt_hash);
                }
            }
            Ok(value)
        })
        .unwrap();
    module.register_method("eth_getTransactionReceipt", move |_, calls, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        json!({"transactionHash":tx,"blockHash":receipt_hash,"blockNumber":"0xa","status":"0x1"})
    }).unwrap();
    let handle = server.start(module);
    let port =
        RpcLifecyclePort::new(&format!("http://{address}"), at.chain_id, at.genesis_hash).unwrap();
    let result = port.finalized_receipt(at, tx).await;
    handle.stop().unwrap();
    assert_eq!(
        result.unwrap(),
        Some(LifecycleReceiptV1 {
            tx_hash: tx,
            block_height: 10,
            block_hash: receipt_hash,
            success: true,
        })
    );
    assert_eq!(calls.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn send_preserves_typed_rpc_error_without_prose_classification() {
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let mut module = RpcModule::new(());
    module
        .register_method("eth_sendRawTransaction", |_, _, _| {
            Err::<Value, _>(ErrorObjectOwned::owned(
                -32000,
                "nonce too low",
                Some(json!({"opaque":17})),
            ))
        })
        .unwrap();
    let handle = server.start(module);
    let port =
        RpcLifecyclePort::new(&format!("http://{address}"), 7, checkpoint().genesis_hash).unwrap();
    match port.send_raw_transaction(&[1, 2]).await.unwrap_err() {
        LifecyclePortError::Source(RpcLifecycleError::Rpc(error))
            if matches!(error.as_ref(), jsonrpsee::core::client::Error::Call(_)) =>
        {
            let jsonrpsee::core::client::Error::Call(error) = error.as_ref() else {
                unreachable!("matched above");
            };
            assert_eq!(error.code(), -32000);
            assert_eq!(error.data().unwrap().get(), "{\"opaque\":17}");
        }
        other => panic!("wrong error: {other:?}"),
    }
    handle.stop().unwrap();
}

#[test]
fn upgrade_proof_cannot_substitute_another_state_root_or_height() {
    let header = alloy_consensus::Header {
        number: 2,
        state_root: B256::repeat_byte(9),
        ..Default::default()
    };
    let mut at = checkpoint();
    at.hash = header.hash_slow();
    // Only checkpoint binding is under test; this is not a BLS/MPT-valid witness.
    let proof = bound_upgrade_proof(&header);
    let port = RpcLifecyclePort::new("127.0.0.1:1", 7, at.genesis_hash).unwrap();
    assert!(port.proof_header_within_head(at, &proof).is_ok());
    // H1 proof under a later freshness head is not a same-height mismatch.
    assert!(port
        .proof_header_within_head(LifecycleCheckpointV1 { height: 3, ..at }, &proof)
        .is_ok());
    for wrong in [
        LifecycleCheckpointV1 {
            state_root: B256::ZERO,
            ..at
        },
        LifecycleCheckpointV1 { height: 1, ..at },
        LifecycleCheckpointV1 {
            hash: B256::ZERO,
            ..at
        },
    ] {
        assert!(matches!(
            port.proof_header_within_head(wrong, &proof),
            Err(RpcLifecycleError::CheckpointMismatch)
        ));
    }
}

#[tokio::test]
async fn finalized_receipt_does_not_promote_pending_or_future_receipts() {
    let tx = B256::repeat_byte(5);
    for response in [
        Value::Null,
        json!({"transactionHash":tx,"blockNumber":"0x3","blockHash":B256::repeat_byte(3),"status":"0x1"}),
    ] {
        let (port, handle, _) = server(vec![("eth_getTransactionReceipt", response)]).await;
        assert_eq!(
            port.finalized_receipt(checkpoint(), tx).await.unwrap(),
            None
        );
        handle.stop().unwrap();
    }
}

#[tokio::test]
async fn latest_schedule_must_not_replace_the_supplied_checkpoint() {
    use outbe_primitives::tee_operator_v1::TeeRenewalScheduleV1;
    let at = checkpoint();
    let schedule = TeeRenewalScheduleV1 {
        finalized_height: at.height + 1,
        finalized_hash: B256::repeat_byte(3),
        finalized_timestamp: 21,
        epoch_number: 0,
        epoch_start_height: 0,
        epoch_length_blocks: 100,
        next_freeze_height: 90,
        planned_activation_height: 100,
        dkg_prepare_window_blocks: 10,
        minimum_block_time_millis: 1000,
    };
    let (port, handle, calls) = server(vec![(
        "outbe_teeRenewalScheduleV1",
        serde_json::to_value(schedule).unwrap(),
    )])
    .await;
    let error = port
        .registry_state(
            at,
            &NodeBindingSelectorV1::Validator(Address::repeat_byte(4)),
        )
        .await
        .unwrap_err();
    match error {
        LifecyclePortError::Source(RpcLifecycleError::Registry(error)) => assert!(matches!(
            *error,
            outbe_tee::registry_view::RegistryError::Rpc {
                source: RpcLifecycleError::CheckpointMismatch,
                ..
            }
        )),
        other => panic!("wrong error: {other:?}"),
    }
    assert_eq!(calls.lock().unwrap().len(), 1);
    handle.stop().unwrap();
}

#[tokio::test]
async fn rejected_signed_transaction_classifies_only_observed_nonce_occupancy() {
    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy_eips::eip2718::Encodable2718;
    let key = k256::ecdsa::SigningKey::from_slice(&[7; 32]).unwrap();
    let tx = TxEip1559 {
        chain_id: 7,
        nonce: 4,
        gas_limit: 21000,
        max_fee_per_gas: 10,
        max_priority_fee_per_gas: 1,
        ..Default::default()
    };
    let (signature, recovery) = key
        .sign_prehash_recoverable(tx.signature_hash().as_slice())
        .unwrap();
    let signature = alloy_primitives::Signature::from_bytes_and_parity(
        &signature.to_bytes(),
        recovery.is_y_odd(),
    );
    let raw = TxEnvelope::from(tx.into_signed(signature)).encoded_2718();
    for (latest, pending, expected) in [(5, 5, 1), (4, 5, 2), (4, 4, 0)] {
        let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap();
        let mut module = RpcModule::new(());
        module
            .register_method("eth_sendRawTransaction", |_, _, _| {
                Err::<Value, _>(ErrorObjectOwned::owned(
                    -32000,
                    "opaque rejection",
                    None::<()>,
                ))
            })
            .unwrap();
        module
            .register_method("eth_getTransactionCount", move |params, _, _| {
                let args: Vec<Value> = params.parse()?;
                Ok::<_, ErrorObjectOwned>(format!(
                    "0x{:x}",
                    if args[1] == "latest" { latest } else { pending }
                ))
            })
            .unwrap();
        let handle = server.start(module);
        let port =
            RpcLifecyclePort::new(&format!("http://{address}"), 7, checkpoint().genesis_hash)
                .unwrap();
        let error = port.send_raw_transaction(&raw).await.unwrap_err();
        assert!(matches!(
            (expected, error),
            (1, LifecyclePortError::NonceTooLow { .. })
                | (2, LifecyclePortError::TransactionDisplaced { nonce: 4, .. })
                | (0, LifecyclePortError::Source(RpcLifecycleError::Rpc(_)))
        ));
        handle.stop().unwrap();
    }
}

fn counts(calls: &Calls, method: &str) -> Vec<Vec<Value>> {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == method)
        .map(|(_, args)| args.clone())
        .collect()
}

#[tokio::test]
async fn admission_history_rescans_only_the_unverified_delta() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let at = LifecycleCheckpointV1 {
        height: 4,
        hash: B256::repeat_byte(4),
        ..checkpoint()
    };
    let corrupt = Arc::new(AtomicBool::new(false));
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let calls = Calls::default();
    let mut module = RpcModule::new((calls.clone(), corrupt.clone()));
    module
        .register_method("eth_getBlockByNumber", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context
                .0
                .lock()
                .unwrap()
                .push(("eth_getBlockByNumber".into(), args.clone()));
            let tag = args[0].as_str().unwrap_or_default();
            let height = u64::from_str_radix(tag.trim_start_matches("0x"), 16).unwrap_or(0);
            let mut hash = B256::repeat_byte(height as u8);
            if height == 4 {
                hash = at.hash;
            }
            if height == 3 && context.1.load(Ordering::SeqCst) {
                hash = B256::repeat_byte(0xee);
            }
            let parent = B256::repeat_byte(height.saturating_sub(1) as u8);
            Ok::<_, ErrorObjectOwned>(json!({
                "number": format!("0x{height:x}"),
                "hash": hash,
                "parentHash": parent,
                "stateRoot": at.state_root,
                "timestamp": "0x1",
                "baseFeePerGas": "0x1",
                "extraData": "0x",
            }))
        })
        .unwrap();
    let handle = server.start(module);
    let port =
        RpcLifecyclePort::new(&format!("http://{address}"), at.chain_id, at.genesis_hash).unwrap();
    assert!(port.admission_history(at).await.is_err());
    let first = counts(&calls, "eth_getBlockByNumber");
    assert!(first.iter().any(|args| args[0] == "0x1"));
    assert!(first.iter().any(|args| args[0] == "0x2"));
    calls.lock().unwrap().clear();
    assert!(port.admission_history(at).await.is_err());
    let second = counts(&calls, "eth_getBlockByNumber");
    assert!(second
        .iter()
        .all(|args| args[0] != "0x1" && args[0] != "0x2"));
    assert!(second.iter().any(|args| args[0] == "0x3"));
    assert!(second.iter().any(|args| args[0] == "0x4"));
    corrupt.store(true, Ordering::SeqCst);
    calls.lock().unwrap().clear();
    assert!(port.admission_history(at).await.is_err());
    let reset = counts(&calls, "eth_getBlockByNumber");
    assert!(reset.iter().any(|args| args[0] == "0x1"));
    handle.stop().unwrap();
}

#[tokio::test]
async fn onboarding_keeps_historical_proof_and_retries_outside_the_window() {
    use alloy_sol_types::SolEvent;
    use outbe_primitives::addresses::TEE_REGISTRY_ADDRESS;
    use outbe_tee::dcap_protocol::DcapOnboardingArtifactV1;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let at = checkpoint();
    let ahead = LifecycleCheckpointV1 {
        height: at.height + 1,
        hash: B256::repeat_byte(0x22),
        ..at
    };
    let intent = B256::repeat_byte(0x44);
    let context = DcapOnboardingContextV1 {
        chain_id: U256::from(at.chain_id).to_be_bytes(),
        genesis_hash: at.genesis_hash,
        intent_hash: intent,
        node_id_hash: B256::repeat_byte(0x55),
        enclave_id: B256::repeat_byte(0x56),
        binding_id: B256::repeat_byte(0x57),
        policy_hash: B256::repeat_byte(0x58),
        recipient_x25519: [9; 32],
        network_encryption_public: [8; 32],
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    let artifact = DcapOnboardingArtifactV1 {
        context,
        nonce: [1; 12],
        ciphertext: vec![2; 16],
    };
    let event = ITeeRegistryV1::OfferKeySealedForRegistryV1 {
        nodeIdHash: context.node_id_hash,
        sealedOfferKey: artifact.encode_canonical().unwrap().into(),
    };
    let encoded = event.encode_log_data();
    let tx = B256::repeat_byte(0x59);
    let log = json!({
        "address": TEE_REGISTRY_ADDRESS,
        "topics": encoded.topics(),
        "data": encoded.data,
        "blockNumber": format!("0x{:x}", at.height),
        "blockHash": at.hash,
        "transactionHash": tx,
        "removed": false,
    });
    let receipt = json!({
        "transactionHash": tx,
        "blockHash": at.hash,
        "blockNumber": format!("0x{:x}", at.height),
        "status": "0x1",
    });
    let proofs = Arc::new(AtomicUsize::new(0));
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let calls = Calls::default();
    let mut module = RpcModule::new((calls.clone(), proofs.clone()));
    module
        .register_method("eth_chainId", move |_, context, _| {
            context
                .0
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
                .0
                .lock()
                .unwrap()
                .push(("eth_getBlockByNumber".into(), args.clone()));
            let tag = args[0].as_str().unwrap_or_default();
            let header = if tag == "0x0" {
                let mut genesis = block(at);
                genesis["number"] = json!("0x0");
                genesis["hash"] = json!(at.genesis_hash);
                genesis
            } else if tag == "finalized" {
                // The window error is raised even though H2 is only one block ahead.
                block(if context.1.load(Ordering::SeqCst) == 0 {
                    at
                } else {
                    ahead
                })
            } else if tag == format!("0x{:x}", at.height) {
                block(at)
            } else {
                block(ahead)
            };
            Ok::<_, ErrorObjectOwned>(header)
        })
        .unwrap();
    let sealed_log = log;
    module
        .register_method("eth_getLogs", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context.0.lock().unwrap().push(("eth_getLogs".into(), args));
            Ok::<_, ErrorObjectOwned>(json!([sealed_log.clone()]))
        })
        .unwrap();
    let sealed_receipt = receipt;
    module
        .register_method("eth_getTransactionReceipt", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context
                .0
                .lock()
                .unwrap()
                .push(("eth_getTransactionReceipt".into(), args));
            Ok::<_, ErrorObjectOwned>(sealed_receipt.clone())
        })
        .unwrap();
    module
        .register_method("eth_getProof", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context
                .0
                .lock()
                .unwrap()
                .push(("eth_getProof".into(), args));
            let seen = context.1.fetch_add(1, Ordering::SeqCst);
            if seen == 0 {
                return Err(ErrorObjectOwned::owned(
                    -32000,
                    "distance to target block exceeds maximum proof window",
                    None::<()>,
                ));
            }
            Ok(json!({}))
        })
        .unwrap();
    let handle = server.start(module);
    let port =
        RpcLifecyclePort::new(&format!("http://{address}"), at.chain_id, at.genesis_hash).unwrap();
    let error = port.onboarding(at, intent).await.expect_err("window");
    assert!(matches!(
        error,
        RpcLifecycleError::ProofWindowMoved {
            proved,
            fresh_height,
            ..
        } if proved == at.height && fresh_height == ahead.height
    ));
    let proof_calls = counts(&calls, "eth_getProof");
    assert_eq!(proof_calls.len(), 1);
    assert_eq!(proof_calls[0][2]["blockHash"], json!(at.hash));
    assert_ne!(proof_calls[0][2]["blockHash"], json!(ahead.hash));
    let recorded = calls.lock().unwrap();
    let first = recorded
        .iter()
        .position(|(name, _)| name == "eth_getProof")
        .unwrap();
    assert_eq!(recorded[first - 1].0, "eth_getBlockByNumber");
    assert_eq!(recorded[first - 1].1[0], json!("finalized"));
    drop(recorded);
    handle.stop().unwrap();
}

#[tokio::test]
async fn onboarding_reads_a_known_receipt_before_a_bounded_reverse_scan() {
    let at = checkpoint();
    let tx = B256::repeat_byte(0x61);
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
            Ok::<_, ErrorObjectOwned>(json!(null))
        })
        .unwrap();
    module
        .register_method("eth_getLogs", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context.lock().unwrap().push(("eth_getLogs".into(), args));
            Ok::<_, ErrorObjectOwned>(json!([]))
        })
        .unwrap();
    let handle = server.start(module);
    let port =
        RpcLifecyclePort::new(&format!("http://{address}"), at.chain_id, at.genesis_hash).unwrap();
    assert_eq!(port.send_raw_transaction(&[0xab]).await.unwrap(), tx);
    assert!(port.onboarding(at, B256::repeat_byte(0x62)).await.is_err());
    let names: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    let receipt = names
        .iter()
        .position(|name| name == "eth_getTransactionReceipt")
        .unwrap();
    let logs = names.iter().position(|name| name == "eth_getLogs").unwrap();
    assert!(receipt < logs);
    handle.stop().unwrap();
}

#[tokio::test]
async fn onboarding_scan_uses_a_bounded_reverse_window() {
    let at = LifecycleCheckpointV1 {
        height: 100_000,
        hash: B256::repeat_byte(4),
        ..checkpoint()
    };
    let server = ServerBuilder::default().build("127.0.0.1:0").await.unwrap();
    let address = server.local_addr().unwrap();
    let calls = Calls::default();
    let mut module = RpcModule::new(calls.clone());
    register_chain_head(&mut module, at);
    module
        .register_method("eth_getLogs", move |params, context, _| {
            let args: Vec<Value> = params.parse()?;
            context.lock().unwrap().push(("eth_getLogs".into(), args));
            Ok::<_, ErrorObjectOwned>(json!([]))
        })
        .unwrap();
    let handle = server.start(module);
    let port =
        RpcLifecyclePort::new(&format!("http://{address}"), at.chain_id, at.genesis_hash).unwrap();
    assert!(port.onboarding(at, B256::repeat_byte(9)).await.is_err());
    let logs = counts(&calls, "eth_getLogs");
    assert_eq!(logs.len(), 4);
    assert!(logs.iter().all(|args| args[0]["fromBlock"] != json!("0x0")));
    handle.stop().unwrap();
}

#[path = "audit_tests.rs"]
mod audit_tests;
