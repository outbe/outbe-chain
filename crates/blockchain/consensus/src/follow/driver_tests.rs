use super::*;
use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use commonware_consensus::{marshal, types::ViewDelta};
use commonware_parallel::Sequential;
use commonware_runtime::{buffer::paged::CacheRef, deterministic, Runner as _, Supervisor as _};
use futures::FutureExt as _;
use reth_ethereum::{primitives::SealedBlock, Block};

use crate::{
    block::ConsensusBlock, hybrid::HybridSchemeProvider, marshal_types::FollowMarshalActor,
    test_fixtures::marshal::MarshalArchiveFixture,
};

/// Real mailbox with an unstarted actor. Its progress query stays pending until
/// the test drops the actor. No replacement mailbox or production test override.
async fn pending_marshal(
    context: &deterministic::Context,
) -> (FollowMarshalActor<deterministic::Context>, MarshalMailbox) {
    let cache = CacheRef::from_pooler(
        context,
        NonZeroU16::new(1024).unwrap(),
        NonZeroUsize::new(10).unwrap(),
    );
    let items = NonZeroU64::new(10).unwrap();
    let buffer = NonZeroUsize::new(1024).unwrap();
    let (certificates, blocks) = MarshalArchiveFixture {
        partition_prefix: "driver-test",
        page_cache: &cache,
        items_per_section: items,
        replay_buffer: buffer,
        write_buffer: buffer,
    }
    .open(context)
    .await
    .unwrap();
    let genesis = Block::default().map_header(outbe_primitives::OutbeHeader::new);
    let (actor, mailbox, _) = marshal::core::Actor::init(
        context.child("marshal"),
        certificates,
        blocks,
        marshal::Config {
            provider: HybridSchemeProvider::new(),
            epocher: FollowerEpocher::new(100, 10),
            start: marshal::Start::Genesis(ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                genesis,
            ))),
            partition_prefix: "driver-test".into(),
            mailbox_size: NonZeroUsize::new(32).unwrap(),
            view_retention: ViewDelta::new(100),
            prunable_items_per_section: items,
            page_cache: cache,
            replay_buffer: buffer,
            key_write_buffer: buffer,
            value_write_buffer: buffer,
            block_codec_config: (),
            max_repair: NonZeroUsize::new(10).unwrap(),
            max_pending_acks: NonZeroUsize::new(1).unwrap(),
            strategy: Sequential,
        },
    )
    .await;
    (actor, mailbox)
}

#[derive(Clone)]
struct Tip {
    pending: bool,
    calls: Arc<AtomicUsize>,
}

impl TipSource for Tip {
    async fn finalized_tip(&self) -> Option<Height> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.pending {
            std::future::pending().await
        } else {
            Some(Height::new(10))
        }
    }
}

#[test]
fn shutdown_cancels_pending_tip_or_marshal_and_polling_sleep() {
    for (pending_tip, closed_marshal) in [(true, false), (false, false), (false, true)] {
        deterministic::Runner::default().start(move |context| async move {
            let (actor, marshal) = pending_marshal(&context).await;
            let _actor = if closed_marshal {
                drop(actor);
                None
            } else {
                Some(actor)
            };
            let calls = Arc::new(AtomicUsize::new(0));
            let driver = Driver::new(
                context.child("driver"),
                Config {
                    marshal,
                    tip: Tip {
                        pending: pending_tip,
                        calls: Arc::clone(&calls),
                    },
                    epocher: FollowerEpocher::new(100, 10),
                    phase_cancel: tokio_util::sync::CancellationToken::new(),
                },
            );
            let run = driver.run();
            futures::pin_mut!(run);
            assert!(run.as_mut().now_or_never().is_none());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            let stop = context.stop(0, Some(Duration::from_secs(1)));
            futures::pin_mut!(stop);
            assert!(stop.as_mut().now_or_never().is_none());
            assert!(
                run.as_mut().now_or_never().is_some(),
                "driver must exit without a query response"
            );
            stop.await.unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1, "no queries after shutdown");
        });
    }
}

#[test]
fn unavailable_progress_is_not_genesis_and_hints_respect_all_bounds() {
    assert_eq!(
        hint_range(None, Height::new(100), Some(Height::new(100))),
        None
    );
    assert_eq!(
        hint_range(
            Some(Height::new(0)),
            Height::new(100),
            Some(Height::new(100))
        ),
        Some(1..=64)
    );
    assert_eq!(
        hint_range(
            Some(Height::new(60)),
            Height::new(200),
            Some(Height::new(80))
        ),
        Some(61..=80)
    );
    assert_eq!(
        hint_range(
            Some(Height::new(60)),
            Height::new(65),
            Some(Height::new(80))
        ),
        Some(61..=65)
    );
    assert_eq!(
        hint_range(
            Some(Height::new(60)),
            Height::new(60),
            Some(Height::new(80))
        ),
        None
    );
    assert_eq!(
        hint_range(Some(Height::new(60)), Height::new(200), None),
        None
    );
}
