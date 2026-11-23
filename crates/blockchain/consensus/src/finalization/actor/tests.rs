use crate::block::ConsensusBlock;
use crate::digest::Digest;
use alloy_primitives::B256;
use outbe_primitives::OutbeHeader;
use reth_ethereum::{primitives::SealedBlock, Block};

/// Build a minimal `ConsensusBlock` with the given height and a salt
/// stored in `extra_data` so distinct salts produce distinct sealed
/// hashes (and therefore distinct `Digest`s).
fn make_block(number: u64, salt: u64) -> ConsensusBlock {
    make_block_with_parent(number, salt, B256::ZERO)
}

fn make_block_with_parent(number: u64, salt: u64, parent_hash: B256) -> ConsensusBlock {
    let mut block = Block::default();
    block.header.number = number;
    block.header.parent_hash = parent_hash;
    block.header.extra_data = salt.to_le_bytes().to_vec().into();
    let block = block.map_header(OutbeHeader::new);
    ConsensusBlock::from_sealed(SealedBlock::seal_slow(block))
}

fn digest_of(block: &ConsensusBlock) -> Digest {
    block.digest()
}

fn wakeup_is_ready(future: &mut std::pin::Pin<&mut impl std::future::Future<Output = ()>>) -> bool {
    let waker = std::task::Waker::noop();
    let mut context = std::task::Context::from_waker(waker);
    future.as_mut().poll(&mut context).is_ready()
}

#[tokio::test]
async fn finalization_wakeup_fans_out_without_losing_the_guard_permit() {
    let _guard = outbe_tee::authority::tee_authority_test_guard();
    let channel = outbe_tee::TeeAuthorityChannel::default();
    outbe_tee::authority::install_tee_authority_watch(channel.subscribe()).unwrap();
    let shared = std::sync::Arc::new(tokio::sync::Notify::new());

    let mut lifecycle = std::pin::pin!(shared.notified());
    assert!(!wakeup_is_ready(&mut lifecycle));
    super::publish_finalized_wakeup(Some(&shared), 8);
    assert!(
        wakeup_is_ready(&mut lifecycle),
        "parked lifecycle subscriber must observe the shared fan-out"
    );
    let mut guard = std::pin::pin!(channel.guard_notified());
    assert!(
        wakeup_is_ready(&mut guard),
        "guard permit must survive while the guard is outside notified()"
    );
    assert_eq!(channel.observed_finalized_height(), 8);

    let mut lifecycle = std::pin::pin!(shared.notified());
    let mut guard = std::pin::pin!(channel.guard_notified());
    assert!(!wakeup_is_ready(&mut lifecycle));
    assert!(!wakeup_is_ready(&mut guard));
    super::publish_finalized_wakeup(Some(&shared), 9);
    assert!(wakeup_is_ready(&mut lifecycle));
    assert!(wakeup_is_ready(&mut guard));
    assert_eq!(channel.observed_finalized_height(), 9);

    super::publish_finalized_wakeup(None, 10);
    assert_eq!(channel.observed_finalized_height(), 10);
    let mut guard = std::pin::pin!(channel.guard_notified());
    assert!(wakeup_is_ready(&mut guard));
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
        certificate_scheme_provider: scheme_provider(epoch, 4),
        committee_provider: crate::committee_provider::CommitteeProvider::new(),
        late_sig_store: store.clone(),
    };
    let (actor, _mailbox) = FinalizationActor::new(deps);

    commonware_runtime::tokio::Runner::default().start(|_ctx| async move {
        actor
            .process_finalization(finalized, block)
            .await
            .expect("process_finalization should rekey without error");
    });

    // The fb_hash-keyed buffer was resolved to the finalized block number,
    // while retaining the bound pending vote for branch-specific late credits.
    let s = store.lock().expect("store");
    assert_eq!(
        s.pending_vote_count(fb_hash),
        1,
        "pending buffer must retain bound votes for branch-specific late-credit selection"
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
            (
                "outbe_consensus_finalization_resolution_stalled_total",
                DebugValue::Counter(value),
            ) => {
                stalled = *value;
            }
            (
                "outbe_consensus_finalization_divergence_suspected_total",
                DebugValue::Counter(value),
            ) => {
                divergence = *value;
            }
            ("outbe_consensus_finalization_divergence_suspected", DebugValue::Gauge(value)) => {
                gauge = value.into_inner();
                saw_gauge = true;
            }
            _ => {}
        }
    }
    if stalled > 0 {
        metrics::counter!("outbe_consensus_finalization_resolution_stalled_total")
            .increment(stalled);
    }
    if divergence > 0 {
        metrics::counter!("outbe_consensus_finalization_divergence_suspected_total")
            .increment(divergence);
    }
    if saw_gauge {
        metrics::gauge!("outbe_consensus_finalization_divergence_suspected").set(gauge);
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
                committee_provider: crate::committee_provider::CommitteeProvider::new(),
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

#[test]
fn unresponsive_marshal_does_not_block_the_next_actor_message() {
    use commonware_consensus::types::Height;
    use commonware_runtime::{Clock as _, Spawner as _, Supervisor as _};

    deterministic::Runner::timed(Duration::from_secs(5)).start(|context| async move {
        let (marshal_actor, mailbox) = pending_marshal(&context).await;
        let view = new_finalization_view(B256::ZERO, 0, None);
        let (actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
            view,
            block_cache: crate::finalization::block_cache::BlockCache::new(),
            marshal_mailbox: Some(mailbox),
            bridge: None,
            dkg_manager: crate::dkg_manager::Mailbox::new(),
            vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
            parent_cert_store: FinalizedParentCertStore::new(),
            certificate_scheme_provider: SchemeProvider::default(),
            committee_provider: crate::committee_provider::CommitteeProvider::new(),
            late_sig_store: late_sig_store::shared(3),
        });
        let task = context
            .child("finalization")
            .spawn(move |ctx| async move { actor.run(ctx).await });
        ingress
            .notify_finalized(Finalized {
                round: Round::new(Epoch::new(0), View::new(1)),
                digest: Digest(B256::repeat_byte(0xAA)),
                vrf_seed: None,
                consensus_data: outbe_primitives::consensus::ConsensusData::default(),
            })
            .expect("first message accepted");
        context.sleep(Duration::from_millis(100)).await;
        ingress
            .reconcile_marshal_tip(crate::marshal_update_reporter::ConsensusTip {
                round: Round::new(Epoch::new(0), View::new(2)),
                height: Height::new(0),
                digest: Digest(B256::repeat_byte(0xBB)),
            })
            .expect("second message accepted");
        let result = context
            .timeout(Duration::from_secs(2), task)
            .await
            .expect("actor must process the second message before the deadline")
            .expect("actor task must join");
        let error = result.expect_err("same-height conflict must stop the actor");
        assert!(
            error
                .to_string()
                .contains("same-height finalization inconsistency"),
            "{error:?}"
        );
        drop(ingress);
        drop(marshal_actor);
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
            (
                "outbe_consensus_finalization_snapshot_degraded_total",
                DebugValue::Counter(value),
            ) => {
                metrics.snapshot_build_degraded = *value;
                if *value > 0 {
                    metrics::counter!("outbe_consensus_finalization_snapshot_degraded_total")
                        .increment(*value);
                }
            }
            ("outbe_consensus_finalization_persist_degraded_total", DebugValue::Counter(value)) => {
                metrics.persist_degraded = *value;
                if *value > 0 {
                    metrics::counter!("outbe_consensus_finalization_persist_degraded_total")
                        .increment(*value);
                }
            }
            ("outbe_consensus_finalization_persist_degraded", DebugValue::Gauge(value)) => {
                metrics.persist_gauge = value.into_inner();
                metrics::gauge!("outbe_consensus_finalization_persist_degraded")
                    .set(metrics.persist_gauge);
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
    harness_with_genesis(provider, B256::ZERO)
}

fn harness_with_genesis(provider: SchemeProvider, genesis_hash: B256) -> ActorHarness {
    let view = new_finalization_view(genesis_hash, 0, None);
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
        committee_provider: crate::committee_provider::CommitteeProvider::new(),
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

fn harness_with_signed_marshal(
    fixture: &sparse_c1_tests::EpochCommitteeFixture,
    genesis_hash: B256,
    marshal_mailbox: crate::marshal_types::MarshalMailbox,
) -> ActorHarness {
    let schemes = SchemeProvider::default();
    let committees = crate::committee_provider::CommitteeProvider::new();
    fixture.register(&schemes, &committees);
    let view = new_finalization_view(genesis_hash, 0, None);
    let store = FinalizedParentCertStore::new();
    let late = late_sig_store::shared(3);
    let cache = crate::finalization::block_cache::BlockCache::new();
    let deps = FinalizationActorDeps {
        view: view.clone(),
        block_cache: cache.clone(),
        marshal_mailbox: Some(marshal_mailbox),
        bridge: None,
        dkg_manager: crate::dkg_manager::Mailbox::new(),
        vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
        parent_cert_store: store.clone(),
        certificate_scheme_provider: schemes,
        committee_provider: committees,
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

fn signed_finalized_event(
    fixture: &sparse_c1_tests::EpochCommitteeFixture,
    block: &ConsensusBlock,
    epoch: u64,
    view: u64,
    parent_view: u64,
) -> Finalized {
    use commonware_codec::Encode as _;

    let mut finalized = finalized_event(block, epoch, view, parent_view, fixture.addresses.clone());
    let certificate = fixture.sign_finalization(view, parent_view, block.digest());
    finalized
        .consensus_data
        .finalized_certificate
        .encoded_certificate = certificate.encode().into();
    finalized.consensus_data.finalized_certificate.signer_bitmap = vec![1; fixture.addresses.len()];
    finalized
}

#[test]
fn first_live_finalization_extends_nonzero_genesis_hash() {
    deterministic::Runner::timed(Duration::from_secs(5)).start(|ctx| async move {
        let genesis_hash = B256::repeat_byte(0xE5);
        let fixture = sparse_c1_tests::EpochCommitteeFixture::new(0);
        let marshal = sparse_c1_tests::start_live_marshal(
            &ctx,
            "first-live-genesis",
            SchemeProvider::default(),
        )
        .await;
        let mut accepted =
            harness_with_signed_marshal(&fixture, genesis_hash, marshal.mailbox.clone());
        let block = make_block_with_parent(1, 1, genesis_hash);
        let digest = block.digest();
        let round = Round::new(Epoch::new(0), View::new(1));
        accepted.cache.insert_bounded(digest, block.clone());
        accepted
            .actor
            .handle_finalized(&ctx, signed_finalized_event(&fixture, &block, 0, 1, 0))
            .await
            .expect("block 1 must extend the canonical genesis hash");
        {
            let view = accepted.view.read();
            assert_eq!(view.last_finalized_number, 1);
            assert_eq!(view.forkchoice.finalized_block_hash, digest.0);
            assert_eq!(view.last_finalized_round, Some(round));
        }

        let mut rejected =
            harness_with_signed_marshal(&fixture, genesis_hash, marshal.mailbox.clone());
        let wrong_parent = make_block_with_parent(1, 2, B256::ZERO);
        let wrong_digest = wrong_parent.digest();
        rejected
            .cache
            .insert_bounded(wrong_digest, wrong_parent.clone());
        let error = rejected
            .actor
            .handle_finalized(
                &ctx,
                signed_finalized_event(&fixture, &wrong_parent, 0, 1, 0),
            )
            .await
            .expect_err("block 1 with a wrong parent must fail closed");
        assert!(matches!(
            finalization_error(error),
            crate::errors::FinalizationError::ArchiveReconciliation { height: 1, .. }
        ));
        let view = rejected.view.read();
        assert_eq!(view.last_finalized_number, 0);
        assert_eq!(view.forkchoice.finalized_block_hash, genesis_hash);
        assert!(view.last_finalized_round.is_none());
        assert_eq!(rejected.store.len(), 0);
    });
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

struct CanonicalCnWitness {
    epoch: u64,
    view: u64,
    parent_view: u64,
    block_hash: B256,
}

fn insert_canonical_cn_witness(
    store: &FinalizedParentCertStore,
    witness: CanonicalCnWitness,
    committee: &[Address],
) {
    let CanonicalCnWitness {
        epoch,
        view,
        parent_view,
        block_hash,
    } = witness;
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
                CanonicalCnWitness {
                    epoch: EPOCH,
                    view: VIEW,
                    parent_view: PARENT_VIEW,
                    block_hash: digest.0,
                },
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
            let outcome = harness
                .actor
                .process_finalization(finalized, block)
                .await
                .expect("snapshot mismatch must not halt finalization");

            assert_eq!(outcome, super::ApplyOutcome::Applied);
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

            // 2b: put exhaustion leaves the gauge latched but authenticated
            // finalization still advances. The canonical committee is rekeyed.
            let block_b = make_block(21, 21);
            let digest_b = digest_of(&block_b);
            let key_b = CertifiedParentProofKey::new(EPOCH, 21, digest_b.0);
            buffer_vote(&harness.late, EPOCH, 21, 20, digest_b.0);
            harness
                .store
                .fail_next_put_finalization(FINALIZE_PERSIST_MAX_ATTEMPTS);
            let outcome = harness
                .actor
                .process_finalization(
                    finalized_event(&block_b, EPOCH, 21, 20, committee.clone()),
                    block_b.clone(),
                )
                .await
                .expect("persist failure must not halt finalization");
            assert_eq!(outcome, super::ApplyOutcome::Applied);
            assert_eq!(harness.view.read().last_finalized_number, 21);
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

            // A duplicate F repairs the missing local proof without repeating block effects.
            finalize_step(&harness, 21, EPOCH, &committee).await;
            assert!(harness.store.get_finalization(key_b).is_some());
            assert_eq!(read_degrade_metrics(&snapshotter).persist_gauge, 0.0);

            // Clean cycle at 22 stays green.
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
        let height = harness(scheme_provider(1, 1));
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

        let fixture = sparse_c1_tests::EpochCommitteeFixture::new(2);
        let marshal = sparse_c1_tests::start_live_marshal(
            &ctx,
            "same-round-conflict",
            SchemeProvider::default(),
        )
        .await;
        let mut round_case =
            harness_with_signed_marshal(&fixture, B256::ZERO, marshal.mailbox.clone());
        let first = make_block(40, 1);
        let second = make_block(41, 2);
        round_case.view.write().last_finalized_number = 39;
        let first_finalized = signed_finalized_event(&fixture, &first, 2, 8, 7);
        let round = first_finalized.round;
        round_case
            .cache
            .insert_bounded(digest_of(&first), first.clone());
        round_case
            .actor
            .handle_finalized(&ctx, first_finalized)
            .await
            .expect("first round finalizes");
        let mut conflicting = signed_finalized_event(&fixture, &second, 2, 8, 7);
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

mod sparse_c1_tests {
    use super::*;
    use crate::committee_provider::CommitteeProvider;
    use crate::finalization::evidence::verify_child_parent_witness;
    use crate::hybrid::HybridScheme;
    use crate::marshal_types::MarshalMailbox;
    use crate::marshal_update_reporter::ConsensusTip;
    use crate::test_fixtures::{block_with_system_inputs, SystemBlockFixture};
    use crate::test_harness::{
        committee_snapshot, fixture_signer_schemes, test_fully_signed_metadata,
        FixtureSignerSharing, NoopMarshalResolver, TestFinalizedParent,
    };
    use commonware_codec::Encode as _;
    use commonware_consensus::{
        simplex::types::{Activity, Finalization, Finalize, Notarization, Notarize, Proposal},
        types::Height,
        Reporter as _,
    };
    use commonware_cryptography::certificate::Scheme as _;
    use commonware_parallel::Sequential;
    use commonware_runtime::{Clock as _, Spawner as _, Supervisor as _};
    use commonware_utils::{
        acknowledgement::Acknowledgement as _, iter::NonEmpty, ordered::Set as OrderedSet,
        TryCollect as _,
    };
    use outbe_primitives::{
        consensus_metadata::ParentParticipationProof,
        reshare_artifact::{encode_consensus_header_artifact, ConsensusHeaderArtifact},
        signer::OutbeEvmSigner,
        system_tx::{SystemTx, CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX},
    };
    use std::num::{NonZeroU16, NonZeroU64, NonZeroUsize};
    use std::sync::Arc;

    #[derive(Clone, Default)]
    struct AckingMarshalReporter;

    impl commonware_consensus::Reporter for AckingMarshalReporter {
        type Activity = crate::marshal_types::MarshalUpdate;

        fn report(&mut self, activity: Self::Activity) -> commonware_actor::Feedback {
            if let commonware_consensus::marshal::Update::Block(_, ack) = activity {
                ack.acknowledge();
            }
            commonware_actor::Feedback::Ok
        }
    }

    #[derive(Clone, Default)]
    struct NoopBuffer;

    impl commonware_consensus::marshal::core::Buffer<crate::marshal_types::Variant> for NoopBuffer {
        type PublicKey = bls12381::PublicKey;

        async fn find_by_digest(&self, _digest: Digest) -> Option<Arc<ConsensusBlock>> {
            None
        }

        async fn find_by_commitment(&self, _commitment: Digest) -> Option<Arc<ConsensusBlock>> {
            None
        }

        fn subscribe_by_digest(
            &self,
            _digest: Digest,
        ) -> Option<commonware_utils::channel::oneshot::Receiver<Arc<ConsensusBlock>>> {
            None
        }

        fn subscribe_by_commitment(
            &self,
            _commitment: Digest,
        ) -> Option<commonware_utils::channel::oneshot::Receiver<Arc<ConsensusBlock>>> {
            None
        }

        fn retire(&self, _update: commonware_consensus::marshal::core::Retirement<Digest>) {}

        fn send(
            &self,
            _round: Round,
            _block: Arc<ConsensusBlock>,
            _recipients: commonware_p2p::Recipients<Self::PublicKey>,
        ) {
        }
    }

    pub(super) struct EpochCommitteeFixture {
        epoch: u64,
        pub(super) addresses: Vec<Address>,
        signers: Vec<HybridScheme<MinSig>>,
        verifier: HybridScheme<MinSig>,
        snapshot: crate::proof::CommitteeSnapshot,
    }

    impl EpochCommitteeFixture {
        pub(super) fn new(epoch: u64) -> Self {
            let n = 4usize;
            let keys: Vec<bls12381::PrivateKey> = (0..n)
                .map(|i| bls12381::PrivateKey::from_seed((epoch * 16 + i as u64) + 1))
                .collect();
            let participants: OrderedSet<bls12381::PublicKey> = keys
                .iter()
                .map(|sk| bls12381::PublicKey::from(sk.clone()))
                .try_collect()
                .expect("ordered participants");
            let dkg = crate::bls::bootstrap_dkg(n as u32).expect("bootstrap dkg");
            let namespace = crate::config::outbe_app_namespace();
            let signers = fixture_signer_schemes(
                &namespace,
                &keys,
                &participants,
                FixtureSignerSharing {
                    polynomial: &dkg.polynomial,
                    shares: &dkg.shares,
                },
            );
            let verifier =
                HybridScheme::<MinSig>::verifier(&namespace, participants.clone(), dkg.polynomial)
                    .expect("verifier scheme");
            let ordered_pubkeys: Vec<bls12381::PublicKey> = participants.iter().cloned().collect();
            let group_pk = verifier.identity().expect("verifier identity");
            let snapshot = committee_snapshot(
                &ordered_pubkeys,
                &group_pk,
                verifier.expected_vrf_material_version(),
            );
            let addresses = snapshot.committee.iter().map(|e| e.address).collect();
            Self {
                epoch,
                addresses,
                signers,
                verifier,
                snapshot,
            }
        }

        pub(super) fn register(&self, schemes: &SchemeProvider, committees: &CommitteeProvider) {
            assert!(schemes.register(Epoch::new(self.epoch), self.verifier.clone()));
            committees.register(Epoch::new(self.epoch), self.addresses.clone());
        }

        pub(super) fn sign_finalization(
            &self,
            view: u64,
            parent_view: u64,
            digest: Digest,
        ) -> Finalization<HybridScheme<MinSig>, Digest> {
            let proposal = Proposal::new(
                Round::new(Epoch::new(self.epoch), View::new(view)),
                View::new(parent_view),
                digest,
            );
            let votes: Vec<_> = self
                .signers
                .iter()
                .map(|signer| Finalize::sign(signer, proposal.clone()).expect("sign finalize vote"))
                .collect();
            let attestations = NonEmpty::try_new(votes.into_iter().map(|v| v.attestation))
                .expect("non-empty attestations");
            let certificate = self
                .verifier
                .assemble(attestations, &Sequential)
                .expect("assemble finalization certificate");
            Finalization {
                proposal,
                certificate,
            }
        }

        fn sign_notarization(
            &self,
            view: u64,
            parent_view: u64,
            digest: Digest,
        ) -> Notarization<HybridScheme<MinSig>, Digest> {
            let proposal = Proposal::new(
                Round::new(Epoch::new(self.epoch), View::new(view)),
                View::new(parent_view),
                digest,
            );
            let votes: Vec<_> = self
                .signers
                .iter()
                .map(|signer| Notarize::sign(signer, proposal.clone()).expect("sign notarize vote"))
                .collect();
            let attestations = NonEmpty::try_new(votes.into_iter().map(|v| v.attestation))
                .expect("non-empty attestations");
            let certificate = self
                .verifier
                .assemble(attestations, &Sequential)
                .expect("assemble notarization certificate");
            Notarization {
                proposal,
                certificate,
            }
        }

        fn block_with_parent_witness(
            &self,
            parent: &ConsensusBlock,
            parent_view: u64,
            parent_parent_view: u64,
            proof_kind: ParentParticipationProof,
        ) -> ConsensusBlock {
            let proof_bytes: Vec<u8> = match proof_kind {
                ParentParticipationProof::Finalization => self
                    .sign_finalization(parent_view, parent_parent_view, parent.digest())
                    .encode()
                    .to_vec(),
                ParentParticipationProof::CertifiedNotarization => self
                    .sign_notarization(parent_view, parent_parent_view, parent.digest())
                    .encode()
                    .to_vec(),
            };
            let metadata = test_fully_signed_metadata(
                &TestFinalizedParent {
                    block_number: parent.number(),
                    block_hash: parent.digest().0,
                    epoch: self.epoch,
                    view: parent_view,
                    parent_view: parent_parent_view,
                    vrf_material_version: self.verifier.expected_vrf_material_version(),
                    proof_kind,
                },
                &self.snapshot,
                &proof_bytes,
            );
            let signer = OutbeEvmSigner::from_secret_bytes([9; 32]).unwrap();
            let block = block_with_system_inputs(
                &signer,
                SystemBlockFixture {
                    block_number: parent.number() + 1,
                    parent_hash: parent.digest().0,
                    extra_data: Bytes::new(),
                    inputs: vec![SystemTx::Consensus {
                        step_index: CONSENSUS_PARENT_ACCOUNTING_STEP_INDEX,
                        payload: metadata.encode().expect("encode phase1 metadata"),
                    }],
                    chain_id: outbe_primitives::chain::CHAIN_ID,
                },
            );
            // Marshal validates `transactions_root` on journal decode; seal with the real root.
            let mut raw = block.into_inner().into_block();
            raw.header.inner.transactions_root =
                alloy_consensus::proofs::calculate_transaction_root(&raw.body.transactions);
            ConsensusBlock::from_sealed(SealedBlock::seal_slow(raw))
        }
    }

    pub(super) struct LiveMarshalFixture {
        pub(super) mailbox: MarshalMailbox,
        _resolver_handler: commonware_consensus::marshal::resolver::handler::Handler<Digest>,
        _handle: commonware_runtime::Handle<()>,
    }

    pub(super) async fn start_live_marshal(
        context: &deterministic::Context,
        prefix: &str,
        schemes: SchemeProvider,
    ) -> LiveMarshalFixture {
        let cache = commonware_runtime::buffer::paged::CacheRef::from_pooler(
            context,
            NonZeroU16::new(1024).unwrap(),
            NonZeroUsize::new(10).unwrap(),
        );
        let items = NonZeroU64::new(10).unwrap();
        let buffer = NonZeroUsize::new(1024).unwrap();
        let (finalizations, blocks) = crate::test_fixtures::marshal::MarshalArchiveFixture {
            partition_prefix: prefix,
            page_cache: &cache,
            items_per_section: items,
            replay_buffer: buffer,
            write_buffer: buffer,
        }
        .open(context)
        .await
        .expect("marshal archives open");
        let genesis = Block::default().map_header(OutbeHeader::new);
        let (marshal_actor, mailbox, _) = commonware_consensus::marshal::core::Actor::init(
            context.child("marshal"),
            finalizations,
            blocks,
            commonware_consensus::marshal::Config {
                provider: schemes,
                epocher: crate::follow::FollowerEpocher::new(10_000, 0),
                start: commonware_consensus::marshal::Start::Genesis(ConsensusBlock::from_sealed(
                    SealedBlock::seal_slow(genesis),
                )),
                partition_prefix: prefix.into(),
                mailbox_size: NonZeroUsize::new(64).unwrap(),
                view_retention: commonware_consensus::types::ViewDelta::new(10_000),
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
        let (resolver_rx, resolver_handler) =
            commonware_consensus::marshal::resolver::handler::init::<Digest>(
                context.child("resolver_handler"),
                NonZeroUsize::new(16).unwrap(),
            );
        let handle = marshal_actor.start(
            AckingMarshalReporter,
            NoopBuffer,
            (resolver_rx, NoopMarshalResolver),
        );
        LiveMarshalFixture {
            mailbox,
            _resolver_handler: resolver_handler,
            _handle: handle,
        }
    }

    async fn wait_for_marshal_finalization(
        context: &deterministic::Context,
        marshal: &MarshalMailbox,
        height: Height,
    ) {
        for _ in 0..100 {
            if marshal.get_finalization(height).await.is_some() {
                return;
            }
            context.sleep(Duration::from_millis(5)).await;
        }
        panic!(
            "timed out waiting for marshal finalization archive at height {}",
            height.get()
        );
    }

    async fn wait_for_height(
        context: &deterministic::Context,
        view: &crate::finalization::state::FinalizationViewHandle,
        target: u64,
        max_steps: usize,
        step: Duration,
    ) {
        for _ in 0..max_steps {
            if view.read().last_finalized_number >= target {
                return;
            }
            context.sleep(step).await;
        }
        panic!(
            "timed out waiting for finalized height {target}; current={}",
            view.read().last_finalized_number
        );
    }

    /// Permanent sparse h181 (CN witness in h182) + h182 (F witness in h183) + direct F(h183).
    /// Verifies:
    /// - segment applies 181, 182, 183 in chain order;
    /// - implicit h181 stays ProofKind::CertifiedNotarization with exact own round (v11),
    ///   no fake F record, no late-vote rekey, no VRF degradation;
    /// - shared `verify_child_parent_witness` seam returns `finalization: None` for CN
    ///   and `Some` for F.
    #[test]
    fn sparse_descendant_segment_applies_implicit_cn_and_f_in_order() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "sparse-h181", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181,
                11,
                10,
                ParentParticipationProof::CertifiedNotarization,
            );
            let b183 = epoch0.block_with_parent_witness(
                &b182,
                14,
                11,
                ParentParticipationProof::Finalization,
            );

            // Check shared C1/C2 seam directly for CN vs F.
            let cn_witness = verify_child_parent_witness(&b181, &b182, &schemes, &committees)
                .expect("valid CN witness")
                .expect("epoch0 registered");
            assert_eq!(cn_witness.round, Round::new(Epoch::new(0), View::new(11)));
            assert_eq!(cn_witness.parent_view, 10);
            assert_eq!(
                cn_witness.proof_kind,
                ParentParticipationProof::CertifiedNotarization
            );
            assert!(
                cn_witness.finalization.is_none(),
                "CN witness must never fabricate a Finalization"
            );

            let f_witness = verify_child_parent_witness(&b182, &b183, &schemes, &committees)
                .expect("valid F witness")
                .expect("epoch0 registered");
            assert_eq!(f_witness.round, Round::new(Epoch::new(0), View::new(14)));
            assert_eq!(f_witness.proof_kind, ParentParticipationProof::Finalization);
            assert!(f_witness.finalization.is_some());

            // Put all blocks in Marshal, but ONLY direct F for h183 (h181 & h182 lack direct F).
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(14)), b182.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(17)), b183.clone())
                    .await
            );
            let f183 = epoch0.sign_finalization(17, 14, b183.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f183));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(183)).await;
            assert!(marshal
                .mailbox
                .get_finalization(Height::new(181))
                .await
                .is_none());
            assert!(marshal
                .mailbox
                .get_finalization(Height::new(182))
                .await
                .is_none());
            assert!(marshal
                .mailbox
                .get_finalization(Height::new(183))
                .await
                .is_some());

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let store = FinalizedParentCertStore::new();
            let late = late_sig_store::shared(3);
            let vrf_safety = VrfSafetyGate::new(0, 0, 1_000, 100);
            buffer_vote(&late, 0, 11, 10, b181.digest().0);

            let (adv_tx, mut adv_rx) = tokio::sync::mpsc::unbounded_channel();
            let (actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: vrf_safety.clone(),
                parent_cert_store: store.clone(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late.clone(),
            });
            let actor = actor.with_view_advanced(adv_tx);
            let actor_task = context
                .child("actor")
                .spawn(move |ctx| async move { actor.run(ctx).await });

            ingress
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(17)),
                    height: Height::new(183),
                    digest: b183.digest(),
                })
                .expect("send tip");

            wait_for_height(&context, &view, 183, 50, Duration::from_millis(10)).await;

            // Applied strictly in ascending chain order: 181, 182, 183.
            let mut applied = Vec::new();
            while let Ok(h) = adv_rx.try_recv() {
                applied.push(h);
            }
            assert_eq!(applied, vec![181, 182, 183]);

            // Final view points to h183 at round (0, 17).
            assert_eq!(view.read().last_finalized_number, 183);
            assert_eq!(view.read().forkchoice.finalized_block_hash, b183.digest().0);
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(0), View::new(17)))
            );

            // h181 CN stayed CN: no Finalization record, exact CN record at (0, 11), no VRF degradation.
            let key_181 = CertifiedParentProofKey::new(0, 11, b181.digest().0);
            assert!(
                store.get_finalization(key_181).is_none(),
                "implicit CN height must never fabricate a Finalization record"
            );
            let cn_181 = store
                .get_certified_notarization(key_181)
                .expect("implicit CN witness must be stored in CN slot");
            assert_eq!(cn_181.kind, ProofKind::CertifiedNotarization);
            assert_eq!(cn_181.parent_view, 10);
            assert_eq!(
                vrf_safety.snapshot().randomness_status,
                outbe_primitives::consensus::RandomnessStatus::Healthy,
                "absence of own F at implicit h181 must not degrade VRF"
            );

            // h182 (genuine F from child Phase 1) and h183 (direct F) both have Finalization records.
            assert!(store
                .get_finalization(CertifiedParentProofKey::new(0, 14, b182.digest().0))
                .is_some());
            assert!(store
                .get_finalization(CertifiedParentProofKey::new(0, 17, b183.digest().0))
                .is_some());
            // h181 was CN, so late_sig_store must NOT have resolved h181 (only h182 and h183).
            assert_eq!(late.lock().expect("late").resolved_len(), 2);

            drop(ingress);
            actor_task.await.expect("join").expect("clean shutdown");
        });
    }

    #[test]
    fn failed_cn_handoff_still_advances_intermediate_and_next_authenticated_f() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|_context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181,
                11,
                10,
                ParentParticipationProof::CertifiedNotarization,
            );
            let b183 = epoch0.block_with_parent_witness(
                &b182,
                14,
                11,
                ParentParticipationProof::Finalization,
            );
            let cn = verify_child_parent_witness(&b181, &b182, &schemes, &committees)
                .expect("valid CN witness")
                .expect("epoch0 registered");
            let f = verify_child_parent_witness(&b182, &b183, &schemes, &committees)
                .expect("valid F witness")
                .expect("epoch0 registered");

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let store = FinalizedParentCertStore::new();
            store.fail_next_put_certified_notarization(FINALIZE_PERSIST_MAX_ATTEMPTS);
            let (adv_tx, mut adv_rx) = tokio::sync::mpsc::unbounded_channel();
            let (actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: None,
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store.clone(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            let actor = actor.with_view_advanced(adv_tx);

            assert_eq!(
                actor.apply_implicit_cn(cn, b181.clone()).await.unwrap(),
                super::super::ApplyOutcome::Applied
            );
            assert_eq!(view.read().last_finalized_number, 181);
            assert!(store
                .get_certified_notarization(CertifiedParentProofKey::new(0, 11, b181.digest().0))
                .is_none());
            assert_eq!(
                actor
                    .process_finalization(
                        super::super::VerifiedStep::from_child_witness(182, b182.digest(), f)
                            .to_finalized(),
                        b182.clone(),
                    )
                    .await
                    .unwrap(),
                super::super::ApplyOutcome::Applied
            );
            assert_eq!(view.read().last_finalized_number, 182);
            assert!(store
                .get_finalization(CertifiedParentProofKey::new(0, 14, b182.digest().0))
                .is_some());
            assert_eq!(adv_rx.try_recv().ok(), Some(181));
            assert_eq!(adv_rx.try_recv().ok(), Some(182));
            assert!(adv_rx.try_recv().is_err());
        });
    }

    /// A missing parent is retried and the full certified chain is applied once repaired.
    #[test]
    fn tip_before_block_repair_wakes_via_digest_subscription_without_tick() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "sub-wakeup", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181,
                11,
                10,
                ParentParticipationProof::CertifiedNotarization,
            );

            // Only b182 + F(182) exist initially; b181 is missing.
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(14)), b182.clone())
                    .await
            );
            let f182 = epoch0.sign_finalization(14, 11, b182.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f182.clone()));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            actor = actor.with_pending_retry_interval(Duration::from_millis(50));
            actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(14)),
                    height: Height::new(182),
                    digest: b182.digest(),
                })
                .await
                .expect("park tip before block repair");
            assert_eq!(
                actor.pending_target.as_ref().map(|t| t.tip.digest),
                Some(b182.digest())
            );
            assert_eq!(
                view.read().last_finalized_number,
                180,
                "partial segment must not publish before h181 arrives"
            );
            let actor_task = context
                .child("actor")
                .spawn(move |ctx| async move { actor.run(ctx).await });

            // Now deliver b181 into Marshal; the bounded retry completes the chain.
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            // A subsequent Marshal finalization runs its archived-gap repair.
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            wait_for_height(&context, &view, 182, 50, Duration::from_millis(10)).await;
            assert_eq!(view.read().forkchoice.finalized_block_hash, b182.digest().0);

            drop(ingress);
            actor_task.await.expect("join").expect("clean shutdown");
        });
    }

    /// The bounded retry drives reconciliation to completion after an absent block arrives.
    #[test]
    fn lost_subscription_recovers_via_bounded_tick() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "tick-fallback", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let f181 = epoch0.sign_finalization(11, 10, b181.digest());

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            actor = actor.with_pending_retry_interval(Duration::from_millis(50));

            // Park tip before F(181) or b181 is in Marshal.
            actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(11)),
                    height: Height::new(181),
                    digest: b181.digest(),
                })
                .await
                .expect("park tip");
            assert_eq!(
                actor.pending_target.as_ref().map(|t| t.tip.digest),
                Some(b181.digest())
            );

            let actor_task = context
                .child("actor")
                .spawn(move |ctx| async move { actor.run(ctx).await });

            // Make b181 + F(181) durable after the actor starts.
            context.sleep(Duration::from_millis(20)).await;
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            let _ = marshal.mailbox.report(Activity::Finalization(f181));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(181)).await;

            wait_for_height(&context, &view, 181, 50, Duration::from_millis(20)).await;
            assert_eq!(view.read().forkchoice.finalized_block_hash, b181.digest().0);

            drop(ingress);
            actor_task.await.expect("join").expect("clean shutdown");
        });
    }

    #[test]
    fn pending_target_before_run_is_processed_without_a_new_mailbox_message() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);
            let mut marshal =
                start_live_marshal(&context, "pre-run-pending", schemes.clone()).await;
            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            let f181 = epoch0.sign_finalization(11, 10, b181.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f181));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(181)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            actor
                .record_target(
                    ConsensusTip {
                        round: Round::new(Epoch::new(0), View::new(11)),
                        height: Height::new(181),
                        digest: b181.digest(),
                    },
                    None,
                )
                .expect("target accepted before run");
            let actor_task = context
                .child("actor")
                .spawn(move |ctx| async move { actor.run(ctx).await });
            wait_for_height(&context, &view, 181, 25, Duration::from_millis(10)).await;
            assert_eq!(view.read().forkchoice.finalized_block_hash, b181.digest().0);
            drop(ingress);
            actor_task.await.expect("join").expect("clean shutdown");
        });
    }

    /// Changing the highest pending target replaces the single digest subscription
    /// so at most one subscription is outstanding at any time.
    #[test]
    fn target_change_replaces_single_digest_subscription() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "replace-sub", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181,
                11,
                10,
                ParentParticipationProof::CertifiedNotarization,
            );
            let b183 = epoch0.block_with_parent_witness(
                &b182,
                14,
                11,
                ParentParticipationProof::Finalization,
            );

            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(14)), b182.clone())
                    .await
            );
            let f182 = epoch0.sign_finalization(14, 11, b182.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            let f183 = epoch0.sign_finalization(17, 14, b183.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f183));
            // Only b182 is present in Marshal, so only height 182 can be indexed in
            // Marshal's finalizations archive; b183 is intentionally absent.
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view,
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });

            // Tip 182 has b182 + F(182) but lacks b181 -> subscribes to b181.
            actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(14)),
                    height: Height::new(182),
                    digest: b182.digest(),
                })
                .await
                .expect("park tip 182");
            assert_eq!(
                actor.pending_target.as_ref().map(|t| t.tip.digest),
                Some(b182.digest())
            );

            // Higher tip 183 lacks b183 -> replaces the single subscription with b183.
            actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(17)),
                    height: Height::new(183),
                    digest: b183.digest(),
                })
                .await
                .expect("park tip 183");
            assert_eq!(
                actor.pending_target.as_ref().map(|t| t.tip.digest),
                Some(b183.digest())
            );
        });
    }

    /// Live `Finalized` at h182 arriving while h181 is absent must be retained
    /// (never silently dropped) and applied once h181 arrives.
    #[test]
    fn live_finalization_retained_when_prefix_absent_and_applied_after_repair() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "live-retain", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181,
                11,
                10,
                ParentParticipationProof::CertifiedNotarization,
            );
            let f182 = epoch0.sign_finalization(14, 11, b182.digest());

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let cache = crate::finalization::block_cache::BlockCache::new();
            cache.insert_bounded(b182.digest(), b182.clone());

            let (mut actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: cache.clone(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            actor = actor.with_pending_retry_interval(Duration::from_millis(50));

            // Deliver live F(182) while b181 is missing from Marshal.
            let mut live_event = finalized_event(&b182, 0, 14, 11, epoch0.addresses.clone());
            live_event
                .consensus_data
                .finalized_certificate
                .encoded_certificate = f182.encode().into();
            live_event
                .consensus_data
                .finalized_certificate
                .signer_bitmap = vec![1; epoch0.addresses.len()];
            actor
                .handle_finalized(&context, live_event)
                .await
                .expect("park live F while prefix absent");
            assert_eq!(
                actor
                    .unresolved
                    .get(&Round::new(Epoch::new(0), View::new(14))),
                Some(&b182.digest()),
                "live F identity must survive until the finalized archive contains its block"
            );
            assert_eq!(view.read().last_finalized_number, 180);

            let actor_task = context
                .child("actor")
                .spawn(move |ctx| async move { actor.run(ctx).await });

            // Marshal archives F(182) first, then repairs its missing parent on
            // a later finalization delivery.
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(14)), b182.clone())
                    .await
            );
            let _ = marshal.mailbox.report(Activity::Finalization(f182.clone()));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            wait_for_height(&context, &view, 182, 50, Duration::from_millis(10)).await;
            assert_eq!(view.read().forkchoice.finalized_block_hash, b182.digest().0);

            drop(ingress);
            actor_task.await.expect("join").expect("clean shutdown");
        });
    }

    /// Direct F of a future epoch before its verifier is registered parks the
    /// suffix cleanly and resumes via the bounded tick once registered.
    #[test]
    fn future_epoch_verifier_parks_suffix_and_resumes_on_registration() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let epoch1 = EpochCommitteeFixture::new(1);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "future-epoch", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(1), View::new(2)), b181.clone())
                    .await
            );
            let f181 = epoch1.sign_finalization(2, 1, b181.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f181));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(181)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes.clone(),
                committee_provider: committees.clone(),
                late_sig_store: late_sig_store::shared(3),
            });
            let actor = actor.with_pending_retry_interval(Duration::from_millis(50));
            let actor_task = context
                .child("actor")
                .spawn(move |ctx| async move { actor.run(ctx).await });

            ingress
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(1), View::new(2)),
                    height: Height::new(181),
                    digest: b181.digest(),
                })
                .expect("send future-epoch tip");
            context.sleep(Duration::from_millis(120)).await;
            assert_eq!(
                view.read().last_finalized_number,
                180,
                "must stay parked while epoch 1 verifier is unregistered"
            );

            // Register epoch 1 verifier & committee; bounded tick picks it up and advances.
            epoch1.register(&schemes, &committees);
            wait_for_height(&context, &view, 181, 50, Duration::from_millis(20)).await;
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(1), View::new(2)))
            );

            drop(ingress);
            actor_task.await.expect("join").expect("clean shutdown");
        });
    }

    /// Invalid child Phase 1 proof, wrong parent hash, and conflicting same-height
    /// pending targets all fail closed before publishing any effects.
    #[test]
    fn bad_evidence_fails_closed_before_publishing_effects() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            // Case 1: broken parent hash link in sparse segment fails closed.
            let mut marshal = start_live_marshal(&context, "bad-evidence", schemes.clone()).await;
            let b180 = make_block(180, 180);
            let b181_wrong = make_block_with_parent(181, 181, B256::repeat_byte(0xEE));
            let b182 = epoch0.block_with_parent_witness(
                &b181_wrong,
                11,
                10,
                ParentParticipationProof::CertifiedNotarization,
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181_wrong.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(14)), b182.clone())
                    .await
            );
            let f182 = epoch0.sign_finalization(14, 11, b182.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let store = FinalizedParentCertStore::new();
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store.clone(),
                certificate_scheme_provider: schemes.clone(),
                committee_provider: committees.clone(),
                late_sig_store: late_sig_store::shared(3),
            });
            let err = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(14)),
                    height: Height::new(182),
                    digest: b182.digest(),
                })
                .await
                .expect_err("broken parent link must fail closed");
            assert!(matches!(
                finalization_error(err),
                crate::errors::FinalizationError::ArchiveReconciliation { height: 181, .. }
            ));
            assert_eq!(view.read().last_finalized_number, 180);
            assert_eq!(store.len(), 0);

            // Case 2: forged child Phase 1 proof fails closed in verify_child_parent_witness.
            let b181_good = make_block_with_parent(181, 182, b180.digest().0);
            let forged_child = make_block_with_parent(182, 183, b181_good.digest().0);
            assert!(
                verify_child_parent_witness(&b181_good, &forged_child, &schemes, &committees)
                    .is_err(),
                "child without valid Phase 1 witness must fail closed"
            );

            // Case 3: conflicting same-height pending target fails closed.
            let conflict_err = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(15)),
                    height: Height::new(182),
                    digest: Digest(B256::repeat_byte(0x99)),
                })
                .await
                .expect_err("conflicting same-height pending target must be fatal");
            assert!(matches!(
                finalization_error(conflict_err),
                crate::errors::FinalizationError::SameHeightInconsistency {
                    block_number: 182,
                    ..
                }
            ));
        });
    }

    /// Same-hash `F(v2)` after `F(v1)`:
    /// - if `put_finalization(v2)` fails, `view.last_finalized_round` stays `v1`;
    /// - neither failed nor succeeded `F(v2)` repeats block effects (`view_advanced`,
    ///   `BoundaryOutcome` observation, or DKG artifact recording).
    #[test]
    fn same_hash_new_round_failed_write_retains_v1_and_never_repeats_block_effects() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|_ctx| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut raw = Block::default();
            raw.header.number = 105;
            raw.header.parent_hash = B256::ZERO;
            raw.header.extra_data =
                encode_consensus_header_artifact(&ConsensusHeaderArtifact::BoundaryOutcome(
                    outbe_primitives::consensus::DkgBoundaryArtifact {
                        epoch: 1,
                        dkg_cycle: 1,
                        freeze_height: 95,
                        planned_activation_height: 105,
                        target_set_hash: B256::ZERO,
                        vrf_material_version: 1,
                        vrf_group_public_key: B256::ZERO,
                        vrf_group_public_key_bytes: Bytes::new(),
                        committee_set_hash: B256::ZERO,
                        is_validator_set_change: false,
                        outcome: Bytes::new(),
                        is_full_dkg: false,
                        reshare: outbe_primitives::consensus::ReshareResult {
                            new_active_set: Vec::new(),
                            active_set_hash: B256::ZERO,
                        },
                        tee_expired_target_exclusions: Vec::new(),
                        tee_expired_target_exclusions_hash: B256::ZERO,
                    },
                ))
                .expect("encode boundary artifact");
            let block = ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                raw.map_header(OutbeHeader::new),
            ));

            let view = new_finalization_view(B256::ZERO, 104, None);
            let store = FinalizedParentCertStore::new();
            let epocher = crate::follow::FollowerEpocher::new(100, 10);
            let (adv_tx, mut adv_rx) = tokio::sync::mpsc::unbounded_channel();
            let (actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: None,
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store.clone(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            let actor = actor.with_epocher(epocher).with_view_advanced(adv_tx);

            // First finalization at v1 succeeds and runs block effects once.
            let f_v1 = finalized_event(&block, 0, 20, 19, epoch0.addresses.clone());
            actor
                .process_finalization(f_v1, block.clone())
                .await
                .expect("v1 finalizes");
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(0), View::new(20)))
            );
            assert_eq!(adv_rx.try_recv().ok(), Some(105));
            assert!(adv_rx.try_recv().is_err());

            // Same-hash F(v2) with failing store write: view round MUST remain v1,
            // and block effects MUST NOT repeat.
            store.fail_next_put_finalization(FINALIZE_PERSIST_MAX_ATTEMPTS);
            let f_v2 = finalized_event(&block, 0, 21, 19, epoch0.addresses.clone());
            actor
                .process_finalization(f_v2, block.clone())
                .await
                .expect("degraded v2 write does not halt");
            assert!(store
                .get_finalization(CertifiedParentProofKey::new(0, 21, block.digest().0))
                .is_none());
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(0), View::new(20))),
                "failed v2 write must retain v1 proof round hint"
            );
            assert!(
                adv_rx.try_recv().is_err(),
                "failed same-hash F(v2) must not repeat block effects"
            );

            // Same-hash F(v2) succeeding: persists v2 and advances view round to v2,
            // STILL without repeating block effects.
            let f_v2_retry = finalized_event(&block, 0, 21, 19, epoch0.addresses.clone());
            actor
                .process_finalization(f_v2_retry, block.clone())
                .await
                .expect("v2 retry succeeds");
            assert!(store
                .get_finalization(CertifiedParentProofKey::new(0, 21, block.digest().0))
                .is_some());
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(0), View::new(21)))
            );
            assert!(
                adv_rx.try_recv().is_err(),
                "succeeded same-hash F(v2) must not repeat block effects"
            );
        });
    }

    #[test]
    fn same_hash_live_round_retries_failed_proof_without_repeating_block_effects() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let block = make_block(1, 1);
            let mut marshal =
                start_live_marshal(&context, "same-hash-live-retry", schemes.clone()).await;
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(1)), block.clone())
                    .await
            );
            let f_v1 = epoch0.sign_finalization(1, 0, block.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f_v1));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(1)).await;
            let view = new_finalization_view(B256::ZERO, 0, None);
            let cache = crate::finalization::block_cache::BlockCache::new();
            cache.insert_bounded(block.digest(), block.clone());
            let store = FinalizedParentCertStore::new();
            let (adv_tx, mut adv_rx) = tokio::sync::mpsc::unbounded_channel();
            let (actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: cache.clone(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store.clone(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            let mut actor = actor.with_view_advanced(adv_tx);

            actor
                .process_finalization(
                    finalized_event(&block, 0, 1, 0, epoch0.addresses.clone()),
                    block.clone(),
                )
                .await
                .expect("v1 finalizes");
            assert_eq!(adv_rx.try_recv().ok(), Some(1));
            cache.insert_bounded(block.digest(), block.clone());

            store.fail_next_put_finalization(2 * FINALIZE_PERSIST_MAX_ATTEMPTS);
            actor
                .handle_finalized(&context, signed_finalized_event(&epoch0, &block, 0, 2, 0))
                .await
                .expect("v2 persist failure retains the head proof without stopping actor");
            assert!(actor.head_proof_retry.is_some());
            assert_eq!(
                actor.parked_reason,
                Some(super::super::ParkedReason::Persistence)
            );
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(0), View::new(1)))
            );
            assert!(adv_rx.try_recv().is_err());

            actor.pending_target = Some(super::super::PendingTarget {
                tip: ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(3)),
                    height: Height::new(2),
                    digest: Digest(B256::repeat_byte(0x73)),
                },
                live: None,
            });
            actor.parked_reason = None; // `step_reconcile` resets the per-turn reason.
            let mut budget = crate::finalization::evidence::EventWorkBudget::new();
            let outcome = actor
                .retry_head_proof(Some(&context), &mut budget)
                .await
                .expect("failed head proof retry leaves higher target eligible");
            assert_eq!(outcome, None, "head repair must not park higher work");
            assert!(actor.head_proof_retry.is_some());
            assert_eq!(actor.parked_reason, None);
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(0), View::new(1)))
            );
            assert!(adv_rx.try_recv().is_err());
            actor.pending_target = None;

            // An unresolved future F is also independent work. Its recovery
            // must not be suppressed by the local head-proof retry.
            store.fail_next_put_finalization(FINALIZE_PERSIST_MAX_ATTEMPTS);
            let unresolved_round = Round::new(Epoch::new(0), View::new(3));
            actor
                .unresolved
                .insert(unresolved_round, Digest(B256::repeat_byte(0x74)));
            let mut budget = crate::finalization::evidence::EventWorkBudget::new();
            let outcome = actor
                .retry_head_proof(Some(&context), &mut budget)
                .await
                .expect("unresolved higher F stays eligible while head proof write fails");
            assert_eq!(outcome, None);
            assert!(actor.head_proof_retry.is_some());
            assert_eq!(actor.parked_reason, None);
            actor.unresolved.remove(&unresolved_round);

            let outcome = actor
                .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                .await
                .expect("v2 proof retry succeeds after the local store recovers");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::Idle);
            assert!(store
                .get_finalization(CertifiedParentProofKey::new(0, 2, block.digest().0))
                .is_some());
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(0), View::new(2)))
            );
            assert!(
                adv_rx.try_recv().is_err(),
                "v2 must not repeat block effects"
            );
            assert!(actor.resolution_stall.is_none());
        });
    }

    #[test]
    fn timed_out_head_proof_repair_does_not_park_a_higher_target() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|context| async move {
            let (marshal_actor, mailbox) = pending_marshal(&context).await;
            let head_hash = B256::repeat_byte(0x72);
            let view =
                new_finalization_view(head_hash, 1, Some(Round::new(Epoch::new(0), View::new(1))));
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(mailbox),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: SchemeProvider::default(),
                committee_provider: CommitteeProvider::new(),
                late_sig_store: late_sig_store::shared(3),
            });
            actor.head_proof_retry = Some(Box::new(Finalized {
                round: Round::new(Epoch::new(0), View::new(2)),
                digest: Digest(head_hash),
                vrf_seed: None,
                consensus_data: ConsensusData::default(),
            }));
            actor.pending_target = Some(super::super::PendingTarget {
                tip: ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(3)),
                    height: Height::new(2),
                    digest: Digest(B256::repeat_byte(0x73)),
                },
                live: None,
            });

            let mut budget = crate::finalization::evidence::EventWorkBudget::new();
            let outcome = actor
                .retry_head_proof(Some(&context), &mut budget)
                .await
                .expect("local head proof lookup timeout is nonfatal");
            assert_eq!(outcome, None, "higher target must remain eligible");
            assert!(!budget.marshal_timed_out);
            assert!(actor.head_proof_retry.is_some());
            assert!(actor.pending_target.is_some());
            assert_eq!(actor.parked_reason, None);
            assert_eq!(view.read().last_finalized_number, 1);
            drop(marshal_actor);
        });
    }

    #[test]
    fn tip_read_budget_exhaustion_keeps_target_instead_of_failing() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|context| async move {
            let mut harness = harness(SchemeProvider::default());
            let tip = ConsensusTip {
                round: Round::new(Epoch::new(0), View::new(1)),
                height: Height::new(1),
                digest: Digest(B256::repeat_byte(0x71)),
            };
            let mut budget = crate::finalization::evidence::EventWorkBudget::new();
            for _ in 0..crate::finalization::evidence::MAX_MARSHAL_READS_PER_EVENT {
                assert!(budget.charge_read());
            }
            harness
                .actor
                .handle_tip(Some(&context), tip, &mut budget)
                .await
                .expect("work budget must not become a fatal archive error");
            assert_eq!(
                harness
                    .actor
                    .pending_target
                    .as_ref()
                    .map(|target| target.tip),
                Some(tip)
            );
        });
    }

    #[test]
    fn archived_block_without_its_f_clears_unresolved_identity() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);
            let mut marshal =
                start_live_marshal(&context, "unresolved-no-f", schemes.clone()).await;
            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = make_block_with_parent(182, 182, b181.digest().0);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(12)), b182.clone())
                    .await
            );
            let f182 = epoch0.sign_finalization(12, 11, b182.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;
            assert!(marshal
                .mailbox
                .get_finalization(Height::new(181))
                .await
                .is_none());
            assert!(marshal.mailbox.get_block(Height::new(181)).await.is_some());

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            let round = Round::new(Epoch::new(0), View::new(11));
            actor.unresolved.insert(round, b181.digest());
            let outcome = actor
                .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                .await
                .expect("missing direct F parks only the target");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::Parked);
            assert!(
                actor.unresolved.is_empty(),
                "archive branch check must release the unresolved identity"
            );
            assert_eq!(
                actor.parked_reason,
                Some(super::super::ParkedReason::MissingCertificate)
            );
            assert!(actor.resolution_stall.is_none());
            assert_eq!(view.read().last_finalized_number, 180);
        });
    }

    #[test]
    fn unresolved_future_epoch_f_becomes_a_missing_verifier_target() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let epoch1 = EpochCommitteeFixture::new(1);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);
            let mut marshal = start_live_marshal(&context, "unresolved-e1", schemes.clone()).await;
            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(1), View::new(1)), b181.clone())
                    .await
            );
            let f181 = epoch1.sign_finalization(1, 0, b181.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f181));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(181)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            let round = Round::new(Epoch::new(1), View::new(1));
            actor.unresolved.insert(round, b181.digest());
            let outcome = actor
                .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                .await
                .expect("future verifier absence is pending");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::Parked);
            assert!(actor.unresolved.is_empty());
            assert_eq!(
                actor.parked_reason,
                Some(super::super::ParkedReason::MissingVerifier)
            );
            assert!(
                actor.resolution_stall.is_none(),
                "missing verifier is not a missing-block divergence"
            );
            assert_eq!(view.read().last_finalized_number, 180);
        });
    }

    #[test]
    fn higher_target_restarts_a_parked_anchor_search_with_its_own_digest() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let epoch1 = EpochCommitteeFixture::new(1);
            let marshal =
                start_live_marshal(&context, "parked-target-change", SchemeProvider::default())
                    .await;
            let mut harness = harness_with_signed_marshal(&epoch0, B256::ZERO, marshal.mailbox);
            let old = ConsensusTip {
                round: Round::new(Epoch::new(1), View::new(1)),
                height: Height::new(3),
                digest: Digest(B256::repeat_byte(0x33)),
            };
            let newer = ConsensusTip {
                round: Round::new(Epoch::new(1), View::new(2)),
                height: Height::new(4),
                digest: Digest(B256::repeat_byte(0x44)),
            };
            harness.actor.pending_target = Some(super::super::PendingTarget {
                tip: newer,
                live: None,
            });
            harness.actor.active_job = Some(super::super::CatchUpJob {
                base_height: 0,
                base_hash: B256::ZERO,
                anchor_height: 3,
                anchor_digest: old.digest,
                anchor_proof: None,
                temporary_anchor: false,
                missing_epochs: std::collections::BTreeSet::from([Epoch::new(1)]),
                phase: super::super::CatchUpPhase::LocateAnchor {
                    height: 0,
                    child: None,
                },
            });

            let outcome = harness
                .actor
                .step_job(
                    Some(&context),
                    &mut crate::finalization::evidence::EventWorkBudget::new(),
                )
                .await
                .expect("changed target must not reuse the old anchor digest");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::Parked);
            assert!(harness.actor.active_job.is_none());
            assert_eq!(
                harness
                    .actor
                    .parked_probe
                    .as_ref()
                    .map(|probe| probe.target),
                Some(newer)
            );

            epoch1.register(
                &harness.actor.deps.certificate_scheme_provider,
                &harness.actor.deps.committee_provider,
            );
            let outcome = harness
                .actor
                .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                .await
                .expect("registration restarts from the current target");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::Parked);
            assert!(harness.actor.parked_probe.is_none());
            assert_eq!(
                harness
                    .actor
                    .pending_target
                    .as_ref()
                    .map(|target| target.tip),
                Some(newer)
            );
            assert!(harness.actor.last_event_stats.marshal_reads > 0);
        });
    }

    /// Actor shutdown with a parked target exits cleanly.
    #[test]
    fn actor_shutdown_with_parked_target_and_live_subscription_terminates_cleanly() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let marshal = start_live_marshal(&context, "clean-shutdown", schemes.clone()).await;
            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                view,
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            actor = actor.with_pending_retry_interval(Duration::from_secs(60));
            actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(11)),
                    height: Height::new(181),
                    digest: b181.digest(),
                })
                .await
                .expect("park tip");
            assert_eq!(
                actor.pending_target.as_ref().map(|t| t.tip.digest),
                Some(b181.digest())
            );
            let actor_task = context
                .child("actor")
                .spawn(move |ctx| async move { actor.run(ctx).await });

            // Drop mailbox while target is parked.
            drop(ingress);
            actor_task
                .await
                .expect("actor task must join cleanly")
                .expect("shutdown with parked target must return Ok(())");
        });
    }

    /// An unresolved live `Finalized` (block absent from cache and Marshal) parks
    /// in the actor loop without monopolizing the mailbox, so subsequent messages
    /// (`ReconcileMarshalTip`, `CertifiedNotarization`) still make immediate progress.
    #[test]
    fn unresolved_live_finalization_does_not_block_mailbox() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal =
                start_live_marshal(&context, "nonblocking-live", schemes.clone()).await;
            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            let f181 = epoch0.sign_finalization(11, 10, b181.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f181));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(181)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            let actor_task = context
                .child("actor")
                .spawn(move |ctx| async move { actor.run(ctx).await });

            // Send a valid Finalized whose block is absent from cache and Marshal.
            let missing_block = make_block(999, 99);
            let missing_f = epoch0.sign_finalization(99, 98, missing_block.digest());
            let mut missing_event =
                finalized_event(&missing_block, 0, 99, 98, epoch0.addresses.clone());
            missing_event
                .consensus_data
                .finalized_certificate
                .encoded_certificate = missing_f.encode().into();
            missing_event
                .consensus_data
                .finalized_certificate
                .signer_bitmap = vec![1; epoch0.addresses.len()];
            ingress
                .notify_finalized(missing_event)
                .expect("send unresolved live F");

            // Immediately send a resolvable Marshal tip; mailbox must not be blocked.
            ingress
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(11)),
                    height: Height::new(181),
                    digest: b181.digest(),
                })
                .expect("send tip");

            wait_for_height(&context, &view, 181, 50, Duration::from_millis(10)).await;
            assert_eq!(view.read().forkchoice.finalized_block_hash, b181.digest().0);

            drop(ingress);
            actor_task.await.expect("join").expect("clean shutdown");
        });
    }

    /// Missing verifier scheme in `persist_finalization_record` must never write
    /// a zero `committee_set_hash` record or advance `view`; once registered,
    /// retry persists the canonical record and publishes `view`.
    #[test]
    fn missing_scheme_does_not_write_zero_committee_record_or_advance_view() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|_ctx| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            let block = make_block(1, 7);
            let proof_key = CertifiedParentProofKey::new(0, 1, block.digest().0);
            let view = new_finalization_view(B256::ZERO, 0, None);
            let store = FinalizedParentCertStore::new();
            let (actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: None,
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store.clone(),
                certificate_scheme_provider: schemes.clone(),
                committee_provider: committees.clone(),
                late_sig_store: late_sig_store::shared(3),
            });

            // Scheme not registered yet -> PendingVerifier, no zero-hash record, view stays 0.
            let outcome = actor
                .process_finalization(
                    finalized_event(&block, 0, 1, 0, epoch0.addresses.clone()),
                    block.clone(),
                )
                .await
                .expect("missing scheme is non-fatal pending");
            assert_eq!(outcome, super::super::ApplyOutcome::PendingVerifier);
            assert!(
                store.get_finalization(proof_key).is_none(),
                "must not write a zero committee_set_hash record"
            );
            assert_eq!(view.read().last_finalized_number, 0);
            assert!(view.read().last_finalized_round.is_none());

            // Register scheme -> retry persists canonical non-zero committee_set_hash and publishes view.
            epoch0.register(&schemes, &committees);
            let outcome = actor
                .process_finalization(
                    finalized_event(&block, 0, 1, 0, epoch0.addresses.clone()),
                    block.clone(),
                )
                .await
                .expect("registered scheme succeeds");
            assert_eq!(outcome, super::super::ApplyOutcome::Applied);
            let rec = store
                .get_finalization(proof_key)
                .expect("canonical record persisted");
            assert_ne!(rec.committee_set_hash, B256::ZERO);
            assert_eq!(view.read().last_finalized_number, 1);
        });
    }

    /// B1(a): A fresh/genesis node with `last_finalized_round == None` must park
    /// an `Epoch 1` tip as `AwaitingFutureEpochVerifier` (never fatal), while a
    /// missing `Epoch 0` verifier remains fatal.
    #[test]
    fn genesis_node_with_none_round_parks_epoch1_tip_and_fails_missing_epoch0() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let epoch1 = EpochCommitteeFixture::new(1);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "b1a-genesis", schemes.clone()).await;
            let genesis_hash = B256::repeat_byte(0x11);
            let b1 = make_block_with_parent(1, 1, genesis_hash);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(1), View::new(2)), b1.clone())
                    .await
            );
            let f1 = epoch1.sign_finalization(2, 1, b1.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f1));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(1)).await;

            // Genesis view has `last_finalized_round == None`.
            let view = new_finalization_view(genesis_hash, 0, None);
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });

            // Epoch 1 tip with `round == None` parks (does NOT fail fatally).
            let outcome = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(1), View::new(2)),
                    height: Height::new(1),
                    digest: b1.digest(),
                })
                .await
                .expect("genesis node must park unregistered epoch 1 tip");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::Parked);
            assert_eq!(view.read().last_finalized_number, 0);

            // Missing Epoch 0 verifier with `round == None` stays fatal.
            let empty_schemes = SchemeProvider::default();
            let empty_committees = CommitteeProvider::new();
            let view0 = new_finalization_view(genesis_hash, 0, None);
            let (mut actor0, _ingress0) = FinalizationActor::new(FinalizationActorDeps {
                view: view0,
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: empty_schemes,
                committee_provider: empty_committees,
                late_sig_store: late_sig_store::shared(3),
            });
            let err = actor0
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(2)),
                    height: Height::new(1),
                    digest: b1.digest(),
                })
                .await
                .expect_err("missing epoch 0 verifier must remain fatal");
            assert!(matches!(
                finalization_error(err),
                crate::errors::FinalizationError::ArchiveReconciliation { height: 1, .. }
            ));
        });
    }

    /// B1(b): Implicit `E`-tail (`h181` CN, `h182` has NO standalone Marshal F) +
    /// boundary child `h183` (in `E+1`) carrying embedded `F(h182)` under `E` +
    /// `Tip(h183, E+1)`. `E+1` is registered ONLY after `FinalizationView` reaches
    /// `h182` via `StreamAnchors` digest walk; then the next tick advances `h183`.
    #[test]
    fn cross_epoch_implicit_tail_advances_via_embedded_f_then_registers_e1() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let epoch1 = EpochCommitteeFixture::new(1);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal =
                start_live_marshal(&context, "b1b-cross-epoch", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181,
                11,
                10,
                ParentParticipationProof::CertifiedNotarization,
            );
            // Boundary child b183 carries embedded genuine F(b182) under epoch 0.
            let b183 = epoch0.block_with_parent_witness(
                &b182,
                12,
                11,
                ParentParticipationProof::Finalization,
            );

            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(12)), b182.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(1), View::new(1)), b183.clone())
                    .await
            );
            // Only direct F(183) in unregistered epoch 1; NO standalone F(181) or F(182).
            let f183 = epoch1.sign_finalization(1, 0, b183.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f183));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(183)).await;
            assert!(marshal
                .mailbox
                .get_finalization(Height::new(181))
                .await
                .is_none());
            assert!(marshal
                .mailbox
                .get_finalization(Height::new(182))
                .await
                .is_none());

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes.clone(),
                committee_provider: committees.clone(),
                late_sig_store: late_sig_store::shared(3),
            });
            let actor = actor.with_pending_retry_interval(Duration::from_millis(20));
            let actor_task = context
                .child("actor")
                .spawn(move |ctx| async move { actor.run(ctx).await });

            ingress
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(1), View::new(1)),
                    height: Height::new(183),
                    digest: b183.digest(),
                })
                .expect("send E+1 tip");

            // StreamAnchors discovers embedded F(182) on b183's branch and advances 181..=182
            // BEFORE epoch 1 is registered.
            wait_for_height(&context, &view, 182, 50, Duration::from_millis(10)).await;
            assert_eq!(view.read().last_finalized_number, 182);
            assert_eq!(view.read().forkchoice.finalized_block_hash, b182.digest().0);

            // Now simulate activation registering epoch 1 ONLY after view reached H_last (182).
            epoch1.register(&schemes, &committees);
            wait_for_height(&context, &view, 183, 50, Duration::from_millis(20)).await;
            assert_eq!(view.read().forkchoice.finalized_block_hash, b183.digest().0);
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(1), View::new(1)))
            );

            drop(ingress);
            actor_task.await.expect("join").expect("clean shutdown");
        });
    }

    /// Many parked tips retain the highest target and bound work per event.
    #[test]
    fn thousand_parked_tips_preserve_all_conflict_identities_and_bound_event_work() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "1k-tips", schemes.clone()).await;
            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            let f181 = epoch0.sign_finalization(11, 10, b181.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f181));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(181)).await;
            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view,
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });

            for i in 1u64..=1_000 {
                let h = 181 + i;
                let mut hash = [0u8; 32];
                hash[..8].copy_from_slice(&h.to_le_bytes());
                let outcome = actor
                    .reconcile_marshal_tip(ConsensusTip {
                        round: Round::new(Epoch::new(0), View::new(10 + i)),
                        height: Height::new(h),
                        digest: Digest(B256::from(hash)),
                    })
                    .await
                    .expect("park tip without eviction");
                assert_ne!(outcome, super::super::ReconcileStepOutcome::Idle);
                assert!(
                    actor.last_event_stats.marshal_reads
                        <= crate::finalization::evidence::MAX_MARSHAL_READS_PER_EVENT
                );
                assert!(
                    actor.last_event_stats.crypto_checks
                        <= crate::finalization::evidence::MAX_VERIFY_CRYPTO_PER_EVENT
                );
                assert!(
                    actor.last_event_stats.applied_steps
                        <= crate::finalization::evidence::MAX_APPLY_STEPS_PER_EVENT
                );
            }
            assert_eq!(
                actor.pending_target.as_ref().map(|t| t.tip.height.get()),
                Some(1_181)
            );

            let historical = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(11)),
                    height: Height::new(181),
                    digest: Digest(B256::repeat_byte(0xFE)),
                })
                .await
                .expect_err("historical conflict must be detected through the archive");
            assert!(matches!(
                finalization_error(historical),
                crate::errors::FinalizationError::ArchiveReconciliation { height: 181, .. }
            ));

            // A conflicting identity for the retained target is fatal.
            let err = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(2000)),
                    height: Height::new(1_181),
                    digest: Digest(B256::repeat_byte(0xFF)),
                })
                .await
                .expect_err("same-height target conflict must remain fatal");
            assert!(matches!(
                finalization_error(err),
                crate::errors::FinalizationError::SameHeightInconsistency {
                    block_number: 1_181,
                    ..
                }
            ));
        });
    }

    /// B2: Multi-turn 3-phase catch-up respects `(R <= 64, C <= 8, A <= 16)` on
    /// every turn and publishes zero heights before Phase 2 (`Verify`) finishes.
    #[test]
    fn multi_turn_catchup_respects_budgets_and_atomic_verify_before_apply() {
        deterministic::Runner::timed(Duration::from_secs(15)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "multi-turn", schemes.clone()).await;

            // Build a 24-block chain (exceeds C = 8 and A = 16 -> requires multiple turns).
            let b0 = make_block(100, 100);
            let mut chain = vec![b0.clone()];
            let b1 = make_block_with_parent(101, 101, b0.digest().0);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b1.clone())
                    .await
            );
            chain.push(b1);
            for offset in 2u64..=24 {
                let parent = chain.last().unwrap();
                let child = epoch0.block_with_parent_witness(
                    parent,
                    10 + offset - 1,
                    10 + offset - 2,
                    ParentParticipationProof::CertifiedNotarization,
                );
                assert!(
                    marshal
                        .mailbox
                        .verified(
                            Round::new(Epoch::new(0), View::new(10 + offset)),
                            child.clone()
                        )
                        .await
                );
                chain.push(child);
            }
            let top = chain.last().unwrap().clone();
            let f_top = epoch0.sign_finalization(34, 33, top.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f_top));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(124)).await;

            let view = new_finalization_view(
                b0.digest().0,
                100,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });

            // Preflight authenticates the full ancestry before forward application.
            let mut outcome = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(34)),
                    height: Height::new(124),
                    digest: top.digest(),
                })
                .await
                .expect("first turn yields");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::YieldMoreWork);
            assert!(matches!(
                actor.active_job.as_ref().map(|job| &job.phase),
                Some(super::super::CatchUpPhase::ApplyForward { .. })
            ));
            assert_eq!(
                view.read().last_finalized_number - 100,
                actor.last_event_stats.applied_steps as u64,
                "published heights must equal completed forward steps"
            );
            assert!(
                actor.last_event_stats.marshal_reads
                    <= crate::finalization::evidence::MAX_MARSHAL_READS_PER_EVENT
            );
            assert!(
                actor.last_event_stats.crypto_checks
                    <= crate::finalization::evidence::MAX_VERIFY_CRYPTO_PER_EVENT
            );
            assert!(actor.last_event_stats.applied_steps > 0);

            // Step remaining turns until Idle, asserting budgets on EVERY turn.
            let mut turns = 1usize;
            let mut saw_partial_apply = view.read().last_finalized_number < 124;
            while outcome != super::super::ReconcileStepOutcome::Idle {
                turns += 1;
                assert!(turns < 20, "catch-up must finish in bounded turns");
                outcome = actor
                    .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                    .await
                    .expect("turn succeeds");
                assert!(
                    actor.last_event_stats.marshal_reads
                        <= crate::finalization::evidence::MAX_MARSHAL_READS_PER_EVENT
                );
                assert!(
                    actor.last_event_stats.crypto_checks
                        <= crate::finalization::evidence::MAX_VERIFY_CRYPTO_PER_EVENT
                );
                assert!(
                    actor.last_event_stats.applied_steps
                        <= crate::finalization::evidence::MAX_APPLY_STEPS_PER_EVENT
                );
                let h = view.read().last_finalized_number;
                if h > 100 && h < 124 {
                    saw_partial_apply = true;
                }
            }
            assert!(
                saw_partial_apply,
                "24 heights must span >1 Apply turn (A=16)"
            );
            assert_eq!(view.read().last_finalized_number, 124);
            assert_eq!(view.read().forkchoice.finalized_block_hash, top.digest().0);
        });
    }

    /// A failed local proof write does not hold back an authenticated F. A later
    /// duplicate F repairs the proof after BlockCache eviction without replaying
    /// the block's effects.
    #[test]
    fn finalized_block_advances_despite_proof_write_failure_and_duplicate_repairs_proof() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "apply-resume", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181,
                11,
                10,
                ParentParticipationProof::CertifiedNotarization,
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(12)), b182.clone())
                    .await
            );
            let f182 = epoch0.sign_finalization(12, 11, b182.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let store = FinalizedParentCertStore::new();
            let cache = crate::finalization::block_cache::BlockCache::new();
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: cache.clone(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store.clone(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });

            // Fail put_finalization on h182 after h181 CN succeeds.
            store.fail_next_put_finalization(FINALIZE_PERSIST_MAX_ATTEMPTS);
            let outcome = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(12)),
                    height: Height::new(182),
                    digest: b182.digest(),
                })
                .await
                .expect("authenticated F advances despite local proof write failure");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::Idle);
            assert_eq!(view.read().last_finalized_number, 182);
            assert_eq!(view.read().forkchoice.finalized_block_hash, b182.digest().0);
            assert!(store
                .get_finalization(CertifiedParentProofKey::new(0, 12, b182.digest().0))
                .is_none());

            // Evict the block and deliver the same signed F. Repair must read the
            // accepted block from Marshal, not re-apply its chain effects.
            cache.evict_at_or_below(u64::MAX);
            actor
                .handle_finalized(&context, signed_finalized_event(&epoch0, &b182, 0, 12, 11))
                .await
                .expect("duplicate F repairs its local proof");
            assert_eq!(actor.last_event_stats.applied_steps, 0);
            assert_eq!(view.read().last_finalized_number, 182);
            assert!(store
                .get_finalization(CertifiedParentProofKey::new(0, 12, b182.digest().0))
                .is_some());
        });
    }

    /// Multi-window (a): Sibling archived `F` at height `h` with `payload != p.digest`
    /// on `T`'s branch is FATAL immediately with zero effects.
    #[test]
    fn stream_anchors_sibling_archived_f_on_different_fork_is_fatal() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let epoch1 = EpochCommitteeFixture::new(1);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal =
                start_live_marshal(&context, "sibling-f-fatal", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181_branch = make_block_with_parent(181, 181, b180.digest().0);
            let b181_sibling = make_block_with_parent(181, 999, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181_branch,
                11,
                10,
                ParentParticipationProof::Finalization,
            );

            // Put sibling F at height 181, while Tip(182, epoch 1) extends b181_branch!
            assert!(
                marshal
                    .mailbox
                    .verified(
                        Round::new(Epoch::new(0), View::new(11)),
                        b181_sibling.clone()
                    )
                    .await
            );
            let f181_sibling = epoch0.sign_finalization(11, 10, b181_sibling.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f181_sibling));
            assert!(
                marshal
                    .mailbox
                    .verified(
                        Round::new(Epoch::new(0), View::new(12)),
                        b181_branch.clone()
                    )
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(1), View::new(1)), b182.clone())
                    .await
            );
            let f182 = epoch1.sign_finalization(1, 0, b182.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(181)).await;
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let store = FinalizedParentCertStore::new();
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store.clone(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });

            let err = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(1), View::new(1)),
                    height: Height::new(182),
                    digest: b182.digest(),
                })
                .await
                .expect_err("archived F on sibling fork must be fatal");
            assert!(matches!(
                finalization_error(err),
                crate::errors::FinalizationError::ArchiveReconciliation { height: 181, .. }
            ));
            assert_eq!(view.read().last_finalized_number, 180);
            assert_eq!(store.len(), 0);
        });
    }

    /// Multi-window (b): Sibling notarized-only block at height `h` (no `F`) is
    /// ignored because `StreamAnchors` walks strictly by `parent_hash` digest.
    #[test]
    fn stream_anchors_sibling_notarized_block_ignored_advances_on_target_branch() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let epoch1 = EpochCommitteeFixture::new(1);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal =
                start_live_marshal(&context, "sibling-notar-ok", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181_branch = make_block_with_parent(181, 181, b180.digest().0);
            let b181_sibling = make_block_with_parent(181, 888, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181_branch,
                11,
                10,
                ParentParticipationProof::Finalization,
            );

            // Sibling notarized block at height 181 (no F), plus canonical branch 181 -> 182.
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(10)), b181_sibling)
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(
                        Round::new(Epoch::new(0), View::new(11)),
                        b181_branch.clone()
                    )
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(1), View::new(1)), b182.clone())
                    .await
            );
            let f182 = epoch1.sign_finalization(1, 0, b182.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });

            let _ = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(1), View::new(1)),
                    height: Height::new(182),
                    digest: b182.digest(),
                })
                .await
                .expect("sibling notarized block ignored");
            assert_eq!(view.read().last_finalized_number, 181);
            assert_eq!(
                view.read().forkchoice.finalized_block_hash,
                b181_branch.digest().0
            );
        });
    }

    /// Multi-window (c), (f) Codex counterexample (`S = 8`, `{4, 9, 16}`), and (h)
    /// singleton window (`min == max`): advances bottom-up `4 -> 9 -> 16` with
    /// zero false overflow and bounded per-event budgets.
    #[test]
    fn codex_counterexample_s8_candidates_4_9_16_and_singleton_window_advance_cleanly() {
        deterministic::Runner::timed(Duration::from_secs(15)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "codex-s8", schemes.clone()).await;

            // Head at 0, chain 1..=16, direct F ONLY at {4, 9, 16} (singleton window 0 at {4},
            // window 1 has {lowest=9, highest=16}); all other heights CN-only.
            let b0 = make_block(0, 0);
            let mut chain = vec![b0.clone()];
            let b1 = make_block_with_parent(1, 1, b0.digest().0);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(1)), b1.clone())
                    .await
            );
            chain.push(b1);
            for h in 2u64..=16 {
                let parent = chain.last().unwrap();
                let proof_kind = if h - 1 == 4 || h - 1 == 9 {
                    ParentParticipationProof::Finalization
                } else {
                    ParentParticipationProof::CertifiedNotarization
                };
                let child = epoch0.block_with_parent_witness(parent, h - 1, h - 2, proof_kind);
                assert!(
                    marshal
                        .mailbox
                        .verified(Round::new(Epoch::new(0), View::new(h)), child.clone())
                        .await
                );
                chain.push(child);
            }
            for &anchor_h in &[4u64, 9, 16] {
                let blk = &chain[anchor_h as usize];
                let f = epoch0.sign_finalization(anchor_h, anchor_h - 1, blk.digest());
                let _ = marshal.mailbox.report(Activity::Finalization(f));
                wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(anchor_h))
                    .await;
            }

            let view = new_finalization_view(b0.digest().0, 0, None);
            let (adv_tx, mut adv_rx) = tokio::sync::mpsc::unbounded_channel();
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            actor = actor.with_view_advanced(adv_tx);

            let top = chain[16].clone();
            let mut outcome = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(16)),
                    height: Height::new(16),
                    digest: top.digest(),
                })
                .await
                .expect("no false overflow on {4, 9, 16} with S=8");
            let mut turns = 1usize;
            while outcome != super::super::ReconcileStepOutcome::Idle {
                turns += 1;
                assert!(turns < 20);
                outcome = actor
                    .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                    .await
                    .expect("chunk succeeds");
                assert!(
                    actor.last_event_stats.marshal_reads
                        <= crate::finalization::evidence::MAX_MARSHAL_READS_PER_EVENT
                );
                assert!(
                    actor.last_event_stats.crypto_checks
                        <= crate::finalization::evidence::MAX_VERIFY_CRYPTO_PER_EVENT
                );
                assert!(
                    actor.last_event_stats.applied_steps
                        <= crate::finalization::evidence::MAX_APPLY_STEPS_PER_EVENT
                );
            }

            let mut applied = Vec::new();
            while let Ok(h) = adv_rx.try_recv() {
                applied.push(h);
            }
            assert_eq!(applied, (1u64..=16).collect::<Vec<_>>());
            assert_eq!(view.read().last_finalized_number, 16);
        });
    }

    /// A long CN-only span needs only the terminal F and valid child witnesses.
    #[test]
    fn cn_only_span_exceeding_s_max_parks_with_overflow_metric_and_no_partial_apply() {
        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
                let epoch0 = EpochCommitteeFixture::new(0);
                let schemes = SchemeProvider::default();
                let committees = CommitteeProvider::new();
                epoch0.register(&schemes, &committees);

                let mut marshal =
                    start_live_marshal(&context, "cn-overflow", schemes.clone()).await;

                // Chain 1..=9 with only F(9).
                let b0 = make_block(0, 0);
                let mut chain = vec![b0.clone()];
                let b1 = make_block_with_parent(1, 1, b0.digest().0);
                assert!(
                    marshal
                        .mailbox
                        .verified(Round::new(Epoch::new(0), View::new(1)), b1.clone())
                        .await
                );
                chain.push(b1);
                for h in 2u64..=9 {
                    let parent = chain.last().unwrap();
                    let child = epoch0.block_with_parent_witness(
                        parent,
                        h - 1,
                        h - 2,
                        ParentParticipationProof::CertifiedNotarization,
                    );
                    assert!(
                        marshal
                            .mailbox
                            .verified(Round::new(Epoch::new(0), View::new(h)), child.clone())
                            .await
                    );
                    chain.push(child);
                }
                let top = chain[9].clone();
                let f9 = epoch0.sign_finalization(9, 8, top.digest());
                let _ = marshal.mailbox.report(Activity::Finalization(f9));
                wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(9)).await;

                let view = new_finalization_view(b0.digest().0, 0, None);
                let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                    view: view.clone(),
                    block_cache: crate::finalization::block_cache::BlockCache::new(),
                    marshal_mailbox: Some(marshal.mailbox.clone()),
                    bridge: None,
                    dkg_manager: crate::dkg_manager::Mailbox::new(),
                    vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                    parent_cert_store: FinalizedParentCertStore::new(),
                    certificate_scheme_provider: schemes,
                    committee_provider: committees,
                    late_sig_store: late_sig_store::shared(3),
                });
                let mut outcome = actor
                    .reconcile_marshal_tip(ConsensusTip {
                        round: Round::new(Epoch::new(0), View::new(9)),
                        height: Height::new(9),
                        digest: top.digest(),
                    })
                    .await
                    .expect("terminal F authenticates the entire linked chain");
                let mut turns = 1;
                while outcome != super::super::ReconcileStepOutcome::Idle {
                    turns += 1;
                    assert!(turns < 20);
                    outcome = actor
                        .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                        .await
                        .expect("sparse chain progresses");
                }
                assert_eq!(view.read().last_finalized_number, 9);
                assert_eq!(view.read().forkchoice.finalized_block_hash, top.digest().0);

                let overflow_count = snapshotter
                    .snapshot()
                    .into_vec()
                    .into_iter()
                    .find_map(|(k, _, _, v)| {
                        (k.key().name() == "outbe_consensus_finalization_segment_overflow_total")
                            .then_some(match v {
                                DebugValue::Counter(c) => c,
                                _ => 0,
                            })
                    })
                    .unwrap_or(0);
                assert_eq!(overflow_count, 0);
            });
        });
    }

    /// A higher target arriving during catch-up is retained and applied in order.
    #[test]
    fn higher_target_mid_stream_anchors_splices_and_reuses_window_bounds() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "splice-reuse", schemes.clone()).await;

            // Chain 1..=12 with sparse finalization anchors at {4, 8, 12}.
            let b0 = make_block(0, 0);
            let mut chain = vec![b0.clone()];
            let b1 = make_block_with_parent(1, 1, b0.digest().0);
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(1)), b1.clone())
                    .await
            );
            chain.push(b1);
            for h in 2u64..=12 {
                let parent = chain.last().unwrap();
                let child = epoch0.block_with_parent_witness(
                    parent,
                    h - 1,
                    h - 2,
                    ParentParticipationProof::CertifiedNotarization,
                );
                assert!(
                    marshal
                        .mailbox
                        .verified(Round::new(Epoch::new(0), View::new(h)), child.clone())
                        .await
                );
                chain.push(child);
            }
            for &anchor_h in &[4u64, 8, 12] {
                let blk = &chain[anchor_h as usize];
                let f = epoch0.sign_finalization(anchor_h, anchor_h - 1, blk.digest());
                let _ = marshal.mailbox.report(Activity::Finalization(f));
                wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(anchor_h))
                    .await;
            }

            let view = new_finalization_view(b0.digest().0, 0, None);
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            // Start with T=8, then deliver a higher target before catch-up completes.
            let first = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(8)),
                    height: Height::new(8),
                    digest: chain[8].digest(),
                })
                .await
                .expect("first target starts catch-up");
            assert_ne!(first, super::super::ReconcileStepOutcome::Idle);

            let mut outcome = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(12)),
                    height: Height::new(12),
                    digest: chain[12].digest(),
                })
                .await
                .expect("higher target is retained");
            while outcome != super::super::ReconcileStepOutcome::Idle {
                outcome = actor
                    .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                    .await
                    .expect("chunk succeeds");
            }
            assert_eq!(view.read().last_finalized_number, 12);
            assert_eq!(
                view.read().forkchoice.finalized_block_hash,
                chain[12].digest().0
            );
        });
    }

    /// N2 parity: the exact same `Finalization`, applied directly vs embedded in
    /// child Phase 1, produces byte-identical `view.prev_randao` and
    /// `CertifiedParentProofRecord`.
    #[test]
    fn direct_and_embedded_f_produce_byte_identical_prev_randao_and_record() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181,
                11,
                10,
                ParentParticipationProof::Finalization,
            );
            let f181 = epoch0.sign_finalization(11, 10, b181.digest());
            let f182 = epoch0.sign_finalization(12, 11, b182.digest());

            // Path A: Direct Marshal F(181).
            let mut marshal_a = start_live_marshal(&context, "n2-direct", schemes.clone()).await;
            assert!(
                marshal_a
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            let _ = marshal_a.mailbox.report(Activity::Finalization(f181));
            wait_for_marshal_finalization(&context, &marshal_a.mailbox, Height::new(181)).await;

            let view_a = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let store_a = FinalizedParentCertStore::new();
            let (mut actor_a, _in_a) = FinalizationActor::new(FinalizationActorDeps {
                view: view_a.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal_a.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store_a.clone(),
                certificate_scheme_provider: schemes.clone(),
                committee_provider: committees.clone(),
                late_sig_store: late_sig_store::shared(3),
            });
            let _ = actor_a
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(11)),
                    height: Height::new(181),
                    digest: b181.digest(),
                })
                .await
                .expect("direct F(181) applies");

            // Path B: Embedded F(181) in child b182, anchored by direct F(182).
            let mut marshal_b = start_live_marshal(&context, "n2-embedded", schemes.clone()).await;
            assert!(
                marshal_b
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            assert!(
                marshal_b
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(12)), b182.clone())
                    .await
            );
            let _ = marshal_b.mailbox.report(Activity::Finalization(f182));
            wait_for_marshal_finalization(&context, &marshal_b.mailbox, Height::new(182)).await;

            let view_b = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let store_b = FinalizedParentCertStore::new();
            let (mut actor_b, _in_b) = FinalizationActor::new(FinalizationActorDeps {
                view: view_b.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal_b.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store_b.clone(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            let _ = actor_b
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(0), View::new(12)),
                    height: Height::new(182),
                    digest: b182.digest(),
                })
                .await
                .expect("embedded F(181) + direct F(182) apply");

            let key_181 = CertifiedParentProofKey::new(0, 11, b181.digest().0);
            let rec_a = store_a.get_finalization(key_181).expect("direct record");
            let rec_b = store_b.get_finalization(key_181).expect("embedded record");
            assert_eq!(
                rec_a, rec_b,
                "direct and embedded F must produce byte-identical CertifiedParentProofRecord"
            );

            // Also verify direct vs embedded prev_randao parity directly via verify_child_parent_witness.
            let witness = verify_child_parent_witness(
                &b181,
                &b182,
                &actor_b.deps.certificate_scheme_provider,
                &actor_b.deps.committee_provider,
            )
            .expect("valid witness")
            .expect("registered");
            assert_eq!(
                view_a.read().prev_randao,
                witness.prev_randao,
                "direct F and embedded Phase 1 F must produce identical prev_randao"
            );
        });
    }

    /// N5: Same-hash `F(v2)` on a block carrying a `DealerLog` artifact never
    /// repeats block effects (`view_advanced` / DKG recording).
    #[test]
    fn dealer_log_duplicate_f_v2_never_repeats_block_effects() {
        deterministic::Runner::timed(Duration::from_secs(5)).start(|_ctx| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut raw = Block::default();
            raw.header.number = 50;
            raw.header.parent_hash = B256::ZERO;
            raw.header.extra_data = encode_consensus_header_artifact(
                &ConsensusHeaderArtifact::DealerLog(Bytes::from_static(b"dealer-log-bytes")),
            )
            .expect("encode dealer log artifact");
            let block = ConsensusBlock::from_sealed(SealedBlock::seal_slow(
                raw.map_header(OutbeHeader::new),
            ));

            let view = new_finalization_view(B256::ZERO, 49, None);
            let store = FinalizedParentCertStore::new();
            let (adv_tx, mut adv_rx) = tokio::sync::mpsc::unbounded_channel();
            let (actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: None,
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: store.clone(),
                certificate_scheme_provider: schemes,
                committee_provider: committees,
                late_sig_store: late_sig_store::shared(3),
            });
            let actor = actor.with_view_advanced(adv_tx);

            actor
                .process_finalization(
                    finalized_event(&block, 0, 20, 19, epoch0.addresses.clone()),
                    block.clone(),
                )
                .await
                .expect("v1 finalizes");
            assert_eq!(adv_rx.try_recv().ok(), Some(50));
            assert!(adv_rx.try_recv().is_err());

            // Same-hash F(v2): persists v2 proof, advances round, NEVER repeats block effects.
            actor
                .process_finalization(
                    finalized_event(&block, 0, 21, 19, epoch0.addresses.clone()),
                    block.clone(),
                )
                .await
                .expect("v2 persists");
            assert!(store
                .get_finalization(CertifiedParentProofKey::new(0, 21, block.digest().0))
                .is_some());
            assert_eq!(
                view.read().last_finalized_round,
                Some(Round::new(Epoch::new(0), View::new(21)))
            );
            assert!(
                adv_rx.try_recv().is_err(),
                "same-hash F(v2) on DealerLog block must not repeat block effects"
            );
        });
    }

    /// K1: Once `StreamAnchors` finishes with `NoRegisteredAnchor` below a
    /// future-epoch target, repeated idle ticks and duplicate same-height tips
    /// perform ZERO Marshal reads until verifier registration or a new lower
    /// registered-epoch target arrives.
    #[test]
    fn parked_future_epoch_zero_reads_on_idle_ticks_and_wakes_on_lower_registered_live_f() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let epoch1 = EpochCommitteeFixture::new(1);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "k1-zero-reads", schemes.clone()).await;

            // Chain 180 -> 181 (CN) -> 182 (CN) -> 183 (epoch 1 direct F).
            // No standalone F(181) or F(182), and b183 carries CN (not F) for b182,
            // so initial StreamAnchors finds NoRegisteredAnchor and parks.
            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch0.block_with_parent_witness(
                &b181,
                11,
                10,
                ParentParticipationProof::CertifiedNotarization,
            );
            let b183 = epoch0.block_with_parent_witness(
                &b182,
                12,
                11,
                ParentParticipationProof::CertifiedNotarization,
            );

            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(11)), b181.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(0), View::new(12)), b182.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(1), View::new(1)), b183.clone())
                    .await
            );
            let f183 = epoch1.sign_finalization(1, 0, b183.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f183));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(183)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let cache = crate::finalization::block_cache::BlockCache::new();
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: cache.clone(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes.clone(),
                committee_provider: committees.clone(),
                late_sig_store: late_sig_store::shared(3),
            });

            // Initial tip walks 183 -> 181, finds NoRegisteredAnchor, parks in ParkedFutureEpoch.
            let tip_183 = ConsensusTip {
                round: Round::new(Epoch::new(1), View::new(1)),
                height: Height::new(183),
                digest: b183.digest(),
            };
            let outcome = actor
                .reconcile_marshal_tip(tip_183)
                .await
                .expect("initial scan parks");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::Parked);
            assert!(actor.last_event_stats.marshal_reads > 0);
            assert_eq!(view.read().last_finalized_number, 180);

            // Repeated retries and duplicate tips remain bounded and publish no unverified height.
            for _ in 0..5 {
                let tick_outcome = actor
                    .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                    .await
                    .expect("idle tick stays parked");
                assert_eq!(tick_outcome, super::super::ReconcileStepOutcome::Parked);
                assert_eq!(actor.last_event_stats.marshal_reads, 0);
                assert_eq!(actor.last_event_stats.crypto_checks, 0);
                assert_eq!(view.read().last_finalized_number, 180);
            }
            let dup_outcome = actor
                .reconcile_marshal_tip(tip_183)
                .await
                .expect("duplicate same-height tip stays parked");
            assert_eq!(dup_outcome, super::super::ReconcileStepOutcome::Parked);
            assert_eq!(actor.last_event_stats.marshal_reads, 0);
            assert_eq!(view.read().last_finalized_number, 180);

            // Now a new lower registered-epoch Live F(182, epoch 0) arrives: wakes
            // ParkedFutureEpoch and advances 181..=182 without re-walking 183.
            let f182 = epoch0.sign_finalization(12, 11, b182.digest());
            let mut live_182 = finalized_event(&b182, 0, 12, 11, epoch0.addresses.clone());
            live_182
                .consensus_data
                .finalized_certificate
                .encoded_certificate = f182.encode().into();
            live_182.consensus_data.finalized_certificate.signer_bitmap =
                vec![1; epoch0.addresses.len()];
            cache.insert_bounded(b182.digest(), b182.clone());
            // The voter reporter also delivers F to Marshal, making the lower
            // certified anchor available in its finalized archive.
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;
            actor
                .handle_finalized(&context, live_182)
                .await
                .expect("lower registered Live F wakes and advances 181..=182");
            assert_eq!(view.read().last_finalized_number, 182);
            assert_eq!(view.read().forkchoice.finalized_block_hash, b182.digest().0);

            // Register epoch 1 verifier -> next reconcile advances 183.
            epoch1.register(&schemes, &committees);
            let final_outcome = actor
                .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                .await
                .expect("registered epoch 1 advances 183");
            assert_eq!(final_outcome, super::super::ReconcileStepOutcome::Idle);
            assert_eq!(view.read().last_finalized_number, 183);
        });
    }

    /// K1 multi-epoch: when `Tip` is in unregistered `Epoch 2` and an intermediate
    /// candidate `F` on its branch is in unregistered `Epoch 1`, `ParkedFutureEpoch`
    /// watches BOTH epochs — idle ticks perform 0 Marshal reads, and registering
    /// ONLY `Epoch 1` wakes immediately to advance through `Epoch 1` while keeping
    /// `Epoch 2` parked.
    #[test]
    fn parked_future_epoch_wakes_when_intermediate_candidate_epoch_registers() {
        deterministic::Runner::timed(Duration::from_secs(10)).start(|context| async move {
            let epoch0 = EpochCommitteeFixture::new(0);
            let epoch1 = EpochCommitteeFixture::new(1);
            let epoch2 = EpochCommitteeFixture::new(2);
            let schemes = SchemeProvider::default();
            let committees = CommitteeProvider::new();
            epoch0.register(&schemes, &committees);

            let mut marshal = start_live_marshal(&context, "k1-multi-epoch", schemes.clone()).await;

            let b180 = make_block(180, 180);
            let b181 = make_block_with_parent(181, 181, b180.digest().0);
            let b182 = epoch1.block_with_parent_witness(
                &b181,
                2,
                1,
                ParentParticipationProof::Finalization,
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(1), View::new(2)), b181.clone())
                    .await
            );
            assert!(
                marshal
                    .mailbox
                    .verified(Round::new(Epoch::new(2), View::new(1)), b182.clone())
                    .await
            );
            let f182 = epoch2.sign_finalization(1, 0, b182.digest());
            let _ = marshal.mailbox.report(Activity::Finalization(f182));
            wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(182)).await;

            let view = new_finalization_view(
                b180.digest().0,
                180,
                Some(Round::new(Epoch::new(0), View::new(10))),
            );
            let (mut actor, _ingress) = FinalizationActor::new(FinalizationActorDeps {
                view: view.clone(),
                block_cache: crate::finalization::block_cache::BlockCache::new(),
                marshal_mailbox: Some(marshal.mailbox.clone()),
                bridge: None,
                dkg_manager: crate::dkg_manager::Mailbox::new(),
                vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                parent_cert_store: FinalizedParentCertStore::new(),
                certificate_scheme_provider: schemes.clone(),
                committee_provider: committees.clone(),
                late_sig_store: late_sig_store::shared(3),
            });

            // Tip(182, epoch 2): sees unregistered embedded F(181, epoch 1), parks.
            let outcome = actor
                .reconcile_marshal_tip(ConsensusTip {
                    round: Round::new(Epoch::new(2), View::new(1)),
                    height: Height::new(182),
                    digest: b182.digest(),
                })
                .await
                .expect("parks awaiting epoch 1 or 2");
            assert_eq!(outcome, super::super::ReconcileStepOutcome::Parked);
            assert_eq!(view.read().last_finalized_number, 180);

            // Retry remains bounded and does not publish an unauthenticated block.
            let idle_outcome = actor
                .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                .await
                .expect("idle tick stays parked");
            assert_eq!(idle_outcome, super::super::ReconcileStepOutcome::Parked);
            assert_eq!(actor.last_event_stats.marshal_reads, 0);
            assert_eq!(view.read().last_finalized_number, 180);

            // Register ONLY epoch 1 (epoch 2 still unregistered) -> wakes and advances 181!
            epoch1.register(&schemes, &committees);
            let e1_outcome = actor
                .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                .await
                .expect("epoch 1 registration wakes and applies 181");
            assert_eq!(e1_outcome, super::super::ReconcileStepOutcome::Parked);
            assert_eq!(view.read().last_finalized_number, 181);
            assert_eq!(view.read().forkchoice.finalized_block_hash, b181.digest().0);

            // Register epoch 2 -> advances 182 to Idle.
            epoch2.register(&schemes, &committees);
            let e2_outcome = actor
                .step_reconcile(&mut crate::finalization::evidence::EventWorkBudget::new())
                .await
                .expect("epoch 2 registration applies 182");
            assert_eq!(e2_outcome, super::super::ReconcileStepOutcome::Idle);
            assert_eq!(view.read().last_finalized_number, 182);
        });
    }

    #[test]
    fn missing_block_alarm_clears_after_archive_repair_and_finalization() {
        use metrics_util::debugging::DebuggingRecorder;

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        metrics::with_local_recorder(&recorder, || {
            deterministic::Runner::timed(Duration::from_secs(360)).start(|context| async move {
                let epoch0 = EpochCommitteeFixture::new(0);
                let schemes = SchemeProvider::default();
                let committees = CommitteeProvider::new();
                epoch0.register(&schemes, &committees);
                let mut marshal =
                    start_live_marshal(&context, "alarm-repair", schemes.clone()).await;
                let block = make_block(1, 1);
                let view = new_finalization_view(B256::ZERO, 0, None);
                let (actor, ingress) = FinalizationActor::new(FinalizationActorDeps {
                    view: view.clone(),
                    block_cache: crate::finalization::block_cache::BlockCache::new(),
                    marshal_mailbox: Some(marshal.mailbox.clone()),
                    bridge: None,
                    dkg_manager: crate::dkg_manager::Mailbox::new(),
                    vrf_safety: VrfSafetyGate::new(0, 0, 1_000, 100),
                    parent_cert_store: FinalizedParentCertStore::new(),
                    certificate_scheme_provider: schemes,
                    committee_provider: committees,
                    late_sig_store: late_sig_store::shared(3),
                });
                ingress
                    .notify_finalized(Finalized {
                        round: Round::new(Epoch::new(0), View::new(1)),
                        digest: block.digest(),
                        vrf_seed: None,
                        consensus_data: outbe_primitives::consensus::ConsensusData::default(),
                    })
                    .expect("send unresolved finalization");
                let actor_task = context
                    .child("alarm_repair_actor")
                    .spawn(move |ctx| actor.run(ctx));

                context
                    .sleep(crate::config::STALL_DIVERGENCE_ALARM + Duration::from_secs(1))
                    .await;
                let (_, alarms, gauge) = snapshot_alarm(&snapshotter);
                assert_eq!(alarms, 1);
                assert_eq!(gauge, 1.0);

                assert!(
                    marshal
                        .mailbox
                        .verified(Round::new(Epoch::new(0), View::new(1)), block.clone())
                        .await
                );
                let f = epoch0.sign_finalization(1, 0, block.digest());
                let _ = marshal.mailbox.report(Activity::Finalization(f));
                wait_for_marshal_finalization(&context, &marshal.mailbox, Height::new(1)).await;
                wait_for_height(&context, &view, 1, 100, Duration::from_millis(10)).await;
                for _ in 0..100 {
                    if snapshot_alarm(&snapshotter).2 == 0.0 {
                        break;
                    }
                    context.sleep(Duration::from_millis(10)).await;
                }
                let (_, alarms_after, gauge_after) = snapshot_alarm(&snapshotter);
                assert_eq!(alarms_after, 1, "repair must not raise another alarm");
                assert_eq!(gauge_after, 0.0, "resolved block must clear the alarm");
                drop(ingress);
                actor_task.await.expect("join").expect("clean shutdown");
            });
        });
    }
}
