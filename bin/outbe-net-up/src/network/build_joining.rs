//! Joining operator bundle assembly and feeder configuration rendering.

use std::{
    fs,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use eyre::{Context, Result};
use outbe_operator_node_role::{
    bundle::{self, BundleInput},
    config::{
        FeederConfig as OperatorFeederConfig, NetworkConfig as OperatorNetworkConfig,
        NetworkPorts as OperatorNetworkPorts, OperatorConfig,
        CONFIG_VERSION as OPERATOR_CONFIG_VERSION,
    },
};
use outbe_software_bundle::sha256_file;
use serde_json::json;

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
        public.push_str(&outbe_genesis_cli::launch_bundle::feeder_pairs_settings(
            &json!({"feeder_pairs": pairs_path}),
        )?);
        public.push('\n');
    }
    public.push('\n');
    public.push_str(&pairs);
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
