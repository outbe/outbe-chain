use super::*;

fn renewal_epoch(number: U256, start: u64, length: u32) -> RenewalEpoch {
    RenewalEpoch {
        finalized_height: 10,
        finalized_hash: B256::repeat_byte(1),
        finalized_timestamp: 100,
        epoch: outbe_primitives::validator_dto::EpochSnapshot {
            number,
            start_timestamp: 0,
            start_block: start,
            length_blocks: length,
        },
    }
}
#[test]
fn renewal_schedule_caps_window_and_keeps_checked_arithmetic_error_order() {
    let config = TeeRenewalScheduleConfigV1 {
        dkg_prepare_window_blocks: 100,
        minimum_block_time_millis: 1000,
    };
    let schedule = renewal_epoch(U256::from(1), 5, 20)
        .schedule(config)
        .unwrap();
    assert_eq!(
        (
            schedule.next_freeze_height,
            schedule.planned_activation_height
        ),
        (5, 25)
    );
    assert_eq!(schedule.dkg_prepare_window_blocks, 20);
    for (number, start, length, message) in [
        (
            U256::from(u64::MAX) + U256::from(1),
            u64::MAX,
            0,
            "finalized epoch number exceeds u64",
        ),
        (U256::ZERO, u64::MAX, 0, "finalized epoch length is zero"),
        (
            U256::ZERO,
            u64::MAX,
            1,
            "planned activation height overflow",
        ),
    ] {
        let error = renewal_epoch(number, start, length)
            .schedule(config)
            .unwrap_err();
        assert_eq!(error.message(), message);
        assert_eq!(error.code(), jsonrpsee::types::error::INTERNAL_ERROR_CODE);
    }
}

#[test]
fn health_json_keeps_schema_and_omits_in_process_heap() {
    use outbe_tee::{TeeEnclaveHealthSnapshot, TeeEnclaveHealthState};
    let health: outbe_tee::protocol::EnclaveHealthStatusV1 =
        serde_json::from_value(serde_json::json!({
            "uptime_s": 10, "offer_key_ready": true,
            "heap": {"current_bytes": 1024, "peak_bytes": 2048},
            "requests": {"total": 2, "errored": 1, "denied": 0},
            "classes": {"initialized": 0, "founding_keyless": 0, "keyless_onboarding": 0,
                "ready": 2, "dev_source_seal": 0, "dev_recipient_ingest": 0}
        }))
        .unwrap();
    for ready in [false, true] {
        let snapshot = TeeEnclaveHealthSnapshot {
            state: if ready {
                TeeEnclaveHealthState::Ready
            } else {
                TeeEnclaveHealthState::Starting
            },
            offer_key_ready: ready,
            last_ok_unix_ms: ready.then_some(100),
            last_canary_latency_ms: ready.then_some(3),
            health_probe_supported: Some(false),
            enclave: Some(health.clone()),
            ..Default::default()
        };
        let info = super::super::enclave_health_info(&snapshot, 105);
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            serde_json::json!({
                "state": if ready {"ready"} else {"starting"},
                "ready": ready, "offerKeyReady": ready,
                "lastOkAgoMillis": ready.then_some(5),
                "lastCanaryLatencyMs": ready.then_some(3),
                "consecutiveFailures": 0, "lastFailureClass": null,
                "uptimeS": 10, "heapCurrentBytes": null, "heapPeakBytes": null,
                "healthProbeSupported": false
            })
        );
        let legacy = TeeEnclaveHealthSnapshot {
            health_probe_supported: Some(true),
            ..snapshot
        };
        let info = super::super::enclave_health_info(&legacy, 105);
        assert_eq!(info.heap_current_bytes, Some(1024));
        assert_eq!(info.heap_peak_bytes, Some(2048));
        assert_eq!(info.uptime_s, Some(10));
    }
}

#[derive(Clone, Copy)]
enum UpgradeProviderHead {
    Open { height: u64, valid_until: u64 },
    Jailed { height: u64 },
    Expired { height: u64 },
}

impl UpgradeProviderHead {
    fn height(self) -> u64 {
        match self {
            Self::Open { height, .. } | Self::Jailed { height } | Self::Expired { height } => {
                height
            }
        }
    }

    fn authority(self) -> outbe_tee::authority::TeeAuthorityState {
        match self {
            Self::Jailed { .. } => outbe_tee::authority::TeeAuthorityState::SuspendedJailed,
            Self::Expired { .. } => outbe_tee::authority::TeeAuthorityState::SuspendedLeaseExpired,
            Self::Open { valid_until, .. } => outbe_tee::authority::TeeAuthorityState::Armed {
                enclave_id: B256::repeat_byte(4),
                valid_until,
            },
        }
    }
}

fn publish_upgrade_provider_head(
    publisher: &outbe_tee::authority::TeeAuthorityChannel,
    head: UpgradeProviderHead,
) {
    let height = head.height();
    if height >= publisher.observed_finalized_height() {
        publisher.publish(head.authority());
        publisher.note_applied_finalized_height(height);
    }
}

async fn upgrade_key_v1_closes_from_fresh_armed(next: UpgradeProviderHead) {
    use outbe_tee::authority::{
        install_tee_authority_watch, tee_authority_test_guard, TeeAuthorityChannel,
    };
    let _guard = tee_authority_test_guard();
    let head = std::sync::Arc::new(std::sync::Mutex::new(UpgradeProviderHead::Open {
        height: 7,
        valid_until: 1_800_001_000,
    }));
    let channel = TeeAuthorityChannel::default();
    install_tee_authority_watch(channel.subscribe()).unwrap();
    let published = std::sync::Arc::clone(&head);
    let publisher = channel.clone();
    channel.install_sync_refresh(move || {
        publish_upgrade_provider_head(&publisher, *published.lock().expect("provider head"));
    });
    channel.note_observed_finalized_height(7);
    let provider: Arc<
        reth_ethereum::storage::noop::NoopProvider<
            reth_ethereum::chainspec::ChainSpec<OutbeHeader>,
            outbe_primitives::OutbePrimitives,
        >,
    > = Arc::new(reth_ethereum::storage::noop::NoopProvider::new(Arc::new(
        reth_ethereum::chainspec::ChainSpec::default(),
    )));
    let rpc = OutbeApiHandler::new(provider, Arc::new(crate::state_reader::ClosedRpcReader));
    let proof = outbe_tee::upgrade_transfer::UpgradeKeyProofV1 {
        anchor_outcome: Bytes::new(),
        committee_transitions: Vec::new(),
        admission: Bytes::new(),
    };
    let open_error = rpc
        .upgrade_key_v1(Bytes::new(), proof.clone(), false)
        .await
        .unwrap_err();
    assert_ne!(open_error.message(), "PrivateExecutionUnavailable");
    assert!(channel.snapshot().is_authority_open());
    let resident = channel.snapshot().has_verified_resident_key();
    assert!(resident);

    *head.lock().expect("provider head") = next;
    channel.note_observed_finalized_height(next.height());
    let closed_error = rpc
        .upgrade_key_v1(Bytes::new(), proof, false)
        .await
        .unwrap_err();
    assert_eq!(closed_error.message(), "PrivateExecutionUnavailable");
    let closed = channel.snapshot();
    assert_eq!(closed, next.authority());
    assert!(closed.has_verified_resident_key());
    assert!(!closed.is_authority_open());
}

#[tokio::test]
async fn upgrade_key_v1_closes_from_fresh_armed_to_jailed() {
    upgrade_key_v1_closes_from_fresh_armed(UpgradeProviderHead::Jailed { height: 8 }).await;
}

#[tokio::test]
async fn upgrade_key_v1_closes_from_fresh_armed_to_lease_expiry() {
    upgrade_key_v1_closes_from_fresh_armed(UpgradeProviderHead::Expired { height: 9 }).await;
}

#[tokio::test]
async fn upgrade_export_requires_live_authority_key_and_finalized_lease() {
    use outbe_tee::authority::{
        install_tee_authority_watch, tee_authority_test_guard, TeeAuthorityChannel,
        TeeAuthorityState as State,
    };
    let _guard = tee_authority_test_guard();
    let channel = TeeAuthorityChannel::default();
    install_tee_authority_watch(channel.subscribe()).unwrap();
    let provider: Arc<
        reth_ethereum::storage::noop::NoopProvider<
            reth_ethereum::chainspec::ChainSpec<OutbeHeader>,
            outbe_primitives::OutbePrimitives,
        >,
    > = Arc::new(reth_ethereum::storage::noop::NoopProvider::new(Arc::new(
        reth_ethereum::chainspec::ChainSpec::default(),
    )));
    let rpc = OutbeApiHandler::new(provider, Arc::new(crate::state_reader::ClosedRpcReader));
    for state in [
        State::UnkeyedAwaitingAdmission,
        State::SuspendedJailed,
        State::SuspendedLeaseExpired,
        State::SuspendedPolicyMismatch,
        State::ArmedBootstrap {
            enclave_id: B256::repeat_byte(7),
        },
    ] {
        channel.publish(state);
        let error = rpc
            .upgrade_key_v1(
                Bytes::new(),
                outbe_tee::upgrade_transfer::UpgradeKeyProofV1 {
                    anchor_outcome: Bytes::new(),
                    committee_transitions: Vec::new(),
                    admission: Bytes::new(),
                },
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(error.message(), "PrivateExecutionUnavailable", "{state:?}");
        channel.publish(State::Armed {
            enclave_id: B256::repeat_byte(7),
            valid_until: 123,
        });
        require_enclave_session().expect("finalized Armed reopens donor export authority");
    }
}
