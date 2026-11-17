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
        LifecyclePortError::Source(RpcLifecycleError::Rpc(
            jsonrpsee::core::client::Error::Call(error),
        )) => {
            assert_eq!(error.code(), -32000);
            assert_eq!(error.data().unwrap().get(), "{\"opaque\":17}");
        }
        other => panic!("wrong error: {other:?}"),
    }
    handle.stop().unwrap();
}

#[test]
fn upgrade_proof_cannot_substitute_another_state_root_or_height() {
    use alloy_consensus::private::alloy_rlp;
    use outbe_tee::finalized_admission::*;
    let header = alloy_consensus::Header {
        number: 2,
        state_root: B256::repeat_byte(9),
        ..Default::default()
    };
    let mut at = checkpoint();
    at.hash = header.hash_slow();
    let witness = FinalizedAdmissionWitnessV1 {
        admission: CertifiedHeaderV1 {
            finalization: vec![1],
            header: alloy_rlp::encode(&header),
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
            .map(|i| MptStorageProofV1 {
                key: B256::from(U256::from(i).to_be_bytes::<32>()),
                value: U256::ZERO,
                nodes: vec![],
            })
            .collect(),
    };
    // Only checkpoint binding is under test; this is not a BLS/MPT-valid witness.
    let proof = UpgradeKeyProofV1 {
        anchor_outcome: vec![1].into(),
        committee_transitions: vec![],
        admission: witness.encode_canonical().unwrap().into(),
    };
    let port = RpcLifecyclePort::new("127.0.0.1:1", 7, at.genesis_hash).unwrap();
    assert!(port.proof_checkpoint(at, &proof).is_ok());
    for wrong in [
        LifecycleCheckpointV1 {
            state_root: B256::ZERO,
            ..at
        },
        LifecycleCheckpointV1 { height: 3, ..at },
        LifecycleCheckpointV1 {
            hash: B256::ZERO,
            ..at
        },
    ] {
        assert!(matches!(
            port.proof_checkpoint(wrong, &proof),
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
