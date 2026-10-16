//! The main config file, the pairs file that a top-level `pairs` key names,
//! and the sources the node serves.

use eyre::{Context, Result};
use std::path::Path;

use crate::errors::ConfigError;

/// Reads the main config. A top-level `pairs` key names a second file,
/// relative to the main one, whose sections join the table. A section set in
/// both files is an error.
pub(super) fn read_with_pairs(path: &str) -> Result<toml::Table> {
    let mut table = read_table(Path::new(path))?;
    let Some(pairs) = table.remove("pairs") else {
        return Ok(table);
    };
    let pairs = pairs.as_str().ok_or(ConfigError::PairsPathNotString)?;
    let pairs_path = Path::new(path)
        .parent()
        .unwrap_or(Path::new(""))
        .join(pairs);
    for (key, value) in read_table(&pairs_path)? {
        if table.insert(key.clone(), value).is_some() {
            return Err(ConfigError::SectionInBothFiles {
                key,
                main: path.to_owned(),
                pairs: pairs_path.display().to_string(),
            }
            .into());
        }
    }
    Ok(table)
}

fn read_table(path: &Path) -> Result<toml::Table> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse config file: {}", path.display()))
}

/// The sections the node serves in `chain` sources mode.
const SHARED_SECTIONS: &[&str] = &[
    "currency_pairs",
    "deviation_thresholds",
    "dex_providers",
    "onchain_providers",
];

/// Adds the node's sources to the local table. The local table must not set
/// a section that the node serves.
pub(super) fn merge_chain_sources(table: &mut toml::Table, text: &str) -> Result<()> {
    if let Some(key) = SHARED_SECTIONS.iter().find(|key| table.contains_key(**key)) {
        return Err(ConfigError::SharedSectionInLocalConfig(key).into());
    }
    let shared: toml::Table =
        toml::from_str(text).context("failed to parse the node's feeder sources")?;
    table.extend(shared);
    Ok(())
}

/// An entry with no own `rpc_endpoint` takes the one for its `chain_id`.
fn fill_rpc(
    rpc_endpoint: &mut String,
    rpc_endpoints: &std::collections::BTreeMap<String, String>,
    name: &str,
    chain_id: u64,
) -> Result<()> {
    if rpc_endpoint.is_empty() {
        *rpc_endpoint = rpc_endpoints
            .get(&chain_id.to_string())
            .ok_or_else(|| ConfigError::MissingRpcEndpoint {
                name: name.to_owned(),
                chain_id,
            })?
            .clone();
    }
    Ok(())
}

/// Gives each DEX and on-chain feed entry without its own `rpc_endpoint` the
/// URL for its `chain_id`.
pub(super) fn fill_rpc_endpoints(config: &mut super::FeederConfig) -> Result<()> {
    for dex in &mut config.dex_providers {
        fill_rpc(
            &mut dex.rpc_endpoint,
            &config.rpc_endpoints,
            &dex.name,
            dex.chain_id,
        )?;
    }
    for feed in &mut config.onchain_providers {
        fill_rpc(
            &mut feed.rpc_endpoint,
            &config.rpc_endpoints,
            &feed.name,
            feed.chain_id,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::config::FeederConfig;

    /// A local config in `chain` sources mode.
    const CHAIN_LOCAL: &str = r#"
[chain]
rpc_endpoint = "http://localhost:8545"
chain_id = 1

[account]
private_key = "0xdead"
validator_address = "0x1111111111111111111111111111111111111111"

[oracle]
vote_period = 60

[rpc_endpoints]
1 = "https://ethereum-rpc.example.invalid"
"#;

    #[test]
    fn every_node_sources_file_builds_a_valid_config() {
        let directory = format!(
            "{}/../../crates/system/oracle/sources",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut files = 0;
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            let config = FeederConfig::build(toml::from_str(CHAIN_LOCAL).unwrap(), Some(&text))
                .unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));
            config
                .validate()
                .unwrap_or_else(|error| panic!("{}: {error:#}", path.display()));
            assert!(!config.currency_pairs.is_empty(), "{}", path.display());
            assert!(config
                .dex_providers
                .iter()
                .all(|dex| dex.rpc_endpoint == "https://ethereum-rpc.example.invalid"));
            files += 1;
        }
        assert!(files > 0);
    }

    #[test]
    fn chain_sources_reject_a_shared_section_in_the_local_config() {
        let local = format!("{CHAIN_LOCAL}\n[[deviation_thresholds]]\nbase = \"COEN\"\n");
        let error = FeederConfig::build(toml::from_str(&local).unwrap(), Some("")).unwrap_err();
        assert!(
            error.to_string().contains("remove `deviation_thresholds`"),
            "{error}"
        );
    }

    #[test]
    fn a_feed_without_an_rpc_for_its_chain_is_rejected() {
        let sources = r#"
[[onchain_providers]]
name = "chainlink_eur_usd"
chain_id = 56
contract = "0x4444444444444444444444444444444444444444"
"#;
        let error =
            FeederConfig::build(toml::from_str(CHAIN_LOCAL).unwrap(), Some(sources)).unwrap_err();
        assert!(error.to_string().contains("no URL for chain 56"), "{error}");
    }

    #[test]
    fn an_omitted_market_quote_is_the_pair_market_quote() {
        let sources = r#"
[[currency_pairs]]
base = "COEN"
quote = "840"
markets = [{ provider = "pyth" }]

[[currency_pairs]]
base = "COEN"
quote = "978"
cross = "840"
markets = [{ provider = "redstone" }]
"#;
        let config =
            FeederConfig::build(toml::from_str(CHAIN_LOCAL).unwrap(), Some(sources)).unwrap();
        config.validate().unwrap();
        assert!(config
            .currency_pairs
            .iter()
            .flat_map(|pair| &pair.sources)
            .all(|source| source.quote == "840"));

        let exchange = r#"
[[currency_pairs]]
base = "COEN"
quote = "840"
markets = [{ provider = "binance" }]
"#;
        let config =
            FeederConfig::build(toml::from_str(CHAIN_LOCAL).unwrap(), Some(exchange)).unwrap();
        let error = config.validate().unwrap_err();
        assert!(error.to_string().contains("is an ISO code"), "{error}");
    }
}
