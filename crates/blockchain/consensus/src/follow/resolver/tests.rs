use super::*;
use commonware_codec::{DecodeExt as _, Encode as _};

#[test]
fn opaque_key_preserves_height_binding_and_codec() {
    use commonware_consensus::types::{Round, View};

    let key = Key::Block(Digest::ZERO);
    let a = BoundKey::new(
        key,
        &Annotation::Certified {
            height: Height::new(7),
        },
    );
    let b = BoundKey::new(
        key,
        &Annotation::Finalized(handler::Finalized::ByHeight {
            height: Height::new(7),
        }),
    );
    let untrusted_no_round = BoundKey::new(
        key,
        &Annotation::Untrusted {
            height: Height::new(7),
            round: None,
        },
    );
    let untrusted_with_round = BoundKey::new(
        key,
        &Annotation::Untrusted {
            height: Height::new(7),
            round: Some(Round::new(Epoch::new(0), View::new(9))),
        },
    );
    let other = BoundKey::new(
        key,
        &Annotation::Certified {
            height: Height::new(8),
        },
    );
    let untrusted_other = BoundKey::new(
        key,
        &Annotation::Untrusted {
            height: Height::new(8),
            round: None,
        },
    );
    let by_round = BoundKey::new(
        key,
        &Annotation::Finalized(handler::Finalized::ByRound {
            round: Round::new(Epoch::new(0), View::new(9)),
        }),
    );
    let notarization = BoundKey::new(
        key,
        &Annotation::Notarization {
            round: Round::new(Epoch::new(0), View::new(9)),
        },
    );
    assert_eq!(a, b);
    assert_eq!(a, untrusted_no_round);
    assert_eq!(a, untrusted_with_round);
    assert_ne!(a, other);
    assert_eq!(other, untrusted_other);
    assert_eq!(by_round.height, None);
    assert_eq!(notarization.height, None);
    assert_eq!(BoundKey::decode(a.encode()).unwrap(), a);
}

#[test]
fn source_backoff_caps_and_canceled_state_expires() {
    let key = BoundKey {
        key: Key::Finalized {
            height: Height::new(7),
        },
        height: None,
    };
    let mut retries = RetryState::default();
    let now = SystemTime::UNIX_EPOCH;
    let mut delay = retries.begin(key, now);
    assert_eq!(delay, Duration::ZERO);
    for _ in 0..12 {
        retries.failed(key, delay, now);
        delay = retries.begin(key, now);
    }
    assert_eq!(delay, MAX_RETRY_DELAY);
    assert_eq!(retries.begin(key, now + RETRY_STATE_TTL), Duration::ZERO);
}

use crate::block::ConsensusBlock;
use crate::follow::upstream::{AncestorFinalityProof, CertifiedFinalizedBlock};
use crate::follow::CommitteeChain;
use commonware_consensus::types::Epoch;
use commonware_runtime::{deterministic, Runner as _, Supervisor as _};
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use std::sync::atomic::{AtomicUsize, Ordering};

fn failures(snapshotter: &Snapshotter) -> BTreeMap<(String, String), u64> {
    snapshotter
        .snapshot()
        .into_vec()
        .into_iter()
        .filter_map(|(key, _, _, value)| {
            if key.key().name() != "outbe_follower_resolver_failures_total" {
                return None;
            }
            let labels: BTreeMap<_, _> = key.key().labels().map(|l| (l.key(), l.value())).collect();
            let DebugValue::Counter(count) = value else {
                panic!("failure must be a counter")
            };
            Some((
                (labels["kind"].to_string(), labels["reason"].to_string()),
                count,
            ))
        })
        .collect()
}

#[derive(Clone, Default)]
struct Source {
    block: Option<ConsensusBlock>,
    proof: Option<AncestorFinalityProof>,
    stall: bool,
    calls: Arc<AtomicUsize>,
}
impl FinalizedSource for Source {
    async fn get_finalization(&self, _: Height) -> Option<CertifiedFinalizedBlock> {
        self.proof.as_ref().map(|p| p.certified.clone())
    }
    async fn get_finality_proof(&self, _: Height) -> Option<AncestorFinalityProof> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stall {
            std::future::pending().await
        } else {
            self.proof.clone()
        }
    }
    async fn get_block(&self, _: Height) -> Option<ConsensusBlock> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stall {
            std::future::pending().await
        } else {
            self.block.clone()
        }
    }
}
impl LocalBlockSource for Source {
    async fn get_block_by_digest(&self, _: Digest) -> Option<ConsensusBlock> {
        None
    }
}

fn block(size: usize) -> ConsensusBlock {
    use reth_ethereum::{primitives::SealedBlock, Block};
    let mut block = Block::default();
    block.header.number = 2;
    block.header.extra_data = vec![0; size].into();
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(
        block.map_header(outbe_primitives::OutbeHeader::new),
    ))
}

fn keys() -> [BoundKey; 2] {
    [
        BoundKey {
            key: Key::Block(block(0).digest()),
            height: Some(Height::new(2)),
        },
        BoundKey {
            key: Key::Finalized {
                height: Height::new(2),
            },
            height: None,
        },
    ]
}

fn fetcher(
    context: &deterministic::Context,
    source: Source,
) -> Fetcher<deterministic::Context, Source, Source> {
    let (_receiver, handler) =
        handler::init(context.child("handler"), NonZeroUsize::new(16).unwrap());
    Fetcher {
        context: Arc::new(context.child("fetcher")),
        resolution: FetchResolution {
            upstream: source,
            local: Source::default(),
            chain: SharedCommitteeChain::new(CommitteeChain::new(
                Epoch::new(0),
                Default::default(),
            )),
            epocher: FollowerEpocher::new(100, 10),
        },
        handler,
        permits: Arc::new(Semaphore::new(MAX_ACTIVE_ACQUISITIONS)),
        retries: Arc::default(),
    }
}

#[test]
fn deterministic_failures_classify_both_kinds_and_keep_backoff() {
    use commonware_resolver::opaque::Fetcher as _;
    for (key, kind) in keys().into_iter().zip(["block", "finalized"]) {
        for reason in ["missing_or_error", "timeout", "permit_saturated"] {
            let recorder = DebuggingRecorder::new();
            let snapshotter = recorder.snapshotter();
            metrics::with_local_recorder(&recorder, || {
                deterministic::Runner::default().start(|context| async move {
                    let source = Source {
                        stall: reason == "timeout",
                        ..Source::default()
                    };
                    let calls = source.calls.clone();
                    let fetcher = fetcher(&context, source);
                    let permit = (reason == "permit_saturated").then(|| {
                        fetcher
                            .permits
                            .clone()
                            .try_acquire_many_owned(MAX_ACTIVE_ACQUISITIONS as u32)
                            .unwrap()
                    });
                    let started = context.current();
                    assert!(fetcher.fetch(key).await.is_none());
                    let elapsed = context.current().duration_since(started).unwrap();
                    let first = failures(&snapshotter);
                    assert_eq!(
                        first,
                        BTreeMap::from([((kind.to_string(), reason.to_string()), 1)])
                    );
                    if reason == "permit_saturated" {
                        assert_eq!(calls.load(Ordering::SeqCst), 0);
                        assert_eq!(
                            fetcher.retries.lock().begin(key, context.current()),
                            Duration::ZERO
                        );
                    } else {
                        assert_eq!(
                            fetcher.retries.lock().begin(key, context.current()),
                            RETRY_FLOOR
                        );
                        assert_eq!(calls.load(Ordering::SeqCst), 1);
                        if reason == "timeout" {
                            assert!(elapsed >= ACQUISITION_TIMEOUT);
                        }
                        let started = context.current();
                        assert!(fetcher.fetch(key).await.is_none());
                        assert!(context.current().duration_since(started).unwrap() >= RETRY_FLOOR);
                        assert_eq!(
                            fetcher.retries.lock().begin(key, context.current()),
                            Duration::from_millis(500)
                        );
                    }
                    drop(permit);
                });
            });
        }
    }
}

#[test]
fn deterministic_retry_schedule_caps_and_expires_at_ttl() {
    deterministic::Runner::default().start(|context| async move {
        let key = keys()[0];
        let mut retries = RetryState::default();
        assert_eq!(retries.begin(key, context.current()), Duration::ZERO);
        let mut delay = Duration::ZERO;
        for millis in [250, 500, 1000, 2000, 4000, 8000, 16000, 30000, 30000] {
            retries.failed(key, delay, context.current());
            delay = retries.begin(key, context.current());
            assert_eq!(delay, Duration::from_millis(millis));
            context.sleep(delay).await;
        }
        context.sleep(RETRY_STATE_TTL - delay).await;
        assert_eq!(retries.begin(key, context.current()), Duration::ZERO);
    });
}

#[test]
fn block_source_rejections_have_precise_reasons_and_success_clears_retry() {
    use commonware_resolver::opaque::Fetcher as _;
    for (source, height, reason) in [
        (Some(block(0)), None, Some("unannotated_block")),
        (Some(block(1)), Some(Height::new(2)), Some("hash_mismatch")),
        (Some(block(0)), Some(Height::new(3)), Some("hash_mismatch")),
        (
            Some(block(resolution::MAX_DELIVERY_BYTES + 1)),
            Some(Height::new(2)),
            Some("oversized"),
        ),
        (Some(block(0)), Some(Height::new(2)), None),
    ] {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async move {
                let digest = source.as_ref().unwrap().digest();
                let key = BoundKey {
                    key: Key::Block(
                        if reason == Some("hash_mismatch") && height == Some(Height::new(2)) {
                            Digest::ZERO
                        } else {
                            digest
                        },
                    ),
                    height,
                };
                let fetcher = fetcher(
                    &context,
                    Source {
                        block: source,
                        ..Source::default()
                    },
                );
                fetcher
                    .retries
                    .lock()
                    .failed(key, Duration::ZERO, context.current());
                assert_eq!(fetcher.fetch(key).await.is_some(), reason.is_none());
                let expected = reason
                    .map(|r| (("block".to_string(), r.to_string()), 1))
                    .into_iter()
                    .collect();
                assert_eq!(failures(&snapshotter), expected);
                if reason.is_none() {
                    assert!(!fetcher.retries.lock().entries.contains_key(&key));
                }
            });
        });
    }
}

#[test]
fn marshal_delivery_closed_for_both_kinds_is_observed() {
    for (key, kind) in keys().into_iter().zip(["block", "finalized"]) {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async move {
                let (_receiver, handler) =
                    handler::init(context.child("handler"), NonZeroUsize::new(1).unwrap());
                drop(_receiver);
                let mut consumer = MarshalConsumer {
                    handler,
                    context: Arc::new(context.child("consumer")),
                };
                let value = Acquired {
                    value: Fetched::Value(Bytes::from_static(b"block")),
                    _permit: Arc::new(Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap()),
                };
                let response = consumer.deliver(
                    Delivery {
                        key,
                        subscribers: NonEmptyVec::new((
                            Annotation::Certified {
                                height: Height::new(2),
                            },
                            tracing::Span::none(),
                        )),
                    },
                    value,
                );
                assert!(response.await.is_err());
                assert_eq!(
                    failures(&snapshotter),
                    BTreeMap::from([((kind.to_string(), "delivery_rejected".to_string()), 1)])
                );
            });
        });
    }
}

#[test]
fn delivery_relay_preserves_verdicts_and_cancels_original_receiver() {
    for outcome in [
        Some(Outcome::Resolved(Resolution::Complete)),
        Some(Outcome::Ignored),
        Some(Outcome::Invalid),
        None,
    ] {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async move {
                let key = keys()[0].key;
                let (original, response) = oneshot::channel();
                let (sender, receiver) = oneshot::channel();
                let handle = context
                    .child("relay")
                    .spawn(move |_| relay_delivery(key, response, sender));
                if let Some(verdict) = outcome {
                    original.send(verdict).unwrap();
                    assert_eq!(receiver.await.unwrap(), verdict);
                } else {
                    drop(receiver);
                    handle.await.unwrap();
                    assert!(original.is_closed());
                }
                let expected = if outcome == Some(Outcome::Invalid) {
                    BTreeMap::from([(("block".to_string(), "delivery_rejected".to_string()), 1)])
                } else {
                    BTreeMap::new()
                };
                assert_eq!(failures(&snapshotter), expected);
            });
        });
    }
}

fn proof(block: ConsensusBlock) -> (AncestorFinalityProof, SharedCommitteeChain) {
    use crate::hybrid::HybridScheme;
    use commonware_consensus::simplex::types::{Finalization, Proposal, Subject};
    use commonware_consensus::types::{Round, View};
    use commonware_cryptography::bls12381::primitives::variant::MinSig;
    use commonware_cryptography::{certificate::Scheme as _, Signer as _};
    use commonware_utils::{
        ordered::{Quorum as _, Set},
        TryCollect as _,
    };
    let mut keys: Vec<_> = (1..=4).map(bls12381::PrivateKey::from_seed).collect();
    keys.sort_by_key(|key| key.public_key().encode());
    let participants: Set<_> = keys
        .iter()
        .map(|key| key.public_key())
        .try_collect()
        .unwrap();
    let dkg = crate::bls::bootstrap_dkg_for_participants(participants.clone()).unwrap();
    let signers: Vec<_> = keys
        .iter()
        .map(|key| {
            let index = participants.index(&key.public_key()).unwrap();
            HybridScheme::<MinSig>::signer(
                &crate::config::outbe_app_namespace(),
                participants.clone(),
                key.clone(),
                dkg.polynomial.clone(),
                dkg.shares[index.get() as usize].clone(),
            )
            .unwrap()
        })
        .collect();
    let proposal = Proposal::new(
        Round::new(Epoch::new(0), View::new(2)),
        View::new(1),
        block.digest(),
    );
    let attestations = signers.iter().map(|signer| {
        signer
            .sign::<Digest>(Subject::Finalize {
                proposal: &proposal,
            })
            .unwrap()
    });
    let certificate = signers[0]
        .assemble(
            commonware_utils::iter::NonEmpty::try_new(attestations).unwrap(),
            &commonware_parallel::Sequential,
        )
        .unwrap();
    let mut chain = CommitteeChain::new(Epoch::new(0), participants);
    chain
        .register_epoch_from_outcome(
            Epoch::new(0),
            &crate::dkg_manager::encode_outcome(Epoch::new(0), &dkg.output, false),
        )
        .unwrap();
    let proof = AncestorFinalityProof {
        certified: CertifiedFinalizedBlock {
            block,
            finalization: Finalization {
                proposal,
                certificate,
            },
        },
        ancestors: Vec::new(),
    };
    (proof, SharedCommitteeChain::new(chain))
}

#[test]
fn finalized_proof_budget_and_authentication_rejections_are_classified() {
    use commonware_resolver::opaque::Fetcher as _;
    for (size, reason) in [
        (0, "proof_unauthenticated"),
        (resolution::MAX_DELIVERY_BYTES + 1, "oversized"),
    ] {
        let (proof, _) = proof(block(size));
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async move {
                let fetcher = fetcher(
                    &context,
                    Source {
                        proof: Some(proof),
                        ..Source::default()
                    },
                );
                assert!(fetcher.fetch(keys()[1]).await.is_none());
                assert_eq!(
                    failures(&snapshotter),
                    BTreeMap::from([(("finalized".to_string(), reason.to_string()), 1)])
                );
            });
        });
    }
}

#[derive(Clone)]
struct BundleConsumer {
    deliveries: Arc<std::sync::Mutex<Vec<(ResolverKey, Annotation)>>>,
    reject_ancestor: bool,
}
impl Consumer for BundleConsumer {
    type Key = ResolverKey;
    type Value = Bytes;
    type Subscriber = Annotation;
    type Outcome = Outcome;
    fn deliver(
        &mut self,
        delivery: Delivery<ResolverKey, Annotation>,
        _: Bytes,
    ) -> oneshot::Receiver<Outcome> {
        let accepted = !self.reject_ancestor || !matches!(delivery.key, Key::Block(_));
        let (annotation, _) = delivery.subscribers.into_iter().next().unwrap();
        self.deliveries
            .lock()
            .unwrap()
            .push((delivery.key, annotation));
        let (sender, receiver) = oneshot::channel();
        sender
            .send(if accepted {
                Outcome::Resolved(Resolution::Complete)
            } else {
                Outcome::Invalid
            })
            .unwrap();
        receiver
    }
}

#[test]
fn finalized_ancestor_failures_keep_enclosing_request_kind() {
    use reth_ethereum::{primitives::SealedBlock, Block};
    for (size, reject, reason) in [
        (0, false, None),
        (0, true, Some("delivery_rejected")),
        (resolution::MAX_DELIVERY_BYTES + 1, false, Some("oversized")),
    ] {
        let ancestor = block(size);
        let mut anchor = Block::default();
        anchor.header.number = 3;
        anchor.header.parent_hash = ancestor.block_hash();
        let anchor = ConsensusBlock::from_sealed(SealedBlock::seal_slow(
            anchor.map_header(outbe_primitives::OutbeHeader::new),
        ));
        let (mut proof, chain) = proof(anchor);
        proof.ancestors.push(ancestor.clone());
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::default().start(|context| async move {
                let mut resolution = fetcher(
                    &context,
                    Source {
                        proof: Some(proof),
                        ..Source::default()
                    },
                )
                .resolution;
                resolution.chain = chain;
                let mut consumer = BundleConsumer {
                    deliveries: Arc::default(),
                    reject_ancestor: reject,
                };
                let result = resolution
                    .acquire(keys()[1], &tracing::Span::none(), &mut consumer)
                    .await;
                assert_eq!(matches!(result, Some(Fetched::Delivered)), reason.is_none());
                let deliveries = consumer.deliveries.lock().unwrap();
                if reason == Some("oversized") {
                    assert!(
                        deliveries.is_empty(),
                        "budget rejection must precede any delivery"
                    );
                } else {
                    assert_eq!(
                        deliveries.as_slice(),
                        &[
                            (
                                Key::Finalized {
                                    height: Height::new(3)
                                },
                                Annotation::Finalized(handler::Finalized::ByHeight {
                                    height: Height::new(3)
                                })
                            ),
                            (
                                Key::Block(ancestor.digest()),
                                Annotation::Finalized(handler::Finalized::ByHeight {
                                    height: Height::new(2)
                                })
                            ),
                        ]
                    );
                }
                let metrics = failures(&snapshotter);
                let expected = reason
                    .map(|r| (("finalized".to_string(), r.to_string()), 1))
                    .into_iter()
                    .collect();
                assert_eq!(metrics, expected);
                for reason in ["oversized", "delivery_rejected"] {
                    assert_eq!(
                        metrics
                            .get(&("block".to_string(), reason.to_string()))
                            .copied()
                            .unwrap_or_default(),
                        0
                    );
                }
            });
        });
    }
}
