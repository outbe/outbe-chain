//! Joining operator bundle assembly and feeder configuration rendering.

use std::{
    collections::BTreeSet,
    fs,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{bail, ensure, Context, Result};
use outbe_operator_node_role::{
    bundle::{self, BundleInput},
    config::{
        FeederConfig as OperatorFeederConfig, NetworkConfig as OperatorNetworkConfig,
        NetworkPorts as OperatorNetworkPorts, OperatorConfig,
        CONFIG_VERSION as OPERATOR_CONFIG_VERSION,
    },
};
use outbe_software_bundle::sha256_file;

use super::{
    build::{BuildArtifacts, JoiningArtifact},
    build_vm::consensus_peers,
};
use crate::network::config::{Network, NetworkConfig};

pub(crate) fn build_joining_artifacts(artifacts: &BuildArtifacts<'_>) -> Result<JoiningArtifact> {
    let joining = artifacts.root.join("joining");
    fs::create_dir(&joining)?;
    let files = joining_bundle_files(artifacts)?;
    let bundle_path = joining.join("outbe-validator-operator-bundle.tar.zst");
    bundle::create_archive(&bundle_path, files)?;

    let bootnodes = fs::read_to_string(artifacts.root.join("reth-bootnodes.txt"))?
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let peers = consensus_peers(artifacts.config, artifacts.validators)?;
    let operator =
        joining_operator_config(artifacts.config, artifacts.software_root, bootnodes, peers)?;
    let config_path = joining.join("operator-config.example.yaml");
    fs::write(&config_path, serde_yaml_ng::to_string(&operator)?)?;
    if !artifacts.config.feeder.is_empty() {
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(JoiningArtifact {
        bundle: "joining/outbe-validator-operator-bundle.tar.zst".into(),
        bundle_sha256: sha256_file(&bundle_path)?,
        config_example: "joining/operator-config.example.yaml".into(),
    })
}

pub(super) fn joining_operator_config(
    config: &NetworkConfig,
    software: &Path,
    bootnodes: Vec<String>,
    consensus_peers: Vec<String>,
) -> Result<OperatorConfig> {
    let (vm_prefix, vm_gateway, vm_dns) = match &config.vm {
        Some(vm) => (vm.prefix, vm.gateway, vm.dns.clone()),
        None => (None, None, Vec::new()),
    };
    Ok(OperatorConfig {
        version: OPERATOR_CONFIG_VERSION,
        network: OperatorNetworkConfig {
            chain_id: config.chain_id,
            certified_rpc: config.certified_rpc.clone(),
            advertise_ip: Ipv4Addr::new(192, 0, 2, 10),
            rpc_bind_ip: config.vm.is_none().then_some(Ipv4Addr::LOCALHOST),
            ports: OperatorNetworkPorts::default(),
            vm_ip: None,
            vm_prefix,
            vm_gateway,
            vm_dns,
            vm_rpc_port: None,
            vm_consensus_port: None,
            vm_reth_port: None,
            vm_discv5_port: None,
            vm_radicle_port: None,
            bootnodes,
            consensus_peers,
            use_local_defaults: config.topology.use_local_defaults,
        },
        feeder: OperatorFeederConfig {
            public_toml: joining_feeder_public_toml(config, software)?,
        },
        hyperlane: config.resolve_joining_hyperlane_config()?,
    })
}

pub(super) fn feeder_pairs_path(config: &NetworkConfig, software: &Path) -> PathBuf {
    let profile = match config.network {
        Network::Devnet => "devnet",
        Network::Testnet | Network::Mainnet => "testnet",
    };
    software
        .join("share/outbe-genesis/profiles")
        .join(profile)
        .join("pairs.toml")
}

#[derive(Default)]
struct FeederProfileRequirements {
    chain_ids: BTreeSet<u64>,
    uses_mock_http: bool,
    uses_pyth: bool,
    uses_redstone: bool,
}

fn parse_feeder_profile_requirements(pairs_toml: &str) -> Result<FeederProfileRequirements> {
    let table: toml::Value =
        toml::from_str(pairs_toml).wrap_err("parse feeder pairs.toml profile")?;
    let mut reqs = FeederProfileRequirements::default();
    for section in ["onchain_providers", "dex_providers", "gas_providers"] {
        if let Some(entries) = table.get(section).and_then(toml::Value::as_array) {
            for entry in entries {
                if let Some(cid) = entry
                    .get("chain_id")
                    .and_then(toml::Value::as_integer)
                    .and_then(|v| u64::try_from(v).ok())
                {
                    reqs.chain_ids.insert(cid);
                }
            }
        }
    }
    if let Some(pairs) = table.get("currency_pairs").and_then(toml::Value::as_array) {
        for pair in pairs {
            if let Some(markets) = pair.get("markets").and_then(toml::Value::as_array) {
                record_market_providers(&mut reqs, markets);
            }
        }
    }
    Ok(reqs)
}

fn record_market_providers(reqs: &mut FeederProfileRequirements, markets: &[toml::Value]) {
    for market in markets {
        match market.get("provider").and_then(toml::Value::as_str) {
            Some("mock_http") => reqs.uses_mock_http = true,
            Some("pyth") => reqs.uses_pyth = true,
            Some("redstone") => reqs.uses_redstone = true,
            _ => {}
        }
    }
}

fn append_feeder_rpc_endpoints(
    public: &mut String,
    config: &NetworkConfig,
    required_chain_ids: &BTreeSet<u64>,
) -> Result<()> {
    let is_vm = config.vm.is_some();
    if is_vm {
        let missing: Vec<String> = required_chain_ids
            .iter()
            .filter(|cid| !config.feeder.rpc_endpoints.contains_key(cid))
            .map(u64::to_string)
            .collect();
        if !missing.is_empty() {
            bail!(
                "non-devnet VM network `{}` is missing feeder.rpc_endpoints for chain ID(s) required by the active feeder profile: {}; add `feeder.rpc_endpoints` to network.yaml",
                config.name,
                missing.join(", ")
            );
        }
    }
    let mut all_chain_ids = required_chain_ids.clone();
    all_chain_ids.extend(config.feeder.rpc_endpoints.keys().copied());
    if all_chain_ids.is_empty() {
        return Ok(());
    }
    public.push_str("\n[rpc_endpoints]\n");
    for cid in all_chain_ids {
        let url = config
            .feeder
            .rpc_endpoints
            .get(&cid)
            .map(String::as_str)
            .unwrap_or("REPLACE_WITH_RPC_URL");
        let quoted = serde_json::to_string(url)?;
        public.push_str(&format!("{cid} = {quoted}\n"));
    }
    Ok(())
}

fn append_feeder_api_providers(
    public: &mut String,
    config: &NetworkConfig,
    reqs: &FeederProfileRequirements,
) -> Result<()> {
    if reqs.uses_mock_http {
        let rest = config
            .genesis
            .overrides
            .get("price_feed_rest")
            .and_then(serde_yaml_ng::Value::as_str)
            .unwrap_or("https://prc.testnet.outbe.net");
        let rest = serde_json::to_string(rest)?;
        public.push_str(&format!(
            "\n[[api_providers]]\nname = \"mock_http\"\nrest = {rest}\n"
        ));
    }
    if reqs.uses_pyth {
        let key = match config.feeder.api_keys.get("pyth") {
            Some(key) => key.as_str(),
            None if config.vm.is_some() => {
                bail!(
                    "non-devnet VM network `{}` requires feeder.api_keys.pyth for the active feeder profile; configure `feeder.api_keys.pyth` in network.yaml",
                    config.name
                );
            }
            None => "REPLACE_WITH_PYTH_API_KEY",
        };
        let quoted = serde_json::to_string(key)?;
        public.push_str(&format!(
            "\n[[api_providers]]\nname = \"pyth\"\napi_key = {quoted}\n"
        ));
    }
    if reqs.uses_redstone {
        if let Some(key) = config.feeder.api_keys.get("redstone") {
            let quoted = serde_json::to_string(key)?;
            public.push_str(&format!(
                "\n[[api_providers]]\nname = \"redstone\"\napi_key = {quoted}\n"
            ));
        }
    }
    Ok(())
}

pub(super) fn joining_feeder_public_toml(
    config: &NetworkConfig,
    software: &Path,
) -> Result<String> {
    let pairs_path = feeder_pairs_path(config, software);
    let pairs = fs::read_to_string(&pairs_path)
        .wrap_err_with(|| format!("read feeder pairs: {}", pairs_path.display()))?;
    let mut public =
        String::from("[oracle]\nsources = \"local\"\nvote_period = 60\npoll_interval_secs = 2\n");
    if config.network != Network::Devnet {
        let reqs = parse_feeder_profile_requirements(&pairs)?;
        append_feeder_rpc_endpoints(&mut public, config, &reqs.chain_ids)?;
        append_feeder_api_providers(&mut public, config, &reqs)?;
    }
    public.push('\n');
    public.push_str(&pairs);
    if config.vm.is_some() && config.network != Network::Devnet {
        ensure!(
            !public.contains("REPLACE_WITH_"),
            "refusing to emit VM feeder-public.toml containing a REPLACE_WITH_ placeholder; \
             configure `feeder.rpc_endpoints` (and `feeder.api_keys` if required) in network.yaml"
        );
    }
    Ok(public)
}

fn joining_bundle_files(artifacts: &BuildArtifacts<'_>) -> Result<Vec<BundleInput>> {
    let mut files = Vec::new();
    for file in &artifacts.software.files {
        let destination = match file.path.strip_prefix("bin/") {
            Some(name) => Some(name.to_owned()),
            None if file.path == ".bb-crs/bn254_g1.dat" => Some(file.path.clone()),
            None => None,
        };
        if let Some(path) = destination {
            files.push(BundleInput {
                path,
                source: artifacts.software_root.join(&file.path),
                mode: file.mode,
            });
        }
    }
    files.push(BundleInput {
        path: "genesis.json".into(),
        source: artifacts.root.join("genesis.json"),
        mode: 0o644,
    });
    Ok(files)
}
