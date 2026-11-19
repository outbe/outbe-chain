//! Queue mechanics of the call notices each block sends after the call slice.
//!
//! The router accepts every send unless a test says otherwise. So these tests pin
//! the queue walk itself:
//!
//! - the chunk bound
//! - the resume point
//! - that a drained entry is gone
//! - where a refused one goes

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_intex::SeriesId;
use outbe_intexfactory::constants::{MAX_REFUSED_RUNS_PER_BLOCK, NOTICE_RETRY_SECONDS};
use outbe_intexfactory::notify::{
    call_notice_attempts, notice_retry_at, pack_call_notice, send_call_notices, with_retry_at,
    KeptLeg,
};
use outbe_intexfactory::precompile::IIntexFactory::IntexSeriesCallNoticeSendFailed;
use outbe_intexfactory::IntexFactoryContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::INTEX_FACTORY_ADDRESS;
use outbe_system_common::pass_budget::SWEEP_WRITES_PER_BLOCK;
use outbe_system_common::time::EmissionDate;

const CHAIN_ID: u64 = 1;
const NOW: u64 = 1_700_000_000;
const CALLED_AT: u64 = NOW - 3_600;

fn series(index: u32) -> SeriesId {
    SeriesId::pack(20_260_101 + index, *b"USD", b'U').expect("well-formed series id")
}

fn seed(handle: &StorageHandle<'_>, count: u32) {
    let factory = IntexFactoryContract::new(handle.clone());
    for index in 0..count {
        record(handle, series(index));
        factory
            .notify_at
            .write(&index, pack_call_notice(series(index), CALLED_AT))
            .unwrap();
    }
    factory.notify_tail.write(count).unwrap();
}

/// A notice goes out only while its series exists. Its auction ran on the day the id names.
fn record(handle: &StorageHandle<'_>, series_id: SeriesId) {
    record_on(handle, series_id, EmissionDate::new(series_id.day()));
}

fn record_on(handle: &StorageHandle<'_>, series_id: SeriesId, emission_day: EmissionDate) {
    if outbe_intex::api::series_exists(handle, series_id).unwrap() {
        return;
    }
    outbe_intex::api::create_series(
        handle,
        outbe_intex::CreateSeriesParams {
            series_id,
            emission_day,
            issued_units: 1,
            promis_load_minor: 1,
            entry_price_minor: U256::from(1u64),
            call_price_minor: U256::from(2u64),
            call_trigger: outbe_intex::IntexCallTrigger {
                call_window_seconds: 1,
                call_threshold_seconds: 1,
                call_notice_period_seconds: 1,
                transit_margin_seconds: 0,
            },
            issued_at: CALLED_AT,
            issuance_currency: 840,
            reference_currency: 840,
        },
    )
    .unwrap();
}

/// A provider whose OriginRouter accepts sends.
fn provider() -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    accept_sends(&mut storage);
    storage
}

fn accept_sends(storage: &mut HashMapStorageProvider) {
    storage.stub_sub_call_at(
        outbe_intexfactory::constants::ORIGIN_ROUTER_ADDRESS,
        alloy_primitives::Bytes::from(vec![0u8; 32]),
    );
}

fn drain(handle: &StorageHandle<'_>) {
    drain_at(handle, NOW);
}

fn drain_at(handle: &StorageHandle<'_>, now: u64) {
    let ctx = BlockRuntimeContext::new(
        BlockContext {
            block_gas_limit: 0,
            user_gas_used: 0,
            block_number: 1,
            timestamp: now,
            chain_id: CHAIN_ID,
            genesis_hash: alloy_primitives::B256::ZERO,
            genesis_timestamp: u64::max(now, 1),
            proposer: alloy_primitives::Address::ZERO,
            validators: Vec::new(),
        },
        handle.clone(),
    );
    send_call_notices(&ctx).expect("a refused notice never fails the send");
}

fn queue_bounds(handle: &StorageHandle<'_>) -> (u32, u32) {
    let factory = IntexFactoryContract::new(handle.clone());
    (
        factory.notify_head.read().unwrap(),
        factory.notify_tail.read().unwrap(),
    )
}

#[test]
fn a_backlog_drains_one_block_worth_at_a_time() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        let queued = SWEEP_WRITES_PER_BLOCK + 5;
        seed(&handle, queued);

        drain(&handle);
        assert_eq!(
            queue_bounds(&handle),
            (SWEEP_WRITES_PER_BLOCK, queued),
            "one block spends its budget and leaves the rest queued"
        );

        drain(&handle);
        assert_eq!(
            queue_bounds(&handle),
            (0, 0),
            "emptying the queue rewinds it so the indices cannot run away"
        );
    });
}

/// A paused entry of the same day and call time does not ride along with a due one.
#[test]
fn a_paused_entry_waits_out_its_pause_behind_a_due_one() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        let day = EmissionDate::new(20_260_101);
        let due = SeriesId::pack(day.value(), *b"USD", b'U').unwrap();
        let paused = SeriesId::pack(day.value(), *b"EUR", b'U').unwrap();
        record(&handle, due);
        record(&handle, paused);
        let factory = IntexFactoryContract::new(handle.clone());
        factory
            .notify_at
            .write(&0, pack_call_notice(due, CALLED_AT))
            .unwrap();
        factory
            .notify_at
            .write(
                &1,
                with_retry_at(pack_call_notice(paused, CALLED_AT), NOW + 1),
            )
            .unwrap();
        factory.notify_tail.write(2).unwrap();

        drain(&handle);
        assert_eq!(queue_bounds(&handle), (1, 2));
        drain_at(&handle, NOW + 1);
        assert_eq!(queue_bounds(&handle), (0, 0));
    });
}

/// A fresh notice behind a paused one waits no longer than the pause.
#[test]
fn a_fresh_entry_behind_a_paused_head_waits_one_pause_at_most() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        let factory = IntexFactoryContract::new(handle.clone());
        record(&handle, series(0));
        record(&handle, series(1));
        let paused = with_retry_at(
            pack_call_notice(series(0), CALLED_AT),
            NOW + NOTICE_RETRY_SECONDS,
        );
        factory.notify_at.write(&0, paused).unwrap();
        factory
            .notify_at
            .write(&1, pack_call_notice(series(1), CALLED_AT))
            .unwrap();
        factory.notify_tail.write(2).unwrap();

        drain(&handle);
        assert_eq!(queue_bounds(&handle), (0, 2));
        drain_at(&handle, NOW + NOTICE_RETRY_SECONDS);
        assert_eq!(queue_bounds(&handle), (0, 0));
    });
}

#[test]
fn a_drained_entry_is_gone() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 3);
        drain(&handle);

        let factory = IntexFactoryContract::new(handle.clone());
        for index in 0..3 {
            assert_eq!(
                factory.notify_at.read(&index).unwrap(),
                U256::ZERO,
                "a sent notice must not be sent twice"
            );
        }
    });
}

#[test]
fn an_empty_queue_is_a_noop() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        drain(&handle);
        assert_eq!(queue_bounds(&handle), (0, 0));
    });
}

#[test]
fn an_exactly_full_block_rewinds_the_queue() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, SWEEP_WRITES_PER_BLOCK);
        drain(&handle);
        assert_eq!(queue_bounds(&handle), (0, 0));
    });
}

fn queued(handle: &StorageHandle<'_>, index: u32) -> U256 {
    IntexFactoryContract::new(handle.clone())
        .notify_at
        .read(&index)
        .unwrap()
}

fn failed_notices(storage: &HashMapStorageProvider) -> Vec<IntexSeriesCallNoticeSendFailed> {
    storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IntexSeriesCallNoticeSendFailed::decode_log_data(log).ok())
        .collect()
}

#[test]
fn a_refused_notice_is_requeued_with_one_more_attempt() {
    // No OriginRouter: every send is refused.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 1);
        drain(&handle);

        assert_eq!(
            queue_bounds(&handle),
            (1, 2),
            "the requeued entry outlives the block that emptied the window"
        );
        assert_eq!(queued(&handle, 0), U256::ZERO);
        let entry = queued(&handle, 1);
        assert_eq!(call_notice_attempts(entry), 1);
        assert_eq!(notice_retry_at(entry), NOW + NOTICE_RETRY_SECONDS);
        assert_eq!(SeriesId::from_word(entry), series(0));
        assert_eq!((entry & U256::from(u64::MAX)).to::<u64>(), CALLED_AT);
    });
    assert!(failed_notices(&storage).is_empty());
}

#[test]
fn a_refused_notice_waits_its_pause_before_the_next_attempt() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 1);
        drain(&handle);
    });
    accept_sends(&mut storage);
    StorageHandle::enter(&mut storage, |handle| {
        drain_at(&handle, NOW + NOTICE_RETRY_SECONDS - 1);
        assert_eq!(
            queue_bounds(&handle),
            (1, 2),
            "not due yet, so still queued"
        );
        drain_at(&handle, NOW + NOTICE_RETRY_SECONDS);
        assert_eq!(queue_bounds(&handle), (0, 0));
    });
    assert!(failed_notices(&storage).is_empty());
}

#[test]
fn a_notice_refused_at_the_cap_is_dropped_with_an_event() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 1);
        for attempt in 0..7 {
            drain_at(&handle, NOW + attempt * NOTICE_RETRY_SECONDS);
        }
        let (head, tail) = queue_bounds(&handle);
        assert_eq!(tail - head, 1, "still queued below the cap");
        assert_eq!(call_notice_attempts(queued(&handle, head)), 7);
    });
    assert!(failed_notices(&storage).is_empty());

    StorageHandle::enter(&mut storage, |handle| {
        drain_at(&handle, NOW + 7 * NOTICE_RETRY_SECONDS);
        assert_eq!(queue_bounds(&handle), (0, 0), "the eighth refusal drops it");
    });
    let dropped = failed_notices(&storage);
    assert_eq!(dropped.len(), 1);
    assert_eq!(SeriesId::from(dropped[0].seriesId), series(0));
    assert_eq!(
        dropped[0].dstChainId, 0,
        "no router names the chains of the day"
    );
}

#[test]
fn a_notice_accepted_at_the_last_send_is_not_dropped() {
    // No OriginRouter: the router refuses the first 7 sends.
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 1);
        for attempt in 0..7 {
            drain_at(&handle, NOW + attempt * NOTICE_RETRY_SECONDS);
        }
        let (head, tail) = queue_bounds(&handle);
        assert_eq!(tail - head, 1, "still queued below the cap");
        assert_eq!(call_notice_attempts(queued(&handle, head)), 7);
    });

    accept_sends(&mut storage);
    StorageHandle::enter(&mut storage, |handle| {
        drain_at(&handle, NOW + 7 * NOTICE_RETRY_SECONDS);
        assert_eq!(queue_bounds(&handle), (0, 0), "the last send lands");
    });
    assert!(failed_notices(&storage).is_empty());
}

#[test]
fn a_refused_notice_does_not_hold_back_the_rest() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 2);
        drain(&handle);

        assert_eq!(queue_bounds(&handle), (2, 4));
        for (slot, index) in [(2, 0), (3, 1)] {
            let entry = queued(&handle, slot);
            assert_eq!(SeriesId::from_word(entry), series(index));
            assert_eq!(
                call_notice_attempts(entry),
                1,
                "every entry got its own call in the block"
            );
        }
    });

    accept_sends(&mut storage);
    StorageHandle::enter(&mut storage, |handle| {
        drain_at(&handle, NOW + NOTICE_RETRY_SECONDS);
        assert_eq!(queue_bounds(&handle), (0, 0));
    });
    assert!(failed_notices(&storage).is_empty());
}

fn same_day_series(index: u8) -> SeriesId {
    SeriesId::pack(20_260_101, [b'0', b'0', b'0' + index], b'U').expect("well-formed series id")
}

#[test]
fn every_entry_of_a_refused_run_is_requeued() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let factory = IntexFactoryContract::new(handle.clone());
        for index in 0..3u8 {
            record(&handle, same_day_series(index));
            factory
                .notify_at
                .write(
                    &u32::from(index),
                    pack_call_notice(same_day_series(index), CALLED_AT),
                )
                .unwrap();
        }
        factory.notify_tail.write(3).unwrap();
        drain(&handle);

        assert_eq!(queue_bounds(&handle), (3, 6), "one run, refused whole");
        for index in 0..3u8 {
            let entry = queued(&handle, 3 + u32::from(index));
            assert_eq!(SeriesId::from_word(entry), same_day_series(index));
            assert_eq!(call_notice_attempts(entry), 1);
        }
    });

    accept_sends(&mut storage);
    StorageHandle::enter(&mut storage, |handle| {
        drain_at(&handle, NOW + NOTICE_RETRY_SECONDS);
        assert_eq!(
            queue_bounds(&handle),
            (0, 0),
            "the requeued run goes out together"
        );
    });
    assert!(failed_notices(&storage).is_empty());
}

fn queue_raw(handle: &StorageHandle<'_>, ids: &[SeriesId]) {
    let factory = IntexFactoryContract::new(handle.clone());
    for (slot, id) in (0u32..).zip(ids) {
        factory
            .notify_at
            .write(&slot, pack_call_notice(*id, CALLED_AT))
            .unwrap();
    }
    factory.notify_tail.write(ids.len() as u32).unwrap();
}

#[test]
fn a_notice_of_a_deleted_series_is_dropped_unsent() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        queue_raw(&handle, &[series(0)]);
        drain(&handle);
        assert_eq!(
            queue_bounds(&handle),
            (0, 0),
            "a refusing router would have requeued it"
        );
    });
    assert!(failed_notices(&storage).is_empty());
}

/// Four refused runs end the block after three; one run is refused whole.
#[test]
fn the_series_record_day_cuts_the_runs() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let ids: Vec<SeriesId> = (0..4u8).map(same_day_series).collect();
        for (offset, id) in (0u32..).zip(&ids) {
            record_on(&handle, *id, EmissionDate::new(20_260_201 + offset));
        }
        queue_raw(&handle, &ids);
        drain(&handle);
        assert_eq!(
            queue_bounds(&handle),
            (MAX_REFUSED_RUNS_PER_BLOCK, 4 + MAX_REFUSED_RUNS_PER_BLOCK),
            "one id date, four auction days: four runs"
        );
    });

    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let ids: Vec<SeriesId> = (0..4).map(series).collect();
        for id in &ids {
            record_on(&handle, *id, EmissionDate::new(20_260_201));
        }
        queue_raw(&handle, &ids);
        drain(&handle);
        assert_eq!(
            queue_bounds(&handle),
            (4, 8),
            "four id dates, one auction day: one run"
        );
    });
}

#[test]
fn a_router_refusing_every_run_ends_the_block_early() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let queued_runs = MAX_REFUSED_RUNS_PER_BLOCK + 2;
        seed(&handle, queued_runs);
        drain(&handle);

        assert_eq!(
            queue_bounds(&handle),
            (
                MAX_REFUSED_RUNS_PER_BLOCK,
                queued_runs + MAX_REFUSED_RUNS_PER_BLOCK
            ),
            "the block stops after the refusals in a row"
        );
        assert_eq!(
            call_notice_attempts(queued(&handle, MAX_REFUSED_RUNS_PER_BLOCK)),
            0,
            "an entry the block never reached keeps its count"
        );
    });
}

mod router {
    alloy_sol_types::sol!("../../../contracts/intex/src/origin/interfaces/IOriginRouter.sol");
}
use alloy_sol_types::{SolCall, SolError, SolValue};
use router::IOriginRouter;

const KEPT_CHAIN: u32 = 97;
const KEPT_IDX: u64 = 5;
const ROUTER: alloy_primitives::Address = outbe_intexfactory::constants::ORIGIN_ROUTER_ADDRESS;

/// A router that sends a notice to every chain but [`KEPT_CHAIN`], keeps that leg at
/// [`KEPT_IDX`] and answers its reads. `retryMessageSend` gets no answer, so every retry fails.
fn keep_one_leg(series_ids: &[SeriesId]) -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    stub_kept_leg(&mut storage, series_ids);
    storage
}

fn stub_kept_leg(storage: &mut HashMapStorageProvider, series_ids: &[SeriesId]) {
    storage.stub_sub_call_at_selector(
        ROUTER,
        IOriginRouter::sendIntexSeriesCallNoticeCall::SELECTOR,
        IOriginRouter::sendIntexSeriesCallNoticeCall::abi_encode_returns(&vec![
            IOriginRouter::FailedCallNoticeLeg {
                dstChainId: KEPT_CHAIN,
                idx: U256::from(KEPT_IDX),
            },
        ])
        .into(),
    );
    let ids: Vec<alloy_primitives::FixedBytes<14>> =
        series_ids.iter().map(|id| (*id).into()).collect();
    let mut payload = vec![1u8, 0u8];
    payload.extend((20_260_101u32, CALLED_AT, ids).abi_encode_params());
    storage.stub_sub_call_at_selector(
        ROUTER,
        IOriginRouter::getFailedMessageSendCall::SELECTOR,
        IOriginRouter::getFailedMessageSendCall::abi_encode_returns(
            &IOriginRouter::FailedMessageSend {
                dstChainId: KEPT_CHAIN,
                gasLimit: 0,
                sent: false,
                payload: payload.into(),
            },
        )
        .into(),
    );
}

fn accept_retries(storage: &mut HashMapStorageProvider) {
    storage.stub_sub_call_at_selector(
        ROUTER,
        IOriginRouter::retryMessageSendCall::SELECTOR,
        alloy_primitives::Bytes::new(),
    );
}

fn router_calls(storage: &HashMapStorageProvider, selector: [u8; 4]) -> usize {
    storage
        .sub_call_inputs()
        .iter()
        .filter(|input| input.target == ROUTER && input.calldata.starts_with(&selector))
        .count()
}

fn retries(storage: &HashMapStorageProvider) -> usize {
    router_calls(storage, IOriginRouter::retryMessageSendCall::SELECTOR)
}

fn kept_legs(handle: &StorageHandle<'_>) -> Vec<KeptLeg> {
    let factory = IntexFactoryContract::new(handle.clone());
    let (head, tail) = (
        factory.leg_head.read().unwrap(),
        factory.leg_tail.read().unwrap(),
    );
    (head..tail)
        .map(|index| KeptLeg::unpack(factory.leg_at.read(&index).unwrap()))
        .collect()
}

/// Seeds one notice, sends it, and fails `tries - 1` retries 300 seconds apart.
fn fail_tries(storage: &mut HashMapStorageProvider, tries: u64) {
    StorageHandle::enter(storage, |handle| {
        seed(&handle, 1);
        for attempt in 0..tries {
            drain_at(&handle, NOW + attempt * NOTICE_RETRY_SECONDS);
        }
    });
}

#[test]
fn call_notice_is_sent_again_for_each_failed_chain() {
    let mut storage = keep_one_leg(&[series(0)]);
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 1);
        drain(&handle);
        assert_eq!(
            queue_bounds(&handle),
            (0, 0),
            "the other chains took the notice"
        );
        assert_eq!(
            kept_legs(&handle),
            vec![KeptLeg {
                idx: KEPT_IDX,
                dst_chain_id: KEPT_CHAIN,
                attempts: 1,
                retry_at: NOW + NOTICE_RETRY_SECONDS,
            }]
        );
        drain_at(&handle, NOW + NOTICE_RETRY_SECONDS - 1);
    });
    assert_eq!(retries(&storage), 0, "the leg waits out its pause");

    StorageHandle::enter(&mut storage, |handle| {
        drain_at(&handle, NOW + NOTICE_RETRY_SECONDS)
    });
    let retried: Vec<_> = storage
        .sub_call_inputs()
        .iter()
        .filter(|input| {
            input
                .calldata
                .starts_with(&IOriginRouter::retryMessageSendCall::SELECTOR)
        })
        .map(|input| {
            IOriginRouter::retryMessageSendCall::abi_decode(&input.calldata)
                .unwrap()
                .idx
        })
        .collect();
    assert_eq!(
        retried,
        vec![U256::from(KEPT_IDX)],
        "only the kept leg goes again"
    );
    assert_eq!(
        router_calls(
            &storage,
            IOriginRouter::sendIntexSeriesCallNoticeCall::SELECTOR
        ),
        1,
        "the chains that took the notice get no second send"
    );
    StorageHandle::enter(&mut storage, |handle| {
        assert_eq!(kept_legs(&handle)[0].attempts, 2);
    });
    assert!(failed_notices(&storage).is_empty());
}

#[test]
fn a_leg_that_lands_at_the_eighth_try_emits_no_failure() {
    let mut storage = keep_one_leg(&[series(0)]);
    fail_tries(&mut storage, 7);
    accept_retries(&mut storage);
    StorageHandle::enter(&mut storage, |handle| {
        drain_at(&handle, NOW + 7 * NOTICE_RETRY_SECONDS);
        assert!(kept_legs(&handle).is_empty());
    });
    assert_eq!(retries(&storage), 7);
    assert!(failed_notices(&storage).is_empty());
}

#[test]
fn a_leg_failed_eight_times_emits_one_failure_and_stays_kept() {
    let mut storage = keep_one_leg(&[series(0)]);
    fail_tries(&mut storage, 8);
    StorageHandle::enter(&mut storage, |handle| {
        assert!(
            kept_legs(&handle).is_empty(),
            "the factory sends it no more"
        );
        drain_at(&handle, NOW + 20 * NOTICE_RETRY_SECONDS);
    });
    assert_eq!(retries(&storage), 7, "the first send is try 1");
    let failed = failed_notices(&storage);
    assert_eq!(failed.len(), 1);
    assert_eq!(SeriesId::from(failed[0].seriesId), series(0));
    assert_eq!(failed[0].dstChainId, KEPT_CHAIN);
}

#[test]
fn a_leg_another_account_sent_counts_as_delivered() {
    let mut storage = keep_one_leg(&[series(0)]);
    fail_tries(&mut storage, 1);
    storage.stub_sub_call_revert_at(
        ROUTER,
        IOriginRouter::NoFailedMessageSend {
            idx: U256::from(KEPT_IDX),
        }
        .abi_encode()
        .into(),
    );
    StorageHandle::enter(&mut storage, |handle| {
        drain_at(&handle, NOW + NOTICE_RETRY_SECONDS);
        assert!(kept_legs(&handle).is_empty());
    });
    assert_eq!(retries(&storage), 1);
    assert!(failed_notices(&storage).is_empty());
}

#[test]
fn a_day_with_no_target_chain_drops_its_notice() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    storage.stub_sub_call_revert_at(ROUTER, IOriginRouter::NoTargets {}.abi_encode().into());
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 1);
        drain(&handle);
        assert_eq!(queue_bounds(&handle), (0, 0), "nobody to tell, so no retry");
        assert!(kept_legs(&handle).is_empty());
    });
    assert!(failed_notices(&storage).is_empty());
}

/// Seven whole refusals and a kept leg at the eighth send are eight tries of that chain.
#[test]
fn a_leg_kept_at_the_last_try_fails_at_once() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    fail_tries(&mut storage, 7);
    stub_kept_leg(&mut storage, &[series(0)]);
    StorageHandle::enter(&mut storage, |handle| {
        drain_at(&handle, NOW + 7 * NOTICE_RETRY_SECONDS);
        assert_eq!(queue_bounds(&handle), (0, 0));
        assert!(kept_legs(&handle).is_empty(), "no ninth try");
        drain_at(&handle, NOW + 8 * NOTICE_RETRY_SECONDS);
    });
    assert_eq!(retries(&storage), 0);
    let failed = failed_notices(&storage);
    assert_eq!(failed.len(), 1);
    assert_eq!(SeriesId::from(failed[0].seriesId), series(0));
    assert_eq!(failed[0].dstChainId, KEPT_CHAIN);
}
