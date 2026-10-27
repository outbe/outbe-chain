//! Orchestrate full genesis creation from `network.yaml` and execute the `seed` CLI workflow.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::attestation::run_tee_stage;
use crate::error::GenesisError;
use crate::inputs::{SeedInputs, SeedOptions};
use crate::keys::discover_validators;
use crate::launch_bundle;
use crate::network::{default_contracts_dir, network_identity, resolve_binary, validate_config};
use crate::stages::{
    build_base_genesis, build_seed, prefund_validators, run_seed_stage, seed_genesis_from_config,
};
use crate::yaml::load_yaml;

/// Execute the full `create_genesis` pipeline from `config_path`.
pub fn run_create_genesis(
    config_path: &Path,
    output_override: Option<&Path>,
) -> Result<PathBuf, GenesisError> {
    run_create_genesis_with_min_stake(config_path, output_override, None)
}

/// Execute the full `create_genesis` pipeline from `config_path` with an optional `min_stake` override.
pub fn run_create_genesis_with_min_stake(
    config_path: &Path,
    output_override: Option<&Path>,
    min_stake_override: Option<&str>,
) -> Result<PathBuf, GenesisError> {
    let resolved_config_path =
        fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());
    let config_dir = resolved_config_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let mut config = load_yaml(config_path)?;
    if let Some(min_stake) = min_stake_override {
        apply_min_stake_override(&mut config, min_stake);
    }
    validate_config(&config)?;
    let (network, chain_id, chain_name) = network_identity(&config)?;

    let keys_dir_raw = config
        .get("keys_dir")
        .and_then(Value::as_str)
        .ok_or_else(|| GenesisError::validation("missing required key: keys_dir"))?;
    let keys_dir_candidate = relative_to_dir(config_dir, keys_dir_raw);
    let keys_dir = fs::canonicalize(&keys_dir_candidate).unwrap_or(keys_dir_candidate);
    if !keys_dir.is_dir() {
        return Err(GenesisError::validation(format!(
            "keys_dir is not a directory: {}",
            keys_dir.display()
        )));
    }

    let output = match output_override {
        Some(path) => path.to_path_buf(),
        None => {
            let out_str = config
                .get("output")
                .and_then(Value::as_str)
                .unwrap_or("genesis.json");
            relative_to_dir(config_dir, out_str)
        }
    };
    if output.exists() {
        return Err(GenesisError::validation(format!(
            "refusing to overwrite existing genesis: {}",
            output.display()
        )));
    }
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| GenesisError::io(parent, err))?;
        }
    }

    let chain_binary = resolve_binary(&config, "chain_binary", "outbe-chain")?;
    let keygen_binary = resolve_binary(&config, "keygen_binary", "outbe-keygen")?;
    let validators = discover_validators(&config, &keys_dir, &keygen_binary)?;

    println!("Founding validators:");
    for (index, validator) in validators.iter().enumerate() {
        let p2p = validator
            .get("p2p_address")
            .and_then(Value::as_str)
            .unwrap_or("");
        let addr = validator
            .get("address")
            .and_then(Value::as_str)
            .unwrap_or("");
        println!("  validator-{index}  {p2p}  {addr}");
    }
    println!();

    let mut base_genesis = build_base_genesis(&config)?;
    prefund_validators(&mut base_genesis, &config, &validators)?;
    let seed = build_seed(&config)?;

    {
        let temp_dir = tempfile::Builder::new()
            .prefix("outbe-genesis-")
            .tempdir()
            .map_err(|err| GenesisError::io(std::env::temp_dir(), err))?;
        let inputs = SeedInputs {
            base_genesis,
            seed,
            validators: validators.clone(),
        };
        let seeded = run_seed_stage(temp_dir.path(), &inputs, &config)?;
        run_tee_stage(&chain_binary, &seeded, &output, &config)?;
    }

    launch_bundle::render(&config, &validators, &output, &keys_dir)?;

    let output_parent = output.parent().unwrap_or_else(|| Path::new("."));
    let identity_path = output_parent.join("network-identity.json");
    let identity_json = serde_json::to_string_pretty(&json!({
        "network": network,
        "chainId": chain_id,
        "chainName": chain_name,
    }))?;
    fs::write(&identity_path, format!("{identity_json}\n"))
        .map_err(|err| GenesisError::io(&identity_path, err))?;

    println!();
    println!("genesis:         {}", output.display());
    println!(
        "bootnodes:       {}",
        output_parent.join("reth-bootnodes.txt").display()
    );
    println!("launch scripts:  {}/validator-N/", output_parent.display());
    println!(
        "instructions:    {}",
        output_parent.join("DEPLOY.md").display()
    );
    println!("network identity:{}", identity_path.display());

    Ok(output)
}

/// CLI arguments for `seed_genesis` mode.
#[derive(Debug, Clone)]
pub struct SeedCliArgs {
    /// Path to `genesis.json`.
    pub genesis: PathBuf,
    /// Path to seed config JSON.
    pub seed: PathBuf,
    /// Optional path to `validators.json`.
    pub validators: Option<PathBuf>,
    /// Output path for patched `genesis.json`.
    pub output: PathBuf,
    /// Optional directory containing external contract files.
    pub contracts_dir: Option<PathBuf>,
    /// Optional directory containing `metacanon.md` and `canon.md`.
    pub canon_dir: Option<PathBuf>,
    /// Optional WorldwideDay override (`YYYYMMDD`).
    pub worldwide_day: Option<u32>,
    /// Do not seed an already-OFFERING WorldwideDay.
    pub fresh_metadosis: bool,
}

/// Execute the `seed_genesis` CLI pipeline.
pub fn run_seed_cli(args: &SeedCliArgs) -> Result<(), GenesisError> {
    run_seed_cli_with_min_stake(args, None)
}

/// Execute the `seed_genesis` CLI pipeline with an optional `min_stake` override.
pub fn run_seed_cli_with_min_stake(
    args: &SeedCliArgs,
    min_stake_override: Option<&str>,
) -> Result<(), GenesisError> {
    let genesis_raw =
        fs::read_to_string(&args.genesis).map_err(|err| GenesisError::io(&args.genesis, err))?;
    let base_genesis: Value = serde_json::from_str(&genesis_raw)?;

    let seed_raw =
        fs::read_to_string(&args.seed).map_err(|err| GenesisError::io(&args.seed, err))?;
    let mut seed: Value = serde_json::from_str(&seed_raw)?;
    if let Some(min_stake) = min_stake_override {
        apply_min_stake_override(&mut seed, min_stake);
    }

    let validators: Vec<Value> = match &args.validators {
        Some(path) => {
            let raw = fs::read_to_string(path).map_err(|err| GenesisError::io(path, err))?;
            serde_json::from_str(&raw)?
        }
        None => Vec::new(),
    };

    let contracts_dir = args
        .contracts_dir
        .as_ref()
        .map(|p| p.display().to_string())
        .or_else(|| {
            let seed_abs = fs::canonicalize(&args.seed).unwrap_or_else(|_| args.seed.clone());
            let sibling = seed_abs
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("contracts");
            if sibling.is_dir() {
                Some(sibling.display().to_string())
            } else {
                Some(default_contracts_dir().display().to_string())
            }
        });
    let canon_dir = args.canon_dir.as_ref().map(|p| p.display().to_string());

    let genesis = seed_genesis_from_config(
        &SeedInputs {
            base_genesis,
            seed,
            validators,
        },
        &SeedOptions {
            contracts_dir,
            canon_dir,
            worldwide_day: args.worldwide_day,
            fresh_metadosis: args.fresh_metadosis,
        },
    )?;

    if let Some(parent) = args.output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| GenesisError::io(parent, err))?;
        }
    }
    let formatted = serde_json::to_string_pretty(&genesis)?;
    fs::write(&args.output, formatted).map_err(|err| GenesisError::io(&args.output, err))?;

    let total_storage: usize = genesis
        .get("alloc")
        .and_then(Value::as_object)
        .map(|alloc| {
            alloc
                .values()
                .filter_map(|v| v.get("storage").and_then(Value::as_object))
                .map(serde_json::Map::len)
                .sum()
        })
        .unwrap_or(0);

    println!("\nGenesis written to {}", args.output.display());
    println!("Total storage entries: {total_storage}");
    Ok(())
}

fn apply_min_stake_override(root: &mut Value, min_stake: &str) {
    if let Some(obj) = root.as_object_mut() {
        let staking = obj
            .entry("staking".to_owned())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(staking_obj) = staking.as_object_mut() {
            staking_obj.insert("min_stake".to_owned(), Value::String(min_stake.to_owned()));
        }
    }
}

fn relative_to_dir(base: &Path, value: &str) -> PathBuf {
    let candidate = Path::new(value);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
}
