//! K06: realized units remain disjoint and only the unpaid remainder returns.
use crate::{IntexFactoryContract, IntexLifecycle};
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_intex::{
    api,
    schema::{CreateSeriesParams, IntexCallTrigger},
    SeriesId,
};
use outbe_primitives::{
    addresses::INTEX_FACTORY_ADDRESS,
    block::{BlockContext, BlockLifecycle, BlockRuntimeContext},
    error::Result,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::WorldwideDay,
};
use outbe_promislimit::PromisLimitContract;

const ISSUED: u32 = 1_704_067_200;
const DAY: u64 = 86_400;
const DEADLINE: u64 = ISSUED as u64 + 7 * DAY;
const LOAD: u128 = 1_000_003;
const WWD: u32 = 20_240_101;

fn world() -> (HashMapStorageProvider, SeriesId) {
    let mut p = HashMapStorageProvider::new(1);
    p.set_timestamp(U256::from(ISSUED));
    let id = SeriesId::pack(WorldwideDay::new(WWD), *b"USD", b'U').unwrap();
    StorageHandle::enter(&mut p, |s| {
        api::create_series(
            &s,
            CreateSeriesParams {
                series_id: id,
                worldwide_day: WorldwideDay::new(WWD),
                issued_units: 10,
                promis_load_minor: LOAD,
                entry_price_minor: U256::from(1_000_000),
                floor_price_minor: U256::from(1_080_000),
                call_price_minor: U256::from(2_280_000),
                call_trigger: IntexCallTrigger {
                    call_window_seconds: 28 * DAY as u32,
                    call_threshold_seconds: 21 * DAY as u32,
                    call_notice_period_seconds: 7 * DAY as u32,
                },
                issued_at: ISSUED,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        // Completed settlement/exercise/conversion footprints, separately
        // authorized through public carriers in K01/K08 and the existing suites.
        api::record_settled_units(&s, id, 3).unwrap();
        api::record_exercised_units(&s, id, Address::repeat_byte(1), 1).unwrap();
        api::record_gem_factory_units(&s, id, Address::repeat_byte(1), 2).unwrap();
        api::mark_called(&s, id, ISSUED).unwrap();
        IntexFactoryContract::new(s.clone())
            .push_called_group(840, WorldwideDay::new(WWD), DEADLINE, &[id])
            .unwrap();
        assert_eq!(
            PromisLimitContract::new(s).get_total_unallocated().unwrap(),
            U256::ZERO
        );
    });
    p.clear_mutation_failure();
    (p, id)
}

fn sweep(p: &mut HashMapStorageProvider, at: u64) -> Result<()> {
    p.set_timestamp(U256::from(at));
    StorageHandle::enter(p, |s| {
        IntexLifecycle::begin_block(&BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, at, 1),
            s,
        ))
    })
}

fn ledger(p: &mut HashMapStorageProvider, id: SeriesId, terminal: bool) {
    StorageHandle::enter(p, |s| {
        let units = api::unit_counts(&s, id).unwrap();
        assert_eq!(
            (
                units.issued,
                units.settled,
                units.exercised,
                units.gem_factory
            ),
            (10, 2, 1, 2)
        );
        assert_eq!(
            units.active + units.settled + units.exercised + units.gem_factory + units.forfeited,
            10
        );
        let key = IntexFactoryContract::scoped(840, WWD);
        let pending = IntexFactoryContract::new(s.clone())
            .called_group_count
            .read(&key)
            .unwrap();
        let returned = PromisLimitContract::new(s).get_total_unallocated().unwrap();
        if terminal {
            assert_eq!((units.active, units.forfeited, pending), (0, 5, 0));
            assert_eq!(returned, U256::from(5 * LOAD));
        } else {
            assert!(pending <= 1);
            assert_eq!(
                returned,
                if pending == 1 {
                    U256::ZERO
                } else {
                    U256::from(5 * LOAD)
                }
            );
        }
    });
}

#[test]
fn partial_intex_expiry_returns_only_five_unpaid_units_and_never_paid_or_converted_load() {
    let (mut p, id) = world();
    sweep(&mut p, DEADLINE).unwrap();
    ledger(&mut p, id, false);
    StorageHandle::enter(&mut p, |s| {
        let units = api::unit_counts(&s, id).unwrap();
        assert_eq!((units.active, units.forfeited), (5, 0));
    });
    let due = IntexFactoryContract::bucket_end(IntexFactoryContract::deadline_bucket(DEADLINE));
    sweep(&mut p, due).unwrap();
    ledger(&mut p, id, true);
    sweep(&mut p, due + DAY).unwrap();
    ledger(&mut p, id, true);
    let events = p
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter_map(|e| crate::precompile::IIntexFactory::SeriesExpired::decode_log_data(e).ok())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].forfeitedUnits, 5);
    assert_eq!(events[0].returnedPromisMinor, U256::from(5 * LOAD));
}

#[test]
fn every_intex_expiry_write_failure_preserves_credit_debt_and_retries_exactly_once() {
    let due = IntexFactoryContract::bucket_end(IntexFactoryContract::deadline_bucket(DEADLINE));
    let (mut baseline, _) = world();
    sweep(&mut baseline, due).unwrap();
    let mutations = baseline.clear_mutation_failure();
    assert!(mutations > 0);
    for after in [false, true] {
        for point in 0..mutations {
            let (mut p, id) = world();
            if after {
                p.fail_after_mutation_at(point);
            } else {
                p.fail_mutation_at(point);
            }
            assert!(sweep(&mut p, due).is_err(), "point {point}, after={after}");
            p.clear_mutation_failure();
            ledger(&mut p, id, false);
            sweep(&mut p, due + 1).unwrap();
            sweep(&mut p, due + DAY).unwrap();
            ledger(&mut p, id, true);
            let closed = p
                .get_events(INTEX_FACTORY_ADDRESS)
                .iter()
                .filter_map(|e| {
                    crate::precompile::IIntexFactory::SeriesExpired::decode_log_data(e).ok()
                })
                .count();
            assert_eq!(closed, 1);
        }
    }
}
