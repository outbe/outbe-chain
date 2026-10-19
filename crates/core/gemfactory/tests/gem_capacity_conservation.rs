//! Included Gem subtypes conserve live load through real call/expiry hooks over the
//! EVM state.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_gem::{
    api, config::PROFILE_PROD, CallTerms, GemAddParams, GemContract, GemParams, GemState,
};
use outbe_gemfactory::{called, hooks};
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
use outbe_primitives::time::SECONDS_PER_DAY as DAY;
const CALLED: u64 = ISSUED + 29 * DAY;
const DEADLINE: u64 = CALLED + 7 * DAY;
const GEMS: u8 = 5;

/// The PROD call terms a bucket opened at `entry` seals.
fn prod_terms(entry: U256) -> CallTerms {
    let prod = GemParams::PROD;
    CallTerms {
        call_price_minor: outbe_primitives::markup::marked_up(entry, prod.call_rate).unwrap(),
        reference_currency: 840,
        call_rate: prod.call_rate,
        call_window_seconds: prod.call_window_seconds,
        call_threshold_seconds: prod.call_threshold_seconds,
        call_notice_period_seconds: prod.call_notice_period_seconds,
    }
}

fn world() -> (HashMapStorageProvider, Vec<(U256, U256)>) {
    let mut p = HashMapStorageProvider::new(1);
    p.set_block_number(1);
    p.set_timestamp(U256::from(CALLED));
    let rights = StorageHandle::enter(&mut p, |s| {
        GemContract::new(s.clone())
            .config_profile
            .write(PROFILE_PROD)
            .unwrap();
        let rights = (0..GEMS)
            .map(|i| {
                let load = U256::from(1_000_001 + i as u64);
                let id = api::add_gem(
                    &s,
                    GemAddParams {
                        owner: Address::repeat_byte(i + 1),
                        promis_load_minor: load,
                        entry_price_minor: U256::from(1_000_000),
                        issuance_currency: 840,
                        reference_currency: 840,
                        issued_at: ISSUED,
                        call_terms: Some(prod_terms(U256::from(1_000_000))),
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
                api::get_view(&s, *id)
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
    (p, rights)
}

fn sweep(p: &mut HashMapStorageProvider, at: u64) -> Result<()> {
    p.set_timestamp(U256::from(at));
    StorageHandle::enter(p, |s| {
        let ctx = BlockRuntimeContext::new(BlockContext::empty_for_tests(2, at, 1), s);
        hooks::sweep_forfeits(&ctx)?;
        called::run_call_slice(&ctx).map(drop)
    })
}

fn conserved(p: &mut HashMapStorageProvider, rights: &[(U256, U256)]) {
    let (live, returned, count) = StorageHandle::enter(p, |s| {
        let live = rights
            .iter()
            .filter_map(|(id, _)| api::get_item(&s, *id).unwrap())
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
        .filter_map(|event| outbe_gem::precompile::IGem::GemForfeited::decode_log_data(event).ok())
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
    let (mut p, rights) = world();
    sweep(&mut p, DEADLINE).unwrap();
    conserved(&mut p, &rights);
    StorageHandle::enter(&mut p, |s| {
        assert_eq!(GemContract::new(s.clone()).total_supply().unwrap(), 5);
        assert_eq!(
            PromisLimitContract::new(s).get_total_unallocated().unwrap(),
            U256::ZERO
        );
    });
    let due = ((DEADLINE / 3600) + 1) * 3600;
    sweep(&mut p, due).unwrap();
    conserved(&mut p, &rights);
    sweep(&mut p, due + DAY).unwrap();
    conserved(&mut p, &rights);
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
    let (mut p, rights) = world();
    let (paid, paid_load) = rights[0];
    // The completed settlement footprint; payment itself is covered by the
    // settlement tests.
    StorageHandle::enter(&mut p, |s| {
        let (item, bucket) = api::load_pair(&s, paid).unwrap().unwrap();
        api::record_settled(&s, item, bucket).unwrap();
        let (item, bucket) = api::load_pair(&s, paid).unwrap().unwrap();
        api::remove_gem(&s, item, bucket).unwrap();
        assert_eq!(
            PromisLimitContract::new(s).get_total_unallocated().unwrap(),
            U256::ZERO
        );
    });
    let due = ((DEADLINE / 3600) + 1) * 3600;
    sweep(&mut p, due).unwrap();
    sweep(&mut p, due + DAY).unwrap();
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
    let (mut baseline, _) = world();
    sweep(&mut baseline, due).unwrap();
    let mutations = baseline.clear_mutation_failure();
    assert!(mutations > 0);
    for after in [false, true] {
        for point in 0..mutations {
            let (mut p, rights) = world();
            if after {
                p.fail_after_mutation_at(point);
            } else {
                p.fail_mutation_at(point);
            }
            let result = sweep(&mut p, due);
            p.clear_mutation_failure();
            assert!(result.is_err(), "failure point {point}, after={after}");
            conserved(&mut p, &rights);
            sweep(&mut p, due + 1).unwrap();
            sweep(&mut p, due + DAY).unwrap();
            conserved(&mut p, &rights);
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
