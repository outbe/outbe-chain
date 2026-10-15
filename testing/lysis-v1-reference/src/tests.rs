use std::path::PathBuf;

use num_bigint::{BigInt, BigUint};
use serde_json::Value;

use crate::{
    arithmetic::{decimal, scale, trunc_signed_div},
    corpus::load_cases,
    distribution::{distribution, fraction_table_with_projection},
    evaluate::validate_and_sort,
    json::strict_json,
    *,
};

#[test]
fn signed_division_truncates_toward_zero() {
    assert_eq!(
        trunc_signed_div(&BigInt::from(7), &BigInt::from(3)),
        BigInt::from(2)
    );
    assert_eq!(
        trunc_signed_div(&BigInt::from(-7), &BigInt::from(3)),
        BigInt::from(-2)
    );
    assert_eq!(
        trunc_signed_div(&BigInt::from(7), &BigInt::from(-3)),
        BigInt::from(-2)
    );
}

#[test]
fn single_league_uses_exact_target_fraction() {
    let unit = scale();
    let target = &unit * 32_u8 / 100_u8;
    let maximum = &unit * 64_u8 / 100_u8;
    assert_eq!(
        distribution(&[unit], &[1], 1, &target, &maximum).unwrap(),
        [target]
    );
}

#[test]
fn duplicate_json_keys_are_rejected() {
    let error = strict_json::<Value>(r#"{"a":1,"a":2}"#).unwrap_err();
    assert!(error.to_string().contains("duplicate JSON key"));
}

#[test]
fn frozen_corpus_is_reference_reproducible() {
    let report = check(&default_vectors_path(), &default_manifest_path()).unwrap();
    assert_eq!(report.status, "PASS", "{report:?}");
    assert_eq!(report.case_count, 16);
}

#[test]
fn six_decimal_projection_has_the_frozen_twenty_five_unit_dust() {
    let case = load_cases(&default_vectors_path())
        .unwrap()
        .into_iter()
        .find(|case| case.case_id == "fifteen-league-normalization")
        .unwrap();
    let tributes = validate_and_sort(&case.input).unwrap();
    let total_nominal = tributes
        .iter()
        .map(|tribute| decimal(&tribute.nominal).unwrap())
        .sum::<BigUint>();
    let allocation = decimal(&case.input.lysis_limit_minor).unwrap();
    let (_, raw, normalized) =
        fraction_table_with_projection(&tributes, &total_nominal, &allocation).unwrap();

    assert_eq!(allocation, BigUint::from(4_800_000u64));
    // The share remainder belongs to the highest league, so this projection
    // is already within budget and needs no real-amount scaling.
    assert_eq!(raw, BigUint::from(4_799_975u64));
    assert_eq!(normalized, raw);
    assert_eq!(allocation - normalized, BigUint::from(25u64));
}

#[test]
fn reference_has_no_production_dependency() {
    let manifest =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .unwrap();
    for forbidden in [
        "outbe-lysis",
        "outbe-primitives",
        "alloy-primitives",
        "outbe-common",
    ] {
        assert!(
            !manifest
                .lines()
                .any(|line| line.trim_start().starts_with(forbidden)),
            "reference depends on production crate {forbidden}"
        );
    }
}
