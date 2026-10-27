use super::*;

#[tokio::test]
async fn pending_lease_guard_cancellation_is_a_clean_stop() {
    let provider =
        reth_provider::test_utils::MockEthProvider::<outbe_primitives::OutbePrimitives>::new();
    let shutdown = tokio_util::sync::CancellationToken::new();
    shutdown.cancel();
    let verdict = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        super::run_tee_lease_guard_v1(
            provider,
            crate::launch::admission::TeeLeaseGuardConfigV1 {
                chain: outbe_node::tee_remote_session::RegistryChainIdentity {
                    chain_id: 676,
                    genesis_hash: alloy_primitives::B256::repeat_byte(1),
                },
                identity: outbe_engine::validators::LocalTeeRuntimeIdentityV1 {
                    reth_p2p_public: [2; 33],
                    expected_enclave_id: None,
                    validator: None,
                },
                anchors: crate::launch::admission::TeeLeaseGuardAnchorsV1 {
                    gate: super::TeeLeaseGuardGateV1::Pending(full_node_admission_anchor()),
                    durable: None,
                },
                finalization_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
                armed: tokio::sync::watch::channel(false).0,
            },
            shutdown,
        ),
    )
    .await
    .expect("cancellation must not wait for the lease polling interval")
    .expect("cancellation must not become a lease failure");
    assert_eq!(verdict, None);
}

#[test]
fn full_node_lease_guard_waits_strictly_below_authenticated_anchor() {
    let anchor = full_node_admission_anchor();
    let gate = super::TeeLeaseGuardGateV1::Pending(anchor);

    assert!(!gate.is_armed());
    assert_eq!(gate.anchor_to_validate(0), None);
    assert_eq!(gate.anchor_to_validate(6), None);
    assert_eq!(gate.anchor_to_validate(7), Some(anchor));
    assert_eq!(gate.anchor_to_validate(70), Some(anchor));
}

#[test]
fn full_node_lease_guard_arms_only_on_exact_ready_anchor() {
    let anchor = full_node_admission_anchor();
    let mut gate = super::TeeLeaseGuardGateV1::Pending(anchor);

    gate.validate_and_arm(
        anchor.finalized_hash,
        outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready {
            valid_until: 1_800_000_000,
        },
    )
    .expect("exact live anchor must arm the local guard");

    assert!(gate.is_armed());
    assert_eq!(gate.anchor_to_validate(anchor.finalized_height), None);
}

#[test]
fn full_node_lease_guard_fails_closed_on_anchor_hash_mismatch() {
    let anchor = full_node_admission_anchor();
    let mut gate = super::TeeLeaseGuardGateV1::Pending(anchor);

    let error = gate
        .validate_and_arm(
            alloy_primitives::B256::repeat_byte(0x78),
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready {
                valid_until: 1_800_000_000,
            },
        )
        .expect_err("a different local canonical hash must fail closed");

    assert!(error.to_string().contains("anchor hash mismatch"));
    assert!(!gate.is_armed());
}

#[test]
fn full_node_lease_guard_rejects_every_non_ready_anchor_verdict() {
    use outbe_engine::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

    for admission in [
        LocalTeeRuntimeAdmissionV1::BootstrapPending,
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::MissingBinding),
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::EnclaveIdentityMismatch),
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::Expired {
            valid_until: 1_700_000_000,
        }),
    ] {
        let anchor = full_node_admission_anchor();
        let mut gate = super::TeeLeaseGuardGateV1::Pending(anchor);
        assert!(gate
            .validate_and_arm(anchor.finalized_hash, admission)
            .is_err());
        assert!(!gate.is_armed());
    }
}

#[test]
fn armed_gate_has_no_anchor_and_later_rejections_remain_terminal() {
    use outbe_engine::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

    let gate = super::TeeLeaseGuardGateV1::Armed;
    assert!(gate.is_armed());
    assert_eq!(gate.anchor_to_validate(u64::MAX), None);

    let reason = super::tee_lease_admission_rejection(LocalTeeRuntimeAdmissionV1::Rejected(
        LocalTeeRuntimeRejectionV1::Expired { valid_until: 42 },
    ))
    .expect("an armed guard must preserve fail-stop semantics");
    assert!(reason.contains("expired at 42"));
}

#[test]
fn validator_current_bootstrap_pending_admission_is_terminal() {
    use outbe_engine::validators::LocalTeeRuntimeAdmissionV1;

    let reason = super::validator_recovery_startup_admission_rejection(
        LocalTeeRuntimeAdmissionV1::BootstrapPending,
    )
    .expect("current finalized BootstrapPending admission must fail closed");
    assert!(reason.contains("bootstrap-pending"));
    assert_eq!(
        super::tee_lease_admission_rejection(LocalTeeRuntimeAdmissionV1::BootstrapPending),
        None,
        "legacy no-anchor startup must preserve bootstrap compatibility"
    );
}

#[test]
fn full_node_restart_revalidates_the_new_upstream_anchor() {
    use outbe_engine::validators::LocalTeeRuntimeAdmissionV1;

    let old_anchor = full_node_admission_anchor();
    let mut before_restart = super::TeeLeaseGuardGateV1::Pending(old_anchor);
    before_restart
        .validate_and_arm(
            old_anchor.finalized_hash,
            LocalTeeRuntimeAdmissionV1::Ready { valid_until: 100 },
        )
        .unwrap();
    assert!(before_restart.is_armed());

    let new_anchor = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 12,
        finalized_hash: alloy_primitives::B256::repeat_byte(0x12),
    };
    let mut after_restart = super::TeeLeaseGuardGateV1::Pending(new_anchor);
    assert!(!after_restart.is_armed());
    assert_eq!(after_restart.anchor_to_validate(11), None);
    assert_eq!(after_restart.anchor_to_validate(12), Some(new_anchor));
    after_restart
        .validate_and_arm(
            new_anchor.finalized_hash,
            LocalTeeRuntimeAdmissionV1::Ready { valid_until: 200 },
        )
        .unwrap();
    assert!(after_restart.is_armed());
}

#[test]
fn validator_below_durable_join_anchor_without_upstream_requires_upstream_bootstrap() {
    let anchor = full_node_admission_anchor();
    let mut recovery = super::TeeLeaseGuardGateV1::Pending(anchor);
    let data_dir = std::path::Path::new("/srv/outbe/validator-3");

    let error = super::require_validator_tee_recovery_complete_v1(true, false, recovery, data_dir)
        .expect_err("a stale validator must not start authority services");
    assert!(matches!(
        error.downcast_ref::<crate::errors::LaunchError>(),
        Some(crate::errors::LaunchError::ValidatorRecoveryCatchUpRequired { height: 7, .. })
    ));
    let message = error.to_string();
    assert!(message.contains("UpstreamBootstrap"));
    assert!(message.contains("/srv/outbe/validator-3"));
    assert!(message.contains("--upstream <healthy-certified-rpc>"));
    assert!(message.contains("Do not use --upstream.nocertify"));
    assert!(message.contains("same process"));
    assert!(message.contains("fresh DKG"));

    super::require_validator_tee_recovery_complete_v1(false, false, recovery, data_dir)
        .expect("a FullNode keeps its existing asynchronous gate");
    super::require_validator_tee_recovery_complete_v1(true, true, recovery, data_dir)
        .expect("--validator --upstream bootstraps through the upstream");
    let error = super::require_validator_tee_recovery_complete_v1(
        true,
        false,
        super::TeeLeaseGuardGateV1::AwaitingAnchor,
        data_dir,
    )
    .expect_err("a validator without any validated anchor must not start");
    assert!(matches!(
        error.downcast_ref::<crate::errors::LaunchError>(),
        Some(crate::errors::LaunchError::ValidatorRecoveryCatchUpRequired { height: 0, .. })
    ));

    recovery
        .validate_and_arm(
            anchor.finalized_hash,
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready { valid_until: 200 },
        )
        .unwrap();
    super::require_validator_tee_recovery_complete_v1(true, false, recovery, data_dir)
        .expect("an exact Ready anchor permits ordinary validator startup");
}

#[test]
fn three_state_gate_arms_only_from_a_validated_pending_anchor() {
    use outbe_engine::validators::LocalTeeRuntimeAdmissionV1;

    let mut awaiting = super::TeeLeaseGuardGateV1::AwaitingAnchor;
    assert!(!awaiting.is_armed());
    assert_eq!(awaiting.pending_anchor(), None);
    assert_eq!(awaiting.anchor_to_validate(u64::MAX), None);
    // Nothing to validate: AwaitingAnchor never arms vacuously.
    awaiting
        .validate_and_arm(
            alloy_primitives::B256::ZERO,
            LocalTeeRuntimeAdmissionV1::Ready { valid_until: 1 },
        )
        .unwrap();
    assert_eq!(awaiting, super::TeeLeaseGuardGateV1::AwaitingAnchor);

    let anchor = full_node_admission_anchor();
    let mut pending = super::TeeLeaseGuardGateV1::Pending(anchor);
    assert!(!pending.is_armed());
    assert_eq!(pending.pending_anchor(), Some(anchor));
    pending
        .validate_and_arm(
            anchor.finalized_hash,
            LocalTeeRuntimeAdmissionV1::Ready { valid_until: 1 },
        )
        .unwrap();
    assert_eq!(pending, super::TeeLeaseGuardGateV1::Armed);
}

#[test]
fn local_validator_gate_uses_local_facts_before_any_upstream() {
    use super::TeeLeaseGuardGateV1 as Gate;
    let durable = full_node_admission_anchor();

    // Restart at or above the durable join anchor validates locally, even
    // with --upstream: the restart needs no upstream.
    for has_upstream in [false, true] {
        assert_eq!(
            super::local_validator_tee_gate_v1(Some(durable), 7, false, has_upstream),
            Some(Gate::Pending(durable))
        );
    }
    // Below the durable anchor: the upstream path decides, or the startup
    // check reports the catch-up error.
    assert_eq!(
        super::local_validator_tee_gate_v1(Some(durable), 6, false, true),
        None
    );
    assert_eq!(
        super::local_validator_tee_gate_v1(Some(durable), 6, false, false),
        Some(Gate::Pending(durable))
    );
    // No durable anchor at H >= 1: Ready local admission with the matching
    // offer key starts armed (founders and nodes without an enclave identity).
    for has_upstream in [false, true] {
        assert_eq!(
            super::local_validator_tee_gate_v1(None, 9, true, has_upstream),
            Some(Gate::Armed)
        );
    }
    assert_eq!(
        super::local_validator_tee_gate_v1(None, 9, false, true),
        None
    );
    assert_eq!(
        super::local_validator_tee_gate_v1(None, 9, false, false),
        Some(Gate::AwaitingAnchor)
    );
    // Genesis founder at H == 0 without an upstream.
    assert_eq!(
        super::local_validator_tee_gate_v1(None, 0, false, false),
        Some(Gate::Armed)
    );
    assert_eq!(
        super::local_validator_tee_gate_v1(None, 0, false, true),
        None
    );
}

#[test]
fn upstream_and_durable_anchors_both_fail_closed() {
    use super::TeeLeaseGuardGateV1 as Gate;
    let durable = full_node_admission_anchor();
    let upstream = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 40,
        finalized_hash: alloy_primitives::B256::repeat_byte(0x40),
    };

    let anchors = super::select_validator_upstream_anchor_v1(None, upstream, None).unwrap();
    assert_eq!(anchors.gate, Gate::Pending(upstream));
    assert_eq!(anchors.durable, None);

    let anchors = super::select_validator_upstream_anchor_v1(
        Some(durable),
        upstream,
        Some(durable.finalized_hash),
    )
    .unwrap();
    assert_eq!(
        anchors.gate,
        Gate::Pending(upstream),
        "max(durable, upstream)"
    );
    assert_eq!(anchors.durable, Some(durable));

    let same = super::select_validator_upstream_anchor_v1(
        Some(durable),
        durable,
        Some(durable.finalized_hash),
    )
    .unwrap();
    assert_eq!(same.gate, Gate::Pending(durable));
    assert_eq!(same.durable, None);

    for observed in [Some(alloy_primitives::B256::repeat_byte(0x66)), None] {
        let error = super::select_validator_upstream_anchor_v1(Some(durable), upstream, observed)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<crate::errors::LaunchError>(),
            Some(crate::errors::LaunchError::TeeAdmissionAnchorHashMismatch { height: 7, .. })
        ));
    }

    let lower_upstream = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 3,
        finalized_hash: alloy_primitives::B256::repeat_byte(0x03),
    };
    let error = super::select_validator_upstream_anchor_v1(
        Some(durable),
        lower_upstream,
        Some(durable.finalized_hash),
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::errors::LaunchError>(),
        Some(
            crate::errors::LaunchError::TeeJoinAnchorAboveUpstreamAnchor {
                durable: 7,
                upstream: 3
            }
        )
    ));
}

#[test]
fn durable_validator_anchor_must_match_the_running_chain_node_and_enclave() {
    use alloy_primitives::U256;
    use outbe_primitives::tee_attestation_v1::NodeIdV1;

    let reth_p2p_public: [u8; 33] = k256::ecdsa::SigningKey::from_bytes((&[0x41; 32]).into())
        .unwrap()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let node_id_hash = NodeIdV1 { reth_p2p_public }.node_id_hash().unwrap();
    let enclave_id = alloy_primitives::B256::repeat_byte(0x42);
    let durable = outbe_tee::FinalizedJoinAdmissionAnchorV1 {
        chain_id: U256::from(676_u64).to_be_bytes(),
        genesis_hash: alloy_primitives::B256::repeat_byte(0x43),
        node_id_hash,
        enclave_id,
        intent_hash: alloy_primitives::B256::repeat_byte(0x44),
        finalized_height: 91,
        finalized_hash: alloy_primitives::B256::repeat_byte(0x45),
        finalized_state_root: alloy_primitives::B256::repeat_byte(0x46),
        finalized_consensus_timestamp: 19_000,
    };
    let identity = outbe_engine::validators::LocalTeeRuntimeIdentityV1 {
        reth_p2p_public,
        expected_enclave_id: Some(enclave_id),
        validator: Some(alloy_primitives::Address::repeat_byte(0x47)),
    };

    assert_eq!(
        super::validator_admission_anchor_from_durable_v1(
            durable,
            676,
            durable.genesis_hash,
            identity,
        )
        .unwrap(),
        super::LocalTeeAdmissionAnchorV1 {
            finalized_height: durable.finalized_height,
            finalized_hash: durable.finalized_hash,
        }
    );

    let wrong_enclave = outbe_engine::validators::LocalTeeRuntimeIdentityV1 {
        expected_enclave_id: Some(alloy_primitives::B256::repeat_byte(0x48)),
        ..identity
    };
    assert!(super::validator_admission_anchor_from_durable_v1(
        durable,
        676,
        durable.genesis_hash,
        wrong_enclave,
    )
    .unwrap_err()
    .to_string()
    .contains("enclave"));
}

#[test]
fn canonical_tee_lease_rejection_is_a_clean_stop() {
    let outcome = outbe_node::shutdown::NodeShutdown::default();
    assert_eq!(
        super::tee_lease_exit_reason(Some(Ok("lease expired".into())), &outcome),
        "lease expired"
    );
    outcome.finish(Ok(())).unwrap();
}

#[test]
fn tee_lease_read_failure_and_missing_verdict_remain_errors() {
    let _no_logging =
        tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
    for verdict in [Some(Err(eyre::eyre!("provider read failed"))), None] {
        let outcome = outbe_node::shutdown::NodeShutdown::default();
        let reason = super::tee_lease_exit_reason(verdict, &outcome);
        assert!(format!("{:#}", outcome.finish(Ok(())).unwrap_err()).contains(&reason));
    }
}
