//! Expiry owns the remaining load of an hour's positions and releases it exactly once.
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_intex::SeriesId;
use outbe_primitives::time::SECONDS_PER_DAY;
use outbe_primitives::{
    addresses::GEM_FACTORY_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    error::Result,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_promislimit::PromisLimitContract;

use crate::{positions_expired, schema::GemPosition, GemFactoryContract};

const EXPIRES: u64 = 1_704_153_600;
/// `EXPIRES` is midnight, so its deadline day closes the following midnight.
const DAY_CLOSED: u64 = EXPIRES + SECONDS_PER_DAY;
const REMAINDER: u64 = 3_000_007;

#[test]
fn position_queue_encoding_preserves_zero_and_maximum_supported_hours() {
    use crate::state::{position_queue_entry, position_queue_hour};
    let max_deadline =
        outbe_primitives::deadline_queue::day_end(outbe_primitives::math::constants::MAX_BIN_ID)
            - 1;
    for hour in [
        0,
        1,
        max_deadline / crate::schema::GemPosition::GROUP_HOUR_SECONDS,
    ] {
        let entry = position_queue_entry(hour).unwrap();
        assert_ne!(entry, 0);
        assert_eq!(position_queue_hour(entry).unwrap(), hour);
    }
    assert!(position_queue_entry(u64::MAX).is_err());
    assert!(position_queue_hour(0).is_err());
}

#[test]
fn daily_position_expiry_preserves_hour_topics_including_epoch_hour_zero() {
    let mut p = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut p, |s| {
        let mut factory = GemFactoryContract::new(s.clone());
        for (id, expires_at) in [(1, 1), (2, 3_601), (3, 86_399), (4, 86_400)] {
            factory
                .add_position(&GemPosition {
                    position_id: U256::from(id),
                    merchant: Address::repeat_byte(1),
                    source_intex_id: SeriesId::pack(1, *b"USD", b'U').unwrap(),
                    remaining_capacity_minor: U256::from(REMAINDER),
                    source_entry_price_minor: U256::from(1_000_000),
                    issuance_currency: 840,
                    reference_currency: 840,
                    issued_at: 0,
                    expires_at,
                })
                .unwrap();
        }
    });
    assert_eq!(sweep(&mut p, 86_399).unwrap(), 0);
    assert_eq!(sweep(&mut p, 86_400).unwrap(), 3);
    StorageHandle::enter(&mut p, |s| {
        assert_eq!(
            PromisLimitContract::new(s.clone()).promis_limit().unwrap(),
            U256::from(3 * REMAINDER)
        );
        let factory = GemFactoryContract::new(s);
        for id in 1..=3 {
            assert!(factory.positions.get(U256::from(id)).unwrap().is_none());
        }
        assert!(factory.positions.get(U256::from(4)).unwrap().is_some());
    });
    let mut hours = p
        .get_events(GEM_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| {
            crate::precompile::IGemFactory::GemPositionGroupExpired::decode_log_data(log)
                .ok()
                .map(|event| event.expiryHour)
        })
        .collect::<Vec<_>>();
    hours.sort_unstable();
    assert_eq!(hours, vec![0, 1, 23]);
    assert_eq!(sweep(&mut p, 2 * SECONDS_PER_DAY).unwrap(), 1);
}

#[test]
fn daily_pass_shares_series_and_position_allowance_across_reentry() {
    use alloy_primitives::B256;
    use outbe_gem::GemContract;
    use outbe_primitives::{pass_budget::SWEEP_WRITES_PER_BLOCK, time::timestamp_to_date_key};
    let mut p = world();
    StorageHandle::enter(&mut p, |s| {
        let genesis =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(0, EXPIRES, 1), s.clone());
        crate::called::initialize_daily(&genesis).unwrap();
        outbe_oracle::schema::OracleContract::new(s.clone())
            .utc_day_vwap_last_finalized
            .write(timestamp_to_date_key(EXPIRES))
            .unwrap();
        let mut gem = GemContract::new(s.clone());
        for n in 1..=SWEEP_WRITES_PER_BLOCK + 1 {
            let group = B256::from(U256::from(n).to_be_bytes::<32>());
            gem.group_called_at.write(&group, 1).unwrap();
            gem.group_gem_count.write(&group, 1).unwrap();
            gem.group_unpaid_load.write(&group, U256::from(1)).unwrap();
            gem.push_called(group, EXPIRES).unwrap();
        }
        let ctx =
            BlockRuntimeContext::new(BlockContext::empty_for_tests(1, DAY_CLOSED, 1), s.clone());
        crate::called::process_call_pass(&ctx).unwrap();
        assert_eq!(
            PromisLimitContract::new(s.clone()).promis_limit().unwrap(),
            U256::from(SWEEP_WRITES_PER_BLOCK)
        );
        assert_eq!(gem.daily_phase.read().unwrap(), 1);
        crate::called::process_call_pass(&ctx).unwrap();
        assert_eq!(
            PromisLimitContract::new(s.clone()).promis_limit().unwrap(),
            U256::from(SWEEP_WRITES_PER_BLOCK)
        );
        let next = BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, DAY_CLOSED + 1, 1),
            s.clone(),
        );
        crate::called::process_call_pass(&next).unwrap();
        assert_eq!(
            PromisLimitContract::new(s.clone()).promis_limit().unwrap(),
            U256::from(u64::from(SWEEP_WRITES_PER_BLOCK) + 1 + REMAINDER)
        );
        assert_eq!(gem.call_sweep_day.read().unwrap(), 0);
        assert_eq!(
            GemFactoryContract::new(s.clone())
                .hour_capacity_minor
                .read(&(EXPIRES / 3_600))
                .unwrap(),
            U256::ZERO
        );
        crate::called::process_call_pass(&next).unwrap();
        assert_eq!(
            PromisLimitContract::new(s).promis_limit().unwrap(),
            U256::from(u64::from(SWEEP_WRITES_PER_BLOCK) + 1 + REMAINDER)
        );
    });
}

fn world() -> HashMapStorageProvider {
    let mut p = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut p, |s| {
        let mut factory = GemFactoryContract::new(s);
        factory
            .add_position(&GemPosition {
                position_id: U256::from(1),
                merchant: Address::repeat_byte(1),
                source_intex_id: SeriesId::pack(20_240_101, *b"USD", b'U').unwrap(),
                remaining_capacity_minor: U256::from(REMAINDER),
                source_entry_price_minor: U256::from(1_000_000),
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: EXPIRES - SECONDS_PER_DAY,
                expires_at: EXPIRES,
            })
            .unwrap();
    });
    p.clear_mutation_failure();
    p
}

/// One block's position expiry, then its cleanup. Returns the hours expired.
fn sweep(p: &mut HashMapStorageProvider, at: u64) -> Result<u32> {
    StorageHandle::enter(p, |s| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(at, at, 1), s);
        let expired = positions_expired::sweep_expired_positions(&ctx)?;
        positions_expired::cleanup_positions(&ctx)?;
        Ok(expired)
    })
}

fn conserved(p: &mut HashMapStorageProvider, terminal: bool) {
    let hour = EXPIRES / 3_600;
    StorageHandle::enter(p, |s| {
        let factory = GemFactoryContract::new(s.clone());
        let left = factory.hour_capacity_minor.read(&hour).unwrap();
        let returned = PromisLimitContract::new(s).promis_limit().unwrap();
        assert_eq!(left + returned, U256::from(REMAINDER));
        if terminal {
            assert_eq!(returned, U256::from(REMAINDER));
            assert!(factory.positions.get(U256::from(1)).unwrap().is_none());
            assert_eq!(factory.balance_of(Address::repeat_byte(1)).unwrap(), 0);
        } else if factory.positions.exists(U256::from(1)).unwrap() {
            assert_eq!(
                factory.owner_of(U256::from(1)).unwrap(),
                Address::repeat_byte(1)
            );
        }
    });
    let events = p
        .get_events(GEM_FACTORY_ADDRESS)
        .iter()
        .filter_map(|e| {
            crate::precompile::IGemFactory::GemPositionGroupExpired::decode_log_data(e).ok()
        })
        .collect::<Vec<_>>();
    if terminal {
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].expiryHour, hour);
        assert_eq!(events[0].positions, 1);
        assert_eq!(events[0].returnedCapacityMinor, U256::from(REMAINDER));
    }
}

#[test]
fn position_expiry_and_cleanup_never_credit_the_remainder_twice() {
    let mut p = world();
    assert_eq!(sweep(&mut p, EXPIRES - 1).unwrap(), 0);
    assert_eq!(sweep(&mut p, DAY_CLOSED - 1).unwrap(), 0);
    conserved(&mut p, false);
    StorageHandle::enter(&mut p, |s| {
        assert_eq!(
            PromisLimitContract::new(s).promis_limit().unwrap(),
            U256::ZERO
        );
    });
    assert_eq!(sweep(&mut p, DAY_CLOSED).unwrap(), 1);
    conserved(&mut p, true);
    assert_eq!(sweep(&mut p, EXPIRES + SECONDS_PER_DAY).unwrap(), 0);
    conserved(&mut p, true);
}

#[test]
fn every_position_expiry_write_failure_preserves_remaining_load_then_retries_once() {
    let mut baseline = world();
    assert_eq!(sweep(&mut baseline, DAY_CLOSED).unwrap(), 1);
    let count = baseline.clear_mutation_failure();
    assert!(count > 0);
    for after in [false, true] {
        for point in 0..count {
            let mut p = world();
            if after {
                p.fail_after_mutation_at(point);
            } else {
                p.fail_mutation_at(point);
            }
            // A failed expiry keeps the hour queued, a failed cleanup keeps its position.
            // Either way the economic checkpoint must hold.
            let _ = sweep(&mut p, DAY_CLOSED);
            let observed = p.clear_mutation_failure();
            let reached = if after {
                observed > point
            } else {
                observed == point
            };
            assert!(reached, "unreached fault {point}, after={after}");
            conserved(&mut p, false);
            sweep(&mut p, DAY_CLOSED + 1).unwrap();
            sweep(&mut p, DAY_CLOSED + SECONDS_PER_DAY).unwrap();
            conserved(&mut p, true);
        }
    }
}
