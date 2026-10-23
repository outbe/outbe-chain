//! Realized units remain disjoint and only the unpaid remainder returns.
use super::SeedCalledGroup;
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_intex::IntexContract;
use outbe_intex::{
    api,
    schema::{CreateSeriesParams, IntexCallTrigger},
    SeriesId,
};
use outbe_primitives::time::SECONDS_PER_DAY_U32;
use outbe_primitives::{
    addresses::INTEX_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    error::Result,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::WorldwideDay,
};
use outbe_promislimit::PromisLimitContract;

const ISSUED: u32 = 1_704_067_200;
use outbe_primitives::time::SECONDS_PER_DAY as DAY;
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
                call_price_minor: U256::from(2_280_000),
                call_trigger: IntexCallTrigger {
                    call_window_seconds: 28 * SECONDS_PER_DAY_U32,
                    call_threshold_seconds: 21 * SECONDS_PER_DAY_U32,
                    call_notice_period_seconds: 7 * SECONDS_PER_DAY_U32,
                },
                issued_at: ISSUED,
                issuance_currency: 840,
                reference_currency: 840,
            },
        )
        .unwrap();
        // Settled, exercised and converted footprints. The settlement and mining
        // tests cover their authorization.
        api::record_settled_units(&s, id, 3).unwrap();
        api::record_exercised_units(&s, id, Address::repeat_byte(1), 1).unwrap();
        api::record_gem_factory_units(&s, id, Address::repeat_byte(1), 2).unwrap();
        IntexContract::new(s.clone())
            .seed_called_group(id, DEADLINE)
            .unwrap();
        assert_eq!(
            PromisLimitContract::new(s).promis_limit().unwrap(),
            U256::ZERO
        );
    });
    p.clear_mutation_failure();
    (p, id)
}

fn sweep(p: &mut HashMapStorageProvider, at: u64) -> Result<()> {
    p.set_timestamp(U256::from(at));
    StorageHandle::enter(p, |s| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, at, 1), s);
        crate::hooks::sweep_forfeits(&ctx)?;
        crate::called::run_call_slice(&ctx)?;
        crate::notify::send_notices(&ctx)
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
        let factory = IntexContract::new(s.clone());
        let key = api::read_series(&s, id)
            .unwrap()
            .group_key()
            .expect("date step");
        let pending = u32::from(factory.called_group_slot.read(&key).unwrap() != 0);
        let returned = PromisLimitContract::new(s).promis_limit().unwrap();
        if terminal {
            assert_eq!((units.active, units.forfeited, pending), (0, 5, 0));
            assert_eq!(returned, U256::from(5 * LOAD));
            assert_eq!(factory.expiry_tree_root.read().unwrap(), U256::ZERO);
            assert!(factory.first_cleanup_hour().unwrap().is_some());
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
    let due = IntexContract::bucket_end(IntexContract::deadline_bucket(DEADLINE));
    sweep(&mut p, due).unwrap();
    ledger(&mut p, id, true);
    sweep(&mut p, due + DAY).unwrap();
    ledger(&mut p, id, true);
    let events = p
        .get_events(INTEX_ADDRESS)
        .iter()
        .filter_map(|e| outbe_intex::precompile::IIntex::IntexGroupExpired::decode_log_data(e).ok())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].forfeitedUnits, 5);
    assert_eq!(events[0].returnedPromisMinor, U256::from(5 * LOAD));
}

#[test]
fn every_intex_expiry_write_failure_preserves_credit_debt_and_retries_exactly_once() {
    let due = IntexContract::bucket_end(IntexContract::deadline_bucket(DEADLINE));
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
                .get_events(INTEX_ADDRESS)
                .iter()
                .filter_map(|e| {
                    outbe_intex::precompile::IIntex::IntexGroupExpired::decode_log_data(e).ok()
                })
                .count();
            assert_eq!(closed, 1);
        }
    }
}
