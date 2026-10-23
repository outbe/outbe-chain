use super::{
    CheckedAddressClass, PlannedAddressClass, PlannedAddressClasses, StablecoinAddressManifest,
};
use eyre::{bail, eyre, Result, WrapErr};
use std::{collections::BTreeSet, fs, path::Path};

pub(super) fn load_planned_classes(
    repo_root: &Path,
    manifest: &StablecoinAddressManifest,
) -> Result<Vec<CheckedAddressClass>> {
    let path = repo_root
        .join("crates/blockchain/primitives/testdata/stablecoin/v1/planned-address-classes.json");
    let bytes = fs::read(&path).wrap_err_with(|| format!("read {}", path.display()))?;
    let registry: PlannedAddressClasses =
        serde_json::from_slice(&bytes).wrap_err_with(|| format!("parse {}", path.display()))?;
    if registry.schema_version != 1 {
        bail!("unsupported planned address-class schema version");
    }
    if registry.classes.is_empty() {
        bail!("planned address-class registry must not be empty");
    }
    let mut checked = Vec::with_capacity(registry.classes.len());
    let mut names = BTreeSet::new();
    for class in registry.classes {
        if !names.insert(class.name.clone()) {
            bail!("duplicate planned address class {}", class.name);
        }
        checked.push(check_planned_class(class)?);
    }
    require_disjoint_classes(&checked)?;
    let stablecoin_prefix = canonical_variable_prefix(&manifest.prefix, 16)?;
    if !checked
        .iter()
        .any(|class| class.name == "stablecoin-v1" && class.prefix == stablecoin_prefix)
    {
        bail!("stablecoin network prefix is absent from the planned class registry");
    }
    Ok(checked)
}

fn check_planned_class(class: PlannedAddressClass) -> Result<CheckedAddressClass> {
    if class.owner.is_empty() {
        bail!("planned address class {} has no owner", class.name);
    }
    if class.prefix_bits == 0 || !class.prefix_bits.is_multiple_of(8) {
        bail!(
            "planned address class {} prefixBits must be nonzero and byte-aligned",
            class.name
        );
    }
    let prefix = canonical_variable_prefix(&class.prefix, class.prefix_bits)?;
    Ok(CheckedAddressClass {
        name: class.name,
        prefix,
    })
}

fn require_disjoint_classes(checked: &[CheckedAddressClass]) -> Result<()> {
    for (index, left) in checked.iter().enumerate() {
        for right in checked.iter().skip(index + 1) {
            if left.prefix.starts_with(&right.prefix) || right.prefix.starts_with(&left.prefix) {
                bail!(
                    "planned address classes {} and {} overlap",
                    left.name,
                    right.name
                );
            }
        }
    }
    Ok(())
}

pub(super) fn address_literals(source: &str) -> Result<Vec<String>> {
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

pub(super) fn canonical_address(value: &str) -> Result<String> {
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid EVM address: {value}");
    }
    Ok(value.to_ascii_lowercase())
}

fn canonical_variable_prefix(value: &str, prefix_bits: u16) -> Result<String> {
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    let expected_hex = usize::from(prefix_bits / 4);
    if value.len() != expected_hex || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid {prefix_bits}-bit address prefix: {value}");
    }
    Ok(value.to_ascii_lowercase())
}

pub(super) fn marker_byte(value: &str) -> Result<u8> {
    let value = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    let bytes = hex::decode(value).wrap_err("decode stablecoin marker")?;
    match bytes.as_slice() {
        [byte] => Ok(*byte),
        _ => {
            bail!("stablecoin marker must contain exactly one byte");
        }
    }
}
