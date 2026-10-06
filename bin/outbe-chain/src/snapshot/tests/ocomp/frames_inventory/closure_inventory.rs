use super::*;
use crate::snapshot::validation::ocomp::verify_closure;
use outbe_ocomp::discovery_spool::ContiguousCheckpointStoreV1;
use outbe_primitives::projection::ProjectionCheckpoint;
use std::path::{Path, PathBuf};

type Observed = (
    ProjectionCheckpoint,
    ProjectionCheckpoint,
    ProjectionCheckpoint,
    u64,
    u64,
);

fn point(fixture: &Fixture, number: u64) -> ProjectionCheckpoint {
    let view = RethReadOnlyView::open(&fixture.layout).unwrap();
    ProjectionCheckpoint {
        block_number: number,
        block_hash: view.header(number).unwrap().unwrap().hash_slow(),
    }
}

fn store(fixture: &Fixture, advances: &[ProjectionCheckpoint]) -> PathBuf {
    let root = fixture.root.path().join("closure-checkpoint-v1");
    let baseline = ProjectionCheckpoint {
        block_number: 0,
        block_hash: fixture.layout.chain.genesis_hash(),
    };
    let writer = ContiguousCheckpointStoreV1::open(&root, baseline).unwrap();
    let mut previous = baseline;
    for next in advances {
        writer.compare_and_advance_to(previous, *next).unwrap();
        previous = *next;
    }
    drop(writer);
    root
}

fn check(
    fixture: &Fixture,
    root: &Path,
    projection: Option<ProjectionCheckpoint>,
    maximum_transactions: Option<u64>,
) -> eyre::Result<Observed> {
    let before = fingerprint(fixture.root.path());
    let result = {
        let view = RethReadOnlyView::open(&fixture.layout).unwrap();
        verify_closure(&view, root, projection, maximum_transactions).map(|audit| {
            (
                audit.checkpoint.baseline,
                audit.checkpoint.previous,
                audit.checkpoint.current,
                audit.replay.blocks,
                audit.replay.transactions,
            )
        })
    };
    assert_eq!(fingerprint(fixture.root.path()), before);
    result
}

#[test]
fn baseline_only_closure_allows_no_projection_and_requires_every_replay_frame() {
    let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
    let baseline = point(&fixture, 0);
    let root = store(&fixture, &[]);
    assert_eq!(
        check(&fixture, &root, None, Some(3)).unwrap(),
        (baseline, baseline, baseline, 3, 3)
    );
    let error = check(&fixture, &root, None, Some(2)).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");

    let missing = Fixture::new(Receipts::Mdbx, Fault::MissingReceipt);
    let root = store(&missing, &[]);
    let error = check(&missing, &root, None, None).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
}

#[test]
fn sparse_closure_below_finalized_accepts_equal_or_later_projection_and_replays_suffix() {
    let fixture = Fixture::new(Receipts::Static, Fault::None);
    let baseline = point(&fixture, 0);
    let closed = point(&fixture, 2);
    let root = store(&fixture, &[closed]);
    for projected in [closed, point(&fixture, 3)] {
        assert_eq!(
            check(&fixture, &root, Some(projected), Some(1)).unwrap(),
            (baseline, baseline, closed, 1, 1)
        );
    }
    let error = check(&fixture, &root, Some(closed), Some(0)).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
}

#[test]
fn sparse_previous_is_its_stored_height_not_current_minus_one() {
    let fixture = Fixture::new(Receipts::Mdbx, Fault::MissingHeader);
    let baseline = point(&fixture, 0);
    let previous = point(&fixture, 1);
    let closed = point(&fixture, 3);
    let root = store(&fixture, &[previous, closed]);
    // Header2 is absent, but it is neither stored previous1 nor current3.
    // Chain-wide retained-header validation is a separate audit.
    assert_eq!(
        check(&fixture, &root, Some(closed), Some(0)).unwrap(),
        (baseline, previous, closed, 0, 0)
    );
}

#[test]
fn closure_requires_projection_at_or_beyond_current_with_equal_height_hash_match() {
    let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
    let closed = point(&fixture, 2);
    let root = store(&fixture, &[closed]);
    let wrong_hash = ProjectionCheckpoint {
        block_hash: B256::repeat_byte(0xee),
        ..closed
    };
    for projected in [None, Some(point(&fixture, 1)), Some(wrong_hash)] {
        let error = check(&fixture, &root, projected, None).unwrap_err();
        assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    }
}

#[test]
fn baseline_previous_and_current_hash_conflicts_are_failed() {
    for corrupted in ["baseline", "previous", "current"] {
        let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
        let mut baseline = point(&fixture, 0);
        let mut previous = point(&fixture, 1);
        let mut current = point(&fixture, 3);
        match corrupted {
            "baseline" => baseline.block_hash = B256::repeat_byte(0xee),
            "previous" => previous.block_hash = B256::repeat_byte(0xee),
            "current" => current.block_hash = B256::repeat_byte(0xee),
            _ => unreachable!(),
        }
        let root = fixture.root.path().join("closure-checkpoint-v1");
        let writer = ContiguousCheckpointStoreV1::open(&root, baseline).unwrap();
        writer.compare_and_advance_to(baseline, previous).unwrap();
        writer.compare_and_advance_to(previous, current).unwrap();
        drop(writer);
        let error = check(&fixture, &root, Some(current), None).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_none(),
            "{corrupted}: {error:#}"
        );
    }
}

#[test]
fn missing_stored_baseline_previous_or_current_header_is_incomplete() {
    for missing in [0_u64, 1, 2] {
        let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
        let previous = point(&fixture, 1);
        let current = point(&fixture, 2);
        let root = store(&fixture, &[previous, current]);
        // Fixture mutation finishes before opening the read-only snapshot.
        let db = init_db(
            fixture.layout.chain_root.join("db"),
            DatabaseArguments::test(),
        )
        .unwrap();
        let tx = db.tx_mut().unwrap();
        assert!(tx
            .delete::<tables::Headers<OutbeHeader>>(missing, None)
            .unwrap());
        tx.commit().unwrap();
        drop(db);
        let error = check(&fixture, &root, Some(current), None).unwrap_err();
        assert!(
            error.downcast_ref::<Incomplete>().is_some(),
            "header{missing}: {error:#}"
        );
    }
}

#[test]
fn absent_checkpoint_is_incomplete_without_initializing_store_and_corruption_is_failed() {
    let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
    let root = fixture.root.path().join("closure-checkpoint-v1");
    let error = check(&fixture, &root, None, None).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    assert!(!root.exists());
    let root = store(&fixture, &[]);
    let path = root.join("checkpoint.v1");
    let mut bytes = fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&path, bytes).unwrap();
    let error = check(&fixture, &root, None, None).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_none(), "{error:#}");
    fs::remove_file(&path).unwrap();
    let error = check(&fixture, &root, None, None).unwrap_err();
    assert!(error.downcast_ref::<Incomplete>().is_some(), "{error:#}");
    assert!(!path.exists());
}

#[test]
fn closure_ahead_of_finalized_keeps_independent_frontier_and_avoids_max_height_overflow() {
    for number in [4_u64, u64::MAX] {
        let fixture = Fixture::new(Receipts::Mdbx, Fault::None);
        let baseline = point(&fixture, 0);
        let header = OutbeHeader::new(Header {
            number,
            parent_hash: point(&fixture, 3).block_hash,
            ..Default::default()
        });
        let closed = ProjectionCheckpoint {
            block_number: number,
            block_hash: header.hash_slow(),
        };
        // Install only the named canonical checkpoint identity above H=3.
        // This isolated audit does not impose equality of independent frontiers.
        let db = init_db(
            fixture.layout.chain_root.join("db"),
            DatabaseArguments::test(),
        )
        .unwrap();
        let tx = db.tx_mut().unwrap();
        tx.put::<tables::Headers<OutbeHeader>>(number, header)
            .unwrap();
        tx.put::<tables::CanonicalHeaders>(number, closed.block_hash)
            .unwrap();
        tx.commit().unwrap();
        drop(db);
        let root = store(&fixture, &[closed]);
        assert_eq!(
            check(&fixture, &root, Some(closed), Some(0)).unwrap(),
            (baseline, baseline, closed, 0, 0)
        );
    }
}
