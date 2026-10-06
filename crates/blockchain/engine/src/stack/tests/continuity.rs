use super::*;
use crate::stack::epoch::continuity::{
    resolve_epoch_floor, wait_for_activation_anchor, AnchorTransition,
};
use commonware_runtime::deterministic;

#[test]
fn genesis_floor_does_not_require_a_finalized_certificate() {
    deterministic::Runner::default().start(|ctx| async move {
        let view = new_finalization_view(B256::ZERO, 0, None);
        let genesis = B256::with_last_byte(7);
        assert_eq!(
            resolve_epoch_floor(&ctx, &view, Epoch::new(0), genesis)
                .await
                .unwrap(),
            Digest(genesis)
        );
    });
}

#[test]
fn dkg_restart_waits_for_the_activation_height() {
    deterministic::Runner::default().start(|ctx| async move {
        let round = Round::new(Epoch::new(1), View::new(3));
        let view = new_finalization_view(B256::with_last_byte(8), 9, Some(round));
        let writer = view.clone();
        let _publish = ctx
            .child("publish_activation")
            .spawn(move |ctx| async move {
                ctx.sleep(Duration::from_millis(200)).await;
                let mut view = writer.write();
                view.last_finalized_number = 10;
                view.forkchoice.finalized_block_hash = B256::with_last_byte(9);
            });
        let started = ctx.current();
        wait_for_activation_anchor(&ctx, &view, 10, AnchorTransition::Dkg)
            .await
            .unwrap();
        assert!(ctx.current().duration_since(started).unwrap() >= Duration::from_millis(200));
    });
}

#[test]
fn restarted_epoch_rejects_an_anchor_without_a_finalized_round() {
    deterministic::Runner::default().start(|ctx| async move {
        let view = new_finalization_view(B256::with_last_byte(8), 10, None);
        let started = ctx.current();
        let error = resolve_epoch_floor(&ctx, &view, Epoch::new(2), B256::with_last_byte(7))
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("restart without finalized anchor"));
        assert_eq!(
            ctx.current().duration_since(started).unwrap(),
            Duration::from_secs(5)
        );
    });
}

#[test]
fn restarted_epoch_uses_the_anchor_published_after_startup() {
    deterministic::Runner::default().start(|ctx| async move {
        let view = new_finalization_view(B256::ZERO, 0, None);
        let writer = view.clone();
        let _publish = ctx
            .child("publish_certificate")
            .spawn(move |ctx| async move {
                ctx.sleep(Duration::from_millis(200)).await;
                let mut view = writer.write();
                view.last_finalized_number = 10;
                view.forkchoice.finalized_block_hash = B256::with_last_byte(9);
                view.last_finalized_round = Some(Round::new(Epoch::new(1), View::new(3)));
            });
        assert_eq!(
            resolve_epoch_floor(&ctx, &view, Epoch::new(2), B256::with_last_byte(7))
                .await
                .unwrap(),
            Digest(B256::with_last_byte(9))
        );
    });
}

#[test]
fn dealer_demotion_rejects_a_zero_anchor_hash() {
    deterministic::Runner::default().start(|ctx| async move {
        let round = Round::new(Epoch::new(1), View::new(3));
        let view = new_finalization_view(B256::ZERO, 10, Some(round));
        let started = ctx.current();
        let error = wait_for_activation_anchor(&ctx, &view, 10, AnchorTransition::DealerDemotion)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("demotion activation race"));
        assert_eq!(
            ctx.current().duration_since(started).unwrap(),
            Duration::from_secs(5)
        );
    });
}
