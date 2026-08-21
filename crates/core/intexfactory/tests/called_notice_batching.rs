//! Coalescing of Called notices in the `intex_notify` drain.
//!
//! A cleared slot reads back as a valid Qualified notice, so these pin the run's boundaries: an
//! overshoot past the chunk limit would drop or re-send entries with nothing in the log to show.

use alloy_primitives::U256;
use outbe_common::WorldwideDay;
use outbe_intex::SeriesId;
use outbe_intexfactory::constants::NOTIFY_CHUNK_LIMIT;
use outbe_intexfactory::qualified::{drain_notices, pack_called_notice, NOTICE_CALLED, NOTICE_QUALIFIED};
use outbe_intexfactory::IntexFactoryContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

const CHAIN_ID: u64 = 1;
const NOW: u64 = 1_700_000_000;
const DAY: u32 = 20_260_101;
const CALLED_AT: u32 = 1_777_000_000;

fn series(index: u32) -> SeriesId {
    let iso = [b'0' + (index / 100) as u8, b'0' + ((index / 10) % 10) as u8, b'0' + (index % 10) as u8];
    SeriesId::pack(WorldwideDay::new(DAY), iso, b'U').expect("well-formed series id")
}

fn push(handle: &StorageHandle<'_>, kind: u8, entry: U256) {
    let factory = IntexFactoryContract::new(handle.clone());
    let tail = factory.notify_tail.read().unwrap();
    factory.notify_at.write(&tail, entry).unwrap();
    factory.notify_kind.write(&tail, kind).unwrap();
    factory.notify_tail.write(tail + 1).unwrap();
}

fn push_called(handle: &StorageHandle<'_>, index: u32, called_at: u32) {
    push(handle, NOTICE_CALLED, pack_called_notice(series(index), called_at));
}

fn drain(handle: &StorageHandle<'_>) {
    let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, NOW, CHAIN_ID), handle.clone());
    drain_notices(&ctx).expect("a dropped notice never fails the drain");
}

fn bounds(handle: &StorageHandle<'_>) -> (u32, u32) {
    let factory = IntexFactoryContract::new(handle.clone());
    (factory.notify_head.read().unwrap(), factory.notify_tail.read().unwrap())
}

#[test]
fn a_run_longer_than_the_wire_cap_still_empties() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        for index in 0..20 {
            push_called(&handle, index, CALLED_AT);
        }
        drain(&handle);
        assert_eq!(bounds(&handle), (0, 0), "the whole run fits one firing and rewinds the queue");
    });
}

#[test]
fn a_run_never_reaches_past_the_chunk_limit() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let queued = NOTIFY_CHUNK_LIMIT + 5;
        for index in 0..queued {
            push_called(&handle, index, CALLED_AT);
        }
        drain(&handle);
        assert_eq!(
            bounds(&handle),
            (NOTIFY_CHUNK_LIMIT, queued),
            "head lands on the first unconsumed entry, not past it"
        );

        drain(&handle);
        assert_eq!(bounds(&handle), (0, 0), "the remainder goes next firing");
    });
}

#[test]
fn a_qualified_entry_ends_the_run() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        push_called(&handle, 0, CALLED_AT);
        push(&handle, NOTICE_QUALIFIED, U256::from(1u64));
        push_called(&handle, 1, CALLED_AT);

        drain(&handle);

        let factory = IntexFactoryContract::new(handle.clone());
        for index in 0..3u32 {
            assert_eq!(factory.notify_at.read(&index).unwrap(), U256::ZERO, "every entry consumed once");
        }
        assert_eq!(bounds(&handle), (0, 0));
    });
}

#[test]
fn a_different_call_time_ends_the_run() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        push_called(&handle, 0, CALLED_AT);
        push_called(&handle, 1, CALLED_AT + 1);

        drain(&handle);

        assert_eq!(bounds(&handle), (0, 0), "both groups leave, each with its own stamp");
    });
}

#[test]
fn a_run_that_hits_the_chunk_limit_is_split_not_overrun() {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        // A qualified entry at 27 leaves the next Called run starting at 28, so it would reach
        // 35 if nothing stopped it — four entries past this firing's limit of 32.
        for index in 0..27 {
            push_called(&handle, index, CALLED_AT);
        }
        push(&handle, NOTICE_QUALIFIED, U256::from(1u64));
        for index in 28..40 {
            push_called(&handle, index, CALLED_AT);
        }

        drain(&handle);
        assert_eq!(bounds(&handle), (NOTIFY_CHUNK_LIMIT, 40), "the run stops at the limit");

        let factory = IntexFactoryContract::new(handle.clone());
        for index in NOTIFY_CHUNK_LIMIT..40 {
            assert_ne!(
                factory.notify_at.read(&index).unwrap(),
                U256::ZERO,
                "entries past the limit are untouched, not cleared behind the drain's back"
            );
        }

        drain(&handle);
        assert_eq!(bounds(&handle), (0, 0), "the tail of the run leaves next firing");
    });
}
