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

struct SeededTeeState {
    valid_until: u64,
    has_validator_binding: bool,
    validator_binding_mismatch: bool,
    validator_jailed: bool,
    offer_key: alloy_primitives::B256,
}

#[derive(Clone)]
struct FinalizedMockProvider {
    inner: reth_provider::test_utils::MockEthProvider<outbe_primitives::OutbePrimitives>,
    finalized: Option<alloy_eips::BlockNumHash>,
    historical_states: std::collections::HashMap<
        alloy_primitives::B256,
        reth_provider::test_utils::MockEthProvider<outbe_primitives::OutbePrimitives>,
    >,
    fail_state_read: bool,
}

impl reth_provider::BlockHashReader for FinalizedMockProvider {
    fn block_hash(
        &self,
        number: u64,
    ) -> reth_provider::ProviderResult<Option<alloy_primitives::B256>> {
        self.inner.block_hash(number)
    }

    fn canonical_hashes_range(
        &self,
        start: u64,
        end: u64,
    ) -> reth_provider::ProviderResult<Vec<alloy_primitives::B256>> {
        self.inner.canonical_hashes_range(start, end)
    }
}

impl reth_provider::BlockNumReader for FinalizedMockProvider {
    fn chain_info(&self) -> reth_provider::ProviderResult<reth_chainspec::ChainInfo> {
        self.inner.chain_info()
    }

    fn best_block_number(&self) -> reth_provider::ProviderResult<u64> {
        self.inner.best_block_number()
    }

    fn last_block_number(&self) -> reth_provider::ProviderResult<u64> {
        self.inner.last_block_number()
    }

    fn block_number(
        &self,
        hash: alloy_primitives::B256,
    ) -> reth_provider::ProviderResult<Option<u64>> {
        self.inner.block_number(hash)
    }
}

impl reth_provider::BlockIdReader for FinalizedMockProvider {
    fn pending_block_num_hash(
        &self,
    ) -> reth_provider::ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(None)
    }

    fn safe_block_num_hash(
        &self,
    ) -> reth_provider::ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(self.finalized)
    }

    fn finalized_block_num_hash(
        &self,
    ) -> reth_provider::ProviderResult<Option<alloy_eips::BlockNumHash>> {
        Ok(self.finalized)
    }
}

impl reth_provider::HeaderProvider for FinalizedMockProvider {
    type Header = OutbeHeader;

    fn header(
        &self,
        block_hash: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<Option<Self::Header>> {
        self.inner.header(block_hash)
    }

    fn header_by_number(&self, number: u64) -> reth_provider::ProviderResult<Option<Self::Header>> {
        self.inner.header_by_number(number)
    }

    fn headers_range(
        &self,
        range: impl std::ops::RangeBounds<alloy_primitives::BlockNumber>,
    ) -> reth_provider::ProviderResult<Vec<Self::Header>> {
        self.inner.headers_range(range)
    }

    fn sealed_header(
        &self,
        number: alloy_primitives::BlockNumber,
    ) -> reth_provider::ProviderResult<Option<reth_primitives_traits::SealedHeader<Self::Header>>>
    {
        self.inner.sealed_header(number)
    }

    fn sealed_headers_while(
        &self,
        range: impl std::ops::RangeBounds<alloy_primitives::BlockNumber>,
        predicate: impl FnMut(&reth_primitives_traits::SealedHeader<Self::Header>) -> bool,
    ) -> reth_provider::ProviderResult<Vec<reth_primitives_traits::SealedHeader<Self::Header>>>
    {
        self.inner.sealed_headers_while(range, predicate)
    }
}

impl reth_provider::StateProviderFactory for FinalizedMockProvider {
    type Primitives = <reth_provider::test_utils::MockEthProvider<
        outbe_primitives::OutbePrimitives,
    > as reth_provider::StateProviderFactory>::Primitives;

    fn latest(&self) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.latest()
    }

    fn state_with_block_appended(
        &self,
        parent_hash: alloy_primitives::BlockHash,
        block: reth_chain_state::ExecutedBlock<Self::Primitives>,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.state_with_block_appended(parent_hash, block)
    }

    fn state_by_block_number_or_tag(
        &self,
        number_or_tag: alloy_eips::BlockNumberOrTag,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.state_by_block_number_or_tag(number_or_tag)
    }

    fn history_by_block_number(
        &self,
        block: alloy_primitives::BlockNumber,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.history_by_block_number(block)
    }

    fn history_by_block_hash(
        &self,
        block: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.history_by_block_hash(block)
    }

    fn state_by_block_hash(
        &self,
        block: alloy_primitives::BlockHash,
    ) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        if self.fail_state_read {
            return Err(reth_provider::ProviderError::BestBlockNotFound);
        }
        if let Some(historical) = self.historical_states.get(&block) {
            return historical.state_by_block_hash(block);
        }
        self.inner.state_by_block_hash(block)
    }

    fn pending(&self) -> reth_provider::ProviderResult<reth_provider::StateProviderBox> {
        self.inner.pending()
    }

    fn pending_state_by_hash(
        &self,
        block_hash: alloy_primitives::B256,
    ) -> reth_provider::ProviderResult<Option<reth_provider::StateProviderBox>> {
        self.inner.pending_state_by_hash(block_hash)
    }

    fn maybe_pending(
        &self,
    ) -> reth_provider::ProviderResult<Option<reth_provider::StateProviderBox>> {
        self.inner.maybe_pending()
    }
}

#[allow(clippy::type_complexity)]
fn build_tee_admission_mock_provider(
    blocks: &[(u64, u64)],
    state_cfg: SeededTeeState,
) -> (
    FinalizedMockProvider,
    outbe_node::tee_remote_session::RegistryChainIdentity,
    outbe_engine::validators::LocalTeeRuntimeIdentityV1,
    Vec<(u64, alloy_primitives::B256)>,
) {
    use alloy_consensus::Header;
    use alloy_primitives::{Address, B256, U256};
    use commonware_codec::Encode as _;
    use commonware_cryptography::{bls12381, Signer as _};
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};
    use outbe_primitives::tee_attestation_v1::NodeIdV1;
    use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};
    use std::collections::HashMap;

    let chain_id = 676_u64;
    let genesis_hash = B256::repeat_byte(0x11);
    let validator = Address::with_last_byte(0x31);
    let owner = Address::repeat_byte(0xA0);
    let reth_p2p_public: [u8; 33] = k256::ecdsa::SigningKey::from_bytes((&[0x31; 32]).into())
        .unwrap()
        .verifying_key()
        .to_encoded_point(true)
        .as_bytes()
        .try_into()
        .unwrap();
    let node_hash = NodeIdV1 { reth_p2p_public }.node_id_hash().unwrap();
    let enclave_id = B256::repeat_byte(0x32);
    let binding_id = B256::repeat_byte(0x33);

    let mut storage_provider = HashMapStorageProvider::new(chain_id);
    storage_provider.set_block_number(blocks.last().map_or(1, |(n, _)| *n));
    StorageHandle::enter(&mut storage_provider, |storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(owner).unwrap();
        vs.config_is_initialized.write(true).unwrap();
        vs.set_config_max_validators(128).unwrap();
        let key = bls12381::PrivateKey::from_seed(31);
        let encoded = key.public_key().encode();
        let mut pubkey = [0u8; 48];
        pubkey.copy_from_slice(&encoded[..48]);
        let minimum = U256::from(1_000u64);
        vs.test_seed_authoritative_min_stake(minimum).unwrap();
        vs.register_validator(owner, validator, &pubkey).unwrap();
        vs.test_seed_authoritative_bonded(validator, minimum)
            .unwrap();
        vs.activate_validator_via_boundary_for_test(validator)
            .unwrap();
        if state_cfg.validator_jailed {
            vs.jail_validator_for_tee_expiry(validator).unwrap();
        }

        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        registry
            .v1_node_enclave_id
            .write(&node_hash, enclave_id)
            .unwrap();
        registry
            .v1_node_binding_id
            .write(&node_hash, binding_id)
            .unwrap();
        registry
            .v1_node_intent_hash
            .write(&node_hash, B256::repeat_byte(0x34))
            .unwrap();
        registry
            .v1_node_valid_until
            .write(&node_hash, state_cfg.valid_until)
            .unwrap();
        if !state_cfg.offer_key.is_zero() {
            registry
                .tribute_offer_public_key
                .write(state_cfg.offer_key)
                .unwrap();
        }
        if state_cfg.has_validator_binding {
            let bound_node = if state_cfg.validator_binding_mismatch {
                let other_node = B256::repeat_byte(0x99);
                registry
                    .v1_node_enclave_id
                    .write(&other_node, enclave_id)
                    .unwrap();
                registry
                    .v1_node_binding_id
                    .write(&other_node, binding_id)
                    .unwrap();
                registry
                    .v1_node_intent_hash
                    .write(&other_node, B256::repeat_byte(0x34))
                    .unwrap();
                registry
                    .v1_node_valid_until
                    .write(&other_node, state_cfg.valid_until)
                    .unwrap();
                other_node
            } else {
                node_hash
            };
            registry
                .validator_v1_node_hash
                .write(&validator, bound_node)
                .unwrap();
        }
    });

    let provider = MockEthProvider::<outbe_primitives::OutbePrimitives>::new();
    let mut by_address: HashMap<Address, Vec<(B256, U256)>> = HashMap::new();
    for ((address, slot), value) in storage_provider.storage {
        by_address
            .entry(address)
            .or_default()
            .push((B256::from(slot.to_be_bytes()), value));
    }
    for (address, slots) in by_address {
        provider.add_account(
            address,
            ExtendedAccount::new(0, U256::ZERO).extend_storage(slots),
        );
    }

    let mut hashes = Vec::with_capacity(blocks.len());
    for &(number, timestamp) in blocks {
        let header = OutbeHeader::new(Header {
            number,
            timestamp,
            ..Default::default()
        });
        let hash = alloy_consensus::Sealable::hash_slow(&header);
        provider.add_header(hash, header);
        hashes.push((number, hash));
    }

    let finalized = hashes
        .last()
        .map(|&(number, hash)| alloy_eips::BlockNumHash::new(number, hash));

    (
        FinalizedMockProvider {
            inner: provider,
            finalized,
            historical_states: HashMap::new(),
            fail_state_read: false,
        },
        outbe_node::tee_remote_session::RegistryChainIdentity {
            chain_id,
            genesis_hash,
        },
        outbe_engine::validators::LocalTeeRuntimeIdentityV1 {
            reth_p2p_public,
            expected_enclave_id: Some(enclave_id),
            validator: Some(validator),
        },
        hashes,
    )
}

#[test]
fn validator_gate_stays_pending_after_tee_join_until_validator_binding_and_offer_key_exist() {
    use outbe_engine::validators::LocalTeeRuntimeAdmissionV1;

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
    let mut anchors = crate::launch::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(join_anchor),
        durable: None,
    };

    let admission_at_join =
        crate::launch::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            &provider_at_join,
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
        crate::launch::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            &provider_at_stake,
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
    let mut dual_anchors = crate::launch::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(stake_anchor),
        durable: Some(join_anchor),
    };
    let admission_after_restart =
        crate::launch::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            &provider_at_stake,
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
    use outbe_engine::validators::{LocalTeeRuntimeAdmissionV1, LocalTeeRuntimeRejectionV1};

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
    let mut anchors = crate::launch::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(join_anchor),
        durable: None,
    };
    let jailed_verdict =
        crate::launch::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            &jailed_provider,
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
        crate::launch::admission::TeeLeaseGuardConfigV1 {
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
    let mut mismatch_anchors = crate::launch::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
            finalized_height: 7,
            finalized_hash: mismatch_hashes[0].1,
        }),
        durable: None,
    };
    assert_eq!(
        crate::launch::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            &mismatch_provider,
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
        crate::launch::admission::TeeLeaseGuardConfigV1 {
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
    let mut offer_anchors = crate::launch::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
            finalized_height: 7,
            finalized_hash: ready_hashes[0].1,
        }),
        durable: None,
    };
    let wrong_offer = alloy_primitives::B256::repeat_byte(0xEE);
    let offer_err =
        crate::launch::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            &ready_provider,
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
    let mut expired_anchors = crate::launch::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
            finalized_height: 7,
            finalized_hash: expired_hashes[0].1,
        }),
        durable: None,
    };
    assert_eq!(
        crate::launch::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            &expired_now_provider,
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
        crate::launch::admission::TeeLeaseGuardConfigV1 {
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
    let mut failing_anchors = crate::launch::admission::TeeLeaseGuardAnchorsV1 {
        gate: super::TeeLeaseGuardGateV1::Pending(super::LocalTeeAdmissionAnchorV1 {
            finalized_height: 7,
            finalized_hash: ready_hashes[0].1,
        }),
        durable: None,
    };
    assert!(
        crate::launch::admission::read_gated_finalized_local_tee_admission_with_resident_offer(
            &failing_provider,
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
        crate::launch::admission::TeeLeaseGuardConfigV1 {
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
