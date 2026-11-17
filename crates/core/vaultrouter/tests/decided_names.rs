//! OIP-00392 gap tests for the decided names and the place of the ABI binding.
//! They read the sources of the vault router and of the three stablecoin crates.

use std::fs;
use std::path::{Path, PathBuf};

/// The crates of OIP-00392 and the interface that each crate binds.
const CRATES: [(&str, &str); 4] = [
    ("vaultrouter", "IVaultRouter"),
    ("stablecoin", "IStablecoin"),
    ("stablecoinfactory", "IStablecoinFactory"),
    ("stablecoinpolicy", "IStablecoinPolicyRegistry"),
];

/// The old names of OIP-00392, split in parts. The parts keep the old names out
/// of this file, so the old-name `rg` check of the Issue finds no match here.
/// The Credis reservation names (OIP-00385) are not in this list.
const OLD_NAMES: &[&[&str]] = &[
    &["Stables", "Source"],
    &["Stables", "Target"],
    &["Liquidity", "Source"],
    &["Liquidity", "Target"],
    &["Liquidity", "Deposited"],
    &["Liquidity", "Withdrawn"],
    &["Liquidity", "Rebalanced"],
    &["Invalid", "Liquidity"],
    &["liquidity", "Source"],
    &["liquidity", "Target"],
    &["liquidity", "_source"],
    &["liquidity", "_target"],
    &["has", "Liquidity"],
    &["has_", "liquidity"],
    &["assetsWithdrawn", "Minor"],
    &["assetsDeposited", "Minor"],
    &["Approved", "VaultAction"],
    &["approved", "VaultAction"],
    &["approved_", "vault_action"],
    &["apply", "Approved"],
    &["apply_", "approved"],
    &["approved", "At"],
    &["set", "SupplyCap"],
    &["set", "PolicyId"],
    &["begin", "AdminTransfer"],
    &["begin", "PolicyAdminTransfer"],
    &["set_", "supply_cap"],
    &["set_", "policy_id"],
    &["begin_", "admin_transfer"],
    &["begin_", "policy_admin_transfer"],
    &["execute_", "approved"],
];

/// The new names that each interface must declare.
const INTERFACE_NAMES: &[(&str, &[&str])] = &[
    (
        "IVaultRouter",
        &[
            "enum StablecoinSource",
            "enum StablecoinTarget",
            "event VaultActionCreated",
            "event VaultActionApplied",
            "event VaultActionExpired",
            "event VaultActionCanceled",
            "event StablecoinSourceAdded",
            "event StablecoinSourceRemoved",
            "event StablecoinTargetAdded",
            "event StablecoinTargetRemoved",
            "event StablecoinDeposited",
            "event StablecoinWithdrawn",
            "event StablecoinRebalanced",
            "error InvalidStablecoinSource",
            "error InvalidStablecoinTarget",
            "error StablecoinSourceNotFound",
            "error StablecoinTargetNotFound",
            "error VaultActionNotFound",
            "error VaultActionIndexInconsistent",
            "error InvalidVaultAction",
            "function stablecoinSourcesCount",
            "function stablecoinSourceAt",
            "function stablecoinTargetsCount",
            "function stablecoinTargetAt",
            "function hasStablecoin",
            "function applyVaultAction",
            "function getVaultAction",
            "createdAt",
        ],
    ),
    (
        "IStablecoin",
        &[
            "function changeSupplyCap",
            "function changePolicy",
            "function startAdminTransfer",
            "previousCapMinor",
            "newCapMinor",
        ],
    ),
    ("IStablecoinFactory", &["isoCode", "supplyCapMinor"]),
    (
        "IStablecoinPolicyRegistry",
        &["function startPolicyAdminTransfer"],
    ),
];

/// The new Rust names that each crate must define.
const RUST_NAMES: &[(&str, &[&str])] = &[
    (
        "vaultrouter",
        &[
            "fn has_stablecoin",
            "fn stablecoin_source",
            "fn stablecoin_target",
            "fn add_stablecoin_source",
            "fn remove_stablecoin_source",
            "fn add_stablecoin_target",
            "fn remove_stablecoin_target",
            "fn registered_stablecoin_source",
            "fn registered_stablecoin_target",
            "fn apply_vault_action",
            "struct VaultActionData",
            "AddStablecoinSource",
            "RemoveStablecoinSource",
            "AddStablecoinTarget",
            "RemoveStablecoinTarget",
            "stablecoin_sources",
            "stablecoin_source_types",
            "stablecoin_targets",
            "stablecoin_target_types",
        ],
    ),
    (
        "stablecoin",
        &[
            "fn start_admin_transfer",
            "fn change_supply_cap",
            "fn change_policy",
        ],
    ),
    ("stablecoinfactory", &["fn create_stablecoin"]),
    ("stablecoinpolicy", &["fn start_policy_admin_transfer"]),
];

fn core_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn precompiles_dir() -> PathBuf {
    core_dir().join("../../contracts/precompiles")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

fn crate_sources(name: &str) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    rust_files(&core_dir().join(name).join("src"), &mut files);
    files.sort();
    files
        .into_iter()
        .map(|path| (path.clone(), read(&path)))
        .collect()
}

/// The interfaces, the ABI exports and the test mock of the vault router.
fn contract_sources() -> Vec<(PathBuf, String)> {
    let mut paths = Vec::new();
    for (_, interface) in CRATES {
        paths.push(precompiles_dir().join(format!("src/{interface}.sol")));
        paths.push(precompiles_dir().join(format!("abi-export/{interface}.json")));
    }
    paths.push(core_dir().join("../../contracts/intex/test/mocks/MockVaultRouter.sol"));
    paths
        .into_iter()
        .map(|path| (path.clone(), read(&path)))
        .collect()
}

#[test]
fn vault_router_and_stablecoin_names_use_the_decided_words() {
    let mut problems = Vec::new();

    let mut sources = contract_sources();
    for (name, _) in CRATES {
        sources.extend(crate_sources(name));
    }
    for (path, text) in &sources {
        for parts in OLD_NAMES {
            let old = parts.concat();
            if text.contains(&old) {
                problems.push(format!("{} has the old name {old}", path.display()));
            }
        }
    }

    for (interface, names) in INTERFACE_NAMES {
        let text = read(&precompiles_dir().join(format!("src/{interface}.sol")));
        for name in *names {
            if !text.contains(name) {
                problems.push(format!("{interface}.sol has no {name}"));
            }
        }
    }

    for (name, expected) in RUST_NAMES {
        let text: String = crate_sources(name)
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        for item in *expected {
            if !text.contains(item) {
                problems.push(format!("crate {name} has no {item}"));
            }
        }
    }

    let runtime = core_dir().join("vaultrouter/src/runtime");
    for file in ["stablecoin.rs", "vault_action.rs"] {
        if !runtime.join(file).is_file() {
            problems.push(format!("runtime/{file} does not exist"));
        }
    }
    for file in ["liquidity.rs", "approved.rs"] {
        if runtime.join(file).exists() {
            problems.push(format!("runtime/{file} still exists"));
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn abi_binding_is_in_precompile_rs() {
    let mut problems = Vec::new();

    for (name, interface) in CRATES {
        let src = core_dir().join(name).join("src");
        if src.join("abi.rs").exists() {
            problems.push(format!("crate {name} still has src/abi.rs"));
        }

        let binding = format!("contracts/precompiles/src/{interface}.sol\"");
        let precompile = src.join("precompile.rs");
        let mut bound_in_precompile = false;
        for (path, text) in crate_sources(name) {
            if !text.contains(&binding) {
                continue;
            }
            if path == precompile {
                bound_in_precompile = text.contains("sol!");
            } else {
                problems.push(format!("{} binds {interface}", path.display()));
            }
        }
        if !bound_in_precompile {
            problems.push(format!(
                "crate {name} has no sol! binding of {interface} in precompile.rs"
            ));
        }
    }

    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
