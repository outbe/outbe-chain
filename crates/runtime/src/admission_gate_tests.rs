use super::*;

#[tokio::test]
async fn pending_lease_guard_cancellation_is_a_clean_stop() {
    let provider =
        reth_provider::test_utils::MockEthProvider::<outbe_primitives::OutbePrimitives>::new();
    let shutdown = tokio_util::sync::CancellationToken::new();
    shutdown.cancel();
    let verdict = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        crate::admission_authority::guard::run_lease_guard(
            provider,
            (
                std::sync::Arc::new(crate::OutbeRuntime::new()),
                std::sync::Arc::new(crate::OutbeRuntime::new()),
            ),
            crate::admission::TeeLeaseGuardConfigV1 {
                chain: outbe_execution::tee_remote_session::RegistryChainIdentity {
                    chain_id: 676,
                    genesis_hash: alloy_primitives::B256::repeat_byte(1),
                },
                identity: outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
                    reth_p2p_public: [2; 33],
                    expected_enclave_id: None,
                    validator: None,
                },
                anchors: crate::admission::TeeLeaseGuardAnchorsV1 {
                    gate: super::TeeLeaseGuardGateV1::Pending(full_node_admission_anchor()),
                    durable: None,
                },
                finalization_notify: std::sync::Arc::new(tokio::sync::Notify::new()),
                armed: tokio::sync::watch::channel(false).0,
            },
            (outbe_tee::TeeAuthorityChannel::default(), None),
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
        outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::Ready {
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
            outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::Ready {
                valid_until: 1_800_000_000,
            },
        )
        .expect_err("a different local canonical hash must fail closed");

    assert!(error.to_string().contains("anchor hash mismatch"));
    assert!(!gate.is_armed());
}

#[test]
fn full_node_lease_guard_rejects_every_non_ready_anchor_verdict() {
    use outbe_consensus::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

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
fn armed_gate_has_no_anchor_and_startup_diagnostics_preserve_rejections() {
    use outbe_consensus::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

    let gate = super::TeeLeaseGuardGateV1::Armed;
    assert!(gate.is_armed());
    assert_eq!(gate.anchor_to_validate(u64::MAX), None);

    let reason = super::tee_lease_admission_rejection(LocalTeeRuntimeAdmissionV1::Rejected(
        LocalTeeRuntimeRejectionV1::Expired { valid_until: 42 },
    ))
    .expect("startup diagnostics must retain the expiry reason");
    assert!(reason.contains("expired at 42"));
}

#[test]
fn validator_current_bootstrap_pending_admission_is_terminal() {
    use outbe_consensus::validators::LocalTeeRuntimeAdmissionV1;

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
    use outbe_consensus::validators::LocalTeeRuntimeAdmissionV1;

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
            outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::Ready { valid_until: 200 },
        )
        .unwrap();
    super::require_validator_tee_recovery_complete_v1(true, false, recovery, data_dir)
        .expect("an exact Ready anchor permits ordinary validator startup");
}

#[test]
fn three_state_gate_arms_only_from_a_validated_pending_anchor() {
    use outbe_consensus::validators::LocalTeeRuntimeAdmissionV1;

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
        for is_founder in [false, true] {
            assert_eq!(
                super::local_validator_tee_gate_v1(
                    Some(durable),
                    7,
                    false,
                    is_founder,
                    has_upstream
                ),
                Some(Gate::Pending(durable))
            );
        }
    }
    // Below the durable anchor (including H == 0): the durable anchor is never bypassed.
    for height in [0, 6] {
        for is_founder in [false, true] {
            assert_eq!(
                super::local_validator_tee_gate_v1(Some(durable), height, false, is_founder, true,),
                None
            );
            assert_eq!(
                super::local_validator_tee_gate_v1(Some(durable), height, false, is_founder, false,),
                Some(Gate::Pending(durable))
            );
        }
    }
    // No durable anchor at H >= 1: Ready local admission with the matching
    // offer key starts armed (founders and nodes without an enclave identity).
    for has_upstream in [false, true] {
        assert_eq!(
            super::local_validator_tee_gate_v1(None, 9, true, false, has_upstream),
            Some(Gate::Armed)
        );
    }
    assert_eq!(
        super::local_validator_tee_gate_v1(None, 9, false, false, true),
        None
    );
    assert_eq!(
        super::local_validator_tee_gate_v1(None, 9, false, false, false),
        Some(Gate::AwaitingAnchor)
    );
    // Genesis founder at H == 0 starts armed with or without --upstream;
    // a non-founder at H == 0 uses --upstream or fails closed with AwaitingAnchor.
    for has_upstream in [false, true] {
        assert_eq!(
            super::local_validator_tee_gate_v1(None, 0, false, true, has_upstream),
            Some(Gate::Armed)
        );
    }
    assert_eq!(
        super::local_validator_tee_gate_v1(None, 0, false, false, true),
        None
    );
    assert_eq!(
        super::local_validator_tee_gate_v1(None, 0, false, false, false),
        Some(Gate::AwaitingAnchor)
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
    let identity = outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
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

    let wrong_enclave = outbe_consensus::validators::LocalTeeRuntimeIdentityV1 {
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
fn canonical_tee_lease_diagnostic_does_not_record_an_operational_failure() {
    let outcome = outbe_execution::shutdown::NodeShutdown::default();
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
        let outcome = outbe_execution::shutdown::NodeShutdown::default();
        let reason = super::tee_lease_exit_reason(verdict, &outcome);
        assert!(format!("{:#}", outcome.finish(Ok(())).unwrap_err()).contains(&reason));
    }
}

#[tokio::test]
async fn live_admission_reopens_after_jail_and_expiry_without_stopping_or_losing_keys() {
    use crate::admission_authority::{guard::run_lease_guard, live::*, test_support as support};
    use futures::FutureExt as _;
    use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState};
    let context = support::context(9);
    let initial = support::provider(&context, 7, 1_800_000_000, 1_800_001_000, false);
    let provider = support::SharedProvider(Arc::new(Mutex::new(initial.0)));
    let channel = TeeAuthorityChannel::default();
    let mut watch = channel.subscribe();
    let anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Armed,
        durable: None,
    };
    let owner = Arc::new(
        LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity: initial.2,
                anchors,
                context: context.clone(),
            },
            channel.clone(),
        )
        .unwrap(),
    );
    let notify = Arc::new(tokio::sync::Notify::new());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let (armed, armed_rx) = tokio::sync::watch::channel(false);
    let guard = run_lease_guard(
        provider.clone(),
        (
            Arc::new(crate::OutbeRuntime::new()),
            Arc::new(crate::OutbeRuntime::new()),
        ),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain: initial.1,
            identity: initial.2,
            anchors,
            finalization_notify: notify.clone(),
            armed,
        },
        (channel.clone(), Some(owner.clone())),
        shutdown.clone(),
    );
    tokio::pin!(guard);
    let enclave_id = context.manifest().enclave_id().unwrap();
    for (height, jailed, valid_until, expected) in [
        (
            7,
            false,
            1_800_001_000,
            TeeAuthorityState::Armed {
                enclave_id,
                valid_until: 1_800_001_000,
            },
        ),
        (8, true, 1_800_001_000, TeeAuthorityState::SuspendedJailed),
        (
            9,
            false,
            1_800_001_000,
            TeeAuthorityState::Armed {
                enclave_id,
                valid_until: 1_800_001_000,
            },
        ),
        (
            10,
            false,
            1_800_000_000,
            TeeAuthorityState::SuspendedLeaseExpired,
        ),
        (
            11,
            false,
            1_800_002_000,
            TeeAuthorityState::Armed {
                enclave_id,
                valid_until: 1_800_002_000,
            },
        ),
    ] {
        *provider.0.lock().unwrap() =
            support::provider(&context, height, 1_800_000_000, valid_until, jailed).0;
        notify.notify_one();
        tokio::select! {
            result = &mut guard => panic!("guard exited during reversible admission: {result:?}"),
            () = support::wait_for_state(&mut watch, expected) => {},
        }
        assert_eq!(watch.snapshot(), expected);
        assert!(watch.snapshot().has_verified_resident_key());
        assert_eq!(*armed_rx.borrow(), expected.is_authority_open());
        assert!(context.offer_key_ready().unwrap());
        assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &context));
        assert!(guard.as_mut().now_or_never().is_none());
    }
    shutdown.cancel();
    assert_eq!(guard.await.unwrap(), None);
}

#[tokio::test]
async fn live_admission_invariant_failure_closes_authority_with_typed_error_and_keeps_context() {
    use crate::admission_authority::{guard::run_lease_guard, live::*, test_support as support};
    use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState};
    let context = support::context(9);
    let (provider, chain, identity) =
        support::provider(&context, 7, 1_800_000_000, 1_800_001_000, false);
    let mut storage = outbe_primitives::storage::hashmap::HashMapStorageProvider::new(676);
    outbe_primitives::storage::StorageHandle::enter(&mut storage, |storage| {
        outbe_teeregistry::TeeRegistry::new(storage)
            .tribute_offer_public_key
            .write(alloy_primitives::B256::repeat_byte(0xEE))
            .unwrap();
    });
    for ((address, slot), value) in storage.storage {
        let mut accounts = provider.inner.accounts.lock();
        let account = accounts
            .get(&address)
            .unwrap()
            .clone()
            .extend_storage([(alloy_primitives::B256::from(slot.to_be_bytes()), value)]);
        accounts.insert(address, account);
    }
    let channel = TeeAuthorityChannel::default();
    let anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Armed,
        durable: None,
    };
    let owner = Arc::new(
        LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity,
                anchors,
                context: context.clone(),
            },
            channel.clone(),
        )
        .unwrap(),
    );
    let (armed, mut armed_rx) = tokio::sync::watch::channel(true);
    let shared = support::SharedProvider(Arc::new(Mutex::new(provider)));
    let error = owner
        .refresh(&shared, &crate::OutbeRuntime::new(), &chain)
        .unwrap_err();
    let notify = Arc::new(tokio::sync::Notify::new());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let mut watch = channel.subscribe();
    let guard = run_lease_guard(
        shared.clone(),
        (
            Arc::new(crate::OutbeRuntime::new()),
            Arc::new(crate::OutbeRuntime::new()),
        ),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain,
            identity,
            anchors,
            finalization_notify: notify.clone(),
            armed,
        },
        (channel.clone(), Some(owner.clone())),
        shutdown.clone(),
    );
    tokio::pin!(guard);
    use futures::FutureExt as _;
    assert!(
        guard.as_mut().now_or_never().is_none(),
        "an offer mismatch must suspend, not terminate, the guard"
    );
    assert!(matches!(
        error.downcast_ref::<crate::errors::TeeAuthorityError>(),
        Some(crate::errors::TeeAuthorityError::ResidentOfferKeyMismatch { .. })
    ));
    assert_eq!(
        channel.snapshot(),
        TeeAuthorityState::SuspendedPolicyMismatch
    );
    tokio::select! { result = &mut guard => panic!("guard stopped on invariant failure: {result:?}"), result = tokio::time::timeout(std::time::Duration::from_secs(5), armed_rx.changed()) => result.expect("guard readiness did not change within five seconds").unwrap() }
    assert!(!*armed_rx.borrow());
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &context));
    assert!(context.offer_key_ready().unwrap());
    *shared.0.lock().unwrap() =
        support::provider(&context, 8, 1_800_000_000, 1_800_001_000, false).0;
    notify.notify_one();
    let expected = TeeAuthorityState::Armed {
        enclave_id: context.manifest().enclave_id().unwrap(),
        valid_until: 1_800_001_000,
    };
    tokio::select! { result = &mut guard => panic!("guard stopped: {result:?}"), () = support::wait_for_state(&mut watch, expected) => {} }
    shutdown.cancel();
    assert_eq!(guard.await.unwrap(), None);
}

#[tokio::test]
async fn live_founder_publishes_bootstrap_then_finalized_authority_on_the_same_guard() {
    use crate::admission_authority::{guard::run_lease_guard, live::*, test_support as support};
    use outbe_tee::{TeeAuthorityChannel, TeeAuthorityState};
    let context = support::context(9);
    let (provider, chain, identity) =
        support::provider(&context, 0, 1_700_000_000, 1_800_001_000, false);
    provider.inner.add_account(
        outbe_primitives::addresses::TEE_REGISTRY_ADDRESS,
        reth_provider::test_utils::ExtendedAccount::new(0, alloy_primitives::U256::ZERO),
    );
    let provider = support::SharedProvider(Arc::new(Mutex::new(provider)));
    let channel = TeeAuthorityChannel::default();
    let mut watch = channel.subscribe();
    let anchors = crate::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Armed,
        durable: None,
    };
    let owner = Arc::new(
        LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity,
                anchors,
                context: context.clone(),
            },
            channel.clone(),
        )
        .unwrap(),
    );
    // No worker or finalized-block stream has run: activation opens Signer now.
    assert_eq!(
        owner
            .refresh(&provider, &crate::OutbeRuntime::new(), &chain)
            .unwrap(),
        TeeAuthorityState::ArmedBootstrap {
            enclave_id: context.manifest().enclave_id().unwrap()
        }
    );
    assert!(watch.snapshot().is_authority_open());
    assert!(!watch.snapshot().has_finalized_lease());
    support::wait_for_state(
        &mut watch,
        TeeAuthorityState::ArmedBootstrap {
            enclave_id: context.manifest().enclave_id().unwrap(),
        },
    )
    .await;
    let notify = Arc::new(tokio::sync::Notify::new());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let guard = run_lease_guard(
        provider.clone(),
        (
            Arc::new(crate::OutbeRuntime::new()),
            Arc::new(crate::OutbeRuntime::new()),
        ),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain,
            identity,
            anchors,
            finalization_notify: notify.clone(),
            armed: tokio::sync::watch::channel(false).0,
        },
        (channel.clone(), Some(owner)),
        shutdown.clone(),
    );
    tokio::pin!(guard);
    for (height, expected) in [
        (
            0,
            TeeAuthorityState::ArmedBootstrap {
                enclave_id: context.manifest().enclave_id().unwrap(),
            },
        ),
        (
            1,
            TeeAuthorityState::Armed {
                enclave_id: context.manifest().enclave_id().unwrap(),
                valid_until: 1_800_001_000,
            },
        ),
    ] {
        if height > 0 {
            *provider.0.lock().unwrap() =
                support::provider(&context, height, 1_700_000_001, 1_800_001_000, false).0;
        }
        notify.notify_one();
        tokio::select! { result = &mut guard => panic!("founder guard stopped: {result:?}"), () = support::wait_for_state(&mut watch, expected) => {} }
        assert_eq!(watch.snapshot(), expected);
    }
    shutdown.cancel();
    assert_eq!(guard.await.unwrap(), None);
}

#[tokio::test]
async fn live_refresh_does_not_notify_an_unchanged_authority_state() {
    use crate::admission_authority::{live::*, test_support as support};
    use futures::FutureExt as _;
    let context = support::context(9);
    let (mut provider, chain, identity) =
        support::provider(&context, 7, 1_800_000_000, 1_800_001_000, false);
    let channel = outbe_tee::TeeAuthorityChannel::default();
    let mut watch = channel.subscribe();
    let owner = LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity,
            anchors: crate::admission::TeeLeaseGuardAnchorsV1 {
                gate: super::TeeLeaseGuardGateV1::Armed,
                durable: None,
            },
            context,
        },
        channel,
    )
    .unwrap();
    owner
        .refresh(&provider, &crate::OutbeRuntime::new(), &chain)
        .unwrap();
    assert!(watch.changed().now_or_never().unwrap().is_ok());
    owner
        .refresh(&provider, &crate::OutbeRuntime::new(), &chain)
        .unwrap();
    assert!(
        watch.changed().now_or_never().is_none(),
        "an unchanged finalized authority must not notify subscribers"
    );
    provider.fail_state_read = true;
    assert!(owner
        .refresh(&provider, &crate::OutbeRuntime::new(), &chain)
        .is_err());
    assert!(watch.changed().now_or_never().unwrap().is_ok());
    assert_eq!(
        watch.snapshot(),
        outbe_tee::TeeAuthorityState::SuspendedPolicyMismatch
    );
    assert!(owner
        .refresh(&provider, &crate::OutbeRuntime::new(), &chain)
        .is_err());
    assert!(
        watch.changed().now_or_never().is_none(),
        "repeated failure must not notify subscribers"
    );
}
