use super::admission_provider::*;

#[test]
fn validator_gate_stays_pending_after_enrollment_until_validator_binding_and_offer_key_exist() {
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
    let mut gate = super::TeeLeaseGuardGateV1::Pending(join_anchor);

    let admission_at_join =
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &provider_at_join),
            chain,
            identity,
            &mut gate,
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
        gate,
        super::TeeLeaseGuardGateV1::Pending(join_anchor),
        "gate must remain Pending until validator binding is finalized"
    );

    // Scenario 1b: at H_stake = 10, validator_enclave_binding_v1 is finalized and offer key matches.
    // Historical state at H_join = 7 retains has_validator_binding = false.
    let (mut provider_at_stake, _, _, _) = build_tee_admission_mock_provider(
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
    let admission_at_stake =
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &provider_at_stake),
            chain,
            identity,
            &mut gate,
            || Ok(Some(offer_key)),
        )
        .expect("finalized validator binding + matching offer key must arm the gate");
    assert_eq!(
        admission_at_stake,
        Some(LocalTeeRuntimeAdmissionV1::Ready { valid_until })
    );
    assert!(gate.is_armed());
}

#[test]
fn validator_gate_never_arms_on_rejection_or_offer_mismatch() {
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
    let mut gate = super::TeeLeaseGuardGateV1::Pending(join_anchor);
    let jailed_verdict =
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &jailed_provider),
            chain,
            identity,
            &mut gate,
            || Ok(Some(offer_key)),
        )
        .unwrap();
    assert_eq!(
        jailed_verdict,
        Some(LocalTeeRuntimeAdmissionV1::Rejected(
            LocalTeeRuntimeRejectionV1::ValidatorJailed
        ))
    );
    assert!(!gate.is_armed());

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
    let mut mismatch_gate = super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 7,
        finalized_hash: mismatch_hashes[0].1,
    });
    assert_eq!(
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &mismatch_provider),
            chain,
            identity,
            &mut mismatch_gate,
            || Ok(Some(offer_key)),
        )
        .unwrap(),
        Some(LocalTeeRuntimeAdmissionV1::Rejected(
            LocalTeeRuntimeRejectionV1::ValidatorBindingMismatch
        ))
    );
    assert!(!mismatch_gate.is_armed());
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
    let mut offer_gate = super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 7,
        finalized_hash: ready_hashes[0].1,
    });
    let wrong_offer = alloy_primitives::B256::repeat_byte(0xEE);
    let offer_err = crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
        (&crate::OutbeRuntime::new(), &ready_provider),
        chain,
        identity,
        &mut offer_gate,
        || Ok(Some(wrong_offer)),
    )
    .expect_err("mismatched resident offer key must fail closed");
    assert!(matches!(
        offer_err.downcast_ref::<crate::errors::LaunchError>(),
        Some(crate::errors::LaunchError::LocalEnclaveOfferKeyMismatch)
    ));
    assert!(!offer_gate.is_armed());

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
    let mut expired_gate = super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 7,
        finalized_hash: expired_hashes[0].1,
    });
    assert_eq!(
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &expired_now_provider),
            chain,
            identity,
            &mut expired_gate,
            || Ok(Some(offer_key)),
        )
        .unwrap(),
        Some(LocalTeeRuntimeAdmissionV1::Rejected(
            LocalTeeRuntimeRejectionV1::Expired { valid_until }
        ))
    );
    assert!(!expired_gate.is_armed());
    // Scenario 3e: Provider state read failure -> returns Err and never arms or signals armed=true.
    let mut failing_provider = ready_provider;
    failing_provider.fail_state_read = true;
    let mut failing_gate = super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 7,
        finalized_hash: ready_hashes[0].1,
    });
    assert!(
        crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            (&crate::OutbeRuntime::new(), &failing_provider),
            chain,
            identity,
            &mut failing_gate,
            || Ok(Some(offer_key)),
        )
        .is_err()
    );
    assert!(!failing_gate.is_armed());
}

#[tokio::test]
async fn live_admission_recovers_validator_binding_and_provider_read_failures() {
    use crate::admission_authority::{guard::run_lease_guard, live::*, test_support as support};
    use futures::FutureExt as _;
    use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState};
    use std::sync::{Arc, Mutex};
    for read_failure in [false, true] {
        let context = support::context(9);
        let (mut broken, chain, mut identity, _) = build_tee_admission_mock_provider(
            &[(7, 1_800_000_000)],
            SeededTeeState {
                valid_until: 1_800_001_000,
                has_validator_binding: true,
                validator_binding_mismatch: !read_failure,
                validator_jailed: false,
                offer_key: alloy_primitives::B256::from(
                    context.network_encryption_public().unwrap(),
                ),
            },
        );
        support::bind_context(&broken, &context);
        identity.expected_enclave_id = Some(context.manifest().enclave_id().unwrap());
        broken.fail_state_read = read_failure;
        let provider = support::SharedProvider(Arc::new(Mutex::new(broken)));
        let channel = TeeAuthorityChannel::default();
        let mut watch = channel.subscribe();
        let gate = super::TeeLeaseGuardGateV1::Armed;
        let owner = Arc::new(
            LiveTeeRuntimeV1::new(
                LiveTeeSnapshotV1 {
                    identity,
                    gate,
                    context: context.clone(),
                },
                channel.clone(),
            )
            .unwrap(),
        );
        if !read_failure {
            assert_eq!(crate::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
                (&crate::OutbeRuntime::new(), &provider), chain, identity, &mut gate.clone(),
                || Ok(Some(alloy_primitives::B256::from(context.network_encryption_public().unwrap())))
            ).unwrap(), Some(outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::Rejected(outbe_consensus::validators::LocalTeeRuntimeRejectionV1::ValidatorBindingMismatch)));
        }
        let notify = Arc::new(tokio::sync::Notify::new());
        let cancel = tokio_util::sync::CancellationToken::new();
        let guard = run_lease_guard(
            provider.clone(),
            Arc::new(crate::OutbeRuntime::new()),
            crate::admission::TeeLeaseGuardConfigV1 {
                chain,
                finalization_notify: notify.clone(),
                armed: tokio::sync::watch::channel(true).0,
            },
            owner.clone(),
            cancel.clone(),
        );
        tokio::pin!(guard);
        tokio::select! { result = &mut guard => panic!("guard terminated on recoverable failure: {result:?}"), () = support::wait_for_state(&mut watch, TeeAuthorityState::SuspendedPolicyMismatch) => {} }
        assert!(guard.as_mut().now_or_never().is_none());
        assert!(watch.snapshot().has_verified_resident_key());
        *provider.0.lock().unwrap() =
            support::provider(&context, 8, 1_800_000_000, 1_800_001_000, false).0;
        notify.notify_one();
        let armed = TeeAuthorityState::Armed {
            enclave_id: context.manifest().enclave_id().unwrap(),
            valid_until: 1_800_001_000,
        };
        tokio::select! { result = &mut guard => panic!("guard did not recover: {result:?}"), () = support::wait_for_state(&mut watch, armed) => {} }
        assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &context));
        cancel.cancel();
        assert_eq!(guard.await.unwrap(), None);
    }
}
