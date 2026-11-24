use super::*;

#[tokio::test]
async fn pending_lease_guard_cancellation_is_a_clean_stop() {
    use crate::admission::TeeLeaseGuardConfigV1;
    use crate::admission_authority::{guard::run_lease_guard, live::*, test_support as support};

    let context = support::context(9);
    let (provider, chain, identity) =
        support::provider(&context, 6, 1_800_000_000, 1_800_001_000, false);
    let owner = Arc::new(
        LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity,
                gate: super::TeeLeaseGuardGateV1::Pending {
                    target: full_node_admission_anchor(),
                    floor: None,
                },
                context,
            },
            outbe_tee::TeeAuthorityChannel::default(),
        )
        .unwrap(),
    );
    let shutdown = tokio_util::sync::CancellationToken::new();
    shutdown.cancel();
    let verdict = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        run_lease_guard(
            provider,
            Arc::new(crate::OutbeRuntime::new()),
            TeeLeaseGuardConfigV1 {
                chain,
                finalization_notify: Arc::new(tokio::sync::Notify::new()),
                armed: tokio::sync::watch::channel(false).0,
            },
            owner,
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
    let gate = super::TeeLeaseGuardGateV1::Pending {
        target: anchor,
        floor: None,
    };

    assert!(!gate.is_armed());
    assert_eq!(gate.anchor_to_validate(0), None);
    assert_eq!(gate.anchor_to_validate(6), None);
    assert_eq!(gate.anchor_to_validate(7), Some(anchor));
    assert_eq!(gate.anchor_to_validate(70), Some(anchor));
}

#[test]
fn full_node_lease_guard_arms_only_on_exact_ready_anchor() {
    let anchor = full_node_admission_anchor();
    let mut gate = super::TeeLeaseGuardGateV1::Pending {
        target: anchor,
        floor: None,
    };

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
    let mut gate = super::TeeLeaseGuardGateV1::Pending {
        target: anchor,
        floor: None,
    };

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
        let mut gate = super::TeeLeaseGuardGateV1::Pending {
            target: anchor,
            floor: None,
        };
        assert!(gate
            .validate_and_arm(anchor.finalized_hash, admission)
            .is_err());
        assert!(!gate.is_armed());
    }
}

#[test]
fn armed_gate_has_no_anchor_and_rejection_preserves_expiry() {
    use outbe_consensus::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

    let gate = super::TeeLeaseGuardGateV1::Armed;
    assert!(gate.is_armed());
    assert_eq!(gate.anchor_to_validate(u64::MAX), None);

    let anchor = full_node_admission_anchor();
    let error = super::TeeLeaseGuardGateV1::validate_anchor(
        anchor,
        anchor.finalized_hash,
        LocalTeeRuntimeAdmissionV1::Rejected(LocalTeeRuntimeRejectionV1::Expired {
            valid_until: 42,
        }),
    )
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::errors::LaunchError>(),
        Some(crate::errors::LaunchError::TeeAdmissionAnchorRejected { reason })
            if reason.contains("expired at 42") && reason.contains("outbe-cli tee status")
    ));
}

#[test]
fn full_node_restart_revalidates_the_new_upstream_anchor() {
    use outbe_consensus::validators::LocalTeeRuntimeAdmissionV1;

    let old_anchor = full_node_admission_anchor();
    let mut before_restart = super::TeeLeaseGuardGateV1::Pending {
        target: old_anchor,
        floor: None,
    };
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
    let mut after_restart = super::TeeLeaseGuardGateV1::Pending {
        target: new_anchor,
        floor: None,
    };
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
    let mut pending = super::TeeLeaseGuardGateV1::Pending {
        target: anchor,
        floor: None,
    };
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
    let gate = super::TeeLeaseGuardGateV1::Armed;
    let owner = Arc::new(
        LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity: initial.2,
                gate,
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
        Arc::new(crate::OutbeRuntime::new()),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain: initial.1,
            finalization_notify: notify.clone(),
            armed,
        },
        owner.clone(),
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
            .offer_public_key
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
        Arc::new(crate::OutbeRuntime::new()),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain,
            finalization_notify: notify.clone(),
            armed,
        },
        owner.clone(),
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
        Arc::new(crate::OutbeRuntime::new()),
        crate::admission::TeeLeaseGuardConfigV1 {
            chain,
            finalization_notify: notify.clone(),
            armed: tokio::sync::watch::channel(false).0,
        },
        owner,
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
            gate: super::TeeLeaseGuardGateV1::Armed,
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

fn fresh_context_with_anchor(
    seed: u8,
    anchor: Option<outbe_tee::in_process::admission::VerifiedAdmissionAnchorV1>,
) -> Arc<outbe_tee::in_process::InProcessTeeContext> {
    let original = crate::admission_authority::test_support::context(seed);
    let context = Arc::new(
        outbe_tee::in_process::InProcessTeeContext::new(
            outbe_tee::resident::ResidentTeeKeys::from_seed([seed; 32]).unwrap(),
            *original.identity(),
            original.manifest().clone(),
            outbe_primitives::tee_attestation_v1::TrustedNetworkDescriptorV1 {
                network_binding: original.manifest().network_binding(),
                genesis_consensus_keys: vec![],
            },
            outbe_tee::in_process::ProofSourceV1::NoHardware,
        )
        .unwrap(),
    );
    let material = outbe_tee::resident::ResidentOfferMaterial::for_test(
        vec![5; 96],
        alloy_primitives::B256::from(context.manifest().chain_id),
        0,
        0,
    )
    .unwrap();
    match anchor {
        Some(anchor) => context
            .activate_for_test_with_anchor(material, anchor)
            .unwrap(),
        None => context.activate_for_test(material).unwrap(),
    }
    context
}

#[test]
fn rolled_back_db_below_durable_floor_stays_closed_until_catch_up_and_hash_match() {
    use crate::admission_authority::{live::*, test_support as support};
    use crate::launch_tests::admission_provider::{
        build_tee_admission_mock_provider, SeededTeeState,
    };
    use outbe_tee::in_process::admission::VerifiedAdmissionAnchorV1;
    use outbe_tee::TeeAuthorityState;

    let valid_until = 1_800_001_000;
    let probe = fresh_context_with_anchor(9, None);
    let offer_key = alloy_primitives::B256::from(probe.network_encryption_public().unwrap());

    // Build canonical chain with blocks 7 and 10; durable floor d is at height 10.
    let (caught_up_provider, chain, mut identity, hashes) = build_tee_admission_mock_provider(
        &[(7, 1_800_000_000), (10, 1_800_000_050)],
        SeededTeeState {
            valid_until,
            has_validator_binding: true,
            validator_binding_mismatch: false,
            validator_jailed: false,
            offer_key,
        },
    );
    let floor_hash = hashes[1].1;
    let floor_verified = VerifiedAdmissionAnchorV1 {
        block_number: 10,
        block_hash: floor_hash,
        state_root: alloy_primitives::B256::repeat_byte(0xBB),
        consensus_timestamp: 1_800_000_050,
    };
    let context = fresh_context_with_anchor(9, Some(floor_verified));
    support::bind_context(&caught_up_provider, &context);
    identity.expected_enclave_id = Some(context.manifest().enclave_id().unwrap());

    // Rolled-back local DB only has finalized height 7 (< floor 10), no --upstream.
    let (rolled_back_provider, _, _, _) = build_tee_admission_mock_provider(
        &[(7, 1_800_000_000)],
        SeededTeeState {
            valid_until,
            has_validator_binding: true,
            validator_binding_mismatch: false,
            validator_jailed: false,
            offer_key,
        },
    );
    support::bind_context(&rolled_back_provider, &context);

    let durable_floor =
        context
            .admission_anchor()
            .unwrap()
            .map(|a| super::LocalTeeAdmissionAnchorV1 {
                finalized_height: a.block_number,
                finalized_hash: a.block_hash,
            });
    let gate = crate::tee::select_startup_lease_gate(
        &rolled_back_provider,
        7,
        (false, true, context.offer_key_ready().unwrap()),
        (durable_floor, None),
        || unreachable!("enrolled tag-2 resident must not use founder gate"),
    )
    .unwrap();
    assert_eq!(
        gate,
        super::TeeLeaseGuardGateV1::Pending {
            target: durable_floor.unwrap(),
            floor: None,
        }
    );

    let owner = LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity,
            gate,
            context: Arc::clone(&context),
        },
        outbe_tee::TeeAuthorityChannel::default(),
    )
    .unwrap();
    let protocol = crate::OutbeRuntime::new();

    // Below floor d: authority stays closed (SuspendedPolicyMismatch), Reth keeps syncing.
    let closed = owner
        .refresh(&rolled_back_provider, &protocol, &chain)
        .unwrap();
    assert_eq!(closed, TeeAuthorityState::SuspendedPolicyMismatch);
    assert!(!closed.is_authority_open());
    assert!(!owner.lock().unwrap().gate.is_armed());

    // Once local Reth catches up to height 10 with matching d.hash and Ready admission, authority arms.
    let opened = owner
        .refresh(&caught_up_provider, &protocol, &chain)
        .unwrap();
    assert!(opened.is_authority_open());
    assert!(owner.lock().unwrap().gate.is_armed());
}

#[test]
fn durable_floor_selection_and_deferred_hash_verification_enforce_i5() {
    use crate::admission_authority::{live::*, test_support as support};
    use crate::launch_tests::admission_provider::{
        build_tee_admission_mock_provider, SeededTeeState,
    };
    use outbe_tee::in_process::admission::VerifiedAdmissionAnchorV1;

    let valid_until = 1_800_001_000;
    let probe = fresh_context_with_anchor(9, None);
    let offer_key = alloy_primitives::B256::from(probe.network_encryption_public().unwrap());

    let (provider_12, chain, mut identity, hashes_12) = build_tee_admission_mock_provider(
        &[(7, 1_800_000_000), (10, 1_800_000_050), (12, 1_800_000_080)],
        SeededTeeState {
            valid_until,
            has_validator_binding: true,
            validator_binding_mismatch: false,
            validator_jailed: false,
            offer_key,
        },
    );
    let d_hash = hashes_12[1].1;
    let u_hash = hashes_12[2].1;
    let d = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 10,
        finalized_hash: d_hash,
    };
    let u_ahead = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 12,
        finalized_hash: u_hash,
    };
    let u_behind = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 7,
        finalized_hash: hashes_12[0].1,
    };
    let u_same_divergent = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 10,
        finalized_hash: alloy_primitives::B256::repeat_byte(0xFE),
    };

    // 1. Same-height u.hash != d.hash fails closed immediately.
    let err = crate::tee::select_startup_lease_gate(
        &provider_12,
        7,
        (true, true, true),
        (Some(d), Some(u_same_divergent)),
        || unreachable!(),
    )
    .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<crate::errors::LaunchError>(),
        Some(crate::errors::LaunchError::TeeAdmissionAnchorHashMismatch { height: 10, .. })
    ));

    // 2. Lagging upstream (u < d) selects target = d, floor = None.
    let lagging = crate::tee::select_startup_lease_gate(
        &provider_12,
        7,
        (true, true, true),
        (Some(d), Some(u_behind)),
        || unreachable!(),
    )
    .unwrap();
    assert_eq!(
        lagging,
        super::TeeLeaseGuardGateV1::Pending {
            target: d,
            floor: None,
        }
    );

    // 3. H_local >= d.height at startup with mismatched local hash at d fails closed synchronously.
    let wrong_d = super::LocalTeeAdmissionAnchorV1 {
        finalized_height: 10,
        finalized_hash: alloy_primitives::B256::repeat_byte(0xEF),
    };
    let sync_err = crate::tee::select_startup_lease_gate(
        &provider_12,
        12,
        (false, true, true),
        (Some(wrong_d), None),
        || unreachable!(),
    )
    .unwrap_err();
    assert!(matches!(
        sync_err.downcast_ref::<crate::errors::LaunchError>(),
        Some(crate::errors::LaunchError::TeeAdmissionAnchorHashMismatch { height: 10, .. })
    ));

    // 4. H_local < d.height < u.height selects Pending { target: u, floor: Some(d) };
    //    when local catches up to u with a divergent hash at d, deferred floor check fails closed.
    let deferred_bad = crate::tee::select_startup_lease_gate(
        &provider_12,
        7,
        (true, true, true),
        (Some(wrong_d), Some(u_ahead)),
        || unreachable!(),
    )
    .unwrap();
    assert_eq!(
        deferred_bad,
        super::TeeLeaseGuardGateV1::Pending {
            target: u_ahead,
            floor: Some(wrong_d),
        }
    );
    let context = fresh_context_with_anchor(
        9,
        Some(VerifiedAdmissionAnchorV1 {
            block_number: 10,
            block_hash: wrong_d.finalized_hash,
            state_root: alloy_primitives::B256::repeat_byte(0xBB),
            consensus_timestamp: 1_800_000_050,
        }),
    );
    support::bind_context(&provider_12, &context);
    identity.expected_enclave_id = Some(context.manifest().enclave_id().unwrap());
    let owner = LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity,
            gate: deferred_bad,
            context: Arc::clone(&context),
        },
        outbe_tee::TeeAuthorityChannel::default(),
    )
    .unwrap();
    let refresh_err = owner
        .refresh(&provider_12, &crate::OutbeRuntime::new(), &chain)
        .unwrap_err();
    assert!(matches!(
        refresh_err.downcast_ref::<crate::errors::LaunchError>(),
        Some(crate::errors::LaunchError::TeeAdmissionAnchorHashMismatch { height: 10, .. })
    ));
    assert!(!owner.lock().unwrap().gate.is_armed());
    assert!(!owner.authority.snapshot().is_authority_open());

    // 5. Same deferred gate with matching d.hash catches up and arms at height 12.
    let deferred_good = crate::tee::select_startup_lease_gate(
        &provider_12,
        7,
        (true, true, true),
        (Some(d), Some(u_ahead)),
        || unreachable!(),
    )
    .unwrap();
    let good_context = fresh_context_with_anchor(
        9,
        Some(VerifiedAdmissionAnchorV1 {
            block_number: 10,
            block_hash: d.finalized_hash,
            state_root: alloy_primitives::B256::repeat_byte(0xBB),
            consensus_timestamp: 1_800_000_050,
        }),
    );
    support::bind_context(&provider_12, &good_context);
    let good_owner = LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity,
            gate: deferred_good,
            context: good_context,
        },
        outbe_tee::TeeAuthorityChannel::default(),
    )
    .unwrap();
    assert!(good_owner
        .refresh(&provider_12, &crate::OutbeRuntime::new(), &chain)
        .unwrap()
        .is_authority_open());
    assert!(good_owner.lock().unwrap().gate.is_armed());

    // 6. Founder tag 1 (durable_floor == None): H=0 uses founder gate; H>=1 without upstream arms;
    //    with upstream uses Pending { target: u, floor: None }.
    assert_eq!(
        crate::tee::select_startup_lease_gate(
            &provider_12,
            0,
            (false, true, false),
            (None, None),
            || Ok(super::TeeLeaseGuardGateV1::Armed),
        )
        .unwrap(),
        super::TeeLeaseGuardGateV1::Armed
    );
    assert_eq!(
        crate::tee::select_startup_lease_gate(
            &provider_12,
            10,
            (false, true, true),
            (None, None),
            || unreachable!(),
        )
        .unwrap(),
        super::TeeLeaseGuardGateV1::Armed
    );
    assert_eq!(
        crate::tee::select_startup_lease_gate(
            &provider_12,
            10,
            (true, true, true),
            (None, Some(u_ahead)),
            || unreachable!(),
        )
        .unwrap(),
        super::TeeLeaseGuardGateV1::Pending {
            target: u_ahead,
            floor: None,
        }
    );

    // 7. Enrolled tag-2 resident at H_local = 0 without upstream cannot use founder bootstrap path.
    let enrolled_at_genesis = crate::tee::select_startup_lease_gate(
        &provider_12,
        0,
        (false, true, true),
        (Some(d), None),
        || unreachable!("enrolled tag-2 resident at H=0 must never call founder gate"),
    )
    .unwrap();
    assert_eq!(
        enrolled_at_genesis,
        super::TeeLeaseGuardGateV1::Pending {
            target: d,
            floor: None,
        }
    );
    let (genesis_provider, genesis_chain, mut genesis_identity) =
        support::provider(&probe, 0, 1_700_000_000, 1_800_001_000, false);
    genesis_provider.inner.accounts.lock().clear();
    let enrolled_ctx = fresh_context_with_anchor(
        9,
        Some(VerifiedAdmissionAnchorV1 {
            block_number: 10,
            block_hash: d.finalized_hash,
            state_root: alloy_primitives::B256::repeat_byte(0xBB),
            consensus_timestamp: 1_800_000_050,
        }),
    );
    genesis_identity.expected_enclave_id = Some(enrolled_ctx.manifest().enclave_id().unwrap());
    let enrolled_owner = LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity: genesis_identity,
            gate: enrolled_at_genesis,
            context: enrolled_ctx,
        },
        outbe_tee::TeeAuthorityChannel::default(),
    )
    .unwrap();
    let genesis_state = enrolled_owner
        .refresh(
            &genesis_provider,
            &crate::OutbeRuntime::new(),
            &genesis_chain,
        )
        .unwrap();
    assert_eq!(
        genesis_state,
        outbe_tee::TeeAuthorityState::SuspendedPolicyMismatch
    );
    assert!(!genesis_state.is_authority_open());
}
