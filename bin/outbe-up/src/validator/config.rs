use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::Path,
};

use alloy_primitives::Address;
use eyre::{bail, ensure, Context, Result};
use outbe_primitives::tee_attestation_v1::{
    AttestationMode, NetworkBindingV1, TeePolicyScheduleV1,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub version: u32,
    pub network: NetworkConfig,
    pub tee: TeeConfig,
    pub feeder: FeederConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub chain_id: u64,
    pub certified_rpc: String,
    pub external_ip: Ipv4Addr,
    pub bootnodes: Vec<String>,
    pub consensus_peers: Vec<String>,
    #[serde(default)]
    pub use_local_defaults: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeeConfig {
    pub runtime: EnclaveRuntime,
    pub lease_duration_seconds: LeaseDuration,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LeaseDuration {
    Seconds(u64),
    Mode(LeaseDurationMode),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LeaseDurationMode {
    Auto,
}

impl LeaseDuration {
    pub fn resolve(self, minimum: u64, maximum: u64) -> Result<u64> {
        ensure!(
            minimum <= maximum,
            "finalized TEE policy has an invalid lease range {minimum}..={maximum} seconds"
        );
        match self {
            Self::Mode(LeaseDurationMode::Auto) => Ok(maximum),
            Self::Seconds(seconds) => {
                ensure!(
                    (minimum..=maximum).contains(&seconds),
                    "tee.lease_duration_seconds={seconds} is outside the finalized TEE policy range {minimum}..={maximum} seconds"
                );
                Ok(seconds)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnclaveRuntime {
    BundledSgx,
    SystemGramine,
    GramineDirectDev,
}

impl EnclaveRuntime {
    pub const fn as_env(self) -> &'static str {
        match self {
            Self::BundledSgx => "bundled-sgx",
            Self::SystemGramine => "system-gramine",
            Self::GramineDirectDev => "gramine-direct-dev",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeederConfig {
    /// Public TOML appended to the locally generated account configuration.
    pub public_toml: String,
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
        let development_binding = match self.tee.runtime {
            EnclaveRuntime::GramineDirectDev => {
                write_development_secrets(root)?;
                development_network_binding(root, self.network.chain_id)?
            }
            EnclaveRuntime::BundledSgx | EnclaveRuntime::SystemGramine => String::new(),
        };
        let env = format!(
            concat!(
                "OUTBE_EXTERNAL_IP=\"{}\"\n",
                "OUTBE_CERTIFIED_RPC=\"{}\"\n",
                "OUTBE_VALIDATOR_ADDRESS=\"{:#x}\"\n",
                "OUTBE_BOOTNODES=\"{}\"\n",
                "OUTBE_CONSENSUS_PEERS=\"{}\"\n",
                "OUTBE_CONSENSUS_EXTRA_ARGS=\"{}\"\n",
                "OUTBE_ENCLAVE_RUNTIME=\"{}\"\n",
                "OUTBE_DEV_NETWORK_BINDING=\"{}\"\n",
                "OUTBE_CHAIN_ID=\"{}\"\n"
            ),
            self.network.external_ip,
            self.network.certified_rpc,
            validator,
            self.network.bootnodes.join(","),
            self.network.consensus_peers.join(","),
            extra,
            self.tee.runtime.as_env(),
            development_binding,
            self.network.chain_id,
        );
        write_private(&root.join("validator.env"), env.as_bytes())?;
        write_private(
            &root.join("feeder-public.toml"),
            self.feeder.public_toml.as_bytes(),
        )
    }

    fn validate(&self) -> Result<()> {
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
        if let LeaseDuration::Seconds(seconds) = self.tee.lease_duration_seconds {
            ensure!(seconds > 0, "tee.lease_duration_seconds must be positive");
        }
        if matches!(self.tee.runtime, EnclaveRuntime::GramineDirectDev) {
            ensure!(
                outbe_primitives::tee_genesis_v1::is_attestation_mode_allowed_for_chain_id(
                    self.network.chain_id,
                    AttestationMode::GramineDirectDev,
                ),
                "tee.runtime=gramine-direct-dev is forbidden for network.chain_id={}",
                self.network.chain_id
            );
        }
        ensure!(
            !self.feeder.public_toml.trim().is_empty(),
            "feeder.public_toml is empty"
        );
        self.feeder
            .public_toml
            .parse::<toml::Table>()
            .wrap_err("feeder.public_toml is not valid TOML")?;
        Ok(())
    }
}

fn development_network_binding(root: &Path, chain_id: u64) -> Result<String> {
    let genesis: serde_json::Value = serde_json::from_slice(&fs::read(root.join("genesis.json"))?)?;
    let encoded = genesis
        .pointer("/config/teeAttestationV1/policySchedule")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| eyre::eyre!("genesis.json has no TEE policy schedule"))?;
    let schedule = TeePolicyScheduleV1::decode_canonical(&hex::decode(
        encoded.strip_prefix("0x").unwrap_or(encoded),
    )?)
    .wrap_err("invalid genesis TEE policy schedule")?;
    ensure!(
        schedule.chain_id == alloy_primitives::U256::from(chain_id).to_be_bytes::<32>(),
        "genesis TEE policy chain id does not match operator config"
    );
    let policy = schedule
        .active_policy(1)
        .wrap_err("genesis TEE policy has no active policy at height 1")?;
    ensure!(
        policy.attestation_mode == AttestationMode::GramineDirectDev,
        "tee.runtime=gramine-direct-dev requires a GramineDirectDev genesis TEE policy"
    );
    Ok(hex::encode(
        NetworkBindingV1 {
            chain_id: schedule.chain_id,
            genesis_hash: schedule.genesis_hash,
            attestation_mode: policy.attestation_mode,
        }
        .encode_canonical()
        .wrap_err("failed to encode development network binding")?,
    ))
}

fn write_development_secrets(root: &Path) -> Result<()> {
    let tee = root.join("tee");
    fs::create_dir(&tee)?;
    fs::set_permissions(&tee, fs::Permissions::from_mode(0o700))?;
    for name in ["dev-dkg-seed.hex", "dev-offer-secret.hex"] {
        let mut secret = [0_u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        if secret.iter().all(|byte| *byte == 0) {
            secret[31] = 1;
        }
        write_private(&tee.join(name), hex::encode(secret).as_bytes())?;
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

pub fn sha256(bytes: &[u8]) -> String {
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
        let config: OperatorConfig = serde_yaml_ng::from_str(&format!(
            r#"
version: 1
network:
  chain_id: 424242
  certified_rpc: https://rpc.example
  external_ip: 192.0.2.10
  bootnodes: ["enode://abc@192.0.2.1:30303"]
  consensus_peers: ["{}@192.0.2.1:30400"]
  use_local_defaults: true
tee:
  runtime: bundled-sgx
  lease_duration_seconds: auto
feeder:
  public_toml: |
    [oracle]
    sources = "local"
"#,
            "c".repeat(96),
        ))
        .unwrap();
        config.validate().unwrap();
        assert!(matches!(
            config.tee.lease_duration_seconds,
            LeaseDuration::Mode(LeaseDurationMode::Auto)
        ));
    }

    #[test]
    fn resolves_lease_duration_against_policy() {
        assert_eq!(
            LeaseDuration::Mode(LeaseDurationMode::Auto)
                .resolve(3_600, 1_209_600)
                .unwrap(),
            1_209_600
        );
        assert_eq!(
            LeaseDuration::Seconds(86_400)
                .resolve(3_600, 1_209_600)
                .unwrap(),
            86_400
        );
        assert!(LeaseDuration::Seconds(2_592_000)
            .resolve(3_600, 1_209_600)
            .unwrap_err()
            .to_string()
            .contains("outside the finalized TEE policy range"));
    }

    #[test]
    fn accepts_existing_numeric_lease_config() {
        let config: TeeConfig =
            serde_yaml_ng::from_str("runtime: bundled-sgx\nlease_duration_seconds: 86400\n")
                .unwrap();
        assert!(matches!(
            config.lease_duration_seconds,
            LeaseDuration::Seconds(86_400)
        ));
    }

    #[test]
    fn direct_dev_runtime_is_limited_to_development_networks() {
        let config = |chain_id| {
            serde_yaml_ng::from_str::<OperatorConfig>(&format!(
                r#"
version: 1
network:
  chain_id: {chain_id}
  certified_rpc: https://rpc.example
  external_ip: 192.0.2.10
  bootnodes: ["enode://abc@192.0.2.1:30303"]
  consensus_peers: ["{}@192.0.2.1:30400"]
tee:
  runtime: gramine-direct-dev
  lease_duration_seconds: auto
feeder:
  public_toml: |
    [oracle]
    sources = "local"
"#,
                "c".repeat(96),
            ))
            .unwrap()
        };

        config(54322345).validate().unwrap();
        assert!(config(676)
            .validate()
            .unwrap_err()
            .to_string()
            .contains("forbidden"));
    }

    #[test]
    fn derives_direct_dev_binding_from_genesis_policy() {
        let root = tempfile::tempdir().unwrap();
        let mut genesis: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../../../release/testnet-genesis.json"))
                .unwrap();
        let encoded = genesis
            .pointer("/config/teeAttestationV1/policySchedule")
            .and_then(serde_json::Value::as_str)
            .unwrap();
        let mut schedule = TeePolicyScheduleV1::decode_canonical(
            &hex::decode(encoded.trim_start_matches("0x")).unwrap(),
        )
        .unwrap();
        schedule.entries[0].policy.attestation_mode = AttestationMode::GramineDirectDev;
        *genesis
            .pointer_mut("/config/teeAttestationV1/policySchedule")
            .unwrap() = serde_json::Value::String(format!(
            "0x{}",
            hex::encode(schedule.encode_canonical().unwrap())
        ));
        fs::write(
            root.path().join("genesis.json"),
            serde_json::to_vec(&genesis).unwrap(),
        )
        .unwrap();

        let binding = development_network_binding(root.path(), 54322345).unwrap();
        assert_eq!(binding.len(), NetworkBindingV1::CANONICAL_LEN * 2);
    }
}
