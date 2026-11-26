use super::super::tests::activation_at;
use super::*;

fn planted_cold() -> (
    tempfile::TempDir,
    TeeLifecycleSupervisor,
    LiveTeeRuntimeV1,
    Arc<InProcessTeeContext>,
    Vec<u8>,
    Vec<u8>,
    outbe_tee::in_process_dispatch::InProcessTestGuard,
) {
    let (root, host_key) = host_tree();
    let committee = FinalityCommitteeFixture::new(6);
    let (old, descriptor) = source_for_promotion(&host_key, &committee);
    let _ = outbe_tee::node_host::ResidentStateStore::load_or_create(
        root.path(),
        &old.authorization_witness().unwrap(),
    )
    .unwrap();
    let old_attempt = admit_successor(&old, &old, &committee);
    let verified = old.admission_anchor().unwrap().unwrap();
    let intent = &old_attempt.prepare_intent;
    persist_live_join_admission_anchor_v1(
        root.path(),
        &old,
        outbe_tee::FinalizedJoinAdmissionAnchorV1 {
            chain_id: intent.chain_id,
            genesis_hash: intent.genesis_hash,
            node_id_hash: intent.node_id.node_id_hash().unwrap(),
            enclave_id: intent.enclave_id,
            intent_hash: intent.intent_hash().unwrap(),
            finalized_height: verified.block_number,
            finalized_hash: verified.block_hash,
            finalized_state_root: verified.state_root,
            finalized_consensus_timestamp: verified.consensus_timestamp,
        },
    )
    .unwrap();
    let candidate = fresh_candidate(&old, descriptor.clone()).unwrap();
    let attempt = admit_successor(&old, &candidate, &committee);
    write_cold_marker(root.path()).unwrap();
    let mut supervisor = TeeLifecycleSupervisor::new(
        "http://127.0.0.1:1",
        operator(),
        (old.clone(), root.path().to_owned(), descriptor),
    )
    .unwrap();
    supervisor.successor = Some(ResidentSuccessor {
        activation: Some(activation_at(&candidate)),
        candidate,
        attempt,
        cold: true,
    });
    let owner = armed_owner(&old);
    let dispatch =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(old.clone()));
    let anchor = std::fs::read(root.path().join("tee-live-join-admission-v1/anchor.bin")).unwrap();
    let marker = std::fs::read(root.path().join("tee-cold-recovery-v1")).unwrap();
    (root, supervisor, owner, old, anchor, marker, dispatch)
}

fn prior_state_unchanged(
    root: &std::path::Path,
    supervisor: &TeeLifecycleSupervisor,
    kept: (&LiveTeeRuntimeV1, &Arc<InProcessTeeContext>, &[u8], &[u8]),
) {
    let (owner, old, anchor, marker) = kept;
    assert_eq!(
        std::fs::read(root.join("tee-live-join-admission-v1/anchor.bin")).unwrap(),
        anchor
    );
    assert_eq!(
        std::fs::read(root.join("tee-cold-recovery-v1")).unwrap(),
        marker
    );
    assert!(cold_recovery_marked(root));
    let snapshot = owner.lock().unwrap();
    assert!(Arc::ptr_eq(&snapshot.context, old));
    assert_eq!(snapshot.gate, TeeLeaseGuardGateV1::Armed);
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        old
    ));
    assert!(Arc::ptr_eq(&supervisor.live, old));
    assert!(supervisor.successor.as_ref().unwrap().cold);
}

#[test]
fn complete_cold_recovery_lock_poison_keeps_the_prior_anchor() {
    let (root, mut supervisor, owner, old, anchor, marker, _dispatch) = planted_cold();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = owner.lock().unwrap();
        panic!("poison owner");
    }));
    let error = supervisor.complete_cold_recovery(Some(&owner)).unwrap_err();
    assert!(matches!(
        error,
        SuccessorRuntimeError::Live(LiveTeeError::LockPoisoned)
    ));
    assert_eq!(
        std::fs::read(root.path().join("tee-live-join-admission-v1/anchor.bin")).unwrap(),
        anchor
    );
    assert_eq!(
        std::fs::read(root.path().join("tee-cold-recovery-v1")).unwrap(),
        marker
    );
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &old
    ));
    assert!(Arc::ptr_eq(&supervisor.live, &old));
    assert!(supervisor.successor.as_ref().unwrap().cold);
}

#[test]
fn complete_cold_recovery_anchor_conflict_restores_marker_and_prior_anchor() {
    let (root, mut supervisor, owner, old, anchor, marker, _dispatch) = planted_cold();
    let before = owner.lock().unwrap().clone();
    let error = supervisor.complete_cold_recovery(Some(&owner)).unwrap_err();
    assert!(
        matches!(
            error,
            SuccessorRuntimeError::Anchor(LiveJoinAnchorError::ConflictingAnchor)
        ),
        "{error:?}"
    );
    prior_state_unchanged(root.path(), &supervisor, (&owner, &old, &anchor, &marker));
    let after = owner.lock().unwrap();
    assert_eq!(after.identity, before.identity);
    assert_eq!(after.gate, before.gate);
    assert!(Arc::ptr_eq(&after.context, &before.context));
}

#[test]
fn directory_marker_rejects_cold_recovery_before_any_publish() {
    let (root, host_key) = host_tree();
    let committee = FinalityCommitteeFixture::new(5);
    let (old, descriptor) = source_for_promotion(&host_key, &committee);
    outbe_tee::node_host::ResidentStateStore::load_or_create(
        root.path(),
        &old.authorization_witness().unwrap(),
    )
    .unwrap();
    let candidate = fresh_candidate(&old, descriptor.clone()).unwrap();
    let attempt = admit_successor(&old, &candidate, &committee);
    std::fs::create_dir(root.path().join("tee-cold-recovery-v1")).unwrap();
    let mut supervisor = TeeLifecycleSupervisor::new(
        "http://127.0.0.1:1",
        operator(),
        (old.clone(), root.path().to_owned(), descriptor),
    )
    .unwrap();
    supervisor.successor = Some(ResidentSuccessor {
        candidate: candidate.clone(),
        attempt,
        activation: Some(activation_at(&candidate)),
        cold: true,
    });
    let owner = armed_owner(&old);
    let _dispatch =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(old.clone()));
    let error = supervisor.complete_cold_recovery(Some(&owner)).unwrap_err();
    let SuccessorRuntimeError::Marker(io) = error else {
        panic!("expected marker rejection")
    };
    assert_eq!(io.kind(), std::io::ErrorKind::InvalidInput);
    assert!(supervisor.successor.is_some());
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &old));
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &old
    ));
    assert!(!root
        .path()
        .join("tee-live-join-admission-v1/anchor.bin")
        .exists());
}

fn rewind_prior_anchor(root: &std::path::Path, enclave_id: [u8; 32], height: u64, timestamp: u64) {
    let path = root.join("tee-live-join-admission-v1/anchor.bin");
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 241);
    bytes[97..129].copy_from_slice(&enclave_id);
    bytes[161..169].copy_from_slice(&height.to_be_bytes());
    bytes[233..241].copy_from_slice(&timestamp.to_be_bytes());
    std::fs::write(&path, &bytes).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(&path, perms).unwrap();
}

fn candidate_verified(supervisor: &TeeLifecycleSupervisor) -> (u64, u64, B256) {
    let verified = supervisor
        .successor
        .as_ref()
        .unwrap()
        .candidate
        .admission_anchor()
        .unwrap()
        .unwrap();
    (
        verified.block_number,
        verified.consensus_timestamp,
        verified.block_hash,
    )
}

#[test]
fn complete_cold_recovery_overwrites_anchor_from_a_previous_enclave() {
    let (root, mut supervisor, owner, old, _anchor, _marker, _dispatch) = planted_cold();
    let (height, timestamp, hash) = candidate_verified(&supervisor);
    assert!(height > 1);
    rewind_prior_anchor(root.path(), [0xee; 32], height - 1, timestamp);
    let anchor = supervisor
        .complete_cold_recovery(Some(&owner))
        .expect("prior enclave anchor is overwritten");
    let candidate = supervisor.live.clone();
    assert!(!Arc::ptr_eq(&candidate, &old));
    assert_ne!(candidate.manifest().enclave_id().unwrap().0, [0xee; 32]);
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &candidate
    ));
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &candidate));
    assert_eq!(
        owner.lock().unwrap().gate,
        TeeLeaseGuardGateV1::Pending {
            target: anchor,
            floor: None,
        }
    );
    assert!(matches!(
        supervisor.phase,
        RenewalUpgradePhaseV1::Promoted { .. }
    ));
    assert!(supervisor.successor.is_none());
    assert!(!cold_recovery_marked(root.path()));
    let persisted = outbe_tee::lifecycle::enrollment::load_live_join_admission_anchor_v1(
        root.path(),
        &candidate,
    )
    .unwrap()
    .expect("candidate anchor");
    assert_ne!(persisted.enclave_id.0, [0xee; 32]);
    assert_eq!(
        persisted.enclave_id,
        candidate.manifest().enclave_id().unwrap()
    );
    assert_eq!(anchor.finalized_hash, activation_at(&candidate).hash);
    assert_eq!(persisted.finalized_hash, hash);
}

#[test]
fn foreign_in_process_context_rejects_cold_recovery_before_disk() {
    let (root, mut supervisor, owner, old, anchor, marker, dispatch) = planted_cold();
    drop(dispatch);
    let foreign = supervisor.successor.as_ref().unwrap().candidate.clone();
    let _dispatch =
        outbe_tee::in_process_dispatch::set_in_process_tee_context_for_test(Some(foreign.clone()));
    let error = supervisor.complete_cold_recovery(Some(&owner)).unwrap_err();
    assert!(matches!(
        error,
        SuccessorRuntimeError::Dispatch(outbe_tee::InProcessDispatchError::ContextChanged)
    ));
    assert_eq!(
        std::fs::read(root.path().join("tee-live-join-admission-v1/anchor.bin")).unwrap(),
        anchor
    );
    assert_eq!(
        std::fs::read(root.path().join("tee-cold-recovery-v1")).unwrap(),
        marker
    );
    assert!(cold_recovery_marked(root.path()));
    assert!(Arc::ptr_eq(&owner.lock().unwrap().context, &old));
    assert_eq!(owner.lock().unwrap().gate, TeeLeaseGuardGateV1::Armed);
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &foreign
    ));
    assert!(Arc::ptr_eq(&supervisor.live, &old));
    assert!(supervisor.successor.as_ref().unwrap().cold);
}

#[tokio::test]
async fn clear_cold_marker_failure_keeps_switched_memory_and_marker_until_the_next_tick() {
    let (root, mut supervisor, _owner, _old, _anchor, marker, _dispatch) = planted_cold();
    let (height, timestamp, hash) = candidate_verified(&supervisor);
    assert!(height > 1);
    let candidate = supervisor.successor.as_ref().unwrap().candidate.clone();
    rewind_prior_anchor(root.path(), [0xab; 32], height - 1, timestamp);
    let fail_clear = FailClearColdMarkerGuard::arm(root.path());
    let prepared = supervisor
        .prepare(&tokio_util::sync::CancellationToken::new())
        .await
        .expect("pre-sync keeps a committed cold promotion");
    let prepared = prepared.expect("finalized cold successor publishes the anchor");
    assert_eq!(prepared.finalized_hash, activation_at(&candidate).hash);
    assert_eq!(
        std::fs::read(root.path().join("tee-cold-recovery-v1")).unwrap(),
        marker
    );
    assert!(cold_recovery_marked(root.path()));
    assert!(Arc::ptr_eq(
        &outbe_tee::in_process_tee_context().unwrap(),
        &candidate
    ));
    assert!(Arc::ptr_eq(&supervisor.live, &candidate));
    assert!(supervisor.successor.is_none());
    assert!(matches!(
        supervisor.phase,
        RenewalUpgradePhaseV1::Promoted { .. }
    ));
    let persisted = outbe_tee::lifecycle::enrollment::load_live_join_admission_anchor_v1(
        root.path(),
        &candidate,
    )
    .unwrap()
    .expect("anchor.bin switched with memory before marker clear failed");
    assert_eq!(
        persisted.enclave_id,
        candidate.manifest().enclave_id().unwrap()
    );
    assert_eq!(persisted.finalized_hash, hash);
    drop(fail_clear);
    let _ = super::super::super::driver::LifecycleTask::tick(&mut supervisor).await;
    assert!(!cold_recovery_marked(root.path()));
}
