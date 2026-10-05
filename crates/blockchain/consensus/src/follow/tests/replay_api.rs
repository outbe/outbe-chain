//! Archive ownership and rejection timing at the public replay boundary.
use super::*;
use std::{
    future::Future,
    task::{Context, Poll, Waker},
};

#[derive(Clone, Debug)]
pub(super) struct ArchiveDropTrace {
    pub label: &'static str,
    pub events: Arc<std::sync::Mutex<Vec<&'static str>>>,
}
impl Drop for ArchiveDropTrace {
    fn drop(&mut self) {
        self.events.lock().unwrap().push(self.label);
    }
}

fn tracked_archives(
    events: &Arc<std::sync::Mutex<Vec<&'static str>>>,
) -> (MemoryCertificates, MemoryBlocks) {
    (
        MemoryCertificates {
            _drop_trace: Some(ArchiveDropTrace {
                label: "certificates",
                events: events.clone(),
            }),
            ..Default::default()
        },
        MemoryBlocks {
            _drop_trace: Some(ArchiveDropTrace {
                label: "blocks",
                events: events.clone(),
            }),
            ..Default::default()
        },
    )
}

struct ReplayFixture {
    chain: SharedCommitteeChain,
    source: ArchivedFinalizedSource,
    epocher: FollowerEpocher,
    events: Arc<std::sync::Mutex<Vec<&'static str>>>,
}
impl ReplayFixture {
    fn new() -> Self {
        let c0 = committee(10);
        let epoch = Epoch::new(0);
        let record = certified_block(&c0, epoch, 1, c0.boundary_block_extra_data(epoch));
        Self {
            chain: SharedCommitteeChain::new(CommitteeChain::new(epoch, c0.participants)),
            source: ArchivedFinalizedSource {
                by_height: Arc::new(BTreeMap::from([(1, record)])),
            },
            epocher: FollowerEpocher::new(10, 0),
            events: Arc::default(),
        }
    }

    fn replay_from<'a, F: FinalizedSource>(
        &'a self,
        source: &'a F,
        window: engine::ReplayWindow,
    ) -> impl Future<Output = eyre::Result<(Epoch, MemoryCertificates, MemoryBlocks)>> + 'a {
        let (certificates, blocks) = tracked_archives(&self.events);
        engine::authenticate_and_reconcile_replay_suffix(
            replay_authority(&self.chain, source, &self.epocher),
            window,
            engine::ReplayArchives::new(certificates, blocks),
        )
    }

    fn replay(
        &self,
        lower: u64,
        upper: u64,
    ) -> impl Future<Output = eyre::Result<(Epoch, MemoryCertificates, MemoryBlocks)>> + '_ {
        self.replay_from(
            &self.source,
            engine::ReplayWindow {
                anchor_epoch: Epoch::new(0),
                lower: Height::new(lower),
                upper: Height::new(upper),
            },
        )
    }
}

#[test]
fn replay_api_unpolled_cancellation_preserves_archive_drop_order() {
    let fixture = ReplayFixture::new();
    let replay = fixture.replay(0, 0);
    assert!(fixture.events.lock().unwrap().is_empty());
    drop(replay);
    assert_eq!(*fixture.events.lock().unwrap(), ["certificates", "blocks"]);
}

#[test]
fn replay_api_invalid_window_keeps_error_and_archive_drop_order() {
    let fixture = ReplayFixture::new();
    let error = futures::executor::block_on(fixture.replay(2, 1)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "follower replay suffix lower height 2 exceeds upper height 1"
    );
    assert_eq!(fixture.chain.lock().highest_registered(), None);
    assert_eq!(*fixture.events.lock().unwrap(), ["blocks", "certificates"]);
}

#[test]
fn replay_api_success_returns_archive_ownership_to_the_caller() {
    let fixture = ReplayFixture::new();
    let result = futures::executor::block_on(fixture.replay(0, 0)).unwrap();
    assert_eq!(result.0, Epoch::new(0));
    assert!(fixture.events.lock().unwrap().is_empty());
    drop(result);
    assert_eq!(*fixture.events.lock().unwrap(), ["certificates", "blocks"]);
}

#[derive(Clone)]
struct PendingSource;
impl FinalizedSource for PendingSource {
    fn get_finalization(
        &self,
        _height: Height,
    ) -> impl Future<Output = Option<CertifiedFinalizedBlock>> + Send {
        std::future::pending()
    }
}

#[test]
fn replay_api_pending_cancellation_preserves_archive_drop_order() {
    let fixture = ReplayFixture::new();
    let source = PendingSource;
    let mut replay = Box::pin(fixture.replay_from(
        &source,
        engine::ReplayWindow {
            anchor_epoch: Epoch::new(0),
            lower: Height::new(0),
            upper: Height::new(0),
        },
    ));
    assert!(matches!(
        replay
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert!(fixture.events.lock().unwrap().is_empty());
    drop(replay);
    assert_eq!(*fixture.events.lock().unwrap(), ["blocks", "certificates"]);
}
