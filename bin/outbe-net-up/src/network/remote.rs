use std::path::{Path, PathBuf};

use eyre::{ensure, Context, Result};
pub(crate) use outbe_operator_remote::{HostExec, SshOptions};
use outbe_software_bundle::sha256_file;

use super::{
    build::{BuildState, DeploymentKind, NetworkManifest},
    config::{Founder, NetworkConfig, Ssh},
};

pub(crate) struct NetworkArtifacts {
    pub(crate) root: PathBuf,
    pub(crate) config: NetworkConfig,
    pub(crate) manifest: NetworkManifest,
}

impl NetworkArtifacts {
    pub(crate) fn load(root: &Path) -> Result<Self> {
        ensure!(
            root.is_dir(),
            "network artifacts directory does not exist: {}",
            root.display()
        );
        let config_path = root.join("network.yaml");
        let config = NetworkConfig::read(&config_path)?;
        let manifest_path = root.join("network-manifest.json");
        let manifest: NetworkManifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)
            .wrap_err_with(|| format!("invalid network manifest: {}", manifest_path.display()))?;
        ensure!(
            manifest.version == 1,
            "unsupported network manifest version"
        );
        ensure!(
            manifest.state == BuildState::Ready,
            "network artifacts are not ready"
        );
        ensure!(
            manifest.name == config.name
                && manifest.network == config.network.as_str()
                && manifest.chain_id == config.chain_id
                && manifest.founders.len() == config.founders.len(),
            "network manifest does not match network.yaml"
        );
        ensure!(
            sha256_file(&config_path)? == manifest.config_sha256,
            "network.yaml changed after network build"
        );
        ensure!(
            sha256_file(&root.join("software-bundle.tar.zst"))? == manifest.software.archive_sha256,
            "software bundle changed after network build"
        );
        let (genesis_block_hash, _) = super::build::genesis_identity(&root.join("genesis.json"))?;
        ensure!(
            manifest.genesis.final_path == "genesis.json"
                && manifest.genesis.final_sha256 == sha256_file(&root.join("genesis.json"))?
                && manifest.genesis.genesis_block_hash == genesis_block_hash,
            "final genesis is missing or changed after network build"
        );
        let base_identity = (
            manifest.genesis.base_path.as_deref(),
            manifest.genesis.base_sha256.as_deref(),
        );
        ensure!(
            matches!(base_identity, (Some(_), Some(_)) | (None, None)),
            "network manifest has an incomplete base genesis identity"
        );
        if let (Some(path), Some(expected)) = base_identity {
            ensure!(
                sha256_file(&root.join(path))? == expected,
                "base genesis is missing or changed after network build"
            );
        }
        for (index, (founder, artifact)) in
            config.founders.iter().zip(&manifest.founders).enumerate()
        {
            ensure!(
                artifact.index == index
                    && artifact.name == founder.name
                    && artifact.advertise_ip == founder.advertise_ip,
                "founder {index} does not match its network manifest"
            );
            match manifest.deployment {
                DeploymentKind::Host => {
                    let ssh = founder.ssh.as_ref().ok_or_else(|| {
                        eyre::eyre!("founder {index} is missing ssh configuration")
                    })?;
                    let expected = format!("dist/validator-{index}.tgz");
                    ensure!(
                        artifact.ssh_host.as_deref() == Some(ssh.host.as_str())
                            && artifact.archive.as_deref() == Some(expected.as_str()),
                        "founder {index} does not match its network manifest"
                    );
                    let expected_sha = artifact.archive_sha256.as_deref().ok_or_else(|| {
                        eyre::eyre!("founder {index} manifest is missing archive_sha256")
                    })?;
                    ensure!(
                        expected_sha == sha256_file(&root.join(&expected))?,
                        "founder {index} archive changed after network build"
                    );
                }
                DeploymentKind::Vm => {
                    let expected = format!("vm/validator-{index}");
                    let vm = artifact.vm.as_ref().ok_or_else(|| {
                        eyre::eyre!("founder {index} manifest is missing vm metadata")
                    })?;
                    let resolved = config.resolve_founder_vm(founder)?;
                    ensure!(
                        vm.data_tree == expected
                            && vm.vmm_host == resolved.vmm_host.host
                            && vm.bridge_ip == resolved.bridge_ip
                            && vm.port_offset == resolved.port_offset
                            && root.join(&expected).is_dir(),
                        "founder {index} VM data tree does not match its network manifest"
                    );
                }
            }
        }
        Ok(Self {
            root: root.to_owned(),
            config,
            manifest,
        })
    }
}

pub(super) fn capture_founder<E: HostExec + ?Sized>(
    exec: &E,
    founder: &Founder,
    command: &str,
) -> Result<String> {
    exec.capture(founder_ssh(founder)?, &founder.name, command)
}

pub(super) fn run_founder<E: HostExec + ?Sized>(
    exec: &E,
    founder: &Founder,
    command: &str,
) -> Result<()> {
    exec.run(founder_ssh(founder)?, &founder.name, command)
}

pub(super) fn run_quiet_founder<E: HostExec + ?Sized>(
    exec: &E,
    founder: &Founder,
    command: &str,
) -> Result<()> {
    exec.run_quiet(founder_ssh(founder)?, &founder.name, command)
}

pub(super) fn copy_to_founder<E: HostExec + ?Sized>(
    exec: &E,
    founder: &Founder,
    sources: &[&Path],
    destination: &str,
    recursive: bool,
) -> Result<()> {
    exec.copy_to(
        founder_ssh(founder)?,
        &founder.name,
        sources,
        destination,
        recursive,
    )
}

fn founder_ssh(founder: &Founder) -> Result<&Ssh> {
    founder
        .ssh
        .as_ref()
        .ok_or_else(|| eyre::eyre!("founder {} has no host SSH endpoint", founder.name))
}
