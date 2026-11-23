use super::*;
use crate::admission::{TeeLeaseGuardConfigV1, TeeLeaseGuardGateV1};
use crate::admission_authority::{guard::run_lease_guard, live::*, test_support as support};
use crate::{BlockIdReader, OutbeRuntime};
use alloy_primitives::{B256, U256};
use commonware_consensus::types::Epoch;
use futures::FutureExt as _;
use outbe_consensus::finalized_admission_test_utils::FinalityCommitteeFixture;
use outbe_primitives::{
    tee_attestation_v1::AttestationOperationV1, tee_test_utils::RegistrationIntentFixture,
};
use outbe_tee::{
    dcap_protocol::DcapOnboardingContextV1,
    finalized_admission::*,
    in_process::admission::FinalizedOnboardingV1,
    node_host::{NodeHostIdentityKey, NODE_HOST_DIRECTORY_V1, NODE_HOST_IDENTITY_KEY_V1},
    resident::{ResidentOfferMaterial, ResidentTeeKeys},
    TeeAuthorityChannel, TeeAuthorityState,
};

pub(super) fn activation_at(candidate: &InProcessTeeContext) -> LifecycleCheckpointV1 {
    let proved = candidate.admission_anchor().unwrap().unwrap();
    LifecycleCheckpointV1 {
        height: proved.block_number + 1,
        hash: B256::repeat_byte(0xEA),
        state_root: B256::repeat_byte(0xEB),
        timestamp: proved.consensus_timestamp + 1,
        base_fee_per_gas: U256::from(1),
        chain_id: u64::try_from(U256::from_be_bytes(candidate.manifest().chain_id)).unwrap(),
        genesis_hash: candidate.manifest().genesis_hash,
    }
}

#[test]
fn finalized_activation_cannot_rewind_or_replace_verified_prepare_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let key = NodeHostIdentityKey::create_new(&root.path().join("host-key")).unwrap();
    let committee = FinalityCommitteeFixture::new(10);
    let (old, descriptor) = source_for_promotion(&key, &committee);
    let candidate = fresh_candidate(&old, descriptor).unwrap();
    admit_successor(&old, &candidate, &committee);
    let proved = candidate.admission_anchor().unwrap().unwrap();
    let later = activation_at(&candidate);
    assert!(validate_activation_checkpoint(&candidate, later).is_ok());
    for invalid in [
        LifecycleCheckpointV1 {
            height: proved.block_number - 1,
            ..later
        },
        LifecycleCheckpointV1 {
            height: proved.block_number,
            ..later
        },
        LifecycleCheckpointV1 {
            timestamp: proved.consensus_timestamp - 1,
            ..later
        },
    ] {
        assert!(matches!(
            validate_activation_checkpoint(&candidate, invalid),
            Err(SuccessorRuntimeError::ActivationCheckpointMismatch { .. })
        ));
    }
}

#[tokio::test]
async fn supervisor_publishes_finalized_successor_only_once() {
    use outbe_tee::lifecycle::enrollment::load_live_join_admission_anchor_v1;
    use std::os::unix::fs::DirBuilderExt as _;

    let root = tempfile::tempdir().unwrap();
    let host = root.path().join(NODE_HOST_DIRECTORY_V1);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&host)
        .unwrap();
    let host_key_path = host.join(NODE_HOST_IDENTITY_KEY_V1);
    let host_key = NodeHostIdentityKey::create_new(&host_key_path).unwrap();
    let committee = FinalityCommitteeFixture::new(10);
    let (old, descriptor) = source_for_promotion(&host_key, &committee);
    seed_resident_state(root.path(), &old);
    let candidate = fresh_candidate(&old, descriptor.clone()).unwrap();
    let attempt = admit_successor(&old, &candidate, &committee);
    let admitted = candidate.admission_anchor().unwrap().unwrap();
    let intent_hash = attempt.prepare_intent.intent_hash().unwrap();
    let phase = RenewalUpgradePhaseV1::Promoted {
        candidate_manifest_hash: candidate.manifest().authorization_hash().unwrap(),
        successor_policy_hash: attempt.prepare_intent.policy_hash,
    };
    let mut supervisor = TeeLifecycleSupervisor::new(
        "http://127.0.0.1:1",
        (
            Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([0x47; 32]).unwrap()),
            k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap(),
        ),
        (old.clone(), root.path().to_owned(), descriptor),
    )
    .unwrap();
    supervisor.successor = Some(ResidentSuccessor {
        candidate: candidate.clone(),
        attempt,
        activation: Some(activation_at(&candidate)),
        cold: false,
    });
    supervisor.phase = RenewalUpgradePhaseV1::Finalized;
    let (_, _, identity) = support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
    let owner = Arc::new(
        LiveTeeRuntimeV1::new(
            LiveTeeSnapshotV1 {
                identity,
                gate: TeeLeaseGuardGateV1::Armed,
                context: old.clone(),
            },
            TeeAuthorityChannel::default(),
        )
        .unwrap(),
    );
    let _dispatch = outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(old));
    let provider = support::provider(&candidate, 105, 1_800_000_000, 1_800_001_000, false).0;
    let protocol = Arc::new(OutbeRuntime::new());

    supervisor
        .publish_successor(&owner, &provider, &protocol)
        .unwrap();
    let persisted = load_live_join_admission_anchor_v1(root.path(), &candidate)
        .unwrap()
        .expect("first poll must durably promote the candidate");
    assert_eq!(persisted.intent_hash, intent_hash);
    assert_eq!(persisted.finalized_hash, admitted.block_hash);

    // Even an idempotent repeated persist consults the stable NodeHost key.
    let offline_key = host.join("key-offline");
    std::fs::rename(&host_key_path, &offline_key).unwrap();
    supervisor
        .publish_successor(&owner, &provider, &protocol)
        .unwrap();
    std::fs::rename(&offline_key, &host_key_path).unwrap();
    assert_eq!(
        load_live_join_admission_anchor_v1(root.path(), &candidate).unwrap(),
        Some(persisted)
    );
    assert!(supervisor.successor.is_none());
    assert_eq!(supervisor.phase, phase);
    assert!(Arc::ptr_eq(&supervisor.live, &candidate));
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &candidate));
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &candidate
    ));
}

pub(super) fn source_for_promotion(
    host_key: &NodeHostIdentityKey,
    committee: &FinalityCommitteeFixture,
) -> (Arc<InProcessTeeContext>, TrustedNetworkDescriptorV1) {
    let template = support::context(9);
    let mut manifest = template.manifest().clone();
    manifest.node_host_noise_x25519 = host_key.public();
    let descriptor = TrustedNetworkDescriptorV1 {
        network_binding: manifest.network_binding(),
        genesis_consensus_keys: committee.public_keys_min_pk(),
    };
    let chain_id = B256::from(manifest.chain_id);
    let old = Arc::new(
        InProcessTeeContext::new(
            ResidentTeeKeys::from_seed([9; 32]).unwrap(),
            *template.identity(),
            manifest,
            descriptor.clone(),
            outbe_tee::in_process::ProofSourceV1::NoHardware,
        )
        .unwrap(),
    );
    old.activate_for_test(ResidentOfferMaterial::for_test(vec![7; 96], chain_id, 0, 0).unwrap())
        .unwrap();
    (old, descriptor)
}

/// The resident state file a running node keeps in its data directory, which promotion replaces.
pub(super) fn seed_resident_state(node_data_dir: &std::path::Path, source: &InProcessTeeContext) {
    outbe_tee::node_host::ResidentStateStore::load_or_create(
        node_data_dir,
        &source.authorization_witness().unwrap(),
    )
    .unwrap();
}

pub(super) fn admit_successor(
    source: &InProcessTeeContext,
    candidate: &InProcessTeeContext,
    committee: &FinalityCommitteeFixture,
) -> SuccessorAttemptV1 {
    let prepare_intent = RegistrationIntentFixture {
        operation: AttestationOperationV1::PrepareEnclaveUpgrade,
        policy_hash: B256::repeat_byte(0x41),
        binding_id: B256::repeat_byte(0x42),
        binding_version: 2,
        registration_version: 2,
        renewal_nonce: 0,
        transition_nonce: 1,
        requested_valid_until: 1_800_001_000,
    }
    .for_manifest(candidate.manifest())
    .unwrap();
    let context = DcapOnboardingContextV1 {
        chain_id: prepare_intent.chain_id,
        genesis_hash: prepare_intent.genesis_hash,
        intent_hash: prepare_intent.intent_hash().unwrap(),
        node_id_hash: prepare_intent.node_id.node_id_hash().unwrap(),
        enclave_id: prepare_intent.enclave_id,
        binding_id: prepare_intent.binding_id,
        policy_hash: prepare_intent.policy_hash,
        recipient_x25519: prepare_intent.recipient_x25519,
        network_encryption_public: source.network_encryption_public().unwrap(),
        key_epoch: 0,
        network_encryption_epoch: 0,
    };
    let source_binding = outbe_tee::test_utils::RenewalBindingFixtureV1::new(1).build();
    let proof = upgrade_admission_proof(committee, &context, source_binding.binding_id);
    let artifact = source
        .prepare_no_attestation_dev_onboarding_artifact_v2(context)
        .unwrap();
    candidate
        .ingest_upgrade_key_v1(FinalizedOnboardingV1 {
            artifact: &artifact.onboarding_artifact,
            intent: &prepare_intent,
            authorization: &candidate.authorization_witness().unwrap(),
            anchor_outcome: &committee.outcome(Epoch::new(0)),
            committee_transitions: &[],
            admission_witness: &proof.encode_canonical().unwrap(),
            expected_network_encryption_public: context.network_encryption_public,
            expected_key_epoch: context.key_epoch,
            expected_network_encryption_epoch: context.network_encryption_epoch,
        })
        .unwrap();
    SuccessorAttemptV1 {
        prepare_intent,
        source: source_binding,
        context,
    }
}

fn upgrade_admission_proof(
    committee: &FinalityCommitteeFixture,
    context: &DcapOnboardingContextV1,
    source_binding: B256,
) -> FinalizedAdmissionWitnessV1 {
    use outbe_primitives::{
        addresses::TEE_REGISTRY_ADDRESS,
        test_utils::state_proof::{account_mpt_with_proofs, storage_trie},
    };

    let source = U256::from_be_bytes(source_binding.0);
    let slots = upgrade_registry_slots_v1(context);
    let values = [
        U256::from_be_bytes(context.network_encryption_public),
        U256::from(context.key_epoch),
        U256::from(context.network_encryption_epoch),
        U256::from_be_bytes(context.context_hash().0),
        U256::from(1_800_001_000_u64),
        source,
        source,
        U256::from_be_bytes(context.policy_hash.0),
        U256::from(1),
    ];
    let leaves = slots
        .iter()
        .zip(values)
        .map(|(slot, value)| (U256::from_be_bytes(slot.0), value))
        .collect::<Vec<_>>();
    let (storage_root, openings) = storage_trie(&leaves);
    let account = reth_trie::TrieAccount {
        nonce: 1,
        balance: U256::ZERO,
        storage_root,
        code_hash: B256::repeat_byte(0x91),
    };
    let (state_root, account_proofs) = account_mpt_with_proofs(&[(TEE_REGISTRY_ADDRESS, account)]);
    let chain_id = u64::try_from(U256::from_be_bytes(context.chain_id)).unwrap();
    outbe_consensus::proof::init_consensus_chain_id(chain_id).unwrap();
    let certified =
        committee.certify_block(Epoch::new(0), 104, 1_799_999_990, state_root, Vec::new());
    let block = outbe_consensus::committee_chain::decode_public_block(&certified.block).unwrap();
    FinalizedAdmissionWitnessV1 {
        admission: CertifiedHeaderV1 {
            finalization: certified.finalization,
            header: alloy_consensus::private::alloy_rlp::encode(block.header()),
            descendants: Vec::new(),
        },
        registry_account: MptAccountProofV1 {
            nonce: account.nonce,
            balance: account.balance,
            code_hash: account.code_hash,
            storage_root,
            nodes: account_proofs[&TEE_REGISTRY_ADDRESS]
                .iter()
                .map(|node| node.to_vec())
                .collect(),
        },
        registry_storage: slots
            .into_iter()
            .zip(values)
            .zip(openings)
            .map(|((key, value), nodes)| MptStorageProofV1 {
                key,
                value,
                nodes: nodes.into_iter().map(|node| node.to_vec()).collect(),
            })
            .collect(),
    }
}

#[tokio::test]
async fn resident_successor_updates_live_identity_only_after_durable_promotion() {
    for fail in [false, true] {
        let old = support::context(9);
        let candidate = support::context(10);
        let (provider, chain, identity) =
            support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
        let shared = support::SharedProvider(Arc::new(std::sync::Mutex::new(provider)));
        let authority = TeeAuthorityChannel::default();
        let mut watch = authority.subscribe();
        let gate = TeeLeaseGuardGateV1::Armed;
        let owner = Arc::new(
            LiveTeeRuntimeV1::new(
                LiveTeeSnapshotV1 {
                    identity,
                    gate,
                    context: old.clone(),
                },
                authority.clone(),
            )
            .unwrap(),
        );
        let notify = Arc::new(tokio::sync::Notify::new());
        let shutdown = CancellationToken::new();
        let protocol = Arc::new(OutbeRuntime::new());
        let guard = run_lease_guard(
            shared.clone(),
            protocol.clone(),
            TeeLeaseGuardConfigV1 {
                chain,
                finalization_notify: notify.clone(),
                armed: tokio::sync::watch::channel(false).0,
            },
            owner.clone(),
            shutdown.clone(),
        );
        tokio::pin!(guard);
        let old_armed = TeeAuthorityState::Armed {
            enclave_id: old.manifest().enclave_id().unwrap(),
            valid_until: 1_800_001_000,
        };
        tokio::select! {
            result = &mut guard => panic!("guard stopped: {result:?}"),
            () = support::wait_for_state(&mut watch, old_armed) => {},
        }

        let (successor, _, _) =
            support::provider(&candidate, 105, 1_800_000_000, 1_800_001_000, false);
        let head = successor.finalized_block_num_hash().unwrap().unwrap();
        *shared.0.lock().unwrap() = successor;
        notify.notify_one();
        tokio::select! {
            result = &mut guard => panic!("guard stopped before promotion: {result:?}"),
            () = support::wait_for_state(&mut watch, TeeAuthorityState::SuspendedPolicyMismatch) => {},
        }

        owner.stage_candidate(candidate.clone()).unwrap();
        let _dispatch =
            outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(old.clone()));
        let mut persisted = 0;
        let result = promote_finalized_successor(&owner, &shared, &protocol, &candidate, || {
            persisted += 1;
            if fail {
                return Err(std::io::Error::other("persist failed").into());
            }
            Ok(())
        });

        assert_eq!(persisted, 1);
        let snapshot = owner.lock().unwrap().clone();
        if fail {
            assert!(matches!(
                result,
                Err(SuccessorRuntimeError::Live(LiveTeeError::Durable(_)))
            ));
            assert_eq!(snapshot.identity, identity);
            assert_eq!(snapshot.gate, gate);
            assert!(Arc::ptr_eq(&snapshot.context, &old));
            assert!(Arc::ptr_eq(
                &outbe_tee::in_process_tee_context().unwrap(),
                &old
            ));
            assert_eq!(
                authority.snapshot(),
                TeeAuthorityState::SuspendedPolicyMismatch
            );
        } else {
            result.unwrap();
            assert!(Arc::ptr_eq(&snapshot.context, &candidate));
            assert!(Arc::ptr_eq(
                &outbe_tee::in_process_tee_context().unwrap(),
                &candidate
            ));
            assert!(owner.staged_candidate().unwrap().is_none());
            assert_eq!(
                snapshot.identity.expected_enclave_id,
                Some(candidate.manifest().enclave_id().unwrap())
            );
            assert_eq!(
                snapshot.gate.pending_anchor(),
                Some(LocalTeeAdmissionAnchorV1 {
                    finalized_height: head.number,
                    finalized_hash: head.hash,
                })
            );
            let armed = TeeAuthorityState::Armed {
                enclave_id: candidate.manifest().enclave_id().unwrap(),
                valid_until: 1_800_001_000,
            };
            support::wait_for_state(&mut watch, armed).await;
            notify.notify_one();
            assert!(guard.as_mut().now_or_never().is_none());
            assert_eq!(watch.snapshot(), armed);
            assert!(owner.lock().unwrap().gate.is_armed());
            *shared.0.lock().unwrap() =
                support::provider(&candidate, 106, 1_800_000_000, 1_800_001_000, true).0;
            notify.notify_one();
            tokio::select! {
                result = &mut guard => panic!("guard stopped after promotion: {result:?}"),
                () = support::wait_for_state(&mut watch, TeeAuthorityState::SuspendedJailed) => {},
            }
        }
        assert!(old.offer_key_ready().unwrap());
        assert!(candidate.offer_key_ready().unwrap());
        shutdown.cancel();
        assert_eq!(guard.await.unwrap(), None);
    }
}

#[test]
fn moving_finalized_head_defers_resident_successor_promotion_until_a_fresh_read() {
    let old = support::context(9);
    let candidate = support::context(10);
    let (_, _, identity) = support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
    let owner = LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity,
            gate: TeeLeaseGuardGateV1::Armed,
            context: old.clone(),
        },
        TeeAuthorityChannel::default(),
    )
    .unwrap();
    owner.stage_candidate(candidate.clone()).unwrap();
    let _dispatch =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(old.clone()));
    let (previous, _, _) = support::provider(&candidate, 105, 1_800_000_000, 1_800_001_000, false);
    let (mut advanced, _, _) =
        support::provider(&candidate, 106, 1_800_000_001, 1_800_001_000, false);
    let stale = previous.finalized_block_num_hash().unwrap().unwrap();
    let fresh = advanced.finalized_block_num_hash().unwrap().unwrap();
    advanced.inner.add_header(
        stale.hash,
        crate::HeaderProvider::header_by_number(&previous, stale.number)
            .unwrap()
            .unwrap(),
    );
    advanced
        .historical_states
        .insert(stale.hash, previous.inner);
    advanced
        .finalized_observations
        .lock()
        .unwrap()
        .extend([stale, fresh]);

    let protocol = OutbeRuntime::new();
    assert!(matches!(
        promote_finalized_successor(&owner, &advanced, &protocol, &candidate, || {
            panic!("moving finality must have no durable effects")
        }),
        Err(SuccessorRuntimeError::MovingFinality)
    ));
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &old));
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &old
    ));

    let mut persisted = 0;
    promote_finalized_successor(&owner, &advanced, &protocol, &candidate, || {
        persisted += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(persisted, 1);
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &candidate));
    assert!(owner.authority.snapshot().is_authority_open());
}

#[test]
fn restaged_candidate_cannot_be_overwritten_by_an_older_promotion() {
    let old = support::context(9);
    let candidate = support::context(10);
    let newer = support::context(11);
    let (_, _, identity) = support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
    let owner = LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity,
            gate: TeeLeaseGuardGateV1::Armed,
            context: old.clone(),
        },
        outbe_tee::TeeAuthorityChannel::default(),
    )
    .unwrap();
    owner.stage_candidate(candidate.clone()).unwrap();
    owner.stage_candidate(newer.clone()).unwrap();
    let _dispatch =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(old.clone()));
    let result = owner.promote(
        candidate,
        LocalTeeAdmissionAnchorV1 {
            finalized_height: 8,
            finalized_hash: B256::repeat_byte(8),
        },
        outbe_consensus::validators::LocalTeeRuntimeAdmissionV1::Ready {
            valid_until: 1_800_001_000,
        },
        B256::from(old.network_encryption_public().unwrap()),
        || panic!("stale candidate must not have durable effects"),
    );
    assert!(matches!(result, Err(LiveTeeError::CandidateMismatch)));
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &old));
    assert!(Arc::ptr_eq(
        &owner.staged_candidate().unwrap().unwrap(),
        &newer
    ));
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &old
    ));
}

#[test]
fn provisioned_cold_candidate_stays_cold_after_cancel_required() {
    let live = support::context(3);
    assert!(live.offer_key_ready().unwrap());
    let mut binding = outbe_tee::test_utils::RenewalBindingFixtureV1::new(1)
        .lease(0, 500, 500)
        .build();
    binding.enclave_id = live.manifest().enclave_id().unwrap();
    let owns = owns_live_binding(&live, Some(&binding), 100).unwrap();
    assert!(owns);
    // A matching binding plus a ready offer key is the warm validate_source trap.
    assert!(!cold_from_signals(owns, false));
    let root = tempfile::tempdir().unwrap();
    write_cold_marker(root.path()).unwrap();
    // CancelRequired drops the resident successor and leaves the marker in place.
    assert!(cold_recovery_marked(root.path()));
    assert!(cold_from_signals(owns, cold_recovery_marked(root.path())));
    use std::os::unix::fs::PermissionsExt as _;
    let meta = std::fs::symlink_metadata(root.path().join("tee-cold-recovery-v1")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
}

#[test]
fn complete_cold_recovery_updates_global_context_identity_and_anchor() {
    use outbe_tee::lifecycle::enrollment::load_live_join_admission_anchor_v1;
    use std::os::unix::fs::DirBuilderExt as _;

    let root = tempfile::tempdir().unwrap();
    let host = root.path().join(NODE_HOST_DIRECTORY_V1);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&host)
        .unwrap();
    let host_key = NodeHostIdentityKey::create_new(&host.join(NODE_HOST_IDENTITY_KEY_V1)).unwrap();
    let committee = FinalityCommitteeFixture::new(10);
    let (old, descriptor) = source_for_promotion(&host_key, &committee);
    seed_resident_state(root.path(), &old);
    let candidate = fresh_candidate(&old, descriptor.clone()).unwrap();
    let attempt = admit_successor(&old, &candidate, &committee);
    write_cold_marker(root.path()).unwrap();
    let mut supervisor = TeeLifecycleSupervisor::new(
        "http://127.0.0.1:1",
        (
            Arc::new(outbe_evm::OutbeEvmSigner::from_secret_bytes([0x47; 32]).unwrap()),
            k256::ecdsa::SigningKey::from_slice(&[0x31; 32]).unwrap(),
        ),
        (old.clone(), root.path().to_owned(), descriptor),
    )
    .unwrap();
    supervisor.successor = Some(ResidentSuccessor {
        candidate: candidate.clone(),
        attempt,
        activation: Some(activation_at(&candidate)),
        cold: true,
    });
    let (_, _, identity) = support::provider(&old, 7, 1_800_000_000, 1_800_001_000, false);
    let owner = LiveTeeRuntimeV1::new(
        LiveTeeSnapshotV1 {
            identity,
            gate: TeeLeaseGuardGateV1::Armed,
            context: old.clone(),
        },
        TeeAuthorityChannel::default(),
    )
    .unwrap();
    let _dispatch =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(old.clone()));

    let anchor = supervisor
        .complete_cold_recovery(Some(&owner))
        .expect("cold recovery");
    let snapshot = owner.lock().unwrap();
    assert!(Arc::ptr_eq(&snapshot.context, &candidate));
    assert_eq!(
        snapshot.identity.expected_enclave_id,
        Some(candidate.manifest().enclave_id().unwrap())
    );
    assert_eq!(
        snapshot.gate,
        TeeLeaseGuardGateV1::Pending {
            target: anchor,
            floor: None,
        }
    );
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &candidate
    ));
    assert!(!cold_recovery_marked(root.path()));
    assert!(supervisor.successor.is_none());
    assert!(Arc::ptr_eq(&supervisor.live, &candidate));
    let persisted = load_live_join_admission_anchor_v1(root.path(), &candidate)
        .unwrap()
        .expect("anchor persisted");
    let activation = activation_at(&candidate);
    assert_eq!(anchor.finalized_hash, activation.hash);
    assert_eq!(anchor.finalized_height, activation.height);
    assert_eq!(
        persisted.finalized_hash,
        candidate.admission_anchor().unwrap().unwrap().block_hash
    );
}
