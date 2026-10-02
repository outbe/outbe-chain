//! Queue mechanics of the lifecycle notices the `intex_drain_notices` trigger sends.
//!
//! The router accepts every send unless a test says otherwise, so these pin the
//! queue walk itself: the chunk bound, the resume point, that a drained entry is
//! gone, and where a refused one goes.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_intex::SeriesId;
use outbe_intexfactory::constants::{
    MAX_CALLED_NOTICE_ATTEMPTS, MAX_REFUSED_RUNS_PER_FIRING, MAX_ROUTER_CALLS_PER_FIRING,
};
use outbe_intexfactory::notify::{called_notice_attempts, drain_notices, pack_called_notice};
use outbe_intexfactory::precompile::IIntexFactory::CalledNoticeDropped;
use outbe_intexfactory::IntexFactoryContract;
use outbe_primitives::addresses::INTEX_FACTORY_ADDRESS;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::types::Storable;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::WorldwideDay;

const CHAIN_ID: u64 = 1;
const NOW: u64 = 1_700_000_000;
const CALLED_AT: u32 = NOW as u32 - 3_600;

fn series(index: u32) -> SeriesId {
    SeriesId::pack(WorldwideDay::new(20_260_101 + index), *b"USD", b'U')
        .expect("well-formed series id")
}

fn seed(handle: &StorageHandle<'_>, count: u32) {
    let factory = IntexFactoryContract::new(handle.clone());
    for index in 0..count {
        factory
            .notify_at
            .write(&index, pack_called_notice(series(index), CALLED_AT))
            .unwrap();
    }
    factory.notify_tail.write(count).unwrap();
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
    let ctx = BlockRuntimeContext::new(
        BlockContext::empty_for_tests(1, NOW, CHAIN_ID),
        handle.clone(),
    );
    drain_notices(&ctx).expect("a refused notice never fails the drain");
}

fn queue_bounds(handle: &StorageHandle<'_>) -> (u32, u32) {
    let factory = IntexFactoryContract::new(handle.clone());
    (
        factory.notify_head.read().unwrap(),
        factory.notify_tail.read().unwrap(),
    )
}

#[test]
fn a_backlog_drains_one_firing_worth_at_a_time() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        let queued = MAX_ROUTER_CALLS_PER_FIRING + 5;
        seed(&handle, queued);

        drain(&handle);
        assert_eq!(
            queue_bounds(&handle),
            (MAX_ROUTER_CALLS_PER_FIRING, queued),
            "one firing spends its budget and leaves the rest queued"
        );

        drain(&handle);
        assert_eq!(
            queue_bounds(&handle),
            (0, 0),
            "emptying the queue rewinds it so the indices cannot run away"
        );
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
fn an_exactly_full_firing_rewinds_the_queue() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, MAX_ROUTER_CALLS_PER_FIRING);
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

fn dropped_notices(storage: &HashMapStorageProvider) -> Vec<CalledNoticeDropped> {
    storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| CalledNoticeDropped::decode_log_data(log).ok())
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
            "the requeued entry outlives the firing that emptied the window"
        );
        assert_eq!(queued(&handle, 0), U256::ZERO);
        let entry = queued(&handle, 1);
        assert_eq!(called_notice_attempts(entry), 1);
        assert_eq!(SeriesId::from_word(entry), series(0));
        assert_eq!((entry & U256::from(u32::MAX)).to::<u32>(), CALLED_AT);
    });
    assert!(dropped_notices(&storage).is_empty());
}

#[test]
fn a_notice_refused_at_the_cap_is_dropped_with_an_event() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 1);
        for _ in 1..MAX_CALLED_NOTICE_ATTEMPTS {
            drain(&handle);
        }
        let (head, tail) = queue_bounds(&handle);
        assert_eq!(tail - head, 1, "still queued below the cap");
        assert_eq!(
            called_notice_attempts(queued(&handle, head)),
            MAX_CALLED_NOTICE_ATTEMPTS - 1
        );
    });
    assert!(dropped_notices(&storage).is_empty());

    StorageHandle::enter(&mut storage, |handle| {
        drain(&handle);
        assert_eq!(queue_bounds(&handle), (0, 0), "the last refusal drops it");
    });
    let dropped = dropped_notices(&storage);
    assert_eq!(dropped.len(), 1);
    assert_eq!(SeriesId::from(dropped[0].seriesId), series(0));
    assert_eq!(dropped[0].calledAt, CALLED_AT);
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
                called_notice_attempts(entry),
                1,
                "every entry got its own call in the firing"
            );
        }
    });

    accept_sends(&mut storage);
    StorageHandle::enter(&mut storage, |handle| {
        drain(&handle);
        assert_eq!(queue_bounds(&handle), (0, 0));
    });
    assert!(dropped_notices(&storage).is_empty());
}

#[test]
fn a_router_refusing_every_run_ends_the_firing_early() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let queued_runs = MAX_REFUSED_RUNS_PER_FIRING + 2;
        seed(&handle, queued_runs);
        drain(&handle);

        assert_eq!(
            queue_bounds(&handle),
            (
                MAX_REFUSED_RUNS_PER_FIRING,
                queued_runs + MAX_REFUSED_RUNS_PER_FIRING
            ),
            "the firing stops after the refusals in a row"
        );
        assert_eq!(
            called_notice_attempts(queued(&handle, MAX_REFUSED_RUNS_PER_FIRING)),
            0,
            "an entry the firing never reached keeps its count"
        );
    });
}
