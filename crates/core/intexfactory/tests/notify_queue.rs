//! Queue mechanics of the call notices each block puts into the outbox after the call slice:
//!
//! - the chunk bound
//! - the resume point
//! - that a drained entry is gone
//! - what each target chain gets

use alloy_primitives::{FixedBytes, U256};
use alloy_sol_types::SolValue;
use outbe_intex::SeriesId;
use outbe_intexfactory::call_notice::{pack_call_notice, send_call_notices};
use outbe_intexfactory::targets::{pack_target, TargetMode};
use outbe_intexfactory::transport::outbox::Message;
use outbe_intexfactory::transport::wire::MSG_INTEX_SERIES_CALL_NOTICE;
use outbe_intexfactory::IntexFactoryContract;
use outbe_primitives::block::{BlockContext, BlockRuntimeContext};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
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

/// The chains that get the series and their call notices.
const CHAINS: [u32; 2] = [97, 11_155_111];

/// The one-series runs a block sends: each one is a message to each chain.
const RUNS_PER_BLOCK: u32 = SWEEP_WRITES_PER_BLOCK / CHAINS.len() as u32;

/// A provider whose registry holds `CHAINS` in Series.
fn provider() -> HashMapStorageProvider {
    let mut storage = HashMapStorageProvider::new(CHAIN_ID);
    StorageHandle::enter(&mut storage, |handle| {
        let factory = IntexFactoryContract::new(handle.clone());
        for chain in CHAINS {
            factory
                .target_chains
                .push(pack_target(chain, TargetMode::Series))
                .unwrap();
        }
    });
    storage
}

fn outbox(handle: &StorageHandle<'_>, chain: u32) -> Vec<Message> {
    IntexFactoryContract::new(handle.clone())
        .outbox()
        .messages(chain)
        .unwrap()
}

/// The call time and the series of a call notice message.
fn notice_of(message: &Message) -> (u64, Vec<SeriesId>) {
    assert_eq!(message.label.msg_type, MSG_INTEX_SERIES_CALL_NOTICE);
    let (_, called_at, ids) =
        <(u32, u64, Vec<FixedBytes<14>>)>::abi_decode_params(&message.payload[2..]).unwrap();
    (called_at, ids.into_iter().map(SeriesId::from).collect())
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
    send_call_notices(&ctx).expect("a notice never fails the drain");
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
        let queued = RUNS_PER_BLOCK + 5;
        seed(&handle, queued);

        drain(&handle);
        assert_eq!(
            queue_bounds(&handle),
            (RUNS_PER_BLOCK, queued),
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
        seed(&handle, RUNS_PER_BLOCK);
        drain(&handle);
        assert_eq!(queue_bounds(&handle), (0, 0));
    });
}

#[test]
fn a_notice_goes_to_every_chain_that_is_not_off() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        seed(&handle, 1);
        drain(&handle);
        for chain in CHAINS {
            let messages = outbox(&handle, chain);
            assert_eq!(messages.len(), 1);
            assert_eq!(notice_of(&messages[0]), (CALLED_AT, vec![series(0)]));
        }
    });
}

#[test]
fn a_run_of_one_day_and_call_time_rides_one_message_up_to_the_wire_cap() {
    use outbe_intexfactory::constants::MAX_SERIES_PER_CALL_NOTICE;

    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        let day = EmissionDate::new(20_260_101);
        let factory = IntexFactoryContract::new(handle.clone());
        let width = MAX_SERIES_PER_CALL_NOTICE as u32 + 1;
        let mut run = Vec::new();
        for index in 0..width {
            let id = SeriesId::pack(day.value(), *b"USD", b'A' + index as u8).unwrap();
            record_on(&handle, id, day);
            factory
                .notify_at
                .write(&index, pack_call_notice(id, CALLED_AT))
                .unwrap();
            run.push(id);
        }
        factory.notify_tail.write(width).unwrap();

        drain(&handle);
        let messages = outbox(&handle, CHAINS[0]);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            notice_of(&messages[0]).1,
            run[..MAX_SERIES_PER_CALL_NOTICE].to_vec()
        );
        assert_eq!(
            notice_of(&messages[1]).1,
            run[MAX_SERIES_PER_CALL_NOTICE..].to_vec()
        );
    });
}

#[test]
fn a_notice_of_a_deleted_series_is_dropped() {
    let mut storage = provider();
    StorageHandle::enter(&mut storage, |handle| {
        let factory = IntexFactoryContract::new(handle.clone());
        factory
            .notify_at
            .write(&0, pack_call_notice(series(0), CALLED_AT))
            .unwrap();
        factory.notify_tail.write(1).unwrap();

        drain(&handle);
        assert_eq!(queue_bounds(&handle), (0, 0));
        assert!(outbox(&handle, CHAINS[0]).is_empty());
    });
}
