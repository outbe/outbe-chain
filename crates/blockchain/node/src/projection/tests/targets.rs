use super::*;

#[test]
fn finalized_targets_coalesce_to_the_latest_height() {
    let first = FinalizedTarget::new(10, B256::repeat_byte(1));
    let second = FinalizedTarget::new(12, B256::repeat_byte(2));
    let mut latest = Some(first);
    let mut pending = Some(first);

    assert!(record_finalized_target(&mut latest, &mut pending, second).unwrap());
    assert_eq!(latest, Some(second));
    assert_eq!(pending, Some(second));
}

#[test]
fn finalized_target_regression_is_rejected() {
    let current = FinalizedTarget::new(10, B256::repeat_byte(1));
    let mut latest = Some(current);
    let mut pending = Some(current);

    let error = record_finalized_target(
        &mut latest,
        &mut pending,
        FinalizedTarget::new(9, B256::repeat_byte(2)),
    )
    .unwrap_err();

    assert!(error.to_string().contains("regressed"));
    assert_eq!(latest, Some(current));
    assert_eq!(pending, Some(current));
}

#[test]
fn conflicting_hash_at_same_finalized_height_is_rejected() {
    let current = FinalizedTarget::new(10, B256::repeat_byte(1));
    let mut latest = Some(current);
    let mut pending = Some(current);

    let error = record_finalized_target(
        &mut latest,
        &mut pending,
        FinalizedTarget::new(10, B256::repeat_byte(2)),
    )
    .unwrap_err();

    assert!(error.to_string().contains("hash changed"));
    assert_eq!(latest, Some(current));
    assert_eq!(pending, Some(current));
}

#[test]
fn unchanged_finalized_target_is_retryable_only_while_pending() {
    let current = FinalizedTarget::new(10, B256::repeat_byte(1));
    let mut latest = Some(current);
    let mut pending = Some(current);

    assert!(record_finalized_target(&mut latest, &mut pending, current).unwrap());
    assert_eq!(pending, Some(current));

    pending = None;
    assert!(!record_finalized_target(&mut latest, &mut pending, current).unwrap());

    assert_eq!(latest, Some(current));
    assert_eq!(pending, None);
}

#[test]
fn finalized_target_conflict_publishes_fatal_exit_on_every_ingress_path() {
    let current = FinalizedTarget::new(10, B256::repeat_byte(1));
    let mut latest = Some(current);
    let mut pending = None;
    let checkpoint = ProjectionCheckpoint {
        block_number: current.number,
        block_hash: current.hash,
    };
    let (publisher, readiness) = projection_readiness(
        checkpoint,
        outbe_offchain_data::ProjectionStatus::Ready { checkpoint },
    );
    let (exit_tx, mut exit_rx) = tokio::sync::mpsc::unbounded_channel();

    assert_eq!(
        record_or_publish_finalized_target(
            &mut latest,
            &mut pending,
            FinalizedTarget::new(10, B256::repeat_byte(2)),
            &publisher,
            &exit_tx,
        ),
        super::FinalizedTargetDisposition::Rejected
    );
    assert!(matches!(
        readiness.current(),
        outbe_offchain_data::ProjectionStatus::Fatal { error, .. }
            if error.class == ProjectionFailureClass::CheckpointMismatch
    ));
    assert_eq!(
        exit_rx.try_recv().unwrap().failure.class,
        ProjectionFailureClass::CheckpointMismatch,
    );
}
