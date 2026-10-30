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

use crate::finalization::committee_prelude::build_committee_prelude;
use crate::finalization::ingress::Finalized;
use crate::finalization::late_sig_store::{self, FinalizeVoteTarget};
use crate::finalization::parent_cert_store::{
    CertifiedParentProofKey, CertifiedParentProofRecord, CertifiedParentProofStore as _,
    FinalizedParentCertStore, ParentProofSelection, ProofKind,
    CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
};
use crate::finalization::state::new_finalization_view;
use crate::hybrid::{HybridScheme, HybridSchemeProvider};
use crate::vrf_safety::VrfSafetyGate;
use alloy_primitives::{Address, Bytes};
use commonware_consensus::types::{Epoch, Round, View};
use commonware_cryptography::bls12381::{self, primitives::variant::MinSig};
use commonware_cryptography::Signer as _;
use commonware_runtime::{deterministic, Runner as _};
use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use outbe_primitives::consensus::{ConsensusData, FinalizedParentCertificateData};
use std::time::Duration;

use super::{FinalizationActor, FinalizationActorDeps, FINALIZE_PERSIST_MAX_ATTEMPTS};

type SchemeProvider = HybridSchemeProvider<MinSig>;

#[derive(Debug, PartialEq)]
struct DegradeMetrics {
    snapshot_build_degraded: u64,
    persist_degraded: u64,
    persist_gauge: f64,
}

fn read_degrade_metrics(snapshotter: &metrics_util::debugging::Snapshotter) -> DegradeMetrics {
    let snapshot = snapshotter.snapshot().into_vec();
    let mut metrics = DegradeMetrics {
        snapshot_build_degraded: 0,
        persist_degraded: 0,
        persist_gauge: 0.0,
    };
    for (key, _, _, value) in &snapshot {
        match (key.key().name(), value) {
            ("outbe_finalization_snapshot_build_degraded_total", DebugValue::Counter(value)) => {
                metrics.snapshot_build_degraded = *value;
                if *value > 0 {
                    metrics::counter!("outbe_finalization_snapshot_build_degraded_total")
                        .increment(*value);
                }
            }
            ("outbe_finalization_persist_degraded_total", DebugValue::Counter(value)) => {
                metrics.persist_degraded = *value;
                if *value > 0 {
                    metrics::counter!("outbe_finalization_persist_degraded_total")
                        .increment(*value);
                }
            }
            ("outbe_finalization_persist_degraded", DebugValue::Gauge(value)) => {
                metrics.persist_gauge = value.into_inner();
                metrics::gauge!("outbe_finalization_persist_degraded").set(metrics.persist_gauge);
            }
            _ => {}
        }
    }
    metrics
}

fn scheme_provider(epoch: u64, participants: u8) -> SchemeProvider {
    let provider = SchemeProvider::default();
    let (_keys, participant_set) = crate::hybrid::test_support::test_participants(participants);
    let dkg = crate::bls::bootstrap_dkg(u32::from(participants)).expect("bootstrap dkg");
    let scheme = HybridScheme::<MinSig>::verifier(
        b"oip-00159-finalization",
        participant_set,
        dkg.polynomial,
    )
    .expect("verifier scheme");
    assert!(provider.register(Epoch::new(epoch), scheme));
    provider
}

fn addresses(n: u8) -> Vec<Address> {
    (0..n).map(|i| Address::with_last_byte(i + 1)).collect()
}

fn finalized_event(
    block: &ConsensusBlock,
    epoch: u64,
    view: u64,
    parent_view: u64,
    committee: Vec<Address>,
) -> Finalized {
    let digest = digest_of(block);
    Finalized {
        round: Round::new(Epoch::new(epoch), View::new(view)),
        digest,
        vrf_seed: None,
        consensus_data: ConsensusData {
            finalized_block_number: block.number(),
            finalized_block_hash: digest.0,
            finalized_certificate: FinalizedParentCertificateData {
                epoch,
                view,
                parent_view,
                ordered_committee: committee,
                signer_bitmap: vec![1],
                encoded_certificate: Bytes::new(),
            },
            vrf_seed: None,
            missed_proposers: Vec::new(),
        },
    }
}

struct ActorHarness {
    actor: FinalizationActor,
    view: crate::finalization::state::FinalizationViewHandle,
    store: FinalizedParentCertStore,
    late: late_sig_store::SharedLateFinalizeStore,
    cache: crate::finalization::block_cache::BlockCache,
}

fn harness(provider: SchemeProvider) -> ActorHarness {
    let view = new_finalization_view(B256::ZERO, 0, None);
    let store = FinalizedParentCertStore::new();
    let late = late_sig_store::shared(3);
    let cache = crate::finalization::block_cache::BlockCache::new();
    let deps = FinalizationActorDeps {
        view: view.clone(),
        block_cache: cache.clone(),
        marshal_mailbox: None,
        bridge: None,
        dkg_manager: crate::dkg_manager::Mailbox::new(),
        vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
        parent_cert_store: store.clone(),
        certificate_scheme_provider: provider,
        late_sig_store: late.clone(),
    };
    let (actor, _mailbox) = FinalizationActor::new(deps);
    ActorHarness {
        actor,
        view,
        store,
        late,
        cache,
    }
}

fn buffer_vote(
    late: &late_sig_store::SharedLateFinalizeStore,
    epoch: u64,
    view: u64,
    parent_view: u64,
    fb_hash: B256,
) {
    let key = bls12381::PrivateKey::from_seed(1);
    let sig = key.sign(b"x", b"y");
    late.lock()
        .expect("late store")
        .record_bound_individual_vote(
            FinalizeVoteTarget {
                epoch,
                view,
                parent_view,
                fb_hash,
            },
            0,
            &sig,
        );
}

fn finalization_error(error: eyre::Report) -> crate::errors::FinalizationError {
    error
        .downcast::<crate::errors::FinalizationError>()
        .unwrap_or_else(|error| panic!("expected FinalizationError, got {error:#}"))
}

fn insert_canonical_cn_witness(
    store: &FinalizedParentCertStore,
    epoch: u64,
    view: u64,
    parent_view: u64,
    block_hash: B256,
    committee: &[Address],
) {
    let valid_provider = scheme_provider(epoch, committee.len() as u8);
    let scheme = valid_provider.scoped(Epoch::new(epoch)).expect("scheme");
    let prelude =
        build_committee_prelude(scheme.as_ref(), committee, epoch).expect("canonical prelude");
    store
        .put_certified_notarization(CertifiedParentProofRecord {
            format_version: CERTIFIED_PARENT_PROOF_RECORD_FORMAT_VERSION,
            kind: ProofKind::CertifiedNotarization,
            finalized_block_hash: block_hash,
            finalized_epoch: epoch,
            finalized_view: view,
            parent_view,
            ordered_committee: committee.to_vec(),
            signer_bitmap: vec![1; committee.len()],
            encoded_proof: Bytes::from_static(b"cn-witness"),
            committee_set_hash: prelude.committee_set_hash,
            vrf_material_version: prelude.vrf_material_version,
            vrf_group_public_key_hash: prelude.vrf_group_public_key_hash,
        })
        .expect("certified-notarization witness");
}

async fn finalize_step(harness: &ActorHarness, number: u64, epoch: u64, committee: &[Address]) {
    let block = make_block(number, number);
    let event = finalized_event(&block, epoch, number, number - 1, committee.to_vec());
    harness
        .actor
        .process_finalization(event, block)
        .await
        .expect("persist failure must not halt finalization");
    assert_eq!(harness.view.read().last_finalized_number, number);
}

/// Probe 1: a committee/pubkey count mismatch skips the finalization write and
/// the late-finalize rekey. The certified-notarization witness stays readable.
#[test]
fn snapshot_mismatch_is_contained_without_halting() {
    const EPOCH: u64 = 1;
    const VIEW: u64 = 9;
    const PARENT_VIEW: u64 = 8;
    const BLOCK_NUMBER: u64 = 10;

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|_ctx| async move {
            let provider = scheme_provider(EPOCH, 3);
            let committee = addresses(4);
            let harness = harness(provider);
            let block = make_block(BLOCK_NUMBER, 7);
            let digest = digest_of(&block);
            let proof_key = CertifiedParentProofKey::new(EPOCH, VIEW, digest.0);
            insert_canonical_cn_witness(
                &harness.store,
                EPOCH,
                VIEW,
                PARENT_VIEW,
                digest.0,
                &committee,
            );
            buffer_vote(&harness.late, EPOCH, VIEW, PARENT_VIEW, digest.0);
            assert_eq!(
                harness
                    .late
                    .lock()
                    .expect("late store")
                    .pending_vote_count(digest.0),
                1
            );

            let finalized = finalized_event(&block, EPOCH, VIEW, PARENT_VIEW, committee);
            harness
                .actor
                .process_finalization(finalized, block)
                .await
                .expect("snapshot mismatch must not halt finalization");

            assert_eq!(harness.view.read().last_finalized_number, BLOCK_NUMBER);
            let metrics = read_degrade_metrics(&snapshotter);
            assert_eq!(metrics.snapshot_build_degraded, 1);
            assert!(harness.store.get_finalization(proof_key).is_none());
            match harness.store.get_best_for_parent(proof_key, BLOCK_NUMBER) {
                Some(ParentProofSelection::CertifiedNotarization(witness)) => {
                    assert_ne!(witness.committee_set_hash, B256::ZERO);
                    assert_eq!(witness.signer_bitmap.len(), 4);
                }
                other => panic!("expected CertifiedNotarization witness, got {other:?}"),
            }
            assert_eq!(harness.late.lock().expect("late store").resolved_len(), 0);
        });
    });
}

/// Probe 2: store writes and prunes retry inside the budget, then degrade
/// without stopping finalization. A later clean cycle clears the gauge.
#[test]
fn store_write_and_prune_errors_retry_and_degrade_without_halting() {
    const EPOCH: u64 = 1;
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    metrics::with_local_recorder(&recorder, || {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|_ctx| async move {
            let provider = scheme_provider(EPOCH, 4);
            let committee = addresses(4);
            let scheme = provider.scoped(Epoch::new(EPOCH)).expect("scheme");
            let expected_hash = build_committee_prelude(scheme.as_ref(), &committee, EPOCH)
                .expect("matching prelude")
                .committee_set_hash;
            assert_ne!(expected_hash, B256::ZERO);
            let harness = harness(provider);

            // 2a: two injected put failures recover on the third attempt.
            let key_a = CertifiedParentProofKey::new(EPOCH, 20, digest_of(&make_block(20, 20)).0);
            harness
                .store
                .fail_next_put_finalization(FINALIZE_PERSIST_MAX_ATTEMPTS - 1);
            finalize_step(&harness, 20, EPOCH, &committee).await;
            assert!(harness.store.get_finalization(key_a).is_some());
            assert_eq!(
                read_degrade_metrics(&snapshotter),
                DegradeMetrics {
                    snapshot_build_degraded: 0,
                    persist_degraded: 0,
                    persist_gauge: 0.0,
                }
            );

            // 2b: put exhaustion leaves the gauge latched. Prunes still succeed.
            // The canonical committee is still rekeyed.
            let digest_b = digest_of(&make_block(21, 21));
            let key_b = CertifiedParentProofKey::new(EPOCH, 21, digest_b.0);
            buffer_vote(&harness.late, EPOCH, 21, 20, digest_b.0);
            harness
                .store
                .fail_next_put_finalization(FINALIZE_PERSIST_MAX_ATTEMPTS);
            finalize_step(&harness, 21, EPOCH, &committee).await;
            assert!(harness.store.get_finalization(key_b).is_none());
            let batch = harness
                .late
                .lock()
                .expect("late store")
                .build_artifact(22)
                .batches
                .into_iter()
                .find(|batch| batch.fb_number == 21)
                .expect("rekey must keep the canonical committee");
            assert_eq!(batch.committee_set_hash, expected_hash);
            let metrics = read_degrade_metrics(&snapshotter);
            assert_eq!(metrics.persist_degraded, 1);
            assert_eq!(metrics.persist_gauge, 1.0);

            // Clean cycle clears the gauge after 2b.
            finalize_step(&harness, 22, EPOCH, &committee).await;
            assert_eq!(read_degrade_metrics(&snapshotter).persist_gauge, 0.0);

            // 2c-1: isolated prune_below_height exhaustion latches the gauge; clean cycle clears it.
            harness
                .store
                .fail_next_prune_below_height(FINALIZE_PERSIST_MAX_ATTEMPTS);
            finalize_step(&harness, 23, EPOCH, &committee).await;
            let metrics = read_degrade_metrics(&snapshotter);
            assert_eq!(metrics.persist_degraded, 2);
            assert_eq!(metrics.persist_gauge, 1.0);
            finalize_step(&harness, 24, EPOCH, &committee).await;
            assert_eq!(read_degrade_metrics(&snapshotter).persist_gauge, 0.0);

            // 2c-2: isolated prune_cn exhaustion latches the gauge; clean cycle clears it.
            harness
                .store
                .fail_next_prune_cn(FINALIZE_PERSIST_MAX_ATTEMPTS);
            finalize_step(&harness, 25, EPOCH, &committee).await;
            let metrics = read_degrade_metrics(&snapshotter);
            assert_eq!(metrics.persist_degraded, 3);
            assert_eq!(metrics.persist_gauge, 1.0);
            finalize_step(&harness, 26, EPOCH, &committee).await;
            assert_eq!(read_degrade_metrics(&snapshotter).persist_gauge, 0.0);

            // 2c-3: joint exhaustion of both prunes increments counter by 2 and recovers on clean cycle.
            harness
                .store
                .fail_next_prune_below_height(FINALIZE_PERSIST_MAX_ATTEMPTS);
            harness
                .store
                .fail_next_prune_cn(FINALIZE_PERSIST_MAX_ATTEMPTS);
            finalize_step(&harness, 27, EPOCH, &committee).await;
            let metrics = read_degrade_metrics(&snapshotter);
            assert_eq!(metrics.persist_degraded, 5);
            assert_eq!(metrics.persist_gauge, 1.0);
            finalize_step(&harness, 28, EPOCH, &committee).await;
            let metrics = read_degrade_metrics(&snapshotter);
            assert_eq!(metrics.persist_degraded, 5);
            assert_eq!(metrics.persist_gauge, 0.0);
        });
    });
}

/// Probe 3: same-height and same-round conflicts stay fatal.
#[test]
fn same_height_and_same_round_inconsistency_remain_fatal() {
    deterministic::Runner::timed(Duration::from_secs(5)).start(|ctx| async move {
        let height = harness(SchemeProvider::default());
        let first = make_block(30, 1);
        let second = make_block(30, 2);
        height
            .actor
            .process_finalization(finalized_event(&first, 1, 4, 3, addresses(1)), first)
            .await
            .expect("first height finalizes");
        let error = height
            .actor
            .process_finalization(finalized_event(&second, 1, 5, 4, addresses(1)), second)
            .await
            .expect_err("same height must stay fatal");
        assert!(matches!(
            finalization_error(error),
            crate::errors::FinalizationError::SameHeightInconsistency { .. }
        ));

        let round_case = harness(SchemeProvider::default());
        let first = make_block(40, 1);
        let second = make_block(41, 2);
        let first_finalized = finalized_event(&first, 2, 8, 7, addresses(1));
        let round = first_finalized.round;
        round_case
            .cache
            .insert_bounded(digest_of(&first), first.clone());
        round_case
            .actor
            .handle_finalized(&ctx, first_finalized)
            .await
            .expect("first round finalizes");
        let mut conflicting = finalized_event(&second, 2, 8, 7, addresses(1));
        conflicting.round = round;
        let error = round_case
            .actor
            .handle_finalized(&ctx, conflicting)
            .await
            .expect_err("same round must stay fatal");
        assert!(matches!(
            finalization_error(error),
            crate::errors::FinalizationError::SameRoundInconsistency { .. }
        ));
    });
}
