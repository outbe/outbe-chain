use std::collections::BTreeMap;

use num_bigint::BigUint;
use num_traits::{One, Zero};
use serde_json::{json, Value};

use crate::{
    arithmetic::{checked_add_u256, decimal, scale, u256_modulus, wrap_u256},
    corpus::{CorpusCase, CorpusInput, CorpusTribute},
    distribution::fraction_table,
    failure::ReferenceFailure,
};

fn is_day_identifier(raw_id: &str, worldwide_day: u32) -> bool {
    let prefix_day = raw_id
        .get(..8)
        .and_then(|prefix| u32::from_str_radix(prefix, 16).ok());
    let is_lower_hex = raw_id
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    raw_id.len() == 64 && is_lower_hex && prefix_day == Some(worldwide_day)
}

pub(crate) fn validate_and_sort(
    input: &CorpusInput,
) -> Result<Vec<CorpusTribute>, ReferenceFailure> {
    let mut tributes = input.tributes.clone();
    tributes.sort_by(|left, right| left.tribute_id.cmp(&right.tribute_id));
    if tributes.is_empty() {
        return Err(ReferenceFailure::new("EMPTY_INPUT"));
    }
    let mut previous: Option<&str> = None;
    for (ordinal, tribute) in tributes.iter().enumerate() {
        let raw_id = tribute.tribute_id.as_str();
        if !is_day_identifier(raw_id, input.worldwide_day)
            || tribute.worldwide_day != input.worldwide_day
        {
            return Err(ReferenceFailure::ordinal("WORLDWIDE_DAY_MISMATCH", ordinal));
        }
        if previous == Some(raw_id) {
            return Err(ReferenceFailure::ordinal("DUPLICATE_INPUT", ordinal));
        }
        previous = Some(raw_id);
    }
    Ok(tributes)
}

fn owner_is_zero(owner: &str) -> bool {
    owner
        .strip_prefix("0x")
        .and_then(|value| BigUint::parse_bytes(value.as_bytes(), 16))
        .is_some_and(|value| value.is_zero())
}

fn first_league_total(
    tributes: &[CorpusTribute],
    observations: &mut Vec<Value>,
) -> Result<BigUint, ReferenceFailure> {
    let mut total_nominal = BigUint::zero();
    for (ordinal, tribute) in tributes.iter().enumerate() {
        let first = tribute
            .f1
            .ok_or_else(|| ReferenceFailure::ordinal("FIDELITY_FIRST_UNAVAILABLE", ordinal))?;
        observations.push(json!({
            "kind": "FIDELITY",
            "ordinal": ordinal,
            "phase": "FIRST",
            "owner": tribute.owner,
            "league": first,
        }));
        total_nominal = checked_add_u256(
            &total_nominal,
            &decimal(&tribute.nominal)?,
            ReferenceFailure::ordinal("TOTAL_NOMINAL_OVERFLOW", ordinal),
        )?;
    }
    if total_nominal.is_zero() {
        return Err(ReferenceFailure::new("ZERO_TOTAL_NOMINAL"));
    }
    Ok(total_nominal)
}

fn fractions_by_league(table: &[Value]) -> BTreeMap<u16, BigUint> {
    table
        .iter()
        .map(|row| {
            let league = row["league"]
                .as_u64()
                .and_then(|value| u16::try_from(value).ok())
                .expect("reference group league is u16");
            let fraction = row["fraction"]
                .as_str()
                .and_then(|value| BigUint::parse_bytes(value.as_bytes(), 10))
                .expect("reference group fraction is decimal");
            (league, fraction)
        })
        .collect()
}

struct Evaluation {
    unit: BigUint,
    remaining: BigUint,
    observations: Vec<Value>,
    actions: Vec<Value>,
}

impl Evaluation {
    fn gratis_load(
        &mut self,
        ordinal: usize,
        tribute: &CorpusTribute,
        fraction: &BigUint,
    ) -> Result<BigUint, ReferenceFailure> {
        let load = wrap_u256(decimal(&tribute.nominal)? * fraction) / &self.unit;
        if load.is_zero() {
            return Err(ReferenceFailure::ordinal("ZERO_GRATIS_LOAD", ordinal));
        }
        if load > self.remaining {
            return Err(ReferenceFailure::ordinal(
                "GRATIS_LOAD_EXCEEDS_REMAINING",
                ordinal,
            ));
        }
        self.remaining -= &load;
        Ok(load)
    }

    fn entry_price(
        &mut self,
        ordinal: usize,
        tribute: &CorpusTribute,
    ) -> Result<BigUint, ReferenceFailure> {
        let currency = tribute.reference_currency;
        let entry_price = tribute
            .entry_price
            .as_deref()
            .ok_or_else(|| {
                ReferenceFailure::ordinal_currency("ENTRY_PRICE_UNAVAILABLE", ordinal, currency)
            })
            .and_then(decimal)?;
        self.observations.push(json!({
            "kind": "ORACLE",
            "ordinal": ordinal,
            "currency": currency,
            "entry_price": entry_price.to_string(),
        }));

        // Validated as input; the entry bound keeps any u16-rate call price in U256.
        decimal(&tribute.tribute_price)?;
        if &entry_price * (100_u32 + u32::from(u16::MAX)) >= u256_modulus() {
            return Err(ReferenceFailure::new("ARITHMETIC"));
        }
        Ok(entry_price)
    }

    fn second_league(
        &mut self,
        ordinal: usize,
        tribute: &CorpusTribute,
        first: u16,
    ) -> Result<u16, ReferenceFailure> {
        let second = tribute
            .f2
            .ok_or_else(|| ReferenceFailure::ordinal("FIDELITY_SECOND_UNAVAILABLE", ordinal))?;
        self.observations.push(json!({
            "kind": "FIDELITY",
            "ordinal": ordinal,
            "phase": "SECOND",
            "owner": tribute.owner,
            "league": second,
        }));
        if second != first {
            return Err(ReferenceFailure::ordinal("FIDELITY_MISMATCH", ordinal));
        }
        Ok(second)
    }

    fn nod_action(
        &mut self,
        ordinal: usize,
        tribute: &CorpusTribute,
        first: u16,
        fraction: &BigUint,
    ) -> Result<(), ReferenceFailure> {
        let load = self.gratis_load(ordinal, tribute, fraction)?;
        let entry_price = self.entry_price(ordinal, tribute)?;
        let second = self.second_league(ordinal, tribute, first)?;
        let cost = wrap_u256(&entry_price * &load) / &self.unit;
        let cost = if !entry_price.is_zero() && !load.is_zero() {
            cost.max(BigUint::one())
        } else {
            cost
        };
        if cost.is_zero() {
            return Err(ReferenceFailure::ordinal("ZERO_COST", ordinal));
        }
        if !tribute.nod_target_available || owner_is_zero(&tribute.owner) {
            return Err(ReferenceFailure::ordinal("INVALID_NOD_TARGET", ordinal));
        }

        self.actions.push(json!({
            "source_tribute_id": tribute.tribute_id,
            "owner": tribute.owner,
            "league": second,
            "entry_price": entry_price.to_string(),
            "issuance_currency": tribute.issuance_currency,
            "reference_currency": tribute.reference_currency,
        }));
        Ok(())
    }
}

fn try_evaluate(case: &CorpusCase) -> Result<Value, ReferenceFailure> {
    let input = &case.input;
    let tributes = validate_and_sort(input)?;
    let mut observations = Vec::new();
    let total_nominal = first_league_total(&tributes, &mut observations)?;
    let allocation = decimal(&input.lysis_limit_minor)?;
    let table = fraction_table(&tributes, &total_nominal, &allocation)?;
    let fractions = fractions_by_league(&table);

    let mut evaluation = Evaluation {
        unit: scale(),
        remaining: allocation.clone(),
        observations,
        actions: Vec::with_capacity(tributes.len()),
    };
    for (ordinal, tribute) in tributes.iter().enumerate() {
        let first = tribute
            .f1
            .expect("first league was validated before fraction lookup");
        let fraction = fractions
            .get(&first)
            .expect("fraction exists for every first league");
        evaluation.nod_action(ordinal, tribute, first, fraction)?;
    }

    Ok(json!({
        "status": "SUCCESS",
        "total_nominal": total_nominal.to_string(),
        "lysis_limit_minor": allocation.to_string(),
        "remaining_lysis_limit_minor": evaluation.remaining.to_string(),
        "group_table": table,
        "nod_actions": evaluation.actions,
        "observations": evaluation.observations,
    }))
}

pub(crate) fn evaluate(case: &CorpusCase) -> Value {
    try_evaluate(case).unwrap_or_else(|failure| failure.envelope())
}
