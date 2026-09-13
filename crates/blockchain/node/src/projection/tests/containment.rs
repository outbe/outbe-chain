use super::*;

#[test]
fn ocomp_projection_containment_accepts_exact_and_ahead_but_reports_behind() {
    let required = checkpoint(10, 0x10);
    let finalized = FinalizedTarget::new(20, B256::repeat_byte(0x20));

    let behind = checkpoint(9, 0x09);
    assert_eq!(
        evaluate_ocomp_projection_containment(
            behind,
            required,
            finalized,
            Some(behind.block_hash),
            Some(required.block_hash),
        )
        .unwrap(),
        OcompProjectionContainment::Behind {
            checkpoint: behind,
            required,
        }
    );

    for contained in [required, checkpoint(15, 0x15)] {
        assert_eq!(
            evaluate_ocomp_projection_containment(
                contained,
                required,
                finalized,
                Some(contained.block_hash),
                Some(required.block_hash),
            )
            .unwrap(),
            OcompProjectionContainment::Contains {
                checkpoint: contained,
                required,
            }
        );
    }
}

#[test]
fn ocomp_projection_containment_rejects_unfinalized_or_conflicting_history() {
    let required = checkpoint(10, 0x10);
    let projection = checkpoint(15, 0x15);
    let finalized = FinalizedTarget::new(20, B256::repeat_byte(0x20));

    assert!(evaluate_ocomp_projection_containment(
        projection,
        required,
        finalized,
        Some(B256::repeat_byte(0xEE)),
        Some(required.block_hash),
    )
    .is_err());
    assert!(evaluate_ocomp_projection_containment(
        projection,
        required,
        finalized,
        Some(projection.block_hash),
        Some(B256::repeat_byte(0xEE)),
    )
    .is_err());
    assert!(evaluate_ocomp_projection_containment(
        checkpoint(21, 0x21),
        required,
        finalized,
        Some(B256::repeat_byte(0x21)),
        Some(required.block_hash),
    )
    .is_err());
}

#[tokio::test]
async fn ocomp_ahead_containment_does_not_change_execution_readiness_semantics() {
    let required = checkpoint(10, 0x10);
    let ahead = checkpoint(15, 0x15);
    let (_publisher, readiness) = projection_readiness(
        checkpoint(0, 0x01),
        ProjectionStatus::Ready { checkpoint: ahead },
    );

    assert_eq!(
        readiness.wait_for(required, std::future::pending()).await,
        WaitOutcome::ProjectionAhead
    );
    assert!(matches!(
        evaluate_ocomp_projection_containment(
            ahead,
            required,
            FinalizedTarget::new(20, B256::repeat_byte(0x20)),
            Some(ahead.block_hash),
            Some(required.block_hash),
        )
        .unwrap(),
        OcompProjectionContainment::Contains { .. }
    ));
}
