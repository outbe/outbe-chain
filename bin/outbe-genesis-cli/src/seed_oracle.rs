//! Validate and seed the Oracle registry and pricing state.

use std::collections::{BTreeMap, BTreeSet};

use alloy_primitives::U256;
use serde_json::Value;

use crate::error::GenesisError;
use crate::seed_codec::{
    SECONDS_PER_DAY, StorageBuilder, address_as_u256, address_bytes, address_pair, asset_address,
    data_slot, is_iso_asset_address, parse_u32, parse_u64, parse_u256, u32_bytes,
    wwd_to_day_timestamp,
};

/// Default overnight policy rates at scale `1e6` (`(iso_code, annual_rate_1e6)`).
pub const DEFAULT_POLICY_RATES: [(u32, u64); 6] = [
    (156, 12_624),
    (344, 23_667),
    (392, 12_280),
    (826, 37_344),
    (840, 38_800),
    (978, 24_410),
];

struct ValidatedPair<'a> {
    idx: u32,
    pair: &'a Value,
    base: String,
    quote: String,
    key_hash: [u8; 40],
}

/// Seed the Oracle precompile storage layout.
pub fn seed_oracle(storage: &mut StorageBuilder, config: &Value) -> Result<(), GenesisError> {
    let pairs = config
        .get("pairs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let validated_pairs = validate_oracle_pairs(pairs)?;
    let cfg = config.get("config").unwrap_or(&Value::Null);
    seed_oracle_config(storage, cfg, pairs.len())?;
    let pair_ids = seed_oracle_registry(storage, config, &validated_pairs)?;
    seed_oracle_scurves(storage, config, &pair_ids)?;
    seed_oracle_currencies(storage, config)?;
    Ok(())
}

fn validate_oracle_pairs(pairs: &[Value]) -> Result<Vec<ValidatedPair<'_>>, GenesisError> {
    let mut pair_keys: BTreeMap<(String, String), [u8; 40]> = BTreeMap::new();
    let mut seen_hashes: BTreeSet<[u8; 40]> = BTreeSet::new();
    let mut validated = Vec::with_capacity(pairs.len());

    for (i, pair) in pairs.iter().enumerate() {
        let idx = (i + 1) as u32;
        let base = value_to_string(pair.get("base"))?;
        let quote = value_to_string(pair.get("quote"))?;
        let base_address = asset_address(&base)?;
        let quote_address = asset_address(&quote)?;
        if base_address == quote_address {
            return Err(GenesisError::validation(format!(
                "oracle pair must contain two different assets: same asset {base}/{quote}"
            )));
        }
        let h = address_pair(&base, &quote)?;
        let key = (base.clone(), quote.clone());
        if pair_keys.contains_key(&key) {
            return Err(GenesisError::validation(format!(
                "duplicate oracle pair: {base}/{quote}"
            )));
        }
        if seen_hashes.contains(&h) {
            return Err(GenesisError::validation(format!(
                "oracle pair already registered inverted: {base}/{quote}"
            )));
        }
        if is_iso_asset_address(&base_address) && quote_address == [0u8; 20] {
            return Err(GenesisError::validation(format!(
                "oracle COEN/ISO pair must use COEN base and ISO quote: {base}/{quote}"
            )));
        }
        pair_keys.insert(key, h);
        seen_hashes.insert(h);
        validated.push(ValidatedPair {
            idx,
            pair,
            base,
            quote,
            key_hash: h,
        });
    }
    Ok(validated)
}

fn seed_oracle_config(
    storage: &mut StorageBuilder,
    cfg: &Value,
    pair_count: usize,
) -> Result<(), GenesisError> {
    let reward_band = cfg
        .get("reward_band")
        .map(parse_u256)
        .transpose()?
        .unwrap_or_else(|| U256::from(20_000_000_000_000_000u64));
    let penalties_enabled = cfg
        .get("penalties_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let (min_valid_per_window, slash_fraction) = if penalties_enabled {
        let min_valid = cfg
            .get("min_valid_per_window")
            .map(parse_u256)
            .transpose()?
            .unwrap_or_else(|| U256::from(50_000_000_000_000_000u64));
        let slash = cfg
            .get("slash_fraction")
            .map(parse_u256)
            .transpose()?
            .unwrap_or(U256::ZERO);
        (min_valid, slash)
    } else {
        (U256::ZERO, U256::ZERO)
    };
    let one_e18 = U256::from(1_000_000_000_000_000_000u64);
    for (field, value) in [
        ("reward_band", reward_band),
        ("min_valid_per_window", min_valid_per_window),
        ("slash_fraction", slash_fraction),
    ] {
        if value > one_e18 {
            return Err(GenesisError::validation(format!(
                "oracle.config.{field} {value} exceeds 1e18 (a ratio above 1)"
            )));
        }
    }
    let vote_period = cfg
        .get("vote_period")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(2);
    let slash_window = cfg
        .get("slash_window")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(96);
    let lookback_duration = cfg
        .get("lookback_duration")
        .map(parse_u64)
        .transpose()?
        .unwrap_or(SECONDS_PER_DAY);
    let enabled = cfg.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let initialized = cfg
        .get("initialized")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    storage.set_slot(0u64, vote_period);
    storage.set_slot(1u64, reward_band);
    storage.set_slot(2u64, slash_window);
    storage.set_slot(3u64, min_valid_per_window);
    storage.set_slot(4u64, slash_fraction);
    storage.set_slot(5u64, lookback_duration);
    storage.set_slot(6u64, u64::from(enabled));
    storage.set_slot(7u64, u64::from(initialized));
    storage.set_slot(8u64, pair_count);
    Ok(())
}

fn seed_oracle_registry(
    storage: &mut StorageBuilder,
    config: &Value,
    validated_pairs: &[ValidatedPair<'_>],
) -> Result<BTreeMap<(String, String), u32>, GenesisError> {
    let mut pair_ids: BTreeMap<(String, String), u32> = BTreeMap::new();
    for vp in validated_pairs {
        pair_ids.insert((vp.base.clone(), vp.quote.clone()), vp.idx);
        storage.set_mapping(10, &vp.key_hash, vp.idx);
        let vote_target = vp
            .pair
            .get("vote_target")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        storage.set_mapping(11, &vp.key_hash, u64::from(vote_target));
        storage.set_mapping_pair(43, &u32_bytes(vp.idx), &vp.base, &vp.quote)?;

        let rate = vp
            .pair
            .get("initial_rate")
            .map(parse_u256)
            .transpose()?
            .unwrap_or(U256::ZERO);
        if !rate.is_zero() {
            let block = vp
                .pair
                .get("initial_block")
                .map(parse_u64)
                .transpose()?
                .unwrap_or(0);
            let ts = vp
                .pair
                .get("initial_timestamp")
                .map(parse_u64)
                .transpose()?
                .unwrap_or(0);
            storage.set_mapping(12, &u32_bytes(vp.idx), rate);
            storage.set_mapping(13, &u32_bytes(vp.idx), block);
            storage.set_mapping(14, &u32_bytes(vp.idx), ts);
        }
    }

    if let Some(initial_rates) = config.get("initial_rates").and_then(Value::as_array) {
        for rate_entry in initial_rates {
            let base = value_to_string(rate_entry.get("base"))?;
            let quote = value_to_string(rate_entry.get("quote"))?;
            let Some(&idx) = pair_ids.get(&(base.clone(), quote.clone())) else {
                return Err(GenesisError::validation(format!(
                    "initial rate pair is not registered: {base}/{quote}"
                )));
            };
            let rate = rate_entry
                .get("rate")
                .ok_or_else(|| GenesisError::validation("missing initial_rates.rate"))
                .and_then(parse_u256)?;
            let block = rate_entry
                .get("block")
                .map(parse_u64)
                .transpose()?
                .unwrap_or(0);
            let ts = rate_entry
                .get("timestamp")
                .map(parse_u64)
                .transpose()?
                .unwrap_or(0);
            storage.set_mapping(12, &u32_bytes(idx), rate);
            storage.set_mapping(13, &u32_bytes(idx), block);
            storage.set_mapping(14, &u32_bytes(idx), ts);
        }
    }

    if let Some(delegations) = config.get("feeder_delegations").and_then(Value::as_array) {
        for delegation in delegations {
            let validator = value_to_string(delegation.get("validator"))?;
            let feeder = value_to_string(delegation.get("feeder"))?;
            storage.set_mapping(15, &address_bytes(&validator)?, address_as_u256(&feeder)?);
        }
    }

    if is_truthy(config.get("protected_validators")) {
        return Err(GenesisError::validation(
            "oracle.protected_validators was removed: every validator follows the same slash-window rules",
        ));
    }
    if is_truthy(config.get("settlement_currencies")) {
        return Err(GenesisError::validation(
            "oracle.settlement_currencies was removed: register the COEN/<iso> pair instead, and list the ISO under oracle.reference_currencies",
        ));
    }
    Ok(pair_ids)
}

fn seed_oracle_scurves(
    storage: &mut StorageBuilder,
    config: &Value,
    pair_ids: &BTreeMap<(String, String), u32>,
) -> Result<(), GenesisError> {
    let Some(scurve_seeds) = config.get("scurve_seeds").and_then(Value::as_array) else {
        return Ok(());
    };
    if scurve_seeds.is_empty() {
        return Ok(());
    }
    storage.set_slot(34u64, scurve_seeds.len());
    storage.set_slot(38u64, 0u64);
    for (idx, sc) in scurve_seeds.iter().enumerate() {
        let base = value_to_string(sc.get("pair_base"))?;
        let quote = value_to_string(sc.get("pair_quote"))?;
        if !pair_ids.contains_key(&(base.clone(), quote.clone())) {
            return Err(GenesisError::validation(format!(
                "scurve seed pair is not registered: {base}/{quote}"
            )));
        }
        let peak_day = sc
            .get("peak_day")
            .ok_or_else(|| GenesisError::validation("missing scurve peak_day"))
            .and_then(parse_u32)?;
        let peak_day_ts = wwd_to_day_timestamp(peak_day)?;
        let peak_price = sc
            .get("peak_price")
            .ok_or_else(|| GenesisError::validation("missing scurve peak_price"))
            .and_then(parse_u256)?;
        let idx_u32 = idx as u32;
        storage.set_mapping_pair(35, &u32_bytes(idx_u32), &base, &quote)?;
        storage.set_mapping(36, &u32_bytes(idx_u32), peak_day_ts);
        storage.set_mapping(37, &u32_bytes(idx_u32), peak_price);
    }
    Ok(())
}

fn seed_oracle_currencies(
    storage: &mut StorageBuilder,
    config: &Value,
) -> Result<(), GenesisError> {
    let reference_currencies = seed_reference_currencies(storage, config)?;
    seed_policy_rates(storage, config, &reference_currencies)
}

fn seed_reference_currencies(
    storage: &mut StorageBuilder,
    config: &Value,
) -> Result<Vec<u32>, GenesisError> {
    let reference_currencies: Vec<u32> = match config
        .get("reference_currencies")
        .and_then(Value::as_array)
    {
        Some(arr) => arr.iter().map(parse_u32).collect::<Result<Vec<_>, _>>()?,
        None => vec![156, 344, 392, 826, 840, 978],
    };
    if reference_currencies.contains(&0) {
        return Err(GenesisError::validation(
            "oracle reference iso_code must be non-zero",
        ));
    }
    if !is_sorted_unique(&reference_currencies) {
        return Err(GenesisError::validation(
            "oracle reference currencies must be sorted and unique",
        ));
    }
    if !reference_currencies.contains(&840) {
        return Err(GenesisError::validation(
            "oracle reference currencies must include USD 840",
        ));
    }
    storage.set_slot(55u64, reference_currencies.len());
    for (i, &iso_code) in reference_currencies.iter().enumerate() {
        storage.set_raw_slot(data_slot(55u64) + U256::from(i), iso_code);
    }
    Ok(reference_currencies)
}

fn seed_policy_rates(
    storage: &mut StorageBuilder,
    config: &Value,
    reference_currencies: &[u32],
) -> Result<(), GenesisError> {
    let parsed_policy_rates: Vec<(u32, u64)> =
        match config.get("policy_rates").and_then(Value::as_array) {
            Some(arr) => arr
                .iter()
                .map(|entry| {
                    let iso = entry
                        .get("iso_code")
                        .ok_or_else(|| GenesisError::validation("missing policy_rates.iso_code"))
                        .and_then(parse_u32)?;
                    let rate = entry
                        .get("annual_rate_1e6")
                        .ok_or_else(|| {
                            GenesisError::validation("missing policy_rates.annual_rate_1e6")
                        })
                        .and_then(parse_u64)?;
                    Ok((iso, rate))
                })
                .collect::<Result<Vec<_>, GenesisError>>()?,
            None => DEFAULT_POLICY_RATES.to_vec(),
        };
    validate_policy_rates(&parsed_policy_rates, reference_currencies)?;
    storage.set_slot(74u64, parsed_policy_rates.len());
    for (i, &(iso_code, rate)) in parsed_policy_rates.iter().enumerate() {
        storage.set_raw_slot(data_slot(74u64) + U256::from(i), iso_code);
        storage.set_mapping(75, &u32_bytes(iso_code), rate);
    }
    Ok(())
}

fn validate_policy_rates(
    parsed_policy_rates: &[(u32, u64)],
    reference_currencies: &[u32],
) -> Result<(), GenesisError> {
    if parsed_policy_rates.iter().any(|&(iso, _)| iso == 0) {
        return Err(GenesisError::validation(
            "oracle policy iso_code must be non-zero",
        ));
    }
    if parsed_policy_rates.iter().any(|&(_, rate)| rate == 0) {
        return Err(GenesisError::validation(
            "oracle policy rate must be non-zero",
        ));
    }
    let policy_isos: Vec<u32> = parsed_policy_rates.iter().map(|&(iso, _)| iso).collect();
    if !is_sorted_unique(&policy_isos) {
        return Err(GenesisError::validation(
            "oracle policy rates must be sorted and unique",
        ));
    }
    let missing: Vec<u32> = reference_currencies
        .iter()
        .copied()
        .filter(|iso| !policy_isos.contains(iso))
        .collect();
    if !missing.is_empty() {
        return Err(GenesisError::validation(format!(
            "oracle reference currencies have no policy rate: {missing:?}"
        )));
    }
    Ok(())
}

fn is_sorted_unique(items: &[u32]) -> bool {
    items.windows(2).all(|w| w[0] < w[1])
}

fn value_to_string(val: Option<&Value>) -> Result<String, GenesisError> {
    match val {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Number(n)) => Ok(n.to_string()),
        _ => Err(GenesisError::validation("expected string or number value")),
    }
}

fn is_truthy(val: Option<&Value>) -> bool {
    match val {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(m)) => !m.is_empty(),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_i64() != Some(0),
    }
}
