use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use eyre::{bail, eyre, Result, WrapErr};
use serde::Deserialize;
use serde_json::{Map, Value};

const ETHEREUM_BUILTIN_MAX: u8 = 10;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StablecoinAddressManifest {
    factory: String,
    policy_registry: String,
    prefix: String,
    marker: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NamespaceReport {
    pub declared_addresses: usize,
    pub ethereum_builtins: usize,
    pub genesis_files: usize,
}

pub fn check_namespace(
    repo_root: &Path,
    genesis_paths: &[PathBuf],
    preseed: bool,
) -> Result<NamespaceReport> {
    let manifest = load_manifest(repo_root)?;
    let declared_addresses = scan_declared_addresses(repo_root, &manifest)?;
    for path in genesis_paths {
        scan_genesis(path, &manifest, preseed)?;
    }
    Ok(NamespaceReport {
        declared_addresses,
        ethereum_builtins: usize::from(ETHEREUM_BUILTIN_MAX),
        genesis_files: genesis_paths.len(),
    })
}

fn load_manifest(repo_root: &Path) -> Result<StablecoinAddressManifest> {
    let path = repo_root
        .join("crates/blockchain/primitives/testdata/stablecoin/v1/network-address-vectors.json");
    let bytes = fs::read(&path).wrap_err_with(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).wrap_err_with(|| format!("parse {}", path.display()))
}

fn scan_declared_addresses(
    repo_root: &Path,
    manifest: &StablecoinAddressManifest,
) -> Result<usize> {
    let path = repo_root.join("crates/blockchain/primitives/src/addresses.rs");
    let source = fs::read_to_string(&path).wrap_err_with(|| format!("read {}", path.display()))?;
    let addresses = address_literals(&source)?;
    let unique: BTreeSet<&str> = addresses.iter().map(String::as_str).collect();
    if unique.len() != addresses.len() {
        bail!("duplicate address! literal in {}", path.display());
    }

    let factory = canonical_address(&manifest.factory)?;
    let policy = canonical_address(&manifest.policy_registry)?;
    if !unique.contains(factory.as_str()) || !unique.contains(policy.as_str()) {
        bail!(
            "stablecoin fixed addresses are missing from {}",
            path.display()
        );
    }
    let prefix = canonical_prefix(&manifest.prefix)?;
    if let Some(collision) = unique.iter().find(|address| address.starts_with(&prefix)) {
        bail!("fixed address {collision} collides with stablecoin prefix 0x{prefix}");
    }

    for suffix in 1..=ETHEREUM_BUILTIN_MAX {
        let builtin = format!("{:040x}", suffix);
        if unique.contains(builtin.as_str()) {
            bail!("Outbe address collides with Ethereum built-in 0x{builtin}");
        }
    }
    marker_byte(&manifest.marker)?;
    Ok(addresses.len())
}

fn scan_genesis(path: &Path, manifest: &StablecoinAddressManifest, preseed: bool) -> Result<()> {
    let bytes = fs::read(path).wrap_err_with(|| format!("read genesis {}", path.display()))?;
    let genesis: Value = serde_json::from_slice(&bytes)
        .wrap_err_with(|| format!("parse genesis {}", path.display()))?;
    let alloc = genesis
        .get("alloc")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("genesis {} alloc must be an object", path.display()))?;

    let factory = canonical_address(&manifest.factory)?;
    let policy = canonical_address(&manifest.policy_registry)?;
    let prefix = canonical_prefix(&manifest.prefix)?;
    let marker = manifest.marker.to_ascii_lowercase();
    let mut normalized = BTreeSet::new();
    let mut fixed_entries = Map::new();

    for (raw_address, account) in alloc {
        let address = canonical_address(raw_address)?;
        if !normalized.insert(address.clone()) {
            bail!("duplicate normalized genesis alloc address 0x{address}");
        }
        if address.starts_with(&prefix) {
            bail!("genesis address 0x{address} collides with stablecoin prefix 0x{prefix}");
        }
        if address == factory || address == policy {
            fixed_entries.insert(address, account.clone());
        }
    }

    for address in [&factory, &policy] {
        let Some(account) = fixed_entries.get(address) else {
            if preseed {
                continue;
            }
            bail!("genesis is missing stablecoin reserved account 0x{address}");
        };
        validate_fixed_account(address, account, &marker, preseed)?;
    }
    Ok(())
}

fn validate_fixed_account(
    address: &str,
    account: &Value,
    marker: &str,
    preseed: bool,
) -> Result<()> {
    let account = account
        .as_object()
        .ok_or_else(|| eyre!("genesis account 0x{address} must be an object"))?;
    match account.get("code").and_then(Value::as_str) {
        Some(code) if code.eq_ignore_ascii_case(marker) => {}
        Some(_) => bail!("stablecoin reserved account 0x{address} has conflicting code"),
        None if preseed => {}
        None => bail!("stablecoin reserved account 0x{address} is missing marker code"),
    }
    if account
        .get("storage")
        .and_then(Value::as_object)
        .is_some_and(|storage| !storage.is_empty())
    {
        bail!("stablecoin reserved account 0x{address} has conflicting storage");
    }
    for field in ["balance", "nonce"] {
        if account
            .get(field)
            .is_some_and(|value| !is_zero_quantity(value))
        {
            bail!("stablecoin reserved account 0x{address} has nonzero or invalid {field}");
        }
    }
    Ok(())
}

fn is_zero_quantity(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_u64() == Some(0),
        Value::String(value) => {
            let digits = value
                .strip_prefix("0x")
                .or_else(|| value.strip_prefix("0X"))
                .unwrap_or(value);
            !digits.is_empty()
                && digits.bytes().all(|byte| byte.is_ascii_hexdigit())
                && digits.bytes().all(|byte| byte == b'0')
        }
        _ => false,
    }
}

fn address_literals(source: &str) -> Result<Vec<String>> {
    let marker = "address!(\"0x";
    let mut remaining = source;
    let mut addresses = Vec::new();
    while let Some(offset) = remaining.find(marker) {
        let start = offset + marker.len();
        let end = start + 40;
        let value = remaining
            .get(start..end)
            .ok_or_else(|| eyre!("truncated address! literal"))?;
        addresses.push(canonical_address(value)?);
        remaining = remaining
            .get(end..)
            .ok_or_else(|| eyre!("truncated addresses source"))?;
    }
    Ok(addresses)
}

fn canonical_address(value: &str) -> Result<String> {
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid EVM address: {value}");
    }
    Ok(value.to_ascii_lowercase())
}

fn canonical_prefix(value: &str) -> Result<String> {
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if value.len() != 4 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid stablecoin prefix: {value}");
    }
    Ok(value.to_ascii_lowercase())
}

fn marker_byte(value: &str) -> Result<u8> {
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    let bytes = hex::decode(value).wrap_err("decode stablecoin marker")?;
    match bytes.as_slice() {
        [byte] => Ok(*byte),
        _ => bail!("stablecoin marker must contain exactly one byte"),
    }
}
