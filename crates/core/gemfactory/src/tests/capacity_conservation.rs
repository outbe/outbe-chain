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
    time::WorldwideDay,
};
use outbe_promislimit::PromisLimitContract;

use crate::{positions_expired, schema::GemPosition, GemFactoryContract};

const EXPIRES: u64 = 1_704_153_600;
/// When the hour `EXPIRES` falls in has closed.
const HOUR_CLOSED: u64 = EXPIRES + 3_600;
const REMAINDER: u64 = 3_000_007;

fn world() -> HashMapStorageProvider {
    let mut p = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut p, |s| {
        let mut factory = GemFactoryContract::new(s);
        factory
            .add_position(&GemPosition {
                position_id: U256::from(1),
                merchant: Address::repeat_byte(1),
                source_intex_id: SeriesId::pack(WorldwideDay::new(20_240_101), *b"USD", b'U')
                    .unwrap(),
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
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, at, 1), s);
        let expired = positions_expired::sweep_expired_positions(&ctx)?;
        positions_expired::sweep_position_cleanup(&ctx)?;
        Ok(expired)
    })
}

fn conserved(p: &mut HashMapStorageProvider, terminal: bool) {
    let hour = EXPIRES / 3_600;
    StorageHandle::enter(p, |s| {
        let factory = GemFactoryContract::new(s.clone());
        let left = factory.hour_capacity_minor.read(&hour).unwrap();
        let returned = PromisLimitContract::new(s).get_total_unallocated().unwrap();
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
    assert_eq!(sweep(&mut p, HOUR_CLOSED - 1).unwrap(), 0);
    conserved(&mut p, false);
    StorageHandle::enter(&mut p, |s| {
        assert_eq!(
            PromisLimitContract::new(s).get_total_unallocated().unwrap(),
            U256::ZERO
        );
    });
    assert_eq!(sweep(&mut p, HOUR_CLOSED).unwrap(), 1);
    conserved(&mut p, true);
    assert_eq!(sweep(&mut p, EXPIRES + SECONDS_PER_DAY).unwrap(), 0);
    conserved(&mut p, true);
}

#[test]
fn every_position_expiry_write_failure_preserves_remaining_load_then_retries_once() {
    let mut baseline = world();
    assert_eq!(sweep(&mut baseline, HOUR_CLOSED).unwrap(), 1);
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
            let _ = sweep(&mut p, HOUR_CLOSED);
            let observed = p.clear_mutation_failure();
            let reached = if after {
                observed > point
            } else {
                observed == point
            };
            assert!(reached, "unreached fault {point}, after={after}");
            conserved(&mut p, false);
            sweep(&mut p, HOUR_CLOSED + 1).unwrap();
            sweep(&mut p, EXPIRES + SECONDS_PER_DAY).unwrap();
            conserved(&mut p, true);
        }
    }
}
