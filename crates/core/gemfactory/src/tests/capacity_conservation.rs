//! Expiry owns the remaining position load and releases it exactly once.
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_intex::SeriesId;
use outbe_primitives::{
    addresses::GEM_FACTORY_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    error::Result,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::WorldwideDay,
};
use outbe_promislimit::PromisLimitContract;

use crate::{expired, schema::GemPosition, GemFactoryContract};

const EXPIRES: u64 = 1_704_153_600;
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
                source_floor_price_minor: U256::from(1_080_000),
                issuance_currency: 840,
                reference_currency: 840,
                issued_at: EXPIRES - 86_400,
                expires_at: EXPIRES,
            })
            .unwrap();
        factory.push_live_position(U256::from(1)).unwrap();
    });
    p.clear_mutation_failure();
    p
}

fn sweep(p: &mut HashMapStorageProvider, at: u64) -> Result<u32> {
    StorageHandle::enter(p, |s| {
        expired::sweep_expired_positions(&BlockRuntimeContext::new(
            BlockContext::empty_for_tests(2, at, 1),
            s,
        ))
    })
}

fn conserved(p: &mut HashMapStorageProvider, terminal: bool) {
    StorageHandle::enter(p, |s| {
        let factory = GemFactoryContract::new(s.clone());
        let remaining = factory
            .positions
            .get(U256::from(1))
            .unwrap()
            .unwrap()
            .remaining_capacity_minor;
        let returned = PromisLimitContract::new(s).get_total_unallocated().unwrap();
        assert_eq!(remaining + returned, U256::from(REMAINDER));
        assert_eq!(
            factory.owner_of(U256::from(1)).unwrap(),
            Address::repeat_byte(1)
        );
        if terminal {
            assert_eq!(remaining, U256::ZERO);
            assert_eq!(returned, U256::from(REMAINDER));
            assert_eq!(factory.live_head.read().unwrap(), 0);
            assert_eq!(factory.live_tail.read().unwrap(), 0);
        }
    });
    let events = p
        .get_events(GEM_FACTORY_ADDRESS)
        .iter()
        .filter_map(|e| crate::precompile::IGemFactory::GemPositionExpired::decode_log_data(e).ok())
        .collect::<Vec<_>>();
    if terminal {
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].returnedCapacityMinor, U256::from(REMAINDER));
    }
}

#[test]
fn position_expiry_and_queue_prune_never_credit_the_remainder_twice() {
    let mut p = world();
    assert_eq!(sweep(&mut p, EXPIRES - 1).unwrap(), 0);
    conserved(&mut p, false);
    StorageHandle::enter(&mut p, |s| {
        assert_eq!(
            PromisLimitContract::new(s).get_total_unallocated().unwrap(),
            U256::ZERO
        );
    });
    assert_eq!(sweep(&mut p, EXPIRES).unwrap(), 1);
    conserved(&mut p, true);
    assert_eq!(sweep(&mut p, EXPIRES + 86_400).unwrap(), 0);
    conserved(&mut p, true);
}

#[test]
fn every_position_expiry_write_failure_preserves_remaining_load_then_retries_once() {
    let mut baseline = world();
    assert_eq!(sweep(&mut baseline, EXPIRES).unwrap(), 1);
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
            // A failed queue compaction errors, and the sweep retains a failed position.
            // Either way the economic checkpoint must hold.
            let _ = sweep(&mut p, EXPIRES);
            let observed = p.clear_mutation_failure();
            let reached = if after {
                observed > point
            } else {
                observed == point
            };
            assert!(reached, "unreached fault {point}, after={after}");
            conserved(&mut p, false);
            sweep(&mut p, EXPIRES + 1).unwrap();
            sweep(&mut p, EXPIRES + 86_400).unwrap();
            conserved(&mut p, true);
        }
    }
}
