//! Included Gem subtypes conserve live load through real call/expiry hooks over the
//! compressed-body store.
use std::sync::Arc;

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::test_support::seed_compressed_entities_genesis;
use outbe_compressed_entities::{begin_block, ExecutionScope};
use outbe_gem::{
    api, called, config::PROFILE_PROD, hooks, GemAddParams, GemContract, GemRepositoryReader,
    GemState,
};
use outbe_offchain_storage::MemoryStorage;
use outbe_oracle::schema::OracleContract;
use outbe_primitives::{
    address_pair::AddressPair,
    addresses::GEM_ADDRESS,
    block::{BlockContext, BlockRuntimeContext},
    error::Result,
    storage::{hashmap::HashMapStorageProvider, StorageHandle},
    time::{previous_date_key, timestamp_to_date_key},
};
use outbe_promislimit::PromisLimitContract;

const ISSUED: u64 = 1_704_067_200;
const DAY: u64 = 86_400;
const CALLED: u64 = ISSUED + 29 * DAY;
const DEADLINE: u64 = CALLED + 7 * DAY;
// Genesis, Validator, SRA, Wallet, Merchant.
const TYPES: [u8; 5] = [0, 1, 2, 3, 5];

struct World {
    scope: ExecutionScope,
    parent: GemRepositoryReader,
}

fn world() -> (HashMapStorageProvider, World, Vec<(U256, U256)>) {
    let mut p = HashMapStorageProvider::new(1);
    p.set_block_number(1);
    p.set_timestamp(U256::from(CALLED));
    let world = World {
        scope: ExecutionScope::default(),
        parent: outbe_gem::gem_reader(Arc::new(MemoryStorage::new())),
    };
    let rights = StorageHandle::enter(&mut p, |s| {
        seed_compressed_entities_genesis(&s).unwrap();
        begin_block(s.clone(), &world.scope).unwrap();
        GemContract::new(s.clone())
            .config_profile
            .write(PROFILE_PROD)
            .unwrap();
        let rights = TYPES
            .into_iter()
            .enumerate()
            .map(|(i, kind)| {
                let load = U256::from(1_000_001 + i as u64);
                let id = api::add_gem(
                    &s,
                    &world.scope,
                    &world.parent,
                    GemAddParams {
                        owner: Address::repeat_byte(i as u8 + 1),
                        gem_type: kind,
                        promis_load_minor: load,
                        entry_price_minor: U256::from(1_000_000),
                        floor_price_minor: if kind == 0 {
                            U256::ZERO
                        } else {
                            U256::from(1_100_000)
                        },
                        issuance_currency: 840,
                        reference_currency: 840,
                        issued_at: ISSUED,
                    },
                )
                .unwrap();
                (id, load)
            })
            .collect::<Vec<_>>();
        let pair =
            outbe_oracle::api::register_pair(s.clone(), AddressPair::new_coen_to(840)).unwrap();
        let oracle = OracleContract::new(s.clone());
        oracle.reference_currencies.push(840).unwrap();
        let latest = previous_date_key(timestamp_to_date_key(CALLED));
        let mut day = latest;
        for _ in 0..28 {
            oracle
                .record_utc_day_vwap(day, pair, U256::from(3_000_000))
                .unwrap();
            day = previous_date_key(day);
        }
        oracle.utc_day_vwap_last_finalized.write(latest).unwrap();
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(1, CALLED, 1), s.clone());
        called::scan_and_call(&ctx).unwrap();
        for (id, _) in &rights {
            assert_eq!(
                api::get_view(&s, &world.scope, &world.parent, *id)
                    .unwrap()
                    .unwrap()
                    .effective_state(CALLED),
                GemState::Called as u8
            );
        }
        assert_eq!(GemContract::new(s.clone()).total_supply().unwrap(), 5);
        assert_eq!(
            PromisLimitContract::new(s).get_total_unallocated().unwrap(),
            U256::ZERO
        );
        rights
    });
    p.clear_mutation_failure();
    (p, world, rights)
}

fn sweep(p: &mut HashMapStorageProvider, world: &World, at: u64) -> Result<()> {
    p.set_timestamp(U256::from(at));
    StorageHandle::enter(p, |s| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, at, 1), s);
        hooks::sweep_forfeits(&ctx, &world.scope, &world.parent)?;
        called::run_call_slice(&ctx).map(drop)
    })
}

fn conserved(p: &mut HashMapStorageProvider, world: &World, rights: &[(U256, U256)]) {
    let (live, returned, count) = StorageHandle::enter(p, |s| {
        let live = rights
            .iter()
            .filter_map(|(id, _)| api::get_item(&s, &world.scope, &world.parent, *id).unwrap())
            .map(|gem| gem.promis_load_minor)
            .sum::<U256>();
        let returned = PromisLimitContract::new(s.clone())
            .get_total_unallocated()
            .unwrap();
        (live, returned, GemContract::new(s).total_supply().unwrap())
    });
    assert_eq!(
        live + returned,
        rights.iter().map(|(_, load)| *load).sum::<U256>()
    );
    let expired = p
        .get_events(GEM_ADDRESS)
        .iter()
        .filter_map(|event| outbe_gem::precompile::IGem::GemExpired::decode_log_data(event).ok())
        .collect::<Vec<_>>();
    assert_eq!(expired.len() as u64 + count, rights.len() as u64);
    assert_eq!(
        expired
            .iter()
            .map(|event| event.promisLoadMinor)
            .sum::<U256>(),
        returned
    );
}

#[test]
fn every_included_gem_subtype_returns_its_full_load_once_after_forfeiture() {
    let (mut p, world, rights) = world();
    sweep(&mut p, &world, DEADLINE).unwrap();
    conserved(&mut p, &world, &rights);
    StorageHandle::enter(&mut p, |s| {
        assert_eq!(GemContract::new(s.clone()).total_supply().unwrap(), 5);
        assert_eq!(
            PromisLimitContract::new(s).get_total_unallocated().unwrap(),
            U256::ZERO
        );
    });
    let due = ((DEADLINE / 3600) + 1) * 3600;
    sweep(&mut p, &world, due).unwrap();
    conserved(&mut p, &world, &rights);
    sweep(&mut p, &world, due + DAY).unwrap();
    conserved(&mut p, &world, &rights);
    StorageHandle::enter(&mut p, |s| {
        assert_eq!(GemContract::new(s.clone()).total_supply().unwrap(), 0);
        assert_eq!(
            GemContract::new(s).expiry_tree_root.read().unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn a_paid_then_mined_gem_returns_no_capacity_while_unpaid_siblings_forfeit() {
    let (mut p, world, rights) = world();
    let (paid, paid_load) = rights[0];
    // The completed settlement footprint; payment itself is covered by the
    // settlement tests.
    StorageHandle::enter(&mut p, |s| {
        api::settle_gem(&s, &world.scope, &world.parent, paid).unwrap();
        api::burn_settled(&s, &world.scope, &world.parent, paid).unwrap();
        assert_eq!(
            PromisLimitContract::new(s).get_total_unallocated().unwrap(),
            U256::ZERO
        );
    });
    let due = ((DEADLINE / 3600) + 1) * 3600;
    sweep(&mut p, &world, due).unwrap();
    sweep(&mut p, &world, due + DAY).unwrap();
    StorageHandle::enter(&mut p, |s| {
        assert_eq!(GemContract::new(s.clone()).total_supply().unwrap(), 0);
        let total = rights.iter().map(|(_, load)| *load).sum::<U256>();
        assert_eq!(
            PromisLimitContract::new(s).get_total_unallocated().unwrap(),
            total - paid_load
        );
    });
}

#[test]
fn failures_before_and_after_every_expiry_mutation_conserve_load_and_retry_once() {
    let due = ((DEADLINE / 3600) + 1) * 3600;
    let (mut baseline, baseline_world, _) = world();
    sweep(&mut baseline, &baseline_world, due).unwrap();
    let mutations = baseline.clear_mutation_failure();
    assert!(mutations > 0);
    for after in [false, true] {
        for point in 0..mutations {
            let (mut p, world, rights) = world();
            if after {
                p.fail_after_mutation_at(point);
            } else {
                p.fail_mutation_at(point);
            }
            let result = sweep(&mut p, &world, due);
            p.clear_mutation_failure();
            assert!(result.is_err(), "failure point {point}, after={after}");
            conserved(&mut p, &world, &rights);
            sweep(&mut p, &world, due + 1).unwrap();
            sweep(&mut p, &world, due + DAY).unwrap();
            conserved(&mut p, &world, &rights);
            StorageHandle::enter(&mut p, |s| {
                assert_eq!(GemContract::new(s.clone()).total_supply().unwrap(), 0);
                assert_eq!(
                    GemContract::new(s).expiry_tree_root.read().unwrap(),
                    U256::ZERO
                );
            });
        }
    }
}
