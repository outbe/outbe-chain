use alloy_primitives::{Address, B256};
use commonware_cryptography::{bls12381, Signer as _};
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use outbe_coderegistry::RepoId;
use outbe_radicle::{
    endpoint::{sign_response, EndpointAddress, EndpointFrame, EndpointResponseBody, PeerId},
    integration::{
        ObservedRepositoryStatus, RadicleMetrics, RadicleRepositoryState, RadicleStatusChannel,
        RadicleVotingGate, SignedEndpointEvidence,
    },
    manager::{
        BoxFuture, FinalizedBlock, FinalizedSnapshot, FinalizedValidator, ManagerError,
        ManagerPhase, ManagerStatus, RepositoryStatus,
    },
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

fn block(number: u64) -> FinalizedBlock {
    FinalizedBlock {
        number,
        hash: B256::with_last_byte(number as u8),
    }
}

fn evidence(
    signer: &bls12381::PrivateKey,
    validator: Address,
    node_id: [u8; 32],
    anchor: FinalizedBlock,
    valid_until: u64,
) -> SignedEndpointEvidence {
    let response = sign_response(
        EndpointResponseBody {
            request_id: [9_u8; 32],
            chain_id: 54_322_345,
            genesis_hash: B256::repeat_byte(0xaa),
            validator,
            node_id,
            addresses: vec![EndpointAddress::dns("peer.example.com", 8776).unwrap()],
            anchor_number: anchor.number,
            anchor_hash: anchor.hash,
            valid_until,
        },
        signer,
    )
    .unwrap();
    SignedEndpointEvidence {
        peer: PeerId::from_public_key(&signer.public_key()),
        encoded_frame: EndpointFrame::Response(Box::new(response.clone())).encode(),
        response,
    }
}

#[test]
fn immutable_status_distinguishes_repository_states() {
    let validator = Address::repeat_byte(0x11);
    let node_id = [1_u8; 32];
    let (publisher, handle) = RadicleStatusChannel::enabled(validator, node_id);
    let available = RepoId::repeat_byte(0x21);
    let pending = RepoId::repeat_byte(0x22);
    publisher.observe_snapshot(&FinalizedSnapshot {
        block: block(12),
        validators: vec![],
        registry_generation: 2,
        repositories: vec![available, pending],
    });
    publisher.observe_repository(available, true);
    publisher.observe_repository(pending, false);
    publisher.observe_manager(ManagerStatus {
        phase: ManagerPhase::JoiningUnbound,
        last_seen_finalized: Some(block(12)),
        last_converged_finalized: Some(block(12)),
        desired_repository_count: 2,
        observed_repository_count: 2,
        available_repository_count: 1,
        pending_repository_count: 1,
        ..ManagerStatus::default()
    });

    let snapshot = handle.snapshot();
    assert_eq!(snapshot.local_node_id, Some(node_id));
    assert_eq!(snapshot.voting_gate, RadicleVotingGate::Verifier);
    assert_eq!(snapshot.manager.observed_repository_count, 2);
    assert_eq!(snapshot.repositories.len(), 2);
    assert_eq!(
        snapshot.repositories[0].state,
        RadicleRepositoryState::Available
    );
    assert_eq!(
        snapshot.repositories[1].state,
        RadicleRepositoryState::Pending
    );

    // Subsequent snapshot observation preserves existing Available state, drops
    // removed repositories, and initializes newly added ones to Pending.
    let added = RepoId::repeat_byte(0x23);
    publisher.observe_snapshot(&FinalizedSnapshot {
        block: block(13),
        validators: vec![],
        registry_generation: 3,
        repositories: vec![available, added],
    });
    let next_snapshot = handle.snapshot();
    assert_eq!(next_snapshot.repositories.len(), 2);
    assert_eq!(next_snapshot.repositories[0].repo_id, available);
    assert_eq!(
        next_snapshot.repositories[0].state,
        RadicleRepositoryState::Available
    );
    assert_eq!(next_snapshot.repositories[1].repo_id, added);
    assert_eq!(
        next_snapshot.repositories[1].state,
        RadicleRepositoryState::Pending
    );

    // The handle is an immutable snapshot reader. No UDS/TCP/network dependency
    // is reachable from this API.
    let cloned = handle.clone();
    assert_eq!(cloned.snapshot(), next_snapshot);
}

#[test]
fn metrics_use_frozen_names_and_known_height_bits() {
    let validator = Address::repeat_byte(0x11);
    let (publisher, handle) = RadicleStatusChannel::enabled(validator, [1_u8; 32]);
    let status_initial = ManagerStatus {
        phase: ManagerPhase::Ready,
        last_seen_finalized: Some(block(0)),
        last_converged_finalized: Some(block(0)),
        resolved_peer_count: 3,
        connected_peer_count: 2,
        desired_repository_count: 5,
        observed_repository_count: 3,
        provider_failures: 4,
        uds_failures: 5,
        ..ManagerStatus::default()
    };
    publisher.observe_manager(status_initial.clone());

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let mut metrics = RadicleMetrics::default();
    metrics::with_local_recorder(&recorder, || {
        // Record the same snapshot twice to ensure cumulative failure totals are not double-counted.
        metrics.record(&handle.snapshot());
        metrics.record(&handle.snapshot());
    });
    let values = snapshotter.snapshot().into_vec();
    let find = |name: &str| {
        values
            .iter()
            .find(|(key, _, _, _)| key.key().name() == name)
            .unwrap_or_else(|| panic!("missing metric {name}"))
    };
    let assert_gauge = |name: &str, expected: f64| match &find(name).3 {
        DebugValue::Gauge(value) => assert_eq!(value.into_inner(), expected, "metric {name}"),
        other => panic!("expected gauge for {name}, got {other:?}"),
    };
    let assert_counter = |name: &str, expected: u64| match &find(name).3 {
        DebugValue::Counter(value) => assert_eq!(*value, expected, "metric {name}"),
        other => panic!("expected counter for {name}, got {other:?}"),
    };

    assert_gauge("outbe_radicle_phase", 2.0);
    assert_gauge("outbe_radicle_last_seen_finalized_known", 1.0);
    assert_gauge("outbe_radicle_last_seen_finalized_height", 0.0);
    assert_gauge("outbe_radicle_last_converged_finalized_known", 1.0);
    assert_gauge("outbe_radicle_last_converged_finalized_height", 0.0);
    assert_gauge("outbe_radicle_resolved_peers", 3.0);
    assert_gauge("outbe_radicle_connected_peers", 2.0);
    assert_gauge("outbe_radicle_desired_repositories", 5.0);
    assert_gauge("outbe_radicle_observed_repositories", 3.0);
    assert_counter("outbe_radicle_provider_failures_total", 4);
    assert_counter("outbe_radicle_uds_failures_total", 5);

    // Advance one failure total and verify only the delta is added.
    publisher.observe_manager(ManagerStatus {
        provider_failures: 7,
        ..status_initial
    });
    metrics::with_local_recorder(&recorder, || {
        metrics.record(&handle.snapshot());
    });
    let values_after = snapshotter.snapshot().into_vec();
    let find_after = |name: &str| {
        values_after
            .iter()
            .find(|(key, _, _, _)| key.key().name() == name)
            .unwrap_or_else(|| panic!("missing metric {name}"))
    };
    assert_eq!(
        find_after("outbe_radicle_provider_failures_total").3,
        DebugValue::Counter(3)
    );
    assert_eq!(
        find_after("outbe_radicle_uds_failures_total").3,
        DebugValue::Counter(0)
    );
}

#[test]
fn disabled_status_has_no_identity_or_progress() {
    let handle = RadicleStatusChannel::disabled();
    let status = handle.snapshot();
    assert_eq!(status.manager.phase, ManagerPhase::Disabled);
    assert_eq!(status.local_node_id, None);
    assert_eq!(status.manager.last_seen_finalized, None);
    assert_eq!(status.manager.last_converged_finalized, None);
    assert!(status.repositories.is_empty());
    assert!(status.signed_peers.is_empty());
}

#[test]
fn disabled_metrics() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        let mut metrics = RadicleMetrics::default();
        metrics.record(&RadicleStatusChannel::disabled().snapshot());
    });
    let values = snapshotter.snapshot().into_vec();
    let gauge = |name: &str| {
        values
            .iter()
            .find(|(key, _, _, _)| key.key().name() == name)
            .unwrap_or_else(|| panic!("missing metric {name}"))
    };
    for name in [
        "outbe_radicle_phase",
        "outbe_radicle_last_seen_finalized_known",
        "outbe_radicle_last_converged_finalized_known",
    ] {
        match &gauge(name).3 {
            DebugValue::Gauge(value) => assert_eq!(value.into_inner(), 0.0),
            other => panic!("expected gauge, got {other:?}"),
        }
    }
    assert!(values
        .iter()
        .all(|(key, _, _, _)| key.key().name() != "outbe_radicle_identity"));
}

#[test]
fn snapshot_prunes_evidence() {
    let local = Address::repeat_byte(0x11);
    let expired = Address::repeat_byte(0x22);
    let changed = Address::repeat_byte(0x23);
    let expired_signer = bls12381::PrivateKey::from_seed(22);
    let changed_signer = bls12381::PrivateKey::from_seed(23);
    let (publisher, handle) = RadicleStatusChannel::enabled(local, [1_u8; 32]);
    let at_10 = FinalizedSnapshot {
        block: block(10),
        validators: vec![
            FinalizedValidator {
                address: expired,
                peer: PeerId::from_public_key(&expired_signer.public_key()),
                node_id: Some([2_u8; 32]),
            },
            FinalizedValidator {
                address: changed,
                peer: PeerId::from_public_key(&changed_signer.public_key()),
                node_id: Some([3_u8; 32]),
            },
        ],
        registry_generation: 1,
        repositories: vec![],
    };
    publisher.observe_snapshot(&at_10);
    publisher.observe_evidence(vec![
        evidence(&expired_signer, expired, [2_u8; 32], at_10.block, 11),
        evidence(&changed_signer, changed, [3_u8; 32], at_10.block, 20),
    ]);
    assert_eq!(handle.snapshot().signed_peers.len(), 2);

    let mut at_11 = at_10.clone();
    at_11.block = block(11);
    at_11.validators[1].node_id = Some([4_u8; 32]);
    publisher.observe_snapshot(&at_11);
    assert!(handle.snapshot().signed_peers.is_empty());

    publisher.observe_evidence(vec![
        evidence(&expired_signer, expired, [2_u8; 32], at_10.block, 11),
        evidence(&changed_signer, changed, [3_u8; 32], at_10.block, 20),
    ]);
    assert!(handle.snapshot().signed_peers.is_empty());
}

struct RepositorySequence(Mutex<VecDeque<Result<bool, ManagerError>>>);

impl RepositoryStatus for RepositorySequence {
    fn available(&self, _repo: RepoId) -> BoxFuture<'_, Result<bool, ManagerError>> {
        let result = self.0.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { result })
    }
}

#[tokio::test]
async fn repo_error_is_pending() {
    let repo = RepoId::repeat_byte(0x31);
    let (publisher, handle) = RadicleStatusChannel::enabled(Address::repeat_byte(0x11), [1_u8; 32]);
    publisher.observe_snapshot(&FinalizedSnapshot {
        block: block(1),
        validators: vec![],
        registry_generation: 1,
        repositories: vec![repo],
    });
    let observed = ObservedRepositoryStatus::new(
        Arc::new(RepositorySequence(Mutex::new(VecDeque::from([
            Ok(true),
            Err(ManagerError::Status("offline".into())),
        ])))),
        publisher,
    );

    assert!(observed.available(repo).await.unwrap());
    assert_eq!(
        handle.snapshot().repositories[0].state,
        RadicleRepositoryState::Available
    );
    assert!(observed.available(repo).await.is_err());
    assert_eq!(
        handle.snapshot().repositories[0].state,
        RadicleRepositoryState::Pending
    );
}
