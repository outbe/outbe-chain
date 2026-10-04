use super::finality::wait_for_finalized_prepare;
use super::*;
use crate::rpc::mock::MockRpc;
use outbe_rpc::test_support::RecordingRpc;

fn saved_prepare() -> Result<(
    RelaySignerV1,
    NetworkUpgradeSubmissionV1,
    DcapOnboardingContextV1,
)> {
    let relay = RelaySignerV1::new(&"11".repeat(32))?;
    let context = DcapOnboardingContextV1 {
        chain_id: U256::from(676).to_be_bytes(),
        genesis_hash: B256::repeat_byte(1),
        intent_hash: B256::repeat_byte(2),
        node_id_hash: B256::repeat_byte(3),
        enclave_id: B256::repeat_byte(4),
        binding_id: B256::repeat_byte(5),
        policy_hash: B256::repeat_byte(6),
        recipient_x25519: [7; 32],
        tribute_offer_public: [8; 32],
        key_epoch: 9,
        tribute_offer_epoch: 10,
    };
    let calldata = vec![0x41, 0x42];
    let transaction = relay.sign_renewal(
        676,
        7,
        U256::from(3),
        100_000,
        TEE_REGISTRY_ADDRESS,
        &calldata,
    )?;
    let durable = NetworkUpgradeSubmissionV1 {
        candidate_manifest_hash: B256::repeat_byte(11),
        evidence: vec![0x51],
        context: context.encode_canonical(),
        calldata,
        transaction,
    };
    Ok((relay, durable, context))
}

fn pending_rpc(context: &DcapOnboardingContextV1, context_hash: B256, valid_until: u64) -> MockRpc {
    let mut rpc = MockRpc {
        finalized_block: Ok(serde_json::json!({"number":"0x5b", "timestamp":"0x64"})),
        ..Default::default()
    };
    rpc.eth_call_overrides.insert(
        (
            TEE_REGISTRY_ADDRESS,
            ITeeRegistryV1::pendingEnclaveUpgradeCall {
                nodeIdHash: context.node_id_hash,
            }
            .abi_encode(),
        ),
        ITeeRegistryV1::pendingEnclaveUpgradeCall::abi_encode_returns(
            &ITeeRegistryV1::pendingEnclaveUpgradeReturn {
                contextHash: context_hash,
                validUntil: valid_until,
                sourceBindingId: B256::repeat_byte(12),
                targetHash: B256::repeat_byte(13),
                nonce: 2,
            },
        ),
    );
    rpc
}

#[tokio::test]
async fn saved_prepare_identity_and_context_are_checked_before_rpc() -> Result<()> {
    let (relay, mut durable, _) = saved_prepare()?;
    let rpc = RecordingRpc::new([]);
    durable.transaction.transaction_hash = B256::ZERO;
    assert_eq!(
        wait_for_finalized_prepare(&rpc, &relay, &durable, Duration::ZERO)
            .await
            .unwrap_err()
            .to_string(),
        "saved prepare transaction does not match this relay"
    );
    durable.transaction.transaction_hash = keccak256(&durable.transaction.raw_transaction);
    let another_relay = RelaySignerV1::new(&"22".repeat(32))?;
    assert_eq!(
        wait_for_finalized_prepare(&rpc, &another_relay, &durable, Duration::ZERO)
            .await
            .unwrap_err()
            .to_string(),
        "saved prepare transaction does not match this relay"
    );
    durable.context = vec![0];
    assert!(
        wait_for_finalized_prepare(&rpc, &relay, &durable, Duration::ZERO)
            .await
            .unwrap_err()
            .to_string()
            .starts_with("saved context:")
    );
    rpc.assert_done();
    assert!(rpc.recorded_calls().is_empty());
    Ok(())
}

#[tokio::test]
async fn exact_live_finalized_candidate_needs_no_relay_or_receipt() -> Result<()> {
    let (relay, durable, context) = saved_prepare()?;
    let rpc = pending_rpc(&context, context.context_hash(), 101);
    let (observed, height) =
        wait_for_finalized_prepare(&rpc, &relay, &durable, Duration::ZERO).await?;
    assert_eq!(observed, context);
    assert_eq!(height, 91);
    // All relay/receipt methods remain errors in this adapter.
    Ok(())
}

#[tokio::test]
async fn expired_or_different_candidate_times_out_before_relay() -> Result<()> {
    let (relay, durable, context) = saved_prepare()?;
    for (hash, deadline) in [(context.context_hash(), 100), (B256::repeat_byte(14), 101)] {
        let rpc = pending_rpc(&context, hash, deadline);
        assert_eq!(
            wait_for_finalized_prepare(&rpc, &relay, &durable, Duration::ZERO)
                .await
                .unwrap_err()
                .to_string(),
            "prepare not finalized; saved transaction retained, rerun upgrade-provision"
        );
    }
    Ok(())
}

#[tokio::test]
async fn finalized_read_error_precedes_timeout() -> Result<()> {
    let (relay, durable, _) = saved_prepare()?;
    let rpc = MockRpc {
        finalized_block: Err(eyre::eyre!("finalized unavailable")),
        ..Default::default()
    };
    assert_eq!(
        wait_for_finalized_prepare(&rpc, &relay, &durable, Duration::ZERO)
            .await
            .unwrap_err()
            .to_string(),
        "finalized unavailable"
    );
    Ok(())
}

#[tokio::test]
async fn different_relay_hash_rejects_without_reading_receipt() -> Result<()> {
    let (relay, durable, context) = saved_prepare()?;
    let mut rpc = pending_rpc(&context, B256::ZERO, 0);
    rpc.send_raw_tx = Ok(format!("{:#x}", B256::repeat_byte(15)));
    assert_eq!(
        wait_for_finalized_prepare(&rpc, &relay, &durable, Duration::from_secs(30))
            .await
            .unwrap_err()
            .to_string(),
        "RPC returned a different prepare transaction hash"
    );
    Ok(())
}

#[tokio::test]
async fn already_known_prepare_still_checks_reverted_receipt() -> Result<()> {
    let (relay, durable, context) = saved_prepare()?;
    let mut rpc = pending_rpc(&context, B256::ZERO, 0);
    rpc.send_raw_tx = Err(eyre::eyre!("ALREADY KNOWN"));
    rpc.transaction_receipt = Ok(Some(serde_json::json!({"status":"0x0"})));
    assert_eq!(
        wait_for_finalized_prepare(&rpc, &relay, &durable, Duration::from_secs(30))
            .await
            .unwrap_err()
            .to_string(),
        "prepare transaction reverted; saved evidence retained for diagnosis"
    );
    Ok(())
}
