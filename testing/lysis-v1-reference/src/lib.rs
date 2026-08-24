//! Independent Rust reference for bounded Lysis Program V1.
//!
//! Independence is structural: this crate has no dependency on the production
//! Lysis crate, Outbe domain crates, Alloy primitives or shared arithmetic
//! helpers. It consumes the frozen JSONL contract and implements the specified
//! operation order with arbitrary-precision integers.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};
use num_bigint::{BigInt, BigUint};
use num_traits::{One, Signed, Zero};
use serde::{
    de::{DeserializeOwned, Error as _, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use serde_json::{json, Map, Number, Value};
use sha2::{Digest, Sha256};

const SCALE_DECIMAL: &str = "1000000";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CorpusCase {
    case_id: String,
    requirements: Vec<String>,
    input: CorpusInput,
    expected: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CorpusInput {
    worldwide_day: u32,
    logical_evaluation_time: u64,
    gratis_allocation: String,
    mandatory_price_840: Option<String>,
    tributes: Vec<CorpusTribute>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CorpusTribute {
    tribute_id: String,
    owner: String,
    worldwide_day: u32,
    issuance_currency: u16,
    nominal: String,
    reference_currency: u16,
    tribute_price: String,
    excluded: bool,
    f1: Option<u16>,
    f2: Option<u16>,
    conditional_price: Option<String>,
    nod_target_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReferenceFailure {
    kind: &'static str,
    ordinal: Option<usize>,
    currency: Option<u16>,
}

impl ReferenceFailure {
    const fn new(kind: &'static str) -> Self {
        Self {
            kind,
            ordinal: None,
            currency: None,
        }
    }

    const fn ordinal(kind: &'static str, ordinal: usize) -> Self {
        Self {
            kind,
            ordinal: Some(ordinal),
            currency: None,
        }
    }

    const fn currency(kind: &'static str, currency: u16) -> Self {
        Self {
            kind,
            ordinal: None,
            currency: Some(currency),
        }
    }

    const fn ordinal_currency(kind: &'static str, ordinal: usize, currency: u16) -> Self {
        Self {
            kind,
            ordinal: Some(ordinal),
            currency: Some(currency),
        }
    }

    fn envelope(&self) -> Value {
        let mut error = Map::new();
        error.insert("kind".to_owned(), Value::String(self.kind.to_owned()));
        if let Some(ordinal) = self.ordinal {
            error.insert("ordinal".to_owned(), json!(ordinal));
        }
        if let Some(currency) = self.currency {
            error.insert("currency".to_owned(), json!(currency));
        }
        json!({"status": "FAILURE", "error": error})
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckReport {
    pub status: String,
    pub case_count: usize,
    pub case_ids: Vec<String>,
    pub expected_outputs_sha256: String,
    pub reference_sha256: String,
    pub case_file_sha256: String,
    pub mismatches: Vec<String>,
    pub integrity_errors: Vec<String>,
}

#[must_use]
pub fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("reference crate is under testing")
        .to_path_buf()
}

#[must_use]
pub fn default_vectors_path() -> PathBuf {
    repository_root().join("crates/core/lysis/vectors/lysis-v1/cases.jsonl")
}

#[must_use]
pub fn default_manifest_path() -> PathBuf {
    repository_root().join("crates/core/lysis/vectors/lysis-v1/manifest.json")
}

#[must_use]
pub fn reference_source_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs")
}

fn scale() -> BigUint {
    BigUint::parse_bytes(SCALE_DECIMAL.as_bytes(), 10).expect("constant scale is valid")
}

fn u256_modulus() -> BigUint {
    BigUint::one() << 256_u16
}

fn i256_max() -> BigUint {
    (BigUint::one() << 255_u16) - BigUint::one()
}

fn wrap_u256(value: BigUint) -> BigUint {
    value % u256_modulus()
}

fn checked_add_u256(
    left: &BigUint,
    right: &BigUint,
    failure: ReferenceFailure,
) -> Result<BigUint, ReferenceFailure> {
    let result = left + right;
    if result >= u256_modulus() {
        Err(failure)
    } else {
        Ok(result)
    }
}

fn decimal(value: &str) -> Result<BigUint, ReferenceFailure> {
    BigUint::parse_bytes(value.as_bytes(), 10).ok_or_else(|| ReferenceFailure::new("ARITHMETIC"))
}

fn to_i256(value: &BigUint) -> Result<BigInt, ReferenceFailure> {
    if value > &i256_max() {
        Err(ReferenceFailure::new("ARITHMETIC"))
    } else {
        Ok(BigInt::from(value.clone()))
    }
}

fn trunc_signed_div(numerator: &BigInt, denominator: &BigInt) -> BigInt {
    assert!(!denominator.is_zero(), "reference denominator is non-zero");
    numerator / denominator
}

fn fp_root(value: &BigUint, exponent_denominator: u32) -> Result<BigUint, ReferenceFailure> {
    let unit = scale();
    if value.is_zero() {
        return Ok(BigUint::zero());
    }
    if value == &unit {
        return Ok(unit);
    }
    let target = value * unit.pow(exponent_denominator - 1);
    let mut low = BigUint::one();
    let mut high = value.max(&unit).clone();
    while low < high {
        let middle = &low + ((&high - &low + BigUint::one()) / 2_u8);
        if middle.pow(exponent_denominator) > target {
            high = middle - BigUint::one();
        } else {
            low = middle;
        }
    }
    if low >= u256_modulus() {
        Err(ReferenceFailure::new("ARITHMETIC"))
    } else {
        Ok(low)
    }
}

fn fallback_tau(group_count: usize, tribute_count: usize) -> Result<BigUint, ReferenceFailure> {
    let unit = scale();
    let group_scaled = wrap_u256(BigUint::from(group_count) * &unit);
    let tribute_scaled = wrap_u256(BigUint::from(tribute_count) * &unit);
    let group_root = fp_root(&group_scaled, 5)?;
    let tribute_root = fp_root(&tribute_scaled, 10)?;
    Ok(wrap_u256(group_root * tribute_root) / unit)
}

fn policy_tau(
    populations: &[usize],
    tribute_count: usize,
) -> Result<Vec<BigUint>, ReferenceFailure> {
    let unit = scale();
    let group_count = populations.len();
    let mut tau = vec![BigUint::zero(); group_count + 1];
    for index in 1..group_count {
        let current = populations[index];
        let previous = populations[index - 1];
        tau[index] = if current != 0 && previous != 0 {
            let half_index = wrap_u256(BigUint::from(2 * index - 1) * &unit) / BigUint::from(2_u8);
            let numerator = fp_root(&half_index, 5)?;
            let current_root = fp_root(&wrap_u256(BigUint::from(current) * &unit), 10)?;
            let previous_root = fp_root(&wrap_u256(BigUint::from(previous) * &unit), 10)?;
            let divisor = current_root.min(previous_root);
            if divisor.is_zero() {
                fallback_tau(group_count, tribute_count)?
            } else {
                wrap_u256(numerator * &unit) / divisor
            }
        } else {
            fallback_tau(group_count, tribute_count)?
        };
    }
    let mut middle_sum = BigUint::zero();
    for value in &tau[1..group_count] {
        middle_sum = wrap_u256(middle_sum + value);
    }
    let policy_c = &unit * 2_u8 / 10_u8;
    tau[0] = wrap_u256(&policy_c * &middle_sum) / &unit;
    tau[group_count] = wrap_u256((&unit - policy_c) * middle_sum) / unit;
    Ok(tau)
}

fn distribution(
    shares: &[BigUint],
    populations: &[usize],
    tribute_count: usize,
    target_fraction: &BigUint,
    maximum_fraction: &BigUint,
) -> Result<Vec<BigUint>, ReferenceFailure> {
    let unit = scale();
    if populations.is_empty() {
        return Ok(vec![BigUint::zero()]);
    }
    if populations.len() == 1 {
        return Ok(vec![target_fraction.clone()]);
    }

    let tau = policy_tau(populations, tribute_count)?;
    let mut tau_total = BigUint::zero();
    for value in &tau {
        tau_total = wrap_u256(tau_total + value);
    }
    let masses = tau
        .iter()
        .map(|value| {
            if tau_total.is_zero() {
                BigUint::zero()
            } else {
                wrap_u256(value * &unit) / &tau_total
            }
        })
        .collect::<Vec<_>>();

    let mut cumulative = vec![BigUint::zero(); shares.len() + 1];
    let mut running = BigUint::zero();
    for (index, value) in shares.iter().enumerate() {
        running = wrap_u256(running + value);
        cumulative[index + 1] = running.clone();
    }
    *cumulative.last_mut().expect("cumulative is non-empty") = unit.clone();

    let mut expected = BigUint::zero();
    let mut expected_square = BigUint::zero();
    for (mass, value) in masses.iter().zip(&cumulative) {
        expected = wrap_u256(expected + wrap_u256(mass * value) / &unit);
        let squared = wrap_u256(wrap_u256(mass * value) * value);
        let scale_square = wrap_u256(&unit * &unit);
        expected_square = wrap_u256(expected_square + squared / scale_square);
    }
    let expected_term = wrap_u256(&expected * &expected) / &unit;
    let variance = if expected_square > expected_term {
        &expected_square - &expected_term
    } else {
        BigUint::zero()
    };

    let fraction_ratio = if maximum_fraction.is_zero() {
        BigUint::zero()
    } else {
        wrap_u256(target_fraction * &unit) / maximum_fraction
    };
    let expected_i = to_i256(&expected)?;
    let beta_numerator = to_i256(&fraction_ratio)? - &expected_i;
    let beta_denominator = to_i256(&variance)?;
    let maximum_i = to_i256(maximum_fraction)?;
    let scale_i = BigInt::from(unit.clone());

    let mut fractions = vec![BigUint::zero(); shares.len()];
    for index in 1..=shares.len() {
        let mut tail_sum = BigInt::zero();
        for tail in index..masses.len() {
            let difference = to_i256(&cumulative[tail])? - &expected_i;
            let beta_term = if beta_denominator.is_positive() {
                trunc_signed_div(&(&beta_numerator * difference), &beta_denominator)
            } else {
                BigInt::zero()
            };
            let factor = &scale_i + beta_term;
            tail_sum += trunc_signed_div(&(to_i256(&masses[tail])? * factor), &scale_i);
        }
        let result = trunc_signed_div(&(&maximum_i * tail_sum), &scale_i);
        if result.is_positive() {
            fractions[index - 1] = result
                .to_biguint()
                .expect("positive signed result converts to unsigned");
        }
    }

    let mut weighted_total = BigUint::zero();
    for (fraction, share) in fractions.iter().zip(shares) {
        weighted_total = wrap_u256(weighted_total + wrap_u256(fraction * share) / &unit);
    }
    if weighted_total > *target_fraction {
        for value in &mut fractions {
            *value = wrap_u256(&*value * target_fraction) / &weighted_total;
        }
    }
    Ok(fractions)
}

fn fraction_table(
    tributes: &[CorpusTribute],
    total_nominal: &BigUint,
    allocation: &BigUint,
) -> Result<Vec<Value>, ReferenceFailure> {
    Ok(fraction_table_with_projection(tributes, total_nominal, allocation)?.0)
}

fn fraction_table_with_projection(
    tributes: &[CorpusTribute],
    total_nominal: &BigUint,
    allocation: &BigUint,
) -> Result<(Vec<Value>, BigUint, BigUint), ReferenceFailure> {
    let unit = scale();
    let mut groups = BTreeMap::<u16, Vec<usize>>::new();
    for (ordinal, tribute) in tributes.iter().enumerate() {
        groups
            .entry(
                tribute.f1.ok_or_else(|| {
                    ReferenceFailure::ordinal("FIDELITY_FIRST_UNAVAILABLE", ordinal)
                })?,
            )
            .or_default()
            .push(ordinal);
    }
    let mut shares = Vec::with_capacity(groups.len());
    let mut populations = Vec::with_capacity(groups.len());
    let mut group_nominals = Vec::with_capacity(groups.len());
    for ordinals in groups.values() {
        let mut group_nominal = BigUint::zero();
        for ordinal in ordinals {
            group_nominal = wrap_u256(group_nominal + decimal(&tributes[*ordinal].nominal)?);
        }
        shares.push(wrap_u256(&group_nominal * &unit) / total_nominal);
        populations.push(ordinals.len());
        group_nominals.push(group_nominal);
    }
    let mut share_sum = BigUint::zero();
    for value in &shares {
        share_sum = wrap_u256(share_sum + value);
    }
    if share_sum < unit {
        if let Some(last) = shares.last_mut() {
            *last = wrap_u256(&*last + (&unit - share_sum));
        }
    }
    let target = wrap_u256(allocation * &unit) / total_nominal;
    let maximum = wrap_u256(&target * 2_u8);
    let mut fractions = distribution(&shares, &populations, tributes.len(), &target, &maximum)?;
    let raw_projected = group_nominals
        .iter()
        .zip(&fractions)
        .fold(BigUint::zero(), |sum, (nominal, fraction)| {
            sum + wrap_u256(nominal * fraction) / &unit
        });
    if raw_projected > *allocation {
        for fraction in &mut fractions {
            *fraction = wrap_u256(&*fraction * allocation) / &raw_projected;
        }
    }
    let normalized_projected = group_nominals
        .iter()
        .zip(&fractions)
        .fold(BigUint::zero(), |sum, (nominal, fraction)| {
            sum + wrap_u256(nominal * fraction) / &unit
        });
    let table = groups
        .keys()
        .zip(shares)
        .zip(populations)
        .zip(fractions)
        .map(|(((league, share), population), fraction)| {
            json!({
                "league": league,
                "share": share.to_string(),
                "population": population,
                "fraction": fraction.to_string(),
            })
        })
        .collect();
    Ok((table, raw_projected, normalized_projected))
}

fn validate_and_sort(input: &CorpusInput) -> Result<Vec<CorpusTribute>, ReferenceFailure> {
    let mut tributes = input.tributes.clone();
    tributes.sort_by(|left, right| left.tribute_id.cmp(&right.tribute_id));
    if tributes.is_empty() {
        return Err(ReferenceFailure::new("EMPTY_INPUT"));
    }
    let mut previous: Option<&str> = None;
    for (ordinal, tribute) in tributes.iter().enumerate() {
        let raw_id = tribute.tribute_id.as_str();
        let prefix_day = raw_id
            .get(..8)
            .and_then(|prefix| u32::from_str_radix(prefix, 16).ok());
        if raw_id.len() != 64
            || raw_id != raw_id.to_ascii_lowercase()
            || !raw_id.bytes().all(|byte| byte.is_ascii_hexdigit())
            || prefix_day != Some(input.worldwide_day)
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

fn try_evaluate(case: &CorpusCase) -> Result<Value, ReferenceFailure> {
    let input = &case.input;
    let tributes = validate_and_sort(input)?;
    let mut observations = Vec::new();
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

    let allocation = decimal(&input.gratis_allocation)?;
    let table = fraction_table(&tributes, &total_nominal, &allocation)?;
    let fractions = table
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
        .collect::<BTreeMap<_, _>>();

    let mandatory_price = input
        .mandatory_price_840
        .as_deref()
        .ok_or_else(|| ReferenceFailure::currency("MANDATORY_ORACLE_UNAVAILABLE", 840))
        .and_then(decimal)?;
    if mandatory_price.is_zero() {
        return Err(ReferenceFailure::currency("ZERO_ENTRY_PRICE", 840));
    }
    observations.push(json!({
        "kind": "ORACLE",
        "ordinal": Value::Null,
        "currency": 840,
        "entry_price": mandatory_price.to_string(),
    }));

    let unit = scale();
    let mut remaining = allocation.clone();
    let mut actions = Vec::with_capacity(tributes.len());
    let mut contributors = BTreeMap::<String, BigUint>::new();
    for (ordinal, tribute) in tributes.iter().enumerate() {
        let first = tribute
            .f1
            .expect("first league was validated before fraction lookup");
        let fraction = fractions
            .get(&first)
            .expect("fraction exists for every first league");
        let load = wrap_u256(decimal(&tribute.nominal)? * fraction) / &unit;
        if load.is_zero() {
            return Err(ReferenceFailure::ordinal("ZERO_GRATIS_LOAD", ordinal));
        }
        if load > remaining {
            return Err(ReferenceFailure::ordinal(
                "GRATIS_LOAD_EXCEEDS_REMAINING",
                ordinal,
            ));
        }
        remaining -= &load;

        let currency = tribute.reference_currency;
        let entry_price = if currency == 840 {
            mandatory_price.clone()
        } else {
            let price = tribute
                .conditional_price
                .as_deref()
                .ok_or_else(|| {
                    ReferenceFailure::ordinal_currency(
                        "CONDITIONAL_ORACLE_UNAVAILABLE",
                        ordinal,
                        currency,
                    )
                })
                .and_then(decimal)?;
            if price.is_zero() {
                return Err(ReferenceFailure::ordinal_currency(
                    "ZERO_ENTRY_PRICE",
                    ordinal,
                    currency,
                ));
            }
            observations.push(json!({
                "kind": "ORACLE",
                "ordinal": ordinal,
                "currency": currency,
                "entry_price": price.to_string(),
            }));
            price
        };

        let tribute_price = decimal(&tribute.tribute_price)?;
        let base_price = tribute_price.max(entry_price.clone());
        let floor_price = wrap_u256(base_price * 108_u16) / 100_u8;
        let second = tribute
            .f2
            .ok_or_else(|| ReferenceFailure::ordinal("FIDELITY_SECOND_UNAVAILABLE", ordinal))?;
        observations.push(json!({
            "kind": "FIDELITY",
            "ordinal": ordinal,
            "phase": "SECOND",
            "owner": tribute.owner,
            "league": second,
        }));
        if second != first {
            return Err(ReferenceFailure::ordinal("FIDELITY_MISMATCH", ordinal));
        }
        let cost = wrap_u256(&entry_price * &load) / &unit;
        if cost.is_zero() {
            return Err(ReferenceFailure::ordinal("ZERO_COST", ordinal));
        }
        if !tribute.nod_target_available || owner_is_zero(&tribute.owner) {
            return Err(ReferenceFailure::ordinal("INVALID_NOD_TARGET", ordinal));
        }

        actions.push(json!({
            "source_tribute_id": tribute.tribute_id,
            "owner": tribute.owner,
            "worldwide_day": tribute.worldwide_day,
            "league": second,
            "floor_price": floor_price.to_string(),
            "gratis_load": load.to_string(),
            "entry_price": entry_price.to_string(),
            "cost": cost.to_string(),
            "issuance_currency": tribute.issuance_currency,
            "reference_currency": currency,
            "issued_at": input.logical_evaluation_time,
        }));
        if !tribute.excluded {
            let current = contributors
                .get(&tribute.owner)
                .cloned()
                .unwrap_or_default();
            let updated = checked_add_u256(
                &current,
                &decimal(&tribute.nominal)?,
                ReferenceFailure::ordinal("CONTRIBUTOR_OVERFLOW", ordinal),
            )?;
            contributors.insert(tribute.owner.clone(), updated);
        }
    }

    Ok(json!({
        "status": "SUCCESS",
        "total_nominal": total_nominal.to_string(),
        "gratis_allocation": allocation.to_string(),
        "remaining_gratis": remaining.to_string(),
        "group_table": table,
        "nod_actions": actions,
        "contributors": contributors
            .into_iter()
            .map(|(owner, nominal)| json!({
                "owner": owner,
                "nominal": nominal.to_string(),
            }))
            .collect::<Vec<_>>(),
        "observations": observations,
    }))
}

fn evaluate(case: &CorpusCase) -> Value {
    try_evaluate(case).unwrap_or_else(|failure| failure.envelope())
}

fn load_cases(path: &Path) -> Result<Vec<CorpusCase>> {
    let contents = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("failed reading {}", path.display()))?;
    let mut identifiers = BTreeSet::new();
    let mut cases = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let case: CorpusCase = strict_json(line)
            .wrap_err_with(|| format!("invalid case at {}:{}", path.display(), index + 1))?;
        ensure!(
            identifiers.insert(case.case_id.clone()),
            "duplicate case_id {:?} at line {}",
            case.case_id,
            index + 1
        );
        cases.push(case);
    }
    Ok(cases)
}

pub fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    let mut output = Vec::new();
    write_canonical_value(&value, &mut output)?;
    Ok(output)
}

fn write_canonical_value(value: &Value, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(value) => {
            output.extend_from_slice(if *value { b"true" } else { b"false" });
        }
        Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        Value::String(value) => output.extend_from_slice(serde_json::to_string(value)?.as_bytes()),
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_value(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                output.extend_from_slice(serde_json::to_string(key)?.as_bytes());
                output.push(b':');
                write_canonical_value(value, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

pub fn render(cases_path: &Path) -> Result<Vec<Vec<u8>>> {
    load_cases(cases_path)?
        .into_iter()
        .map(|mut case| {
            case.expected = evaluate(&case);
            canonical_json(&case)
        })
        .collect()
}

pub fn write(cases_path: &Path) -> Result<()> {
    let rendered = render(cases_path)?;
    let temporary = cases_path.with_extension("jsonl.tmp");
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .wrap_err_with(|| format!("failed creating {}", temporary.display()))?;
    for line in rendered {
        output.write_all(&line)?;
        output.write_all(b"\n")?;
    }
    output.sync_all()?;
    drop(output);
    std::fs::rename(&temporary, cases_path)?;
    Ok(())
}

pub fn check(cases_path: &Path, manifest_path: &Path) -> Result<CheckReport> {
    let cases = load_cases(cases_path)?;
    let mut mismatches = Vec::new();
    let mut expected_outputs = Vec::with_capacity(cases.len());
    for case in &cases {
        let actual = evaluate(case);
        expected_outputs.push(actual.clone());
        if case.expected != actual {
            mismatches.push(case.case_id.clone());
        }
    }

    let manifest: Value = strict_json(
        &std::fs::read_to_string(manifest_path)
            .wrap_err_with(|| format!("failed reading {}", manifest_path.display()))?,
    )?;
    let reference_sha256 = sha256_file(&reference_source_path())?;
    let case_file_sha256 = sha256_file(cases_path)?;
    let mut integrity_errors = Vec::new();
    if manifest["case_count"].as_u64() != u64::try_from(cases.len()).ok() {
        integrity_errors.push("case_count".to_owned());
    }
    if manifest["case_file_sha256"].as_str() != Some(&case_file_sha256) {
        integrity_errors.push("case_file_sha256".to_owned());
    }
    if manifest["reference_sha256"].as_str() != Some(&reference_sha256) {
        integrity_errors.push("reference_sha256".to_owned());
    }
    if manifest["reference_path"].as_str() != Some("testing/lysis-v1-reference/src/lib.rs") {
        integrity_errors.push("reference_path".to_owned());
    }
    if cases.iter().any(|case| case.requirements.is_empty()) {
        integrity_errors.push("case_requirements".to_owned());
    }

    let mut output_bytes = Vec::new();
    for value in &expected_outputs {
        output_bytes.extend_from_slice(&canonical_json(value)?);
        output_bytes.push(b'\n');
    }
    let status = if mismatches.is_empty() && integrity_errors.is_empty() {
        "PASS"
    } else {
        "FAIL"
    };
    Ok(CheckReport {
        status: status.to_owned(),
        case_count: cases.len(),
        case_ids: cases.into_iter().map(|case| case.case_id).collect(),
        expected_outputs_sha256: sha256_bytes(&output_bytes),
        reference_sha256,
        case_file_sha256,
        mismatches,
        integrity_errors,
    })
}

pub fn sha256_file(path: &Path) -> Result<String> {
    Ok(sha256_bytes(&std::fs::read(path).wrap_err_with(|| {
        format!("failed reading {}", path.display())
    })?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn strict_json<T: DeserializeOwned>(input: &str) -> Result<T> {
    let strict = serde_json::from_str::<StrictValue>(input)?;
    Ok(serde_json::from_value(strict.0)?)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(StrictValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some((key, value)) = object.next_entry::<String, StrictValue>()? {
            if values.insert(key.clone(), value.0).is_some() {
                return Err(A::Error::custom(format!("duplicate JSON key: {key}")));
            }
        }
        Ok(StrictValue(Value::Object(values)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn six_decimal_projection_normalization_has_the_frozen_eight_unit_dust() {
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
        let allocation = decimal(&case.input.gratis_allocation).unwrap();
        let (_, raw, normalized) =
            fraction_table_with_projection(&tributes, &total_nominal, &allocation).unwrap();

        assert_eq!(allocation, BigUint::from(4_800_000u64));
        assert_eq!(raw, BigUint::from(4_800_034u64));
        assert_eq!(normalized, BigUint::from(4_799_992u64));
        assert_eq!(allocation - normalized, BigUint::from(8u64));
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
}
