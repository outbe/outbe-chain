use std::{
    collections::BTreeSet,
    fmt,
    fs::{self, OpenOptions},
    io::Write,
    net::Ipv4Addr,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    str::FromStr,
};

use alloy_primitives::Address;
use eyre::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CONFIG_VERSION: u32 = 1;
pub const NODE_ROLE_FILE: &str = "node-role";

/// Role of a single Outbe VM data disk, communicated to `vm-boot` via `/opt/outbe-chain/node-role`.
///
/// An absent `node-role` marker file defaults to [`NodeRole::Founder`] for backward compatibility
/// with existing founder data trees and byte-identical `vm disk` output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRole {
    Founder,
    JoiningValidator,
    FullNode,
}

impl NodeRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Founder => "founder",
            Self::JoiningValidator => "joining-validator",
            Self::FullNode => "full-node",
        }
    }

    pub fn marker_contents(self) -> String {
        format!("{}\n", self.as_str())
    }

    /// Strictly parse the contents of `/opt/outbe-chain/node-role`.
    ///
    /// Accepts only the exact lowercase role token with an optional single trailing newline.
    pub fn parse_marker(raw: &str) -> Result<Self> {
        let trimmed = raw.strip_suffix('\n').unwrap_or(raw);
        Self::from_str(trimmed)
    }
}

impl fmt::Display for NodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for NodeRole {
    type Err = eyre::Report;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "founder" => Ok(Self::Founder),
            "joining-validator" => Ok(Self::JoiningValidator),
            "full-node" => Ok(Self::FullNode),
            other => Err(eyre::eyre!(
                "invalid node-role value {other:?} (expected 'founder', 'joining-validator', or 'full-node')"
            )),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FullNodeConfig {
    pub version: u32,
    pub network: FullNodeNetworkConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FullNodeNetworkConfig {
    pub chain_id: u64,
    pub certified_rpc: String,
    #[serde(alias = "external_ip")]
    pub advertise_ip: Ipv4Addr,
    #[serde(default = "default_rpc_bind_ip")]
    pub rpc_bind_ip: Ipv4Addr,
    pub bootnodes: Vec<String>,
    #[serde(default)]
    pub use_local_defaults: bool,
}

impl FullNodeConfig {
    pub fn read(path: &Path) -> Result<(Self, String)> {
        let raw = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read full-node config: {}", path.display()))?;
        let config: Self = serde_yaml_ng::from_str(&raw)
            .wrap_err_with(|| format!("invalid full-node config: {}", path.display()))?;
        config.validate()?;
        Ok((config, sha256(raw.as_bytes())))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == CONFIG_VERSION,
            "unsupported full-node config version {}",
            self.version
        );
        ensure!(
            self.network.chain_id > 0,
            "network.chain_id must be positive"
        );
        ensure!(
            self.network.rpc_bind_ip.is_loopback() || self.network.rpc_bind_ip.is_private(),
            "network.rpc_bind_ip must be a loopback or private IPv4 address"
        );
        ensure!(
            (self.network.certified_rpc.starts_with("http://")
                || self.network.certified_rpc.starts_with("https://"))
                && !self.network.certified_rpc.contains(char::is_whitespace),
            "network.certified_rpc must be an HTTP(S) URL"
        );
        ensure!(
            !self.network.bootnodes.is_empty(),
            "network.bootnodes is empty"
        );
        if self
            .network
            .bootnodes
            .iter()
            .any(|node| !node.starts_with("enode://") || node.contains(char::is_whitespace))
        {
            bail!("network.bootnodes contains an invalid enode URL");
        }
        Ok(())
    }

    pub fn write_runtime_config(&self, root: &Path) -> Result<()> {
        for value in self
            .network
            .bootnodes
            .iter()
            .chain(std::iter::once(&self.network.certified_rpc))
        {
            ensure!(
                !value
                    .chars()
                    .any(|character| matches!(character, '"' | '\n' | '\r')),
                "full-node config contains a value that cannot be written to validator.env"
            );
        }
        let extra = if self.network.use_local_defaults {
            "--consensus.use-local-defaults"
        } else {
            ""
        };
        let env = format!(
            concat!(
                "OUTBE_EXTERNAL_IP=\"{}\"\n",
                "OUTBE_RPC_BIND_IP=\"{}\"\n",
                "OUTBE_CERTIFIED_RPC=\"{}\"\n",
                "OUTBE_BOOTNODES=\"{}\"\n",
                "OUTBE_CONSENSUS_EXTRA_ARGS=\"{}\"\n",
                "OUTBE_CHAIN_ID=\"{}\"\n"
            ),
            self.network.advertise_ip,
            self.network.rpc_bind_ip,
            self.network.certified_rpc,
            self.network.bootnodes.join(","),
            extra,
            self.network.chain_id,
        );
        write_private(&root.join("validator.env"), env.as_bytes())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub version: u32,
    pub network: NetworkConfig,
    pub feeder: FeederConfig,
    pub hyperlane: HyperlaneConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub chain_id: u64,
    pub certified_rpc: String,
    #[serde(alias = "external_ip")]
    pub advertise_ip: Ipv4Addr,
    #[serde(default = "default_rpc_bind_ip")]
    pub rpc_bind_ip: Ipv4Addr,
    pub bootnodes: Vec<String>,
    pub consensus_peers: Vec<String>,
    #[serde(default)]
    pub use_local_defaults: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeederConfig {
    /// Public TOML appended to the locally generated account configuration.
    pub public_toml: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HyperlaneConfig {
    pub public_url: String,
    #[serde(
        default = "default_hyperlane_poll_interval",
        skip_serializing_if = "is_default_hyperlane_poll_interval"
    )]
    pub poll_interval_secs: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub gasless: bool,
    #[serde(
        default = "default_hyperlane_funding_wei",
        skip_serializing_if = "is_default_hyperlane_funding_wei"
    )]
    pub signer_funding_wei: String,
    pub origins: Vec<HyperlaneOrigin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HyperlaneOrigin {
    pub domain: u32,
    pub rpc_urls: Vec<String>,
    #[serde(default, skip_serializing_if = "is_default_rpc_consensus")]
    pub rpc_consensus: RpcConsensus,
    pub reorg_period: ReorgPeriod,
    #[serde(
        default = "default_origin_interval",
        skip_serializing_if = "is_default_origin_interval"
    )]
    pub interval_secs: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_from: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RpcConsensus {
    #[default]
    Majority,
    Quorum,
    Single,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReorgPeriod {
    Blocks(u64),
    Tag(FinalityTag),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FinalityTag {
    Finalized,
}

impl OperatorConfig {
    pub fn read(path: &Path) -> Result<(Self, String)> {
        let raw = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to read operator config: {}", path.display()))?;
        let config: Self = serde_yaml_ng::from_str(&raw)
            .wrap_err_with(|| format!("invalid operator config: {}", path.display()))?;
        config.validate()?;
        Ok((config, sha256(raw.as_bytes())))
    }

    pub fn write_runtime_config(&self, root: &Path, validator: Address) -> Result<()> {
        for value in self
            .network
            .bootnodes
            .iter()
            .chain(self.network.consensus_peers.iter())
            .chain(std::iter::once(&self.network.certified_rpc))
        {
            ensure!(
                !value
                    .chars()
                    .any(|character| matches!(character, '"' | '\n' | '\r')),
                "operator config contains a value that cannot be written to validator.env"
            );
        }
        let extra = if self.network.use_local_defaults {
            "--consensus.use-local-defaults"
        } else {
            ""
        };
        let env = format!(
            concat!(
                "OUTBE_EXTERNAL_IP=\"{}\"\n",
                "OUTBE_RPC_BIND_IP=\"{}\"\n",
                "OUTBE_CERTIFIED_RPC=\"{}\"\n",
                "OUTBE_VALIDATOR_ADDRESS=\"{:#x}\"\n",
                "OUTBE_BOOTNODES=\"{}\"\n",
                "OUTBE_CONSENSUS_PEERS=\"{}\"\n",
                "OUTBE_CONSENSUS_EXTRA_ARGS=\"{}\"\n",
                "OUTBE_CHAIN_ID=\"{}\"\n"
            ),
            self.network.advertise_ip,
            self.network.rpc_bind_ip,
            self.network.certified_rpc,
            validator,
            self.network.bootnodes.join(","),
            self.network.consensus_peers.join(","),
            extra,
            self.network.chain_id,
        );
        write_private(&root.join("validator.env"), env.as_bytes())?;
        write_private(
            &root.join("feeder-public.toml"),
            self.feeder.public_toml.as_bytes(),
        )?;
        write_private(
            &root.join("hyperlane-public.toml"),
            self.hyperlane.public_toml()?.as_bytes(),
        )
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == CONFIG_VERSION,
            "unsupported operator config version {}",
            self.version
        );
        ensure!(
            self.network.chain_id > 0,
            "network.chain_id must be positive"
        );
        ensure!(
            self.network.rpc_bind_ip.is_loopback() || self.network.rpc_bind_ip.is_private(),
            "network.rpc_bind_ip must be a loopback or private IPv4 address"
        );
        ensure!(
            self.network.certified_rpc.starts_with("http://")
                || self.network.certified_rpc.starts_with("https://"),
            "network.certified_rpc must be an HTTP(S) URL"
        );
        ensure!(
            !self.network.bootnodes.is_empty(),
            "network.bootnodes is empty"
        );
        if self
            .network
            .bootnodes
            .iter()
            .any(|node| !node.starts_with("enode://") || node.contains(char::is_whitespace))
        {
            bail!("network.bootnodes contains an invalid enode URL");
        }
        ensure!(
            !self.network.consensus_peers.is_empty(),
            "network.consensus_peers is empty"
        );
        if self
            .network
            .consensus_peers
            .iter()
            .any(|peer| !valid_consensus_peer(peer))
        {
            bail!("network.consensus_peers contains an invalid peer");
        }
        ensure!(
            !self.feeder.public_toml.trim().is_empty(),
            "feeder.public_toml is empty"
        );
        self.feeder
            .public_toml
            .parse::<toml::Table>()
            .wrap_err("feeder.public_toml is not valid TOML")?;
        self.hyperlane.validate()?;
        Ok(())
    }
}

impl HyperlaneConfig {
    pub(crate) fn public_toml(&self) -> Result<String> {
        #[derive(Serialize)]
        struct HyperlaneSection {
            poll_interval_secs: u64,
            gasless: bool,
        }
        #[derive(Serialize)]
        struct HealthSection<'a> {
            enabled: bool,
            bind_address: &'a str,
        }
        #[derive(Serialize)]
        struct StorageSection<'a> {
            db_path: &'a str,
        }
        #[derive(Serialize)]
        struct ServerSection<'a> {
            bind_address: &'a str,
            public_url: &'a str,
        }
        #[derive(Serialize)]
        struct PublicConfig<'a> {
            hyperlane: HyperlaneSection,
            health: HealthSection<'a>,
            storage: StorageSection<'a>,
            server: ServerSection<'a>,
            origin: &'a [HyperlaneOrigin],
        }
        toml::to_string_pretty(&PublicConfig {
            hyperlane: HyperlaneSection {
                poll_interval_secs: self.poll_interval_secs,
                gasless: self.gasless,
            },
            health: HealthSection {
                enabled: true,
                bind_address: "127.0.0.1:9003",
            },
            storage: StorageSection {
                db_path: "/opt/outbe-chain/state/hyperlane/db",
            },
            server: ServerSection {
                bind_address: "0.0.0.0:9000",
                public_url: &self.public_url,
            },
            origin: &self.origins,
        })
        .wrap_err("failed to serialize Hyperlane configuration")
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.poll_interval_secs > 0,
            "hyperlane.poll_interval_secs must be positive"
        );
        ensure!(
            self.signer_funding_wei
                .parse::<alloy_primitives::U256>()
                .is_ok_and(|amount| !amount.is_zero()),
            "hyperlane.signer_funding_wei must be a positive integer"
        );
        let public_url = reqwest::Url::parse(&self.public_url)
            .wrap_err("hyperlane.public_url must be a valid URL")?;
        ensure!(
            matches!(public_url.scheme(), "http" | "https")
                && public_url.host_str().is_some()
                && public_url.path() == "/"
                && public_url.query().is_none()
                && public_url.username().is_empty(),
            "hyperlane.public_url must be an HTTP(S) origin without path, query, or credentials"
        );
        ensure!(!self.origins.is_empty(), "hyperlane.origins is empty");
        let mut domains = BTreeSet::new();
        for origin in &self.origins {
            ensure!(
                origin.domain > 0,
                "Hyperlane origin domain must be non-zero"
            );
            ensure!(
                domains.insert(origin.domain),
                "duplicate Hyperlane origin domain {}",
                origin.domain
            );
            ensure!(
                origin.interval_secs > 0,
                "Hyperlane origin {} interval_secs must be positive",
                origin.domain
            );
            ensure!(
                !origin.rpc_urls.is_empty(),
                "Hyperlane origin {} has no rpc_urls",
                origin.domain
            );
            for rpc in &origin.rpc_urls {
                let url = reqwest::Url::parse(rpc).wrap_err_with(|| {
                    format!("invalid Hyperlane origin {} RPC URL", origin.domain)
                })?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none(),
                    "Hyperlane origin {} RPC must be an HTTP(S) URL without credentials",
                    origin.domain
                );
            }
            if let Some(ws) = &origin.ws_url {
                let url = reqwest::Url::parse(ws).wrap_err_with(|| {
                    format!("invalid Hyperlane origin {} WS URL", origin.domain)
                })?;
                ensure!(
                    matches!(url.scheme(), "ws" | "wss")
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none(),
                    "Hyperlane origin {} WS URL must be a WS(S) URL without credentials",
                    origin.domain
                );
            }
        }
        self.public_toml()?.parse::<toml::Table>()?;
        Ok(())
    }
}

/// Shared offline validation for `hyperlane-public.toml` used by network build, host install,
/// and `vm-boot`. Rejects secret sections (`[account]`, `[chain]`, `private_key`) and validates
/// every origin entry without making network or controller RPC calls.
pub fn validate_hyperlane_public_toml(
    raw: &str,
    expected_chain_id: u64,
    require_local_origin: bool,
) -> Result<()> {
    let table: toml::Table = raw
        .parse()
        .wrap_err("hyperlane-public.toml is not valid TOML")?;
    ensure!(
        !table.contains_key("account"),
        "hyperlane-public.toml must not contain an [account] section"
    );
    ensure!(
        !table.contains_key("chain"),
        "hyperlane-public.toml must not contain a [chain] section"
    );
    ensure!(
        !table.contains_key("private_key"),
        "hyperlane-public.toml must not contain private_key"
    );

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublicHyperlaneSection {
        poll_interval_secs: u64,
        gasless: bool,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublicHealthSection {
        #[allow(dead_code)]
        enabled: bool,
        #[allow(dead_code)]
        bind_address: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublicStorageSection {
        #[allow(dead_code)]
        db_path: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublicServerSection {
        #[allow(dead_code)]
        bind_address: String,
        public_url: String,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PublicToml {
        hyperlane: PublicHyperlaneSection,
        #[allow(dead_code)]
        health: PublicHealthSection,
        #[allow(dead_code)]
        storage: PublicStorageSection,
        server: PublicServerSection,
        #[serde(default)]
        origin: Vec<HyperlaneOrigin>,
    }

    let parsed: PublicToml =
        toml::from_str(raw).wrap_err("invalid hyperlane-public.toml structure")?;
    let cfg = HyperlaneConfig {
        public_url: parsed.server.public_url,
        poll_interval_secs: parsed.hyperlane.poll_interval_secs,
        gasless: parsed.hyperlane.gasless,
        signer_funding_wei: default_hyperlane_funding_wei(),
        origins: parsed.origin,
    };
    cfg.validate()?;
    if require_local_origin {
        let local_domain = u32::try_from(expected_chain_id).wrap_err_with(|| {
            format!("chain_id {expected_chain_id} does not fit in Hyperlane uint32 domain")
        })?;
        ensure!(
            cfg.origins
                .iter()
                .any(|origin| origin.domain == local_domain),
            "hyperlane-public.toml must include local origin domain {local_domain}"
        );
    }
    Ok(())
}

impl NetworkConfig {
    pub(crate) fn local_rpc_url(&self) -> String {
        format!("http://{}:8545", self.rpc_bind_ip)
    }
}

const fn default_rpc_bind_ip() -> Ipv4Addr {
    Ipv4Addr::LOCALHOST
}

const fn default_hyperlane_poll_interval() -> u64 {
    30
}

const fn default_origin_interval() -> u64 {
    15
}

fn default_hyperlane_funding_wei() -> String {
    "1000000000000000000".to_owned()
}

fn is_default_hyperlane_poll_interval(value: &u64) -> bool {
    *value == default_hyperlane_poll_interval()
}

fn is_default_hyperlane_funding_wei(value: &String) -> bool {
    value == &default_hyperlane_funding_wei()
}

fn is_default_origin_interval(value: &u64) -> bool {
    *value == default_origin_interval()
}

fn is_default_rpc_consensus(value: &RpcConsensus) -> bool {
    matches!(value, RpcConsensus::Majority)
}

const fn is_false(value: &bool) -> bool {
    !*value
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn valid_consensus_peer(value: &str) -> bool {
    let Some((key, address)) = value.split_once('@') else {
        return false;
    };
    let Some((host, port)) = address.rsplit_once(':') else {
        return false;
    };
    key.len() == 96
        && key.bytes().all(|byte| byte.is_ascii_hexdigit())
        && host.parse::<Ipv4Addr>().is_ok()
        && port.parse::<u16>().is_ok_and(|port| port > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_minimal_config() {
        let template = format!(
            r#"
version: 1
network:
  chain_id: 424242
  certified_rpc: https://rpc.example
  advertise_ip: 192.0.2.10
  bootnodes: ["enode://abc@192.0.2.1:30303"]
  consensus_peers: ["{}@192.0.2.1:30400"]
  use_local_defaults: true
feeder:
  public_toml: |
    [oracle]
    sources = "local"
hyperlane:
  public_url: http://192.0.2.10:9000
  origins:
    - domain: 424242
      rpc_urls: [https://rpc.example]
      reorg_period: 0
"#,
            "c".repeat(96),
        );
        for yaml in [
            template.clone(),
            template.replace("advertise_ip:", "external_ip:"),
        ] {
            let config: OperatorConfig = serde_yaml_ng::from_str(&yaml).unwrap();
            config.validate().unwrap();
            assert_eq!(config.network.advertise_ip, Ipv4Addr::new(192, 0, 2, 10));
            assert_eq!(config.network.rpc_bind_ip, Ipv4Addr::LOCALHOST);
        }
    }

    #[test]
    fn full_node_config_validates_minimal_and_rejects_validator_sections() {
        let valid = r#"
version: 1
network:
  chain_id: 424242
  certified_rpc: https://rpc.example
  advertise_ip: 192.0.2.10
  bootnodes: ["enode://abc@192.0.2.1:30303"]
  use_local_defaults: true
"#;
        let cfg: FullNodeConfig = serde_yaml_ng::from_str(valid).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.network.rpc_bind_ip, Ipv4Addr::LOCALHOST);

        let dir = tempfile::tempdir().unwrap();
        cfg.write_runtime_config(dir.path()).unwrap();
        let env = fs::read_to_string(dir.path().join("validator.env")).unwrap();
        assert!(env.contains("OUTBE_CERTIFIED_RPC=\"https://rpc.example\""));
        assert!(!env.contains("OUTBE_VALIDATOR_ADDRESS"));
        assert!(!env.contains("OUTBE_CONSENSUS_PEERS"));

        // Reject unused/validator sections strictly via deny_unknown_fields.
        for bad in [
            format!("{valid}\nfeeder:\n  public_toml: \"[oracle]\\n\"\n"),
            format!("{valid}\nhyperlane:\n  public_url: http://192.0.2.10:9000\n  origins: []\n"),
            format!(
                "{valid}  consensus_peers: [\"{}@192.0.2.1:30400\"]\n",
                "c".repeat(96)
            ),
        ] {
            assert!(
                serde_yaml_ng::from_str::<FullNodeConfig>(&bad).is_err(),
                "full-node config must reject validator fields:\n{bad}"
            );
        }
    }

    #[test]
    fn node_role_marker_parsing_is_strict() {
        assert_eq!(
            NodeRole::parse_marker("founder").unwrap(),
            NodeRole::Founder
        );
        assert_eq!(
            NodeRole::parse_marker("founder\n").unwrap(),
            NodeRole::Founder
        );
        assert_eq!(
            NodeRole::parse_marker("joining-validator\n").unwrap(),
            NodeRole::JoiningValidator
        );
        assert_eq!(
            NodeRole::parse_marker("full-node\n").unwrap(),
            NodeRole::FullNode
        );

        for invalid in [
            "",
            "\n",
            "founder\n\n",
            " founder\n",
            "founder \n",
            "FOUNDER\n",
            "validator\n",
            "fullnode\n",
        ] {
            assert!(
                NodeRole::parse_marker(invalid).is_err(),
                "must reject invalid role marker: {invalid:?}"
            );
        }
    }
}
