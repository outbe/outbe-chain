//! Promis load ladder: a tenfold move of any reference currency steps the load.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_primitives::addresses::DESIS_ADDRESS;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::time::EmissionDate;

use crate::precompile::IDesis;
use crate::runtime;
use crate::schema::{DesisContract, ReferenceCurrencyPrice};

const LAUNCH: u32 = 12;
const USD: u16 = 840;
const EUR: u16 = 978;
const FIRST_DAY: u32 = 20_260_101;

type Day<'a> = &'a [(u16, u128)];

struct Run {
    exponents: Vec<u32>,
    /// First priced day: reference currency, rate and reference are zero.
    launches: Vec<IDesis::IntexPromisLoadSet>,
    /// Tenfold steps. A launch fix is not a step.
    steps: Vec<IDesis::IntexPromisLoadSet>,
    references: Vec<(u16, U256, u32)>,
}

fn is_launch_fix(event: &IDesis::IntexPromisLoadSet) -> bool {
    event.referenceCurrency == 0 && event.rateMinor.is_zero() && event.referenceMinor.is_zero()
}

/// One auction start per day, rows in ascending ISO as the oracle hands them over.
fn run(days: &[Day<'_>]) -> Run {
    run_from(0, days)
}

fn run_from(stored_exponent: u32, days: &[Day<'_>]) -> Run {
    let mut provider = HashMapStorageProvider::new(1);
    let (exponents, references) = StorageHandle::enter(&mut provider, |s| {
        let mut contract = s.contract::<DesisContract>();
        contract
            .promis_load_exponent
            .write(stored_exponent)
            .unwrap();
        let exponents = days
            .iter()
            .zip(FIRST_DAY..)
            .map(|(day, key)| {
                let rows: Vec<ReferenceCurrencyPrice> = day
                    .iter()
                    .map(|&(iso_code, rate)| ReferenceCurrencyPrice {
                        iso_code,
                        entry_price_minor: U256::from(rate),
                        call_price_minor: U256::ZERO,
                    })
                    .collect();
                runtime::step_promis_load(&mut contract, EmissionDate::new(key), &rows)
                    .unwrap()
                    .ilog10()
            })
            .collect();
        let references = [USD, EUR]
            .into_iter()
            .map(|iso| {
                (
                    iso,
                    contract.promis_load_reference_minor.read(&iso).unwrap(),
                    contract.promis_load_reference_exponent.read(&iso).unwrap(),
                )
            })
            .collect();
        (exponents, references)
    });
    let sets: Vec<IDesis::IntexPromisLoadSet> = provider
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|log| IDesis::IntexPromisLoadSet::decode_log_data(log).ok())
        .collect();
    let (launches, steps) = sets.into_iter().partition(is_launch_fix);
    Run {
        exponents,
        launches,
        steps,
        references,
    }
}

#[test]
fn whole_decades_are_counted_from_the_capture() {
    for (rate, reference, decades) in [
        (100u128, 100u128, 0),
        (999, 100, 0),
        (1_000, 100, 1),
        (100_000, 100, 3),
        (101, 1_000, 0),
        (100, 1_000, -1),
        (1, 100_000, -5),
    ] {
        assert_eq!(
            runtime::decades_moved(U256::from(rate), U256::from(reference)),
            decades,
            "{rate} against {reference}"
        );
    }
}

#[test]
fn the_rung_stays_inside_the_table() {
    assert_eq!(runtime::stepped_exponent(LAUNCH, 1), 11);
    assert_eq!(runtime::stepped_exponent(LAUNCH, -1), 13);
    assert_eq!(runtime::stepped_exponent(LAUNCH, 20), 1);
    assert_eq!(runtime::stepped_exponent(LAUNCH, -20), 21);
}

#[test]
fn the_first_priced_day_captures_every_currency_without_a_step() {
    let r = run(&[&[(USD, 100), (EUR, 92)]]);
    assert_eq!(r.exponents, [LAUNCH]);
    assert!(r.steps.is_empty());
    assert_eq!(r.launches.len(), 1);
    assert_eq!(r.launches[0].emissionDay, FIRST_DAY);
    assert_eq!(r.launches[0].loadMinor, 10u128.pow(LAUNCH));
    assert_eq!(r.launches[0].referenceCurrency, 0);
    assert!(r.launches[0].rateMinor.is_zero());
    assert!(r.launches[0].referenceMinor.is_zero());
    assert_eq!(
        r.references,
        [
            (USD, U256::from(100), LAUNCH),
            (EUR, U256::from(92), LAUNCH)
        ]
    );
}

#[test]
fn a_tenfold_rise_steps_the_load_down_and_less_holds() {
    let r = run(&[&[(USD, 100)], &[(USD, 999)], &[(USD, 1_000)]]);
    assert_eq!(r.exponents, [LAUNCH, LAUNCH, LAUNCH - 1]);
    assert_eq!(r.launches.len(), 1);
    assert_eq!(r.launches[0].loadMinor, 10u128.pow(LAUNCH));
    assert_eq!(r.steps.len(), 1);
    let step = &r.steps[0];
    assert_eq!(step.emissionDay, FIRST_DAY + 2);
    assert_eq!(step.loadMinor, 10u128.pow(LAUNCH - 1));
    assert_eq!(step.referenceCurrency, USD);
    assert_eq!(step.rateMinor, U256::from(1_000));
    assert_eq!(step.referenceMinor, U256::from(100));
}

#[test]
fn a_tenfold_fall_steps_the_load_up() {
    let r = run(&[&[(USD, 100)], &[(USD, 10)]]);
    assert_eq!(r.exponents, [LAUNCH, LAUNCH + 1]);
}

#[test]
fn one_currency_steps_the_load_and_every_priced_one_recaptures() {
    let r = run(&[
        &[(USD, 100), (EUR, 100)],
        &[(USD, 500), (EUR, 1_000)],
        // Against its launch capture USD would be 49.99x here.
        &[(USD, 4_999), (EUR, 9_999)],
        &[(USD, 5_000), (EUR, 9_999)],
    ]);
    assert_eq!(r.exponents, [LAUNCH, LAUNCH - 1, LAUNCH - 1, LAUNCH - 2]);
    assert_eq!(r.steps[0].referenceCurrency, EUR);
    assert_eq!(r.steps[1].referenceCurrency, USD);
}

#[test]
fn the_load_returns_only_after_a_tenfold_move_back() {
    let r = run(&[&[(USD, 100)], &[(USD, 1_000)], &[(USD, 101)], &[(USD, 100)]]);
    assert_eq!(r.exponents, [LAUNCH, LAUNCH - 1, LAUNCH - 1, LAUNCH]);
}

#[test]
fn a_multi_decade_move_steps_all_its_decades_at_once() {
    let r = run(&[&[(USD, 100)], &[(USD, 100_000)]]);
    assert_eq!(r.exponents, [LAUNCH, LAUNCH - 3]);
    assert_eq!(r.steps.len(), 1);
}

#[test]
fn the_strongest_mover_steps_and_a_tie_goes_to_the_lowest_iso() {
    let strongest = run(&[&[(USD, 100), (EUR, 100)], &[(USD, 1_000), (EUR, 100_000)]]);
    assert_eq!(strongest.exponents, [LAUNCH, LAUNCH - 3]);
    assert_eq!(strongest.steps[0].referenceCurrency, EUR);

    let tie = run(&[&[(USD, 100), (EUR, 100)], &[(USD, 1_000), (EUR, 1_000)]]);
    assert_eq!(tie.exponents, [LAUNCH, LAUNCH - 1]);
    assert_eq!(tie.steps[0].referenceCurrency, USD);
}

#[test]
fn currencies_moving_both_ways_hold_without_recapturing() {
    let r = run(&[
        &[(USD, 100), (EUR, 100)],
        &[(USD, 1_000), (EUR, 10)],
        &[(USD, 1_000), (EUR, 100)],
    ]);
    assert_eq!(r.exponents, [LAUNCH, LAUNCH, LAUNCH - 1]);
    assert_eq!(r.steps.len(), 1);
}

#[test]
fn a_currency_unpriced_on_the_step_day_recaptures_before_it_is_checked() {
    let r = run(&[
        &[(USD, 100), (EUR, 100)],
        &[(USD, 1_000)],
        // EUR's launch capture is stale: it re-captures instead of stepping again.
        &[(USD, 1_000), (EUR, 1_000)],
        &[(USD, 1_000), (EUR, 10_000)],
    ]);
    assert_eq!(r.exponents, [LAUNCH, LAUNCH - 1, LAUNCH - 1, LAUNCH - 2]);
    assert_eq!(r.steps.len(), 2);
}

#[test]
fn a_currency_priced_after_launch_captures_then_counts() {
    let r = run(&[
        &[(USD, 100)],
        &[(USD, 100), (EUR, 1)],
        &[(USD, 100), (EUR, 10)],
    ]);
    assert_eq!(r.exponents, [LAUNCH, LAUNCH, LAUNCH - 1]);
    assert_eq!(r.steps[0].referenceCurrency, EUR);
}

#[test]
fn a_step_clamped_to_nothing_holds() {
    let r = run(&[
        &[(USD, 1)],
        &[(USD, 1_000_000_000_000)],
        &[(USD, 10_000_000_000_000)],
    ]);
    assert_eq!(r.exponents, [LAUNCH, 1, 1]);
    assert_eq!(r.steps.len(), 1);
    assert_eq!(r.references[0], (USD, U256::from(1_000_000_000_000u128), 1));
}

#[test]
fn a_corrupt_stored_exponent_runs_on_the_widest_rung() {
    let r = run_from(u32::MAX, &[&[(USD, 100)], &[(USD, 1_000)]]);
    assert_eq!(r.exponents, [21, 20]);
    assert!(r.launches.is_empty());
    assert_eq!(r.steps.len(), 1);
}

fn day(rows: &[(u16, u128)]) -> Vec<ReferenceCurrencyPrice> {
    rows.iter()
        .map(|&(iso_code, rate)| ReferenceCurrencyPrice {
            iso_code,
            entry_price_minor: U256::from(rate),
            call_price_minor: U256::ZERO,
        })
        .collect()
}

#[test]
fn an_unpriced_day_does_not_store_a_rung_or_emit() {
    let mut provider = HashMapStorageProvider::new(1);
    let stored = StorageHandle::enter(&mut provider, |s| {
        let mut contract = s.contract::<DesisContract>();
        let load = runtime::step_promis_load_with_launch(
            &mut contract,
            EmissionDate::new(FIRST_DAY),
            &[],
            9,
        )
        .unwrap();
        assert_eq!(load, 10u128.pow(9));
        contract.promis_load_exponent.read().unwrap()
    });
    assert_eq!(stored, 0);
    assert!(provider.get_events(DESIS_ADDRESS).is_empty());
}

#[test]
fn a_nonstandard_launch_exponent_is_the_first_priced_rung() {
    let mut provider = HashMapStorageProvider::new(1);
    let stored = StorageHandle::enter(&mut provider, |s| {
        let mut contract = s.contract::<DesisContract>();
        let rows = day(&[(USD, 100)]);
        let load = runtime::step_promis_load_with_launch(
            &mut contract,
            EmissionDate::new(FIRST_DAY),
            &rows,
            9,
        )
        .unwrap();
        assert_eq!(load, 10u128.pow(9));
        contract.promis_load_exponent.read().unwrap()
    });
    assert_eq!(stored, 9);
    let sets: Vec<_> = provider
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|log| IDesis::IntexPromisLoadSet::decode_log_data(log).ok())
        .collect();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].loadMinor, 10u128.pow(9));
    assert_eq!(sets[0].referenceCurrency, 0);
    assert!(sets[0].rateMinor.is_zero());
    assert!(sets[0].referenceMinor.is_zero());
}

/// A stored rung stays put when a later call names a different launch rung.
#[test]
fn a_stored_rung_ignores_a_different_launch_exponent() {
    let mut provider = HashMapStorageProvider::new(1);
    let stored = StorageHandle::enter(&mut provider, |s| {
        let mut contract = s.contract::<DesisContract>();
        let first = day(&[(USD, 100)]);
        runtime::step_promis_load_with_launch(
            &mut contract,
            EmissionDate::new(FIRST_DAY),
            &first,
            9,
        )
        .unwrap();
        let quiet = day(&[(USD, 100)]);
        runtime::step_promis_load(&mut contract, EmissionDate::new(FIRST_DAY + 1), &quiet).unwrap();
        let stepped = day(&[(USD, 1_000)]);
        runtime::step_promis_load_with_launch(
            &mut contract,
            EmissionDate::new(FIRST_DAY + 2),
            &stepped,
            4,
        )
        .unwrap();
        contract.promis_load_exponent.read().unwrap()
    });
    assert_eq!(stored, 8);
    let sets: Vec<IDesis::IntexPromisLoadSet> = provider
        .get_events(DESIS_ADDRESS)
        .iter()
        .filter_map(|log| IDesis::IntexPromisLoadSet::decode_log_data(log).ok())
        .collect();
    assert_eq!(sets.len(), 2);
    assert!(is_launch_fix(&sets[0]));
    assert_eq!(sets[0].loadMinor, 10u128.pow(9));
    assert_eq!(sets[1].loadMinor, 10u128.pow(8));
    assert_eq!(sets[1].referenceCurrency, USD);
}
