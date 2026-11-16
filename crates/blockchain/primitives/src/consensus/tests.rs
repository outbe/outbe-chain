use super::*;

#[test]
fn bridge_constructor_emits_no_metrics() {
    let recorder = metrics_util::debugging::DebuggingRecorder::new();
    metrics::with_local_recorder(&recorder, || {
        assert_eq!(
            ConsensusExecutionBridge::new().operating_mode(),
            ConsensusOperatingMode::Follower
        );
    });
    assert!(recorder.snapshotter().snapshot().into_vec().is_empty());
}

#[test]
fn bridge_records_operating_mode_metrics() {
    use metrics_util::debugging::{DebugValue, DebuggingRecorder};
    use ConsensusOperatingMode::{Follower, Signer, UpstreamBootstrap, Verifier};

    let recorder = DebuggingRecorder::new();
    let bridge = ConsensusExecutionBridge::new();
    let mut expected = vec![(
        metrics::Key::from_name("outbe_consensus_operating_mode"),
        DebugValue::Gauge(0.0.into()),
    )];
    let mut previous = "follower";
    for (mode, label, value) in [
        (Follower, "follower", 0.0),
        (UpstreamBootstrap, "upstream_bootstrap", 1.0),
        (Verifier, "verifier", 2.0),
        (Signer, "signer", 3.0),
        (Verifier, "verifier", 2.0),
    ] {
        assert_eq!(mode.as_label(), label);
        if previous != label {
            expected.push((
                metrics::Key::from_parts(
                    "outbe_consensus_mode_transitions_total",
                    vec![
                        metrics::Label::new("from", previous),
                        metrics::Label::new("to", label),
                    ],
                ),
                DebugValue::Counter(1),
            ));
        }
        expected[0].1 = DebugValue::Gauge(value.into());
        for _ in 0..2 {
            metrics::with_local_recorder(&recorder, || {
                metrics::gauge!("outbe_consensus_operating_mode").set(-1.0);
                bridge.set_operating_mode(mode);
            });
            assert_eq!(bridge.operating_mode(), mode);
            let snapshot = recorder.snapshotter().snapshot().into_vec();
            assert_eq!(snapshot.len(), expected.len());
            for (key, value) in &mut expected {
                let observed = snapshot
                    .iter()
                    .find(|(actual, _, _, _)| actual.key() == key)
                    .unwrap();
                assert_eq!(&observed.3, value, "{key:?}");
                // DebuggingRecorder snapshots reset counters to zero.
                if matches!(value, DebugValue::Counter(_)) {
                    *value = DebugValue::Counter(0);
                }
            }
        }
        previous = label;
    }
}

#[test]
fn randomness_status_maps_operator_flags() {
    for (status, expected_active, expected_shares) in [
        (RandomnessStatus::Unknown, false, false),
        (RandomnessStatus::Healthy, true, true),
        (RandomnessStatus::Preparing, true, true),
        (RandomnessStatus::PendingActivation, true, true),
        (RandomnessStatus::Grace, true, true),
        (RandomnessStatus::Degraded, true, false),
        (RandomnessStatus::Expired, false, false),
    ] {
        assert_eq!(status.is_consensus_active(), expected_active, "{status:?}");
        assert_eq!(status.has_threshold_shares(), expected_shares, "{status:?}");
    }
}

#[test]
fn test_bridge_recovers_poisoned_lock_without_panicking() {
    let bridge = ConsensusExecutionBridge::new();
    let poisoned = bridge.clone();

    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoned.inner.lock().unwrap();
        panic!("poison bridge lock");
    });

    bridge.set_genesis_validators(GenesisValidators {
        validators: vec![GenesisValidator {
            address: Address::with_last_byte(0x11),
            consensus_pubkey: [1u8; 48],
        }],
        epoch_length_blocks: 100,
    });

    let validators = bridge.peek_genesis_validators().unwrap();
    assert_eq!(validators.validators.len(), 1);
    assert_eq!(validators.epoch_length_blocks, 100);
}

#[test]
fn test_bridge_last_finalized_block_number() {
    let bridge = ConsensusExecutionBridge::new();
    assert_eq!(bridge.consensus_status().last_finalized_block, 0);

    bridge.set_last_finalized_block_number(42);
    assert_eq!(bridge.consensus_status().last_finalized_block, 42);

    // Reporter sets status (with view as block number placeholder).
    bridge.set_consensus_status(ConsensusStatus {
        current_view: 100,
        last_finalized_block: 0, // placeholder
        ..Default::default()
    });
    // Handler updates the real block number after.
    bridge.set_last_finalized_block_number(99);
    assert_eq!(bridge.consensus_status().last_finalized_block, 99);
}

#[test]
fn bridge_reports_only_usable_active_local_threshold_share() {
    let bridge = ConsensusExecutionBridge::new();
    bridge.set_consensus_status(ConsensusStatus {
        randomness_status: RandomnessStatus::Healthy,
        ..Default::default()
    });
    assert!(
        !bridge.has_threshold_shares(),
        "global Healthy status must not imply a local private share"
    );

    bridge.set_local_threshold_share_present(true);
    assert!(bridge.has_threshold_shares());

    for (status, expected) in [
        (RandomnessStatus::Preparing, true),
        (RandomnessStatus::PendingActivation, true),
        (RandomnessStatus::Grace, true),
        (RandomnessStatus::Degraded, false),
        (RandomnessStatus::Expired, false),
        (RandomnessStatus::Unknown, false),
        (RandomnessStatus::Healthy, true),
    ] {
        bridge.set_consensus_status(ConsensusStatus {
            randomness_status: status,
            ..Default::default()
        });
        assert_eq!(bridge.has_threshold_shares(), expected, "{status:?}");
        let (snapshot, snapshot_has_share) = bridge.consensus_status_with_threshold_shares();
        assert_eq!(snapshot.randomness_status, status);
        assert_eq!(
            snapshot_has_share, expected,
            "atomic snapshot for {status:?}"
        );
    }

    bridge.set_local_threshold_share_present(false);
    assert!(!bridge.has_threshold_shares());
}

#[test]
fn bridge_tracks_consensus_operating_mode_atomically() {
    let bridge = ConsensusExecutionBridge::new();
    assert_eq!(bridge.operating_mode(), ConsensusOperatingMode::Follower);
    assert!(!bridge.operating_mode().is_validator());

    let clone = bridge.clone();
    for (mode, expected_is_validator, expected_json) in [
        (
            ConsensusOperatingMode::UpstreamBootstrap,
            true,
            "\"upstreamBootstrap\"",
        ),
        (ConsensusOperatingMode::Verifier, true, "\"verifier\""),
        (ConsensusOperatingMode::Signer, true, "\"signer\""),
        (ConsensusOperatingMode::Follower, false, "\"follower\""),
    ] {
        bridge.set_operating_mode(mode);
        assert_eq!(clone.operating_mode(), mode);
        assert_eq!(clone.operating_mode().is_validator(), expected_is_validator);
        let serialized = serde_json::to_string(&mode).unwrap();
        assert_eq!(serialized, expected_json);
        let deserialized: ConsensusOperatingMode = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, mode);
    }
}
