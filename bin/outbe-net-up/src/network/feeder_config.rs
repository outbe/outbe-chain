//! Feeder configuration schema and validation for `network.yaml`.

use std::collections::BTreeMap;

use eyre::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FeederNetworkConfig {
    /// External EVM JSON-RPC endpoints keyed by chain ID (e.g. `1`, `97`, `11155111`).
    /// Required for non-devnet VM networks for every chain ID used by the active feeder profile.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "deserialize_rpc_endpoints"
    )]
    pub rpc_endpoints: BTreeMap<u64, String>,
    /// Optional provider API keys keyed by provider name (`pyth`, `redstone`), only needed
    /// when the active feeder profile uses a provider that requires one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub api_keys: BTreeMap<String, String>,
}

impl FeederNetworkConfig {
    pub fn is_empty(&self) -> bool {
        self.rpc_endpoints.is_empty() && self.api_keys.is_empty()
    }

    pub fn validate(&self) -> Result<()> {
        for (&chain_id, url) in &self.rpc_endpoints {
            ensure!(
                chain_id > 0,
                "feeder.rpc_endpoints chain_id must be positive (got 0)"
            );
            validate_http_rpc_url(url, &format!("feeder.rpc_endpoints.{chain_id}"))?;
        }
        for (provider, key) in &self.api_keys {
            ensure!(
                matches!(provider.as_str(), "pyth" | "redstone"),
                "feeder.api_keys.{provider} is not a supported API-key provider (expected `pyth` or `redstone`)"
            );
            ensure!(
                !key.trim().is_empty() && !key.chars().any(char::is_whitespace),
                "feeder.api_keys.{provider} must be non-empty without whitespace"
            );
            ensure!(
                !key.contains("REPLACE_WITH_"),
                "feeder.api_keys.{provider} must not contain a REPLACE_WITH_ placeholder"
            );
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ChainIdKey {
    Int(u64),
    Str(String),
}

fn deserialize_rpc_endpoints<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<u64, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct RpcEndpointsVisitor;

    impl<'de> serde::de::Visitor<'de> for RpcEndpointsVisitor {
        type Value = BTreeMap<u64, String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map of numeric chain IDs to HTTP(S) RPC URLs")
        }

        fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut out = BTreeMap::new();
            while let Some((raw_key, value)) = map.next_entry::<ChainIdKey, String>()? {
                let chain_id = match raw_key {
                    ChainIdKey::Int(id) => id,
                    ChainIdKey::Str(ref text) => text.parse::<u64>().map_err(|_| {
                        serde::de::Error::custom(format!(
                            "invalid feeder.rpc_endpoints chain_id `{text}`"
                        ))
                    })?,
                };
                if out.insert(chain_id, value).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate feeder.rpc_endpoints chain_id {chain_id}"
                    )));
                }
            }
            Ok(out)
        }
    }

    deserializer.deserialize_map(RpcEndpointsVisitor)
}

fn validate_http_rpc_url(url: &str, field: &str) -> Result<()> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| eyre::eyre!("{field} must be an HTTP(S) URL: `{url}`"))?;
    ensure!(
        !url.chars().any(char::is_whitespace),
        "{field} must not contain whitespace: `{url}`"
    );
    ensure!(
        !url.contains("REPLACE_WITH_"),
        "{field} must not contain a REPLACE_WITH_ placeholder: `{url}`"
    );
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed
            .split(']')
            .next()
            .ok_or_else(|| eyre::eyre!("{field} has an invalid IPv6 host: `{url}`"))?
    } else {
        host_port.split(':').next().unwrap_or("")
    };
    ensure!(
        !host.is_empty(),
        "{field} must include a non-empty host: `{url}`"
    );
    Ok(())
}
