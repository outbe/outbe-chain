use super::admission_provider::*;

#[test]
fn validator_gate_stays_pending_after_tee_join_until_validator_binding_and_offer_key_exist() {
    use outbe_consensus::validators::LocalTeeRuntimeAdmissionV1;

    let valid_until = 1_800_001_000;
    let offer_key = alloy_primitives::B256::repeat_byte(0xAB);

    // Scenario 1a: at H_join = 7, NodeHost binding is Ready, but validator_enclave_binding_v1
    // does not exist yet (stake() has not finalized).
    let (provider_at_join, chain, identity, hashes_at_join) = build_tee_admission_mock_provider(
        &[(7, valid_until - 100)],
        SeededTeeState {
            valid_until,
            has_validator_binding: false,
            validator_binding_mismatch: false,
            validator_jailed: false,
            offer_key,
        },
    );
    let join_anchor = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 7,
        finalized_hash: hashes_at_join[0].1,
    };
    let mut anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(join_anchor),
        durable: None,
    };

    let admission_at_join =
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &provider_at_join),
            chain,
            identity,
            &mut anchors,
            || Ok(Some(offer_key)),
        )
        .expect(
            "missing validator binding before stake() must not fail closed during UpstreamBootstrap",
        );
    assert_eq!(
        admission_at_join,
        Some(LocalTeeRuntimeAdmissionV1::Ready { valid_until })
    );
    assert_eq!(
        anchors.gate,
        super::TeeLeaseGuardGateV1::Pending(join_anchor),
        "gate must remain Pending until validator binding is finalized"
    );

    // Scenario 1b: at H_stake = 10, validator_enclave_binding_v1 is finalized and offer key matches.
    // Historical state at H_join = 7 retains has_validator_binding = false.
    let (mut provider_at_stake, _, _, hashes_at_stake) = build_tee_admission_mock_provider(
        &[(7, valid_until - 100), (10, valid_until - 50)],
        SeededTeeState {
            valid_until,
            has_validator_binding: true,
            validator_binding_mismatch: false,
            validator_jailed: false,
            offer_key,
        },
    );
    provider_at_stake
        .historical_states
        .insert(join_anchor.finalized_hash, provider_at_join.inner.clone());
    let stake_anchor = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 10,
        finalized_hash: hashes_at_stake[1].1,
    };

    let admission_at_stake =
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &provider_at_stake),
            chain,
            identity,
            &mut anchors,
            || Ok(Some(offer_key)),
        )
        .expect("finalized validator binding + matching offer key must arm the gate");
    assert_eq!(
        admission_at_stake,
        Some(LocalTeeRuntimeAdmissionV1::Ready { valid_until })
    );
    assert!(anchors.gate.is_armed());

    // Scenario 2: restart with older durable anchor (H_join = 7, where validator binding is absent)
    // and upstream anchor (H_stake = 10, where validator binding is present).
    let mut dual_anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(stake_anchor),
        durable: Some(join_anchor),
    };
    let admission_after_restart =
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &provider_at_stake),
            chain,
            identity,
            &mut dual_anchors,
            || Ok(Some(offer_key)),
        )
        .expect("durable anchor at H_join must pass NodeHost validation and arm at H_stake");
    assert_eq!(
        admission_after_restart,
        Some(LocalTeeRuntimeAdmissionV1::Ready { valid_until })
    );
    assert!(dual_anchors.gate.is_armed());
}

#[tokio::test]
async fn validator_gate_never_arms_or_signals_armed_on_terminal_rejection_or_offer_mismatch() {
    use outbe_consensus::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

    let valid_until = 1_800_001_000;
    let offer_key = alloy_primitives::B256::repeat_byte(0xCD);

    // Scenario 3a: ValidatorJailed at finalized = 10 -> gate stays Pending, armed watch stays false.
    let (jailed_provider, chain, identity, hashes) = build_tee_admission_mock_provider(
        &[(7, valid_until - 100), (10, valid_until - 50)],
        SeededTeeState {
            valid_until,
            has_validator_binding: true,
            validator_binding_mismatch: false,
            validator_jailed: true,
            offer_key,
        },
    );
    let join_anchor = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 7,
        finalized_hash: hashes[0].1,
    };
    let mut anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(join_anchor),
        durable: None,
    };
    let jailed_verdict =
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &jailed_provider),
            chain,
            identity,
            &mut anchors,
            || Ok(Some(offer_key)),
        )
        .unwrap();
    assert_eq!(
        jailed_verdict,
        Some(LocalTeeRuntimeAdmissionV1::Rejected(
            LocalTeeRuntimeRejectionV1::ValidatorJailed
        ))
    );
    assert!(!anchors.gate.is_armed());

    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    notify.notify_one();
    let (armed_tx, armed_rx) = tokio::sync::watch::channel(false);
    let guard_verdict = super::run_tee_lease_guard_v1(
        jailed_provider,
        std::sync::Arc::new(crate::OutbeRuntime::new()),
        std::sync::Arc::new(crate::OutbeRuntime::new()),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain,
            identity,
            anchors,
            finalization_notify: notify,
            armed: armed_tx,
        },
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("jailed validator returns a clean terminal rejection string");
    assert!(guard_verdict
        .expect("must reject jailed validator")
        .contains("jailed"));
    assert!(
        !*armed_rx.borrow(),
        "run_tee_lease_guard_v1 must never publish armed=true on a rejected check"
    );

    // Scenario 3b: ValidatorBindingMismatch -> gate stays Pending, armed watch stays false.
    let (mismatch_provider, _, _, mismatch_hashes) = build_tee_admission_mock_provider(
        &[(7, valid_until - 100), (10, valid_until - 50)],
        SeededTeeState {
            valid_until,
            has_validator_binding: true,
            validator_binding_mismatch: true,
            validator_jailed: false,
            offer_key,
        },
    );
    let mut mismatch_anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
            finalized_height: 7,
            finalized_hash: mismatch_hashes[0].1,
        }),
        durable: None,
    };
    assert_eq!(
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &mismatch_provider),
            chain,
            identity,
            &mut mismatch_anchors,
            || Ok(Some(offer_key)),
        )
        .unwrap(),
        Some(LocalTeeRuntimeAdmissionV1::Rejected(
            LocalTeeRuntimeRejectionV1::ValidatorBindingMismatch
        ))
    );
    assert!(!mismatch_anchors.gate.is_armed());
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    notify.notify_one();
    let (armed_tx, armed_rx) = tokio::sync::watch::channel(false);
    let mismatch_guard = super::run_tee_lease_guard_v1(
        mismatch_provider,
        std::sync::Arc::new(crate::OutbeRuntime::new()),
        std::sync::Arc::new(crate::OutbeRuntime::new()),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain,
            identity,
            anchors: mismatch_anchors,
            finalization_notify: notify,
            armed: armed_tx,
        },
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("binding mismatch returns a clean terminal rejection string");
    assert!(mismatch_guard
        .expect("must reject binding mismatch")
        .contains("disagree"));
    assert!(!*armed_rx.borrow());

    // Scenario 3c: Offer key mismatch when validator binding is Ready -> fails closed with LocalEnclaveOfferKeyMismatch.
    let (ready_provider, _, _, ready_hashes) = build_tee_admission_mock_provider(
        &[(7, valid_until - 100), (10, valid_until - 50)],
        SeededTeeState {
            valid_until,
            has_validator_binding: true,
            validator_binding_mismatch: false,
            validator_jailed: false,
            offer_key,
        },
    );
    let mut offer_anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
            finalized_height: 7,
            finalized_hash: ready_hashes[0].1,
        }),
        durable: None,
    };
    let wrong_offer = alloy_primitives::B256::repeat_byte(0xEE);
    let offer_err = crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
        (&crate::OutbeRuntime::new(), &ready_provider),
        chain,
        identity,
        &mut offer_anchors,
        || Ok(Some(wrong_offer)),
    )
    .expect_err("mismatched resident offer key must fail closed");
    assert!(matches!(
        offer_err.downcast_ref::<crate::errors::LaunchError>(),
        Some(crate::errors::LaunchError::LocalEnclaveOfferKeyMismatch)
    ));
    assert!(!offer_anchors.gate.is_armed());

    // Scenario 3d: Anchor at H_join = 7 is Ready, but current FullNode admission at H = 10 is Expired.
    let (expired_now_provider, _, _, expired_hashes) = build_tee_admission_mock_provider(
        &[(7, valid_until - 100), (10, valid_until + 10)],
        SeededTeeState {
            valid_until,
            has_validator_binding: true,
            validator_binding_mismatch: false,
            validator_jailed: false,
            offer_key,
        },
    );
    let mut expired_anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
            finalized_height: 7,
            finalized_hash: expired_hashes[0].1,
        }),
        durable: None,
    };
    assert_eq!(
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &expired_now_provider),
            chain,
            identity,
            &mut expired_anchors,
            || Ok(Some(offer_key)),
        )
        .unwrap(),
        Some(LocalTeeRuntimeAdmissionV1::Rejected(
            LocalTeeRuntimeRejectionV1::Expired { valid_until }
        ))
    );
    assert!(!expired_anchors.gate.is_armed());
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    notify.notify_one();
    let (armed_tx, armed_rx) = tokio::sync::watch::channel(false);
    let expired_guard = super::run_tee_lease_guard_v1(
        expired_now_provider,
        std::sync::Arc::new(crate::OutbeRuntime::new()),
        std::sync::Arc::new(crate::OutbeRuntime::new()),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain,
            identity,
            anchors: expired_anchors,
            finalization_notify: notify,
            armed: armed_tx,
        },
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .expect("expired lease returns a clean terminal rejection string");
    assert!(expired_guard
        .expect("must reject expired lease")
        .contains("expired"));
    assert!(!*armed_rx.borrow());

    // Scenario 3e: Provider state read failure -> returns Err and never arms or signals armed=true.
    let mut failing_provider = ready_provider;
    failing_provider.fail_state_read = true;
    let mut failing_anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
            finalized_height: 7,
            finalized_hash: ready_hashes[0].1,
        }),
        durable: None,
    };
    assert!(
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &failing_provider),
            chain,
            identity,
            &mut failing_anchors,
            || Ok(Some(offer_key)),
        )
        .is_err()
    );
    assert!(!failing_anchors.gate.is_armed());
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    notify.notify_one();
    let (armed_tx, armed_rx) = tokio::sync::watch::channel(false);
    assert!(super::run_tee_lease_guard_v1(
        failing_provider,
        std::sync::Arc::new(crate::OutbeRuntime::new()),
        std::sync::Arc::new(crate::OutbeRuntime::new()),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain,
            identity,
            anchors: failing_anchors,
            finalization_notify: notify,
            armed: armed_tx,
        },
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .is_err());
    assert!(!*armed_rx.borrow());
}
