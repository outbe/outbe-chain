use alloy_primitives::{Address, Bytes, B256};
use commonware_cryptography::{bls12381, Signer as _};
use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
use outbe_radicle::{
    endpoint::{sign_response, EndpointAddress, EndpointFrame, EndpointResponseBody, PeerId},
    integration::{RadicleStatusChannel, SignedEndpointEvidence},
};
use outbe_slashindicator::constants::{
    DEFAULT_EVIDENCE_REWARD_PERCENT, DEFAULT_PROPOSER_FELONY_THRESHOLD,
    DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD, DEFAULT_SLASH_AMOUNT_PERCENT,
    DEFAULT_VOTER_FELONY_THRESHOLD, DEFAULT_VOTER_MISDEMEANOR_THRESHOLD,
};

use super::{radicle_peer_info, radicle_status_info, read_effective_slash_config};

#[test]
fn a_follower_bridge_does_not_make_the_handler_a_validator() {
    use outbe_primitives::consensus::{ConsensusExecutionBridge, ConsensusOperatingMode};
    let provider = std::sync::Arc::new(());
    let plain = super::OutbeApiHandler::new(provider.clone());
    let validator_bridge = ConsensusExecutionBridge::new();
    validator_bridge.set_operating_mode(ConsensusOperatingMode::Signer);
    let validator = super::OutbeApiHandler::with_bridge(provider.clone(), validator_bridge);
    let follower = super::OutbeApiHandler::with_bridge(provider, ConsensusExecutionBridge::new());
    assert!(plain.bridge.is_none());
    assert!(validator
        .bridge
        .as_ref()
        .unwrap()
        .operating_mode()
        .is_validator());
    assert!(!follower
        .bridge
        .as_ref()
        .unwrap()
        .operating_mode()
        .is_validator());
}

#[test]
fn slash_config_rpc_reports_runtime_defaults_for_zero_storage() {
    let mut provider = HashMapStorageProvider::new(1);
    let config = StorageHandle::enter(&mut provider, |storage| {
        read_effective_slash_config(storage).expect("read effective slash config")
    });

    assert_eq!(
        config.proposer_misdemeanor_threshold,
        DEFAULT_PROPOSER_MISDEMEANOR_THRESHOLD
    );
    assert_eq!(
        config.proposer_felony_threshold,
        DEFAULT_PROPOSER_FELONY_THRESHOLD
    );
    assert_eq!(
        config.voter_misdemeanor_threshold,
        DEFAULT_VOTER_MISDEMEANOR_THRESHOLD
    );
    assert_eq!(
        config.voter_felony_threshold,
        DEFAULT_VOTER_FELONY_THRESHOLD
    );
    assert_eq!(config.slash_amount_percent, DEFAULT_SLASH_AMOUNT_PERCENT);
    assert_eq!(
        config.evidence_reward_percent,
        DEFAULT_EVIDENCE_REWARD_PERCENT
    );
}

#[test]
fn radicle_rpc_reads_immutable_status() {
    let status = radicle_status_info(&RadicleStatusChannel::disabled().snapshot()).unwrap();
    assert_eq!(status.phase, crate::api::RadiclePhaseInfo::Disabled);
    assert!(status.local_node_id.is_none());
    assert_eq!(status.desired_repository_count, 0);
    assert_eq!(status.observed_repository_count, 0);

    let (publisher, handle) = RadicleStatusChannel::enabled(Address::repeat_byte(0x11), [1_u8; 32]);
    publisher.observe_manager(outbe_radicle::manager::ManagerStatus {
        desired_repository_count: 40,
        observed_repository_count: 16,
        available_repository_count: 10,
        pending_repository_count: 6,
        ..outbe_radicle::manager::ManagerStatus::default()
    });
    let active = radicle_status_info(&handle.snapshot()).unwrap();
    assert_eq!(active.desired_repository_count, 40);
    assert_eq!(active.observed_repository_count, 16);
    assert_eq!(active.available_repository_count, 10);
    assert_eq!(active.pending_repository_count, 6);
}

#[test]
fn enclave_health_defaults_to_disabled() {
    let info = super::enclave_health_info(
        &outbe_tee::TeeEnclaveHealthChannel::disabled().snapshot(),
        1_000_000,
    );
    assert_eq!(info.state, crate::api::EnclaveHealth::Disabled);
    assert!(!info.ready);
    assert!(info.last_ok_ago_millis.is_none());
    assert!(info.health_probe_supported.is_none());
}

#[test]
fn enclave_health_maps_ready_snapshot() {
    let channel = outbe_tee::TeeEnclaveHealthChannel::disabled();
    channel.publish(outbe_tee::TeeEnclaveHealthSnapshot {
        state: outbe_tee::TeeEnclaveHealthState::Ready,
        last_ok_unix_ms: Some(900_000),
        last_canary_latency_ms: Some(4),
        consecutive_failures: 0,
        last_failure: None,
        offer_key_ready: true,
        health_probe_supported: Some(true),
        enclave: Some(outbe_tee::protocol::EnclaveHealthStatusV1 {
            uptime_s: 77,
            offer_key_ready: true,
            heap: outbe_tee::protocol::EnclaveHeapUsageV1 {
                current_bytes: 1024,
                peak_bytes: 4096,
            },
            requests: outbe_tee::protocol::EnclaveRequestCountsV1 {
                total: 10,
                errored: 0,
                denied: 0,
            },
            classes: outbe_tee::protocol::EnclaveRequestClassCountsV1 {
                initialized: 2,
                founding_keyless: 0,
                keyless_onboarding: 0,
                ready: 8,
                dev_source_seal: 0,
                dev_recipient_ingest: 0,
            },
        }),
    });
    let info = super::enclave_health_info(&channel.snapshot(), 1_000_000);
    assert_eq!(info.state, crate::api::EnclaveHealth::Ready);
    assert!(info.ready);
    assert_eq!(info.last_ok_ago_millis, Some(100_000));
    assert_eq!(info.last_canary_latency_ms, Some(4));
    assert_eq!(info.uptime_s, Some(77));
    assert_eq!(info.heap_peak_bytes, Some(4096));
    // Stable JSON vocabulary (camelCase, lowercase state labels).
    let json = serde_json::to_string(&info).expect("serialize");
    assert!(json.contains("\"state\":\"ready\""), "json: {json}");
    assert!(json.contains("\"offerKeyReady\":true"), "json: {json}");
    assert!(
        json.contains("\"healthProbeSupported\":true"),
        "json: {json}"
    );
}

#[test]
fn radicle_rpc_evidence() {
    let signer = bls12381::PrivateKey::from_seed(7);
    let body = EndpointResponseBody {
        request_id: [8_u8; 32],
        chain_id: 54_322_345,
        genesis_hash: B256::repeat_byte(0xaa),
        validator: alloy_primitives::Address::repeat_byte(0x11),
        node_id: [9_u8; 32],
        addresses: vec![EndpointAddress::dns("peer.example.com", 8776).unwrap()],
        anchor_number: 40,
        anchor_hash: B256::repeat_byte(0x40),
        valid_until: 80,
    };
    let response = sign_response(body, &signer).unwrap();
    let evidence = SignedEndpointEvidence {
        peer: PeerId::from_public_key(&signer.public_key()),
        encoded_frame: EndpointFrame::Response(Box::new(response.clone())).encode(),
        response,
    };
    let info = radicle_peer_info(&evidence);
    assert_eq!(info.sender_bls_public_key.len(), 48);
    assert_eq!(info.signature.len(), 96);
    assert_eq!(info.encoded_frame.as_ref(), evidence.encoded_frame);
    assert_eq!(info.addresses, ["peer.example.com:8776"]);
    assert_eq!(info.valid_until_height, 80);
}

#[tokio::test]
async fn derive_keys_and_gratis_alias_reject_invalid_and_mismatched_signatures() {
    let handler = super::OutbeApiHandler::new(std::sync::Arc::new(()));
    let account = Address::repeat_byte(0x11);
    let ephemeral_pubkey = B256::repeat_byte(0x22);

    let short_err = handler
        .derive_keys_checked(
            outbe_tee::protocol::Ledger::Gratis,
            account,
            ephemeral_pubkey,
            Bytes::from_static(&[1, 2, 3]),
        )
        .await
        .unwrap_err();
    assert_eq!(
        short_err.code(),
        jsonrpsee::types::error::INVALID_PARAMS_CODE
    );
    assert_eq!(
        short_err.message(),
        "signature must be 65 bytes (r||s||v), got 3"
    );

    let malformed_err = handler
        .derive_gratis_keys_checked(account, ephemeral_pubkey, Bytes::from([0xff; 65]))
        .await
        .unwrap_err();
    assert_eq!(
        malformed_err.code(),
        jsonrpsee::types::error::INVALID_PARAMS_CODE
    );
    assert!(malformed_err
        .message()
        .starts_with("signature recovery failed:"));

    let prehash =
        outbe_tee::protocol::eip191_hash(&outbe_tee::protocol::derive_account_keys_message(
            outbe_tee::protocol::Ledger::Gratis,
            account,
            ephemeral_pubkey,
        ));
    let (foreign_sig, recovered) = (1u8..=16)
        .find_map(|r_byte| {
            let mut sig = [0u8; 65];
            sig[31] = r_byte;
            sig[63] = 1;
            sig[64] = 27;
            outbe_primitives::tee_signatures::recover_signer(&prehash, &sig)
                .ok()
                .filter(|addr| *addr != account)
                .map(|addr| (sig, addr))
        })
        .expect("should find a recoverable secp256k1 point");

    for err in [
        handler
            .derive_keys_checked(
                outbe_tee::protocol::Ledger::Gratis,
                account,
                ephemeral_pubkey,
                Bytes::copy_from_slice(&foreign_sig),
            )
            .await
            .unwrap_err(),
        handler
            .derive_gratis_keys_checked(
                account,
                ephemeral_pubkey,
                Bytes::copy_from_slice(&foreign_sig),
            )
            .await
            .unwrap_err(),
    ] {
        assert_eq!(err.code(), jsonrpsee::types::error::INVALID_PARAMS_CODE);
        assert_eq!(
            err.message(),
            format!("signature signer {recovered} does not control account {account}")
        );
    }
}

#[tokio::test]
async fn get_finality_proof_enforces_bridge_overflow_window_and_byte_limits() {
    use outbe_primitives::consensus::{ConsensusExecutionBridge, FinalizedBlockBytes};

    let no_bridge = super::OutbeApiHandler::new(std::sync::Arc::new(()));
    let err = no_bridge.finality_proof_for_height(1).await.unwrap_err();
    assert_eq!(err.code(), jsonrpsee::types::error::INTERNAL_ERROR_CODE);
    assert_eq!(err.message(), "node is not serving consensus finalizations");

    // Overflow when walking from u64::MAX without a direct certificate at u64::MAX.
    let overflow_bridge = ConsensusExecutionBridge::new();
    overflow_bridge.set_last_finalized_block_number(u64::MAX);
    let mut overflow_rx = overflow_bridge.set_finalization_fetcher();
    tokio::spawn(async move {
        if let Some((height, reply)) = overflow_rx.recv().await {
            assert_eq!(height, u64::MAX);
            let _ = reply.send(Some(FinalizedBlockBytes {
                finalization: Bytes::new(),
                block: Bytes::from_static(&[0xaa]),
            }));
        }
    });
    let overflow_handler =
        super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), overflow_bridge);
    let err = overflow_handler
        .finality_proof_for_height(u64::MAX)
        .await
        .unwrap_err();
    assert_eq!(err.message(), "proof height overflow");

    // Exceeds 4 MiB total block payload limit.
    let oversize_bridge = ConsensusExecutionBridge::new();
    oversize_bridge.set_last_finalized_block_number(10);
    let mut oversize_rx = oversize_bridge.set_finalization_fetcher();
    tokio::spawn(async move {
        if let Some((_, reply)) = oversize_rx.recv().await {
            let _ = reply.send(Some(FinalizedBlockBytes {
                finalization: Bytes::from_static(&[0x01]),
                block: Bytes::from(vec![0u8; 4 * 1024 * 1024 + 1]),
            }));
        }
    });
    let oversize_handler =
        super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), oversize_bridge);
    let err = oversize_handler
        .finality_proof_for_height(10)
        .await
        .unwrap_err();
    assert_eq!(err.message(), "ancestor finality proof exceeds byte limit");

    // 65 consecutive uncertified blocks (offsets 0..=64) exhaust the 64-block descendant window.
    let window_bridge = ConsensusExecutionBridge::new();
    window_bridge.set_last_finalized_block_number(100);
    let mut window_rx = window_bridge.set_finalization_fetcher();
    tokio::spawn(async move {
        while let Some((_, reply)) = window_rx.recv().await {
            let _ = reply.send(Some(FinalizedBlockBytes {
                finalization: Bytes::new(),
                block: Bytes::from_static(&[0xbb]),
            }));
        }
    });
    let window_handler =
        super::OutbeApiHandler::with_bridge(std::sync::Arc::new(()), window_bridge);
    let err = window_handler
        .finality_proof_for_height(100)
        .await
        .unwrap_err();
    assert_eq!(err.message(), "no descendant finalization within 64 blocks");
}
