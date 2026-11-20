//! Expiry owns the remaining load of a day's positions and releases it exactly once.
use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_intex::SeriesId;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    error::Result,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
};
use outbe_promislimit::PromisLimitContract;
use outbe_system_common::addresses::{GEM_FACTORY_ADDRESS, PROMIS_LIMIT_ADDRESS};
use outbe_system_common::time::SECONDS_PER_DAY;

use crate::precompile::IGemFactory::GemFactoryPositionExpired;
use crate::{position_expiry, schema::GemFactoryPosition, GemFactoryContract};

const EXPIRES: u64 = 1_704_153_600;
/// `EXPIRES` is midnight, so its deadline day closes the following midnight.
const DAY_CLOSED: u64 = EXPIRES + SECONDS_PER_DAY;
const REMAINDER: u64 = 3_000_007;

#[test]
fn position_queue_encoding_keeps_day_zero_and_the_last_supported_day() {
    use crate::state::{position_queue_day, position_queue_entry};
    for day in [0, 1, outbe_system_common::math::constants::MAX_BIN_ID] {
        let entry = position_queue_entry(day);
        assert_ne!(entry, 0);
        assert_eq!(position_queue_day(entry).unwrap(), day);
    }
    assert!(position_queue_day(0).is_err());
}

#[test]
fn positions_of_one_day_return_their_capacity_in_one_credit() {
    let day = EXPIRES;
    let mut p = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut p, |s| {
        let mut factory = GemFactoryContract::new(s.clone());
        for (id, expires_at) in [
            (1, day + 1),
            (2, day + 3_601),
            (3, day + 86_399),
            (4, day + 86_400),
        ] {
            factory
                .add_position(&GemFactoryPosition {
                    gem_factory_position_id: U256::from(id),
                    owner: Address::repeat_byte(1),
                    source_series_id: SeriesId::pack(20_240_101, *b"USD", b'U').unwrap(),
                    remaining_capacity_minor: U256::from(REMAINDER),
                    issued_at: day - SECONDS_PER_DAY,
                    expires_at,
                })
                .unwrap();
        }
    });
    assert_eq!(sweep(&mut p, day + 86_399).unwrap(), 0);
    assert_eq!(sweep(&mut p, day + 86_400).unwrap(), 3);
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
    assert_eq!(position_credits(&p), vec![U256::from(3 * REMAINDER)]);
    let mut expired = expired_positions(&p);
    expired.sort_unstable();
    assert_eq!(expired, vec![U256::from(1), U256::from(2), U256::from(3)]);
    assert_eq!(sweep(&mut p, day + 2 * SECONDS_PER_DAY).unwrap(), 1);
}

/// The amounts of the `PromisLimitAdded` credits of the Gem Factory positions.
fn position_credits(p: &HashMapStorageProvider) -> Vec<U256> {
    p.get_events(PROMIS_LIMIT_ADDRESS)
        .iter()
        .filter_map(|log| {
            outbe_promislimit::precompile::IPromisLimit::PromisLimitAdded::decode_log_data(log).ok()
        })
        .filter(|event| event.source == outbe_promislimit::LimitSource::GemFactoryPosition as u8)
        .map(|event| event.amountMinor)
        .collect()
}

/// The ids of the positions the deletion removed.
fn expired_positions(p: &HashMapStorageProvider) -> Vec<U256> {
    p.get_events(GEM_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| GemFactoryPositionExpired::decode_log_data(log).ok())
        .map(|event| event.gemFactoryPositionId)
        .collect()
}

#[test]
fn daily_pass_shares_series_and_position_allowance_across_reentry() {
    use alloy_primitives::B256;
    use outbe_gem::GemContract;
    use outbe_system_common::pass_budget::SWEEP_WRITES_PER_BLOCK;
    use outbe_system_common::time::timestamp_to_date_key;
    let mut p = world();
    StorageHandle::enter(&mut p, |s| {
        outbe_oracle::schema::OracleContract::new(s.clone())
            .utc_day_vwap_last_finalized
            .write(timestamp_to_date_key(EXPIRES))
            .unwrap();
        let mut gem = GemContract::new(s.clone());
        for n in 1..=SWEEP_WRITES_PER_BLOCK + 1 {
            let group = B256::from(U256::from(n).to_be_bytes::<32>());
            gem.group_called_at.write(&group, 1).unwrap();
            gem.group_gem_count.write(&group, 1).unwrap();
            gem.group_unsettled_load_minor
                .write(&group, U256::from(1))
                .unwrap();
            gem.push_called(group, EXPIRES).unwrap();
        }
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 1,
                timestamp: DAY_CLOSED,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: EXPIRES,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            s.clone(),
        );
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
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: 2,
                timestamp: DAY_CLOSED + 1,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: EXPIRES,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
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
                .day_capacity_minor
                .read(&expiry_day())
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
            .add_position(&GemFactoryPosition {
                gem_factory_position_id: U256::from(1),
                owner: Address::repeat_byte(1),
                source_series_id: SeriesId::pack(20_240_101, *b"USD", b'U').unwrap(),
                remaining_capacity_minor: U256::from(REMAINDER),
                issued_at: EXPIRES - SECONDS_PER_DAY,
                expires_at: EXPIRES,
            })
            .unwrap();
    });
    p.clear_mutation_failure();
    p
}

/// One block's daily pass, then its position deletion. Returns the positions deleted.
fn sweep(p: &mut HashMapStorageProvider, at: u64) -> Result<u32> {
    StorageHandle::enter(p, |s| {
        let ctx = BlockRuntimeContext::new(
            BlockContext {
                block_gas_limit: 0,
                user_gas_used: 0,
                block_number: at,
                timestamp: at,
                chain_id: 1,
                genesis_hash: alloy_primitives::B256::ZERO,
                genesis_timestamp: EXPIRES - SECONDS_PER_DAY,
                proposer: alloy_primitives::Address::ZERO,
                validators: Vec::new(),
            },
            s,
        );
        let block = super::daily_pass(&ctx)?;
        position_expiry::delete_expired_positions(&block)
    })
}

fn expiry_day() -> u32 {
    outbe_core_common::deadline_queue::deadline_day(EXPIRES).unwrap()
}

fn conserved(p: &mut HashMapStorageProvider, terminal: bool) {
    StorageHandle::enter(p, |s| {
        let factory = GemFactoryContract::new(s.clone());
        let left = factory.day_capacity_minor.read(&expiry_day()).unwrap();
        let returned = PromisLimitContract::new(s).promis_limit().unwrap();
        assert_eq!(left + returned, U256::from(REMAINDER));
        if terminal {
            assert_eq!(returned, U256::from(REMAINDER));
            assert!(factory.positions.get(U256::from(1)).unwrap().is_none());
        } else if let Some(position) = factory.positions.get(U256::from(1)).unwrap() {
            assert_eq!(position.owner, Address::repeat_byte(1));
        }
    });
    if terminal {
        assert_eq!(position_credits(p), vec![U256::from(REMAINDER)]);
        assert_eq!(expired_positions(p), vec![U256::from(1)]);
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
            // A failed expiry keeps the day queued, a failed deletion keeps its position.
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
