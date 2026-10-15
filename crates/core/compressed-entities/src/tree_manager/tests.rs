use super::*;
use outbe_primitives::time::WorldwideDay;

use crate::{
    persistence::{EnvironmentIdentity, LOCAL_STORAGE_SCHEMA_VERSION},
    sealed_root, CeTopologyV1, Commitment, EntityRef, FinalLeafMutation, WwdEntityId,
    ACTIVE_COMMITMENT_SCHEME,
};

fn b256(last: u8) -> B256 {
    let mut bytes = [0_u8; 32];
    bytes[31] = last;
    B256::from(bytes)
}

fn environment() -> EnvironmentIdentity {
    EnvironmentIdentity {
        local_storage_schema_version: LOCAL_STORAGE_SCHEMA_VERSION,
        chain_id: 8080,
        genesis_hash: b256(1),
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        topology: CeTopologyV1.encode(),
        tree_format: "ckb-smt-v0.6.1-poseidon2-catalog-v3".to_owned(),
        vendor_revision: "ad555350c866b2265d87d2d7fbd146fbc918bfe5".to_owned(),
    }
}

fn genesis() -> FinalizedMarker {
    FinalizedMarker {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        height: 0,
        block_hash: environment().genesis_hash,
        parent_block_hash: B256::ZERO,
        parent_root: B256::ZERO,
        new_root: sealed_root(B256::ZERO).unwrap(),
    }
}

fn service_with_lease_timeout(
    directory: &std::path::Path,
    lease_timeout: std::time::Duration,
) -> CompressedTreeService {
    let db = CeMdbx::open(directory, environment(), genesis()).unwrap();
    // These are test fixture bounds, not production defaults.
    CompressedTreeService::new_with_export_lease_timeout(
        db,
        CandidateCacheLimits {
            max_candidates: 4,
            max_encoded_bytes: 1_000_000,
        },
        lease_timeout,
    )
    .unwrap()
}

fn service(directory: &std::path::Path) -> CompressedTreeService {
    service_with_lease_timeout(directory, std::time::Duration::from_secs(1))
}

#[test]
fn ocomp_lease_configuration_rejects_a_timeout_above_the_poc_bound() {
    let directory = tempfile::tempdir().unwrap();
    let db = CeMdbx::open(directory.path(), environment(), genesis()).unwrap();
    assert!(matches!(
        CompressedTreeService::new_with_export_lease_timeout(
            db,
            CandidateCacheLimits {
                max_candidates: 4,
                max_encoded_bytes: 1_000_000,
            },
            DEFAULT_EXPORT_LEASE_OPEN_TIMEOUT + std::time::Duration::from_nanos(1),
        ),
        Err(TreeServiceError::InvalidExportLeaseTimeout)
    ));
}

fn genesis_identity() -> ExactParentIdentity {
    ExactParentIdentity {
        commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
        block_number: 0,
        block_hash: genesis().block_hash,
        root: genesis().new_root,
    }
}

const CHILD_ROOT: &str = "OUTBE_CE_TEST_ROOT";
const CHILD_OFFER: &str = "OUTBE_CE_TEST_OFFER";
const CHILD_ACK: &str = "OUTBE_CE_TEST_ACK";
const CHILD_RELEASE: &str = "OUTBE_CE_TEST_RELEASE";

struct ReadOnlyExporterChild {
    child: std::process::Child,
    acknowledgement_path: std::path::PathBuf,
    release_path: std::path::PathBuf,
}

impl ReadOnlyExporterChild {
    fn wait_for_ack(&mut self) -> ExportLeaseOpenAck {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(bytes) = std::fs::read(&self.acknowledgement_path) {
                return ExportLeaseOpenAck::decode_fixed(&bytes).unwrap();
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("read-only exporter exited before acknowledgement: {status}");
            }
            assert!(
                std::time::Instant::now() < deadline,
                "read-only exporter acknowledgement timed out"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn release_and_wait(mut self) {
        std::fs::write(&self.release_path, []).unwrap();
        let status = self.child.wait().unwrap();
        assert!(status.success(), "read-only exporter failed: {status}");
    }
}

fn spawn_read_only_exporter(
    directory: &std::path::Path,
    offer: ExportLeaseOffer,
) -> ReadOnlyExporterChild {
    let control = directory.join(format!(
        "lease-{}-{:#x}",
        offer.generation(),
        offer.challenge
    ));
    std::fs::create_dir_all(&control).unwrap();
    let offer_path = control.join("offer");
    let acknowledgement_path = control.join("ack");
    let release_path = control.join("release");
    std::fs::write(&offer_path, offer.encode_fixed()).unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tree_manager::tests::ce_read_only_exporter_child",
            "--nocapture",
        ])
        .env(CHILD_ROOT, directory)
        .env(CHILD_OFFER, &offer_path)
        .env(CHILD_ACK, &acknowledgement_path)
        .env(CHILD_RELEASE, &release_path)
        .spawn()
        .unwrap();
    ReadOnlyExporterChild {
        child,
        acknowledgement_path,
        release_path,
    }
}

#[test]
fn ce_read_only_exporter_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT) else {
        return;
    };
    let offer_path = std::env::var_os(CHILD_OFFER).unwrap();
    let acknowledgement_path = std::env::var_os(CHILD_ACK).unwrap();
    let release_path = std::env::var_os(CHILD_RELEASE).unwrap();
    let offer = ExportLeaseOffer::decode_fixed(&std::fs::read(offer_path).unwrap()).unwrap();
    let read_only =
        crate::CeMdbxReadOnly::open(std::path::Path::new(&root), environment()).unwrap();
    let view = read_only.open_exact(offer.identity()).unwrap();
    let catalog_root = view.catalog_root();
    let acknowledgement = offer.confirm_open(&view).unwrap();
    // Publish atomically so the parent cannot read an empty or partial acknowledgement.
    let pending_ack = std::path::Path::new(&acknowledgement_path).with_extension("tmp");
    std::fs::write(&pending_ack, acknowledgement.encode_fixed()).unwrap();
    std::fs::rename(pending_ack, acknowledgement_path).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !std::path::Path::new(&release_path).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "parent did not release read-only exporter"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(view.catalog_root(), catalog_root);
}

#[test]
fn exact_parent_identity_is_checked_before_any_tree_read() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    assert!(service.open_parent(genesis_identity()).is_ok());

    for wrong in [
        ExactParentIdentity {
            block_hash: b256(99),
            ..genesis_identity()
        },
        ExactParentIdentity {
            root: b256(99),
            ..genesis_identity()
        },
        ExactParentIdentity {
            block_number: 9,
            ..genesis_identity()
        },
    ] {
        assert!(matches!(
            service.open_parent(wrong),
            Err(TreeServiceError::ParentView(_))
        ));
    }
}

#[test]
fn exact_parent_identity_and_read_root_mismatches_are_corruption_not_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    let wrong = ExactParentIdentity {
        block_hash: b256(99),
        ..genesis_identity()
    };

    let factory: &dyn AuthenticatedParentTreeFactory = &service;
    assert!(matches!(
        factory.open_parent(wrong),
        Err(PrecompileError::Fatal(_))
    ));

    let parent = factory.open_parent(genesis_identity()).unwrap();
    let entity = EntityRef::Tribute(WwdEntityId::from_day_and_digest(
        WorldwideDay::new(7),
        [3_u8; 32],
    ));
    assert!(matches!(
        parent.read_leaf_verified(entity, b256(88)),
        Err(PrecompileError::TreeUnavailable(_))
    ));
}

#[test]
fn parent_proof_candidate_and_finalized_reopen_form_one_authenticated_flow() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    let parent = service.open_parent(genesis_identity()).unwrap();
    let entity = EntityRef::Tribute(WwdEntityId::from_day_and_digest(
        WorldwideDay::new(7),
        [3_u8; 32],
    ));
    let commitment = Commitment::try_from(b256(17).0).unwrap();

    assert_eq!(
        parent
            .read_leaf_verified(entity, genesis_identity().root)
            .unwrap(),
        None
    );
    let provisional = parent
        .prepare_seal(
            1,
            &[FinalLeafMutation {
                entity,
                final_leaf: Some(commitment),
            }],
            &[],
        )
        .unwrap();
    let block_hash = b256(2);
    assert_eq!(
        service
            .publish_candidate(block_hash, provisional.clone())
            .unwrap(),
        PublicationOutcome::Published
    );
    assert_eq!(
        service.publish_candidate(block_hash, provisional).unwrap(),
        PublicationOutcome::AlreadyPublished
    );

    let staged = service.candidate(1, block_hash).unwrap().unwrap();
    assert_eq!(staged.parent_root(), genesis().new_root);
    let new_root = staged.new_root();
    assert_eq!(
        service.apply_finalized(1, block_hash, new_root).unwrap(),
        FinalizedCandidateOutcome::Applied(staged.marker(ACTIVE_COMMITMENT_SCHEME))
    );
    assert_eq!(service.retention_height(), 1);
    assert!(service.candidate(1, block_hash).unwrap().is_none());

    let reopened = service
        .open_parent(ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 1,
            block_hash,
            root: new_root,
        })
        .unwrap();
    assert_eq!(
        reopened.read_leaf_verified(entity, new_root).unwrap(),
        Some(commitment)
    );
}

#[test]
fn one_candidate_atomically_seals_and_reopens_changes_from_multiple_shards() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    let identity = WwdEntityId::try_from(
        hex::decode("000000010405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    let tribute = EntityRef::Tribute(identity);
    let bucket = EntityRef::NodBucket(identity);
    let tribute_leaf = Commitment::try_from(b256(11).0).unwrap();
    let bucket_leaf = Commitment::try_from(b256(12).0).unwrap();

    let parent = service.open_parent(genesis_identity()).unwrap();
    let provisional = parent
        .prepare_seal(
            1,
            &[
                FinalLeafMutation {
                    entity: tribute,
                    final_leaf: Some(tribute_leaf),
                },
                FinalLeafMutation {
                    entity: bucket,
                    final_leaf: Some(bucket_leaf),
                },
            ],
            &[],
        )
        .unwrap();
    assert_eq!(provisional.changed_shard_count(), 2);
    assert_eq!(provisional.changed_collections.len(), 2);
    assert!(provisional.changed_collections.values().all(|collection| {
        let collection = collection.mutation().expect("mutation operation");
        collection.shard_set.parent_shard_roots.len()
            == crate::CeDomain::try_from(collection.domain_id)
                .unwrap()
                .shard_count() as usize
            && collection.shard_set.new_shard_roots.len()
                == crate::CeDomain::try_from(collection.domain_id)
                    .unwrap()
                    .shard_count() as usize
    }));

    let block_hash = b256(91);
    let new_root = provisional.new_root();
    service.publish_candidate(block_hash, provisional).unwrap();
    service.apply_finalized(1, block_hash, new_root).unwrap();

    let reopened = service
        .open_parent(ExactParentIdentity {
            commitment_scheme_version: ACTIVE_COMMITMENT_SCHEME,
            block_number: 1,
            block_hash,
            root: new_root,
        })
        .unwrap();
    assert_eq!(
        reopened.read_leaf_verified(tribute, new_root).unwrap(),
        Some(tribute_leaf)
    );
    assert_eq!(
        reopened.read_leaf_verified(bucket, new_root).unwrap(),
        Some(bucket_leaf)
    );
}

#[test]
fn completed_finalization_is_idempotent_after_candidate_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    let parent = service.open_parent(genesis_identity()).unwrap();
    let provisional = parent.prepare_seal(1, &[], &[]).unwrap();
    let block_hash = b256(2);
    service.publish_candidate(block_hash, provisional).unwrap();

    let root = service
        .candidate(1, block_hash)
        .unwrap()
        .unwrap()
        .new_root();

    assert!(matches!(
        service.apply_finalized(1, block_hash, root).unwrap(),
        FinalizedCandidateOutcome::Applied(_)
    ));
    assert!(matches!(
        service.apply_finalized(1, block_hash, root).unwrap(),
        FinalizedCandidateOutcome::AlreadyApplied(_)
    ));
    assert_eq!(service.retention_height(), 1);
    assert!(matches!(
        service.apply_finalized(2, b256(3), b256(4)),
        Err(TreeServiceError::CandidateMissing { .. })
    ));
}

#[test]
fn authoritative_root_mismatch_cannot_mutate_mdbx_or_drop_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    let parent = service.open_parent(genesis_identity()).unwrap();
    let provisional = parent.prepare_seal(1, &[], &[]).unwrap();
    let block_hash = b256(2);
    service.publish_candidate(block_hash, provisional).unwrap();

    assert!(matches!(
        service.apply_finalized(1, block_hash, b256(77)),
        Err(TreeServiceError::AuthoritativeRootMismatch { .. })
    ));
    assert_eq!(service.finalized_marker().unwrap(), genesis());
    assert_eq!(service.retention_height(), 0);
    assert!(service.candidate(1, block_hash).unwrap().is_some());
}

#[test]
fn late_payload_rejection_discards_only_the_exact_candidate() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    let first_hash = b256(2);
    let competing_hash = b256(3);
    for hash in [first_hash, competing_hash] {
        let provisional = service
            .open_parent(genesis_identity())
            .unwrap()
            .prepare_seal(1, &[], &[])
            .unwrap();
        service.publish_candidate(hash, provisional).unwrap();
    }

    assert!(service.discard_candidate(1, first_hash).unwrap());
    assert!(service.candidate(1, first_hash).unwrap().is_none());
    assert!(service.candidate(1, competing_hash).unwrap().is_some());
    assert_eq!(service.finalized_marker().unwrap(), genesis());
}

#[test]
fn stale_or_wrong_parent_candidate_is_rejected_before_cache_publication() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    let mut wrong_parent = service
        .open_parent(genesis_identity())
        .unwrap()
        .prepare_seal(1, &[], &[])
        .unwrap();
    wrong_parent.parent_block_hash = b256(44);
    assert!(matches!(
        service.publish_candidate(b256(2), wrong_parent),
        Err(TreeServiceError::NonContiguousPublication { .. })
    ));
    assert!(service.candidate(1, b256(2)).unwrap().is_none());
}

#[test]
fn ocm_exporter_opens_one_exact_read_only_snapshot_before_the_writer_advances() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    let offer = service.arm_finalized_export(genesis(), b256(70)).unwrap();
    let mut exporter = spawn_read_only_exporter(directory.path(), offer);
    let opened = exporter.wait_for_ack();
    service.acknowledge_export_open(opened).unwrap();

    let entity = EntityRef::Tribute(WwdEntityId::from_day_and_digest(
        WorldwideDay::new(7),
        [0x44_u8; 32],
    ));
    let commitment = Commitment::try_from(b256(17).0).unwrap();
    let provisional = service
        .open_parent(genesis_identity())
        .unwrap()
        .prepare_seal(
            1,
            &[FinalLeafMutation {
                entity,
                final_leaf: Some(commitment),
            }],
            &[],
        )
        .unwrap();
    let block_hash = b256(2);
    let new_root = provisional.new_root();
    service.publish_candidate(block_hash, provisional).unwrap();
    service.apply_finalized(1, block_hash, new_root).unwrap();

    assert_ne!(
        service.finalized_marker().unwrap().new_root,
        genesis().new_root
    );
    exporter.release_and_wait();
}

#[test]
fn ocm_export_offer_bytes_cannot_acknowledge_an_unopened_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());
    let offer = service.arm_finalized_export(genesis(), b256(71)).unwrap();

    assert!(matches!(
        service.acknowledge_export_open_bytes(&offer.encode_fixed()),
        Err(TreeServiceError::MalformedExportLease)
    ));
    assert_eq!(
        service.export_lease_status(offer.generation()).unwrap(),
        ExportLeaseStatus::Pending
    );

    let mut exporter = spawn_read_only_exporter(directory.path(), offer);
    let opened = exporter.wait_for_ack();
    service
        .acknowledge_export_open_bytes(&opened.encode_fixed())
        .unwrap();
    service
        .acknowledge_export_open_bytes(&opened.encode_fixed())
        .expect("lost acknowledgement response must replay exactly");
    assert_eq!(
        service.export_lease_status(offer.generation()).unwrap(),
        ExportLeaseStatus::Opened
    );
    exporter.release_and_wait();
}

#[test]
fn ocm_lease_restart_challenge_rejects_an_old_ack_at_a_reused_generation() {
    let directory = tempfile::tempdir().unwrap();
    let (old_offer, old_ack) = {
        let service = service(directory.path());
        let offer = service.arm_finalized_export(genesis(), b256(80)).unwrap();
        let mut exporter = spawn_read_only_exporter(directory.path(), offer);
        let ack = exporter.wait_for_ack();
        service.acknowledge_export_open(ack).unwrap();
        exporter.release_and_wait();
        (offer, ack)
    };

    let restarted = service(directory.path());
    let fresh = restarted.arm_finalized_export(genesis(), b256(81)).unwrap();
    assert_eq!(fresh.generation(), old_offer.generation());
    assert_ne!(fresh.encode_fixed(), old_offer.encode_fixed());
    assert!(matches!(
        restarted.acknowledge_export_open(old_ack),
        Err(TreeServiceError::ExportLeaseChallengeMismatch)
    ));
    assert_eq!(
        restarted.export_lease_status(fresh.generation()).unwrap(),
        ExportLeaseStatus::Pending
    );

    let mut exporter = spawn_read_only_exporter(directory.path(), fresh);
    let fresh_ack = exporter.wait_for_ack();
    restarted.acknowledge_export_open(fresh_ack).unwrap();
    exporter.release_and_wait();
}

#[test]
fn ocm_lease_generation_rejects_stale_ack_and_marker_races() {
    let directory = tempfile::tempdir().unwrap();
    let service = service(directory.path());

    let first = service.arm_finalized_export(genesis(), b256(72)).unwrap();
    let mut first_exporter = spawn_read_only_exporter(directory.path(), first);
    let first_ack = first_exporter.wait_for_ack();
    service.acknowledge_export_open(first_ack).unwrap();
    first_exporter.release_and_wait();

    let provisional = service
        .open_parent(genesis_identity())
        .unwrap()
        .prepare_seal(1, &[], &[])
        .unwrap();
    let block_hash = b256(2);
    let root = provisional.new_root();
    service.publish_candidate(block_hash, provisional).unwrap();
    let marker = service
        .apply_finalized(1, block_hash, root)
        .unwrap()
        .marker();

    assert!(matches!(
        service.arm_finalized_export(genesis(), b256(73)),
        Err(TreeServiceError::ExportLeaseMarkerAdvanced { .. })
    ));

    let second = service.arm_finalized_export(marker, b256(74)).unwrap();
    assert!(second.generation() > first.generation());
    assert!(matches!(
        service.acknowledge_export_open(first_ack),
        Err(TreeServiceError::StaleExportLease { .. })
    ));
    let mut second_exporter = spawn_read_only_exporter(directory.path(), second);
    let second_ack = second_exporter.wait_for_ack();
    service.acknowledge_export_open(second_ack).unwrap();
    second_exporter.release_and_wait();
    assert_eq!(
        service.export_lease_status(second.generation()).unwrap(),
        ExportLeaseStatus::Opened
    );
}

#[test]
fn ocm_lease_timeout_is_bounded_and_the_next_finalized_apply_continues() {
    let directory = tempfile::tempdir().unwrap();
    let timeout = std::time::Duration::from_millis(25);
    let service = service_with_lease_timeout(directory.path(), timeout);

    let provisional = service
        .open_parent(genesis_identity())
        .unwrap()
        .prepare_seal(1, &[], &[])
        .unwrap();
    let block_hash = b256(2);
    let root = provisional.new_root();
    service.publish_candidate(block_hash, provisional).unwrap();
    let offer = service.arm_finalized_export(genesis(), b256(75)).unwrap();

    let started = std::time::Instant::now();
    service.apply_finalized(1, block_hash, root).unwrap();
    let elapsed = started.elapsed();

    assert!(elapsed >= timeout);
    assert!(elapsed < std::time::Duration::from_secs(1));
    assert_eq!(
        service.export_lease_status(offer.generation()).unwrap(),
        ExportLeaseStatus::TimedOut
    );
    assert_eq!(service.finalized_marker().unwrap().height, 1);
}
