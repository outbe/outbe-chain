use super::*;
use crate::test_fixtures::marshal::{start_processed_absent_marshal, wait_until_bounded};
use commonware_consensus::marshal::core::Processed;
use commonware_runtime::{deterministic, Runner as _};
use commonware_utils::Acknowledgement as _;

#[test]
fn finality_observer_quiesce_enforces_processed_absent_floor_then_accepts_block() {
    deterministic::Runner::default().start(|context| async move {
        let fixture = start_processed_absent_marshal(&context, "quiesce-absent", 5).await;

        let (_control, drain) = crate::follower_shutdown::follower_drain_pair();
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let reporter = drain
            .install(crate::executor::Mailbox::from_sender(tx))
            .unwrap()
            .unwrap();
        reporter.initiate_promotion(5).unwrap();

        // At Processed::Absent(4): persisted == 4 is acknowledged, persisted == 5 fails closed.
        verify_finality_observer_quiesced(&reporter, &fixture.mailbox, Some(4))
            .await
            .expect("persisted == Absent(4) floor must quiesce cleanly");
        let err = verify_finality_observer_quiesced(&reporter, &fixture.mailbox, Some(5))
            .await
            .expect_err("persisted > Absent(4) must fail quiesce");
        let typed = err
            .downcast_ref::<crate::errors::FollowerStackError>()
            .expect("typed FollowerStackError");
        assert!(
            matches!(
                typed,
                crate::errors::FollowerStackError::MarshalHasNotAcknowledgedProofDrain {
                    processed,
                    persisted: 5,
                } if *processed == Height::new(4)
            ),
            "unexpected error: {typed:?}"
        );

        // Acknowledging block 5 advances Processed::Absent(4) -> Processed::Block(5),
        // allowing persisted == 5 to quiesce cleanly.
        wait_until_bounded(&context, 200, "held block 5 delivery", || async {
            !fixture.held.lock().unwrap().is_empty()
        })
        .await;
        for (height, ack) in fixture.held.lock().unwrap().drain(..) {
            assert_eq!(height, 5);
            ack.acknowledge();
        }
        wait_until_bounded(&context, 200, "Processed::Block(5)", || async {
            fixture.mailbox.get_processed().await == Some(Processed::Block(Height::new(5)))
        })
        .await;
        verify_finality_observer_quiesced(&reporter, &fixture.mailbox, Some(5))
            .await
            .expect("persisted == Block(5) must quiesce cleanly");
    });
}
