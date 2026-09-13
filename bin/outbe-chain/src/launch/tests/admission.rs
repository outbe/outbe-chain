use super::*;

#[test]
fn full_node_lease_guard_waits_strictly_below_authenticated_anchor() {
    let anchor = full_node_admission_anchor();
    let gate = super::TeeLeaseGuardGateV1::new(Some(anchor));

    assert!(!gate.is_armed());
    assert_eq!(gate.anchor_to_validate(0), None);
    assert_eq!(gate.anchor_to_validate(6), None);
    assert_eq!(gate.anchor_to_validate(7), Some(anchor));
    assert_eq!(gate.anchor_to_validate(70), Some(anchor));
}

#[test]
fn full_node_lease_guard_arms_only_on_exact_ready_anchor() {
    let anchor = full_node_admission_anchor();
    let mut gate = super::TeeLeaseGuardGateV1::new(Some(anchor));

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
    let mut gate = super::TeeLeaseGuardGateV1::new(Some(anchor));

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
        let mut gate = super::TeeLeaseGuardGateV1::new(Some(anchor));
        assert!(gate
            .validate_and_arm(anchor.finalized_hash, admission)
            .is_err());
        assert!(!gate.is_armed());
    }
}

#[test]
fn validators_start_armed_and_later_rejections_remain_terminal() {
    use outbe_engine::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

    let gate = super::TeeLeaseGuardGateV1::new(None);
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
    let mut before_restart = super::TeeLeaseGuardGateV1::new(Some(old_anchor));
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
    let mut after_restart = super::TeeLeaseGuardGateV1::new(Some(new_anchor));
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
fn validator_below_durable_join_anchor_fails_with_certified_follower_recovery() {
    let anchor = full_node_admission_anchor();
    let mut recovery = super::TeeLeaseGuardGateV1::new(Some(anchor));
    let data_dir = std::path::Path::new("/srv/outbe/validator-3");

    let error = super::require_validator_tee_recovery_complete_v1(true, recovery, data_dir)
        .expect_err("a stale validator must not start authority services");
    let message = error.to_string();
    assert!(message.contains("certified follower"));
    assert!(message.contains("/srv/outbe/validator-3"));
    assert!(message.contains("omit --validator"));
    assert!(message.contains("--upstream <healthy-certified-rpc>"));
    assert!(message.contains("do not use --upstream.nocertify"));
    assert!(message.contains("stop the follower"));
    assert!(message.contains("fresh DKG"));

    super::require_validator_tee_recovery_complete_v1(false, recovery, data_dir)
        .expect("a FullNode keeps its existing asynchronous gate");
    super::require_validator_tee_recovery_complete_v1(
        true,
        super::TeeLeaseGuardGateV1::new(None),
        data_dir,
    )
    .expect("a legacy validator without recovery state remains compatible");

    recovery
        .validate_and_arm(
            anchor.finalized_hash,
            outbe_engine::validators::LocalTeeRuntimeAdmissionV1::Ready { valid_until: 200 },
        )
        .unwrap();
    super::require_validator_tee_recovery_complete_v1(true, recovery, data_dir)
        .expect("an exact Ready anchor permits ordinary validator startup");
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
