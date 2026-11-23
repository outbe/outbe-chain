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
    let cache = CacheRef::new(
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
            start: marshal::Start::Genesis(crate::test_fixtures::digested_block(
                ConsensusBlock::from_sealed(SealedBlock::seal_slow(genesis)),
            )),
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
    async fn finalized_tip(&self) -> Result<Height, crate::follow::upstream::UpstreamTipError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.pending {
            std::future::pending().await
        } else {
            Ok(Height::new(10))
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
    for (processed, tip, ceiling, expected) in [
        (None, 100, Some(100), (None, false)),
        (Some(0), 100, Some(100), (Some(1..=64), false)),
        (Some(60), 200, Some(80), (Some(61..=80), true)),
        (Some(60), 65, Some(80), (Some(61..=65), false)),
        (Some(60), 60, Some(80), (None, false)),
        (Some(60), 200, None, (None, true)),
        (Some(60), 200, Some(60), (None, true)),
        (Some(60), 200, Some(150), (Some(61..=124), false)),
    ] {
        assert_eq!(
            hint_range(
                processed.map(Height::new),
                Height::new(tip),
                ceiling.map(Height::new)
            ),
            expected
        );
    }
}

use crate::follow::upstream::{FinalizedSource, LocalBlockSource, UpstreamTipError};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use std::collections::BTreeMap;

fn telemetry(snapshotter: &Snapshotter) -> BTreeMap<String, DebugValue> {
    snapshotter
        .snapshot()
        .into_vec()
        .into_iter()
        .map(|(key, _, _, value)| {
            let labels = key
                .key()
                .labels()
                .map(|label| format!("{}={}", label.key(), label.value()))
                .collect::<Vec<_>>()
                .join(",");
            (format!("{}:{labels}", key.key().name()), value)
        })
        .collect()
}

#[derive(Clone)]
struct PollingTip {
    height: u64,
    calls: Arc<AtomicUsize>,
}
impl TipSource for PollingTip {
    async fn finalized_tip(&self) -> Result<Height, UpstreamTipError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(Height::new(self.height))
        } else {
            Err(UpstreamTipError::HttpRateLimited {
                source: jsonrpsee::core::client::Error::Custom("rate limited".into()),
            })
        }
    }
}

#[derive(Clone)]
struct EmptySource;
impl FinalizedSource for EmptySource {
    async fn get_finalization(
        &self,
        _: Height,
    ) -> Option<crate::follow::upstream::CertifiedFinalizedBlock> {
        None
    }
}
impl LocalBlockSource for EmptySource {
    async fn get_block_by_digest(&self, _: crate::digest::Digest) -> Option<ConsensusBlock> {
        None
    }
}

#[derive(Clone)]
struct ProgressReporter;
impl commonware_consensus::Reporter for ProgressReporter {
    type Activity = crate::marshal_types::MarshalUpdate;
    fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
        use commonware_utils::Acknowledgement as _;
        if let marshal::Update::Block(_, ack) = activity {
            ack.acknowledge();
        }
        commonware_actor::Feedback::Ok
    }
}

async fn running_driver(
    context: &deterministic::Context,
    tip: PollingTip,
) -> commonware_runtime::Handle<()> {
    use crate::follow::{resolver, CommitteeChain, SharedCommitteeChain};
    use commonware_consensus::{marshal::resolver::handler, types::Epoch};
    let (actor, marshal) = pending_marshal(context).await;
    let (receiver, handler) =
        handler::init(context.child("handler"), NonZeroUsize::new(32).unwrap());
    let resolver = resolver::init(
        context.child("resolver"),
        handler,
        resolver::FetchResolution {
            upstream: EmptySource,
            local: EmptySource,
            chain: SharedCommitteeChain::new(CommitteeChain::new(
                Epoch::new(0),
                Default::default(),
            )),
            epocher: FollowerEpocher::new(10, 0),
        },
        NonZeroUsize::new(32).unwrap(),
    );
    actor.start_unbuffered(ProgressReporter, (receiver, resolver));
    Driver::new(
        context.child("driver"),
        Config {
            marshal,
            tip,
            epocher: FollowerEpocher::new(10, 0),
        },
    )
    .start()
}

#[test]
fn driver_records_success_429_cached_tip_and_shutdown_reset() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        let config = deterministic::Config::default()
            .with_start_time(SystemTime::UNIX_EPOCH + Duration::from_secs(100));
        deterministic::Runner::new(config).start(|context| async move {
            let calls = Arc::new(AtomicUsize::new(0));
            let handle = running_driver(
                &context,
                PollingTip {
                    height: 10,
                    calls: calls.clone(),
                },
            )
            .await;
            context.sleep(Duration::from_millis(100)).await;
            let first = telemetry(&snapshotter);
            assert_eq!(
                first["outbe_follower_upstream_tip_height:"],
                DebugValue::Gauge(10.0.into())
            );
            assert_eq!(
                first["outbe_follower_upstream_tip_known:"],
                DebugValue::Gauge(1.0.into())
            );
            let DebugValue::Gauge(timestamp) =
                &first["outbe_follower_upstream_last_ok_timestamp_seconds:"]
            else {
                panic!("timestamp must be a gauge")
            };
            assert!((100.0..101.0).contains(&timestamp.into_inner()));
            assert_eq!(
                first["outbe_follower_processed_height:"],
                DebugValue::Gauge(0.0.into())
            );
            assert_eq!(
                first["outbe_follower_supported_ceiling_height:"],
                DebugValue::Gauge(11.0.into())
            );
            assert_eq!(
                first["outbe_follower_rounds_total:result=pulled"],
                DebugValue::Counter(1)
            );
            context.sleep(Duration::from_millis(2100)).await;
            let after_error = telemetry(&snapshotter);
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(
                after_error["outbe_follower_upstream_tip_known:"],
                DebugValue::Gauge(0.0.into())
            );
            assert_eq!(
                after_error["outbe_follower_upstream_poll_errors_total:reason=http_429"],
                DebugValue::Counter(1)
            );
            assert_eq!(
                after_error["outbe_follower_rounds_total:result=pulled"],
                DebugValue::Counter(4)
            );
            let stop = context.stop(0, Some(Duration::from_secs(1)));
            futures::pin_mut!(stop);
            assert!(stop.as_mut().now_or_never().is_none());
            handle.await.unwrap();
            stop.await.unwrap();
        });
    });
}

#[test]
fn stopping_after_success_and_aborting_run_clear_tip_known() {
    for abort in [false, true] {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async move {
                let handle = running_driver(
                    &context,
                    PollingTip {
                        height: 10,
                        calls: Arc::default(),
                    },
                )
                .await;
                context.sleep(Duration::from_millis(100)).await;
                assert_eq!(
                    telemetry(&snapshotter)["outbe_follower_upstream_tip_known:"],
                    DebugValue::Gauge(1.0.into())
                );
                if abort {
                    handle.abort();
                    assert!(handle.await.is_err());
                } else {
                    let stop = context.stop(0, Some(Duration::from_secs(1)));
                    futures::pin_mut!(stop);
                    assert!(stop.as_mut().now_or_never().is_none());
                    handle.await.unwrap();
                    stop.await.unwrap();
                }
                assert_eq!(
                    telemetry(&snapshotter)["outbe_follower_upstream_tip_known:"],
                    DebugValue::Gauge(0.0.into())
                );
            });
        });
    }
}

#[test]
fn driver_classifies_idle_and_ceiling_clamped_rounds() {
    for (height, result) in [(0, "idle"), (200, "ceiling_clamped")] {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async move {
                let handle = running_driver(
                    &context,
                    PollingTip {
                        height,
                        calls: Arc::default(),
                    },
                )
                .await;
                context.sleep(Duration::from_millis(100)).await;
                assert_eq!(
                    telemetry(&snapshotter)
                        [&format!("outbe_follower_rounds_total:result={result}")],
                    DebugValue::Counter(1)
                );
                handle.abort();
                assert!(handle.await.is_err());
            });
        });
    }
}

#[test]
fn first_poll_failure_is_unreachable_and_all_poll_labels_are_closed() {
    use crate::follow::metrics::UpstreamPollErrorReason;
    let source = || jsonrpsee::core::client::Error::Custom("upstream failure".into());
    for (error, expected) in [
        (
            UpstreamTipError::HttpRateLimited { source: source() },
            "http_429",
        ),
        (
            UpstreamTipError::HttpStatus {
                status_code: 503,
                source: source(),
            },
            "http_status",
        ),
        (
            UpstreamTipError::Transport { source: source() },
            "transport",
        ),
        (
            UpstreamTipError::InvalidResponse { source: source() },
            "invalid_response",
        ),
    ] {
        assert_eq!(UpstreamPollErrorReason::from(&error).as_label(), expected);
    }
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        deterministic::Runner::default().start(|context| async move {
            let handle = running_driver(
                &context,
                PollingTip {
                    height: 10,
                    calls: Arc::new(AtomicUsize::new(1)),
                },
            )
            .await;
            context.sleep(Duration::from_millis(100)).await;
            let snapshot = telemetry(&snapshotter);
            assert_eq!(
                snapshot["outbe_follower_upstream_tip_known:"],
                DebugValue::Gauge(0.0.into())
            );
            assert_eq!(
                snapshot["outbe_follower_rounds_total:result=tip_unreachable"],
                DebugValue::Counter(1)
            );
            handle.abort();
            assert!(handle.await.is_err());
        });
    });
}

#[test]
fn driver_pull_to_advances_from_processed_absent_floor_to_block() {
    use commonware_consensus::{
        marshal::core::Processed,
        marshal::resolver::handler,
        simplex::types::{Finalization, Finalize, Proposal},
        types::{Epoch, Round, View},
    };
    use commonware_cryptography::{
        bls12381::{self, primitives::variant::MinSig},
        Signer as _,
    };
    use commonware_utils::{
        ordered::{Quorum as _, Set},
        Acknowledgement as _, TryCollect as _,
    };
    use std::sync::Mutex;

    #[derive(Clone)]
    struct HoldingReporter {
        held: Arc<Mutex<Vec<(u64, commonware_utils::acknowledgement::Exact)>>>,
    }

    impl commonware_consensus::Reporter for HoldingReporter {
        type Activity = crate::marshal_types::MarshalUpdate;
        fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
            if let marshal::Update::Block(block, ack) = activity {
                if block.inner().number() == 0 {
                    ack.acknowledge();
                } else {
                    self.held
                        .lock()
                        .unwrap()
                        .push((block.inner().number(), ack));
                }
            }
            commonware_actor::Feedback::Ok
        }
    }

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        deterministic::Runner::default().start(|context| async move {
            let keys: Vec<bls12381::PrivateKey> =
                (1u64..=3).map(bls12381::PrivateKey::from_seed).collect();
            let participants: Set<bls12381::PublicKey> =
                keys.iter().map(|k| k.public_key()).try_collect().unwrap();
            let dkg = crate::bls::bootstrap_dkg(3).unwrap();
            let signers: Vec<crate::hybrid::HybridScheme<MinSig>> = keys
                .iter()
                .map(|key| {
                    let idx = participants.index(&key.public_key()).unwrap();
                    crate::hybrid::HybridScheme::signer(
                        &crate::config::outbe_app_namespace(),
                        participants.clone(),
                        key.clone(),
                        dkg.polynomial.clone(),
                        dkg.shares[idx.get() as usize].clone(),
                    )
                    .unwrap()
                })
                .collect();
            let verifier = crate::hybrid::HybridScheme::<MinSig>::verifier(
                &crate::config::outbe_app_namespace(),
                participants,
                dkg.polynomial.clone(),
            )
            .unwrap();
            let provider = HybridSchemeProvider::new();
            assert!(provider.register(Epoch::new(0), verifier.clone()));

            let mut raw = Block::default();
            raw.header.number = 5;
            raw.header.extra_data = alloy_primitives::Bytes::from_static(&[5]);
            let floor_block = ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                raw.map_header(outbe_primitives::OutbeHeader::new),
            ));
            let round = Round::new(Epoch::new(0), View::new(5));
            let proposal = Proposal::new(round, View::new(4), floor_block.digest());
            let votes: Vec<_> = signers
                .iter()
                .map(|s| Finalize::sign(s, proposal.clone()).unwrap())
                .collect();
            let finalization = Finalization::from_finalizes(
                &verifier,
                commonware_utils::iter::NonEmpty::try_new(votes.iter()).unwrap(),
                &Sequential,
            )
            .unwrap();

            let cache = CacheRef::new(
                NonZeroU16::new(1024).unwrap(),
                NonZeroUsize::new(10).unwrap(),
            );
            let items = NonZeroU64::new(10).unwrap();
            let buffer = NonZeroUsize::new(1024).unwrap();
            let (certificates, blocks) = MarshalArchiveFixture {
                partition_prefix: "driver-absent-floor",
                page_cache: &cache,
                items_per_section: items,
                replay_buffer: buffer,
                write_buffer: buffer,
            }
            .open(&context)
            .await
            .unwrap();
            let genesis = Block::default().map_header(outbe_primitives::OutbeHeader::new);
            let (actor, mailbox, _) = marshal::core::Actor::init(
                context.child("marshal"),
                certificates,
                blocks,
                marshal::Config {
                    provider,
                    epocher: FollowerEpocher::new(100, 10),
                    start: marshal::Start::Genesis(crate::test_fixtures::digested_block(
                        ConsensusBlock::from_sealed(SealedBlock::seal_slow(genesis)),
                    )),
                    partition_prefix: "driver-absent-floor".into(),
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
            let held = Arc::new(Mutex::new(Vec::new()));
            let (receiver, handler) =
                handler::init(context.child("handler"), NonZeroUsize::new(32).unwrap());
            let resolver = crate::follow::resolver::init(
                context.child("resolver"),
                handler,
                crate::follow::resolver::FetchResolution {
                    upstream: EmptySource,
                    local: EmptySource,
                    chain: crate::follow::SharedCommitteeChain::new(
                        crate::follow::CommitteeChain::new(Epoch::new(0), Default::default()),
                    ),
                    epocher: FollowerEpocher::new(100, 10),
                },
                NonZeroUsize::new(32).unwrap(),
            );
            let _marshal_handle = actor.start_unbuffered(
                HoldingReporter {
                    held: Arc::clone(&held),
                },
                (receiver, resolver),
            );

            while mailbox.get_processed().await != Some(Processed::Block(Height::zero())) {
                context.sleep(Duration::from_millis(1)).await;
            }
            assert!(
                mailbox
                    .verified(
                        round,
                        commonware_cryptography::Digested::precomputed(
                            floor_block.clone(),
                            floor_block.digest(),
                        ),
                    )
                    .await
            );
            mailbox.set_floor(finalization);
            while mailbox.get_processed().await != Some(Processed::Absent(Height::new(4))) {
                context.sleep(Duration::from_millis(1)).await;
            }

            let mut driver = Driver::new(
                context.child("driver"),
                Config {
                    marshal: mailbox.clone(),
                    tip: Tip {
                        pending: false,
                        calls: Arc::new(AtomicUsize::new(0)),
                    },
                    epocher: FollowerEpocher::new(100, 10),
                },
            );

            // Consumer path under Processed::Absent(4): pull_to must read floor height 4 and
            // emit hints for 5..=7 rather than stalling or resetting to genesis.
            driver.pull_to(Height::new(7)).await;
            let snap_absent = telemetry(&snapshotter);
            assert_eq!(
                snap_absent["outbe_follower_processed_height:"],
                DebugValue::Gauge(4.0.into())
            );
            assert_eq!(
                snap_absent["outbe_follower_rounds_total:result=pulled"],
                DebugValue::Counter(1)
            );

            // Acknowledge block 5 -> Processed::Block(5), then pull_to(7) pulls and pull_to(5) idles.
            while held.lock().unwrap().is_empty() {
                context.sleep(Duration::from_millis(1)).await;
            }
            for (height, ack) in held.lock().unwrap().drain(..) {
                assert_eq!(height, 5);
                ack.acknowledge();
            }
            while mailbox.get_processed().await != Some(Processed::Block(Height::new(5))) {
                context.sleep(Duration::from_millis(1)).await;
            }
            driver.pull_to(Height::new(7)).await;
            let snap_block = telemetry(&snapshotter);
            assert_eq!(
                snap_block["outbe_follower_processed_height:"],
                DebugValue::Gauge(5.0.into())
            );
            assert_eq!(
                snap_block["outbe_follower_rounds_total:result=pulled"],
                DebugValue::Counter(1),
                "DebuggingRecorder snapshots drain counters; the second pull_to(7) must record 1 fresh pull"
            );
            driver.pull_to(Height::new(5)).await;
            let snap_idle = telemetry(&snapshotter);
            assert_eq!(
                snap_idle["outbe_follower_rounds_total:result=idle"],
                DebugValue::Counter(1)
            );
            assert_eq!(
                snap_idle["outbe_follower_rounds_total:result=pulled"],
                DebugValue::Counter(0)
            );
        });
    });
}
