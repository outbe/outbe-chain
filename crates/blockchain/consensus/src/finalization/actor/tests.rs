use crate::block::ConsensusBlock;
use crate::digest::Digest;
use alloy_primitives::B256;
use outbe_primitives::OutbeHeader;
use reth_ethereum::{primitives::SealedBlock, Block};

/// Build a minimal `ConsensusBlock` with the given height and a salt
/// stored in `extra_data` so distinct salts produce distinct sealed
/// hashes (and therefore distinct `Digest`s).
fn make_block(number: u64, salt: u64) -> ConsensusBlock {
    let mut block = Block::default();
    block.header.number = number;
    block.header.extra_data = salt.to_le_bytes().to_vec().into();
    let block = block.map_header(OutbeHeader::new);
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
}

fn digest_of(block: &ConsensusBlock) -> Digest {
    block.digest()
}

/// `FinalizationActor::process_finalization` rekeys the
/// reporter-buffered (view-keyed) late finalize votes to the now-known block
/// number. Drives the real actor method directly with an already-resolved
/// block (no marshal needed - the deps carry `marshal_mailbox: None`).
#[test]
fn rekey_via_finalization_actor() {
    use super::{FinalizationActor, FinalizationActorDeps};
    use crate::finalization::ingress::Finalized;
    use crate::finalization::late_sig_store;
    use crate::finalization::parent_cert_store::FinalizedParentCertStore;
    use crate::finalization::state::new_finalization_view;
    use crate::hybrid::HybridSchemeProvider;
    use crate::vrf_safety::VrfSafetyGate;
    use alloy_primitives::{Address, Bytes};
    use commonware_consensus::types::{Epoch, Round, View};
    use commonware_cryptography::{bls12381, Signer as _};
    use commonware_math::algebra::Random as _;
    use commonware_runtime::Runner as _;
    use outbe_primitives::consensus::{ConsensusData, FinalizedParentCertificateData};

    const WINDOW_K: u64 = 3;
    let (epoch, view, parent_view, fb_number) = (0u64, 9u64, 8u64, 10u64);

    // Shared store + a buffered (view-keyed) vote, as the reporter would have
    // recorded it before the block number was known. The signature value is
    // arbitrary here. The rekey path never verifies it.
    let block = make_block(fb_number, 7);
    let digest = digest_of(&block);
    let fb_hash = digest.0;

    let store = late_sig_store::shared(WINDOW_K);
    let key = bls12381::PrivateKey::random(rand_core_commonware::UnwrapErr(
        rand_commonware::rngs::SysRng,
    ));
    let sig = key.sign(b"x", b"y");
    store.lock().expect("store").record_bound_individual_vote(
        late_sig_store::FinalizeVoteTarget {
            epoch,
            view,
            parent_view,
            fb_hash,
        },
        0,
        &sig,
    );
    assert_eq!(store.lock().expect("store").pending_vote_count(fb_hash), 1);

    let committee: Vec<Address> = (0..4).map(|i| Address::with_last_byte(i + 1)).collect();
    let finalized = Finalized {
        round: Round::new(Epoch::new(epoch), View::new(view)),
        digest,
        vrf_seed: None,
        consensus_data: ConsensusData {
            finalized_block_number: fb_number,
            finalized_block_hash: digest.0,
            finalized_certificate: FinalizedParentCertificateData {
                epoch,
                view,
                parent_view,
                ordered_committee: committee,
                signer_bitmap: vec![1, 0, 0, 0],
                encoded_certificate: Bytes::new(),
            },
            vrf_seed: None,
            missed_proposers: Vec::new(),
        },
    };

    let deps = FinalizationActorDeps {
        view: new_finalization_view(B256::ZERO, 0, None),
        block_cache: crate::finalization::block_cache::BlockCache::new(),
        marshal_mailbox: None,
        bridge: None,
        dkg_manager: crate::dkg_manager::Mailbox::new(),
        vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
        parent_cert_store: FinalizedParentCertStore::new(),
        certificate_scheme_provider: HybridSchemeProvider::default(),
        late_sig_store: store.clone(),
    };
    let (actor, _mailbox) = FinalizationActor::new(deps);

    commonware_runtime::tokio::Runner::default().start(|_ctx| async move {
        actor
            .process_finalization(finalized, block)
            .await
            .expect("process_finalization should rekey without error");
    });

    // The fb_hash-keyed buffer was rekeyed to the finalized block number.
    let s = store.lock().expect("store");
    assert_eq!(
        s.pending_vote_count(fb_hash),
        0,
        "pending buffer must be drained after rekey"
    );
    assert_eq!(s.resolved_len(), 1, "exactly one target resolved by number");
}

/// One snapshot of the stall and divergence metrics.
///
/// `DebuggingRecorder::snapshot` swaps each counter and gauge back to zero.
/// Restore the values just read so a later checkpoint sees the absolute total
/// rather than the delta since this read. The actor is parked on a timer while
/// this runs, so nothing else updates these metrics between the swap and the restore.
fn snapshot_alarm(snapshotter: &metrics_util::debugging::Snapshotter) -> (u64, u64, f64) {
    use metrics_util::debugging::DebugValue;

    let snapshot = snapshotter.snapshot().into_vec();
    let mut stalled = 0u64;
    let mut divergence = 0u64;
    let mut gauge = 0.0;
    let mut saw_gauge = false;
    for (key, _, _, value) in &snapshot {
        match (key.key().name(), value) {
            ("outbe_finalization_resolution_stalled_total", DebugValue::Counter(value)) => {
                stalled = *value;
            }
            ("outbe_finalization_divergence_suspected_total", DebugValue::Counter(value)) => {
                divergence = *value;
            }
            ("outbe_finalization_divergence_suspected", DebugValue::Gauge(value)) => {
                gauge = value.into_inner();
                saw_gauge = true;
            }
            _ => {}
        }
    }
    if stalled > 0 {
        metrics::counter!("outbe_finalization_resolution_stalled_total").increment(stalled);
    }
    if divergence > 0 {
        metrics::counter!("outbe_finalization_divergence_suspected_total").increment(divergence);
    }
    if saw_gauge {
        metrics::gauge!("outbe_finalization_divergence_suspected").set(gauge);
    }
    (stalled, divergence, gauge)
}

fn is_pending<F: std::future::Future + ?Sized>(fut: std::pin::Pin<&mut F>) -> bool {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    fut.poll(&mut cx).is_pending()
}

/// Unstarted marshal actor. `subscribe_by_digest` oneshots stay pending and
/// each attempt times out instead of failing closed.
async fn pending_marshal(
    context: &commonware_runtime::deterministic::Context,
) -> (
    crate::marshal_types::MarshalActor<commonware_runtime::deterministic::Context>,
    crate::marshal_types::MarshalMailbox,
) {
    use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};

    use commonware_consensus::{marshal, types::ViewDelta};
    use commonware_parallel::Sequential;
    use commonware_runtime::buffer::paged::CacheRef;
    use commonware_runtime::Supervisor as _;

    let cache = CacheRef::from_pooler(
        context,
        NonZeroU16::new(1024).unwrap(),
        NonZeroUsize::new(10).unwrap(),
    );
    let items = NonZeroU64::new(10).unwrap();
    let buffer = NonZeroUsize::new(1024).unwrap();
    let (certificates, blocks) = crate::test_fixtures::marshal::MarshalArchiveFixture {
        partition_prefix: "divergence-alarm",
        page_cache: &cache,
        items_per_section: items,
        replay_buffer: buffer,
        write_buffer: buffer,
    }
    .open(context)
    .await
    .expect("marshal archives open");
    let genesis = Block::default().map_header(OutbeHeader::new);
    let (marshal_actor, mailbox, _) = marshal::core::Actor::init(
        context.child("marshal"),
        certificates,
        blocks,
        marshal::Config {
            provider: crate::hybrid::HybridSchemeProvider::new(),
            epocher: crate::follow::FollowerEpocher::new(100, 10),
            start: marshal::Start::Genesis(ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                genesis,
            ))),
            partition_prefix: "divergence-alarm".into(),
            mailbox_size: NonZeroUsize::new(64).unwrap(),
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
    (marshal_actor, mailbox)
}

/// A digest that marshal never serves must stall. The divergence gauge latches
/// once at `STALL_DIVERGENCE_ALARM` and stays up while the actor keeps running.
/// `record_block_finalized` does not clear it.
#[test]
fn resolution_divergence_alarm() {
    use std::time::Duration;

    use commonware_consensus::types::{Epoch, Round, View};
    use commonware_runtime::{
        deterministic, Clock as _, Runner as _, Spawner as _, Supervisor as _,
    };
    use metrics_util::debugging::DebuggingRecorder;

    use super::{FinalizationActor, FinalizationActorDeps};
    use crate::finalization::ingress::Finalized;
    use crate::finalization::parent_cert_store::{
        CertifiedParentProofStore as _, FinalizedParentCertStore,
    };
    use crate::finalization::state::new_finalization_view;
    use crate::hybrid::HybridSchemeProvider;
    use crate::vrf_safety::VrfSafetyGate;
    use outbe_primitives::consensus::ConsensusData;

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        deterministic::Runner::timed(Duration::from_secs(600)).start(|context| async move {
            let cycle = crate::config::FINALIZE_RESOLUTION_TIMEOUT
                * crate::config::FINALIZE_MAX_RETRIES
                + crate::config::FINALIZE_RETRY_DELAY * (crate::config::FINALIZE_MAX_RETRIES - 1);
            let alarm = crate::config::STALL_DIVERGENCE_ALARM;
            assert_eq!(alarm, Duration::from_secs(290));
            assert_eq!(
                alarm,
                cycle * crate::config::FINALIZE_DIVERGENCE_ALARM_CYCLES
            );

            let (marshal_actor, marshal_mailbox) = pending_marshal(&context).await;
            let view = new_finalization_view(B256::ZERO, 0, None);
            let parent_store = FinalizedParentCertStore::new();
            let deps = FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal_mailbox),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: parent_store.clone(),
                certificate_scheme_provider: HybridSchemeProvider::default(),
                late_sig_store: crate::finalization::late_sig_store::shared(3),
            };
            let (actor, ingress) = FinalizationActor::new(deps);
            let digest = Digest(B256::repeat_byte(0x42));
            ingress
                .notify_finalized(Finalized {
                    round: Round::new(Epoch::new(1), View::new(1)),
                    digest,
                    vrf_seed: None,
                    consensus_data: ConsensusData::default(),
                })
                .expect("actor mailbox is open");

            let actor_task = context.child("finalization").spawn(move |ctx| async move {
                actor.run(ctx).await.expect("stall must not stop the actor");
            });
            let mut actor_task = std::pin::pin!(actor_task);

            context.sleep(cycle + Duration::from_secs(1)).await;
            let (stalled, divergence, gauge) = snapshot_alarm(&snapshotter);
            assert!(
                stalled > 0,
                "one retry cycle must stall before 290s, stalled={stalled}"
            );
            assert_eq!(
                divergence, 0,
                "divergence counter stays at 0 below the alarm"
            );
            assert_eq!(gauge, 0.0);

            context.sleep(alarm - cycle).await;
            let (_stalled, divergence, gauge) = snapshot_alarm(&snapshotter);
            assert_eq!(divergence, 1, "five timeout cycles raise the alarm once");
            assert_eq!(gauge, 1.0);

            context.sleep(cycle).await;
            let (_stalled, divergence, gauge) = snapshot_alarm(&snapshotter);
            assert_eq!(
                divergence, 1,
                "the sixth cycle does not raise the alarm again"
            );
            assert_eq!(gauge, 1.0);
            assert!(
                is_pending(actor_task.as_mut()),
                "actor keeps retrying after the sixth cycle"
            );
            assert_eq!(view.read().last_finalized_number, 0);
            assert!(view.read().last_finalized_round.is_none());
            assert_eq!(parent_store.len(), 0);

            crate::metrics::record_block_finalized(10, 4, 4);
            let (_stalled, divergence, gauge) = snapshot_alarm(&snapshotter);
            assert_eq!(divergence, 1);
            assert_eq!(
                gauge, 1.0,
                "record_block_finalized must not clear the divergence gauge"
            );

            drop(ingress);
            drop(marshal_actor);
        });
    });
}
