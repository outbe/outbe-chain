use super::super::resolver::{FetchResolution, ResolverKey};
use super::*;
use commonware_consensus::marshal::resolver::handler::{self, Annotation, Finalized, Key};
use commonware_resolver::{Consumer, Delivery, Fetch};
use commonware_utils::channel::oneshot;
use std::{collections::VecDeque, sync::Mutex};
use upstream::AncestorFinalityProof;

type Reads = Arc<Mutex<Vec<&'static str>>>;
#[derive(Clone)]
struct Source {
    block: Option<crate::block::ConsensusBlock>,
    proof: Option<AncestorFinalityProof>,
    reads: Reads,
}
impl FinalizedSource for Source {
    async fn get_finalization(&self, _: Height) -> Option<CertifiedFinalizedBlock> {
        self.proof.as_ref().map(|proof| proof.certified.clone())
    }
    async fn get_finality_proof(&self, _: Height) -> Option<AncestorFinalityProof> {
        self.reads.lock().unwrap().push("proof");
        self.proof.clone()
    }
    async fn get_block(&self, _: Height) -> Option<crate::block::ConsensusBlock> {
        self.reads.lock().unwrap().push("upstream");
        self.block.clone()
    }
}
#[derive(Clone)]
struct Local {
    block: Option<crate::block::ConsensusBlock>,
    reads: Reads,
}
impl LocalBlockSource for Local {
    async fn get_block_by_digest(&self, _: Digest) -> Option<crate::block::ConsensusBlock> {
        self.reads.lock().unwrap().push("local");
        self.block.clone()
    }
}
#[derive(Clone, Copy)]
enum Ack {
    Accept,
    Reject,
    Closed,
    Hold,
}
type Deliveries = Arc<Mutex<Vec<(ResolverKey, Vec<Annotation>, bytes::Bytes)>>>;
#[derive(Clone)]
struct Recorder {
    deliveries: Deliveries,
    acknowledgements: VecDeque<Ack>,
    pending: Arc<Mutex<Option<oneshot::Sender<bool>>>>,
}
impl Recorder {
    fn new(acknowledgements: impl IntoIterator<Item = Ack>) -> Self {
        Self {
            deliveries: Arc::default(),
            acknowledgements: acknowledgements.into_iter().collect(),
            pending: Arc::default(),
        }
    }
}
impl Consumer for Recorder {
    type Key = ResolverKey;
    type Value = bytes::Bytes;
    type Subscriber = Annotation;
    type Outcome = bool;
    fn deliver(
        &mut self,
        delivery: Delivery<ResolverKey, Annotation>,
        value: bytes::Bytes,
    ) -> oneshot::Receiver<bool> {
        self.deliveries.lock().unwrap().push((
            delivery.key,
            delivery.subscribers.into_iter().map(|(a, _)| a).collect(),
            value,
        ));
        let (sender, receiver) = oneshot::channel();
        match self
            .acknowledgements
            .pop_front()
            .expect("unexpected marshal delivery")
        {
            Ack::Accept => {
                sender.send(true).unwrap();
            }
            Ack::Reject => {
                sender.send(false).unwrap();
            }
            Ack::Closed => drop(sender),
            Ack::Hold => {
                *self.pending.lock().unwrap() = Some(sender);
            }
        }
        receiver
    }
}
fn request(key: ResolverKey, height: Height) -> Fetch<ResolverKey, Annotation> {
    Fetch {
        key,
        subscriber: Annotation::Finalized(Finalized::ByHeight { height }),
        span: tracing::Span::none(),
    }
}
fn resolution(
    signer: &Committee,
    local: Option<crate::block::ConsensusBlock>,
    source: Source,
) -> FetchResolution<Source, Local> {
    let chain = SharedCommitteeChain::new(CommitteeChain::new(
        Epoch::new(0),
        signer.participants.clone(),
    ));
    chain
        .lock()
        .advance_from_block_extra_data(&signer.boundary_block_extra_data(Epoch::new(0)))
        .unwrap();
    FetchResolution {
        local: Local {
            block: local,
            reads: Arc::clone(&source.reads),
        },
        upstream: source,
        chain,
        epocher: FollowerEpocher::new(10, 0),
    }
}

#[test]
fn local_block_keeps_ack_receiver_alive_until_response_or_cancellation() {
    let signer = committee(110);
    let record = certified_block(&signer, Epoch::new(0), 2, Vec::new());
    for cancel in [false, true] {
        let reads = Arc::default();
        let recorder = Recorder::new([Ack::Hold]);
        let pending = Arc::clone(&recorder.pending);
        let delivered = Arc::clone(&recorder.deliveries);
        let resolver = resolution(
            &signer,
            Some(record.block.clone()),
            Source {
                block: None,
                proof: None,
                reads: Arc::clone(&reads),
            },
        );
        futures::executor::block_on(async {
            let mut resolving = Box::pin(resolver.resolve(
                request(Key::Block(record.block.digest()), Height::new(2)),
                recorder,
            ));
            assert!(futures::poll!(resolving.as_mut()).is_pending());
            assert_eq!(*reads.lock().unwrap(), ["local"]);
            let sender = pending.lock().unwrap().take().unwrap();
            assert!(!sender.is_closed());
            assert_eq!(delivered.lock().unwrap()[0].2, record.block.encode());
            if cancel {
                drop(resolving);
                assert!(sender.is_closed());
            } else {
                sender.send(true).unwrap();
                resolving.await;
            }
        });
    }
}
#[test]
fn upstream_block_requires_height_and_digest_and_round_only_fetches_do_not_read_upstream() {
    let signer = committee(110);
    let record = certified_block(&signer, Epoch::new(0), 2, Vec::new());
    for (height, digest, accepted) in [
        (2, record.block.digest(), true),
        (3, record.block.digest(), false),
        (2, Digest::from(B256::ZERO), false),
    ] {
        let reads = Arc::default();
        let recorder = Recorder::new([Ack::Accept]);
        let delivered = Arc::clone(&recorder.deliveries);
        futures::executor::block_on(
            resolution(
                &signer,
                None,
                Source {
                    block: Some(record.block.clone()),
                    proof: None,
                    reads: Arc::clone(&reads),
                },
            )
            .resolve(request(Key::Block(digest), Height::new(height)), recorder),
        );
        assert_eq!(*reads.lock().unwrap(), ["local", "upstream"]);
        assert_eq!(delivered.lock().unwrap().len(), usize::from(accepted));
    }
    let reads = Arc::default();
    let recorder = Recorder::new([]);
    let fetch = Fetch {
        key: Key::Block(record.block.digest()),
        subscriber: Annotation::Notarization {
            round: Round::new(Epoch::new(0), View::new(1)),
        },
        span: tracing::Span::none(),
    };
    futures::executor::block_on(
        resolution(
            &signer,
            None,
            Source {
                block: None,
                proof: None,
                reads: Arc::clone(&reads),
            },
        )
        .resolve(fetch, recorder),
    );
    assert_eq!(*reads.lock().unwrap(), ["local"]);
}
fn linked_records(signer: &Committee) -> Vec<CertifiedFinalizedBlock> {
    use reth_ethereum::{primitives::SealedBlock, Block};
    let mut parent = B256::ZERO;
    (1..=4)
        .map(|height| {
            let mut raw = Block::default();
            raw.header.number = height;
            raw.header.parent_hash = parent;
            let block = crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                raw.map_header(outbe_primitives::OutbeHeader::new),
            ));
            parent = block.block_hash();
            CertifiedFinalizedBlock {
                finalization: signer.finalization_for(Epoch::new(0), block.digest()),
                block,
            }
        })
        .collect()
}
#[test]
fn finalized_anchor_precedes_reversed_ancestors_and_rejection_stops_delivery() {
    let signer = committee(110);
    let records = linked_records(&signer);
    let proof = AncestorFinalityProof {
        certified: records[3].clone(),
        ancestors: vec![records[1].block.clone(), records[2].block.clone()],
    };
    for (acks, count) in [
        (vec![Ack::Accept, Ack::Accept, Ack::Accept], 3),
        (vec![Ack::Reject], 1),
        (vec![Ack::Accept, Ack::Reject], 2),
        (vec![Ack::Closed], 1),
    ] {
        let recorder = Recorder::new(acks);
        let delivered = Arc::clone(&recorder.deliveries);
        futures::executor::block_on(
            resolution(
                &signer,
                None,
                Source {
                    block: None,
                    proof: Some(proof.clone()),
                    reads: Arc::default(),
                },
            )
            .resolve(
                request(
                    Key::Finalized {
                        height: Height::new(2),
                    },
                    Height::new(2),
                ),
                recorder,
            ),
        );
        let deliveries = delivered.lock().unwrap();
        assert_eq!(deliveries.len(), count);
        assert_eq!(
            deliveries[0].0,
            Key::Finalized {
                height: Height::new(4)
            }
        );
        let mut anchor = records[3].finalization.encode().to_vec();
        anchor.extend_from_slice(records[3].block.encode().as_ref());
        assert_eq!(deliveries[0].2.as_ref(), anchor);
        if count > 1 {
            assert_eq!(deliveries[1].0, Key::Block(records[2].block.digest()));
        }
        if count > 2 {
            assert_eq!(deliveries[2].0, Key::Block(records[1].block.digest()));
        }
    }
}

#[test]
fn invalid_ancestor_chain_never_reaches_marshal_delivery() {
    let signer = committee(110);
    let records = linked_records(&signer);
    let proof = AncestorFinalityProof {
        certified: records[3].clone(),
        ancestors: vec![records[1].block.clone(), records[2].block.clone()],
    };
    let mut corrupt = proof;
    corrupt.ancestors[0] = records[0].block.clone();
    let recorder = Recorder::new([]);
    let delivered = Arc::clone(&recorder.deliveries);
    futures::executor::block_on(
        resolution(
            &signer,
            None,
            Source {
                block: None,
                proof: Some(corrupt),
                reads: Arc::default(),
            },
        )
        .resolve(
            request(
                Key::Finalized {
                    height: Height::new(2),
                },
                Height::new(2),
            ),
            recorder,
        ),
    );
    assert!(delivered.lock().unwrap().is_empty());
}

#[test]
fn values_above_the_delivery_cap_are_never_encoded_or_delivered() {
    use reth_ethereum::{primitives::SealedBlock, Block};
    let signer = committee(110);
    let mut raw = Block::default();
    raw.header.number = 2;
    raw.header.extra_data = vec![0u8; crate::config::MAX_P2P_MESSAGE_SIZE as usize + 1].into();
    let oversized = crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(
        raw.map_header(outbe_primitives::OutbeHeader::new),
    ));
    for local in [true, false] {
        let reads = Arc::default();
        let recorder = Recorder::new([]);
        let delivered = Arc::clone(&recorder.deliveries);
        futures::executor::block_on(
            resolution(
                &signer,
                local.then(|| oversized.clone()),
                Source {
                    block: Some(oversized.clone()),
                    proof: None,
                    reads: Arc::clone(&reads),
                },
            )
            .resolve(
                request(Key::Block(oversized.digest()), Height::new(2)),
                recorder,
            ),
        );
        assert!(delivered.lock().unwrap().is_empty());
    }
}

#[test]
fn proof_bundle_above_the_aggregate_cap_is_dropped_before_authentication() {
    use reth_ethereum::{primitives::SealedBlock, Block};
    let signer = committee(110);
    // Nine linked blocks of ~1.5 MiB each (ommer padding, so the header
    // artifacts stay valid), all inside the fixture's first epoch: every one fits
    // the per-delivery cap, while the anchor plus its seven ancestors exceed the
    // bundle cap.
    let mut parent = B256::ZERO;
    let records: Vec<CertifiedFinalizedBlock> = (1..=9)
        .map(|height| {
            let mut raw = Block::default();
            raw.header.number = height;
            raw.header.parent_hash = parent;
            raw.body.ommers = vec![alloy_consensus::Header::default(); 3 * 1024];
            let block = crate::block::ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                raw.map_header(outbe_primitives::OutbeHeader::new),
            ));
            parent = block.block_hash();
            CertifiedFinalizedBlock {
                finalization: signer.finalization_for(Epoch::new(0), block.digest()),
                block,
            }
        })
        .collect();
    let proof = AncestorFinalityProof {
        certified: records[8].clone(),
        ancestors: records[1..8]
            .iter()
            .map(|record| record.block.clone())
            .collect(),
    };
    let recorder = Recorder::new([]);
    let delivered = Arc::clone(&recorder.deliveries);
    futures::executor::block_on(
        resolution(
            &signer,
            None,
            Source {
                block: None,
                proof: Some(proof),
                reads: Arc::default(),
            },
        )
        .resolve(
            request(
                Key::Finalized {
                    height: Height::new(2),
                },
                Height::new(2),
            ),
            recorder,
        ),
    );
    assert!(delivered.lock().unwrap().is_empty());
}

#[derive(Clone)]
struct RetryingSource {
    block: crate::block::ConsensusBlock,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    active: Arc<std::sync::atomic::AtomicUsize>,
    peak: Arc<std::sync::atomic::AtomicUsize>,
    stall: bool,
}
struct ActiveRead(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for ActiveRead {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl FinalizedSource for RetryingSource {
    async fn get_finalization(&self, _: Height) -> Option<CertifiedFinalizedBlock> {
        None
    }
    async fn get_block(&self, _: Height) -> Option<crate::block::ConsensusBlock> {
        use std::sync::atomic::Ordering::SeqCst;
        let call = self.calls.fetch_add(1, SeqCst);
        let active = self.active.fetch_add(1, SeqCst) + 1;
        self.peak.fetch_max(active, SeqCst);
        let _active = ActiveRead(self.active.clone());
        if self.stall {
            return std::future::pending().await;
        }
        if call == 0 {
            None
        } else {
            Some(self.block.clone())
        }
    }
}

#[test]
fn opaque_retries_transient_source_failure_without_a_new_hint_and_deduplicates() {
    use commonware_resolver::Resolver as _;
    use commonware_runtime::{Clock as _, Runner as _, Supervisor as _};
    use std::sync::atomic::Ordering::SeqCst;
    let signer = committee(160);
    let block = certified_block(&signer, Epoch::new(0), 2, Vec::new()).block;
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(5)).start(
        |context| async move {
            let source = RetryingSource {
                block: block.clone(),
                calls: Arc::default(),
                active: Arc::default(),
                peak: Arc::default(),
                stall: false,
            };
            let calls = source.calls.clone();
            let base = resolution(
                &signer,
                None,
                Source {
                    block: None,
                    proof: None,
                    reads: Arc::default(),
                },
            );
            let (receiver, handler) = handler::init(
                context.child("handler"),
                std::num::NonZeroUsize::new(16).unwrap(),
            );
            let mut resolver = super::super::resolver::init(
                context.child("resolver"),
                handler,
                FetchResolution {
                    upstream: source,
                    local: base.local,
                    chain: base.chain,
                    epocher: base.epocher,
                },
                std::num::NonZeroUsize::new(16).unwrap(),
            );
            let fetch = request(Key::Block(block.digest()), Height::new(2));
            assert!(resolver.fetch(fetch.clone()).accepted());
            assert!(resolver.fetch(fetch).accepted());
            context.sleep(std::time::Duration::from_millis(750)).await;
            assert_eq!(
                calls.load(SeqCst),
                2,
                "first failure must retry automatically; duplicate hints share acquisition"
            );
            assert!(resolver.retain(|_, _| false).accepted());
            context.sleep(std::time::Duration::from_secs(2)).await;
            assert_eq!(
                calls.load(SeqCst),
                2,
                "retain must cancel delivery and further retries"
            );
            drop(receiver);
        },
    );
}

#[test]
fn opaque_acquisition_cap_and_retain_cancel_stalled_rpc_reads() {
    use commonware_resolver::Resolver as _;
    use commonware_runtime::{Clock as _, Runner as _, Supervisor as _};
    use std::sync::atomic::Ordering::SeqCst;
    let signer = committee(170);
    let block = certified_block(&signer, Epoch::new(0), 2, Vec::new()).block;
    commonware_runtime::deterministic::Runner::timed(std::time::Duration::from_secs(5)).start(
        |context| async move {
            let source = RetryingSource {
                block,
                calls: Arc::default(),
                active: Arc::default(),
                peak: Arc::default(),
                stall: true,
            };
            let (active, peak) = (source.active.clone(), source.peak.clone());
            let base = resolution(
                &signer,
                None,
                Source {
                    block: None,
                    proof: None,
                    reads: Arc::default(),
                },
            );
            let (_receiver, handler) = handler::init(
                context.child("handler"),
                std::num::NonZeroUsize::new(16).unwrap(),
            );
            let mut resolver = super::super::resolver::init(
                context.child("resolver"),
                handler,
                FetchResolution {
                    upstream: source,
                    local: base.local,
                    chain: base.chain,
                    epocher: base.epocher,
                },
                std::num::NonZeroUsize::new(16).unwrap(),
            );
            for i in 0..12u8 {
                assert!(resolver
                    .fetch(request(
                        Key::Block(Digest(alloy_primitives::B256::with_last_byte(i))),
                        Height::new(2)
                    ))
                    .accepted());
            }
            context.sleep(std::time::Duration::from_millis(100)).await;
            assert_eq!(active.load(SeqCst), 8);
            assert_eq!(peak.load(SeqCst), 8);
            assert!(resolver.retain(|_, _| false).accepted());
            context.sleep(std::time::Duration::from_secs(1)).await;
            assert_eq!(
                active.load(SeqCst),
                0,
                "opaque cancellation drops in-flight RPC futures"
            );
        },
    );
}
