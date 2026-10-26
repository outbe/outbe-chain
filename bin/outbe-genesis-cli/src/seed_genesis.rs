//! Seed `genesis.json` with precompile storage entries for `outbe-chain`.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Map, Value};

use crate::error::GenesisError;
use crate::inputs::SeedOptions;
use crate::network::SEED_SECTIONS;
use crate::seed_alloc::{
    seed_external_contracts, seed_protocol_constants, validate_stablecoin_namespace_alloc,
};
use crate::seed_codec::{
    parse_header_timestamp, seed_profile_selector, timestamp_to_utc_date_key, StorageBuilder,
    ALL_PRECOMPILE_ADDRESSES, CREDIS_ADDRESS, CREDIS_PROFILE_SLOT, CYCLE_ADDRESS, GEM_ADDRESS,
    GEM_PROFILE_SLOT, INTEX_ADDRESS, INTEX_PROFILE_SLOT, MARKER_CODE, NOD_ADDRESS,
    NOD_PROFILE_SLOT,
};
use crate::seed_economy::{
    clear_seeded_metadosis_days, merge_storage_into_alloc, override_worldwide_day, seed_coen,
    seed_cycle, seed_economic_state,
};
use crate::seed_system::seed_system_state;
use crate::seed_validators::seed_founders;

const PROFILE_SELECTOR_TARGETS: &[(&str, &str, u64, &str)] = &[
    ("intex_profile", INTEX_ADDRESS, INTEX_PROFILE_SLOT, "Intex"),
    ("gem_profile", GEM_ADDRESS, GEM_PROFILE_SLOT, "Gem"),
    ("nod_profile", NOD_ADDRESS, NOD_PROFILE_SLOT, "Nod"),
    (
        "credis_profile",
        CREDIS_ADDRESS,
        CREDIS_PROFILE_SLOT,
        "Credis",
    ),
];

/// Compute every derived value a genesis needs and write it into `genesis`.
pub fn apply_seed(
    genesis: &mut Value,
    seed: &Value,
    validators: Option<&[Value]>,
    options: Option<&SeedOptions>,
) -> Result<Value, GenesisError> {
    let default_options = SeedOptions::default();
    let options = options.unwrap_or(&default_options);
    let mut seed_owned = seed.clone();

    seed_protocol_constants(genesis, &seed_owned)?;

    if let Some(worldwide_day) = options.worldwide_day {
        override_worldwide_day(&mut seed_owned, worldwide_day);
    }
    if options.fresh_metadosis {
        clear_seeded_metadosis_days(&mut seed_owned)?;
    }

    let validators = validators.unwrap_or(&[]);

    validate_seed_keys(&seed_owned)?;

    {
        let alloc = ensure_object_field(genesis, "alloc")?;
        validate_stablecoin_namespace_alloc(alloc, false)?;
        ensure_precompile_markers(alloc);
    }

    let header_timestamp = parse_header_timestamp(genesis)?;
    let mut cycle_storage = StorageBuilder::new();
    seed_cycle(&mut cycle_storage, header_timestamp);
    {
        let alloc = ensure_object_field(genesis, "alloc")?;
        merge_storage_into_alloc(alloc, CYCLE_ADDRESS, &cycle_storage)?;
    }
    println!(
        "  Cycle: active_utc_day={}, slot 2 seeded",
        timestamp_to_utc_date_key(header_timestamp)
    );

    if let Some(balances) = seed_owned.get("balance").and_then(Value::as_object) {
        let alloc = ensure_object_field(genesis, "alloc")?;
        seed_coen(alloc, balances)?;
        println!("  balance: {} entries", balances.len());
    }

    if !validators.is_empty() {
        let genesis_snapshot = genesis.clone();
        let alloc = ensure_object_field(genesis, "alloc")?;
        seed_founders(&genesis_snapshot, &seed_owned, validators, alloc)?;
    }

    {
        let mut genesis_snapshot = genesis.clone();
        {
            let alloc = ensure_object_field(genesis, "alloc")?;
            seed_system_state(
                &mut genesis_snapshot,
                &seed_owned,
                alloc,
                options.canon_dir.as_deref(),
            )?;
        }
        if let Some(cfg) = genesis_snapshot.get("config").cloned() {
            if let Some(obj) = genesis.as_object_mut() {
                obj.insert("config".to_owned(), cfg);
            }
        }
        let alloc = ensure_object_field(genesis, "alloc")?;
        seed_economic_state(&seed_owned, alloc)?;
        let contracts_path = options.contracts_dir.as_deref().map(Path::new);
        seed_contract_alloc(alloc, &seed_owned, contracts_path)?;
    }

    Ok(genesis.clone())
}

fn validate_seed_keys(seed: &Value) -> Result<(), GenesisError> {
    let Some(seed_obj) = seed.as_object() else {
        return Err(GenesisError::validation("seed must be a mapping"));
    };

    for unsupported_key in ["gratis_balances", "promis_balances", "gems"] {
        if seed_obj.contains_key(unsupported_key) {
            return Err(GenesisError::validation(format!(
                "{unsupported_key} is no longer supported at genesis: issue the Gem at \
                 runtime and mine Gem -> Promis -> Gratis"
            )));
        }
    }
    if seed_obj.contains_key("intex_factory") {
        return Err(GenesisError::validation(
            "intex_factory was renamed to intex_profile",
        ));
    }

    let mut allowed: BTreeSet<&str> = SEED_SECTIONS.iter().copied().collect();
    allowed.insert("hyperlane_controller");
    let unknown: Vec<&str> = seed_obj
        .keys()
        .map(String::as_str)
        .filter(|key| !allowed.contains(key))
        .collect();
    if !unknown.is_empty() {
        let allowed_vec: Vec<&str> = allowed.into_iter().collect();
        return Err(GenesisError::validation(format!(
            "unknown key(s): {}; allowed: {}",
            unknown.join(", "),
            allowed_vec.join(", ")
        )));
    }
    Ok(())
}

fn ensure_precompile_markers(alloc: &mut Map<String, Value>) {
    for addr in ALL_PRECOMPILE_ADDRESSES {
        let entry = alloc
            .entry(addr.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(entry_obj) = entry.as_object_mut() {
            entry_obj.insert("code".to_owned(), Value::String(MARKER_CODE.to_owned()));
            entry_obj
                .entry("balance".to_owned())
                .or_insert_with(|| Value::String("0x0".to_owned()));
        }
    }
}

/// Seed profile selectors, external contracts, and default zero balances into `alloc`.
pub fn seed_contract_alloc(
    alloc: &mut Map<String, Value>,
    seed: &Value,
    contracts_dir: Option<&Path>,
) -> Result<(), GenesisError> {
    for &(section, address, slot, label) in PROFILE_SELECTOR_TARGETS {
        let Some(cfg) = seed.get(section) else {
            continue;
        };
        let mut profile_storage = StorageBuilder::new();
        seed_profile_selector(&mut profile_storage, cfg, section, slot)?;
        if !profile_storage.entries.is_empty() {
            let count = profile_storage.entries.len();
            merge_storage_into_alloc(alloc, address, &profile_storage)?;
            if let Some(entry_obj) = alloc.get_mut(address).and_then(Value::as_object_mut) {
                entry_obj
                    .entry("code".to_owned())
                    .or_insert_with(|| Value::String(MARKER_CODE.to_owned()));
            }
            println!("  {label}: {count} storage entries");
        }
    }

    if let Some(contracts) = seed.get("contracts") {
        let Some(dir) = contracts_dir else {
            return Err(GenesisError::validation(
                "contracts_dir is required to seed `contracts`",
            ));
        };
        let Some(contracts_list) = contracts.as_array() else {
            return Err(GenesisError::validation("contracts must be a list"));
        };
        seed_external_contracts(alloc, contracts_list, dir)?;
    }

    for account in alloc.values_mut() {
        if let Some(account_obj) = account.as_object_mut() {
            account_obj
                .entry("balance".to_owned())
                .or_insert_with(|| Value::String("0x0".to_owned()));
        }
    }

    validate_stablecoin_namespace_alloc(alloc, true)
}

fn ensure_object_field<'a>(
    root: &'a mut Value,
    key: &str,
) -> Result<&'a mut Map<String, Value>, GenesisError> {
    let Some(root_obj) = root.as_object_mut() else {
        return Err(GenesisError::validation("genesis must be a JSON object"));
    };
    let field = root_obj
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    field
        .as_object_mut()
        .ok_or_else(|| GenesisError::validation(format!("{key} must be a JSON object")))
}
