use super::*;

#[test]
fn finalized_parent_wait_blocks_until_the_exact_checkpoint_is_published() {
    commonware_runtime::deterministic::Runner::default().start(|_| async move {
        let baseline = ProjectionCheckpoint {
            block_number: 0,
            block_hash: B256::ZERO,
        };
        let required = ProjectionCheckpoint {
            block_number: 4,
            block_hash: B256::repeat_byte(0x44),
        };
        let (publisher, readiness) =
            projection_readiness(baseline, ProjectionStatus::CatchingUp { checkpoint: None });
        let wait = super::wait_for_finalized_parent(readiness, required);
        futures::pin_mut!(wait);

        assert!(matches!(
            futures::poll!(&mut wait),
            std::task::Poll::Pending
        ));

        publisher.publish(ProjectionStatus::Ready {
            checkpoint: required,
        });
        wait.await
            .expect("finalized execution must resume at the exact projected parent");
    });
}

#[test]
fn finalized_parent_wait_fails_closed_for_ahead_and_fatal_projection() {
    commonware_runtime::deterministic::Runner::default().start(|_| async move {
        let baseline = ProjectionCheckpoint {
            block_number: 0,
            block_hash: B256::ZERO,
        };
        let required = ProjectionCheckpoint {
            block_number: 4,
            block_hash: B256::repeat_byte(0x44),
        };
        let ahead = ProjectionCheckpoint {
            block_number: 5,
            block_hash: B256::repeat_byte(0x55),
        };
        let (_publisher, ahead_readiness) =
            projection_readiness(baseline, ProjectionStatus::Ready { checkpoint: ahead });
        let error = super::wait_for_finalized_parent(ahead_readiness, required)
            .await
            .expect_err("an ahead projection must fail finalized execution closed");
        assert!(error.to_string().contains("projection is ahead"));

        let (_publisher, fatal_readiness) = projection_readiness(
            baseline,
            ProjectionStatus::Fatal {
                checkpoint: None,
                error: ProjectionFailure::new(
                    ProjectionFailureClass::CorruptBody,
                    "test corrupt body",
                ),
            },
        );
        let error = super::wait_for_finalized_parent(fatal_readiness, required)
            .await
            .expect_err("a fatal projection must fail finalized execution closed");
        assert!(error.to_string().contains("test corrupt body"));
    });
}

#[test]
fn optional_full_node_ocomp_gate_waits_for_the_exact_parent() {
    commonware_runtime::deterministic::Runner::default().start(|_| async move {
        let baseline = ProjectionCheckpoint {
            block_number: 0,
            block_hash: B256::ZERO,
        };
        let required = ProjectionCheckpoint {
            block_number: 100,
            block_hash: B256::repeat_byte(0x64),
        };
        super::wait_for_optional_ocomp_parent(None, required)
            .await
            .expect("Validator path has no FullNode OCOMP gate");

        let (publisher, readiness) =
            projection_readiness(baseline, ProjectionStatus::CatchingUp { checkpoint: None });
        let wait = super::wait_for_optional_ocomp_parent(Some(readiness), required);
        futures::pin_mut!(wait);
        assert!(matches!(
            futures::poll!(&mut wait),
            std::task::Poll::Pending
        ));
        publisher.publish(ProjectionStatus::Ready {
            checkpoint: required,
        });
        wait.await
            .expect("FullNode resumes only at the exact OCOMP checkpoint");
    });
}
